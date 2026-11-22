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

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::doctor::host::redact_argv;
use crate::error::{Error, Result};
use crate::issue::{board, model, time};
use crate::proc::run_bounded;

mod sync;

use sync::sync_project;
#[cfg(test)]
use sync::{
    actionable_comment, dispatch_pending, ingest_comments, publish_local_reports, quota_decision,
};

pub const CONFIG_FILE: &str = "intake-relay.yaml";
pub const STATE_FILE: &str = "intake-relay-state.json";
pub const DEFAULT_POLL_SECONDS: u64 = 300;
pub const RELAY_LABEL: &str = "cadence-report";
const STATE_SCHEMA: u32 = 1;
const CONFIG_SCHEMA: u32 = 1;
pub(super) const MAX_SUMMARY_CHARS: usize = 4_000;
const MAX_TITLE_CHARS: usize = 180;
const GH_TIMEOUT: Duration = Duration::from_secs(20);
pub(super) const CLAIM_SECONDS: i64 = 300;
/// GitHub `per_page` maximum. A full page means another page may exist.
const PAGE_SIZE: usize = 100;
/// Stop listing rather than silently treating a longer result as complete.
const MAX_LIST_PAGES: u32 = 10;
/// Newest event ids survive eviction. Older ids are dropped first.
const SEEN_EVENT_CAP: usize = 2_000;

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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelayState {
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default)]
    pub projects: BTreeMap<String, RelayProjectState>,
}

impl Default for RelayState {
    fn default() -> Self {
        Self {
            schema: STATE_SCHEMA,
            projects: BTreeMap::new(),
        }
    }
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
    /// Oldest first. A string entry is a pre-stamp id (array order is its
    /// age); a new event stores `{id, at}` so eviction drops the oldest
    /// rather than the lexicographically first id.
    #[serde(default, deserialize_with = "deserialize_seen_events")]
    pub seen_events: Vec<SeenEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SeenEvent {
    pub id: String,
    #[serde(default)]
    pub at: i64,
}

fn deserialize_seen_events<'de, D>(deserializer: D) -> std::result::Result<Vec<SeenEvent>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = Vec::<Value>::deserialize(deserializer)?;
    let mut out = Vec::with_capacity(values.len());
    for (index, value) in values.into_iter().enumerate() {
        if let Some(id) = value.as_str() {
            out.push(SeenEvent {
                id: id.to_string(),
                at: i64::try_from(index).unwrap_or(i64::MAX),
            });
            continue;
        }
        let event: SeenEvent = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        out.push(event);
    }
    Ok(out)
}

impl RelayProjectState {
    pub(super) fn has_seen(&self, id: &str) -> bool {
        self.seen_events.iter().any(|event| event.id == id)
    }

    /// Record `id` once. When the cap is exceeded, drop the oldest stamp
    /// (then the smallest id), never the newest event.
    pub(super) fn remember(&mut self, id: String) {
        if self.has_seen(&id) {
            return;
        }
        self.seen_events.push(SeenEvent { id, at: now() });
        if self.seen_events.len() <= SEEN_EVENT_CAP {
            return;
        }
        self.seen_events
            .sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
        let overflow = self.seen_events.len() - SEEN_EVENT_CAP;
        self.seen_events.drain(0..overflow);
    }
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
        since: Option<&str>,
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

fn issues_query(repo: &str, page: u32, since: Option<&str>) -> String {
    let mut path = format!(
        "{}?state=all&per_page={PAGE_SIZE}&page={page}",
        gh_resource(repo, "issues")
    );
    // Cursor values are GitHub `updated_at` timestamps. Anything else is
    // ignored so a corrupt cursor cannot change the request target.
    if let Some(since) = since.filter(|value| {
        !value.is_empty()
            && value.len() <= 40
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | ':' | '.' | '+'))
    }) {
        path.push_str("&since=");
        path.push_str(since);
    }
    path
}

impl GithubApi for GhApi {
    fn list_issues(
        &mut self,
        repo: &str,
        page: u32,
        since: Option<&str>,
    ) -> std::result::Result<Vec<GithubIssue>, String> {
        let path = issues_query(repo, page, since);
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
            "{}?per_page={PAGE_SIZE}&page={page}",
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

/// Exclusive `flock` held for the life of this value. The kernel drops it
/// when the process dies, so a crashed sync cannot leave a stale file that
/// blocks the next poll. The file itself may remain; ownership is the lock.
#[derive(Debug)]
pub struct RelayLock {
    _file: crate::issue::pmlock::Unlock,
}

pub fn acquire_lock(state_dir: &Path) -> Result<RelayLock> {
    fs::create_dir_all(state_dir)?;
    let path = state_dir.join("intake-relay.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err(Error::rejected(
            "intake relay is already running; wait for the existing sync or inspect its state",
        ));
    }
    Ok(RelayLock {
        _file: crate::issue::pmlock::Unlock(file),
    })
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

pub(super) fn now() -> i64 {
    time::now_epoch()
}

pub(super) fn iso_now() -> String {
    time::iso(now())
}

pub(super) fn validate_project_key(key: &str) -> Result<()> {
    Ok(model::check_key(key).map(|_| ())?)
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

pub(super) fn cap_chars(text: &str, max: usize) -> String {
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

pub(super) fn report_summary(issue: &board::Issue) -> (String, String) {
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

pub(super) fn marker(project_key: &str, report_id: &str) -> String {
    format!(
        "{}\n<!-- cadence-relay:report={report_id} -->",
        project_marker(project_key)
    )
}

pub(super) fn event_marker(event_id: &str) -> String {
    format!("<!-- cadence-relay:event={event_id} -->")
}

pub(super) fn report_id_from_body(body: &str) -> Option<String> {
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

pub(super) fn managed_issue_for_project(issue: &GithubIssue, project_key: &str) -> bool {
    managed_issue(issue)
        && issue.body.as_deref().is_some_and(|body| {
            project_key_from_body(body) == Some(project_key) && report_id_from_body(body).is_some()
        })
}

pub(super) fn issue_marker_map(
    issues: &[GithubIssue],
    project_key: &str,
) -> BTreeMap<String, GithubIssue> {
    issues
        .iter()
        .filter(|issue| managed_issue_for_project(issue, project_key))
        .filter_map(|issue| {
            let body = issue.body.as_deref()?;
            report_id_from_body(body).map(|id| (id, issue.clone()))
        })
        .collect()
}

fn list_paged<T>(
    mut fetch: impl FnMut(u32) -> std::result::Result<Vec<T>, String>,
    what: &str,
) -> std::result::Result<Vec<T>, String> {
    let mut all = Vec::new();
    for page in 1..=MAX_LIST_PAGES {
        let rows = fetch(page)?;
        let full = rows.len() >= PAGE_SIZE;
        all.extend(rows);
        if !full {
            return Ok(all);
        }
        if page == MAX_LIST_PAGES {
            return Err(format!(
                "GitHub {what} list truncated at {} rows; refusing a partial read",
                MAX_LIST_PAGES as usize * PAGE_SIZE
            ));
        }
    }
    Err(format!("GitHub {what} list truncated"))
}

pub(super) fn list_all_issues<T: GithubApi>(
    api: &mut T,
    repo: &str,
    since: Option<&str>,
) -> std::result::Result<Vec<GithubIssue>, String> {
    list_paged(|page| api.list_issues(repo, page, since), "issue")
}

pub(super) fn list_all_comments<T: GithubApi>(
    api: &mut T,
    repo: &str,
    issue: u64,
) -> std::result::Result<Vec<GithubComment>, String> {
    list_paged(|page| api.list_comments(repo, issue, page), "comment")
}

pub(super) fn receipt<'a>(state: &'a mut RelayProjectState, id: &str) -> &'a mut ReportReceipt {
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

pub(super) fn mark_published(
    receipt: &mut ReportReceipt,
    issue: &GithubIssue,
    source_rev: Option<String>,
) {
    receipt.state = "published".to_string();
    receipt.github_number = Some(issue.number);
    receipt.github_url = (!issue.html_url.is_empty()).then(|| issue.html_url.clone());
    receipt.source_rev = source_rev;
    receipt.reason = None;
    receipt.last_error = None;
    receipt.next_retry_at = None;
    receipt.updated_at = iso_now();
}

pub(super) fn mark_retry(receipt: &mut ReportReceipt, error: &str) {
    receipt.attempts = receipt.attempts.saturating_add(1);
    receipt.state = "retrying".to_string();
    receipt.last_error = Some(safe_text(error, 600));
    receipt.reason = Some("publication is pending; no blind duplicate was created".to_string());
    let backoff = 30_i64.saturating_mul(1_i64 << receipt.attempts.min(7));
    receipt.next_retry_at = Some(now().saturating_add(backoff.min(3600)));
    receipt.updated_at = iso_now();
}

pub(super) fn retry_due(receipt: &ReportReceipt) -> bool {
    receipt.next_retry_at.map(|at| at <= now()).unwrap_or(true)
}

pub(super) fn recover_claims(state: &mut RelayProjectState) {
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

/// One Needs-you row for relay work the consumer could not finish:
/// a publication that is retrying or blocked, or a comment still queued
/// because quota, a stopped PM, or an explicit dispatch gate held it.
/// A missing or unreadable state file yields no rows — overview must not
/// fail closed on the relay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayAttention {
    pub project: String,
    pub subject_id: String,
    pub title: String,
    pub command: String,
    pub age_secs: i64,
    pub owner: Option<String>,
}

fn age_since(updated_at: &str, now_epoch: i64) -> i64 {
    time::parse_iso(updated_at)
        .map(|at| (now_epoch - at).max(0))
        .unwrap_or(0)
}

pub fn attention(state_dir: &Path, now_epoch: i64) -> Vec<RelayAttention> {
    let config = load_config(state_dir).unwrap_or_default();
    let Ok(state) = load_state(state_dir) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for (project, project_state) in &state.projects {
        let owner = config
            .projects
            .get(project)
            .and_then(|item| item.pm_alias.clone())
            .filter(|alias| !alias.is_empty());
        let command = format!("cadence intake status {project}");
        for receipt in project_state.reports.values() {
            if receipt.state != "retrying" && receipt.state != "blocked" {
                continue;
            }
            let reason = receipt
                .reason
                .as_deref()
                .or(receipt.last_error.as_deref())
                .unwrap_or("publication needs attention");
            rows.push(RelayAttention {
                project: project.clone(),
                subject_id: receipt.report_id.clone(),
                title: format!(
                    "{} relay {} — {}",
                    receipt.report_id,
                    receipt.state,
                    safe_text(reason, 180)
                ),
                command: command.clone(),
                age_secs: age_since(&receipt.updated_at, now_epoch),
                owner: owner.clone(),
            });
        }
        for action in project_state.actions.values() {
            if action.state != "queued" && action.state != "retrying" {
                continue;
            }
            let reason = action
                .reason
                .as_deref()
                .unwrap_or("unclaimed; waiting for a quota-aware dispatch");
            rows.push(RelayAttention {
                project: project.clone(),
                subject_id: action.event_id.clone(),
                title: format!(
                    "{} comment {} — {}",
                    action.report_id,
                    action.state,
                    safe_text(reason, 180)
                ),
                command: command.clone(),
                age_secs: age_since(&action.updated_at, now_epoch),
                owner: owner.clone(),
            });
        }
    }
    rows.sort_by(|a, b| {
        b.age_secs
            .cmp(&a.age_secs)
            .then(a.subject_id.cmp(&b.subject_id))
    });
    let cap = crate::issue::report::NEEDS_ME_CAP;
    if let Some(extra) = rows.len().checked_sub(cap).filter(|n| *n > 0) {
        rows.truncate(cap);
        rows.push(RelayAttention {
            project: String::new(),
            subject_id: "intake-relay-overflow".to_string(),
            title: format!("… {extra} more intake relay items"),
            command: "cadence intake status".to_string(),
            age_secs: 0,
            owner: None,
        });
    }
    rows
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
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[derive(Default)]
    struct MockApi {
        issues: Vec<GithubIssue>,
        comments: BTreeMap<u64, Vec<GithubComment>>,
        create_calls: usize,
        timeout_after_create: bool,
        fail_reads: bool,
        /// Every page is a full page, including the last allowed page.
        force_full_pages: bool,
        fail_comments: bool,
        saw_since: Vec<Option<String>>,
    }

    impl GithubApi for MockApi {
        fn list_issues(
            &mut self,
            _repo: &str,
            page: u32,
            since: Option<&str>,
        ) -> std::result::Result<Vec<GithubIssue>, String> {
            self.saw_since.push(since.map(str::to_string));
            if self.fail_reads {
                return Err("auth unavailable".to_string());
            }
            if self.force_full_pages {
                return Ok((0..PAGE_SIZE)
                    .map(|index| GithubIssue {
                        number: u64::from(page) * 1_000 + index as u64,
                        html_url: String::new(),
                        title: "unrelated".to_string(),
                        body: None,
                        updated_at: Some("2026-09-20T00:00:00Z".to_string()),
                        labels: vec![],
                        pull_request: None,
                    })
                    .collect());
            }
            if page > 1 {
                return Ok(Vec::new());
            }
            let mut issues = self.issues.clone();
            if let Some(since) = since {
                issues.retain(|issue| {
                    issue
                        .updated_at
                        .as_deref()
                        .is_some_and(|updated| updated >= since)
                });
            }
            Ok(issues)
        }
        fn list_comments(
            &mut self,
            _repo: &str,
            issue: u64,
            _page: u32,
        ) -> std::result::Result<Vec<GithubComment>, String> {
            if self.fail_reads || self.fail_comments {
                Err("comments unavailable".to_string())
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
            None,
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
            None,
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
                paths: vec![],
                kind: Some("feedback".to_string()),
                plan: None,
                plan_epic: None,
                item_type: None,
                stage: None,
                stage_at: None,
                milestone: None,
                size: None,
                last_groomed_at: None,
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

    /// CAD-109: a credential shape the summary scrubber lets through —
    /// a GitLab deploy token, whose `gldt-` prefix the scrubber's shared
    /// list does not know — is refused by the secret scan. Nothing
    /// reaches GitHub, the receipt names the rule and not the value, and
    /// an edit that removes the value publishes.
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
        let token = ["gl", "dt-", &digest[..20]].concat();
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
        assert!(reason.contains("rule gitlab-deploy-token"), "{reason}");
        assert!(!reason.contains(&token[5..]), "{reason}");
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

    #[test]
    fn stale_lock_file_does_not_block_and_a_live_holder_does() {
        let (_temp, state_dir) = temp_state();
        fs::write(state_dir.join("intake-relay.lock"), b"dead").unwrap();
        let held = acquire_lock(&state_dir).expect("a leftover lock file is not a holder");
        let blocked = acquire_lock(&state_dir).expect_err("the live holder excludes a second sync");
        assert!(blocked.to_string().contains("already running"));
        drop(held);
        acquire_lock(&state_dir).expect("dropping the holder releases the lock");
    }

    #[test]
    fn detached_holder_blocks_until_it_exits() {
        let (_temp, state_dir) = temp_state();
        let script = state_dir.join("hold.py");
        let lock = state_dir.join("intake-relay.lock");
        fs::write(
            &script,
            format!(
                "import fcntl, os, time, sys\nos.setsid()\nf = open({lock:?}, 'a+')\nfcntl.flock(f.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)\nsys.stdout.write('held\\n')\nsys.stdout.flush()\ntime.sleep(60)\n"
            ),
        )
        .unwrap();
        let child = Command::new("python3")
            .arg(&script)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        struct Kill(Option<std::process::Child>);
        impl Drop for Kill {
            fn drop(&mut self) {
                if let Some(child) = self.0.as_mut() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        let mut child = Kill(Some(child));
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(child.0.as_mut().unwrap().stdout.take().unwrap()),
            &mut line,
        )
        .unwrap();
        assert_eq!(line.trim(), "held");
        let blocked = acquire_lock(&state_dir).expect_err("a live detached holder blocks");
        assert!(blocked.to_string().contains("already running"));
        let held = child.0.take().unwrap();
        drop(Kill(Some(held)));
        acquire_lock(&state_dir).expect("a dead holder must not wedge the consumer");
        assert!(lock.is_file());
    }

    #[test]
    fn truncated_issue_list_does_not_publish() {
        let pm_temp = tempfile::tempdir().unwrap();
        let pm = intake_pm(&pm_temp);
        let config = relay_config();
        let mut api = MockApi {
            force_full_pages: true,
            ..Default::default()
        };
        let mut state = RelayProjectState::default();
        let (published, _, errors) =
            publish_local_reports(&pm.dir, "cadence", &config, &mut state, &mut api);
        assert_eq!(published, 0);
        assert_eq!(api.create_calls, 0);
        assert!(errors.iter().any(|error| error.contains("truncated")));
        assert_eq!(state.reports["CAD-1"].state, "retrying");
        assert!(state.reports["CAD-1"]
            .reason
            .as_deref()
            .unwrap()
            .contains("not attempted"));
    }

    #[test]
    fn comment_failure_does_not_advance_the_cursor() {
        let mut api = MockApi {
            fail_comments: true,
            issues: vec![managed_remote("CAD-9", "2026-09-20T01:02:03Z")],
            ..Default::default()
        };
        let mut project_state = RelayProjectState::default();
        let remote = api.issues.clone();
        let (queued, errors) = ingest_comments(
            &mut api,
            "fake/repo",
            "cadence",
            &relay_config(),
            &mut project_state,
            &remote,
        );
        assert_eq!(queued, 0);
        assert!(errors
            .iter()
            .any(|error| error.contains("comments unavailable")));
        assert!(project_state.cursor.is_none());
        assert!(project_state.actions.is_empty());
    }

    #[test]
    fn comment_poll_passes_the_cursor_as_since() {
        let pm_temp = tempfile::tempdir().unwrap();
        let pm = crate::issue::Pm::init(pm_temp.path()).unwrap();
        let (_temp, state_dir) = temp_state();
        let mut api = MockApi {
            issues: vec![managed_remote("CAD-9", "2026-09-20T01:02:03Z")],
            comments: [(
                4,
                vec![GithubComment {
                    id: 8,
                    body: "please look at this".to_string(),
                    user: None,
                    created_at: None,
                    updated_at: None,
                }],
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let mut state = RelayState::default();
        sync_project(
            &pm.dir,
            &state_dir,
            "cadence",
            &relay_config(),
            &mut state,
            &mut api,
            false,
        )
        .unwrap();
        assert_eq!(
            state.projects["cadence"].cursor.as_deref(),
            Some("2026-09-20T01:02:03Z")
        );
        api.saw_since.clear();
        sync_project(
            &pm.dir,
            &state_dir,
            "cadence",
            &relay_config(),
            &mut state,
            &mut api,
            false,
        )
        .unwrap();
        assert!(
            api.saw_since
                .iter()
                .any(|since| since.as_deref() == Some("2026-09-20T01:02:03Z")),
            "second poll since values: {:?}",
            api.saw_since
        );
    }

    #[test]
    fn eviction_drops_the_oldest_event_not_the_lexicographic_first() {
        let mut state = RelayProjectState::default();
        for index in 0..SEEN_EVENT_CAP {
            state.seen_events.push(SeenEvent {
                id: format!("comment:z:{index:04}"),
                at: i64::try_from(index).unwrap(),
            });
        }
        state.remember("comment:a:newest".to_string());
        assert!(state.has_seen("comment:a:newest"));
        assert!(!state.has_seen("comment:z:0000"));
        assert_eq!(state.seen_events.len(), SEEN_EVENT_CAP);
        let stamped = state
            .seen_events
            .iter()
            .find(|event| event.id == "comment:a:newest")
            .unwrap()
            .at;
        assert!(stamped > 1_000_000, "newest stamp was {stamped}");
        let (_temp, state_dir) = temp_state();
        let mut saved = RelayState::default();
        saved.projects.insert("cadence".to_string(), state);
        save_state(&state_dir, &saved).unwrap();
        let loaded = load_state(&state_dir).unwrap();
        let mut project = loaded.projects["cadence"].clone();
        assert_eq!(
            project
                .seen_events
                .iter()
                .find(|event| event.id == "comment:a:newest")
                .map(|event| event.at),
            Some(stamped),
            "a reload must not zero the stamp"
        );
        project.remember("comment:z:extra".to_string());
        assert!(project.has_seen("comment:a:newest"));
        assert!(!project.has_seen("comment:z:0001"));
    }

    #[test]
    fn restart_keeps_a_seen_comment_deduped() {
        let (_temp, state_dir) = temp_state();
        let mut api = MockApi {
            issues: vec![managed_remote("CAD-9", "2026-09-20T01:02:03Z")],
            comments: [(
                4,
                vec![GithubComment {
                    id: 8,
                    body: "please look at this".to_string(),
                    user: None,
                    created_at: None,
                    updated_at: None,
                }],
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let remote = api.issues.clone();
        let mut project = RelayProjectState::default();
        let (queued, errors) = ingest_comments(
            &mut api,
            "fake/repo",
            "cadence",
            &relay_config(),
            &mut project,
            &remote,
        );
        assert_eq!(queued, 1);
        assert!(errors.is_empty());
        let mut saved = RelayState::default();
        saved.projects.insert("cadence".to_string(), project);
        save_state(&state_dir, &saved).unwrap();
        let loaded = load_state(&state_dir).unwrap();
        let mut project = loaded.projects["cadence"].clone();
        let (queued, errors) = ingest_comments(
            &mut api,
            "fake/repo",
            "cadence",
            &relay_config(),
            &mut project,
            &remote,
        );
        assert_eq!(queued, 0, "a reloaded comment id must not queue again");
        assert!(errors.is_empty());
        assert_eq!(project.actions.len(), 1);
        assert!(project.has_seen("comment:fake/repo:4:8"));
    }

    #[test]
    fn legacy_seen_event_strings_stay_seen_after_load() {
        let (_temp, state_dir) = temp_state();
        fs::write(
            state_dir.join(STATE_FILE),
            r#"{"schema":1,"projects":{"cadence":{"seen_events":["comment:fake/repo:4:8","comment:fake/repo:4:9"]}}}"#,
        )
        .unwrap();
        let loaded = load_state(&state_dir).unwrap();
        let mut project = loaded.projects["cadence"].clone();
        assert!(project.has_seen("comment:fake/repo:4:8"));
        assert!(project.has_seen("comment:fake/repo:4:9"));
        let mut api = MockApi {
            issues: vec![managed_remote("CAD-9", "2026-09-20T01:02:03Z")],
            comments: [(
                4,
                vec![GithubComment {
                    id: 8,
                    body: "please look at this".to_string(),
                    user: None,
                    created_at: None,
                    updated_at: None,
                }],
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let remote = api.issues.clone();
        let (queued, errors) = ingest_comments(
            &mut api,
            "fake/repo",
            "cadence",
            &relay_config(),
            &mut project,
            &remote,
        );
        assert_eq!(queued, 0);
        assert!(errors.is_empty());
        assert!(project.actions.is_empty());
    }

    fn canonical_pm(used_percent: u64, observed_at: &str) -> Value {
        json!({
            "alias": "cc13-pm",
            "provider": "codex",
            "state": "idle",
            "thread_id": "thread",
            "quota": {
                "provider": "codex",
                "assignee": "cc13-pm",
                "account_id": "acct",
                "thread_id": "thread",
                "state": "available",
                "source": "account/rateLimits/read",
                "observed_at": observed_at,
                "data": {
                    "accountId": "acct",
                    "rateLimits": {"primary": {"usedPercent": used_percent}}
                }
            }
        })
    }

    #[test]
    fn canonical_quota_allows_only_fresh_headroom() {
        let fresh = time::iso(now());
        assert!(quota_decision(&canonical_pm(10, &fresh)).is_ok());
        let exhausted = quota_decision(&canonical_pm(100, &fresh)).unwrap_err();
        assert!(exhausted.contains("exhausted"), "{exhausted}");
        let stale = quota_decision(&canonical_pm(10, "2020-01-01T00:00:00Z")).unwrap_err();
        assert!(stale.contains("stale"), "{stale}");
        let mut unbound = canonical_pm(10, &fresh);
        unbound["quota"]["assignee"] = json!("someone-else");
        let unbound = quota_decision(&unbound).unwrap_err();
        assert!(unbound.contains("not bound"), "{unbound}");
        let mut bare = canonical_pm(100, &fresh);
        bare["quota"] = json!({"state": "available", "remaining": 0, "used_percent": 100});
        let bare = quota_decision(&bare).unwrap_err();
        assert!(bare.contains("quota unknown"), "{bare}");
        assert!(quota_decision(&json!({"state": "stopped"}))
            .unwrap_err()
            .contains("stopped"));
        assert!(quota_decision(&json!({"state": "idle"}))
            .unwrap_err()
            .contains("quota unknown"));
    }

    #[test]
    fn unknown_or_exhausted_quota_does_not_dispatch() {
        assert!(quota_decision(&json!({"state": "idle"}))
            .unwrap_err()
            .contains("quota unknown"));

        let (_temp, state_dir) = temp_state();
        let config = RelayProjectConfig {
            enabled: true,
            repo: "fake/repo".to_string(),
            poll_seconds: 300,
            dispatch: true,
            pm_alias: Some("cc13-pm".to_string()),
            actor: None,
        };
        let mut relay_state = RelayState::default();
        relay_state
            .projects
            .entry("cadence".to_string())
            .or_default()
            .actions
            .insert(
                "comment:fake:4:8".to_string(),
                PendingAction {
                    event_id: "comment:fake:4:8".to_string(),
                    report_id: "CAD-9".to_string(),
                    issue_number: 4,
                    summary: "please look".to_string(),
                    state: "queued".to_string(),
                    attempts: 0,
                    reason: None,
                    claim_owner: None,
                    claim_until: None,
                    updated_at: "2026-09-20T00:00:00Z".to_string(),
                },
            );
        let sent = dispatch_pending(
            &state_dir,
            Path::new("/tmp"),
            "cadence",
            &config,
            &mut relay_state,
            true,
        );
        assert_eq!(sent, 0);
        let action = &relay_state.projects["cadence"].actions["comment:fake:4:8"];
        assert_eq!(action.state, "queued");
        assert!(action.reason.as_deref().unwrap().contains("quota unknown"));
        assert!(action.claim_owner.is_none());
    }

    #[test]
    fn attention_surfaces_quota_blocked_and_retrying_work() {
        let (_temp, state_dir) = temp_state();
        let mut config = RelayConfig::default();
        config.projects.insert(
            "cadence".to_string(),
            RelayProjectConfig {
                enabled: true,
                repo: "fake/repo".to_string(),
                poll_seconds: 300,
                dispatch: true,
                pm_alias: Some("cc13-pm".to_string()),
                actor: None,
            },
        );
        save_config(&state_dir, &config).unwrap();
        let mut state = RelayState::default();
        let project = state.projects.entry("cadence".to_string()).or_default();
        project.reports.insert(
            "CAD-9".to_string(),
            ReportReceipt {
                report_id: "CAD-9".to_string(),
                state: "retrying".to_string(),
                attempts: 1,
                github_number: None,
                github_url: None,
                source_rev: None,
                reason: Some("publication is pending; no blind duplicate was created".to_string()),
                last_error: Some("timed out".to_string()),
                next_retry_at: None,
                updated_at: "2026-09-20T00:00:00Z".to_string(),
            },
        );
        project.actions.insert(
            "comment:fake:4:8".to_string(),
            PendingAction {
                event_id: "comment:fake:4:8".to_string(),
                report_id: "CAD-9".to_string(),
                issue_number: 4,
                summary: "please look".to_string(),
                state: "queued".to_string(),
                attempts: 0,
                reason: Some("quota exhausted".to_string()),
                claim_owner: None,
                claim_until: None,
                updated_at: "2026-09-20T00:00:00Z".to_string(),
            },
        );
        project.actions.insert(
            "comment:fake:4:9".to_string(),
            PendingAction {
                event_id: "comment:fake:4:9".to_string(),
                report_id: "CAD-9".to_string(),
                issue_number: 4,
                summary: "done".to_string(),
                state: "dispatched".to_string(),
                attempts: 1,
                reason: None,
                claim_owner: None,
                claim_until: None,
                updated_at: "2026-09-20T00:00:00Z".to_string(),
            },
        );
        save_state(&state_dir, &state).unwrap();
        let rows = attention(&state_dir, 1_800_000_000);
        assert!(rows.iter().any(|row| {
            row.subject_id == "CAD-9"
                && row.title.contains("retrying")
                && row.owner.as_deref() == Some("cc13-pm")
        }));
        assert!(rows.iter().any(|row| {
            row.subject_id == "comment:fake:4:8" && row.title.contains("quota exhausted")
        }));
        assert!(rows.iter().all(|row| row.subject_id != "comment:fake:4:9"));
    }

    fn managed_remote(report_id: &str, updated_at: &str) -> GithubIssue {
        GithubIssue {
            number: 4,
            html_url: String::new(),
            title: "report".to_string(),
            body: Some(marker("cadence", report_id)),
            updated_at: Some(updated_at.to_string()),
            labels: vec![GithubLabel {
                name: RELAY_LABEL.to_string(),
            }],
            pull_request: None,
        }
    }
}
