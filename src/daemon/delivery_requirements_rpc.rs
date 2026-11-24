//! Read-only CAD-1298 delivery-requirements bridge. All classification
//! inputs are derived from the daemon's PM, recorded approvals and the
//! current GitHub PR; no caller-supplied policy or evidence is accepted.

use serde_json::{json, Value};
use std::collections::HashMap;

use super::Shared;
pub(super) use crate::daemon::delivery_classifier::branch_segment;
use crate::daemon::delivery_classifier::{
    api, classify_from, default_branch, gh, pr_view, require_queue_authority, GhRead,
    RequirementInputs,
};
use crate::error::{Error, Result};
use crate::issue::{self, delivery_policy};

fn reject(why: impl std::fmt::Display) -> Error {
    Error::rejected(format!("delivery requirements: {why}"))
}

impl GhRead for Shared {
    fn gh_read(&self, args: &[&str]) -> Result<String> {
        crate::delivery::gh(
            self.delivery_gh
                .to_str()
                .unwrap_or("/nonexistent/gh-not-on-path-at-boot"),
            args,
        )
    }
}

impl RequirementInputs for Shared {
    fn issue_project(&self, issue: &str) -> Result<String> {
        let pm = self.pm()?;
        let issue = issue::board::find_issue(&pm.dir, issue)?;
        Ok(issue::project::load(&pm.dir.join(&issue.project).join("project.yaml"))?.key)
    }

    fn pr_refusal(&self, project: &str, pr_url: &str) -> Result<Option<String>> {
        crate::delivery::project_pr_refusal(&self.pm()?.dir, project, pr_url)
    }

    fn policy(
        &self,
        project: &str,
    ) -> Result<(delivery_policy::Resolved, Option<delivery_policy::Approved>)> {
        policy_snapshot(self, &self.pm()?.dir, project)
    }
}

/// Local policy and approval provenance, without remote collection. A caller
/// committing authority holds the tracker lock through its mutation.
pub(super) fn policy_snapshot(
    shared: &Shared,
    pm_dir: &std::path::Path,
    project: &str,
) -> Result<(delivery_policy::Resolved, Option<delivery_policy::Approved>)> {
    let approvals = shared.store.work_approvals()?;
    let approved = approvals
        .get(project)
        .and_then(delivery_policy::approved_from);
    let resolved = delivery_policy::effective(
        project,
        delivery_policy::load(pm_dir, project).map_err(|e| e.to_string()),
        approved.as_ref(),
    );
    Ok((resolved, approved))
}

pub(super) fn classify(shared: &Shared, params: &Value) -> Result<Value> {
    classify_with_policy(shared, params).map(|(answer, _)| answer)
}

pub(super) fn classify_with_policy(
    shared: &Shared,
    params: &Value,
) -> Result<(Value, delivery_policy::Resolved)> {
    classify_from(shared, params)
}

/// Recheck the actual target's required checks and latest canonical PR CI.
/// Automatic landing additionally requires the default branch's enforced
/// native merge queue; prequeue feedback is not current-base integration.
pub(super) fn full_ci_green(
    shared: &Shared,
    repo: &str,
    number: u64,
    head: &str,
    base: &str,
    base_ref: &str,
    require_queue: bool,
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
            "headRefOid,baseRefOid,baseRefName,statusCheckRollup",
        ],
    )?)
    .map_err(|e| reject(format!("unreadable current PR checks: {e}")))?;
    if current["headRefOid"].as_str() != Some(head)
        || current["baseRefOid"].as_str() != Some(base)
        || current["baseRefName"].as_str() != Some(base_ref)
    {
        return Ok(false);
    }
    if require_queue && default_branch(shared, repo)? != base_ref {
        return Err(reject(
            "automatic landing requires the repository default branch",
        ));
    }
    let branch = branch_segment(base_ref);
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
    match &protection {
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
    let runs = api(shared, &format!("repos/{repo}/actions/workflows/ci.yml/runs?head_sha={head}&event=pull_request&per_page=100"))?;
    let entries = runs["workflow_runs"]
        .as_array()
        .ok_or_else(|| reject("canonical CI runs are unreadable"))?;
    if runs["total_count"].as_u64() != Some(entries.len() as u64) {
        return Err(reject(
            "canonical CI history is incomplete; newest attempt cannot be proven",
        ));
    }
    let mut latest: Option<(&Value, (u64, u64))> = None;
    for run in entries {
        if run["head_sha"].as_str() != Some(head) || run["event"].as_str() != Some("pull_request") {
            return Err(reject(
                "canonical CI response contradicts its head/event filter",
            ));
        }
        let order = (
            run["run_number"].as_u64().filter(|n| *n > 0),
            run["run_attempt"].as_u64().filter(|n| *n > 0),
        );
        let (Some(number), Some(attempt)) = order else {
            return Err(reject(
                "canonical CI run has no server-owned ordering evidence",
            ));
        };
        let order = (number, attempt);
        if let Some((previous, key)) = latest {
            if order == key && previous["id"] != run["id"] {
                return Err(reject("canonical CI attempts have ambiguous ordering"));
            }
        }
        if latest.is_none_or(|(_, key)| order > key) {
            latest = Some((run, order));
        }
    }
    let Some((run, _)) = latest else {
        return Ok(false);
    };
    // Select before checking status: an older success cannot hide a new
    // pending/failed attempt. Those are work-in-progress, not human approvals.
    if run["status"].as_str() != Some("completed") || run["conclusion"].as_str() != Some("success")
    {
        return Ok(false);
    }
    let id = run["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .ok_or_else(|| reject("canonical CI run has no id"))?;
    if !run["html_url"]
        .as_str()
        .and_then(|url| url.strip_prefix("https://github.com/"))
        .and_then(|path| path.strip_suffix(&format!("/actions/runs/{id}")))
        .is_some_and(|run_repo| run_repo.eq_ignore_ascii_case(repo))
    {
        return Err(reject("canonical CI run belongs to another repository"));
    }
    if require_queue {
        require_queue_authority(shared, repo, base, &rules, protection.as_ref().ok())?;
    }
    Ok(true)
}

/// Recheck the external enforcement boundary before postmerge settlement.
pub(super) fn unattended_queue_authority(
    shared: &Shared,
    repo: &str,
    base: &str,
    base_ref: &str,
) -> Result<()> {
    if default_branch(shared, repo)? != base_ref {
        return Err(reject(
            "the authorized target is no longer the repository default",
        ));
    }
    let branch = branch_segment(base_ref);
    let rules = api(shared, &format!("repos/{repo}/rules/branches/{branch}"))?;
    let protection = api(
        shared,
        &format!("repos/{repo}/branches/{branch}/protection"),
    )?;
    require_queue_authority(shared, repo, base, &rules, Some(&protection))
}

/// An enabled auto-merge request is not proof of admission to the queue.
pub(super) fn unattended_queue_entry(
    shared: &Shared,
    repo: &str,
    number: u64,
    head: &str,
    base: &str,
    base_ref: &str,
) -> Result<Option<String>> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| reject("invalid queue repository"))?;
    let query = format!(
        "query {{ repository(owner:{},name:{}) {{ pullRequest(number:{number}) {{ headRefOid baseRefOid baseRefName state mergeQueueEntry {{ id }} }} }} }}",
        json!(owner), json!(name),
    );
    let response: Value = serde_json::from_str(&gh(
        shared,
        &["api", "graphql", "-f", &format!("query={query}")],
    )?)
    .map_err(|_| reject("native merge queue response is unreadable"))?;
    if response
        .get("errors")
        .is_some_and(|errors| !errors.as_array().is_some_and(Vec::is_empty))
    {
        return Err(reject("native merge queue query failed"));
    }
    let pr = &response["data"]["repository"]["pullRequest"];
    if pr["headRefOid"] != head
        || pr["state"] != "OPEN"
        || pr["baseRefOid"] != base
        || pr["baseRefName"] != base_ref
    {
        return Err(reject(
            "queue observation no longer names the open authorized head",
        ));
    }
    if pr.get("mergeQueueEntry").is_none() {
        return Err(reject("native merge queue entry evidence is missing"));
    }
    if pr["mergeQueueEntry"].is_null() {
        return Ok(None);
    }
    pr["mergeQueueEntry"]["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(|id| Some(id.to_owned()))
        .ok_or_else(|| reject("native merge queue entry has no identity"))
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
    let base_ref = answer["base_ref"]
        .as_str()
        .ok_or_else(|| reject("missing base branch name"))?;
    let number = answer["pr"]
        .as_u64()
        .ok_or_else(|| reject("missing PR number"))?;
    let branch = branch_segment(base_ref);
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
    if view["headRefOid"].as_str() != Some(head)
        || view["baseRefOid"].as_str() != Some(base)
        || view["baseRefName"].as_str() != Some(base_ref)
    {
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
        || final_answer["base_ref"] != answer["base_ref"]
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
        base_ref: base_ref.to_string(),
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
