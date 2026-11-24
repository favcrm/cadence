#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::Path;

use serde_json::{json, Value};

use super::github::{gh_text, git_text, is_plain_ref};
use super::{item, parse_iso, Item};

#[path = "main_ci/classify.rs"]
mod classify;

pub(crate) use classify::{classify_main_ci, main_ci_alerts, CiState, MainCiAlerts, ShaCi};

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

/// A repo's default branch, read once an hour: it changes about never,
/// and re-reading it each refresh was a third of the overview's calls.
static DEFAULT_BRANCHES: std::sync::Mutex<Vec<(String, std::time::Instant, String)>> =
    std::sync::Mutex::new(Vec::new());
const DEFAULT_BRANCH_TTL: std::time::Duration = std::time::Duration::from_secs(3600);

fn default_branch(slug: &str) -> Result<String, String> {
    let kept = DEFAULT_BRANCHES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(s, at, _)| s == slug && at.elapsed() < DEFAULT_BRANCH_TTL)
        .map(|(_, _, b)| b.clone());
    if let Some(b) = kept {
        return Ok(b);
    }
    let repo = gh_text(&["api".into(), format!("repos/{slug}")])
        .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| format!("unreadable ({e})")))?;
    match repo["default_branch"].as_str() {
        Some(b) if is_plain_ref(b) => {
            let mut kept = DEFAULT_BRANCHES.lock().unwrap_or_else(|e| e.into_inner());
            kept.retain(|(s, _, _)| s != slug);
            kept.push((slug.to_string(), std::time::Instant::now(), b.to_string()));
            Ok(b.to_string())
        }
        _ => Err(format!("repos/{slug}: no readable default_branch")),
    }
}

/// `{branch, runs}` for the repo's default branch: the newest
/// [`CI_RUNS_PAGE`] `ci.yml` push runs. A repo without a `ci.yml`
/// workflow is `{absent: true}` — no CI to read, not an error; any
/// other failure is `{error}`.
pub(super) fn gh_main_ci(slug: &str) -> Value {
    let branch = match default_branch(slug) {
        Ok(b) => b,
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
    let alerts: MainCiAlerts<'_> = main_ci_alerts(&shas);
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
