//! Report publication, comment ingestion, and quota-gated PM dispatch for
//! [`super`].

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::fs;
use std::path::Path;

use serde_json::{json, Value};

use super::{
    cap_chars, event_marker, iso_now, issue_marker_map, list_all_comments, list_all_issues,
    managed_issue_for_project, mark_published, mark_retry, marker, now, receipt, recover_claims,
    report_id_from_body, report_summary, retry_due, safe_text, save_state, validate_project_key,
    validate_repo, GithubApi, GithubIssue, PendingAction, RelayProjectConfig, RelayProjectState,
    RelayState, CLAIM_SECONDS, MAX_SUMMARY_CHARS,
};
use crate::client;
use crate::error::Result;
use crate::issue::{board, time};

pub(super) fn actionable_comment(body: &str) -> bool {
    let normalized = body.trim().to_ascii_lowercase();
    if normalized.is_empty() || normalized.starts_with("<!-- cadence-relay:") {
        return false;
    }
    !matches!(
        normalized.as_str(),
        "ack" | "/ack" | "acknowledged" | "received" | "thanks" | "thank you"
    )
}

pub(super) fn ingest_comments<T: GithubApi>(
    api: &mut T,
    repo: &str,
    project_key: &str,
    config: &RelayProjectConfig,
    project_state: &mut RelayProjectState,
    issues: &[GithubIssue],
) -> (usize, Vec<String>) {
    let mut queued = 0;
    let mut errors = Vec::new();
    let mut complete = true;
    let mut max_updated = project_state.cursor.clone();
    for issue in issues
        .iter()
        .filter(|issue| managed_issue_for_project(issue, project_key))
    {
        let report_id = issue
            .body
            .as_deref()
            .and_then(report_id_from_body)
            .unwrap_or_else(|| format!("github-{}", issue.number));
        let comments = match list_all_comments(api, repo, issue.number) {
            Ok(rows) => rows,
            Err(error) => {
                errors.push(safe_text(&error, 600));
                complete = false;
                continue;
            }
        };
        for comment in comments {
            let event_id = format!("comment:{repo}:{}:{}", issue.number, comment.id);
            if project_state.has_seen(&event_id) {
                continue;
            }
            project_state.remember(event_id.clone());
            let own = comment.body.contains("<!-- cadence-relay:")
                || config
                    .actor
                    .as_deref()
                    .zip(comment.user.as_ref().map(|u| u.login.as_str()))
                    .is_some_and(|(expected, actual)| !expected.is_empty() && expected == actual);
            if own || !actionable_comment(&comment.body) {
                continue;
            }
            let summary = safe_text(&comment.body, MAX_SUMMARY_CHARS);
            project_state
                .actions
                .entry(event_id.clone())
                .or_insert_with(|| {
                    queued += 1;
                    PendingAction {
                        event_id: event_id.clone(),
                        report_id: report_id.clone(),
                        issue_number: issue.number,
                        summary,
                        state: "queued".to_string(),
                        attempts: 0,
                        reason: Some(
                            "actionable comment queued; PM dispatch is explicit".to_string(),
                        ),
                        claim_owner: None,
                        claim_until: None,
                        updated_at: iso_now(),
                    }
                });
        }
        if let Some(updated) = &issue.updated_at {
            if max_updated
                .as_ref()
                .map(|old| old < updated)
                .unwrap_or(true)
            {
                max_updated = Some(updated.clone());
            }
        }
    }
    // A failed comment page must not move the watermark past work we did
    // not finish. The next poll's inclusive `since` overlaps this cursor,
    // and `remember` keeps the newest event ids.
    if complete {
        project_state.cursor = max_updated;
    }
    (queued, errors)
}

fn pm_alias(pm_dir: &Path, project_key: &str, config: &RelayProjectConfig) -> Option<String> {
    if let Some(alias) = config.pm_alias.as_deref().filter(|a| !a.is_empty()) {
        return Some(alias.to_string());
    }
    let text = fs::read_to_string(pm_dir.join(project_key).join("team.yaml")).ok()?;
    let value: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    value["roles"]["pm"]["alias"].as_str().map(str::to_string)
}

/// Same bound as `automatic_quota_error`: provider evidence older than this
/// is not a grant to wake a PM.
const QUOTA_EVIDENCE_MAX_AGE_SECS: i64 = 300;

/// `agent_show` stores `canonical_quota`, not a top-level `remaining`.
/// `state: available` only means the account id and rate-limit object were
/// present. Usage lives at `data.rateLimits.primary.usedPercent`, and a
/// stale or unbound `observed_at` is not evidence.
pub(super) fn quota_decision(agent: &Value) -> std::result::Result<(), String> {
    if matches!(agent["state"].as_str(), Some("stopped" | "attention")) {
        return Err(format!(
            "PM agent is {}",
            agent["state"].as_str().unwrap_or("unavailable")
        ));
    }
    let Some(quota) = agent.get("quota").filter(|value| value.is_object()) else {
        return Err("quota unknown: no account allowance telemetry".to_string());
    };
    if quota["provider"].as_str() != agent["provider"].as_str() {
        return Err("quota unknown: allowance provider does not match agent".to_string());
    }
    if quota["assignee"].as_str() != agent["alias"].as_str() {
        return Err("quota unknown: allowance is not bound to this agent".to_string());
    }
    let Some(thread_id) = agent["thread_id"].as_str().filter(|id| !id.is_empty()) else {
        return Err("quota unknown: agent has no current provider thread".to_string());
    };
    if quota["thread_id"].as_str() != Some(thread_id) {
        return Err("quota unknown: allowance is not bound to the current thread".to_string());
    }
    let Some(account_id) = quota["account_id"].as_str().filter(|id| !id.is_empty()) else {
        return Err("quota unknown: provider evidence has no account identity".to_string());
    };
    if quota.pointer("/data/accountId").and_then(Value::as_str) != Some(account_id) {
        return Err("quota unknown: account identity is not provider-bound".to_string());
    }
    if !matches!(
        quota["source"].as_str(),
        Some("account/rateLimits/read") | Some("account/rateLimits/updated")
    ) {
        return Err("quota unknown: provider evidence source is not canonical".to_string());
    }
    if quota["state"].as_str() != Some("available") {
        let state = quota["state"].as_str().unwrap_or("unknown");
        return Err(format!("quota {state}"));
    }
    let Some(observed_at) = quota["observed_at"].as_str().and_then(time::parse_iso) else {
        return Err("quota unknown: provider evidence has no canonical timestamp".to_string());
    };
    let age = i128::from(now()) - i128::from(observed_at);
    if age < -30 || age > i128::from(QUOTA_EVIDENCE_MAX_AGE_SECS) {
        return Err("quota unknown: provider allowance evidence is stale".to_string());
    }
    let Some(used) = quota
        .pointer("/data/rateLimits/primary/usedPercent")
        .and_then(Value::as_f64)
    else {
        return Err("quota unknown: no primary usedPercent".to_string());
    };
    if !used.is_finite() || used >= 100.0 {
        return Err("quota exhausted".to_string());
    }
    Ok(())
}

fn quota_allows(state_dir: &Path, alias: &str) -> std::result::Result<(), String> {
    let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))
        .map_err(|_| "quota unknown: PM agent state is unavailable".to_string())?;
    quota_decision(&show["agent"])
}

pub(super) fn dispatch_pending(
    state_dir: &Path,
    pm_dir: &Path,
    project_key: &str,
    config: &RelayProjectConfig,
    state: &mut RelayState,
    requested: bool,
) -> usize {
    let project_state = state.projects.entry(project_key.to_string()).or_default();
    if !requested {
        return 0;
    }
    let Some(alias) = pm_alias(pm_dir, project_key, config) else {
        for action in project_state
            .actions
            .values_mut()
            .filter(|a| a.state == "queued")
        {
            action.reason =
                Some("PM alias is not configured; pending durable dispatch".to_string());
            action.updated_at = iso_now();
        }
        return 0;
    };
    if !config.dispatch {
        for action in project_state
            .actions
            .values_mut()
            .filter(|a| a.state == "queued")
        {
            action.reason = Some("dispatch is disabled in relay configuration".to_string());
            action.updated_at = iso_now();
        }
        return 0;
    }
    if let Err(reason) = quota_allows(state_dir, &alias) {
        for action in project_state
            .actions
            .values_mut()
            .filter(|a| a.state == "queued")
        {
            action.reason = Some(reason.clone());
            action.updated_at = iso_now();
        }
        return 0;
    }
    let owner = format!("relay-{}-{}", std::process::id(), now());
    let action_ids: Vec<String> = project_state
        .actions
        .iter()
        .filter(|(_, action)| action.state == "queued" && action.claim_until.is_none())
        .map(|(id, _)| id.clone())
        .collect();
    let mut sent = 0;
    for event_id in action_ids {
        let Some((message, message_id)) = (|| {
            let project_state = state.projects.get_mut(project_key)?;
            let action = project_state.actions.get_mut(&event_id)?;
            if action.state != "queued" || action.claim_until.is_some() {
                return None;
            }
            action.state = "claimed".to_string();
            action.claim_owner = Some(owner.clone());
            action.claim_until = Some(now() + CLAIM_SECONDS);
            action.updated_at = iso_now();
            let message = format!(
                "[cadence intake] report {} on GitHub issue #{}\n\n{}\n\n{}",
                action.report_id,
                action.issue_number,
                action.summary,
                event_marker(&action.event_id)
            );
            let message_id = format!("intake-{}", safe_id(&action.event_id));
            Some((message, message_id))
        })() else {
            continue;
        };

        // Claim before the provider call and flush it.  A crashed consumer
        // is then recovered by `recover_claims` instead of a second worker
        // claiming the same event immediately.
        if let Err(error) = save_state(state_dir, state) {
            if let Some(action) = state
                .projects
                .get_mut(project_key)
                .and_then(|p| p.actions.get_mut(&event_id))
            {
                action.state = "retrying".to_string();
                action.reason = Some(format!(
                    "could not persist PM claim: {}",
                    safe_text(&error.to_string(), 300)
                ));
                action.claim_owner = None;
                action.claim_until = None;
                action.updated_at = iso_now();
            }
            continue;
        }
        match client::rpc(
            state_dir,
            "agent_send",
            json!({"alias": alias, "text": message, "message": message_id,
                   "source": "intake-relay"}),
        ) {
            Ok(_) => {
                if let Some(action) = state
                    .projects
                    .get_mut(project_key)
                    .and_then(|p| p.actions.get_mut(&event_id))
                {
                    action.state = "dispatched".to_string();
                    action.reason = None;
                    action.claim_owner = None;
                    action.claim_until = None;
                    action.updated_at = iso_now();
                }
                sent += 1;
            }
            Err(error) => {
                if let Some(action) = state
                    .projects
                    .get_mut(project_key)
                    .and_then(|p| p.actions.get_mut(&event_id))
                {
                    action.state = "retrying".to_string();
                    action.attempts = action.attempts.saturating_add(1);
                    action.reason =
                        Some("PM dispatch failed; durable retry remains queued".to_string());
                    action.claim_owner = None;
                    action.claim_until = None;
                    action.updated_at = iso_now();
                    action.summary = safe_text(&action.summary, MAX_SUMMARY_CHARS);
                    if error.to_string().contains("quota") {
                        action.reason =
                            Some("quota blocked; durable retry remains queued".to_string());
                    }
                }
            }
        }
        // Persist both the successful outbox handoff and a failed attempt.
        // The next loop can still save a newer state if the disk is briefly
        // unavailable, while a restart sees the pre-send claim.
        let _ = save_state(state_dir, state);
    }
    sent
}

fn safe_id(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    cap_chars(out.trim_matches('-'), 60)
}

pub(super) fn publish_local_reports<T: GithubApi>(
    pm_dir: &Path,
    project_key: &str,
    config: &RelayProjectConfig,
    project_state: &mut RelayProjectState,
    api: &mut T,
) -> (usize, usize, Vec<String>) {
    let issues = match board::load_all(pm_dir, Some(project_key)) {
        Ok(issues) => issues,
        Err(error) => return (0, 0, vec![error.to_string()]),
    };
    let reports: Vec<_> = issues
        .iter()
        .filter(|issue| issue.front.tags.iter().any(|tag| tag == "intake"))
        .collect();
    let remote = match list_all_issues(api, &config.repo, None) {
        Ok(issues) => issues,
        Err(error) => {
            for issue in &reports {
                let r = receipt(project_state, &issue.front.id);
                mark_retry(r, &error);
                r.reason = Some("GitHub read failed; publication was not attempted".to_string());
            }
            return (0, 0, vec![safe_text(&error, 600)]);
        }
    };
    let markers = issue_marker_map(&remote, project_key);
    let mut published = 0;
    let mut pending = 0;
    let mut errors = Vec::new();
    for issue in reports {
        let id = issue.front.id.clone();
        let source_rev = crate::issue::write::issue_rev(&issue.dir).ok();
        let r = receipt(project_state, &id);
        if let Some(remote_issue) = markers.get(&id) {
            mark_published(r, remote_issue, source_rev);
            continue;
        }
        if r.state == "published" || (r.state == "retrying" && !retry_due(r)) {
            continue;
        }
        let (title, summary) = report_summary(issue);
        let body = format!(
            "{}\n\nSource: local report `{}`\n\n{}",
            marker(project_key, &id),
            id,
            summary
        );
        let title = format!("[Cadence report] {title}");
        // CAD-109: the sanitized summary still passes the secret scan
        // before it can cross the GitHub boundary. A blocked report stays
        // local and is re-checked on every pass, so an edit that removes
        // the value publishes it.
        if let Err(refused) = crate::secret::guard(
            &format!("intake relay of {id}"),
            &format!("{title}\n{body}"),
        ) {
            let reason = refused.to_string();
            if r.state != "blocked" || r.reason.as_deref() != Some(reason.as_str()) {
                errors.push(reason.clone());
                r.state = "blocked".to_string();
                r.reason = Some(reason);
                r.next_retry_at = None;
                r.updated_at = iso_now();
            }
            continue;
        }
        // The receipt is persisted by `sync_once` after this bounded pass.
        // If the process dies after GitHub creates the issue, the next run
        // reconciles the stable marker before attempting another create.
        r.state = "publishing".to_string();
        r.reason = Some("publishing a sanitized summary".to_string());
        r.updated_at = iso_now();
        match api.create_issue(&config.repo, &title, &body) {
            Ok(remote_issue) => {
                mark_published(r, &remote_issue, source_rev);
                published += 1;
            }
            Err(error) => {
                // A timeout is ambiguous. Reconcile by marker before making
                // any retry decision; a failed read leaves the receipt
                // retrying rather than risking a duplicate issue.
                let mut unresolved = false;
                match list_all_issues(api, &config.repo, None) {
                    Ok(after) => {
                        if let Some(remote_issue) = issue_marker_map(&after, project_key).get(&id) {
                            mark_published(r, remote_issue, source_rev);
                            published += 1;
                        } else {
                            mark_retry(r, &error);
                            pending += 1;
                            unresolved = true;
                        }
                    }
                    Err(read_error) => {
                        mark_retry(r, &format!("{error}; reconciliation failed: {read_error}"));
                        pending += 1;
                        unresolved = true;
                    }
                }
                if unresolved {
                    errors.push(safe_text(&error, 600));
                }
            }
        }
    }
    (published, pending, errors)
}

pub(super) fn sync_project<T: GithubApi>(
    pm_dir: &Path,
    state_dir: &Path,
    project_key: &str,
    config: &RelayProjectConfig,
    state: &mut RelayState,
    api: &mut T,
    dispatch: bool,
) -> Result<Value> {
    validate_project_key(project_key)?;
    validate_repo(&config.repo)?;
    let (published, pending, mut errors) = {
        let project_state = state.projects.entry(project_key.to_string()).or_default();
        project_state.last_check_at = Some(iso_now());
        recover_claims(project_state);
        publish_local_reports(pm_dir, project_key, config, project_state, api)
    };
    let since = state
        .projects
        .get(project_key)
        .and_then(|project| project.cursor.clone());
    let listed = match list_all_issues(api, &config.repo, since.as_deref()) {
        Ok(issues) => Some(issues),
        Err(error) => {
            errors.push(safe_text(&error, 600));
            None
        }
    };
    let (queued_comments, comment_errors) = if let Some(remote) = listed {
        let project_state = state.projects.entry(project_key.to_string()).or_default();
        ingest_comments(
            api,
            &config.repo,
            project_key,
            config,
            project_state,
            &remote,
        )
    } else {
        // A truncated or failed issue list is not an empty discussion.
        // Leave the cursor where it was and do not mark comments seen.
        (0, Vec::new())
    };
    errors.extend(comment_errors);
    let dispatched = dispatch_pending(state_dir, pm_dir, project_key, config, state, dispatch);
    let project_state = state.projects.entry(project_key.to_string()).or_default();
    if errors.is_empty() {
        project_state.last_success_at = project_state.last_check_at.clone();
        project_state.last_error = None;
    } else {
        project_state.last_error = Some(safe_text(&errors.join("; "), 600));
    }
    Ok(json!({
        "project": project_key,
        "repo": config.repo,
        "enabled": config.enabled,
        "published": published,
        "pending": pending,
        "queued_comments": queued_comments,
        "dispatched": dispatched,
        "cursor": project_state.cursor,
        "last_check_at": project_state.last_check_at,
        "last_success_at": project_state.last_success_at,
        "last_error": project_state.last_error,
        "errors": errors,
    }))
}
