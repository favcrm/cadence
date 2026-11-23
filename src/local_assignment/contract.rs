//! Hosted local-assignment contract validators, constants, error builders, and parser.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::fmt::Write as _;

use serde_json::{json, Value};
use uuid::Uuid;

use super::{Assignment, VERSION};
use crate::error::{Error, Result};

pub(super) fn hosted_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub(super) fn prefixed_id(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix) && hosted_id(value) && value.len() > prefix.len()
}

pub(super) fn head_sha(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn operation_id() -> String {
    format!("op_{}", Uuid::new_v4().simple())
}

pub(super) fn message_id() -> String {
    Uuid::new_v4().simple().to_string()
}

/// The client id a profile-scoped session claims under (CAD-1964). Derived
/// from the profile rather than random, so retrying the same start replays
/// the same claim instead of opening a second one.
pub(super) fn session_client_agent_id(profile: &str) -> String {
    format!("session-{profile}")
}

pub(super) fn valid_hct(token: &str) -> bool {
    let Some(rest) = token.strip_prefix("hct_") else {
        return false;
    };
    rest.len() == 43
        && rest
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub(super) fn safe_code(body: &Value) -> String {
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

pub(super) fn attempt_key(issue: &str, action: &str, target: Option<&str>) -> String {
    format!("{issue}\u{1}{action}\u{1}{}", target.unwrap_or("-"))
}

pub(super) fn assignment_prefix(org: &str) -> String {
    format!("/__platform/hosted-cadence/{org}/local-assignments")
}

pub(super) fn assignment_unknown(org: &str, path: &str, body: Option<&Value>) -> Error {
    let prefix = assignment_prefix(org);
    let assignment = path
        .strip_prefix(&prefix)
        .and_then(|tail| tail.strip_prefix('/'))
        .and_then(|tail| tail.split('/').next())
        .filter(|id| prefixed_id(id, "hla_"));
    let mut message = match assignment {
        Some(id) => format!("hosted assignment {id} in workspace {org} may have changed"),
        None => format!("hosted coordination in workspace {org} may have changed"),
    };
    for key in ["operation_id", "idempotency_key", "claim_id"] {
        if let Some(value) = body
            .and_then(|value| value.get(key))
            .and_then(Value::as_str)
            .filter(|value| hosted_id(value))
        {
            message.push_str("; ");
            message.push_str(key);
            message.push('=');
            message.push_str(value);
        }
    }
    if let Some(generation) = body
        .and_then(|value| value.get("expected_generation"))
        .and_then(Value::as_u64)
    {
        write!(&mut message, "; expected_generation={generation}")
            .expect("writing to a String cannot fail");
    }
    message.push_str(
        "; the outcome is unknown. Inspect `cadence agent assignments` in the same \
         selected workspace and reconcile the original assignment and operation \
         before deciding whether to retry; do not replay blindly",
    );
    Error::unknown(message)
}

pub(super) fn assignment_custody_failed(
    org: &str,
    action: &str,
    assignment: &Assignment,
    body: &Value,
    error: Error,
) -> Error {
    let mut message = format!(
        "hosted {action} for assignment {} in workspace {org} was confirmed at generation={}; \
         local custody could not be recorded: {error}",
        assignment.assignment_id, assignment.generation
    );
    if let Some(claim) = assignment.claim_id.as_deref() {
        write!(&mut message, "; claim_id={claim}").expect("writing to a String cannot fail");
    }
    // Per path: queue sends `idempotency_key`, reassignment `operation_id`; claim and
    // report send neither (they name `claim_id` above and `expected_generation` below).
    for key in ["operation_id", "idempotency_key"] {
        if let Some(value) = body
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| hosted_id(value))
        {
            write!(&mut message, "; {key}={value}").expect("writing to a String cannot fail");
        }
    }
    if let Some(generation) = body.get("expected_generation").and_then(Value::as_u64) {
        write!(&mut message, "; expected_generation={generation}")
            .expect("writing to a String cannot fail");
    }
    message.push_str(
        "; inspect `cadence agent assignments` in the same selected workspace and reconcile \
         the original assignment before deciding whether to retry; do not replay blindly",
    );
    if body.get("idempotency_key").is_some() {
        message.push_str("; repeating the same queue command reuses this idempotency_key");
    }
    Error::internal(message)
}

pub(super) fn slug(name: Option<&str>) -> Result<String> {
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
pub(super) fn bounded_lane(
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

pub(super) fn alias_name(alias: &str) -> Result<String> {
    crate::proto::identifier(alias, "local agent alias")
}

impl Assignment {
    pub(super) fn to_json(&self) -> Value {
        let mut row = json!({
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
            "profile_id": self.profile_id,
            "guidance": self.guidance,
            "reported_head_sha": self.reported_head_sha,
            "text": self.text,
        });
        if let Some(kind) = &self.unsupported_kind {
            row["kind"] = json!(kind);
        }
        row
    }

    pub(super) fn work_kind_refusal(&self) -> Option<Error> {
        self.unsupported_kind.as_ref().map(|kind| {
            Error::invalid(
                "unsupported_work_kind",
                format!(
                    "{} is {kind} work, which this client cannot run; nothing local was started",
                    self.issue_id
                ),
            )
        })
    }

    pub(super) fn confirmed_kind_refusal(&self, action: &str) -> Option<Error> {
        self.unsupported_kind.as_ref().map(|kind| {
            Error::invalid(
                "unsupported_work_kind",
                format!(
                    "the hosted {action} for {} was confirmed; this client cannot run {kind} \
                     work, so no further local work was started",
                    self.issue_id
                ),
            )
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

/// What this client tolerates, announced on every assignment-plane request
/// (CAD-2060).
/// A header, because the board's request bodies and queries refuse unknown
/// fields: an older board ignores a header and would reject anything else.
pub(super) const CLIENT_FEATURES_HEADER: &str = "X-Cadence-Client-Features";
///
/// It does not announce work kinds, so a board must not send this client a
/// non-dev row. `unsupported_kind` is the defence if one arrives anyway.
pub(super) const CLIENT_FEATURES: &str = "assignment-unknown-keys,event-unknown-types";

pub(super) fn parse_assignment(value: &Value) -> Result<Assignment> {
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
    // An unknown key is ignored so a newer board can add one. Required keys,
    // their values and the forbidden path check above stay strict.
    let object = value
        .as_object()
        .filter(|object| required.iter().all(|key| object.contains_key(*key)))
        .ok_or_else(|| Error::rejected("coordination response is not the assignment contract"))?;
    // Ignoring a work kind would run review or research as dev work. Refusing
    // the row here would stop every later event, snapshot and list, so it is
    // kept and refused where work would start.
    let unsupported_kind = match object.get("kind") {
        None => None,
        Some(kind) if kind == "dev" => None,
        // Board text reaches messages, so only a plain bounded token is shown.
        Some(kind) => Some(
            kind.as_str()
                .filter(|kind| {
                    kind.len() <= 32
                        && kind
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b == b'-' || b == b'_')
                })
                .unwrap_or("unrecognized")
                .to_string(),
        ),
    };
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
    if let Some(profile) = object.get("profile_id") {
        let valid = profile
            .as_str()
            .is_some_and(|value| prefixed_id(value, "ap_"));
        if !valid {
            return Err(Error::rejected(
                "coordination response has an invalid profile id",
            ));
        }
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
    let profile_id = match value.get("profile_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) if prefixed_id(id, "ap_") => Some(id.clone()),
        _ => {
            return Err(Error::rejected(
                "coordination response has an invalid profile id",
            ))
        }
    };
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
        unsupported_kind,
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
        profile_id,
        guidance: parse_guidance(value)?,
        reported_head_sha,
        text: optional_text(value, "text")?,
    })
}
