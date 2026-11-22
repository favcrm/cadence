//! Milestone view aggregation and formatting for `issue::work::view`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::super::{date_epoch, stage_of, Ctx};
use super::{effective_milestone, health_json, is_open, progress_json};
use crate::error::{Error, Result};
use crate::issue::board::View;
use crate::issue::model;

const HEALTH_RANK: &[&str] = &["on_track", "at_risk", "stalled"];

/// The `--health` vocabulary (`HEALTH_RANK`, exported for validation).
pub const HEALTH_STATES: &[&str] = HEALTH_RANK;

fn worse(a: &str, b: &str) -> &'static str {
    let rank = |s: &str| HEALTH_RANK.iter().position(|h| *h == s).unwrap_or(0);
    HEALTH_RANK[rank(a).max(rank(b))]
}

/// `cadence milestone ls|show`: one row per (project, milestone) —
/// configured in `PROJECT.md` (in its order) or named by an issue's
/// `milestone` / `m<n>-…` tag (natural order after). Progress rolls up
/// the milestone's work items the same way as an epic's: its loose
/// non-epic tasks, size-weighted. A task's explicit assignment overrides
/// its epic's default. Health uses only work scoped to this checkpoint;
/// checkpoint status and calendar dates are declared, never inferred.
pub fn milestones_json(ctx: &Ctx, views: &[View], project_key: Option<&str>) -> Vec<Value> {
    // (project, milestone) → member views, in id order.
    let mut members: BTreeMap<(String, String), Vec<&View>> = BTreeMap::new();
    for v in views {
        if project_key.is_some_and(|p| v.issue.project != p) {
            continue;
        }
        if let Some((m, _)) = effective_milestone(ctx, v) {
            members
                .entry((v.issue.project.clone(), m))
                .or_default()
                .push(v);
        }
    }
    let mut keys: Vec<(String, String)> = vec![];
    let mut projects: Vec<&String> = ctx.configs.keys().collect();
    projects.sort();
    for key in projects {
        if project_key.is_some_and(|p| key != p) {
            continue;
        }
        for m in &ctx.config(key).milestones {
            keys.push((key.clone(), m.id.clone()));
        }
    }
    let mut found: Vec<&(String, String)> = members.keys().filter(|k| !keys.contains(k)).collect();
    found.sort_by_key(|(p, m)| (p.clone(), natural(m)));
    keys.extend(found.into_iter().cloned());
    keys.sort_by_key(|(p, _)| p.clone());
    keys.iter()
        .map(|(key, id)| milestone_row(ctx, key, id, members.get(&(key.clone(), id.clone()))))
        .collect()
}

/// `m10` after `m2`: digits compare as numbers.
fn natural(id: &str) -> (String, u64) {
    let digits: String = id.chars().skip_while(|c| !c.is_ascii_digit()).collect();
    let head: String = id.chars().take_while(|c| !c.is_ascii_digit()).collect();
    (head, digits.parse().unwrap_or(u64::MAX))
}

fn milestone_row(ctx: &Ctx, key: &str, id: &str, members: Option<&Vec<&View>>) -> Value {
    let cfg = ctx.config(key);
    let conf = cfg.milestones.iter().find(|m| m.id == id);
    let members: &[&View] = members.map(Vec::as_slice).unwrap_or(&[]);
    let items: Vec<&View> = members
        .iter()
        .copied()
        .filter(|v| model::item_type(&v.issue.front, v.container) != "epic")
        .collect();
    let mut contributors: BTreeMap<&str, &View> = BTreeMap::new();
    for v in members {
        if model::item_type(&v.issue.front, v.container) == "epic" {
            contributors.insert(&v.issue.front.id, v);
        } else if let Some(parent) = v.issue.front.parent.as_ref().and_then(|p| ctx.by_id.get(p)) {
            if parent.issue.project == key
                && model::item_type(&parent.issue.front, parent.container) == "epic"
            {
                contributors.insert(&parent.issue.front.id, parent);
            }
        }
    }
    let mut epics = vec![];
    let mut health: &'static str = "on_track";
    let mut reasons: Vec<Value> = vec![];
    for v in contributors.values() {
        let kids: Vec<&View> = items
            .iter()
            .copied()
            .filter(|k| k.issue.front.parent.as_deref() == Some(v.issue.front.id.as_str()))
            .collect();
        let stage = stage_of(&v.issue.front, cfg, v.status == "done");
        // A completed slice doesn't inherit timing risk from an epic's
        // unfinished work in another checkpoint.
        let scoped_health = if kids.iter().any(|k| is_open(k)) {
            health_json(v, &stage, cfg, &kids, ctx.now)
        } else {
            json!({"state": "on_track", "reasons": []})
        };
        let state = scoped_health["state"].as_str().unwrap_or("on_track");
        health = worse(health, state);
        if let Some(scoped_reasons) = scoped_health["reasons"].as_array() {
            reasons.extend(scoped_reasons.iter().cloned());
        }
        epics.push(json!({
            "id": v.issue.front.id,
            "title": v.issue.front.title,
            "owner": v.issue.front.owner,
            "stage": stage.id,
            "progress": progress_json(&kids)["ratio"],
            "task_count": kids.len(),
            "health": state,
        }));
    }
    for v in &items {
        if v.blocked
            && is_open(v)
            && !reasons
                .iter()
                .any(|r| r["issue"] == v.issue.front.id && r["cause"] == "blocked")
        {
            health = worse(health, "at_risk");
            reasons.push(json!({
                "cause": "blocked",
                "issue": v.issue.front.id,
                "owner": v.issue.front.owner,
                "detail": format!("{} is blocked", v.issue.front.id),
                "next": format!("unblock {}", v.issue.front.id),
            }));
        }
    }
    let tasks: Vec<Value> = items
        .iter()
        .map(|v| {
            json!({
                "id": v.issue.front.id,
                "title": v.issue.front.title,
                "type": model::item_type(&v.issue.front, v.container),
                "status": v.status,
                "size": v.issue.front.size,
                "owner": v.issue.front.owner,
                "blocked": v.blocked,
                "parent": v.issue.front.parent,
            })
        })
        .collect();
    let loose: Vec<&Value> = tasks.iter().filter(|v| v["parent"].is_null()).collect();
    let status = conf.map(|m| m.status.as_str());
    let today = crate::issue::time::iso(ctx.now);
    let days_remaining = conf
        .and_then(|m| m.target_date.as_deref())
        .and_then(date_epoch)
        .zip(date_epoch(&today[..10]))
        .map(|(target, now)| (target - now) / 86_400);
    let schedule = match status {
        Some("achieved") => "achieved",
        Some("cancelled") => "cancelled",
        _ => match days_remaining {
            Some(days) if days < 0 => "overdue",
            Some(0) => "due_today",
            Some(_) => "upcoming",
            None => "unscheduled",
        },
    };
    let dependencies: Vec<Value> = conf
        .into_iter()
        .flat_map(|m| &m.depends_on)
        .map(|id| {
            let dependency = cfg
                .milestones
                .iter()
                .find(|m| &m.id == id)
                .expect("validated dependency");
            json!({"id": id, "title": dependency.title, "status": dependency.status.as_str()})
        })
        .collect();
    json!({
        "project": key,
        "id": id,
        "title": conf.and_then(|m| m.title.clone()),
        "exit": conf.and_then(|m| m.exit.clone()),
        "description": conf.and_then(|m| m.description.clone()),
        "owner": conf.and_then(|m| m.owner.clone()),
        "start_date": conf.and_then(|m| m.start_date.clone()),
        "target_date": conf.and_then(|m| m.target_date.clone()),
        "completed_date": conf.and_then(|m| m.completed_date.clone()),
        "status": status,
        "evidence": conf.map(|m| &m.evidence).cloned().unwrap_or_default(),
        "depends_on": conf.map(|m| &m.depends_on).cloned().unwrap_or_default(),
        "dependencies": dependencies,
        "schedule": {"state": schedule, "days_remaining": days_remaining},
        "configured": conf.is_some(),
        "progress": progress_json(&items),
        "health": {"state": health, "reasons": reasons},
        "epics": epics,
        "issues": loose,
        "tasks": tasks,
        "config_error": ctx.work(key).and_then(|w| w.error.as_ref()),
    })
}

/// `milestone show <id>`: the one row for `id`, from `project` when
/// given; without it the id must name a milestone in exactly one
/// project.
pub fn milestone_show(
    ctx: &Ctx,
    views: &[View],
    id: &str,
    project_key: Option<&str>,
) -> Result<Value> {
    let rows: Vec<Value> = milestones_json(ctx, views, project_key)
        .into_iter()
        .filter(|r| r["id"] == id)
        .collect();
    if rows.len() > 1 {
        return Err(Error::rejected(format!(
            "Milestone '{id}' exists in {} — pass --project",
            rows.iter()
                .filter_map(|r| r["project"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    rows.into_iter().next().ok_or_else(|| {
        Error::rejected(format!(
            "Unknown milestone '{id}' — `cadence milestone ls` lists them"
        ))
    })
}
