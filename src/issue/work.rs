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
//! The gate keys — `stages` and `operator_stages` — decide who may
//! move an epic where, so an edit to them takes effect only once the
//! operator approves it (`project_work_approve`, recorded in the daemon
//! store with who and when). Until then every reader and every stage
//! move uses the default gates and reports `config_unapproved`;
//! `milestones` and `stage_limit_days` stay freely editable.
//!
//! Progress reuses the plan's size weights ([`plan::progress`]). Health
//! is `on_track`, `at_risk` (an open child is blocked, or the epic has
//! sat in its stage longer than the limit) or `stalled` (in the stage
//! for 2× the limit or more — time in stage, not child activity).

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
    match read_project_md(pm_dir, key)? {
        None => Ok(WorkConfig::default()),
        Some(text) => parse_config(&text).map_err(|e| Error::rejected(format!("{key}: {e}"))),
    }
}

/// `<pm>/<key>/PROJECT.md`'s text — `None` when there is no file; a
/// symlinked or unreadable one refuses. Every PROJECT.md reader (the
/// work model here, CAD-378's `areas:`) loads through this.
pub fn read_project_md(pm_dir: &Path, key: &str) -> Result<Option<String>> {
    let file = config_file(pm_dir, key);
    if file.symlink_metadata().is_err() {
        return Ok(None);
    }
    if !board::is_real_file(&file) {
        return Err(Error::rejected(format!(
            "{} is not a regular file — the board never follows links",
            file.display()
        )));
    }
    std::fs::read_to_string(&file)
        .map(Some)
        .map_err(|e| Error::rejected(format!("cannot read {}: {e}", file.display())))
}

/// Lenient load for readers: a bad file reads as the defaults plus the
/// error, which views surface as `config_error` and lint warns about.
pub fn load_config_or_default(pm_dir: &Path, key: &str) -> (WorkConfig, Option<String>) {
    match load_config(pm_dir, key) {
        Ok(cfg) => (cfg, None),
        Err(e) => (WorkConfig::default(), Some(e.to_string())),
    }
}

/// Project key → the gate digest the operator last approved.
pub type Approvals = HashMap<String, String>;

/// The normalized gate keys: the stage ids in order and the operator
/// stages as a set. Milestones and the limit are not gates.
pub fn gate_keys(cfg: &WorkConfig) -> String {
    let mut ops = cfg.operator_stages.clone();
    ops.sort();
    ops.dedup();
    format!(
        "stages={}\noperator_stages={}",
        cfg.stage_ids().join(","),
        ops.join(",")
    )
}

/// `sha256:<hex>` of [`gate_keys`] — what an approval records.
pub fn gate_digest(cfg: &WorkConfig) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(gate_keys(cfg).as_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// The gates are the defaults — nothing to approve.
pub fn gates_default(cfg: &WorkConfig) -> bool {
    gate_keys(cfg) == gate_keys(&WorkConfig::default())
}

/// The config a reader or a stage move uses: the file's own when its
/// gate keys are the defaults or match the operator's approved digest;
/// else the default gates (keeping the file's milestones and limit)
/// plus the `config_unapproved` note. An agent editing PROJECT.md can
/// therefore never reorder, drop, rename or un-gate a stage.
pub fn effective(
    key: &str,
    cfg: WorkConfig,
    approved: Option<&str>,
) -> (WorkConfig, Option<String>) {
    if gates_default(&cfg) || approved == Some(gate_digest(&cfg).as_str()) {
        return (cfg, None);
    }
    let d = WorkConfig::default();
    let note = format!(
        "config_unapproved: {key}/PROJECT.md stages/operator_stages differ from the defaults \
         and are not operator-approved ({}) — the default stages apply until \
         `cadence issue project approve-work {key}`",
        gate_digest(&cfg)
    );
    (
        WorkConfig {
            stages: d.stages,
            operator_stages: d.operator_stages,
            ..cfg
        },
        Some(note),
    )
}

/// Every project's approved gate digest, from the daemon. An
/// unreachable daemon is no approvals — custom gates then read as the
/// defaults, the fail-safe direction.
pub fn fetch_approvals(state_dir: &Path) -> Approvals {
    crate::client::rpc(state_dir, "project_work_approvals", json!({}))
        .ok()
        .and_then(|v| v["approvals"].as_object().cloned())
        .map(|m| {
            m.into_iter()
                .filter_map(|(k, v)| v["digest"].as_str().map(|d| (k, d.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// An epic's effective stage and where it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct StageState {
    pub id: String,
    /// `field` (a recorded move), `plan` (mapped from the plan state),
    /// `status` (a done epic never moved) or `default` (never moved —
    /// the first stage).
    pub source: &'static str,
    /// When the epic entered the stage; `None` when unknown.
    pub since: Option<i64>,
    /// Position in the project's list; `None` for a stage the list does
    /// not have (a hand edit, a changed PROJECT.md) or `rejected`.
    pub index: Option<usize>,
    /// Last stage, or a rejected plan — no further health checks.
    pub terminal: bool,
}

/// The earliest stage an epic may be in: an approved plan owns the
/// first stage (shaping is the plan's proposal), so it never goes back
/// before the second — to reshape, reject or re-propose the plan.
pub fn floor(front: &Front, cfg: &WorkConfig) -> usize {
    match &front.plan {
        Some(p) if p.state == "approved" => 1.min(cfg.stages.len() - 1),
        _ => 0,
    }
}

/// The stage of an epic. A plan's state bounds it: `proposed` is always
/// the first stage (nothing may start), `rejected` is terminal, and an
/// approved plan is never before its [`floor`] (the second stage, where
/// it reads when no move is recorded). Else a recorded `stage`, else
/// the last stage for an epic whose status is `done` (`done`), else the
/// first stage. A dropped `stage` (an older binary rewrote the file)
/// therefore reads earlier, never later.
pub fn stage_of(front: &Front, cfg: &WorkConfig, done: bool) -> StageState {
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
    let floor = floor(front, cfg);
    let plan_at = || at(front.plan.as_ref().and_then(|p| p.decided_at.as_deref()));
    if let Some(stage) = front.stage.as_deref().filter(|s| !s.is_empty()) {
        let since = at(front.stage_at.as_deref());
        return match cfg.index(stage) {
            Some(i) if i < floor => at_index(floor, "plan", plan_at()),
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
    if floor > 0 {
        return at_index(floor, "plan", plan_at());
    }
    if done {
        return at_index(last, "status", None);
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
/// sending an epic back — may go to any earlier stage down to `floor`,
/// and need the operator when they land in an operator stage from a
/// stage that was never entered; any move out of a status-derived
/// stage needs the operator
/// ([`floor`]: an approved plan's first stage belongs to the plan). From
/// a stage the list does not know, only the floor stage is reachable.
pub fn check_move(cfg: &WorkConfig, cur: &StageState, to: &str, floor: usize) -> Result<Move> {
    let Some(target) = cfg.index(to) else {
        return Err(Error::rejected(format!(
            "Unknown stage '{to}' — one of {}",
            cfg.stage_ids().join(" ")
        )));
    };
    if target < floor {
        return Err(Error::invalid(
            "plan_owns_stage",
            format!(
                "an approved plan's epic is never before '{}' — the plan owns '{to}'; \
                 reject or re-propose the plan to reshape it",
                cfg.stages[floor].id
            ),
        ));
    }
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
        None if target == floor => false,
        None => {
            return Err(Error::rejected(format!(
                "stage '{}' is not in this project's list — move it to '{}' first",
                cur.id, cfg.stages[floor].id
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
        // A stage read off the status (a done epic never moved) was never
        // entered, so every move out of it is the operator's — whatever
        // the target, or a pane could step to a non-operator stage and
        // then "back" into build. From an entered stage (recorded by a
        // move, or mapped from the approved plan) a move into an
        // operator stage needs the operator unless it goes back. The
        // `default` source is always the floor (first) stage: its only
        // move is one step forward, under that same rule.
        needs_operator: cur.source == "status"
            || (cfg.operator_stages.iter().any(|s| s == to)
                && (forward || !matches!(cur.source, "field" | "plan"))),
        exit,
    })
}

/// A plan that is not approved owns its epic's stage: no move, by
/// anyone — the one rule [`crate::issue::write::move_stage`] and
/// [`legal_moves`] share.
pub fn plan_allows_moves(front: &Front) -> Result<()> {
    match &front.plan {
        Some(plan) if plan.state != "approved" => Err(Error::invalid(
            "plan_not_approved",
            format!(
                "{} is a {} plan — its stage follows the plan: \
                 `cadence plan approve {}` moves it to build",
                front.id, plan.state, front.id
            ),
        )),
        _ => Ok(()),
    }
}

/// Every stage `epic_stage` would accept from `cur` right now —
/// [`check_move`] over the whole list, so a view offers exactly the
/// moves the writer allows and never re-derives the rule. A plan that
/// is not approved owns the stage: no moves (`move_stage` refuses them).
/// `needs_operator` marks the moves only the proven operator may make.
pub fn legal_moves(front: &Front, cfg: &WorkConfig, cur: &StageState) -> Vec<Value> {
    if plan_allows_moves(front).is_err() {
        return vec![];
    }
    let floor = floor(front, cfg);
    cfg.stages
        .iter()
        .filter_map(|s| check_move(cfg, cur, &s.id, floor).ok())
        .map(|mv| {
            json!({
                "to": mv.to,
                "forward": mv.forward,
                "needs_operator": mv.needs_operator,
            })
        })
        .collect()
}

/// One project's work settings as a render uses them.
#[derive(Clone, Debug)]
pub struct ProjectWork {
    /// The effective config ([`effective`]).
    pub cfg: WorkConfig,
    /// PROJECT.md could not be read or parsed — the defaults apply.
    pub error: Option<String>,
    /// Its gate keys are not operator-approved — the default gates apply.
    pub unapproved: Option<String>,
}

impl ProjectWork {
    /// Load `key`'s PROJECT.md and apply the approvals.
    pub fn load(pm_dir: &Path, key: &str, approvals: &Approvals) -> Self {
        let (raw, error) = load_config_or_default(pm_dir, key);
        let (cfg, unapproved) = effective(key, raw, approvals.get(key).map(String::as_str));
        Self {
            cfg,
            error,
            unapproved,
        }
    }
}

/// Everything a render needs once: the views by id, each project's
/// effective work config, and the clock.
pub struct Ctx<'a> {
    pub by_id: &'a HashMap<String, &'a View>,
    pub configs: HashMap<String, ProjectWork>,
    pub now: i64,
}

impl<'a> Ctx<'a> {
    pub fn new(
        pm_dir: &Path,
        by_id: &'a HashMap<String, &'a View>,
        now: i64,
        approvals: &Approvals,
    ) -> Self {
        let configs = project::list(pm_dir)
            .unwrap_or_default()
            .into_iter()
            .map(|p| {
                let work = ProjectWork::load(pm_dir, &p.key, approvals);
                (p.key, work)
            })
            .collect();
        Self {
            by_id,
            configs,
            now,
        }
    }

    fn work(&self, key: &str) -> Option<&ProjectWork> {
        self.configs.get(key)
    }

    fn config(&self, key: &str) -> &WorkConfig {
        static DEFAULT: std::sync::OnceLock<WorkConfig> = std::sync::OnceLock::new();
        match self.configs.get(key) {
            Some(w) => &w.cfg,
            None => DEFAULT.get_or_init(WorkConfig::default),
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
            reasons.push(json!({
                "cause": "blocked",
                "issue": kf.id,
                "owner": kf.owner,
                "detail": format!("{} {waits}", kf.id),
                "next": format!("unblock {} or re-plan around it", kf.id),
            }));
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
            reasons.push(json!({
                "cause": if stalled { "stalled" } else { "stage_time" },
                "issue": f.id,
                "owner": f.owner,
                "detail": format!("{} days in '{}' (limit {limit})", e / DAY, stage.id),
                "next": next,
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
    let cfg = ctx.config(&view.issue.project);
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
        let stage = stage_of(f, cfg, view.status == "done");
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
            // A PROJECT.md the writer cannot load refuses every move.
            "moves": if ctx.work(&view.issue.project).is_some_and(|w| w.error.is_some()) {
                vec![]
            } else {
                legal_moves(f, cfg, &stage)
            },
        });
        out["progress"] = progress_json(&kids);
        out["health"] = health_json(view, &stage, cfg, &kids, ctx.now);
    }
    if let Some(w) = ctx.work(&view.issue.project) {
        if let Some(err) = &w.error {
            out["config_error"] = json!(err);
        }
        if let Some(note) = &w.unapproved {
            out["config_unapproved"] = json!(note);
        }
    }
    out
}

/// The drawer payload ([`board::detail_json`]) with its `work` block —
/// `GET /api/issues/:id` and `issue show --json`.
pub fn detail_json(pm_dir: &Path, ctx: &Ctx, view: &View) -> Value {
    let mut detail = board::detail_json(pm_dir, view, ctx.by_id);
    detail["work"] = item_json(ctx, view);
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
    views: &[View],
    project_key: Option<&str>,
    now: i64,
    approvals: &Approvals,
) -> Vec<Value> {
    let by_id: HashMap<String, &View> = views
        .iter()
        .map(|v| (v.issue.front.id.clone(), v))
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
    fn gate_keys_apply_only_when_approved() {
        let d = WorkConfig::default();
        assert!(gates_default(&d));
        let (cfg, note) = effective("x", d.clone(), None);
        assert_eq!((cfg, note), (d.clone(), None), "defaults need no approval");
        // Milestones and the limit are not gates.
        let mild = parse_config("---\nstage_limit_days: 2\nmilestones: [{id: m1}]\n---\n").unwrap();
        assert!(gates_default(&mild));
        for yaml in [
            "stages: [shape, verify, build, release, done]",
            "stages: [build, verify, release, done]",
            "stages: [shape, construct, verify, ship, done]",
            "operator_stages: []",
        ] {
            let raw = parse_config(&format!("---\n{yaml}\nstage_limit_days: 3\n---\n")).unwrap();
            assert!(!gates_default(&raw), "{yaml}");
            let (cfg, note) = effective("x", raw.clone(), None);
            assert_eq!(cfg.stage_ids(), d.stage_ids(), "{yaml}");
            assert_eq!(cfg.operator_stages, d.operator_stages, "{yaml}");
            assert_eq!(cfg.stage_limit_days, 3, "{yaml}: the limit still applies");
            assert!(note.unwrap().starts_with("config_unapproved"), "{yaml}");
            let (cfg, note) = effective("x", raw.clone(), Some("sha256:stale"));
            assert!(note.is_some() && cfg.stage_ids() == d.stage_ids(), "{yaml}");
            let digest = gate_digest(&raw);
            let (cfg, note) = effective("x", raw.clone(), Some(&digest));
            assert_eq!((cfg, note), (raw, None), "{yaml}: approved");
        }
        // Operator-stage order is not a change.
        let a = parse_config("---\noperator_stages: [release, build]\n---\n").unwrap();
        assert!(gates_default(&a));
    }

    #[test]
    fn stage_resolution_and_plan_mapping() {
        let cfg = WorkConfig::default();
        let mut f = Front::new("CAD-1", "e", "2026-09-01T00:00:00Z");
        let s = stage_of(&f, &cfg, false);
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
            workflow: None,
        });
        // A proposed plan is in shape whatever a stage field claims.
        f.stage = Some("release".into());
        assert_eq!(stage_of(&f, &cfg, false).id, "shape");
        assert_eq!(stage_of(&f, &cfg, false).source, "plan");

        let plan = f.plan.as_mut().unwrap();
        plan.state = "approved".into();
        plan.decided_at = Some("2026-09-03T00:00:00Z".into());
        f.stage = None;
        let s = stage_of(&f, &cfg, false);
        assert_eq!((s.id.as_str(), s.source), ("build", "plan"));
        assert_eq!(
            s.since,
            crate::issue::time::parse_iso("2026-09-03T00:00:00Z")
        );

        f.stage = Some("verify".into());
        f.stage_at = Some("2026-09-05T00:00:00Z".into());
        let s = stage_of(&f, &cfg, false);
        assert_eq!(
            (s.id.as_str(), s.source, s.index),
            ("verify", "field", Some(2))
        );

        // An approved plan never reads before build — the plan owns shape,
        // including a hand-edited or stale `stage: shape`.
        f.stage = Some("shape".into());
        let s = stage_of(&f, &cfg, false);
        assert_eq!((s.id.as_str(), s.source), ("build", "plan"));
        assert_eq!(floor(&f, &cfg), 1);
        let err = check_move(&cfg, &stage_of(&f, &cfg, false), "shape", 1)
            .unwrap_err()
            .to_string();
        assert!(err.contains("the plan owns 'shape'"), "{err}");

        f.plan.as_mut().unwrap().state = "rejected".into();
        let s = stage_of(&f, &cfg, false);
        assert!(s.terminal && s.id == "rejected");

        let mut g = Front::new("CAD-2", "e", "2026-09-01T00:00:00Z");
        g.stage = Some("done".into());
        assert!(stage_of(&g, &cfg, false).terminal);
        g.stage = Some("limbo".into());
        assert_eq!(stage_of(&g, &cfg, false).index, None);
        // Migration: a done epic never moved reads the last stage.
        let h = Front::new("CAD-3", "e", "2026-09-01T00:00:00Z");
        let s = stage_of(&h, &cfg, true);
        assert_eq!(
            (s.id.as_str(), s.source, s.terminal),
            ("done", "status", true)
        );
        assert_eq!(stage_of(&h, &cfg, false).id, "shape");
    }

    #[test]
    fn moves_are_one_step_forward_any_step_back() {
        let cfg = WorkConfig::default();
        let at = |id: &str| {
            let mut f = Front::new("CAD-1", "e", "2026-09-01T00:00:00Z");
            f.stage = Some(id.to_string());
            stage_of(&f, &cfg, false)
        };
        let m = check_move(&cfg, &at("shape"), "build", 0).unwrap();
        assert!(
            m.forward && m.needs_operator,
            "shape → build is the operator's"
        );
        assert_eq!(
            m.exit,
            "Goal, non-goals and acceptance written; tasks listed"
        );
        let m = check_move(&cfg, &at("build"), "verify", 0).unwrap();
        assert!(m.forward && !m.needs_operator);
        assert!(
            check_move(&cfg, &at("verify"), "release", 0)
                .unwrap()
                .needs_operator
        );
        assert!(
            !check_move(&cfg, &at("release"), "done", 0)
                .unwrap()
                .needs_operator
        );
        // Back: any earlier stage, by anyone — re-entering forward later
        // needs the operator again.
        let m = check_move(&cfg, &at("verify"), "shape", 0).unwrap();
        assert!(!m.forward && !m.needs_operator);
        let m = check_move(&cfg, &at("done"), "release", 0).unwrap();
        assert!(!m.forward && !m.needs_operator, "recorded done → release");
        // Source × target class → does the move need the operator?
        let with_source = |id: &str, source: &'static str| {
            let mut s = at(id);
            s.source = source;
            s
        };
        let derived = stage_of(
            &Front::new("CAD-9", "e", "2026-09-01T00:00:00Z"),
            &cfg,
            true,
        );
        assert_eq!((derived.id.as_str(), derived.source), ("done", "status"));
        let default = stage_of(
            &Front::new("CAD-9", "e", "2026-09-01T00:00:00Z"),
            &cfg,
            false,
        );
        assert_eq!((default.id.as_str(), default.source), ("shape", "default"));
        for (cur, to, want, class) in [
            // Entered stages (`field`, `plan`): forward into an operator
            // stage needs the operator; back and non-operator do not.
            (with_source("shape", "field"), "build", true, "field fwd op"),
            (
                with_source("build", "field"),
                "verify",
                false,
                "field fwd plain",
            ),
            (
                with_source("done", "field"),
                "release",
                false,
                "field back op",
            ),
            (
                with_source("release", "field"),
                "verify",
                false,
                "field back plain",
            ),
            (
                with_source("verify", "field"),
                "shape",
                false,
                "field back floor",
            ),
            (
                with_source("verify", "plan"),
                "release",
                true,
                "plan fwd op",
            ),
            (
                with_source("build", "plan"),
                "verify",
                false,
                "plan fwd plain",
            ),
            (
                with_source("verify", "plan"),
                "build",
                false,
                "plan back op",
            ),
            // Status-derived: every move is the operator's.
            (derived.clone(), "release", true, "status back op"),
            (derived.clone(), "build", true, "status back op 2"),
            (derived.clone(), "verify", true, "status back plain"),
            (derived.clone(), "shape", true, "status back floor"),
            // Default (always the floor): forward under the normal rule.
            (default.clone(), "build", true, "default fwd op"),
        ] {
            let m = check_move(&cfg, &cur, to, 0).unwrap();
            assert_eq!(m.needs_operator, want, "{class}: {} → {to}", cur.id);
        }
        let open = WorkConfig {
            operator_stages: vec![],
            ..WorkConfig::default()
        };
        assert!(
            !check_move(&open, &default, "build", 0)
                .unwrap()
                .needs_operator,
            "default fwd plain (no operator stages)"
        );
        assert!(
            check_move(&open, &derived, "verify", 0)
                .unwrap()
                .needs_operator,
            "status needs the operator even with no operator stages"
        );
        for (from, to, want) in [
            ("shape", "verify", "skips a stage"),
            ("build", "build", "already"),
            ("build", "ship", "Unknown stage"),
            ("limbo", "build", "move it to 'shape' first"),
        ] {
            let err = check_move(&cfg, &at(from), to, 0).unwrap_err().to_string();
            assert!(err.contains(want), "{from}→{to}: {err}");
        }
        assert!(!check_move(&cfg, &at("limbo"), "shape", 0).unwrap().forward);
    }

    /// CAD-432: the board offers exactly the moves `check_move` accepts
    /// — one step forward, any step back to the floor — with the
    /// operator flag the writer applies; a plan that is not approved
    /// offers none.
    #[test]
    fn legal_moves_mirror_check_move() {
        let cfg = WorkConfig::default();
        let moves = |f: &Front, done: bool| -> Vec<(String, bool, bool)> {
            legal_moves(f, &cfg, &stage_of(f, &cfg, done))
                .iter()
                .map(|m| {
                    (
                        m["to"].as_str().unwrap().to_string(),
                        m["forward"].as_bool().unwrap(),
                        m["needs_operator"].as_bool().unwrap(),
                    )
                })
                .collect()
        };
        let own = |s: &str| (s.to_string(), false, false);
        let mut f = Front::new("CAD-1", "e", "2026-09-01T00:00:00Z");
        assert_eq!(moves(&f, false), vec![("build".to_string(), true, true)]);
        f.stage = Some("verify".into());
        assert_eq!(
            moves(&f, false),
            vec![
                own("shape"),
                own("build"),
                ("release".to_string(), true, true)
            ]
        );
        f.stage = Some("build".into());
        assert_eq!(
            moves(&f, false),
            vec![own("shape"), ("verify".to_string(), true, false)]
        );
        f.stage = Some("done".into());
        assert_eq!(
            moves(&f, false),
            vec![own("shape"), own("build"), own("verify"), own("release")]
        );
        // A done epic never moved: every move out is the operator's.
        let derived = Front::new("CAD-2", "e", "2026-09-01T00:00:00Z");
        assert!(moves(&derived, true).iter().all(|(_, fwd, op)| !fwd && *op));
        // An approved plan owns the first stage; an unapproved one owns
        // the stage outright.
        let mut p = Front::new("CAD-3", "e", "2026-09-01T00:00:00Z");
        p.plan = Some(model::Plan {
            state: "approved".into(),
            proposed_by: "operator".into(),
            proposed_at: "2026-09-01T00:00:00Z".into(),
            tickets: vec![],
            decided_by: Some("operator".into()),
            decided_at: Some("2026-09-02T00:00:00Z".into()),
            reason: None,
            workflow: None,
        });
        assert_eq!(moves(&p, false), vec![("verify".to_string(), true, false)]);
        for state in ["proposed", "rejected"] {
            p.plan.as_mut().unwrap().state = state.into();
            assert!(moves(&p, false).is_empty(), "{state}");
        }
    }

    /// CAD-432 review: a PROJECT.md the writer cannot load refuses every
    /// move, so the card offers none; a plan that is not approved is the
    /// same shared rule the writer applies.
    #[test]
    fn no_moves_on_a_broken_project_md_or_unapproved_plan() {
        let mut epic = issue("CAD-1", "backlog");
        epic.front.item_type = Some("epic".into());
        epic.front.stage = Some("build".into());
        let vs = views(Path::new("/nonexistent"), vec![epic]);
        let by_id: HashMap<String, &View> =
            vs.iter().map(|v| (v.issue.front.id.clone(), v)).collect();
        let mut ctx = ctx_for(&by_id);
        let moves = |ctx: &Ctx| item_json(ctx, &vs[0])["stage"]["moves"].clone();
        assert_eq!(moves(&ctx).as_array().unwrap().len(), 2, "{}", moves(&ctx));
        ctx.configs.insert(
            "cadence".into(),
            ProjectWork {
                cfg: WorkConfig::default(),
                error: Some("PROJECT.md frontmatter: bad".into()),
                unapproved: None,
            },
        );
        assert_eq!(moves(&ctx), json!([]));
        let mut f = Front::new("CAD-2", "e", "2026-09-01T00:00:00Z");
        assert!(plan_allows_moves(&f).is_ok());
        f.plan = Some(model::Plan {
            state: "proposed".into(),
            proposed_by: "operator".into(),
            proposed_at: "2026-09-01T00:00:00Z".into(),
            tickets: vec![],
            decided_by: None,
            decided_at: None,
            reason: None,
            workflow: None,
        });
        let err = plan_allows_moves(&f).unwrap_err().to_string();
        assert!(err.contains("CAD-2 is a proposed plan"), "{err}");
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
        let epic_at = |secs: i64, stage: &str| {
            let mut e = issue("CAD-1", "backlog");
            e.front.item_type = Some("epic".into());
            e.front.stage = Some(stage.into());
            e.front.stage_at = Some(crate::issue::time::iso(NOW - secs));
            let vs = views(Path::new("/no-notes"), vec![e]);
            let v = &vs[0];
            let s = stage_of(&v.issue.front, &cfg, false);
            health_json(v, &s, &cfg, &[], NOW)
        };
        // Seconds in the stage: at risk strictly after the 5-day limit,
        // stalled from exactly 2× it.
        assert_eq!(epic_at(5 * DAY, "build")["state"], "on_track");
        assert_eq!(epic_at(5 * DAY + 1, "build")["state"], "at_risk");
        assert_eq!(
            epic_at(6 * DAY, "build")["reasons"][0]["cause"],
            "stage_time"
        );
        assert_eq!(epic_at(10 * DAY - 1, "build")["state"], "at_risk");
        assert_eq!(epic_at(10 * DAY, "build")["state"], "stalled");
        assert_eq!(epic_at(40 * DAY, "done")["state"], "on_track", "terminal");
        // A proposed plan waits on the operator, not on a stage move.
        let mut e = issue("CAD-1", "backlog");
        e.front.plan = Some(model::Plan {
            state: "proposed".into(),
            proposed_by: "pm".into(),
            proposed_at: crate::issue::time::iso(NOW - 6 * DAY),
            tickets: vec![],
            decided_by: None,
            decided_at: None,
            reason: None,
            workflow: None,
        });
        let vs = views(Path::new("/no-notes"), vec![e]);
        let s = stage_of(&vs[0].issue.front, &cfg, false);
        let h = health_json(&vs[0], &s, &cfg, &[], NOW);
        let next = h["reasons"][0]["next"].as_str().unwrap();
        assert!(
            next.starts_with("approve the plan (`cadence plan approve CAD-1`)"),
            "{h}"
        );
        // Unknown entry time (never moved) skips the time check.
        let e = issue("CAD-1", "backlog");
        let vs = views(Path::new("/no-notes"), vec![e]);
        let s = stage_of(&vs[0].issue.front, &cfg, false);
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
        let cfg = WorkConfig {
            milestones: vec![
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
            ],
            ..WorkConfig::default()
        };
        ctx.configs.insert(
            "cadence".into(),
            ProjectWork {
                cfg,
                error: None,
                unapproved: None,
            },
        );
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
