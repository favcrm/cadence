//! Hosted coordination for one member's local agent (CAD-1460).
//!
//! The board persists the assignment at
//! `/__platform/hosted-cadence/:organizationId/local-assignments`.
//! Version `hosted-cadence-local-assignment.v1`. This machine claims a
//! row only for its own directory user, then hands the issue to the
//! configured local actor. A worktree is the checkout, not execution.
//! When that actor finishes the kickoff message, the originating claim
//! is reported to the board. The daemon multiplexes explicitly connected
//! agents over one authenticated outbound workspace event stream.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::remote_cli::{self, CliMaterial, RemoteTarget};
use crate::worktree::{self, layout};

pub mod delivery;
pub mod events;
pub mod http;
pub mod notifications;
pub mod reporting;
mod wire;

pub const VERSION: &str = "hosted-cadence-local-assignment.v1";
const MAX_BODY: u64 = 1024 * 1024;

pub struct Reply {
    status: u16,
    body: Value,
}

pub trait CoordHttp {
    fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply>;
}

/// What the local daemon actor must receive after a claim. The path is
/// this machine's checkout. It is not sent to the board.
pub struct Kickoff<'a> {
    pub alias: &'a str,
    pub issue_id: &'a str,
    pub assignment_id: &'a str,
    pub worktree: &'a str,
    pub message_id: &'a str,
    /// Claim that owned the kickoff. A later reclaim must not replace it.
    pub claim_id: &'a str,
    pub generation: u64,
    pub organization_id: &'a str,
    pub audience: &'a str,
    /// Directory member and issuer captured when this coordinator opened.
    /// Both are required together. A missing half is not inferred.
    pub subject_id: Option<&'a str>,
    pub issuer: Option<&'a str>,
}

#[derive(Clone)]
struct Assignment {
    assignment_id: String,
    organization_id: String,
    issue_id: String,
    action: String,
    execution: String,
    state: String,
    generation: u64,
    claim_id: Option<String>,
    target: Option<String>,
    created_by_subject_id: String,
    client_agent_id: Option<String>,
    guidance: Option<Value>,
    reported_head_sha: Option<String>,
    text: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    operation_id: String,
    issue_id: String,
    action: String,
    target: Option<String>,
    assignment_id: Option<String>,
    generation: Option<u64>,
    claim_id: Option<String>,
    state: String,
    fenced: bool,
    reassign_operation_id: Option<String>,
    #[serde(default)]
    reassign_fingerprint: Option<String>,
    /// Generation saved with the pending reassign. A newer observation must
    /// not replace it; a lost response replays this value.
    #[serde(default)]
    reassign_expected_generation: Option<u64>,
    worktree: Option<String>,
    /// Lane chosen for this claim and generation. A later generation
    /// does not reuse it while an older actor is still unsettled.
    #[serde(default)]
    lane: Option<String>,
    #[serde(default)]
    lane_claim: Option<String>,
    #[serde(default)]
    lane_generation: Option<u64>,
    #[serde(default)]
    kickoff_message: Option<String>,
    /// Reserved before `agent_send`. Retries reuse it so the daemon dedupes.
    #[serde(default)]
    kickoff_reserved: Option<String>,
    /// Claim pinned for the current kickoff. Retired with that kickoff when
    /// the observed claim moves. The origins map is not.
    #[serde(default)]
    origin_claim_id: Option<String>,
    #[serde(default)]
    origin_generation: Option<u64>,
    /// CAD-1964: the hosted profile this session claimed under. There is no
    /// local actor and therefore no kickoff; the claim itself is the only
    /// authority a report may name.
    ///
    /// Omitted when unset: this struct refuses unknown fields, so writing a
    /// null here would make a shared ledger unreadable to an older client
    /// that has never heard of the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_profile: Option<String>,
}

/// The claim a kickoff message was allowed to report. Keyed by message id
/// so a later reclaim of the same assignment cannot upgrade it.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Origin {
    message_id: String,
    organization_id: String,
    audience: String,
    #[serde(default)]
    subject_id: Option<String>,
    #[serde(default)]
    issuer: Option<String>,
    assignment_id: String,
    claim_id: String,
    generation: u64,
    /// Checkout name for this kickoff. A new claim must not reuse it
    /// until this origin is terminal.
    #[serde(default)]
    lane: Option<String>,
    #[serde(default)]
    settled: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    version: u32,
    client_agent_id: String,
    repo: Option<String>,
    #[serde(default)]
    alias: Option<String>,
    attempts: BTreeMap<String, Attempt>,
    /// Message id → originating claim. Never rewritten by `stamp`.
    #[serde(default)]
    origins: BTreeMap<String, Origin>,
}

pub struct Coordinator<H> {
    http: H,
    org_id: String,
    ledger: PathBuf,
    subject_id: Option<String>,
    issuer: Option<String>,
    /// Board audience recorded at login. Reports go here, not to whatever
    /// org a later process has selected.
    audience: String,
    /// Daemon binding lifetime. Cancellation is not an execution failure and
    /// must never turn a detached context read into a late blocked report.
    delivery_guard: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
    /// Test seam: runs after the delivery check and before reservation,
    /// while the ledger lock is not held.
    #[cfg(test)]
    before_reservation: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}

fn hosted_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn prefixed_id(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix) && hosted_id(value) && value.len() > prefix.len()
}

fn head_sha(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn operation_id() -> String {
    format!("op_{}", Uuid::new_v4().simple())
}

fn message_id() -> String {
    Uuid::new_v4().simple().to_string()
}

/// The client id a profile-scoped session claims under (CAD-1964). Derived
/// from the profile rather than random, so retrying the same start replays
/// the same claim instead of opening a second one.
fn session_client_agent_id(profile: &str) -> String {
    format!("session-{profile}")
}

fn valid_hct(token: &str) -> bool {
    let Some(rest) = token.strip_prefix("hct_") else {
        return false;
    };
    rest.len() == 43
        && rest
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn safe_code(body: &Value) -> String {
    match body.get("code").and_then(Value::as_str) {
        Some(code)
            if code.len() <= 64
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') =>
        {
            code.to_string()
        }
        _ => "unavailable".to_string(),
    }
}

fn forbidden_path_key(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, child)| {
            matches!(
                key.as_str(),
                "path"
                    | "repo"
                    | "cwd"
                    | "worktree"
                    | "filesystem"
                    | "bundle"
                    | "result_custody"
                    | "presence"
                    | "attempt"
                    | "target_user_id"
            ) || forbidden_path_key(child)
        }),
        Value::Array(items) => items.iter().any(forbidden_path_key),
        _ => false,
    }
}

fn attempt_key(issue: &str, action: &str, target: Option<&str>) -> String {
    format!("{issue}\u{1}{action}\u{1}{}", target.unwrap_or("-"))
}

fn assignment_prefix(org: &str) -> String {
    format!("/__platform/hosted-cadence/{org}/local-assignments")
}

fn slug(name: Option<&str>) -> Result<String> {
    let slug = name.unwrap_or("work").to_ascii_lowercase();
    if slug.is_empty()
        || slug.len() > 32
        || slug.starts_with('-')
        || !slug
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(Error::rejected(
            "worktree name must be 1-32 lowercase letters, digits or hyphens",
        ));
    }
    Ok(slug)
}

/// One checkout per assignment generation. The human slug is kept when
/// it fits; the full assignment identity and generation keep distinct
/// assignments off each other's branches within the 64-character lane limit.
fn bounded_lane(
    issue: &str,
    name: Option<&str>,
    assignment: &str,
    generation: u64,
) -> Result<String> {
    if !hosted_id(issue) || !prefixed_id(assignment, "hla_") || generation < 1 {
        return Err(Error::rejected(
            "lane identity needs a hosted issue, assignment, and generation",
        ));
    }
    let human = slug(name)?;
    let suffix = format!("-{}-g{generation}", &assignment["hla_".len()..]);
    if suffix.len() >= 62 {
        return Err(Error::rejected(
            "assignment generation does not fit in a worktree name",
        ));
    }
    let room = 64 - suffix.len();
    let stem_raw: String = issue
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    let slug_len = human.len().min(16).min(room.saturating_sub(2));
    let slug_part: String = human.chars().take(slug_len).collect();
    let stem_len = room - slug_part.len() - 1;
    let stem: String = stem_raw.chars().take(stem_len).collect();
    if stem.is_empty() || slug_part.is_empty() || stem.starts_with('-') {
        return Err(Error::rejected(
            "worktree name must keep the issue and the requested label",
        ));
    }
    let lane = format!("{stem}-{slug_part}{suffix}");
    crate::proto::identifier(&lane, "Worktree name")?;
    Ok(lane)
}

fn alias_name(alias: &str) -> Result<String> {
    crate::proto::identifier(alias, "local agent alias")
}

impl Assignment {
    fn to_json(&self) -> Value {
        json!({
            "version": VERSION,
            "assignment_id": self.assignment_id,
            "organization_id": self.organization_id,
            "issue_id": self.issue_id,
            "action": self.action,
            "execution": self.execution,
            "state": self.state,
            "generation": self.generation,
            "claim_id": self.claim_id,
            "target": self.target,
            "created_by_subject_id": self.created_by_subject_id,
            "client_agent_id": self.client_agent_id,
            "guidance": self.guidance,
            "reported_head_sha": self.reported_head_sha,
            "text": self.text,
        })
    }
}

fn optional_id(value: &Value, key: &str) -> Result<Option<String>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(id)) if hosted_id(id) => Ok(Some(id.clone())),
        _ => Err(Error::rejected("assignment id field is invalid")),
    }
}

fn optional_text(value: &Value, key: &str) -> Result<Option<String>> {
    match value.get(key) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.len() <= 32 * 1024 && !text.contains('\0') => {
            Ok(Some(text.clone()))
        }
        _ => Err(Error::rejected("assignment text is invalid")),
    }
}

fn parse_guidance(value: &Value) -> Result<Option<Value>> {
    match value.get("guidance") {
        Some(Value::Null) => Ok(None),
        Some(object @ Value::Object(_)) if !forbidden_path_key(object) => Ok(Some(object.clone())),
        _ => Err(Error::rejected("assignment guidance is not the contract")),
    }
}

fn parse_references(value: &Value, issue: &str) -> Result<()> {
    match value.get("references") {
        None => Ok(()),
        Some(object) => {
            let fields = object.as_object().ok_or_else(|| {
                Error::rejected("claim references are not the assignment contract")
            })?;
            if fields.len() != 1 || fields.get("issue_id").and_then(Value::as_str) != Some(issue) {
                return Err(Error::rejected(
                    "claim references must be the issue id only; nothing local was started",
                ));
            }
            Ok(())
        }
    }
}

fn parse_assignment(value: &Value) -> Result<Assignment> {
    if forbidden_path_key(value) {
        return Err(Error::rejected(
            "coordination response is not the assignment contract; nothing local was started",
        ));
    }
    let required = [
        "version",
        "assignment_id",
        "organization_id",
        "issue_id",
        "action",
        "execution",
        "state",
        "generation",
        "claim_id",
        "target",
        "created_by_subject_id",
        "client_agent_id",
        "guidance",
        "reported_head_sha",
        "text",
    ];
    let object = value
        .as_object()
        .filter(|object| {
            required.iter().all(|key| object.contains_key(*key))
                && object
                    .keys()
                    .all(|key| required.contains(&key.as_str()) || key == "references")
        })
        .ok_or_else(|| Error::rejected("coordination response is not the assignment contract"))?;
    if object.get("version").and_then(Value::as_str) != Some(VERSION) {
        return Err(Error::rejected(
            "coordination response is not the assignment contract",
        ));
    }
    let assignment_id = object["assignment_id"].as_str().unwrap_or("");
    let organization_id = object["organization_id"].as_str().unwrap_or("");
    let issue_id = object["issue_id"].as_str().unwrap_or("");
    let action = object["action"].as_str().unwrap_or("");
    let state = object["state"].as_str().unwrap_or("");
    let created_by = object["created_by_subject_id"].as_str().unwrap_or("");
    let Some(generation) = object["generation"]
        .as_u64()
        .filter(|generation| *generation >= 1)
    else {
        return Err(Error::rejected(
            "coordination response has an invalid assignment",
        ));
    };
    if !prefixed_id(assignment_id, "hla_")
        || !hosted_id(organization_id)
        || !hosted_id(issue_id)
        || !hosted_id(created_by)
        || !matches!(action, "dispatch" | "start")
        || !matches!(
            state,
            "waiting" | "queued" | "running" | "blocked" | "failed" | "result" | "cancelled"
        )
    {
        return Err(Error::rejected(
            "coordination response has an invalid assignment",
        ));
    }
    if object["execution"].as_str() != Some("local") {
        return Err(Error::invalid(
            "hosted_execution_unavailable",
            "the board returned hosted execution; this client will not run it and did not fall back",
        ));
    }
    parse_references(value, issue_id)?;
    let claim_id = match optional_id(value, "claim_id")? {
        Some(id) if prefixed_id(&id, "hlc_") => Some(id),
        Some(_) => return Err(Error::rejected("claim id is not the assignment contract")),
        None => None,
    };
    let client_agent_id = optional_id(value, "client_agent_id")?;
    let target = optional_id(value, "target")?;
    let live_target = matches!(state, "queued" | "running" | "blocked");
    if state == "waiting" && target.is_some() || live_target && target.is_none() {
        return Err(Error::rejected(
            "assignment target does not match its state",
        ));
    }
    if state == "running" && (claim_id.is_none() || client_agent_id.is_none()) {
        return Err(Error::rejected(
            "a running assignment has no claim; nothing local was started",
        ));
    }
    if state == "blocked" && claim_id.is_none() {
        return Err(Error::rejected(
            "a blocked assignment has no claim to resume",
        ));
    }
    let reported_head_sha = match value.get("reported_head_sha") {
        Some(Value::Null) => None,
        Some(Value::String(sha)) if head_sha(sha) => Some(sha.clone()),
        _ => {
            return Err(Error::rejected(
                "reported head is not 40 or 64 lowercase hex",
            ))
        }
    };
    Ok(Assignment {
        assignment_id: assignment_id.to_string(),
        organization_id: organization_id.to_string(),
        issue_id: issue_id.to_string(),
        action: action.to_string(),
        execution: "local".to_string(),
        state: state.to_string(),
        generation,
        claim_id,
        target,
        created_by_subject_id: created_by.to_string(),
        client_agent_id,
        guidance: parse_guidance(value)?,
        reported_head_sha,
        text: optional_text(value, "text")?,
    })
}

impl Ledger {
    fn load(dir: &Path, org: &str) -> Result<Self> {
        let path = dir.join(format!("local-assign-{org}.json"));
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    version: 1,
                    client_agent_id: format!("la_{}", Uuid::new_v4().simple()),
                    repo: None,
                    alias: None,
                    attempts: BTreeMap::new(),
                    origins: BTreeMap::new(),
                });
            }
            Err(error) => return Err(error.into()),
        };
        if !meta.is_file() || meta.file_type().is_symlink() || meta.mode() & 0o077 != 0 {
            return Err(Error::rejected(
                "local assignment ledger must be a private regular file",
            ));
        }
        let mut file = File::open(&path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        serde_json::from_slice(&bytes)
            .map_err(|_| Error::rejected("local assignment ledger is unreadable"))
    }

    fn save(&self, dir: &Path, org: &str) -> Result<()> {
        let path = dir.join(format!("local-assign-{org}.json"));
        let tmp = dir.join(format!(".local-assign-{org}.tmp"));
        let bytes =
            serde_json::to_vec_pretty(self).map_err(|error| Error::internal(error.to_string()))?;
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        fs::rename(tmp, path)?;
        File::open(dir)?.sync_all()?;
        Ok(())
    }
}

/// Drop mutable custody, delivery, lane and worktree for the retired claim.
/// Origins, binding files, old checkouts, and pending reassign operations stay.
fn retire_current_claim(attempt: &mut Attempt) {
    attempt.claim_id = None;
    // The session that marked this record no longer holds a claim here, so
    // its report authority retires with the claim it was given.
    attempt.session_profile = None;
    attempt.target = None;
    attempt.state = "discovered".to_string();
    attempt.kickoff_message = None;
    attempt.kickoff_reserved = None;
    attempt.origin_claim_id = None;
    attempt.origin_generation = None;
    attempt.worktree = None;
    attempt.lane = None;
    attempt.lane_claim = None;
    attempt.lane_generation = None;
}

fn attempt_ended(state: &str) -> bool {
    matches!(state, "result" | "failed" | "cancelled")
}

/// Ledger row still belongs to this claim generation and can message an actor.
/// A newer generation, a fenced row, or a cancelled claim is not authority.
fn claim_still_active(attempt: &Attempt, claimed: &Assignment) -> bool {
    attempt.assignment_id.as_deref() == Some(claimed.assignment_id.as_str())
        && attempt.claim_id.as_deref() == claimed.claim_id.as_deref()
        && attempt.generation == Some(claimed.generation)
        && attempt.state == claimed.state
        && !attempt.fenced
        && !attempt_ended(&attempt.state)
}

/// Observe the board without letting delayed rows retire newer custody.
/// An older generation, a same-generation null or different claim, or a
/// non-terminal row for a claim that already ended is ignored. Only a higher
/// generation retires the current delivery. Pending reassignment receipts
/// retain their original expected generation.
fn stamp(attempt: &mut Attempt, assignment: &Assignment) {
    if let Some(generation) = attempt.generation {
        let same_generation = assignment.generation == generation;
        let claim_regressed = same_generation
            && attempt.claim_id.is_some()
            && assignment.claim_id != attempt.claim_id;
        let reopened =
            same_generation && attempt_ended(&attempt.state) && !attempt_ended(&assignment.state);
        if assignment.generation < generation || claim_regressed || reopened {
            return;
        }
        if assignment.generation > generation {
            retire_current_claim(attempt);
        }
    }
    if attempt.claim_id != assignment.claim_id {
        // A record may only be reported through the claim this session took.
        // Once the observed claim moves, a later holder must never become
        // reportable through the retired marker.
        attempt.session_profile = None;
    }
    attempt.assignment_id = Some(assignment.assignment_id.clone());
    attempt.generation = Some(assignment.generation);
    attempt.claim_id = assignment.claim_id.clone();
    attempt.state = assignment.state.clone();
    attempt.target = assignment.target.clone();
    if assignment.state == "running" {
        attempt.fenced = false;
    }
}

impl<H: CoordHttp> Coordinator<H> {
    pub fn new(http: H, org_id: String, ledger: PathBuf) -> Result<Self> {
        Self::open(http, org_id, ledger, None, None, String::new())
    }

    fn open(
        http: H,
        org_id: String,
        ledger: PathBuf,
        subject_id: Option<String>,
        issuer: Option<String>,
        audience: String,
    ) -> Result<Self> {
        if !hosted_id(&org_id) {
            return Err(Error::rejected("workspace id is not a hosted id"));
        }
        if let Some(subject) = &subject_id {
            if !hosted_id(subject) {
                return Err(Error::rejected(
                    "session subject is not a directory user id",
                ));
            }
        }
        let _lock = remote_cli::private_lock(
            &ledger,
            "local-assign",
            Instant::now() + Duration::from_secs(5),
        )?;
        Ok(Self {
            http,
            org_id,
            ledger,
            subject_id,
            issuer,
            audience,
            delivery_guard: None,
            #[cfg(test)]
            before_reservation: None,
        })
    }

    #[cfg(test)]
    pub(super) fn pause_after_delivery_check(
        &mut self,
        wait: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) {
        self.before_reservation = Some(wait);
    }

    #[cfg(test)]
    fn set_subject(&mut self, subject: &str) {
        self.subject_id = Some(subject.to_string());
    }

    fn with_ledger<T>(&self, body: impl FnOnce(&mut Ledger) -> Result<T>) -> Result<T> {
        let _lock = remote_cli::private_lock(
            &self.ledger,
            "local-assign",
            Instant::now() + Duration::from_secs(5),
        )?;
        let mut ledger = Ledger::load(&self.ledger, &self.org_id)?;
        let out = body(&mut ledger)?;
        ledger.save(&self.ledger, &self.org_id)?;
        Ok(out)
    }

    fn base(&self) -> String {
        assignment_prefix(&self.org_id)
    }
    fn ensure_active(&self) -> Result<()> {
        if self.delivery_guard.as_ref().is_some_and(|active| !active()) {
            return Err(Error::busy_coded(
                "binding_cancelled",
                "the original local workspace binding is no longer active",
            ));
        }
        Ok(())
    }

    fn exchange(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        if method == "POST" {
            self.ensure_active()?;
        }
        let reply = self.http.send(method, path, body)?;
        if (300..400).contains(&reply.status) {
            return Err(Error::invalid(
                "redirect",
                "remote redirect refused; nothing was followed and nothing ran locally",
            ));
        }
        match reply.status {
            200 | 201 => Ok(reply.body),
            401 | 403 => Err(Error::rejected(
                "the hosted org refused this session — run `cadence login` again; nothing ran locally",
            )),
            404 => Err(Error::invalid(
                "api_missing",
                "hosted local-assignment routes are not on this board yet; nothing ran locally and no container verb was sent",
            )),
            409 => Err(explain_409(&reply.body)),
            _ => Err(Error::invalid(
                "unavailable",
                "hosted assignment coordination is unavailable; nothing ran locally",
            )),
        }
    }

    fn parse_owned(&self, value: &Value) -> Result<Assignment> {
        let assignment = parse_assignment(value)?;
        if assignment.organization_id != self.org_id {
            return Err(Error::invalid(
                "cross_workspace",
                "the board returned another workspace; nothing ran locally",
            ));
        }
        Ok(assignment)
    }

    fn client_agent_id(&self) -> Result<String> {
        self.with_ledger(|ledger| Ok(ledger.client_agent_id.clone()))
    }

    fn stored_alias(&self) -> Result<Option<String>> {
        self.with_ledger(|ledger| Ok(ledger.alias.clone()))
    }
    pub fn configured_alias(&self) -> Result<Option<String>> {
        self.stored_alias()
    }

    pub fn bind_alias(&self, alias: &str) -> Result<String> {
        let alias = alias_name(alias)?;
        self.with_ledger(|ledger| {
            ledger.alias = Some(alias.clone());
            Ok(alias)
        })
    }

    fn default_target(&self) -> Result<Option<String>> {
        if self.stored_alias()?.is_none() {
            return Ok(None);
        }
        self.subject_id.clone().map(Some).ok_or_else(|| {
            Error::invalid(
                "config_required",
                "this workspace login has no directory subject, so dispatch cannot select you; nothing was sent",
            )
        })
    }

    fn queue(&mut self, issue: &str, action: &str, target: Option<&str>) -> Result<Assignment> {
        let body = self.with_ledger(|ledger| {
            if !hosted_id(issue) {
                return Err(Error::rejected("issue id is not a hosted id"));
            }
            if let Some(target) = target {
                if !hosted_id(target) {
                    return Err(Error::rejected("--to must be a directory user id"));
                }
            }
            let key = attempt_key(issue, action, target);
            let finished = ledger.attempts.get(&key).is_some_and(|attempt| {
                matches!(attempt.state.as_str(), "result" | "failed" | "cancelled")
            });
            if finished {
                ledger.attempts.remove(&key);
            }
            let attempt = ledger.attempts.entry(key).or_insert_with(|| Attempt {
                operation_id: operation_id(),
                issue_id: issue.to_string(),
                action: action.to_string(),
                target: target.map(str::to_string),
                assignment_id: None,
                generation: None,
                claim_id: None,
                state: "preparing".to_string(),
                fenced: false,
                reassign_operation_id: None,
                reassign_fingerprint: None,
                reassign_expected_generation: None,
                worktree: None,
                lane: None,
                lane_claim: None,
                lane_generation: None,
                kickoff_message: None,
                kickoff_reserved: None,
                origin_claim_id: None,
                origin_generation: None,
                session_profile: None,
            });
            let mut body = json!({
                "version": VERSION,
                "organization_id": self.org_id,
                "idempotency_key": attempt.operation_id,
                "issue_id": issue,
                "action": action,
                "execution": "local",
                "identity_kind": "local_worker",
            });
            if let Some(target) = target {
                body["target"] = json!(target);
            }
            Ok(body)
        })?;
        let value = self.exchange("POST", &self.base(), Some(&body))?;
        let assignment = self.parse_owned(&value)?;
        let key = attempt_key(issue, action, target);
        self.with_ledger(|ledger| {
            if let Some(attempt) = ledger.attempts.get_mut(&key) {
                stamp(attempt, &assignment);
            }
            Ok(())
        })?;
        Ok(assignment)
    }

    fn list_state(&mut self, state: Option<&str>) -> Result<Vec<Assignment>> {
        let mut path = self.base();
        if let Some(state) = state {
            path.reserve(7 + state.len());
            path.push_str("?state=");
            path.push_str(state);
        }
        let value = self.exchange("GET", &path, None)?;
        if value.get("version").and_then(Value::as_str) != Some(VERSION) {
            return Err(Error::rejected(
                "assignment list is not the assignment contract",
            ));
        }
        let rows = value
            .get("assignments")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::rejected("assignment list is missing"))?;
        if value.as_object().is_none_or(|object| object.len() != 2) {
            return Err(Error::rejected(
                "assignment list is not the assignment contract",
            ));
        }
        rows.iter().map(|row| self.parse_owned(row)).collect()
    }
    /// Exhaust recovery before adopting its watermark. The ledger's original
    /// generation and claim fences also apply to every snapshot observation.
    fn snapshot(&mut self) -> Result<(u64, Vec<Assignment>)> {
        let mut watermark = None;
        let mut after: Option<String> = None;
        let mut rows = Vec::new();
        loop {
            let mut path = format!("{}/snapshot", self.base());
            if let (Some(after), Some(watermark)) = (&after, watermark) {
                path.push_str(&format!("?after_id={after}&watermark={watermark}"));
            }
            let value = self.exchange("GET", &path, None)?;
            let page_watermark = value["watermark"]
                .as_u64()
                .ok_or_else(|| Error::rejected("snapshot watermark is missing"))?;
            if value["version"].as_str() != Some(events::VERSION)
                || watermark.is_some_and(|prior| prior != page_watermark)
            {
                return Err(Error::rejected("snapshot contract or watermark changed"));
            }
            watermark = Some(page_watermark);
            let page = value["assignments"]
                .as_array()
                .filter(|page| page.len() <= 50)
                .ok_or_else(|| Error::rejected("snapshot page is invalid"))?;
            for item in page {
                let row = self.parse_owned(item)?;
                if after
                    .as_ref()
                    .is_some_and(|after| row.assignment_id <= *after)
                    || rows
                        .last()
                        .is_some_and(|prior: &Assignment| prior.assignment_id >= row.assignment_id)
                {
                    return Err(Error::rejected("snapshot did not advance"));
                }
                self.remember_discovered(&row)?;
                rows.push(row);
            }
            match &value["after_id"] {
                Value::Null => return Ok((page_watermark, rows)),
                Value::String(next)
                    if prefixed_id(next, "hla_")
                        && after.as_ref().is_none_or(|prior| next > prior)
                        && rows.last().is_some_and(|row| &row.assignment_id == next) =>
                {
                    after = Some(next.clone());
                }
                _ => return Err(Error::rejected("snapshot continuation is invalid")),
            }
        }
    }

    fn live_assignments(&mut self, resume_blocked: bool) -> Result<Vec<Assignment>> {
        let states = ["waiting", "queued", "running", "blocked"];
        let count = if resume_blocked { 4 } else { 3 };
        let mut rows = Vec::new();
        for &state in states.iter().take(count) {
            rows.extend(self.list_state(Some(state))?);
        }
        // A row can move between state queries. Keep the newest generation
        // and latest observation without copying assignment identities.
        let query_order = |state: &str| {
            states
                .iter()
                .position(|candidate| *candidate == state)
                .unwrap_or(0)
        };
        rows.sort_unstable_by(|left, right| {
            left.assignment_id
                .cmp(&right.assignment_id)
                .then_with(|| right.generation.cmp(&left.generation))
                .then_with(|| query_order(&right.state).cmp(&query_order(&left.state)))
        });
        rows.dedup_by(|next, prior| next.assignment_id == prior.assignment_id);
        Ok(rows)
    }

    fn claim(&mut self, assignment: &Assignment) -> Result<Assignment> {
        let client_agent_id = self.client_agent_id()?;
        self.claim_as(assignment, &client_agent_id)
    }

    /// Claim as an explicit client. A bound local alias uses its own stored
    /// id; a profile-scoped session (CAD-1964) uses one derived from the
    /// profile, so the board can tell the two apart and refuse a report from
    /// the wrong holder.
    fn claim_as(&mut self, assignment: &Assignment, client_agent_id: &str) -> Result<Assignment> {
        self.remember_discovered(assignment)?;
        let body = json!({
            "version": VERSION,
            "organization_id": self.org_id,
            "expected_generation": assignment.generation,
            "client_agent_id": client_agent_id,
        });
        let value = self.exchange(
            "POST",
            &format!("{}/{}/claim", self.base(), assignment.assignment_id),
            Some(&body),
        )?;
        let claimed = self.parse_owned(&value)?;
        if claimed.assignment_id != assignment.assignment_id
            || claimed.generation != assignment.generation
            || claimed.issue_id != assignment.issue_id
            || claimed.client_agent_id.as_deref() != Some(client_agent_id)
            || claimed.state != "running"
        {
            return Err(Error::rejected(
                "claim response named a different assignment",
            ));
        }
        self.with_ledger(|ledger| {
            let mut found = false;
            for attempt in ledger.attempts.values_mut() {
                if attempt.assignment_id.as_deref() == Some(claimed.assignment_id.as_str())
                    || (attempt.assignment_id.is_none()
                        && attempt.issue_id == claimed.issue_id
                        && attempt.action == claimed.action)
                {
                    stamp(attempt, &claimed);
                    found = true;
                }
            }
            if !found {
                let mut attempt = Attempt {
                    operation_id: operation_id(),
                    issue_id: claimed.issue_id.clone(),
                    action: claimed.action.clone(),
                    target: None,
                    assignment_id: None,
                    generation: None,
                    claim_id: None,
                    state: "discovered".to_string(),
                    fenced: false,
                    reassign_operation_id: None,
                    reassign_fingerprint: None,
                    reassign_expected_generation: None,
                    worktree: None,
                    lane: None,
                    lane_claim: None,
                    lane_generation: None,
                    kickoff_message: None,
                    kickoff_reserved: None,
                    origin_claim_id: None,
                    origin_generation: None,
                    session_profile: None,
                };
                stamp(&mut attempt, &claimed);
                ledger
                    .attempts
                    .insert(format!("discovered:{}", claimed.assignment_id), attempt);
            }
            Ok(())
        })?;
        Ok(claimed)
    }

    /// Record a board row before claim, reassign, or delivery. A known row
    /// is stamped again, including when another process already claimed the
    /// replacement, so the old kickoff is not treated as delivered.
    fn remember_discovered(&self, assignment: &Assignment) -> Result<()> {
        self.with_ledger(|ledger| {
            let mut found = false;
            for attempt in ledger.attempts.values_mut() {
                if attempt.assignment_id.as_deref() == Some(assignment.assignment_id.as_str()) {
                    stamp(attempt, assignment);
                    found = true;
                }
            }
            if found {
                return Ok(());
            }
            let mut attempt = Attempt {
                operation_id: operation_id(),
                issue_id: assignment.issue_id.clone(),
                action: assignment.action.clone(),
                target: assignment.target.clone(),
                assignment_id: None,
                generation: None,
                claim_id: None,
                state: "discovered".to_string(),
                fenced: false,
                reassign_operation_id: None,
                reassign_fingerprint: None,
                reassign_expected_generation: None,
                worktree: None,
                lane: None,
                lane_claim: None,
                lane_generation: None,
                kickoff_message: None,
                kickoff_reserved: None,
                origin_claim_id: None,
                origin_generation: None,
                session_profile: None,
            };
            stamp(&mut attempt, assignment);
            ledger
                .attempts
                .insert(format!("discovered:{}", assignment.assignment_id), attempt);
            Ok(())
        })
    }

    /// Persist the kickoff message id before the actor is messaged.
    /// A retry returns the same id and does not replace its origin.
    fn reserve_kickoff(&self, claimed: &Assignment) -> Result<String> {
        let claim_id = claimed.claim_id.clone().ok_or_else(|| {
            Error::rejected("the local claim has no id; the actor was not messaged")
        })?;
        if !prefixed_id(&claim_id, "hlc_") {
            return Err(Error::rejected(
                "the local claim has no id; the actor was not messaged",
            ));
        }
        let organization_id = self.org_id.clone();
        let audience = self.audience.clone();
        self.with_ledger(|ledger| {
            let (id, fresh) = {
                let recorded = ledger.attempts.values().any(|attempt| {
                    attempt.assignment_id.as_deref() == Some(claimed.assignment_id.as_str())
                });
                let Some(attempt) = ledger
                    .attempts
                    .values_mut()
                    .find(|attempt| claim_still_active(attempt, claimed))
                else {
                    return Err(if recorded {
                        Error::rejected(
                            "this claim is no longer active; a later generation was not messaged with its kickoff",
                        )
                    } else {
                        Error::rejected(
                            "the assignment was not recorded before the actor was messaged",
                        )
                    });
                };
                if let Some(id) = attempt
                    .kickoff_reserved
                    .clone()
                    .or_else(|| attempt.kickoff_message.clone())
                {
                    (id, false)
                } else {
                    let id = message_id();
                    attempt.kickoff_reserved = Some(id.clone());
                    attempt.origin_claim_id = Some(claim_id.clone());
                    attempt.origin_generation = Some(claimed.generation);
                    (id, true)
                }
            };
            if let Some(origin) = ledger.origins.get(&id) {
                if origin.assignment_id != claimed.assignment_id
                    || origin.claim_id != claim_id
                    || origin.generation != claimed.generation
                    || origin.organization_id != self.org_id || origin.audience != self.audience
                    || origin.subject_id != self.subject_id || origin.issuer != self.issuer
                {
                    return Err(Error::rejected(
                        "this claim is no longer active; a later generation was not messaged with its kickoff",
                    ));
                }
                return Ok(id);
            }
            if !fresh && self.subject_id.is_some() {
                return Err(Error::rejected("reserved kickoff has no original member pin; it cannot inherit the current login"));
            }
            let (origin_claim, origin_generation) = if fresh {
                (claim_id.clone(), claimed.generation)
            } else {
                ledger
                    .attempts
                    .values()
                    .find(|attempt| claim_still_active(attempt, claimed))
                    .and_then(|attempt| {
                        attempt.origin_claim_id.clone().zip(attempt.origin_generation)
                    })
                    .ok_or_else(|| {
                        Error::rejected(
                            "that kickoff has no originating claim; a later claim is not authority to report it",
                        )
                    })?
            };
            if origin_claim != claim_id || origin_generation != claimed.generation {
                return Err(Error::rejected(
                    "this claim is no longer active; a later generation was not messaged with its kickoff",
                ));
            }
            let lane = ledger
                .attempts
                .values()
                .find(|attempt| claim_still_active(attempt, claimed))
                .and_then(|attempt| attempt.lane.clone());
            ledger.origins.insert(
                id.clone(),
                Origin {
                    message_id: id.clone(),
                    organization_id: organization_id.clone(),
                    audience: audience.clone(),
                    subject_id: self.subject_id.clone(),
                    issuer: self.issuer.clone(),
                    assignment_id: claimed.assignment_id.clone(),
                    claim_id: origin_claim,
                    generation: origin_generation,
                    lane,
                    settled: false,
                },
            );
            Ok(id)
        })
    }

    fn mark_kickoff_delivered(&self, assignment: &str, message: &str) -> Result<()> {
        self.with_ledger(|ledger| {
            let settled = ledger
                .origins
                .get(message)
                .is_some_and(|origin| origin.assignment_id == assignment && origin.settled);
            if settled {
                return Ok(());
            }
            if !ledger.origins.contains_key(message) {
                return Err(Error::rejected(
                    "the actor receipt is not the reserved kickoff; nothing was recorded as delivered",
                ));
            }
            let reserved = ledger.attempts.values().any(|attempt| {
                attempt.assignment_id.as_deref() == Some(assignment)
                    && (attempt.kickoff_reserved.as_deref() == Some(message)
                        || attempt.kickoff_message.as_deref() == Some(message))
            });
            if !reserved {
                return Err(Error::rejected(
                    "the actor receipt is not the reserved kickoff; nothing was recorded as delivered",
                ));
            }
            for attempt in ledger.attempts.values_mut() {
                if attempt.assignment_id.as_deref() == Some(assignment) {
                    attempt.kickoff_message = Some(message.to_string());
                }
            }
            Ok(())
        })
    }

    /// Delivered kickoff for this claim only. A newer generation's message
    /// is not already delivered for the checkout this activation opened.
    fn delivered_kickoff(&self, claimed: &Assignment) -> Result<Option<String>> {
        self.with_ledger(|ledger| {
            Ok(ledger.attempts.values().find_map(|attempt| {
                claim_still_active(attempt, claimed)
                    .then(|| attempt.kickoff_message.clone())
                    .flatten()
            }))
        })
    }

    fn origin_for_message(&self, message: &str) -> Result<(String, String, u64)> {
        self.with_ledger(|ledger| {
            let origin = ledger.origins.get(message).ok_or_else(|| {
                Error::rejected(
                    "that kickoff has no originating claim; a later claim is not authority to report it",
                )
            })?;
            if origin.organization_id != self.org_id || origin.audience != self.audience
                || origin.subject_id != self.subject_id || origin.issuer != self.issuer {
                return Err(Error::rejected("kickoff belongs to its original workspace member and issuer"));
            }
            Ok((
                origin.assignment_id.clone(),
                origin.claim_id.clone(),
                origin.generation,
            ))
        })
    }

    /// The claim reserved with this assignment's current kickoff.
    /// The mutable ledger claim is not authority.
    fn origin_for_assignment(&self, assignment: &str) -> Result<(String, u64)> {
        self.with_ledger(|ledger| {
            let attempt = ledger
                .attempts
                .values()
                .find(|attempt| {
                    attempt.assignment_id.as_deref() == Some(assignment) && !attempt.fenced
                })
                .ok_or_else(|| {
                    Error::rejected(
                        "no local claim for that assignment; connect again to recover it rather than inventing a report",
                    )
                })?;
            let message = match attempt
                .kickoff_message
                .clone()
                .or_else(|| attempt.kickoff_reserved.clone())
            {
                Some(message) => message,
                // CAD-1964: a profile-scoped session has no local actor to
                // message, so the claim it made in this ledger is its only
                // authority. The board still re-checks the claim, its
                // generation and the client id on every report.
                None if attempt.session_profile.is_some() => {
                    return match (attempt.claim_id.clone(), attempt.generation) {
                        (Some(claim), Some(generation)) => Ok((claim, generation)),
                        _ => Err(Error::rejected(
                            "that assignment has no claim this session made; a report needs one",
                        )),
                    };
                }
                None => {
                    return Err(Error::rejected(
                        "that assignment has no kickoff identity; a later claim is not authority to report it",
                    ));
                }
            };
            let origin = ledger.origins.get(&message).ok_or_else(|| {
                Error::rejected(
                    "that assignment has no originating claim; a later claim is not authority to report it",
                )
            })?;
            if origin.assignment_id != assignment || origin.organization_id != self.org_id
                || origin.audience != self.audience || origin.subject_id != self.subject_id || origin.issuer != self.issuer {
                return Err(Error::rejected(
                    "kickoff identity does not match that assignment",
                ));
            }
            Ok((origin.claim_id.clone(), origin.generation))
        })
    }

    fn block_claim(&mut self, claimed: &Assignment, text: &str) -> bool {
        if self.ensure_active().is_err() {
            return false;
        }
        let Some(claim_id) = claimed.claim_id.as_deref() else {
            return false;
        };
        let Ok(text) = wire::bounded_report_text(
            &wire::ReportIdentity {
                organization_id: &claimed.organization_id,
                claim_id,
                generation: claimed.generation,
            },
            "blocked",
            None,
            &claimed.assignment_id,
            text,
        ) else {
            return false;
        };
        self.report_claimed(
            &claimed.assignment_id,
            claim_id,
            claimed.generation,
            "blocked",
            Some(&text),
            None,
        )
        .is_ok()
    }

    fn activation_failure(&mut self, claimed: &Assignment, error: Error) -> Error {
        if let Err(cancelled) = self.ensure_active() {
            return cancelled;
        }
        if error.code() == Some("binding_cancelled") {
            return error;
        }
        let blocked = self.block_claim(claimed, &error.to_string());
        if let Err(cancelled) = self.ensure_active() {
            return cancelled;
        }
        if blocked {
            Error::invalid("assignment_blocked", error.to_string())
        } else {
            error
        }
    }

    /// Lane already stored for this claim, or a new one that does not
    /// sit on an unsettled earlier actor's branch.
    fn lane_for(&self, claimed: &Assignment, name: Option<&str>) -> Result<String> {
        let claim_id = claimed
            .claim_id
            .clone()
            .ok_or_else(|| Error::rejected("the local claim has no id; no worktree was created"))?;
        self.with_ledger(|ledger| {
            if let Some(lane) = ledger.attempts.values().find_map(|attempt| {
                (attempt.assignment_id.as_deref() == Some(claimed.assignment_id.as_str())
                    && attempt.lane_claim.as_deref() == Some(claim_id.as_str())
                    && attempt.lane_generation == Some(claimed.generation))
                .then(|| attempt.lane.clone())
                .flatten()
            }) {
                return Ok(lane);
            }
            let lane = bounded_lane(
                &claimed.issue_id,
                name,
                &claimed.assignment_id,
                claimed.generation,
            )?;
            let occupied = ledger.origins.values().any(|origin| {
                origin.assignment_id == claimed.assignment_id
                    && !origin.settled
                    && (origin.claim_id != claim_id || origin.generation != claimed.generation)
                    && origin.lane.as_deref() == Some(lane.as_str())
            });
            if occupied {
                return Err(Error::rejected(
                    "that worktree still belongs to an unsettled actor; a new generation was not checked out there",
                ));
            }
            for attempt in ledger.attempts.values_mut() {
                if attempt.assignment_id.as_deref() == Some(claimed.assignment_id.as_str()) {
                    attempt.lane = Some(lane.clone());
                    attempt.lane_claim = Some(claim_id.clone());
                    attempt.lane_generation = Some(claimed.generation);
                    attempt.worktree = None;
                }
            }
            Ok(lane)
        })
    }

    /// True when this claim generation was already reported terminal.
    /// A later running or queued snapshot must not reserve another message.
    fn claim_already_ended(&self, claimed: &Assignment) -> Result<bool> {
        let Some(claim_id) = claimed.claim_id.as_deref() else {
            return Ok(false);
        };
        self.with_ledger(|ledger| {
            let settled = ledger.origins.values().any(|origin| {
                origin.assignment_id == claimed.assignment_id
                    && origin.claim_id == claim_id
                    && origin.generation == claimed.generation
                    && origin.settled
            });
            let ended = ledger.attempts.values().any(|attempt| {
                attempt.assignment_id.as_deref() == Some(claimed.assignment_id.as_str())
                    && attempt.claim_id.as_deref() == Some(claim_id)
                    && attempt.generation == Some(claimed.generation)
                    && attempt_ended(&attempt.state)
            });
            Ok(settled || ended)
        })
    }

    fn activate<F>(
        &mut self,
        assignment: &Assignment,
        root: &Path,
        name: Option<&str>,
        alias: &str,
        deliver: &mut F,
    ) -> Result<Value>
    where
        F: FnMut(&Kickoff) -> Result<Value>,
    {
        self.ensure_active()?;
        let client_agent_id = self.client_agent_id()?;
        let claimed = if assignment.state == "blocked" {
            self.remember_discovered(assignment)?;
            if assignment.claim_id.is_none()
                || assignment.client_agent_id.as_deref() != Some(client_agent_id.as_str())
            {
                return Err(Error::invalid(
                    "fenced",
                    "this blocked assignment is not this machine's claim; nothing was started",
                ));
            }
            assignment.clone()
        } else if assignment.state == "running"
            && assignment.client_agent_id.as_deref() != Some(client_agent_id.as_str())
        {
            return Err(Error::invalid(
                "fenced",
                "another local worker of this member already holds the claim; this machine did not start a second one",
            ));
        } else {
            // Lists are observations, not execution authority. Replay the
            // live claim even for this machine before any local handoff.
            self.claim(assignment)?
        };
        if self.claim_already_ended(&claimed)? {
            return Err(Error::invalid(
                "terminal",
                "that assignment already ended; it was not reopened",
            ));
        }
        // A blocked row is already this claim. Mark it running before the
        // actor is messaged so a fast completion cannot land first and
        // then be downgraded. A fresh claim is already running.
        let mut current = claimed.clone();
        if assignment.state == "blocked" {
            let (claim_id, generation) = self.resume_binding(&claimed)?;
            current = self.report_claimed(
                &claimed.assignment_id,
                &claim_id,
                generation,
                "running",
                None,
                None,
            )?;
        }
        let lane = match self.lane_for(&current, name) {
            Ok(lane) => lane,
            Err(error) => {
                return Err(self.activation_failure(&current, error));
            }
        };
        let pinned = self.with_ledger(|ledger| {
            Ok(ledger.attempts.values().find_map(|attempt| {
                (attempt.assignment_id.as_deref() == Some(current.assignment_id.as_str())
                    && attempt.lane_claim.as_deref() == current.claim_id.as_deref()
                    && attempt.lane_generation == Some(current.generation))
                .then(|| attempt.worktree.clone())
                .flatten()
            }))
        })?;
        let opened = match pinned {
            Some(path) => worktree::main_root(Path::new(&path))
                .and_then(|original_root| ensure_lane(&original_root, &lane)),
            None => ensure_lane(root, &lane),
        };
        let dir = match opened {
            Ok(dir) => dir,
            Err(error) => {
                return Err(self.activation_failure(&current, error));
            }
        };
        let path = dir.display().to_string();
        self.with_ledger(|ledger| {
            for attempt in ledger.attempts.values_mut() {
                if attempt.assignment_id.as_deref() == Some(current.assignment_id.as_str())
                    && attempt.lane_claim.as_deref() == current.claim_id.as_deref()
                    && attempt.lane_generation == Some(current.generation)
                {
                    attempt.worktree = Some(path.clone());
                }
            }
            Ok(())
        })?;
        let actor = if let Some(message) = self.delivered_kickoff(&current)? {
            json!({"message": message, "state": "already_delivered"})
        } else {
            #[cfg(test)]
            if let Some(wait) = self.before_reservation.clone() {
                wait();
            }
            let message_id = self.reserve_kickoff(&current)?;
            let (_origin_assignment, origin_claim, origin_generation) =
                self.origin_for_message(&message_id)?;
            let kickoff = Kickoff {
                alias,
                issue_id: &current.issue_id,
                assignment_id: &current.assignment_id,
                worktree: &path,
                message_id: &message_id,
                claim_id: &origin_claim,
                generation: origin_generation,
                organization_id: &current.organization_id,
                audience: self.audience.as_str(),
                subject_id: self.subject_id.as_deref(),
                issuer: self.issuer.as_deref(),
            };
            match deliver(&kickoff) {
                Ok(value) => {
                    let message = value
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or(message_id.as_str())
                        .to_string();
                    if message != message_id
                        || crate::proto::identifier(&message, "actor message").is_err()
                    {
                        return Err(self.activation_failure(
                            &current,
                            Error::rejected(
                                "the local actor receipt did not match the reserved message id",
                            ),
                        ));
                    }
                    self.mark_kickoff_delivered(&current.assignment_id, &message)?;
                    let mut receipt = if value.is_object() {
                        value
                    } else {
                        json!({"receipt": value})
                    };
                    receipt["alias"] = json!(alias);
                    receipt["message"] = json!(message);
                    receipt
                }
                Err(error) => {
                    return Err(self.activation_failure(&current, error));
                }
            }
        };
        let mut value = current.to_json();
        value["worktree"] = json!(path);
        value["lane"] = json!(lane);
        value["actor"] = actor;
        Ok(value)
    }

    /// Claim recorded for this blocked row. Prefer the kickoff origin so
    /// a later ledger claim cannot resume a different generation.
    fn resume_binding(&self, claimed: &Assignment) -> Result<(String, u64)> {
        let reserved = self.with_ledger(|ledger| {
            Ok(ledger
                .attempts
                .values()
                .find(|attempt| {
                    attempt.assignment_id.as_deref() == Some(claimed.assignment_id.as_str())
                        && !attempt.fenced
                })
                .and_then(|attempt| {
                    attempt
                        .kickoff_message
                        .clone()
                        .or_else(|| attempt.kickoff_reserved.clone())
                }))
        })?;
        if let Some(message) = reserved {
            let (_assignment, claim_id, generation) = self.origin_for_message(&message)?;
            return Ok((claim_id, generation));
        }
        let claim_id = claimed
            .claim_id
            .clone()
            .ok_or_else(|| Error::rejected("this blocked assignment has no claim to resume"))?;
        Ok((claim_id, claimed.generation))
    }

    fn report_claimed(
        &mut self,
        assignment: &str,
        claim_id: &str,
        generation: u64,
        state: &str,
        text: Option<&str>,
        head: Option<&str>,
    ) -> Result<Assignment> {
        if !prefixed_id(assignment, "hla_") || !prefixed_id(claim_id, "hlc_") {
            return Err(Error::rejected("report is missing its claim"));
        }
        let body = wire::report_body(
            &wire::ReportIdentity {
                organization_id: &self.org_id,
                claim_id,
                generation,
            },
            state,
            text,
            head,
        )?;
        match self.exchange(
            "POST",
            &format!("{}/{assignment}/report", self.base()),
            Some(&body),
        ) {
            Ok(value) => {
                let reported = self.parse_owned(&value)?;
                if reported.assignment_id != assignment {
                    return Err(Error::rejected(
                        "report response named a different assignment",
                    ));
                }
                self.with_ledger(|ledger| {
                    for attempt in ledger.attempts.values_mut() {
                        if attempt.assignment_id.as_deref() != Some(reported.assignment_id.as_str())
                        {
                            continue;
                        }
                        if attempt.claim_id.as_deref() != Some(claim_id)
                            || attempt.generation != Some(generation)
                        {
                            continue;
                        }
                        stamp(attempt, &reported);
                        if matches!(reported.state.as_str(), "failed" | "result" | "cancelled") {
                            attempt.kickoff_message = None;
                            attempt.kickoff_reserved = None;
                        }
                    }
                    if matches!(reported.state.as_str(), "failed" | "result" | "cancelled") {
                        for origin in ledger.origins.values_mut() {
                            if origin.assignment_id == assignment
                                && origin.claim_id == claim_id
                                && origin.generation == generation
                            {
                                origin.settled = true;
                            }
                        }
                    }
                    Ok(())
                })?;
                Ok(reported)
            }
            Err(error) if error.code() == Some("fenced") => {
                self.with_ledger(|ledger| {
                    let protected: Vec<String> = ledger
                        .origins
                        .iter()
                        .filter(|(_, origin)| {
                            origin.assignment_id == assignment
                                && origin.claim_id == claim_id
                                && origin.generation == generation
                        })
                        .map(|(id, _)| id.clone())
                        .collect();
                    for attempt in ledger.attempts.values_mut() {
                        if attempt.assignment_id.as_deref() != Some(assignment) {
                            continue;
                        }
                        let current = attempt
                            .kickoff_message
                            .clone()
                            .or_else(|| attempt.kickoff_reserved.clone());
                        if current
                            .as_ref()
                            .is_some_and(|id| protected.iter().any(|item| item == id))
                        {
                            attempt.fenced = true;
                            attempt.claim_id = None;
                            attempt.kickoff_message = None;
                            attempt.kickoff_reserved = None;
                        }
                    }
                    Ok(())
                })?;
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    pub fn dispatch(&mut self, issue: &str, target: Option<&str>) -> Result<Value> {
        let target = match target {
            Some(target) => Some(target.to_string()),
            None => self.default_target()?,
        };
        let assignment = self.queue(issue, "dispatch", target.as_deref())?;
        let mut value = assignment.to_json();
        if assignment.state == "waiting" {
            value["guidance"] = json!({
                "code": "connect_local_agent",
                "text": "Connect a local agent alias for this workspace. This assignment is waiting and was not sent to a hosted runner.",
            });
        }
        Ok(value)
    }

    fn live_for_issue(&mut self, issue: &str) -> Result<Option<Assignment>> {
        Ok(self.live_assignments(true)?.into_iter().find(|row| {
            row.issue_id == issue
                && !matches!(row.state.as_str(), "result" | "failed" | "cancelled")
        }))
    }

    /// Waiting work this user created is reassigned to them. Another
    /// member's target is refused before a worktree exists.
    fn prepare_owned(&mut self, row: Assignment, subject: &str) -> Result<Assignment> {
        if row.state == "waiting" && row.target.is_none() {
            if row.created_by_subject_id != subject {
                return Err(Error::invalid(
                    "assignment_live",
                    "this issue is waiting for another member; nothing was claimed",
                ));
            }
            self.remember_discovered(&row)?;
            return self.reassign_owned(&row.assignment_id, Some(subject), false);
        }
        if row.target.as_deref() != Some(subject) {
            return Err(Error::invalid(
                "assignment_live",
                "this issue is assigned to another member; reassign it or start on that machine. No worktree was created",
            ));
        }
        if !matches!(row.state.as_str(), "queued" | "running" | "blocked") {
            return Err(Error::invalid(
                "terminal",
                "that assignment already ended; it was not reopened",
            ));
        }
        self.remember_discovered(&row)?;
        Ok(row)
    }

    pub fn start_here<F>(
        &mut self,
        issue: &str,
        repo: &Path,
        name: Option<&str>,
        deliver: &mut F,
    ) -> Result<Value>
    where
        F: FnMut(&Kickoff) -> Result<Value>,
    {
        let root = worktree::main_root(repo)?;
        slug(name)?;
        let alias = self.stored_alias()?;
        let Some(alias) = alias else {
            let queued = self.queue(issue, "start", None)?;
            let mut value = queued.to_json();
            value["actor"] = Value::Null;
            value["guidance"] = json!({
                "code": "connect_local_agent",
                "text": "This assignment is waiting for your local agent. Run `cadence local-agent connect --alias <alias> --repo <checkout>`. It was not sent to a hosted runner.",
            });
            return Ok(value);
        };
        let subject = self.subject_id.clone().ok_or_else(|| {
            Error::invalid(
                "config_required",
                "this workspace login has no directory subject; nothing was started",
            )
        })?;
        if let Some(existing) = self.live_for_issue(issue)? {
            let ready = self.prepare_owned(existing, &subject)?;
            return self.activate(&ready, &root, name, &alias, deliver);
        }
        let queued = self.queue(issue, "start", Some(&subject))?;
        let ready = self.prepare_owned(queued, &subject)?;
        self.activate(&ready, &root, name, &alias, deliver)
    }

    /// Start one named issue for the calling session under a reusable
    /// hosted profile (CAD-1964).
    ///
    /// The caller is the actor, so no local alias is needed: nothing is
    /// messaged to a daemon and no other waiting assignment is touched. The
    /// claim is bound to a client id derived from the profile, which the
    /// board re-checks on every report. A profile is configuration, not a
    /// credential — the authenticated login still carries the authority.
    pub fn start_session(
        &mut self,
        issue: &str,
        repo: &Path,
        name: Option<&str>,
        profile: &str,
    ) -> Result<Value> {
        self.ensure_active()?;
        if !prefixed_id(profile, "ap_") {
            return Err(Error::rejected(
                "--profile must be a hosted profile id (ap_…)",
            ));
        }
        let root = worktree::main_root(repo)?;
        slug(name)?;
        let subject = self.subject_id.clone().ok_or_else(|| {
            Error::invalid(
                "config_required",
                "this workspace login has no directory subject; nothing was started",
            )
        })?;
        let client = session_client_agent_id(profile);
        let ready = match self.live_for_issue(issue)? {
            Some(row) => self.prepare_owned(row, &subject)?,
            None => {
                let queued = self.queue(issue, "start", Some(&subject))?;
                self.prepare_owned(queued, &subject)?
            }
        };
        let claimed = if matches!(ready.state.as_str(), "running" | "blocked") {
            if ready.client_agent_id.as_deref() != Some(client.as_str()) {
                return Err(Error::invalid(
                    "fenced",
                    "another worker of this member already holds the claim; this session did not start a second one",
                ));
            }
            ready.clone()
        } else {
            self.claim_as(&ready, &client)?
        };
        if self.claim_already_ended(&claimed)? {
            return Err(Error::invalid(
                "terminal",
                "that assignment already ended; it was not reopened",
            ));
        }
        let claim_id = claimed
            .claim_id
            .clone()
            .ok_or_else(|| Error::rejected("the claim has no id; no worktree was created"))?;
        // The claim is reportable from the moment it exists: a lane that
        // cannot be opened still leaves this session holding the work.
        self.with_ledger(|ledger| {
            for attempt in ledger.attempts.values_mut() {
                if attempt.assignment_id.as_deref() == Some(claimed.assignment_id.as_str())
                    && attempt.claim_id.as_deref() == Some(claim_id.as_str())
                {
                    attempt.session_profile = Some(profile.to_string());
                }
            }
            Ok(())
        })?;
        let lane = self.lane_for(&claimed, name)?;
        let dir = ensure_lane(&root, &lane)?;
        let path = dir.display().to_string();
        self.with_ledger(|ledger| {
            for attempt in ledger.attempts.values_mut() {
                if attempt.assignment_id.as_deref() == Some(claimed.assignment_id.as_str())
                    && attempt.claim_id.as_deref() == Some(claim_id.as_str())
                {
                    attempt.worktree = Some(path.clone());
                }
            }
            Ok(())
        })?;
        Ok(json!({
            "version": VERSION,
            "assignment_id": claimed.assignment_id,
            "issue_id": claimed.issue_id,
            "organization_id": claimed.organization_id,
            "state": claimed.state,
            "claim_id": claim_id,
            "generation": claimed.generation,
            "client_agent_id": client,
            "profile": profile,
            "lane": lane,
            "worktree": path,
            "actor": "external_session",
            "guidance": {
                "code": "report_from_this_session",
                "text": "This session holds the claim. Report with `cadence local-agent report <assignment> --state running|blocked|failed|result`; no local agent was connected.",
            },
        }))
    }

    pub fn connect<F>(
        &mut self,
        repo: Option<&Path>,
        name: Option<&str>,
        alias: Option<&str>,
        deliver: &mut F,
    ) -> Result<Value>
    where
        F: FnMut(&Kickoff) -> Result<Value>,
    {
        self.configure(repo, name, alias)?;
        self.pickup(name, true, deliver)
    }

    /// Persist explicit configuration without a second pickup owner.
    pub fn configure(
        &self,
        repo: Option<&Path>,
        name: Option<&str>,
        alias: Option<&str>,
    ) -> Result<()> {
        slug(name)?;
        let alias = alias.map(alias_name).transpose()?;
        let repo = repo
            .map(worktree::main_root)
            .transpose()?
            .map(|root| root.display().to_string());
        self.with_ledger(|ledger| {
            if let Some(alias) = alias {
                ledger.alias = Some(alias);
            }
            if let Some(repo) = repo {
                ledger.repo = Some(repo);
            }
            Ok(())
        })
    }

    /// Consume configured local work. Background pickup leaves blocked work
    /// for an explicit connect/start retry rather than silently resuming it.
    pub fn pickup<F>(
        &mut self,
        name: Option<&str>,
        resume_blocked: bool,
        deliver: &mut F,
    ) -> Result<Value>
    where
        F: FnMut(&Kickoff) -> Result<Value>,
    {
        let rows = self.live_assignments(resume_blocked)?;
        self.pickup_rows(rows, name, resume_blocked, None, deliver)
    }

    fn pickup_rows<F>(
        &mut self,
        rows: Vec<Assignment>,
        name: Option<&str>,
        resume_blocked: bool,
        bound_alias: Option<&str>,
        deliver: &mut F,
    ) -> Result<Value>
    where
        F: FnMut(&Kickoff) -> Result<Value>,
    {
        let alias = match bound_alias {
            Some(alias) => alias_name(alias)?,
            None => self.stored_alias()?.ok_or_else(|| {
                Error::invalid(
                    "config_required",
                    "connect an explicit local assignment alias first",
                )
            })?,
        };
        let subject = self.subject_id.clone().ok_or_else(|| {
            Error::invalid(
                "config_required",
                "this workspace login has no directory subject",
            )
        })?;
        let client_agent_id = self.client_agent_id()?;
        let stored = self.with_ledger(|ledger| Ok(ledger.repo.clone()))?;
        let Some(stored) = stored else {
            return Ok(json!({
                "configured": true,
                "target": subject,
                "alias": alias,
                "client_agent_id": client_agent_id,
                "assignments": rows.iter().map(Assignment::to_json).collect::<Vec<_>>(),
                "guidance": {
                    "code": "repo_required",
                    "text": "Pass --repo to claim queued work into a local worktree and deliver it to the local agent. Nothing was claimed.",
                },
            }));
        };
        let root = worktree::main_root(Path::new(&stored))?;
        let mut claimed = Vec::new();
        let mut refused = Vec::new();
        for row in rows {
            if row.state == "blocked" && !resume_blocked {
                continue;
            }
            let mut row = row;
            if row.state == "waiting"
                && row.target.is_none()
                && row.created_by_subject_id == subject
            {
                if let Err(error) = self.remember_discovered(&row) {
                    refused.push(json!({
                        "assignment_id": row.assignment_id,
                        "error": error.to_string(),
                        "code": error.code(),
                    }));
                    continue;
                }
                match self.reassign_owned(&row.assignment_id, Some(&subject), false) {
                    Ok(updated) => row = updated,
                    Err(error) => {
                        refused.push(json!({
                            "assignment_id": row.assignment_id,
                            "error": error.to_string(),
                            "code": error.code(),
                        }));
                        continue;
                    }
                }
            }
            if row.target.as_deref() != Some(subject.as_str()) {
                continue;
            }
            if !matches!(row.state.as_str(), "queued" | "running" | "blocked") {
                continue;
            }
            if row
                .client_agent_id
                .as_ref()
                .is_some_and(|id| id != &client_agent_id)
            {
                refused.push(json!({
                    "assignment_id": row.assignment_id,
                    "code": "fenced",
                    "error": "another local worker of this member already holds the claim",
                }));
                continue;
            }
            match self.activate(&row, &root, name, &alias, deliver) {
                Ok(value) => claimed.push(value),
                Err(error) => refused.push(json!({
                    "assignment_id": row.assignment_id,
                    "error": error.to_string(),
                    "code": error.code(),
                })),
            }
        }
        Ok(json!({
            "configured": true,
            "target": subject,
            "alias": alias,
            "client_agent_id": client_agent_id,
            "claimed": claimed,
            "refused": refused,
        }))
    }

    pub fn disconnect(&self) -> Result<Value> {
        self.with_ledger(|ledger| {
            ledger.alias = None;
            Ok(())
        })?;
        Ok(json!({
            "configured": false,
            "stopped": false,
            "reported": false,
            "text": "Local pickup disconnected. Existing actors continue and retain their original claims.",
        }))
    }

    pub fn status(&mut self) -> Result<Value> {
        let alias = self.stored_alias()?;
        let client_agent_id = self.client_agent_id()?;
        let assignments = self.list_state(None)?;
        Ok(json!({
            "configured": alias.is_some(),
            "target": self.subject_id,
            "alias": alias,
            "client_agent_id": client_agent_id,
            "assignments": assignments.iter().map(Assignment::to_json).collect::<Vec<_>>(),
        }))
    }

    pub fn report(
        &mut self,
        assignment: &str,
        state: &str,
        text: Option<&str>,
        head: Option<&str>,
    ) -> Result<Value> {
        if !matches!(state, "running" | "blocked" | "failed" | "result") {
            return Err(Error::rejected(
                "report state must be running, blocked, failed, or result",
            ));
        }
        if state == "result" && !head_sha(head.unwrap_or("")) {
            return Err(Error::rejected(
                "result requires --head of 40 or 64 lowercase hex characters",
            ));
        }
        if matches!(state, "blocked" | "failed" | "result") && text.unwrap_or("").is_empty() {
            return Err(Error::rejected("this report requires --text"));
        }
        let (claim_id, generation) = self.origin_for_assignment(assignment)?;
        Ok(self
            .report_claimed(assignment, &claim_id, generation, state, text, head)?
            .to_json())
    }

    pub fn reassign(
        &mut self,
        assignment: &str,
        target: Option<&str>,
        cancel: bool,
    ) -> Result<Value> {
        let updated = self.reassign_owned(assignment, target, cancel)?;
        let mut value = updated.to_json();
        if cancel {
            value["stopped"] = json!(false);
        }
        Ok(value)
    }

    fn reassign_owned(
        &mut self,
        assignment: &str,
        target: Option<&str>,
        cancel: bool,
    ) -> Result<Assignment> {
        if cancel && target.is_some() {
            return Err(Error::rejected("cancel and --to cannot be combined"));
        }
        if !cancel && target.is_none() {
            return Err(Error::rejected("reassign requires --to or --cancel"));
        }
        if let Some(target) = target {
            if !hosted_id(target) {
                return Err(Error::rejected("--to must be a directory user id"));
            }
        }
        let fingerprint = if cancel {
            "cancel".to_string()
        } else {
            format!("to:{}", target.unwrap_or(""))
        };
        let (operation_id, generation) = self.with_ledger(|ledger| {
            let attempt = ledger
                .attempts
                .values_mut()
                .find(|attempt| attempt.assignment_id.as_deref() == Some(assignment))
                .ok_or_else(|| Error::rejected("no local assignment to reassign"))?;
            if attempt.reassign_fingerprint.as_deref() != Some(fingerprint.as_str()) {
                attempt.reassign_operation_id = Some(operation_id());
                attempt.reassign_fingerprint = Some(fingerprint);
                attempt.reassign_expected_generation = attempt.generation;
            } else if attempt.reassign_expected_generation.is_none() {
                attempt.reassign_expected_generation = attempt.generation;
            }
            let operation_id = attempt
                .reassign_operation_id
                .clone()
                .ok_or_else(|| Error::rejected("reassign operation was not saved"))?;
            let generation = attempt
                .reassign_expected_generation
                .ok_or_else(|| Error::rejected("assignment has no generation"))?;
            Ok((operation_id, generation))
        })?;
        let mut body = json!({
            "version": VERSION,
            "organization_id": self.org_id,
            "operation_id": operation_id,
            "expected_generation": generation,
        });
        if cancel {
            body["cancel"] = json!(true);
        } else {
            body["target"] = json!(target);
        }
        let value = self.exchange(
            "POST",
            &format!("{}/{assignment}/reassign", self.base()),
            Some(&body),
        )?;
        let updated = self.parse_owned(&value)?;
        if updated.assignment_id != assignment {
            return Err(Error::rejected(
                "reassign response named a different assignment",
            ));
        }
        self.with_ledger(|ledger| {
            for attempt in ledger.attempts.values_mut() {
                if attempt.assignment_id.as_deref() != Some(assignment) {
                    continue;
                }
                // The board accepted this operation. Clear it before stamp so
                // the saved replay generation is not left behind on success.
                attempt.reassign_operation_id = None;
                attempt.reassign_fingerprint = None;
                attempt.reassign_expected_generation = None;
                stamp(attempt, &updated);
                attempt.fenced = true;
            }
            Ok(())
        })?;
        Ok(updated)
    }

    pub fn authorize_tool(&self, lane: &str) -> Result<Value> {
        let Some(issuer) = self.issuer.as_deref() else {
            return Err(Error::invalid(
                "config_required",
                "this workspace login has no issuer origin, so the existing device grant cannot run; no lane credential was saved",
            ));
        };
        if lane == "provider.draft" {
            crate::platform::connections::provider_account_identifier(
                crate::platform::agenticos_external::PLATFORM,
                &self.org_id,
            )
            .map_err(|_| {
                Error::invalid(
                    "config_required",
                    "the legacy draft adapter requires a canonical workspace account; CLI/wiki collaboration is unaffected and no financial grant was requested",
                )
            })?;
        }
        let path =
            crate::remote_auth::authorize_lane(issuer, &self.org_id, lane, &self.ledger, false)?;
        Ok(json!({
            "lane": lane,
            "status": "credential_saved",
            "activation": "not_checked",
            "credential_file": path.display().to_string(),
            "enrollment": if lane == "provider.draft" {
                Some(json!({
                    "caller": "local_operator",
                    "argv": ["cadence", "--org", "local", "connection", "create",
                        "agenticos_external", "--account", self.org_id,
                        "--scope", "provider.draft", "--token-stdin"],
                    "stdin_file": path.display().to_string(),
                }))
            } else {
                None
            },
            "guidance": if lane == "publish.send" {
                "Credential saved only. If not already configured, configure the existing publisher door URL and credential file; this did not register a sender, restart the daemon, send, or spend. Standing authorization and budgets still apply."
            } else {
                "Credential saved only. The local operator can enroll or rotate its raw token through the existing custody command, configure the external provider door for this issuer, and select that enrolled account in the authorized app connection. Existing custody policy, app authority and budgets still apply. Normal signed-in funded-account images do not require this legacy provider.draft lane."
            },
        }))
    }
}

fn coded_conflict(code: &'static str, generation: i64, message: &str) -> Error {
    Error::Structured(crate::error::Structured {
        kind: "conflict",
        code: code.to_string(),
        message: message.to_string(),
        revision: Some(generation),
    })
}

fn explain_409(body: &Value) -> Error {
    let code = safe_code(body);
    let generation = body
        .get("generation")
        .and_then(Value::as_u64)
        .and_then(|generation| i64::try_from(generation).ok())
        .unwrap_or(0);
    match code.as_str() {
        "fenced" => coded_conflict(
            "fenced",
            generation,
            "stale assignment report was fenced; current state was not changed",
        ),
        "assignment_live" => coded_conflict(
            "assignment_live",
            generation,
            "this issue already has a live local assignment; reassign it instead of starting another",
        ),
        "idempotency_conflict" => coded_conflict(
            "idempotency_conflict",
            generation,
            "that operation id was already used with a different body; nothing was written",
        ),
        "terminal" => coded_conflict(
            "terminal",
            generation,
            "that assignment already ended; it was not reopened",
        ),
        "hosted_execution_unavailable" => Error::invalid(
            "hosted_execution_unavailable",
            "hosted container execution is not available; the assignment was not rewritten to local",
        ),
        "not_claimable" => Error::invalid(
            "not_claimable",
            "the assignment has no target member to claim yet",
        ),
        "target_unavailable" => Error::invalid(
            "target_unavailable",
            "that directory user cannot take this assignment; nothing was written",
        ),
        _ => Error::invalid(
            "conflict",
            "the hosted assignment refused the request; nothing ran locally",
        ),
    }
}

fn ensure_lane(root: &Path, lane: &str) -> Result<PathBuf> {
    let dir = layout::worktree_dir(root, lane);
    if fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(Error::rejected(
            "local lane path is a symlink; refusing to use it",
        ));
    }
    if dir.is_dir() {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"]);
        let output = crate::reaper::output(&mut command)
            .map_err(|error| Error::internal(error.to_string()))?;
        let branch = String::from_utf8(output.stdout).unwrap_or_default();
        if output.status.success() && branch.trim() == layout::branch(lane) {
            return Ok(dir);
        }
        return Err(Error::rejected(
            "a local lane directory exists on a different branch; no second checkout was created",
        ));
    }
    match worktree::create_worktree(root, lane) {
        Ok(dir) => Ok(dir),
        Err(error) => {
            if dir.is_dir() {
                let mut command = Command::new("git");
                command
                    .arg("-C")
                    .arg(&dir)
                    .args(["rev-parse", "--abbrev-ref", "HEAD"]);
                if let Ok(output) = crate::reaper::output(&mut command) {
                    let branch = String::from_utf8(output.stdout).unwrap_or_default();
                    if output.status.success() && branch.trim() == layout::branch(lane) {
                        return Ok(dir);
                    }
                }
            }
            Err(error)
        }
    }
}

#[cfg(test)]
#[path = "local_assignment_acceptance.rs"]
mod acceptance;
