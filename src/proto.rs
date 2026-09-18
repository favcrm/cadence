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
    json!({"ok": false, "error": {"kind": error.kind(), "message": error.to_string()}})
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
