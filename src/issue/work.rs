//! CAD-405 work model: item types, epic stages, computed progress and
//! health, milestones (docs/design/WORK-MODEL.md).
//!
//! Everything here is read-side and computed: an issue stores only its
//! `type`, `size`, `milestone` and — on an epic — `stage` + `stage_at`.
//! The stage list, the time-in-stage limit, the stages whose entry
//! needs the operator, and the milestones come from the project's
//! optional `<pm>/<key>/PROJECT.md` frontmatter, never `project.yaml`
//! (strict, `deny_unknown_fields` — older binaries would refuse a new
//! key there). A project without `PROJECT.md` gets the defaults.
//!
//! Progress reuses the plan's size weights ([`plan::progress`]). Health
//! is `on_track`, `at_risk` (an open child is blocked, or the epic has
//! sat in its stage longer than the limit) or `stalled` (2× the limit).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::board::{self, View};
use crate::issue::model::{self, Front};
use crate::issue::{parse, plan, project};

/// Default epic stages and their exit criteria.
pub const DEFAULT_STAGES: &[(&str, &str)] = &[
    (
        "shape",
        "Goal, non-goals and acceptance written; tasks listed",
    ),
    ("build", "All tasks done or dropped"),
    (
        "verify",
        "Independent review of the combined result; the milestone's exit test run where relevant",
    ),
    (
        "release",
        "Merged, deployed or published as the project defines",
    ),
    ("done", "Retro filed; lessons proposed"),
];
/// Days an epic may sit in one stage before it is at risk.
pub const DEFAULT_STAGE_LIMIT_DAYS: u64 = 5;
/// Stages whose forward entry is an operator decision: `build` is plan
/// approval's twin (work may start), `release` ships.
pub const DEFAULT_OPERATOR_STAGES: &[&str] = &["build", "release"];

#[derive(Clone, Debug, PartialEq)]
pub struct Stage {
    pub id: String,
    pub exit: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Milestone {
    pub id: String,
    pub title: Option<String>,
    pub exit: Option<String>,
}

/// A project's work-model settings — `PROJECT.md` or the defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkConfig {
    pub stages: Vec<Stage>,
    pub stage_limit_days: u64,
    pub operator_stages: Vec<String>,
    pub milestones: Vec<Milestone>,
}

impl Default for WorkConfig {
    fn default() -> Self {
        Self {
            stages: DEFAULT_STAGES
                .iter()
                .map(|(id, exit)| Stage {
                    id: id.to_string(),
                    exit: exit.to_string(),
                })
                .collect(),
            stage_limit_days: DEFAULT_STAGE_LIMIT_DAYS,
            operator_stages: DEFAULT_OPERATOR_STAGES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            milestones: vec![],
        }
    }
}

impl WorkConfig {
    pub fn index(&self, stage: &str) -> Option<usize> {
        self.stages.iter().position(|s| s.id == stage)
    }

    pub fn stage_ids(&self) -> Vec<&str> {
        self.stages.iter().map(|s| s.id.as_str()).collect()
    }
}

/// `PROJECT.md` frontmatter — lenient: the file carries other project
/// settings (agents, autonomy, repos) this reader does not own.
#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    stages: Option<Vec<RawStage>>,
    #[serde(default)]
    stage_limit_days: Option<u64>,
    #[serde(default)]
    operator_stages: Option<Vec<String>>,
    #[serde(default)]
    milestones: Vec<RawMilestone>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawStage {
    Name(String),
    Full {
        id: String,
        #[serde(default)]
        exit: Option<String>,
    },
}

#[derive(Deserialize)]
struct RawMilestone {
    id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    exit: Option<String>,
}

/// `<pm>/<key>/PROJECT.md`.
pub fn config_file(pm_dir: &Path, key: &str) -> PathBuf {
    pm_dir.join(key).join("PROJECT.md")
}

/// Parse `PROJECT.md`. No frontmatter, or none of the work keys, is the
/// defaults; a malformed value is an error that names it.
pub fn parse_config(text: &str) -> Result<WorkConfig> {
    let trimmed = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !trimmed.starts_with("---\n") && !trimmed.starts_with("---\r\n") {
        return Ok(WorkConfig::default());
    }
    let (yaml, _) = parse::split_front(trimmed)?;
    let raw: Raw = serde_yaml::from_str(yaml)
        .map_err(|e| Error::rejected(format!("PROJECT.md frontmatter: {e}")))?;
    let mut cfg = WorkConfig::default();
    if let Some(stages) = raw.stages {
        cfg.stages = stages
            .into_iter()
            .map(|s| {
                let (id, exit) = match s {
                    RawStage::Name(id) => (id, None),
                    RawStage::Full { id, exit } => (id, exit),
                };
                let exit = exit.unwrap_or_else(|| {
                    DEFAULT_STAGES
                        .iter()
                        .find(|(d, _)| *d == id)
                        .map(|(_, e)| e.to_string())
                        .unwrap_or_default()
                });
                Stage { id, exit }
            })
            .collect();
    }
    if let Some(limit) = raw.stage_limit_days {
        cfg.stage_limit_days = limit;
    }
    if let Some(ops) = raw.operator_stages {
        cfg.operator_stages = ops;
    } else {
        // Defaults apply only to stages the list actually has.
        let ids = cfg.stage_ids();
        cfg.operator_stages = DEFAULT_OPERATOR_STAGES
            .iter()
            .filter(|s| ids.contains(s))
            .map(|s| s.to_string())
            .collect();
    }
    cfg.milestones = raw
        .milestones
        .into_iter()
        .map(|m| Milestone {
            id: m.id,
            title: m.title,
            exit: m.exit,
        })
        .collect();
    check_config(&cfg)?;
    Ok(cfg)
}

fn check_config(cfg: &WorkConfig) -> Result<()> {
    let bad = |m: String| Err(Error::rejected(format!("PROJECT.md: {m}")));
    if cfg.stages.len() < 2 {
        return bad("stages needs at least two entries — a first stage and a terminal one".into());
    }
    let mut seen = HashSet::new();
    for s in &cfg.stages {
        if !model::valid_tag(&s.id) {
            return bad(format!(
                "stage '{}' — 1-32 lowercase letters, digits or hyphens",
                s.id
            ));
        }
        if !seen.insert(s.id.as_str()) {
            return bad(format!("stage '{}' is listed twice", s.id));
        }
    }
    if let Some(op) = cfg.operator_stages.iter().find(|o| cfg.index(o).is_none()) {
        return bad(format!(
            "operator_stages names '{op}', which is not a stage"
        ));
    }
    if cfg.stage_limit_days == 0 {
        return bad("stage_limit_days must be at least 1".into());
    }
    let mut seen = HashSet::new();
    for m in &cfg.milestones {
        if !model::valid_tag(&m.id) {
            return bad(format!(
                "milestone '{}' — 1-32 lowercase letters, digits or hyphens",
                m.id
            ));
        }
        if !seen.insert(m.id.as_str()) {
            return bad(format!("milestone '{}' is listed twice", m.id));
        }
    }
    Ok(())
}

/// Strict load for writers: no file is the defaults; a symlinked,
/// unreadable or malformed one refuses — a stage move never guesses.
pub fn load_config(pm_dir: &Path, key: &str) -> Result<WorkConfig> {
    let file = config_file(pm_dir, key);
    if file.symlink_metadata().is_err() {
        return Ok(WorkConfig::default());
    }
    if !board::is_real_file(&file) {
        return Err(Error::rejected(format!(
            "{} is not a regular file — the board never follows links",
            file.display()
        )));
    }
    let text = std::fs::read_to_string(&file)
        .map_err(|e| Error::rejected(format!("cannot read {}: {e}", file.display())))?;
    parse_config(&text).map_err(|e| Error::rejected(format!("{key}: {e}")))
}

/// Lenient load for readers: a bad file reads as the defaults plus the
/// error, which views surface as `config_error` and lint warns about.
pub fn load_config_or_default(pm_dir: &Path, key: &str) -> (WorkConfig, Option<String>) {
    match load_config(pm_dir, key) {
        Ok(cfg) => (cfg, None),
        Err(e) => (WorkConfig::default(), Some(e.to_string())),
    }
}

/// An epic's effective stage and where it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct StageState {
    pub id: String,
    /// `field` (a recorded move), `plan` (mapped from the plan state)
    /// or `default` (never moved — the first stage).
    pub source: &'static str,
    /// When the epic entered the stage; `None` when unknown.
    pub since: Option<i64>,
    /// Position in the project's list; `None` for a stage the list does
    /// not have (a hand edit, a changed PROJECT.md) or `rejected`.
    pub index: Option<usize>,
    /// Last stage, or a rejected plan — no further health checks.
    pub terminal: bool,
}

/// The stage of an epic. A plan's state bounds it: `proposed` is always
/// the first stage (nothing may start), `rejected` is terminal; an
/// approved plan without a recorded move is in the second stage. Else a
/// recorded `stage`, else the first stage. A dropped `stage` (an older
/// binary rewrote the file) therefore reads earlier, never later.
pub fn stage_of(front: &Front, cfg: &WorkConfig) -> StageState {
    let at = |s: Option<&str>| s.and_then(crate::issue::time::parse_iso);
    let last = cfg.stages.len() - 1;
    let at_index = |i: usize, source, since| StageState {
        id: cfg.stages[i].id.clone(),
        source,
        since,
        index: Some(i),
        terminal: i == last,
    };
    if let Some(p) = &front.plan {
        match p.state.as_str() {
            "proposed" => return at_index(0, "plan", at(Some(&p.proposed_at))),
            "rejected" => {
                return StageState {
                    id: "rejected".to_string(),
                    source: "plan",
                    since: at(p.decided_at.as_deref()),
                    index: None,
                    terminal: true,
                }
            }
            _ => {}
        }
    }
    if let Some(stage) = front.stage.as_deref().filter(|s| !s.is_empty()) {
        let since = at(front.stage_at.as_deref());
        return match cfg.index(stage) {
            Some(i) => at_index(i, "field", since),
            None => StageState {
                id: stage.to_string(),
                source: "field",
                since,
                index: None,
                terminal: false,
            },
        };
    }
    if let Some(p) = front.plan.as_ref().filter(|p| p.state == "approved") {
        return at_index(1.min(last), "plan", at(p.decided_at.as_deref()));
    }
    at_index(0, "default", None)
}

/// A checked stage move.
#[derive(Clone, Debug, PartialEq)]
pub struct Move {
    pub from: String,
    pub to: String,
    pub forward: bool,
    /// A forward move into one of the project's `operator_stages`.
    pub needs_operator: bool,
    /// The exit criterion of the stage being left — what the mover
    /// asserts is met on a forward move.
    pub exit: String,
}

/// Decide whether `to` is a legal move from `cur`. Forward moves go one
/// stage at a time (each exit criterion is a gate); backward moves —
/// sending an epic back — may go to any earlier stage. From a stage the
/// list does not know, only the first stage is reachable.
pub fn check_move(cfg: &WorkConfig, cur: &StageState, to: &str) -> Result<Move> {
    let Some(target) = cfg.index(to) else {
        return Err(Error::rejected(format!(
            "Unknown stage '{to}' — one of {}",
            cfg.stage_ids().join(" ")
        )));
    };
    let forward = match cur.index {
        Some(i) if i == target => {
            return Err(Error::rejected(format!("already in stage '{to}'")));
        }
        Some(i) if target == i + 1 => true,
        Some(i) if target > i + 1 => {
            return Err(Error::rejected(format!(
                "'{}' → '{to}' skips a stage — move one stage at a time (next: '{}')",
                cur.id,
                cfg.stages[i + 1].id
            )));
        }
        Some(_) => false,
        None if target == 0 => false,
        None => {
            return Err(Error::rejected(format!(
                "stage '{}' is not in this project's list — move it to '{}' first",
                cur.id, cfg.stages[0].id
            )));
        }
    };
    let exit = cur
        .index
        .map(|i| cfg.stages[i].exit.clone())
        .unwrap_or_default();
    Ok(Move {
        from: cur.id.clone(),
        to: to.to_string(),
        forward,
        needs_operator: forward && cfg.operator_stages.iter().any(|s| s == to),
        exit,
    })
}

/// Everything a render needs once: the views by id, each project's
/// work config (with its load error), and the clock.
pub struct Ctx<'a> {
    pub by_id: &'a HashMap<String, &'a View>,
    pub configs: HashMap<String, (WorkConfig, Option<String>)>,
    pub now: i64,
}

impl<'a> Ctx<'a> {
    pub fn new(pm_dir: &Path, by_id: &'a HashMap<String, &'a View>, now: i64) -> Self {
        let configs = project::list(pm_dir)
            .unwrap_or_default()
            .into_iter()
            .map(|p| {
                let cfg = load_config_or_default(pm_dir, &p.key);
                (p.key, cfg)
            })
            .collect();
        Self {
            by_id,
            configs,
            now,
        }
    }

    fn config(&self, key: &str) -> (&WorkConfig, Option<&str>) {
        static DEFAULT: std::sync::OnceLock<WorkConfig> = std::sync::OnceLock::new();
        match self.configs.get(key) {
            Some((cfg, err)) => (cfg, err.as_deref()),
            None => (DEFAULT.get_or_init(WorkConfig::default), None),
        }
    }

    fn kids(&self, view: &View) -> Vec<&'a View> {
        view.children
            .iter()
            .filter_map(|id| self.by_id.get(id).copied())
            .collect()
    }
}

fn is_open(v: &View) -> bool {
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
    let pairs: Vec<(&str, Option<&str>)> = items
        .iter()
        .map(|v| (v.status.as_str(), v.issue.front.size.as_deref()))
        .collect();
    let (done, total) = plan::progress(&pairs);
    let n = |s: &[&str]| {
        items
            .iter()
            .filter(|v| s.contains(&v.status.as_str()))
            .count()
    };
    json!({
        "done_weight": done,
        "total_weight": total,
        "ratio": ratio(done, total),
        "counts": {
            "open": n(&["backlog", "ready"]),
            "doing": n(&["doing"]),
            "review": n(&["review"]),
            "blocked": items.iter().filter(|v| v.blocked && is_open(v)).count(),
            "done": n(&["done"]),
            "dropped": n(&["dropped"]),
            "total": items.len(),
        },
    })
}

/// Health of an epic: `stalled` past 2× the stage limit, `at_risk` when
/// an open child is blocked or the stage limit is passed, else
/// `on_track`. Each reason names the owner and the next action. A
/// terminal stage is never at risk; an unknown entry time skips the
/// time check.
pub fn health_json(
    epic: &View,
    stage: &StageState,
    cfg: &WorkConfig,
    kids: &[&View],
    now: i64,
) -> Value {
    let limit = cfg.stage_limit_days;
    let days = stage.since.map(|s| (now - s).max(0) / 86_400);
    let mut reasons = vec![];
    let mut state = "on_track";
    if !stage.terminal {
        for k in kids.iter().filter(|k| k.blocked && is_open(k)) {
            let f = &k.issue.front;
            let waits = match k.blocked_reason {
                Some(r) => r.to_string(),
                None => format!("waits on {}", f.blocked_by.join(", ")),
            };
            reasons.push(json!({
                "cause": "blocked",
                "issue": f.id,
                "owner": f.owner,
                "detail": format!("{} {waits}", f.id),
                "next": format!("unblock {} or re-plan around it", f.id),
            }));
            state = "at_risk";
        }
        if let Some(d) = days.filter(|d| *d as u64 > limit) {
            let stalled = d as u64 > limit * 2;
            reasons.push(json!({
                "cause": if stalled { "stalled" } else { "stage_time" },
                "issue": epic.issue.front.id,
                "owner": epic.issue.front.owner,
                "detail": format!("{d} days in '{}' (limit {limit})", stage.id),
                "next": format!(
                    "meet the '{}' exit criterion and move the stage, or record why it waits",
                    stage.id
                ),
            }));
            state = if stalled { "stalled" } else { "at_risk" };
        }
    }
    json!({
        "state": state,
        "days_in_stage": days,
        "limit_days": limit,
        "reasons": reasons,
    })
}

/// The `work` block of one issue for the board (list and detail), the
/// CLI and the epic rows: type, milestone, size and weight for every
/// issue; stage, progress and health for epics (`null` otherwise).
pub fn item_json(ctx: &Ctx, view: &View) -> Value {
    let f = &view.issue.front;
    let (cfg, cfg_err) = ctx.config(&view.issue.project);
    let kind = model::item_type(f, view.container);
    let milestone = model::milestone_of(f);
    let mut out = json!({
        "type": kind,
        "type_source": if f.item_type.as_deref().is_some_and(|t| model::TYPES.contains(&t)) {
            "field"
        } else {
            "implicit"
        },
        "milestone": milestone.as_ref().map(|(m, _)| m),
        "milestone_source": milestone.as_ref().map(|(_, s)| s),
        "size": f.size,
        "weight": model::size_weight(f.size.as_deref()),
        "stage": Value::Null,
        "progress": Value::Null,
        "health": Value::Null,
    });
    if kind == "epic" {
        let stage = stage_of(f, cfg);
        let kids = ctx.kids(view);
        let next = stage
            .index
            .and_then(|i| cfg.stages.get(i + 1))
            .map(|s| s.id.clone());
        out["stage"] = json!({
            "id": stage.id,
            "source": stage.source,
            "since": stage.since.map(crate::issue::time::iso),
            "exit": stage.index.map(|i| cfg.stages[i].exit.clone()),
            "next": next,
            "next_needs_operator": next.as_ref().is_some_and(|n| cfg.operator_stages.contains(n)),
            "terminal": stage.terminal,
            "stages": cfg.stage_ids(),
        });
        out["progress"] = progress_json(&kids);
        out["health"] = health_json(view, &stage, cfg, &kids, ctx.now);
    }
    if let Some(err) = cfg_err {
        out["config_error"] = json!(err);
    }
    out
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
    views: &[View],
    project_key: Option<&str>,
    now: i64,
) -> Vec<Value> {
    let by_id: HashMap<String, &View> = views
        .iter()
        .map(|v| (v.issue.front.id.clone(), v))
        .collect();
    let ctx = Ctx::new(pm_dir, &by_id, now);
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

const HEALTH_RANK: &[&str] = &["on_track", "at_risk", "stalled"];

fn worse(a: &str, b: &str) -> &'static str {
    let rank = |s: &str| HEALTH_RANK.iter().position(|h| *h == s).unwrap_or(0);
    HEALTH_RANK[rank(a).max(rank(b))]
}

/// `cadence milestone ls|show`: one row per (project, milestone) —
/// configured in `PROJECT.md` (in its order) or named by an issue's
/// `milestone` / `m<n>-…` tag (natural order after). Progress rolls up
/// the milestone's work items the same way as an epic's: its loose
/// non-epic issues plus every child of its epics, size-weighted;
/// health is the worst of its epics', at risk too when a loose open
/// item is blocked.
pub fn milestones_json(ctx: &Ctx, views: &[View], project_key: Option<&str>) -> Vec<Value> {
    // (project, milestone) → member views, in id order.
    let mut members: BTreeMap<(String, String), Vec<&View>> = BTreeMap::new();
    for v in views {
        if project_key.is_some_and(|p| v.issue.project != p) {
            continue;
        }
        if let Some((m, _)) = model::milestone_of(&v.issue.front) {
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
        for m in &ctx.config(key).0.milestones {
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
    let (cfg, _) = ctx.config(key);
    let conf = cfg.milestones.iter().find(|m| m.id == id);
    let members: &[&View] = members.map(Vec::as_slice).unwrap_or(&[]);
    let mut items: Vec<&View> = vec![];
    let mut seen: HashSet<&str> = HashSet::new();
    let mut epics = vec![];
    let mut health: &'static str = "on_track";
    let mut reasons: Vec<Value> = vec![];
    for v in members {
        if model::item_type(&v.issue.front, v.container) == "epic" {
            let work = item_json(ctx, v);
            let state = work["health"]["state"].as_str().unwrap_or("on_track");
            health = worse(health, state);
            if state != "on_track" {
                reasons.push(json!({
                    "cause": state,
                    "issue": v.issue.front.id,
                    "owner": v.issue.front.owner,
                    "detail": format!("epic {} is {state}", v.issue.front.id),
                    "next": format!("`cadence issue epic show {}`", v.issue.front.id),
                }));
            }
            epics.push(json!({
                "id": v.issue.front.id,
                "title": v.issue.front.title,
                "owner": v.issue.front.owner,
                "stage": work["stage"]["id"],
                "progress": work["progress"]["ratio"],
                "health": state,
            }));
            for k in ctx.kids(v) {
                if seen.insert(k.issue.front.id.as_str()) {
                    items.push(k);
                }
            }
        } else if seen.insert(v.issue.front.id.as_str()) {
            items.push(v);
            if v.blocked && is_open(v) {
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
    }
    let loose: Vec<Value> = members
        .iter()
        .filter(|v| model::item_type(&v.issue.front, v.container) != "epic")
        .map(|v| {
            json!({
                "id": v.issue.front.id,
                "title": v.issue.front.title,
                "type": model::item_type(&v.issue.front, v.container),
                "status": v.status,
                "size": v.issue.front.size,
                "owner": v.issue.front.owner,
                "blocked": v.blocked,
            })
        })
        .collect();
    json!({
        "project": key,
        "id": id,
        "title": conf.and_then(|m| m.title.clone()),
        "exit": conf.and_then(|m| m.exit.clone()),
        "configured": conf.is_some(),
        "progress": progress_json(&items),
        "health": {"state": health, "reasons": reasons},
        "epics": epics,
        "issues": loose,
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
    match rows.len() {
        1 => Ok(rows.into_iter().next().unwrap()),
        0 => Err(Error::rejected(format!(
            "Unknown milestone '{id}' — `cadence milestone ls` lists them"
        ))),
        _ => Err(Error::rejected(format!(
            "Milestone '{id}' exists in {} — pass --project",
            rows.iter()
                .filter_map(|r| r["project"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::board::{views, Issue};

    const DAY: i64 = 86_400;
    const NOW: i64 = 1_790_000_000;

    fn issue(id: &str, status: &str) -> Issue {
        let mut front = Front::new(id, id, "2026-09-01T00:00:00Z");
        front.status = status.to_string();
        Issue {
            project: "cadence".to_string(),
            dir: PathBuf::from("/nonexistent"),
            front,
            body: String::new(),
            comments: vec![],
            artifacts: vec![],
        }
    }

    fn child(id: &str, status: &str, size: Option<&str>) -> Issue {
        let mut i = issue(id, status);
        i.front.parent = Some("CAD-1".into());
        i.front.size = size.map(str::to_string);
        i
    }

    fn ctx_for<'a>(by_id: &'a HashMap<String, &'a View>) -> Ctx<'a> {
        Ctx {
            by_id,
            configs: HashMap::new(),
            now: NOW,
        }
    }

    #[test]
    fn project_md_defaults_and_overrides() {
        let d = parse_config("# Cadence\nno frontmatter\n").unwrap();
        assert_eq!(d, WorkConfig::default());
        assert_eq!(
            d.stage_ids(),
            vec!["shape", "build", "verify", "release", "done"]
        );
        assert_eq!(d.operator_stages, vec!["build", "release"]);
        // Unknown keys belong to other readers — never an error here.
        let d = parse_config("---\nproject: x\nagents: {dev: 4}\n---\n# X\n").unwrap();
        assert_eq!(d, WorkConfig::default());

        let cfg = parse_config(
            "---\nstages: [shape, build, {id: soak, exit: a week in prod}, done]\n\
             stage_limit_days: 3\nmilestones:\n  - {id: m0, title: Safe, exit: restore works}\n\
             ---\n",
        )
        .unwrap();
        assert_eq!(cfg.stage_ids(), vec!["shape", "build", "soak", "done"]);
        assert_eq!(cfg.stages[1].exit, "All tasks done or dropped");
        assert_eq!(cfg.stages[2].exit, "a week in prod");
        assert_eq!(cfg.stage_limit_days, 3);
        assert_eq!(cfg.operator_stages, vec!["build"], "release is not listed");
        assert_eq!(cfg.milestones[0].title.as_deref(), Some("Safe"));

        for (yaml, want) in [
            ("stages: [only]", "at least two"),
            ("stages: [a, a]", "twice"),
            ("stages: [a, B]", "stage 'B'"),
            ("operator_stages: [ship]", "'ship'"),
            ("stage_limit_days: 0", "at least 1"),
            ("milestones: [{id: M1}]", "milestone 'M1'"),
            ("stages: 3", "PROJECT.md"),
        ] {
            let err = parse_config(&format!("---\n{yaml}\n---\n"))
                .unwrap_err()
                .to_string();
            assert!(err.contains(want), "{yaml}: {err}");
        }
    }

    #[test]
    fn stage_resolution_and_plan_mapping() {
        let cfg = WorkConfig::default();
        let mut f = Front::new("CAD-1", "e", "2026-09-01T00:00:00Z");
        let s = stage_of(&f, &cfg);
        assert_eq!(
            (s.id.as_str(), s.source, s.since),
            ("shape", "default", None)
        );

        f.plan = Some(model::Plan {
            state: "proposed".into(),
            proposed_by: "pm".into(),
            proposed_at: "2026-09-02T00:00:00Z".into(),
            tickets: vec![],
            decided_by: None,
            decided_at: None,
            reason: None,
        });
        // A proposed plan is in shape whatever a stage field claims.
        f.stage = Some("release".into());
        assert_eq!(stage_of(&f, &cfg).id, "shape");
        assert_eq!(stage_of(&f, &cfg).source, "plan");

        let plan = f.plan.as_mut().unwrap();
        plan.state = "approved".into();
        plan.decided_at = Some("2026-09-03T00:00:00Z".into());
        f.stage = None;
        let s = stage_of(&f, &cfg);
        assert_eq!((s.id.as_str(), s.source), ("build", "plan"));
        assert_eq!(
            s.since,
            crate::issue::time::parse_iso("2026-09-03T00:00:00Z")
        );

        f.stage = Some("verify".into());
        f.stage_at = Some("2026-09-05T00:00:00Z".into());
        let s = stage_of(&f, &cfg);
        assert_eq!(
            (s.id.as_str(), s.source, s.index),
            ("verify", "field", Some(2))
        );

        f.plan.as_mut().unwrap().state = "rejected".into();
        let s = stage_of(&f, &cfg);
        assert!(s.terminal && s.id == "rejected");

        let mut g = Front::new("CAD-2", "e", "2026-09-01T00:00:00Z");
        g.stage = Some("done".into());
        assert!(stage_of(&g, &cfg).terminal);
        g.stage = Some("limbo".into());
        assert_eq!(stage_of(&g, &cfg).index, None);
    }

    #[test]
    fn moves_are_one_step_forward_any_step_back() {
        let cfg = WorkConfig::default();
        let at = |id: &str| {
            let mut f = Front::new("CAD-1", "e", "2026-09-01T00:00:00Z");
            f.stage = Some(id.to_string());
            stage_of(&f, &cfg)
        };
        let m = check_move(&cfg, &at("shape"), "build").unwrap();
        assert!(
            m.forward && m.needs_operator,
            "shape → build is the operator's"
        );
        assert_eq!(
            m.exit,
            "Goal, non-goals and acceptance written; tasks listed"
        );
        let m = check_move(&cfg, &at("build"), "verify").unwrap();
        assert!(m.forward && !m.needs_operator);
        assert!(
            check_move(&cfg, &at("verify"), "release")
                .unwrap()
                .needs_operator
        );
        assert!(
            !check_move(&cfg, &at("release"), "done")
                .unwrap()
                .needs_operator
        );
        // Back: any earlier stage, by anyone — re-entering forward later
        // needs the operator again.
        let m = check_move(&cfg, &at("verify"), "shape").unwrap();
        assert!(!m.forward && !m.needs_operator);
        for (from, to, want) in [
            ("shape", "verify", "skips a stage"),
            ("build", "build", "already"),
            ("build", "ship", "Unknown stage"),
            ("limbo", "build", "move it to 'shape' first"),
        ] {
            let err = check_move(&cfg, &at(from), to).unwrap_err().to_string();
            assert!(err.contains(want), "{from}→{to}: {err}");
        }
        assert!(!check_move(&cfg, &at("limbo"), "shape").unwrap().forward);
    }

    #[test]
    fn epic_progress_is_weighted_and_health_computed() {
        let mut epic = issue("CAD-1", "backlog");
        epic.front.item_type = Some("epic".into());
        epic.front.stage = Some("build".into());
        epic.front.stage_at = Some(crate::issue::time::iso(NOW - 2 * DAY));
        let mut blocked = child("CAD-4", "ready", None);
        blocked.front.blocked_by = vec!["CAD-9".into()];
        blocked.front.owner = Some("dev-1".into());
        let all = vec![
            epic,
            child("CAD-2", "done", Some("L")),
            child("CAD-3", "doing", Some("S")),
            blocked,
            child("CAD-5", "dropped", Some("L")),
            issue("CAD-9", "doing"),
        ];
        let vs = views(Path::new("/no-notes"), all);
        let by_id: HashMap<String, &View> =
            vs.iter().map(|v| (v.issue.front.id.clone(), v)).collect();
        let ctx = ctx_for(&by_id);
        let w = item_json(&ctx, by_id["CAD-1"]);
        assert_eq!(w["type"], "epic");
        assert_eq!(w["stage"]["id"], "build");
        assert_eq!(w["stage"]["next"], "verify");
        assert_eq!(w["stage"]["next_needs_operator"], false);
        // done L=8 over live L+S+M = 12 (dropped L excluded).
        assert_eq!(w["progress"]["done_weight"], 8);
        assert_eq!(w["progress"]["total_weight"], 12);
        assert_eq!(w["progress"]["ratio"], 0.67);
        assert_eq!(w["progress"]["counts"]["open"], 1);
        assert_eq!(w["progress"]["counts"]["blocked"], 1);
        assert_eq!(w["health"]["state"], "at_risk", "{w}");
        assert_eq!(w["health"]["reasons"][0]["cause"], "blocked");
        assert_eq!(w["health"]["reasons"][0]["owner"], "dev-1");
        assert_eq!(w["health"]["days_in_stage"], 2);

        // A task carries type/weight only.
        let t = item_json(&ctx, by_id["CAD-3"]);
        assert_eq!(
            (t["type"].as_str(), t["weight"].as_u64()),
            (Some("task"), Some(1))
        );
        assert!(t["stage"].is_null() && t["health"].is_null());
    }

    #[test]
    fn health_by_time_in_stage() {
        let cfg = WorkConfig::default();
        let epic_at = |days: i64, stage: &str| {
            let mut e = issue("CAD-1", "backlog");
            e.front.item_type = Some("epic".into());
            e.front.stage = Some(stage.into());
            e.front.stage_at = Some(crate::issue::time::iso(NOW - days * DAY));
            let vs = views(Path::new("/no-notes"), vec![e]);
            let v = &vs[0];
            let s = stage_of(&v.issue.front, &cfg);
            health_json(v, &s, &cfg, &[], NOW)
        };
        assert_eq!(epic_at(5, "build")["state"], "on_track");
        assert_eq!(epic_at(6, "build")["state"], "at_risk");
        assert_eq!(epic_at(6, "build")["reasons"][0]["cause"], "stage_time");
        assert_eq!(epic_at(10, "build")["state"], "at_risk");
        assert_eq!(epic_at(11, "build")["state"], "stalled");
        assert_eq!(epic_at(40, "done")["state"], "on_track", "terminal");
        // Unknown entry time (never moved) skips the time check.
        let e = issue("CAD-1", "backlog");
        let vs = views(Path::new("/no-notes"), vec![e]);
        let s = stage_of(&vs[0].issue.front, &cfg);
        let h = health_json(&vs[0], &s, &cfg, &[], NOW);
        assert_eq!(
            (h["state"].as_str(), h["days_in_stage"].is_null()),
            (Some("on_track"), true)
        );
    }

    #[test]
    fn milestones_roll_up_epics_and_loose_items_from_tags() {
        let mut epic = issue("CAD-1", "backlog");
        epic.front.tags = vec!["m2-one-team".into()];
        epic.front.stage = Some("build".into());
        epic.front.stage_at = Some(crate::issue::time::iso(NOW - 20 * DAY));
        let mut loose = issue("CAD-6", "done");
        loose.front.milestone = Some("m2".into());
        loose.front.size = Some("S".into());
        let mut other = issue("CAD-7", "doing");
        other.front.tags = vec!["m10-later".into()];
        let all = vec![
            epic,
            child("CAD-2", "done", Some("M")),
            child("CAD-3", "ready", Some("M")),
            loose,
            other,
        ];
        let vs = views(Path::new("/no-notes"), all);
        let by_id: HashMap<String, &View> =
            vs.iter().map(|v| (v.issue.front.id.clone(), v)).collect();
        let mut ctx = ctx_for(&by_id);
        let mut cfg = WorkConfig::default();
        cfg.milestones = vec![
            Milestone {
                id: "m0".into(),
                title: Some("Safe".into()),
                exit: Some("restore".into()),
            },
            Milestone {
                id: "m2".into(),
                title: Some("One team".into()),
                exit: None,
            },
        ];
        ctx.configs.insert("cadence".into(), (cfg, None));
        let rows = milestones_json(&ctx, &vs, None);
        let ids: Vec<&str> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["m0", "m2", "m10"], "configured first, then found");
        let m2 = &rows[1];
        assert_eq!(m2["title"], "One team");
        // Items: CAD-2 (M done), CAD-3 (M), CAD-6 (S done) → 4 / 7.
        assert_eq!(m2["progress"]["done_weight"], 4, "{m2}");
        assert_eq!(m2["progress"]["total_weight"], 7, "{m2}");
        assert_eq!(m2["epics"][0]["id"], "CAD-1");
        assert_eq!(m2["health"]["state"], "stalled", "20 days in build: {m2}");
        assert_eq!(m2["issues"][0]["id"], "CAD-6");
        assert_eq!(rows[0]["progress"]["total_weight"], 0);
        assert_eq!(rows[2]["configured"], false);

        let show = milestone_show(&ctx, &vs, "m2", None).unwrap();
        assert_eq!(show["id"], "m2");
        assert!(milestone_show(&ctx, &vs, "m9", None).is_err());
    }
}
