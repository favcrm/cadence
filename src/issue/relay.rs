//! Durable local report relay and the small, non-model intake poller.
//!
//! The relay deliberately lives beside the tracker rather than inside a
//! provider adapter.  A report is first represented by a local intake issue;
//! the relay then publishes a bounded, redacted summary to an explicitly
//! configured GitHub repository.  The local state file is the source of
//! truth for publication attempts, cursors, deduplication and claims.
//!
//! CAD-136 supplies the `intake` issue/tag shape when its PR is merged.  This
//! module only consumes the stable issue-folder contract (`intake` tag and
//! title/body), so it can be compiled and tested independently of that PR.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::client;
use crate::doctor::host::redact_argv;
use crate::error::{Error, Result};
use crate::issue::{board, model, time};
use crate::proc::run_bounded;

pub const CONFIG_FILE: &str = "intake-relay.yaml";
pub const STATE_FILE: &str = "intake-relay-state.json";
pub const DEFAULT_POLL_SECONDS: u64 = 300;
pub const RELAY_LABEL: &str = "cadence-report";
const STATE_SCHEMA: u32 = 1;
const CONFIG_SCHEMA: u32 = 1;
const MAX_SUMMARY_CHARS: usize = 4_000;
const MAX_TITLE_CHARS: usize = 180;
const GH_TIMEOUT: Duration = Duration::from_secs(20);
const CLAIM_SECONDS: i64 = 300;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelayProjectConfig {
    #[serde(default)]
    pub enabled: bool,
    pub repo: String,
    #[serde(default = "default_poll_seconds")]
    pub poll_seconds: u64,
    /// Dispatch is a second, explicit gate after relay enablement.  A
    /// publish-only deployment never wakes a provider agent.
    #[serde(default)]
    pub dispatch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pm_alias: Option<String>,
    /// GitHub login/name used to suppress the relay's own echoed comments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
}

fn default_poll_seconds() -> u64 {
    DEFAULT_POLL_SECONDS
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelayConfig {
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default)]
    pub projects: BTreeMap<String, RelayProjectConfig>,
}

fn default_schema() -> u32 {
    CONFIG_SCHEMA
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            schema: CONFIG_SCHEMA,
            projects: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RelayState {
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default)]
    pub projects: BTreeMap<String, RelayProjectState>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RelayProjectState {
    /// Highest GitHub `updated_at` observed.  It is an overlap watermark:
    /// event IDs below remain deduplicated when GitHub reorders rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// The last poll attempt that completed its local state update.  This is
    /// a heartbeat for the relay process, not a claim that GitHub or PM
    /// delivery succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check_at: Option<String>,
    /// Only set after the GitHub read/publish/comment pass completed without
    /// a transport error.  A stale value remains visible when auth/network
    /// failures prevent a fresh confirmation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub reports: BTreeMap<String, ReportReceipt>,
    #[serde(default)]
    pub actions: BTreeMap<String, PendingAction>,
    #[serde(default)]
    pub seen_events: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReportReceipt {
    pub report_id: String,
    pub state: String,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_rev: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_at: Option<i64>,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingAction {
    pub event_id: String,
    pub report_id: String,
    pub issue_number: u64,
    pub summary: String,
    pub state: String,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_until: Option<i64>,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GithubLabel {
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GithubUser {
    #[serde(default)]
    pub login: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GithubIssue {
    pub number: u64,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub labels: Vec<GithubLabel>,
    #[serde(default)]
    pub pull_request: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GithubComment {
    pub id: u64,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub user: Option<GithubUser>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// The production transport is a bounded `gh api` call.  Tests use this
/// trait to prove retry and deduplication without reaching GitHub.
pub trait GithubApi {
    fn list_issues(
        &mut self,
        repo: &str,
        page: u32,
    ) -> std::result::Result<Vec<GithubIssue>, String>;
    fn list_comments(
        &mut self,
        repo: &str,
        issue: u64,
        page: u32,
    ) -> std::result::Result<Vec<GithubComment>, String>;
    fn create_issue(
        &mut self,
        repo: &str,
        title: &str,
        body: &str,
    ) -> std::result::Result<GithubIssue, String>;
}

#[derive(Default)]
pub struct GhApi;

fn gh_json(args: &[String]) -> std::result::Result<Value, String> {
    let mut cmd = Command::new("gh");
    cmd.args(args);
    let out = run_bounded(&mut cmd, GH_TIMEOUT).map_err(|e| format!("GitHub request: {e}"))?;
    if !out.status.success() {
        let err = safe_text(&String::from_utf8_lossy(&out.stderr), 600);
        return Err(if err.is_empty() {
            "GitHub request failed".to_string()
        } else {
            err
        });
    }
    serde_json::from_slice(&out.stdout).map_err(|_| "GitHub returned invalid JSON".to_string())
}

fn gh_resource(repo: &str, suffix: &str) -> String {
    format!("repos/{repo}/{suffix}")
}

impl GithubApi for GhApi {
    fn list_issues(
        &mut self,
        repo: &str,
        page: u32,
    ) -> std::result::Result<Vec<GithubIssue>, String> {
        let path = format!(
            "{}?state=all&per_page=100&page={page}",
            gh_resource(repo, "issues")
        );
        let value = gh_json(&["api".into(), path])?;
        serde_json::from_value(value)
            .map_err(|_| "GitHub issues response had an unexpected shape".to_string())
    }

    fn list_comments(
        &mut self,
        repo: &str,
        issue: u64,
        page: u32,
    ) -> std::result::Result<Vec<GithubComment>, String> {
        let path = format!(
            "{}?per_page=100&page={page}",
            gh_resource(repo, &format!("issues/{issue}/comments"))
        );
        let value = gh_json(&["api".into(), path])?;
        serde_json::from_value(value)
            .map_err(|_| "GitHub comments response had an unexpected shape".to_string())
    }

    fn create_issue(
        &mut self,
        repo: &str,
        title: &str,
        body: &str,
    ) -> std::result::Result<GithubIssue, String> {
        let args = vec![
            "api".to_string(),
            "--method".to_string(),
            "POST".to_string(),
            gh_resource(repo, "issues"),
            "-f".to_string(),
            format!("title={title}"),
            "-f".to_string(),
            format!("body={body}"),
            "-f".to_string(),
            format!("labels[]={RELAY_LABEL}"),
        ];
        let value = gh_json(&args)?;
        serde_json::from_value(value)
            .map_err(|_| "GitHub create response had an unexpected shape".to_string())
    }
}

pub struct RelayLock {
    path: PathBuf,
}

impl Drop for RelayLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn acquire_lock(state_dir: &Path) -> Result<RelayLock> {
    fs::create_dir_all(state_dir)?;
    let path = state_dir.join("intake-relay.lock");
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(_) => Ok(RelayLock { path }),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(Error::rejected(
            "intake relay is already running; wait for the existing sync or inspect its state",
        )),
        Err(e) => Err(e.into()),
    }
}

fn atomic_write(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, path)?;
    Ok(())
}

pub fn load_config(state_dir: &Path) -> Result<RelayConfig> {
    let path = state_dir.join(CONFIG_FILE);
    if !path.is_file() {
        return Ok(RelayConfig::default());
    }
    let text = fs::read_to_string(&path)?;
    let config: RelayConfig = serde_yaml::from_str(&text)
        .map_err(|e| Error::rejected(format!("{} is invalid: {e}", path.display())))?;
    if config.schema != CONFIG_SCHEMA {
        return Err(Error::rejected(format!(
            "unsupported relay config schema {} (expected {})",
            config.schema, CONFIG_SCHEMA
        )));
    }
    for (project, item) in &config.projects {
        validate_project_key(project)?;
        validate_repo(&item.repo)?;
        if item.poll_seconds == 0 {
            return Err(Error::rejected(format!(
                "relay poll_seconds for {project} must be positive"
            )));
        }
    }
    Ok(config)
}

pub fn save_config(state_dir: &Path, config: &RelayConfig) -> Result<()> {
    fs::create_dir_all(state_dir)?;
    let text =
        serde_yaml::to_string(config).map_err(|e| Error::internal(format!("relay config: {e}")))?;
    atomic_write(&state_dir.join(CONFIG_FILE), &text)
}

pub fn load_state(state_dir: &Path) -> Result<RelayState> {
    let path = state_dir.join(STATE_FILE);
    if !path.is_file() {
        return Ok(RelayState {
            schema: STATE_SCHEMA,
            ..RelayState::default()
        });
    }
    let text = fs::read_to_string(&path)?;
    let state: RelayState = serde_json::from_str(&text)
        .map_err(|e| Error::rejected(format!("{} is invalid: {e}", path.display())))?;
    if state.schema != STATE_SCHEMA {
        return Err(Error::rejected(format!(
            "unsupported relay state schema {} (expected {})",
            state.schema, STATE_SCHEMA
        )));
    }
    Ok(state)
}

pub fn save_state(state_dir: &Path, state: &RelayState) -> Result<()> {
    fs::create_dir_all(state_dir)?;
    let text = serde_json::to_string_pretty(state)?;
    atomic_write(&state_dir.join(STATE_FILE), &text)
}

fn now() -> i64 {
    time::now_epoch()
}

fn iso_now() -> String {
    time::iso(now())
}

fn validate_project_key(key: &str) -> Result<()> {
    model::check_key(key).map(|_| ())
}

pub fn validate_repo(repo: &str) -> Result<()> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err(Error::rejected(
            "relay repo must be owner/name; credentials and URLs are not accepted",
        ));
    };
    let valid = |part: &str| {
        !part.is_empty()
            && part.len() <= 100
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    };
    if !valid(owner) || !valid(name) || name.contains("..") {
        return Err(Error::rejected(
            "relay repo must be owner/name; credentials and URLs are not accepted",
        ));
    }
    Ok(())
}

fn cap_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str("… [truncated]");
    out
}

/// Redact credential-shaped tokens and obvious key/value secret lines before
/// anything can cross the GitHub boundary.  The local report remains intact.
pub fn safe_text(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut pem = false;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let trim = line.trim_start();
        if trim.starts_with("-----BEGIN ") {
            pem = true;
        }
        if pem {
            out.push_str("[REDACTED]\n");
            if trim.starts_with("-----END ") {
                pem = false;
            }
            continue;
        }
        let lower = line.to_ascii_lowercase();
        let sensitive = [
            "password",
            "passwd",
            "token",
            "api_key",
            "apikey",
            "secret",
            "authorization",
            "private_key",
            "access_key",
        ]
        .iter()
        .any(|word| lower.contains(word));
        if sensitive {
            let mut cut = None;
            for needle in [":", "=", " is "] {
                if let Some(i) = lower.find(needle) {
                    cut = Some(i + needle.len());
                    break;
                }
            }
            if let Some(i) = cut {
                out.push_str(&line[..i]);
                out.push_str("[REDACTED]\n");
                continue;
            }
        }
        let tokens: Vec<String> = line.split_whitespace().map(str::to_string).collect();
        if tokens.is_empty() {
            out.push('\n');
        } else {
            out.push_str(&redact_argv(&tokens));
            out.push('\n');
        }
    }
    cap_chars(out.trim_end_matches('\n'), max)
}

fn report_kind(issue: &board::Issue) -> String {
    // CAD-136 stores the intake kind as frontmatter.  Reading that optional
    // field from disk keeps this branch buildable against the current board
    // model while accepting the post-PR73 shape.
    let frontmatter_kind = fs::read_to_string(issue.dir.join("issue.md"))
        .ok()
        .and_then(|text| {
            let mut parts = text.splitn(3, "---");
            let _ = parts.next()?;
            let yaml = parts.next()?;
            let value: serde_yaml::Value = serde_yaml::from_str(yaml).ok()?;
            value.get("kind")?.as_str().map(str::to_string)
        })
        .filter(|kind| {
            !kind.is_empty()
                && kind.len() <= 32
                && kind
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        });
    frontmatter_kind
        .or_else(|| {
            issue
                .front
                .tags
                .iter()
                .find(|tag| tag.as_str() != "intake")
                .cloned()
        })
        .unwrap_or_else(|| "report".to_string())
}

fn report_summary(issue: &board::Issue) -> (String, String) {
    let title = cap_chars(
        &safe_text(&issue.front.title, MAX_TITLE_CHARS),
        MAX_TITLE_CHARS,
    );
    let body = issue
        .body
        .split_once("## Report context")
        .map(|(body, _)| body)
        .unwrap_or(&issue.body);
    let body = body
        .lines()
        .skip_while(|line| line.trim() == issue.front.title.trim())
        .collect::<Vec<_>>()
        .join("\n");
    let kind = report_kind(issue);
    let summary = safe_text(&body, MAX_SUMMARY_CHARS);
    let summary = if summary.trim().is_empty() {
        format!("kind: {kind}")
    } else {
        format!("kind: {kind}\n\n{summary}")
    };
    (title, summary)
}

fn project_marker(project_key: &str) -> String {
    format!("<!-- cadence-relay:project={project_key} -->")
}

fn marker(project_key: &str, report_id: &str) -> String {
    format!(
        "{}\n<!-- cadence-relay:report={report_id} -->",
        project_marker(project_key)
    )
}

fn event_marker(event_id: &str) -> String {
    format!("<!-- cadence-relay:event={event_id} -->")
}

fn report_id_from_body(body: &str) -> Option<String> {
    let prefix = "<!-- cadence-relay:report=";
    let start = body.find(prefix)? + prefix.len();
    let rest = &body[start..];
    let end = rest.find(" -->")?;
    let id = &rest[..end];
    model_id(id).then(|| id.to_string())
}

fn project_key_from_body(body: &str) -> Option<&str> {
    let prefix = "<!-- cadence-relay:project=";
    let start = body.find(prefix)? + prefix.len();
    let rest = &body[start..];
    let end = rest.find(" -->")?;
    let key = &rest[..end];
    model::valid_key(key).then_some(key)
}

fn model_id(id: &str) -> bool {
    crate::issue::model::valid_id(id)
}

fn managed_issue(issue: &GithubIssue) -> bool {
    issue.pull_request.is_none()
        && (issue.labels.iter().any(|label| label.name == RELAY_LABEL)
            || issue
                .body
                .as_deref()
                .and_then(report_id_from_body)
                .is_some())
}

fn managed_issue_for_project(issue: &GithubIssue, project_key: &str) -> bool {
    managed_issue(issue)
        && issue.body.as_deref().is_some_and(|body| {
            project_key_from_body(body) == Some(project_key) && report_id_from_body(body).is_some()
        })
}

fn issue_marker_map(issues: &[GithubIssue], project_key: &str) -> BTreeMap<String, GithubIssue> {
    issues
        .iter()
        .filter(|issue| managed_issue_for_project(issue, project_key))
        .filter_map(|issue| {
            let body = issue.body.as_deref()?;
            report_id_from_body(body).map(|id| (id, issue.clone()))
        })
        .collect()
}

fn list_all_issues<T: GithubApi>(
    api: &mut T,
    repo: &str,
) -> std::result::Result<Vec<GithubIssue>, String> {
    let mut all = Vec::new();
    for page in 1..=10 {
        let rows = api.list_issues(repo, page)?;
        let done = rows.len() < 100;
        all.extend(rows);
        if done {
            break;
        }
    }
    Ok(all)
}

fn receipt<'a>(state: &'a mut RelayProjectState, id: &str) -> &'a mut ReportReceipt {
    state
        .reports
        .entry(id.to_string())
        .or_insert_with(|| ReportReceipt {
            report_id: id.to_string(),
            state: "pending".to_string(),
            attempts: 0,
            github_number: None,
            github_url: None,
            source_rev: None,
            reason: Some("captured locally; relay has not published it".to_string()),
            last_error: None,
            next_retry_at: None,
            updated_at: iso_now(),
        })
}

fn mark_published(receipt: &mut ReportReceipt, issue: &GithubIssue, source_rev: Option<String>) {
    receipt.state = "published".to_string();
    receipt.github_number = Some(issue.number);
    receipt.github_url = (!issue.html_url.is_empty()).then(|| issue.html_url.clone());
    receipt.source_rev = source_rev;
    receipt.reason = None;
    receipt.last_error = None;
    receipt.next_retry_at = None;
    receipt.updated_at = iso_now();
}

fn mark_retry(receipt: &mut ReportReceipt, error: &str) {
    receipt.attempts = receipt.attempts.saturating_add(1);
    receipt.state = "retrying".to_string();
    receipt.last_error = Some(safe_text(error, 600));
    receipt.reason = Some("publication is pending; no blind duplicate was created".to_string());
    let backoff = 30_i64.saturating_mul(1_i64 << receipt.attempts.min(7));
    receipt.next_retry_at = Some(now().saturating_add(backoff.min(3600)));
    receipt.updated_at = iso_now();
}

fn retry_due(receipt: &ReportReceipt) -> bool {
    receipt.next_retry_at.map(|at| at <= now()).unwrap_or(true)
}

fn recover_claims(state: &mut RelayProjectState) {
    let current = now();
    for action in state.actions.values_mut() {
        if action.state == "claimed" && action.claim_until.unwrap_or(0) <= current {
            action.state = "queued".to_string();
            action.reason =
                Some("previous consumer claim expired; queued for reconciliation".to_string());
            action.claim_owner = None;
            action.claim_until = None;
            action.updated_at = iso_now();
        }
    }
}

fn actionable_comment(body: &str) -> bool {
    let normalized = body.trim().to_ascii_lowercase();
    if normalized.is_empty() || normalized.starts_with("<!-- cadence-relay:") {
        return false;
    }
    !matches!(
        normalized.as_str(),
        "ack" | "/ack" | "acknowledged" | "received" | "thanks" | "thank you"
    )
}

fn ingest_comments<T: GithubApi>(
    api: &mut T,
    repo: &str,
    project_key: &str,
    config: &RelayProjectConfig,
    project_state: &mut RelayProjectState,
    issues: &[GithubIssue],
) -> (usize, Vec<String>) {
    let mut queued = 0;
    let mut errors = Vec::new();
    for issue in issues
        .iter()
        .filter(|issue| managed_issue_for_project(issue, project_key))
    {
        let report_id = issue
            .body
            .as_deref()
            .and_then(report_id_from_body)
            .unwrap_or_else(|| format!("github-{}", issue.number));
        let mut comments = Vec::new();
        let mut comments_ok = true;
        for page in 1..=10 {
            let rows = match api.list_comments(repo, issue.number, page) {
                Ok(rows) => rows,
                Err(error) => {
                    errors.push(safe_text(&error, 600));
                    comments_ok = false;
                    break;
                }
            };
            let done = rows.len() < 100;
            comments.extend(rows);
            if done {
                break;
            }
        }
        if !comments_ok {
            continue;
        }
        for comment in comments {
            let event_id = format!("comment:{repo}:{}:{}", issue.number, comment.id);
            if project_state.seen_events.contains(&event_id) {
                continue;
            }
            project_state.seen_events.insert(event_id.clone());
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
            if project_state
                .cursor
                .as_ref()
                .map(|old| old < updated)
                .unwrap_or(true)
            {
                project_state.cursor = Some(updated.clone());
            }
        }
    }
    // Bound the durable dedup set.  The cursor still overlaps the next poll;
    // keeping the newest IDs is enough to suppress normal GitHub reordering.
    while project_state.seen_events.len() > 2_000 {
        if let Some(first) = project_state.seen_events.iter().next().cloned() {
            project_state.seen_events.remove(&first);
        }
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

fn quota_allows(state_dir: &Path, alias: &str) -> std::result::Result<(), String> {
    let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))
        .map_err(|_| "quota unknown: PM agent state is unavailable".to_string())?;
    let agent = &show["agent"];
    if matches!(agent["state"].as_str(), Some("stopped" | "attention")) {
        return Err(format!(
            "PM agent is {}",
            agent["state"].as_str().unwrap_or("unavailable")
        ));
    }
    let quota = agent
        .get("quota")
        .filter(|v| !v.is_null())
        .or_else(|| agent.get("usage_limit").filter(|v| !v.is_null()))
        .ok_or_else(|| "quota unknown: no account allowance telemetry".to_string())?;
    if quota["state"].as_str() != Some("available") {
        return Err(format!(
            "quota {}",
            quota["reason"]
                .as_str()
                .or(quota["state"].as_str())
                .unwrap_or("unknown")
        ));
    }
    if quota["remaining"].as_i64() == Some(0) || quota["used_percent"].as_f64() == Some(100.0) {
        return Err("quota exhausted".to_string());
    }
    Ok(())
}

fn dispatch_pending(
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

fn publish_local_reports<T: GithubApi>(
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
    let remote = match list_all_issues(api, &config.repo) {
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
                match list_all_issues(api, &config.repo) {
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

fn sync_project<T: GithubApi>(
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
    let remote = match list_all_issues(api, &config.repo) {
        Ok(issues) => issues,
        Err(error) => {
            errors.push(safe_text(&error, 600));
            Vec::new()
        }
    };
    let (queued_comments, comment_errors) = {
        let project_state = state.projects.entry(project_key.to_string()).or_default();
        ingest_comments(
            api,
            &config.repo,
            project_key,
            config,
            project_state,
            &remote,
        )
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

pub fn sync_once(
    pm_dir: &Path,
    state_dir: &Path,
    project_filter: Option<&str>,
    dispatch: bool,
) -> Result<Value> {
    let _lock = acquire_lock(state_dir)?;
    let config = load_config(state_dir)?;
    let mut state = load_state(state_dir)?;
    let selected: Vec<_> = config
        .projects
        .iter()
        .filter(|(key, item)| {
            item.enabled
                && project_filter
                    .map(|wanted| wanted == key.as_str())
                    .unwrap_or(true)
        })
        .collect();
    if selected.is_empty() {
        return Ok(json!({
            "state": "disabled",
            "reason": if project_filter.is_some() { "project is not configured and enabled" } else { "no relay project is explicitly enabled" },
            "projects": [],
        }));
    }
    let mut rows = Vec::new();
    for (key, item) in selected {
        let mut api = GhApi;
        rows.push(sync_project(
            pm_dir, state_dir, key, item, &mut state, &mut api, dispatch,
        )?);
        save_state(state_dir, &state)?;
    }
    Ok(json!({"state": "ok", "projects": rows}))
}

pub fn status(state_dir: &Path, project_filter: Option<&str>) -> Result<Value> {
    let config = load_config(state_dir)?;
    let state = load_state(state_dir)?;
    let mut projects = Vec::new();
    for (key, item) in &config.projects {
        if project_filter
            .map(|wanted| wanted != key.as_str())
            .unwrap_or(false)
        {
            continue;
        }
        let current = state.projects.get(key).cloned().unwrap_or_default();
        projects.push(json!({
            "project": key,
            "repo": item.repo,
            "enabled": item.enabled,
            "dispatch": item.dispatch,
            "poll_seconds": item.poll_seconds,
            "cursor": current.cursor,
            "last_check_at": current.last_check_at,
            "last_success_at": current.last_success_at,
            "last_error": current.last_error,
            "reports": current.reports.values().collect::<Vec<_>>(),
            "actions": current.actions.values().collect::<Vec<_>>(),
        }));
    }
    let any_enabled = projects.iter().any(|item| {
        item.get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    });
    Ok(json!({
        "state": if any_enabled { "configured" } else { "disabled" },
        "config": CONFIG_FILE,
        "state_file": STATE_FILE,
        "projects": projects,
    }))
}

#[allow(clippy::too_many_arguments)]
pub fn configure(
    state_dir: &Path,
    project_key: &str,
    repo: &str,
    enabled: bool,
    poll_seconds: u64,
    dispatch: bool,
    pm_alias: Option<String>,
    actor: Option<String>,
) -> Result<Value> {
    validate_project_key(project_key)?;
    validate_repo(repo)?;
    if poll_seconds == 0 {
        return Err(Error::rejected("poll interval must be positive"));
    }
    let _lock = acquire_lock(state_dir)?;
    let mut config = load_config(state_dir)?;
    config.projects.insert(
        project_key.to_string(),
        RelayProjectConfig {
            enabled,
            repo: repo.to_string(),
            poll_seconds,
            dispatch,
            pm_alias,
            actor,
        },
    );
    save_config(state_dir, &config)?;
    Ok(json!({
        "project": project_key,
        "repo": repo,
        "enabled": enabled,
        "dispatch": dispatch,
        "poll_seconds": poll_seconds,
    }))
}

pub fn retry(state_dir: &Path, project_key: &str, report_id: &str) -> Result<Value> {
    validate_project_key(project_key)?;
    crate::issue::model::check_id(report_id)?;
    let _lock = acquire_lock(state_dir)?;
    let mut state = load_state(state_dir)?;
    let project_state = state.projects.entry(project_key.to_string()).or_default();
    let item = project_state
        .reports
        .entry(report_id.to_string())
        .or_insert_with(|| ReportReceipt {
            report_id: report_id.to_string(),
            state: "pending".to_string(),
            attempts: 0,
            github_number: None,
            github_url: None,
            source_rev: None,
            reason: Some("explicit retry requested".to_string()),
            last_error: None,
            next_retry_at: None,
            updated_at: iso_now(),
        });
    item.state = "pending".to_string();
    item.reason = Some("explicit retry requested".to_string());
    item.last_error = None;
    item.next_retry_at = None;
    item.updated_at = iso_now();
    save_state(state_dir, &state)?;
    Ok(json!({"project": project_key, "report_id": report_id, "state": "pending"}))
}

pub fn run_loop(
    pm_dir: &Path,
    state_dir: &Path,
    project_filter: Option<&str>,
    dispatch: bool,
    once: bool,
) -> Result<Value> {
    let mut last = json!({"state": "disabled", "projects": []});
    loop {
        last = sync_once(pm_dir, state_dir, project_filter, dispatch)?;
        if once {
            return Ok(last);
        }
        let config = load_config(state_dir)?;
        let seconds = config
            .projects
            .iter()
            .filter(|(key, item)| {
                item.enabled
                    && project_filter
                        .map(|wanted| wanted == key.as_str())
                        .unwrap_or(true)
            })
            .map(|(_, item)| item.poll_seconds)
            .min()
            .unwrap_or(DEFAULT_POLL_SECONDS)
            .max(1);
        std::thread::sleep(Duration::from_secs(seconds));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[derive(Default)]
    struct MockApi {
        issues: Vec<GithubIssue>,
        comments: BTreeMap<u64, Vec<GithubComment>>,
        create_calls: usize,
        timeout_after_create: bool,
        fail_reads: bool,
    }

    impl GithubApi for MockApi {
        fn list_issues(
            &mut self,
            _repo: &str,
            _page: u32,
        ) -> std::result::Result<Vec<GithubIssue>, String> {
            if self.fail_reads {
                Err("auth unavailable".to_string())
            } else {
                Ok(self.issues.clone())
            }
        }
        fn list_comments(
            &mut self,
            _repo: &str,
            issue: u64,
            _page: u32,
        ) -> std::result::Result<Vec<GithubComment>, String> {
            if self.fail_reads {
                Err("auth unavailable".to_string())
            } else {
                Ok(self.comments.get(&issue).cloned().unwrap_or_default())
            }
        }
        fn create_issue(
            &mut self,
            _repo: &str,
            title: &str,
            body: &str,
        ) -> std::result::Result<GithubIssue, String> {
            self.create_calls += 1;
            let issue = GithubIssue {
                number: self.create_calls as u64,
                html_url: format!("https://github.com/fake/{}", self.create_calls),
                title: title.to_string(),
                body: Some(body.to_string()),
                updated_at: Some("2026-09-20T00:00:00Z".to_string()),
                labels: vec![GithubLabel {
                    name: RELAY_LABEL.to_string(),
                }],
                pull_request: None,
            };
            self.issues.push(issue.clone());
            if self.timeout_after_create {
                self.timeout_after_create = false;
                Err("request timed out after create".to_string())
            } else {
                Ok(issue)
            }
        }
    }

    fn temp_state() -> (TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        fs::create_dir_all(&root).unwrap();
        (temp, root)
    }

    fn intake_pm(temp: &TempDir) -> crate::issue::Pm {
        let pm = crate::issue::Pm::init(temp.path()).unwrap();
        crate::issue::write::project_add(
            &pm,
            "cadence",
            "CAD",
            &[],
            &[],
            &["intake".to_string(), "question".to_string()],
            None,
        )
        .unwrap();
        crate::issue::write::new_issue(
            &pm,
            temp.path(),
            Some("cadence"),
            "A report",
            None,
            None,
            &[],
            None,
            None,
            &["intake".to_string()],
            Some("CAD-1"),
            "",
        )
        .unwrap();
        // Simulate the post-CAD136 frontmatter field while compiling this
        // test against the current board model.
        let path = temp.path().join("cadence/CAD-1/issue.md");
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, text.replacen("---\n", "---\nkind: question\n", 1)).unwrap();
        pm
    }

    fn add_intake_project(
        pm: &crate::issue::Pm,
        temp: &TempDir,
        project_key: &str,
        prefix: &str,
        issue_id: &str,
        title: &str,
    ) {
        crate::issue::write::project_add(
            pm,
            project_key,
            prefix,
            &[],
            &[],
            &["intake".to_string(), "question".to_string()],
            None,
        )
        .unwrap();
        crate::issue::write::new_issue(
            pm,
            temp.path(),
            Some(project_key),
            title,
            None,
            None,
            &[],
            None,
            None,
            &["intake".to_string()],
            Some(issue_id),
            "",
        )
        .unwrap();
        let path = temp
            .path()
            .join(project_key)
            .join(issue_id)
            .join("issue.md");
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, text.replacen("---\n", "---\nkind: question\n", 1)).unwrap();
    }

    fn relay_config() -> RelayProjectConfig {
        RelayProjectConfig {
            enabled: true,
            repo: "fake/repo".to_string(),
            poll_seconds: 300,
            dispatch: false,
            pm_alias: None,
            actor: None,
        }
    }

    fn sample_issue(temp: &TempDir) -> board::Issue {
        let dir = temp.path().join("issue");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("issue.md"), "---\nkind: question\n---\n").unwrap();
        board::Issue {
            project: "cadence".to_string(),
            dir,
            front: crate::issue::model::Front {
                id: "CAD-1".to_string(),
                title: "A report".to_string(),
                status: "backlog".to_string(),
                priority: "P2".to_string(),
                owner: None,
                claim: None,
                component: None,
                tags: vec!["intake".to_string(), "feedback".to_string()],
                kind: Some("feedback".to_string()),
                plan: None,
                size: None,
                parent: None,
                blocked_by: vec![],
                relates: vec![],
                duplicate_of: None,
                refs: vec![],
                created: "2026-09-20T00:00:00Z".to_string(),
            },
            body: "A report\n\npassword: TEST_ONLY_REDACTION_SENTINEL_12345\nnormal detail"
                .to_string(),
            comments: vec![],
            artifacts: vec![],
        }
    }

    #[test]
    fn safe_summary_does_not_publish_secret_or_context() {
        let temp = tempfile::tempdir().unwrap();
        let issue = sample_issue(&temp);
        let (_, summary) = report_summary(&issue);
        assert!(!summary.contains("TEST_ONLY_REDACTION_SENTINEL_12345"));
        assert!(summary.contains("[REDACTED]"));
        assert!(summary.contains("normal detail"));
        assert!(!summary.contains("Report context"));
        assert!(summary.starts_with("kind: question"));
    }

    #[test]
    fn offline_receipt_survives_restart() {
        let (_temp, state_dir) = temp_state();
        let mut state = RelayState {
            schema: STATE_SCHEMA,
            ..Default::default()
        };
        let p = state.projects.entry("cadence".to_string()).or_default();
        receipt(p, "CAD-1");
        save_state(&state_dir, &state).unwrap();
        let reopened = load_state(&state_dir).unwrap();
        assert_eq!(
            reopened.projects["cadence"].reports["CAD-1"].state,
            "pending"
        );
    }

    /// CAD-109: a credential shape the summary scrubber lets through (a
    /// GitLab token is too short for its entropy test) is refused by the
    /// secret scan. Nothing reaches GitHub, the receipt names the rule and
    /// not the value, and an edit that removes the value publishes.
    #[test]
    fn credential_shaped_report_is_blocked_not_published() {
        use sha2::{Digest, Sha256};
        let pm_temp = tempfile::tempdir().unwrap();
        let pm = intake_pm(&pm_temp);
        let config = RelayProjectConfig {
            enabled: true,
            repo: "fake/repo".to_string(),
            poll_seconds: 300,
            dispatch: false,
            pm_alias: None,
            actor: None,
        };
        let digest: String = Sha256::digest(b"relay-fixture")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let token = ["gl", "pat-", &digest[..20]].concat();
        let path = pm_temp.path().join("cadence/CAD-1/issue.md");
        let clean = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{clean}\nDeploy with\n{token}\n")).unwrap();

        let mut api = MockApi::default();
        let mut state = RelayProjectState::default();
        let (published, pending, errors) =
            publish_local_reports(&pm.dir, "cadence", &config, &mut state, &mut api);
        assert_eq!((published, pending, api.create_calls), (0, 0, 0));
        let r = &state.reports["CAD-1"];
        assert_eq!(r.state, "blocked");
        let reason = r.reason.clone().unwrap();
        assert!(reason.contains("rule cadence-gitlab-pat"), "{reason}");
        assert!(!reason.contains(&token[6..]), "{reason}");
        assert_eq!(errors, vec![reason]);

        // A later pass re-checks without repeating the error.
        let (_, _, errors) =
            publish_local_reports(&pm.dir, "cadence", &config, &mut state, &mut api);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(api.create_calls, 0);

        fs::write(&path, clean).unwrap();
        let (published, _, errors) =
            publish_local_reports(&pm.dir, "cadence", &config, &mut state, &mut api);
        assert_eq!(published, 1, "{errors:?}");
        assert_eq!(state.reports["CAD-1"].state, "published");
    }

    #[test]
    fn timeout_after_create_reconciles_by_marker() {
        let pm_temp = tempfile::tempdir().unwrap();
        let pm = intake_pm(&pm_temp);
        let config = RelayProjectConfig {
            enabled: true,
            repo: "fake/repo".to_string(),
            poll_seconds: 300,
            dispatch: false,
            pm_alias: None,
            actor: None,
        };
        let mut api = MockApi {
            timeout_after_create: true,
            ..Default::default()
        };
        let mut state = RelayProjectState::default();
        let (published, pending, errors) =
            publish_local_reports(&pm.dir, "cadence", &config, &mut state, &mut api);
        assert_eq!(published, 1);
        assert_eq!(pending, 0);
        assert!(errors.is_empty());
        assert_eq!(state.reports["CAD-1"].state, "published");
        assert_eq!(api.create_calls, 1);
    }

    #[test]
    fn shared_repo_projects_publish_and_ingest_only_owned_comments() {
        let pm_temp = tempfile::tempdir().unwrap();
        let pm = intake_pm(&pm_temp);
        add_intake_project(&pm, &pm_temp, "other", "OTH", "OTH-1", "Other report");
        let config = relay_config();
        let mut api = MockApi::default();
        let mut cadence_state = RelayProjectState::default();
        let mut other_state = RelayProjectState::default();

        let (published, pending, errors) =
            publish_local_reports(&pm.dir, "cadence", &config, &mut cadence_state, &mut api);
        assert_eq!((published, pending), (1, 0));
        assert!(errors.is_empty());
        let (published, pending, errors) =
            publish_local_reports(&pm.dir, "other", &config, &mut other_state, &mut api);
        assert_eq!((published, pending), (1, 0));
        assert!(errors.is_empty());
        assert_eq!(api.create_calls, 2);
        assert_eq!(cadence_state.reports["CAD-1"].state, "published");
        assert_eq!(other_state.reports["OTH-1"].state, "published");

        let cadence_issue = api
            .issues
            .iter()
            .find(|issue| {
                issue
                    .body
                    .as_deref()
                    .is_some_and(|body| project_key_from_body(body) == Some("cadence"))
            })
            .unwrap()
            .number;
        let other_issue = api
            .issues
            .iter()
            .find(|issue| {
                issue
                    .body
                    .as_deref()
                    .is_some_and(|body| project_key_from_body(body) == Some("other"))
            })
            .unwrap()
            .number;
        api.issues.push(GithubIssue {
            number: 99,
            html_url: String::new(),
            title: "legacy report-only issue".to_string(),
            body: Some("<!-- cadence-relay:report=LEG-1 -->".to_string()),
            updated_at: Some("2026-09-20T00:00:03Z".to_string()),
            labels: vec![GithubLabel {
                name: RELAY_LABEL.to_string(),
            }],
            pull_request: None,
        });
        api.comments.insert(
            cadence_issue,
            vec![GithubComment {
                id: 1,
                body: "cadence action".to_string(),
                user: None,
                created_at: None,
                updated_at: None,
            }],
        );
        api.comments.insert(
            other_issue,
            vec![GithubComment {
                id: 2,
                body: "other action".to_string(),
                user: None,
                created_at: None,
                updated_at: None,
            }],
        );
        api.comments.insert(
            99,
            vec![GithubComment {
                id: 3,
                body: "unowned action".to_string(),
                user: None,
                created_at: None,
                updated_at: None,
            }],
        );

        let (_state_temp, state_dir) = temp_state();
        let mut state = RelayState::default();
        state.projects.insert("cadence".to_string(), cadence_state);
        state.projects.insert("other".to_string(), other_state);
        let cadence = sync_project(
            &pm.dir, &state_dir, "cadence", &config, &mut state, &mut api, false,
        )
        .unwrap();
        let other = sync_project(
            &pm.dir, &state_dir, "other", &config, &mut state, &mut api, false,
        )
        .unwrap();

        assert_eq!(cadence["published"], 0);
        assert_eq!(cadence["queued_comments"], 1);
        assert_eq!(cadence["dispatched"], 0);
        assert_eq!(other["published"], 0);
        assert_eq!(other["queued_comments"], 1);
        assert_eq!(other["dispatched"], 0);
        assert_eq!(state.projects["cadence"].actions.len(), 1);
        assert_eq!(state.projects["other"].actions.len(), 1);
        assert_eq!(
            state.projects["cadence"]
                .actions
                .values()
                .next()
                .unwrap()
                .report_id,
            "CAD-1"
        );
        assert_eq!(
            state.projects["other"]
                .actions
                .values()
                .next()
                .unwrap()
                .report_id,
            "OTH-1"
        );
        assert!(!state.projects["cadence"]
            .actions
            .values()
            .any(|action| action.report_id == "LEG-1"));
        assert!(!state.projects["other"]
            .actions
            .values()
            .any(|action| action.report_id == "LEG-1"));
    }

    #[test]
    fn reordered_events_and_self_echo_are_deduplicated() {
        let (_temp, state_dir) = temp_state();
        let config = RelayProjectConfig {
            enabled: true,
            repo: "fake/repo".to_string(),
            poll_seconds: 300,
            dispatch: false,
            pm_alias: None,
            actor: Some("relay-bot".to_string()),
        };
        let mut api = MockApi {
            issues: vec![GithubIssue {
                number: 7,
                html_url: String::new(),
                title: "report".to_string(),
                body: Some(marker("cadence", "CAD-1")),
                updated_at: Some("2026-09-20T00:00:02Z".to_string()),
                labels: vec![GithubLabel {
                    name: RELAY_LABEL.to_string(),
                }],
                pull_request: None,
            }],
            comments: [(
                7,
                vec![
                    GithubComment {
                        id: 2,
                        body: "real action".to_string(),
                        user: None,
                        created_at: None,
                        updated_at: None,
                    },
                    GithubComment {
                        id: 1,
                        body: "ack".to_string(),
                        user: None,
                        created_at: None,
                        updated_at: None,
                    },
                    GithubComment {
                        id: 2,
                        body: "real action".to_string(),
                        user: None,
                        created_at: None,
                        updated_at: None,
                    },
                    GithubComment {
                        id: 3,
                        body: format!("{}\nreceipt", event_marker("old")),
                        user: None,
                        created_at: None,
                        updated_at: None,
                    },
                ],
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let mut project_state = RelayProjectState::default();
        let remote = api.issues.clone();
        let (queued, errors) = ingest_comments(
            &mut api,
            "fake/repo",
            "cadence",
            &config,
            &mut project_state,
            &remote,
        );
        assert_eq!(queued, 1);
        assert!(errors.is_empty());
        assert_eq!(project_state.actions.len(), 1);
        assert_eq!(project_state.seen_events.len(), 3);
        let _ = state_dir;
    }

    #[test]
    fn claim_expiry_requeues_without_two_owners() {
        let mut state = RelayProjectState::default();
        state.actions.insert(
            "comment:x".to_string(),
            PendingAction {
                event_id: "comment:x".to_string(),
                report_id: "CAD-1".to_string(),
                issue_number: 1,
                summary: "action".to_string(),
                state: "claimed".to_string(),
                attempts: 0,
                reason: None,
                claim_owner: Some("dead-process".to_string()),
                claim_until: Some(1),
                updated_at: iso_now(),
            },
        );
        recover_claims(&mut state);
        assert_eq!(state.actions["comment:x"].state, "queued");
        assert!(state.actions["comment:x"].claim_owner.is_none());
    }

    #[test]
    fn unsupported_auth_leaves_report_retrying() {
        let pm_temp = tempfile::tempdir().unwrap();
        let pm = intake_pm(&pm_temp);
        let config = RelayProjectConfig {
            enabled: true,
            repo: "fake/repo".to_string(),
            poll_seconds: 300,
            dispatch: false,
            pm_alias: None,
            actor: None,
        };
        let mut api = MockApi {
            fail_reads: true,
            ..Default::default()
        };
        let mut state = RelayProjectState::default();
        let (_, _, errors) =
            publish_local_reports(&pm.dir, "cadence", &config, &mut state, &mut api);
        assert!(errors
            .iter()
            .any(|error| error.contains("auth unavailable")));
        let receipt = &state.reports["CAD-1"];
        assert_eq!(receipt.state, "retrying");
        assert!(receipt
            .last_error
            .as_deref()
            .unwrap()
            .contains("auth unavailable"));
    }

    #[test]
    fn config_is_disabled_until_explicit_enable() {
        let (_temp, state_dir) = temp_state();
        let out = configure(
            &state_dir,
            "cadence",
            "favcrm/cadence",
            false,
            300,
            false,
            None,
            None,
        )
        .unwrap();
        assert_eq!(out["enabled"], false);
        assert!(!load_config(&state_dir).unwrap().projects["cadence"].enabled);
    }

    #[test]
    fn receipt_only_ack_is_not_actionable() {
        assert!(!actionable_comment("ack"));
        assert!(!actionable_comment("  /ack  "));
        assert!(!actionable_comment(
            "<!-- cadence-relay:event=x -->\nreceipt"
        ));
        assert!(actionable_comment("please investigate this regression"));
    }
}
