use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};

use super::github::{gh_text, git_text, is_plain_ref};
use super::{item, parse_iso, Item};

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
    pub(super) fn as_str(self) -> &'static str {
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

/// The run fields [`classify_main_ci`] and the rows read — the rest of
/// the API object is dropped before it reaches the cache.
const RUN_FIELDS: [&str; 9] = [
    "id",
    "head_sha",
    "status",
    "conclusion",
    "event",
    "path",
    "head_branch",
    "html_url",
    "created_at",
];

/// `{branch, runs}` for the repo's default branch: the newest
/// [`CI_RUNS_PAGE`] `ci.yml` push runs. A repo without a `ci.yml`
/// workflow is `{absent: true}` — no CI to read, not an error; any
/// other failure is `{error}`.
pub(super) fn gh_main_ci(slug: &str) -> Value {
    let branch = match gh_text(&["api".into(), format!("repos/{slug}")])
        .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| format!("unreadable ({e})")))
    {
        Ok(repo) => match repo["default_branch"].as_str() {
            Some(b) if is_plain_ref(b) => b.to_string(),
            _ => return json!({"error": format!("repos/{slug}: no readable default_branch")}),
        },
        Err(e) => return json!({"error": e}),
    };
    let path = format!(
        "repos/{slug}/actions/workflows/{CI_WORKFLOW}/runs?branch={branch}&event=push&per_page={CI_RUNS_PAGE}"
    );
    let body = match gh_text(&["api".into(), path]) {
        Ok(t) => t,
        Err(e) if e.contains("HTTP 404") => return json!({"branch": branch, "absent": true}),
        Err(e) => return json!({"branch": branch, "error": e}),
    };
    let parsed: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return json!({"branch": branch, "error": format!("ci runs: unreadable ({e})")}),
    };
    let Some(runs) = parsed["workflow_runs"].as_array() else {
        return json!({"branch": branch, "error": "ci runs: no workflow_runs array"});
    };
    let runs: Vec<Value> = runs
        .iter()
        .map(|r| {
            RUN_FIELDS
                .iter()
                .map(|f| (f.to_string(), r[*f].clone()))
                .collect::<serde_json::Map<_, _>>()
                .into()
        })
        .collect();
    json!({"branch": branch, "runs": runs})
}

/// The workflow whose push runs are the default branch's CI verdict.
/// Runs of any other workflow (Handover, …) never count.
pub(super) const CI_WORKFLOW: &str = "ci.yml";
/// Push runs fetched per refresh — one per pushed SHA under the
/// `queue: max` policy, so a page covers more SHAs than we classify.
const CI_RUNS_PAGE: usize = 30;
/// First-parent SHAs read from the local clone to place each run.
const MAIN_CI_LOG: usize = 60;

pub(super) fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// `git log --first-parent` of `branch` in the local clone, newest
/// first — `origin/<branch>` when the clone tracks it, else the local
/// branch. Local refs only, never a fetch.
pub(super) fn first_parent_log(repo: &Path, branch: &str) -> Result<Vec<String>, String> {
    let rev = [format!("origin/{branch}"), branch.to_string()]
        .into_iter()
        .find(|r| {
            git_text(
                repo,
                &[
                    "rev-parse".into(),
                    "--verify".into(),
                    "-q".into(),
                    r.clone(),
                ],
            )
            .is_ok()
        })
        .ok_or_else(|| format!("no {branch} ref in {}", repo.display()))?;
    let text = git_text(
        repo,
        &[
            "log".into(),
            "--first-parent".into(),
            format!("-{MAIN_CI_LOG}"),
            "--format=%H".into(),
            rev,
        ],
    )?;
    Ok(text.lines().map(str::to_string).collect())
}

/// The `main_ci` block for one slug plus its needs-me rows.
pub(super) fn main_ci_view(
    slug: &str,
    project: &str,
    data: &Value,
    clone: Option<&Path>,
    now: i64,
) -> (Value, Vec<Item>) {
    let block = &data["main_ci"];
    // A cache body written before CAD-267 has no block — nothing to say.
    if block.is_null() {
        return (Value::Null, Vec::new());
    }
    let Some(branch) = block["branch"].as_str() else {
        return (
            json!({"slug": slug, "project": project, "error": block["error"]}),
            Vec::new(),
        );
    };
    if block["absent"].as_bool() == Some(true) {
        return (Value::Null, Vec::new());
    }
    let Some(runs) = block["runs"].as_array() else {
        return (
            json!({"slug": slug, "project": project, "branch": branch, "error": block["error"]}),
            Vec::new(),
        );
    };
    let (first_parent, log_error) = match clone.map(|c| first_parent_log(c, branch)) {
        Some(Ok(log)) => (log, None),
        Some(Err(e)) => (Vec::new(), Some(e)),
        None => (Vec::new(), Some("no local clone declared".to_string())),
    };
    let shas = classify_main_ci(runs, &first_parent);
    let alerts = main_ci_alerts(&shas);
    let subject = format!("{slug}@{branch}");
    let run_at = |s: &ShaCi| s.created_at.as_deref().and_then(parse_iso);
    let run_age = |s: &ShaCi| run_at(s).map(|t| now - t).unwrap_or(0);
    let mut rows = Vec::new();
    if let Some(s) = alerts.red {
        let command = match s.run_id {
            Some(id) => format!("gh run view {id} --repo {slug}"),
            None => format!("gh run list --repo {slug} --workflow {CI_WORKFLOW} --branch {branch}"),
        };
        rows.push(
            item(
                90,
                "ci_red",
                &format!("{branch} CI failed at {} — {slug}", short(&s.sha)),
                run_age(s),
                project,
                s.run_url.as_deref(),
                &command,
            )
            .about("ci", &subject)
            .since(run_at(s)),
        );
    }
    if let Some(newest) = alerts.unverified.first() {
        let n = alerts.unverified.len();
        let what = format!("{} {}", short(&newest.sha), newest.state.as_str());
        let desc = if n == 1 {
            what
        } else {
            format!("{n} SHAs, newest {what}")
        };
        // Re-running the newest cancelled run covers every older one
        // once it passes; a missing run has nothing to re-run.
        let command = match (newest.state, newest.run_id) {
            (CiState::Cancelled, Some(id)) => format!("gh run rerun {id} --repo {slug}"),
            _ => format!("gh run list --repo {slug} --workflow {CI_WORKFLOW} --branch {branch}"),
        };
        rows.push(
            item(
                92,
                "ci_unverified",
                &format!("{branch} CI unverified: {desc}, no later SHA passed yet — {slug}"),
                run_age(newest),
                project,
                newest.run_url.as_deref(),
                &command,
            )
            .about("ci", &subject)
            .since(run_at(newest)),
        );
    }
    let view = json!({
        "slug": slug,
        "project": project,
        "branch": branch,
        "workflow": CI_WORKFLOW,
        "order": if first_parent.is_empty() { "runs" } else { "first_parent" },
        "log_error": log_error,
        "shas": shas.iter().map(ShaCi::to_json).collect::<Vec<_>>(),
    });
    (view, rows)
}
