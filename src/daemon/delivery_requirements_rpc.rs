//! Read-only CAD-1298 delivery-requirements bridge. All classification
//! inputs are derived from the daemon's PM, recorded approvals and the
//! current GitHub PR; no caller-supplied policy or evidence is accepted.

use serde_json::{json, Value};
use std::collections::HashMap;

use super::{required_str, Shared};
use crate::error::{Error, Result};
use crate::issue::{self, delivery_policy, delivery_requirements as requirements};

#[cfg(test)]
#[path = "cad1298_requirements_acceptance.rs"]
mod acceptance;

fn reject(why: impl std::fmt::Display) -> Error {
    Error::rejected(format!("delivery requirements: {why}"))
}

fn gh(shared: &Shared, args: &[&str]) -> Result<String> {
    crate::delivery::gh(
        shared
            .delivery_gh
            .to_str()
            .unwrap_or("/nonexistent/gh-not-on-path-at-boot"),
        args,
    )
}

fn api(shared: &Shared, endpoint: &str) -> Result<Value> {
    let text = gh(shared, &["api", endpoint])?;
    serde_json::from_str(&text).map_err(|e| reject(format!("unreadable GitHub response: {e}")))
}

fn pr_view(shared: &Shared, slug: &str, number: u64) -> Result<Value> {
    let text = gh(
        shared,
        &[
            "pr",
            "view",
            &number.to_string(),
            "-R",
            slug,
            "--json",
            "title,headRefOid,baseRefName,baseRefOid,changedFiles,state",
        ],
    )?;
    serde_json::from_str(&text).map_err(|e| reject(format!("unreadable PR view: {e}")))
}

fn default_branch(shared: &Shared, repo: &str) -> Result<String> {
    let text = gh(
        shared,
        &["repo", "view", repo, "--json", "defaultBranchRef"],
    )?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| reject(format!("unreadable repository default branch: {e}")))?;
    value["defaultBranchRef"]["name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .ok_or_else(|| reject("repository has no default branch"))
}

fn valid_sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn request_fields(params: &Value) -> Result<(&str, &str, &str)> {
    let fields = params
        .as_object()
        .ok_or_else(|| reject("request must be an object"))?;
    if fields
        .keys()
        .any(|key| !matches!(key.as_str(), "issue" | "pr" | "head"))
    {
        return Err(reject("request accepts only issue, pr and head"));
    }
    let issue = required_str(params, "issue")?;
    let pr = required_str(params, "pr")?;
    let head = required_str(params, "head")?;
    if issue.trim().is_empty() || pr.trim().is_empty() {
        return Err(reject("issue and pr must not be empty or whitespace-only"));
    }
    if !valid_sha(head) {
        return Err(reject("head must be a full lowercase 40-hex commit SHA"));
    }
    Ok((issue, pr, head))
}

fn title_names_issue(title: &str, issue: &str) -> bool {
    regex::Regex::new(r"^\s*([A-Z][A-Z0-9]*-\d+)\b")
        .expect("static issue-token pattern")
        .captures(title)
        .and_then(|captures| captures.get(1))
        .is_some_and(|token| token.as_str() == issue)
}

type TreeEntry = (String, String);

fn tree_modes(shared: &Shared, repo: &str, sha: &str) -> Result<HashMap<String, TreeEntry>> {
    let tree = api(shared, &format!("repos/{repo}/git/trees/{sha}?recursive=1"))?;
    if tree["truncated"].as_bool() != Some(false) {
        return Err(reject(
            "GitHub tree is truncated or has no truncation proof",
        ));
    }
    let entries = tree["tree"]
        .as_array()
        .ok_or_else(|| reject("GitHub tree has no entries"))?;
    let mut modes = HashMap::with_capacity(entries.len());
    for entry in entries {
        let path = entry["path"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| reject("GitHub tree contains an entry without a path"))?;
        let mode = entry["mode"]
            .as_str()
            .ok_or_else(|| reject(format!("tree entry {path} has no mode")))?;
        let kind = entry["type"]
            .as_str()
            .ok_or_else(|| reject(format!("tree entry {path} has no type")))?;
        let agrees = match kind {
            "blob" => matches!(mode, "100644" | "100755" | "120000"),
            "tree" => mode == "040000",
            "commit" => mode == "160000",
            _ => false,
        };
        if !agrees {
            return Err(reject(format!(
                "tree entry {path} has invalid type/mode {kind}/{mode}"
            )));
        }
        if modes
            .insert(path.to_string(), (mode.to_string(), kind.to_string()))
            .is_some()
        {
            return Err(reject(format!("GitHub tree repeats path {path}")));
        }
    }
    Ok(modes)
}

fn changed_mode(tree: &HashMap<String, TreeEntry>, path: &str) -> Result<String> {
    let (mode, kind) = tree
        .get(path)
        .ok_or_else(|| reject(format!("tree mode unavailable for {path}")))?;
    if kind == "tree" {
        return Err(reject(format!("changed path {path} resolves to a tree")));
    }
    Ok(mode.clone())
}

fn content_at_base(shared: &Shared, repo: &str, path: &str, base: &str) -> Option<String> {
    let endpoint = format!("repos/{repo}/contents/{path}?ref={base}");
    let value = api(shared, &endpoint).ok()?;
    if value["encoding"].as_str()? != "base64" {
        return None;
    }
    let encoded = value["content"].as_str()?.replace('\n', "");
    use base64::Engine;
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?,
    )
    .ok()
}

pub(super) fn classify(shared: &Shared, params: &Value) -> Result<Value> {
    let (issue_id, pr_url, requested_head) = request_fields(params)?;

    let pm = shared.pm()?;
    let issue = issue::board::find_issue(&pm.dir, issue_id)?;
    let project = issue::project::load(&pm.dir.join(&issue.project).join("project.yaml"))?;
    if let Some(why) = crate::delivery::project_pr_refusal(&pm.dir, &project.key, pr_url)? {
        return Err(reject(why));
    }
    let (slug, number) = issue::task_report::parse_pr_url(pr_url)?;
    let repo = slug.to_ascii_lowercase();
    let default = default_branch(shared, &repo)?;
    let view = pr_view(shared, &repo, number)?;
    if view["state"].as_str() != Some("OPEN") {
        return Err(reject("PR is not open"));
    }
    if view["title"]
        .as_str()
        .is_none_or(|title| !title_names_issue(title, issue_id))
    {
        return Err(reject(
            "PR title's first issue token does not exactly match the requested issue",
        ));
    }
    let head = view["headRefOid"]
        .as_str()
        .filter(|s| valid_sha(s))
        .ok_or_else(|| reject("PR has no valid full head SHA"))?;
    if head != requested_head {
        return Err(reject("requested head does not match current PR head"));
    }
    let base_ref = view["baseRefName"]
        .as_str()
        .ok_or_else(|| reject("PR has no base branch"))?;
    if base_ref != default {
        return Err(reject("PR base is not the repository default branch"));
    }
    let base = view["baseRefOid"]
        .as_str()
        .filter(|s| valid_sha(s))
        .ok_or_else(|| reject("PR has no valid full base commit SHA"))?
        .to_string();
    let changed_files = view["changedFiles"]
        .as_u64()
        .ok_or_else(|| reject("PR view has no changed-file count"))?;

    let comparison = api(shared, &format!("repos/{repo}/compare/{base}...{head}"))?;
    let merge_base = comparison["merge_base_commit"]["sha"]
        .as_str()
        .filter(|s| valid_sha(s))
        .ok_or_else(|| reject("comparison has no valid full merge-base SHA"))?
        .to_string();
    let files = comparison["files"]
        .as_array()
        .ok_or_else(|| reject("comparison has no complete file list"))?;
    if files.len() >= 300 || files.len() as u64 != changed_files {
        return Err(reject(
            "GitHub comparison file list is truncated or differs from the PR file count",
        ));
    }
    let base_modes = tree_modes(shared, &repo, &merge_base)?;
    let head_modes = tree_modes(shared, &repo, head)?;
    let mut changes = Vec::with_capacity(files.len());
    let mut lines = 0u64;
    for file in files {
        let path = file["filename"]
            .as_str()
            .ok_or_else(|| reject("comparison file has no path"))?;
        let status = match file["status"].as_str().unwrap_or("") {
            "added" => "A",
            "modified" => "M",
            "removed" => "D",
            "renamed" => "R",
            "changed" => "T",
            "copied" => "C",
            _ => "X",
        };
        let old_path = if status == "R" {
            file["previous_filename"]
                .as_str()
                .ok_or_else(|| reject(format!("renamed path {path} has no previous filename")))?
        } else {
            path
        };
        let old_mode = if status == "A" {
            "000000".to_string()
        } else {
            changed_mode(&base_modes, old_path)?
        };
        let new_mode = if status == "D" {
            "000000".to_string()
        } else {
            changed_mode(&head_modes, path)?
        };
        if file["additions"].as_u64().is_none() || file["deletions"].as_u64().is_none() {
            return Err(reject(format!(
                "changed-line counts unavailable for {path}"
            )));
        }
        lines = lines
            .saturating_add(file["additions"].as_u64().unwrap())
            .saturating_add(file["deletions"].as_u64().unwrap());
        changes.push(requirements::change(status, &old_mode, &new_mode, path));
    }

    let approvals = shared.store.work_approvals()?;
    let approved = approvals
        .get(&project.key)
        .and_then(delivery_policy::approved_from);
    let resolved = delivery_policy::effective(
        &project.key,
        delivery_policy::load(&pm.dir, &project.key),
        approved.as_ref(),
    );
    let risk_paths = content_at_base(shared, &repo, "docs/roles/risk-paths.toml", &base);
    let one_review = content_at_base(shared, &repo, "docs/roles/one-review-paths.toml", &base);
    let lists = requirements::TrustedLists {
        risk_paths: risk_paths.as_deref(),
        one_review: one_review.as_deref(),
    };
    let result = requirements::classify(&resolved, &changes, lines, &lists);

    // Refuse a mixed-head answer: re-read all external and operator-owned
    // inputs that determine the result before returning it.
    let after = pr_view(shared, &repo, number)?;
    let after_approvals = shared.store.work_approvals()?;
    let after_approved = after_approvals
        .get(&project.key)
        .and_then(delivery_policy::approved_from);
    let after_resolved = delivery_policy::effective(
        &project.key,
        delivery_policy::load(&pm.dir, &project.key),
        after_approved.as_ref(),
    );
    if after["state"].as_str() != Some("OPEN")
        || after["title"]
            .as_str()
            .is_none_or(|title| !title_names_issue(title, issue_id))
        || after["headRefOid"].as_str() != Some(head)
        || after["baseRefName"].as_str() != Some(base_ref)
        || after["baseRefOid"].as_str() != Some(base.as_str())
        || after["changedFiles"].as_u64() != Some(changed_files)
        || default_branch(shared, &repo)? != default
        || after_resolved.digest != resolved.digest
    {
        return Err(reject(
            "PR head/base or approved policy changed during collection",
        ));
    }
    Ok(
        json!({"issue": issue_id, "project": project.key, "repo": repo, "pr": number,
        "head": head, "base": base, "merge_base": merge_base,
        "policy_digest": result.policy_digest, "requirements": result}),
    )
}

/// Recheck the current full PR check floor for profile-backed approvals.
/// This uses GitHub's required branch checks plus a successful canonical
/// pull_request ci.yml run, never the daemon's previously observed boolean.
pub(super) fn full_ci_green(
    shared: &Shared,
    repo: &str,
    number: u64,
    head: &str,
    base: &str,
) -> Result<bool> {
    let current: Value = serde_json::from_str(&gh(
        shared,
        &[
            "pr",
            "view",
            &number.to_string(),
            "-R",
            repo,
            "--json",
            "headRefOid,baseRefOid,statusCheckRollup",
        ],
    )?)
    .map_err(|e| reject(format!("unreadable current PR checks: {e}")))?;
    if current["headRefOid"].as_str() != Some(head) || current["baseRefOid"].as_str() != Some(base)
    {
        return Ok(false);
    }
    let branch = default_branch(shared, repo)?;
    let rules = api(shared, &format!("repos/{repo}/rules/branches/{branch}"))?;
    let protection = api(
        shared,
        &format!("repos/{repo}/branches/{branch}/protection"),
    );
    let mut required = std::collections::BTreeSet::new();
    for rule in rules
        .as_array()
        .ok_or_else(|| reject("branch rules response is not an array"))?
    {
        if rule["type"] == "required_status_checks" {
            for check in rule["parameters"]["required_status_checks"]
                .as_array()
                .ok_or_else(|| reject("required branch rules have no check list"))?
            {
                required.insert(
                    check["context"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| reject("required check has no context"))?
                        .to_string(),
                );
            }
        }
    }
    match protection {
        Ok(doc) => {
            let checks = &doc["required_status_checks"];
            if !checks.is_null() && !checks.is_object() {
                return Err(reject(
                    "branch protection required_status_checks is malformed",
                ));
            }
            for (field, context_field) in [("contexts", false), ("checks", true)] {
                if let Some(values) = checks.get(field) {
                    for item in values
                        .as_array()
                        .ok_or_else(|| reject("branch protection checks are malformed"))?
                    {
                        let value = if context_field {
                            item["context"].as_str()
                        } else {
                            item.as_str()
                        }
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| reject("invalid required check"))?;
                        required.insert(value.to_string());
                    }
                }
            }
        }
        Err(error)
            if error.to_string().to_ascii_lowercase().contains("http 404")
                && error
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("not protected")
                && !required.is_empty() => {}
        Err(_) => return Ok(false),
    }
    if required.is_empty() {
        return Ok(false);
    }
    let mut states: HashMap<String, Vec<String>> = HashMap::new();
    for check in current["statusCheckRollup"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let (name, state) = if check.get("context").is_some() {
            (
                check["context"].as_str(),
                check["state"].as_str().unwrap_or("").to_ascii_uppercase(),
            )
        } else {
            let status = check["status"].as_str().unwrap_or("").to_ascii_uppercase();
            let state = if status == "COMPLETED" {
                check["conclusion"]
                    .as_str()
                    .unwrap_or("")
                    .to_ascii_uppercase()
            } else {
                status
            };
            (check["name"].as_str(), state)
        };
        if let Some(name) = name.filter(|s| !s.is_empty()) {
            states.entry(name.to_string()).or_default().push(state);
        }
    }
    if required.iter().any(|name| {
        states
            .get(name)
            .is_none_or(|values| values.iter().any(|state| state != "SUCCESS"))
    }) {
        return Ok(false);
    }
    let runs = api(shared, &format!("repos/{repo}/actions/workflows/ci.yml/runs?head_sha={head}&event=pull_request&status=success&per_page=100"))?;
    let Some(runs) = runs["workflow_runs"].as_array() else {
        return Ok(false);
    };
    Ok(runs.iter().any(|run| {
        run["head_sha"].as_str() == Some(head)
            && run["event"].as_str() == Some("pull_request")
            && run["status"].as_str() == Some("completed")
            && run["conclusion"].as_str() == Some("success")
            && run["id"].as_u64().is_some_and(|id| id > 0)
            && run["html_url"].as_str()
                == run["id"]
                    .as_u64()
                    .map(|id| format!("https://github.com/{repo}/actions/runs/{id}"))
                    .as_deref()
    }))
}

pub(super) enum RoutineReadiness {
    NotRoutine,
    Waiting,
    Ready(crate::delivery::ReadyEvidence),
}

/// Recompute routine eligibility from server-owned PR/policy/diff inputs and
/// actual GitHub checks. `Waiting` is retryable; no report or CI is fabricated.
pub(super) fn routine_readiness(
    shared: &Shared,
    issue_id: &str,
    pr_url: &str,
    head: &str,
    outcome_report: &str,
) -> Result<RoutineReadiness> {
    routine_readiness_inner(shared, issue_id, pr_url, head, outcome_report, true)
}

fn routine_readiness_inner(
    shared: &Shared,
    issue_id: &str,
    pr_url: &str,
    head: &str,
    outcome_report: &str,
    confirm: bool,
) -> Result<RoutineReadiness> {
    let answer = match classify(shared, &json!({"issue":issue_id,"pr":pr_url,"head":head})) {
        Ok(answer) => answer,
        Err(_) => return Ok(RoutineReadiness::NotRoutine),
    };
    if answer["requirements"]["class"] != "routine" {
        return Ok(RoutineReadiness::NotRoutine);
    }
    if outcome_report.trim().is_empty() {
        return Ok(RoutineReadiness::NotRoutine);
    }
    let record = crate::delivery::load(&shared.state_dir)?
        .remove(issue_id)
        .ok_or_else(|| reject("delivery record is unavailable"))?;
    if record
        .outcome_report
        .as_deref()
        .is_some_and(|accepted| accepted != outcome_report)
    {
        return Ok(RoutineReadiness::NotRoutine);
    }
    let pm = shared.pm()?;
    let report = issue::task_report::list(&pm.dir.join(&record.project).join(issue_id), issue_id)
        .into_iter()
        .find(|row| row["path"].as_str() == Some(outcome_report));
    let Some(report) = report else {
        return Ok(RoutineReadiness::NotRoutine);
    };
    let filed_at = report["at"].as_str().and_then(issue::time::parse_iso);
    if !report["error"].is_null()
        || report["kind"] != "done"
        || report["agent"].as_str() != Some(record.worker.as_str())
        || report["sha"].as_str() != Some(head)
        || report["pr"].as_str() != Some(pr_url)
        || !filed_at.is_some_and(|at| at >= record.dispatched_at)
    {
        return Ok(RoutineReadiness::NotRoutine);
    }
    let repo = answer["repo"]
        .as_str()
        .ok_or_else(|| reject("missing canonical repo"))?;
    let base = answer["base"]
        .as_str()
        .ok_or_else(|| reject("missing base SHA"))?;
    let number = answer["pr"]
        .as_u64()
        .ok_or_else(|| reject("missing PR number"))?;
    let branch = default_branch(shared, repo)?;
    let rules = api(shared, &format!("repos/{repo}/rules/branches/{branch}"))?;
    let protection = api(
        shared,
        &format!("repos/{repo}/branches/{branch}/protection"),
    );
    let mut required = std::collections::BTreeSet::new();
    let rules = rules
        .as_array()
        .ok_or_else(|| reject("branch rules response is not an array"))?;
    for rule in rules {
        if rule["type"] == "required_status_checks" {
            let checks = rule["parameters"]["required_status_checks"]
                .as_array()
                .ok_or_else(|| reject("required branch rules have no check list"))?;
            for check in checks {
                required.insert(
                    check["context"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| reject("required check has no context"))?
                        .to_string(),
                );
            }
        }
    }
    let verified_ruleset_checks = !required.is_empty();
    match protection {
        Ok(doc) => {
            let status_checks = &doc["required_status_checks"];
            if !status_checks.is_null() && !status_checks.is_object() {
                return Err(reject(
                    "branch protection required_status_checks is malformed",
                ));
            }
            for (field, what) in [
                ("contexts", "required context"),
                ("checks", "required check"),
            ] {
                let Some(value) = status_checks.get(field) else {
                    continue;
                };
                let values = value
                    .as_array()
                    .ok_or_else(|| reject(format!("branch protection {what}s are malformed")))?;
                for item in values {
                    let name = if field == "contexts" {
                        item.as_str()
                    } else {
                        item["context"].as_str()
                    }
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| reject(format!("invalid {what}")))?;
                    required.insert(name.to_string());
                }
            }
        }
        Err(error) => {
            let message = error.to_string().to_ascii_lowercase();
            if !(message.contains("http 404")
                && message.contains("not protected")
                && verified_ruleset_checks)
            {
                return Ok(RoutineReadiness::NotRoutine);
            }
        }
    }
    if required.is_empty() {
        return Ok(RoutineReadiness::NotRoutine);
    }
    let view = pr_view(shared, repo, number)?;
    if view["headRefOid"].as_str() != Some(head) || view["baseRefOid"].as_str() != Some(base) {
        return Ok(RoutineReadiness::NotRoutine);
    }
    let mut states: HashMap<String, Vec<String>> = HashMap::new();
    for check in view["statusCheckRollup"].as_array().into_iter().flatten() {
        let (name, state) = if check.get("context").is_some() {
            (
                check["context"].as_str(),
                check["state"].as_str().unwrap_or("").to_ascii_uppercase(),
            )
        } else {
            let status = check["status"].as_str().unwrap_or("").to_ascii_uppercase();
            let state = if status == "COMPLETED" {
                check["conclusion"]
                    .as_str()
                    .unwrap_or("")
                    .to_ascii_uppercase()
            } else {
                status
            };
            (check["name"].as_str(), state)
        };
        if let Some(name) = name.filter(|s| !s.is_empty()) {
            states.entry(name.to_string()).or_default().push(state);
        }
    }
    let mut waiting = false;
    for name in &required {
        let Some(values) = states.get(name) else {
            waiting = true;
            continue;
        };
        if values.iter().any(|v| v != "SUCCESS") {
            if values.iter().any(|v| {
                matches!(
                    v.as_str(),
                    "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED"
                )
            }) {
                return Ok(RoutineReadiness::NotRoutine);
            }
            waiting = true;
        }
    }
    let runs = api(shared, &format!("repos/{repo}/actions/workflows/ci.yml/runs?head_sha={head}&event=pull_request&status=success&per_page=100"))?;
    let runs = runs["workflow_runs"]
        .as_array()
        .ok_or_else(|| reject("ci.yml runs response has no workflow_runs"))?;
    let run = runs.iter().find(|run| {
        run["head_sha"].as_str() == Some(head)
            && run["event"].as_str() == Some("pull_request")
            && run["status"].as_str() == Some("completed")
            && run["conclusion"].as_str() == Some("success")
            && run["id"].as_u64().is_some_and(|id| id > 0)
    });
    let Some(run) = run else {
        return Ok(RoutineReadiness::Waiting);
    };
    let run_id = run["id"].as_u64().unwrap();
    let ci_url = format!("https://github.com/{repo}/actions/runs/{run_id}");
    if run["html_url"].as_str() != Some(ci_url.as_str()) {
        return Ok(RoutineReadiness::NotRoutine);
    }
    if waiting {
        return Ok(RoutineReadiness::Waiting);
    }
    let final_answer = classify(shared, &json!({"issue":issue_id,"pr":pr_url,"head":head}))?;
    if final_answer["head"] != answer["head"]
        || final_answer["base"] != answer["base"]
        || final_answer["policy_digest"] != answer["policy_digest"]
        || final_answer["requirements"]["class"] != "routine"
    {
        return Ok(RoutineReadiness::NotRoutine);
    }
    let evidence = crate::delivery::ReadyEvidence {
        repo: repo.to_string(),
        pr: number,
        head: head.to_string(),
        base: base.to_string(),
        policy_digest: answer["policy_digest"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        ci_run_id: run_id,
        ci_run_url: ci_url,
        outcome_report: outcome_report.to_string(),
    };
    if confirm {
        return match routine_readiness_inner(shared, issue_id, pr_url, head, outcome_report, false)?
        {
            RoutineReadiness::Ready(current) if current == evidence => {
                Ok(RoutineReadiness::Ready(evidence))
            }
            RoutineReadiness::Waiting => Ok(RoutineReadiness::Waiting),
            _ => Ok(RoutineReadiness::NotRoutine),
        };
    }
    Ok(RoutineReadiness::Ready(evidence))
}

impl Shared {
    pub(super) fn rpc_delivery_requirements(&self, params: &Value) -> Result<Value> {
        let mut answer = classify(self, params)?;
        let issue_id = params["issue"].as_str().unwrap_or_default();
        let pr = params["pr"].as_str().unwrap_or_default();
        let head = params["head"].as_str().unwrap_or_default();
        let report = crate::delivery::load(&self.state_dir)?
            .get(issue_id)
            .and_then(|record| record.outcome_report.clone());
        let readiness = match report {
            Some(report) => match routine_readiness(self, issue_id, pr, head, &report)
                .unwrap_or(RoutineReadiness::NotRoutine)
            {
                RoutineReadiness::Ready(evidence) => Some(evidence),
                RoutineReadiness::NotRoutine | RoutineReadiness::Waiting => None,
            },
            None => None,
        };
        answer["readiness"] = json!(readiness);
        Ok(answer)
    }
}
