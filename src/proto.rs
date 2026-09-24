//! Wire protocol shared by the daemon and the CLI client.
//!
//! One JSON object per line over a Unix-domain socket. Each request is
//! `{"method": ..., "params": {...}}`; each response is either
//! `{"ok": true, "result": ...}` or `{"ok": false, "error": {"kind", "message"}}`.
//! See docs/PROTOCOL.md for the full contract.

use serde_json::{json, Value};

use crate::error::{Error, Result};

pub const PROTOCOL_VERSION: u32 = 1;

/// Server capabilities reported by `health` — generated from the
/// endpoint registry plus daemon-level features, never hand-listed.
pub use crate::adapter::registry::capabilities;

pub fn ok(result: Value) -> Value {
    json!({"ok": true, "result": result})
}

pub fn err(error: &Error) -> Value {
    let mut body = json!({"kind": error.kind(), "message": error.to_string()});
    if let Some(code) = error.code() {
        body["code"] = json!(code);
    }
    if let Some(revision) = error.revision() {
        body["revision"] = json!(revision);
    }
    json!({"ok": false, "error": body})
}

/// Extract the `result` field or convert an error frame into [`Error`].
pub fn unwrap(frame: Value) -> Result<Value> {
    if frame.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(frame.get("result").cloned().unwrap_or(Value::Null));
    }
    let kind = frame
        .pointer("/error/kind")
        .and_then(Value::as_str)
        .unwrap_or("internal");
    let message = frame
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("unspecified error")
        .to_string();
    let code = frame
        .pointer("/error/code")
        .and_then(Value::as_str)
        .map(str::to_string);
    let revision = frame.pointer("/error/revision").and_then(Value::as_i64);
    if kind == "conflict" || code.is_some() {
        return Err(Error::Structured(crate::error::Structured {
            kind: if kind == "conflict" {
                "conflict"
            } else {
                "rejected"
            },
            code: code.unwrap_or_else(|| "rejected".to_string()),
            message,
            revision,
        }));
    }
    Err(match kind {
        "rejected" => Error::Rejected(message),
        "provider" => Error::Provider(message),
        "unknown" => Error::OutcomeUnknown(message),
        _ => Error::Internal(message),
    })
}

pub fn request(method: &str, params: Value) -> Value {
    json!({"method": method, "params": params})
}

/// Validate a params key: `[a-z0-9_-]` like identifiers plus
/// underscores (`auto_ready`, `reply_to`). Keys are never used in
/// filenames, so the wider charset is safe.
pub fn param_key(value: &str) -> Result<String> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && value.chars().next().is_some_and(|c| c != '-' && c != '_');
    if valid {
        Ok(value.to_string())
    } else {
        Err(Error::rejected(
            "Params key must be 1-64 lowercase letters, digits, hyphens or underscores and not start with '-' or '_'",
        ))
    }
}

/// Validate an agent/message identifier: 1-64 chars of `[a-z0-9-]`,
/// starting with a letter or digit.
pub fn identifier(value: &str, what: &str) -> Result<String> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && value.chars().next().is_some_and(|c| c != '-');
    if valid {
        Ok(value.to_string())
    } else {
        Err(Error::rejected(format!(
            "{what} must be 1-64 lowercase letters, digits or hyphens and not start with '-'"
        )))
    }
}

/// The message-id prefix only the daemon writes (CAD-445): a message
/// the daemon originates — a master wake, and any later system message
/// — is queued under `sys-<kind>-<hash>`, and the hash is of a key a
/// caller can often predict (`blocker_done/D-3/D-2`). A caller-supplied
/// id with this prefix is refused ([`caller_message_id`]), so no agent
/// can squat the id and suppress the daemon's message as a duplicate.
pub const DAEMON_MESSAGE_PREFIX: &str = "sys-";

/// The id of the daemon-originated message of `kind` for `key` — the
/// dedupe key: the same `(kind, key)` always maps to the same id, so a
/// replay or a restart finds the message already queued.
pub fn daemon_message_id(kind: &str, key: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash: String = Sha256::digest(format!("{kind}\n{key}").as_bytes())
        .iter()
        .take(10)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{DAEMON_MESSAGE_PREFIX}{kind}-{hash}")
}

/// Refuse a caller-supplied message id in the daemon's namespace
/// ([`DAEMON_MESSAGE_PREFIX`]).
pub fn caller_message_id(id: &str) -> Result<()> {
    if id.starts_with(DAEMON_MESSAGE_PREFIX) {
        return Err(Error::rejected(format!(
            "message ids starting '{DAEMON_MESSAGE_PREFIX}' are the daemon's own — pick another \
             id, or omit it"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_message_ids_are_stable_valid_and_reserved() {
        let a = daemon_message_id("wake", "blocker_done/D-3/D-2");
        assert_eq!(a, daemon_message_id("wake", "blocker_done/D-3/D-2"));
        assert_ne!(a, daemon_message_id("wake", "blocker_done/D-4/D-2"));
        assert_ne!(a, daemon_message_id("answer", "blocker_done/D-3/D-2"));
        assert!(a.starts_with("sys-wake-"), "{a}");
        identifier(&a, "Message id").unwrap();
        assert!(caller_message_id(&a).is_err());
        caller_message_id("dispatch-d-3").unwrap();
    }
}
