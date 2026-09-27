use std::collections::HashMap;

use serde_json::{json, Value};

use super::CI_WORKFLOW;

/// First-parent SHAs of the default branch the overview classifies.
const MAIN_CI_SHAS: usize = 20;

/// One default-branch SHA's own `ci.yml` verdict. Only a SHA's own
/// successful push run makes it `Passed` — never absence, another
/// workflow, or a later SHA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CiState {
    Passed,
    /// `failure`, `timed_out`, `startup_failure`.
    Failed,
    /// The run exists and has not completed.
    Pending,
    /// The run completed without a verdict: `cancelled`, or `skipped`,
    /// `neutral`, `stale`, `action_required` (see `conclusion`).
    Cancelled,
    /// No `ci.yml` push run for this SHA at all.
    Missing,
}

impl CiState {
    fn as_str(self) -> &'static str {
        match self {
            CiState::Passed => "passed",
            CiState::Failed => "failed",
            CiState::Pending => "pending",
            CiState::Cancelled => "cancelled",
            CiState::Missing => "missing",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShaCi {
    pub sha: String,
    pub state: CiState,
    /// Cancelled/missing only: the nearest later SHA whose own run
    /// passed. The SHA itself stays unverified — covered, not passed.
    pub covered_by: Option<String>,
    /// The run that decided `state` — `None` when missing.
    pub run_id: Option<u64>,
    pub run_url: Option<String>,
    pub conclusion: Option<String>,
    pub created_at: Option<String>,
}

impl ShaCi {
    pub(super) fn to_json(&self) -> Value {
        json!({
            "sha": self.sha,
            "state": self.state.as_str(),
            "covered_by": self.covered_by,
            "run_id": self.run_id,
            "run_url": self.run_url,
            "conclusion": self.conclusion,
            "created_at": self.created_at,
        })
    }
}

fn is_ci_run(r: &Value) -> bool {
    let path = r["path"].as_str().unwrap_or_default();
    let file = path.split('@').next().unwrap_or_default();
    r["event"].as_str() == Some("push")
        && (file == CI_WORKFLOW || file.ends_with(&format!("/{CI_WORKFLOW}")))
}

fn run_state(r: &Value) -> CiState {
    if r["status"].as_str() != Some("completed") {
        return CiState::Pending;
    }
    match r["conclusion"].as_str() {
        Some("success") => CiState::Passed,
        Some("failure" | "timed_out" | "startup_failure") => CiState::Failed,
        _ => CiState::Cancelled,
    }
}

/// Classify the default branch's recent SHAs by their own `ci.yml`
/// push run (CAD-267). Pure — fixtures test it.
///
/// `first_parent` is `git log --first-parent` of the default branch,
/// newest first. A run SHA it does not contain but whose run is newer
/// than every listed SHA's run was pushed after the clone's last fetch:
/// it leads the list, newest run first (with no clone at all, the runs
/// alone give the order). Older unlisted run SHAs are not on the
/// branch's first-parent history and are ignored. Per SHA the newest
/// run (highest id) decides; returns at most [`MAIN_CI_SHAS`], newest
/// first.
pub(crate) fn classify_main_ci(runs: &[Value], first_parent: &[String]) -> Vec<ShaCi> {
    let mut own: HashMap<&str, &Value> = HashMap::new();
    for r in runs.iter().filter(|r| is_ci_run(r)) {
        let Some(sha) = r["head_sha"].as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        let id = r["id"].as_u64().unwrap_or(0);
        if own
            .get(sha)
            .is_none_or(|prev| prev["id"].as_u64().unwrap_or(0) < id)
        {
            own.insert(sha, r);
        }
    }
    let created = |sha: &str| {
        own.get(sha)
            .and_then(|r| r["created_at"].as_str())
            .unwrap_or_default()
            .to_string()
    };
    let listed: std::collections::HashSet<&str> = first_parent.iter().map(String::as_str).collect();
    let newest_listed = first_parent
        .iter()
        .map(|s| created(s))
        .max()
        .unwrap_or_default();
    let mut ahead: Vec<&str> = own
        .keys()
        .copied()
        .filter(|s| !listed.contains(s) && created(s) > newest_listed)
        .collect();
    ahead.sort_by_key(|s| std::cmp::Reverse((created(s), own[s]["id"].as_u64().unwrap_or(0))));
    let mut out: Vec<ShaCi> = ahead
        .into_iter()
        .chain(first_parent.iter().map(String::as_str))
        .map(|sha| {
            let run = own.get(sha);
            ShaCi {
                sha: sha.to_string(),
                state: run.map_or(CiState::Missing, |r| run_state(r)),
                covered_by: None,
                run_id: run.and_then(|r| r["id"].as_u64()),
                run_url: run.and_then(|r| r["html_url"].as_str().map(str::to_string)),
                conclusion: run.and_then(|r| r["conclusion"].as_str().map(str::to_string)),
                created_at: run.and_then(|r| r["created_at"].as_str().map(str::to_string)),
            }
        })
        .collect();
    // Newest first: the last pass seen before index i is the nearest
    // later SHA whose own run passed.
    let mut nearest_pass: Option<String> = None;
    for s in &mut out {
        if matches!(s.state, CiState::Cancelled | CiState::Missing) {
            s.covered_by = nearest_pass.clone();
        }
        if s.state == CiState::Passed {
            nearest_pass = Some(s.sha.clone());
        }
    }
    out.truncate(MAIN_CI_SHAS);
    out
}

/// What the classification alerts on. `red`: the newest SHA with a
/// verdict (passed or failed) failed — pending and cancelled SHAs
/// carry no verdict, so they neither raise nor clear it.
/// `unverified`: cancelled/missing SHAs with no covering later pass,
/// newest first — they clear the moment a later SHA passes, while the
/// SHAs keep their own label.
pub(crate) struct MainCiAlerts<'a> {
    pub red: Option<&'a ShaCi>,
    pub unverified: Vec<&'a ShaCi>,
}

pub(crate) fn main_ci_alerts(shas: &[ShaCi]) -> MainCiAlerts<'_> {
    let red = shas
        .iter()
        .find(|s| matches!(s.state, CiState::Passed | CiState::Failed))
        .filter(|s| s.state == CiState::Failed);
    let unverified = shas
        .iter()
        .filter(|s| matches!(s.state, CiState::Cancelled | CiState::Missing))
        .filter(|s| s.covered_by.is_none())
        .collect();
    MainCiAlerts { red, unverified }
}
