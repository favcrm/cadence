//! Work-model configuration parsing and loading for `issue::work`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::issue::areas::scrub;
use crate::issue::{board, model, parse};

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

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneStatus {
    #[default]
    Planned,
    Active,
    Achieved,
    Cancelled,
}

impl MilestoneStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Active => "active",
            Self::Achieved => "achieved",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Milestone {
    pub id: String,
    pub title: Option<String>,
    pub exit: Option<String>,
    pub description: Option<String>,
    pub owner: Option<String>,
    pub start_date: Option<String>,
    pub target_date: Option<String>,
    pub status: MilestoneStatus,
    pub completed_date: Option<String>,
    pub evidence: Vec<String>,
    pub depends_on: Vec<String>,
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
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    start_date: Option<String>,
    #[serde(default)]
    target_date: Option<String>,
    #[serde(default)]
    status: MilestoneStatus,
    #[serde(default)]
    completed_date: Option<String>,
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    depends_on: Vec<String>,
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
    let raw: Raw = serde_yaml::from_str(yaml).map_err(|e| {
        Error::rejected(format!("PROJECT.md frontmatter: {}", scrub(&e.to_string())))
    })?;
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
            description: m.description,
            owner: m.owner,
            start_date: m.start_date,
            target_date: m.target_date,
            status: m.status,
            completed_date: m.completed_date,
            evidence: m.evidence,
            depends_on: m.depends_on,
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
                scrub(&s.id)
            ));
        }
        if !seen.insert(s.id.as_str()) {
            return bad(format!("stage '{}' is listed twice", scrub(&s.id)));
        }
    }
    if let Some(op) = cfg.operator_stages.iter().find(|o| cfg.index(o).is_none()) {
        return bad(format!(
            "operator_stages names '{}', which is not a stage",
            scrub(op)
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
                scrub(&m.id)
            ));
        }
        if !seen.insert(m.id.as_str()) {
            return bad(format!("milestone '{}' is listed twice", scrub(&m.id)));
        }
        for (field, date) in [
            ("start_date", &m.start_date),
            ("target_date", &m.target_date),
            ("completed_date", &m.completed_date),
        ] {
            if let Some(date) = date {
                if date_epoch(date).is_none() {
                    return bad(format!(
                        "milestone '{}' {field} must be a real YYYY-MM-DD date",
                        scrub(&m.id)
                    ));
                }
            }
        }
        if let (Some(start), Some(target)) = (&m.start_date, &m.target_date) {
            if start > target {
                return bad(format!(
                    "milestone '{}' start_date is after target_date",
                    scrub(&m.id)
                ));
            }
        }
        if m.completed_date.is_some() && m.status != MilestoneStatus::Achieved {
            return bad(format!(
                "milestone '{}' completed_date requires status: achieved",
                scrub(&m.id)
            ));
        }
    }
    for m in &cfg.milestones {
        for dependency in &m.depends_on {
            if !seen.contains(dependency.as_str()) || dependency == &m.id {
                return bad(format!(
                    "milestone '{}' depends_on must name another declared milestone: '{}'",
                    scrub(&m.id),
                    scrub(dependency)
                ));
            }
        }
    }
    // Remove resolvable nodes iteratively: no recursion even for a long roadmap.
    let mut remaining: HashSet<&str> = cfg.milestones.iter().map(|m| m.id.as_str()).collect();
    while !remaining.is_empty() {
        let ready: Vec<&str> = cfg
            .milestones
            .iter()
            .filter(|m| {
                remaining.contains(m.id.as_str())
                    && m.depends_on.iter().all(|d| !remaining.contains(d.as_str()))
            })
            .map(|m| m.id.as_str())
            .collect();
        if ready.is_empty() {
            return bad("milestone depends_on contains a cycle".into());
        }
        for id in ready {
            remaining.remove(id);
        }
    }
    Ok(())
}

pub(super) fn date_epoch(date: &str) -> Option<i64> {
    if date.len() != 10 {
        return None;
    }
    crate::issue::time::parse_iso(&format!("{date}T00:00:00Z"))
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
