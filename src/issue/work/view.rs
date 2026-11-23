//! Work-item progress, health, and milestone view projection for `issue::work`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::HashMap;
use std::path::Path;

use cadence_core::board_api::{
    HealthReason, IssueDetail, ProgressCounts, WorkBlock, WorkHealth, WorkProgress, WorkStage,
};
use serde_json::Value;

use super::{legal_moves, stage_of, Approvals, Ctx, StageState, WorkConfig};
use crate::issue::board::{self, View};
use crate::issue::{model, plan};

mod milestones;

pub use milestones::{milestone_show, milestones_json, HEALTH_STATES};

pub(super) fn is_open(v: &View) -> bool {
    !matches!(v.status.as_str(), "done" | "dropped")
}

fn ratio(done: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64 * 100.0).round() / 100.0
    }
}

/// Size-weighted progress over a set of work items — the plan's
/// weights ([`plan::progress`]) — with the counts the views show:
/// open (backlog + ready) · doing · review · blocked, plus done and
/// dropped.
pub fn progress_json(items: &[&View]) -> Value {
    serde_json::to_value(progress(items)).expect("progress serializes")
}

/// [`progress_json`] as the typed wire struct.
pub fn progress(items: &[&View]) -> WorkProgress {
    let pairs: Vec<(&str, Option<&str>)> = items
        .iter()
        .map(|v| (v.status.as_str(), v.issue.front.size.as_deref()))
        .collect();
    let (done, total) = plan::progress(&pairs);
    let n = |s: &[&str]| {
        items
            .iter()
            .filter(|v| s.contains(&v.status.as_str()))
            .count() as u64
    };
    WorkProgress {
        done_weight: done,
        total_weight: total,
        ratio: ratio(done, total),
        counts: ProgressCounts {
            open: n(&["backlog", "ready"]),
            doing: n(&["doing"]),
            review: n(&["review"]),
            blocked: items.iter().filter(|v| v.blocked && is_open(v)).count() as u64,
            done: n(&["done"]),
            dropped: n(&["dropped"]),
            total: items.len() as u64,
        },
    }
}

/// Health of an epic: `stalled` once it has been in its stage for 2×
/// the limit or more, `at_risk` when an open child is blocked or it has
/// been in the stage longer than the limit, else `on_track`. "Stalled"
/// is time in stage, not child activity. Each reason names the owner
/// and the next action. A terminal stage is never at risk; an unknown
/// entry time skips the time check.
pub fn health_json(
    epic: &View,
    stage: &StageState,
    cfg: &WorkConfig,
    kids: &[&View],
    now: i64,
) -> Value {
    serde_json::to_value(health(epic, stage, cfg, kids, now)).expect("health serializes")
}

/// [`health_json`] as the typed wire struct.
pub fn health(
    epic: &View,
    stage: &StageState,
    cfg: &WorkConfig,
    kids: &[&View],
    now: i64,
) -> WorkHealth {
    const DAY: i64 = 86_400;
    let limit = cfg.stage_limit_days;
    let elapsed = stage.since.map(|s| (now - s).max(0));
    let days = elapsed.map(|e| e / DAY);
    let f = &epic.issue.front;
    let mut reasons = vec![];
    let mut state = "on_track";
    if !stage.terminal {
        for k in kids.iter().filter(|k| k.blocked && is_open(k)) {
            let kf = &k.issue.front;
            let waits = match k.blocked_reason {
                Some(r) => r.to_string(),
                None => format!("waits on {}", kf.blocked_by.join(", ")),
            };
            reasons.push(HealthReason {
                cause: "blocked".into(),
                issue: kf.id.clone(),
                owner: kf.owner.clone(),
                detail: format!("{} {waits}", kf.id),
                next: format!("unblock {} or re-plan around it", kf.id),
            });
            state = "at_risk";
        }
        let limit_s = limit as i64 * DAY;
        if let Some(e) = elapsed.filter(|e| *e > limit_s) {
            let stalled = e >= 2 * limit_s;
            let next = if f.plan.as_ref().is_some_and(|p| p.state == "proposed") {
                format!(
                    "approve the plan (`cadence plan approve {}`) or reject it",
                    f.id
                )
            } else {
                format!(
                    "meet the '{}' exit criterion and move the stage, or record why it waits",
                    stage.id
                )
            };
            reasons.push(HealthReason {
                cause: if stalled { "stalled" } else { "stage_time" }.into(),
                issue: f.id.clone(),
                owner: f.owner.clone(),
                detail: format!("{} days in '{}' (limit {limit})", e / DAY, stage.id),
                next,
            });
            state = if stalled { "stalled" } else { "at_risk" };
        }
    }
    WorkHealth {
        state: state.into(),
        days_in_stage: days,
        limit_days: limit,
        reasons,
    }
}

/// A task's own field or legacy tag wins over its parent epic's default.
/// Inheritance stays inside the project and never expands a whole epic
/// into a checkpoint when one of its tasks explicitly belongs elsewhere.
pub(super) fn effective_milestone(ctx: &Ctx, view: &View) -> Option<(String, &'static str)> {
    model::milestone_of(&view.issue.front).or_else(|| {
        if model::item_type(&view.issue.front, view.container) == "epic" {
            return None;
        }
        let parent = ctx.by_id.get(view.issue.front.parent.as_deref()?)?;
        if parent.issue.project != view.issue.project
            || model::item_type(&parent.issue.front, parent.container) != "epic"
        {
            return None;
        }
        model::milestone_of(&parent.issue.front).map(|(id, _)| (id, "parent"))
    })
}

/// The `work` block of one issue for the board (list and detail), the
/// CLI and the epic rows: type, milestone, size and weight for every
/// issue; stage, progress and health for epics (`null` otherwise).
pub fn item_json(ctx: &Ctx, view: &View) -> Value {
    serde_json::to_value(item_block(ctx, view)).expect("work block serializes")
}

/// [`item_json`] as the typed wire block.
pub fn item_block(ctx: &Ctx, view: &View) -> WorkBlock {
    let f = &view.issue.front;
    let cfg = ctx.config(&view.issue.project);
    let kind = model::item_type(f, view.container);
    let milestone = effective_milestone(ctx, view);
    let mut out = WorkBlock {
        kind: kind.to_string(),
        type_source: if f
            .item_type
            .as_deref()
            .is_some_and(|t| model::TYPES.contains(&t))
        {
            "field"
        } else {
            "implicit"
        }
        .to_string(),
        milestone: milestone.as_ref().map(|(m, _)| m.clone()),
        milestone_source: milestone.as_ref().map(|(_, s)| s.to_string()),
        size: f.size.clone(),
        weight: model::size_weight(f.size.as_deref()),
        stage: None,
        progress: None,
        health: None,
        config_error: None,
        config_unapproved: None,
    };
    if kind == "epic" {
        let stage = stage_of(f, cfg, view.status == "done");
        let kids = ctx.kids(view);
        let next = stage
            .index
            .and_then(|i| cfg.stages.get(i + 1))
            .map(|s| s.id.clone());
        out.stage = Some(WorkStage {
            id: stage.id.clone(),
            source: stage.source.to_string(),
            since: stage.since.map(crate::issue::time::iso),
            exit: stage.index.map(|i| cfg.stages[i].exit.clone()),
            next_needs_operator: next
                .as_ref()
                .is_some_and(|n| cfg.operator_stages.contains(n)),
            next,
            terminal: stage.terminal,
            stages: cfg.stage_ids().into_iter().map(String::from).collect(),
            // A PROJECT.md the writer cannot load refuses every move.
            moves: if ctx
                .work(&view.issue.project)
                .is_some_and(|w| w.error.is_some())
            {
                vec![]
            } else {
                legal_moves(f, cfg, &stage)
            },
        });
        out.progress = Some(progress(&kids));
        out.health = Some(health(view, &stage, cfg, &kids, ctx.now));
    }
    if let Some(w) = ctx.work(&view.issue.project) {
        out.config_error = w.error.clone();
        out.config_unapproved = w.unapproved.clone();
    }
    out
}

/// The drawer payload ([`board::detail_json`]) with its `work` block —
/// `GET /api/issues/:id` and `issue show --json`.
pub fn detail_json(pm_dir: &Path, ctx: &Ctx, view: &View) -> Value {
    serde_json::to_value(detail(pm_dir, ctx, view)).expect("issue detail serializes")
}

/// [`detail_json`] as the typed wire struct.
pub fn detail(pm_dir: &Path, ctx: &Ctx, view: &View) -> IssueDetail {
    let mut detail = board::detail(pm_dir, view, ctx.by_id);
    detail.work = Some(item_block(ctx, view));
    detail
}

/// A board card ([`board::card_json`]) with its `work` block — the
/// list payload of `GET /api/issues` and `issue ls --json`.
pub fn card_json(ctx: &Ctx, view: &View) -> Value {
    let mut card = board::card_json(view);
    card["work"] = item_json(ctx, view);
    card
}

/// `issue epic ls` / `GET /api/epics`: every epic (effective type) in
/// id order, optionally one project's — [`board::epic_json`] plus the
/// `work` block.
pub fn epics_json(
    pm_dir: &Path,
    views: &[&View],
    project_key: Option<&str>,
    now: i64,
    approvals: &Approvals,
) -> Vec<Value> {
    let by_id: HashMap<String, &View> = views
        .iter()
        .map(|v| (v.issue.front.id.clone(), *v))
        .collect();
    let ctx = Ctx::new(pm_dir, &by_id, now, approvals);
    views
        .iter()
        .filter(|v| project_key.is_none_or(|p| v.issue.project == p))
        .filter(|v| model::item_type(&v.issue.front, v.container) == "epic")
        .map(|v| epic_row(&ctx, v))
        .collect()
}

pub fn epic_row(ctx: &Ctx, view: &View) -> Value {
    let mut row = board::epic_json(view, ctx.by_id);
    row["work"] = item_json(ctx, view);
    row
}
