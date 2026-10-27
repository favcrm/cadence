//! CAD-1163: persistent manual-execution fence.
//!
//! A portable activation in `execution_mode = "disabled"` leaves a receipt
//! beside the managed root. Its presence is the durable statement that the
//! daemon must start with NO execution-capable work: no actor launch, no
//! relaunch, no monitor/stall/WAL/slot/report-router/publish worker. The
//! receipt is read before the store opens and after the singleton lock, and
//! it is only ever written by the activation transition, never by a caller
//! flag or an imported `enabled` value.
//!
//! Loading is closed: a missing receipt is the normal, unchanged daemon; any
//! other shape (unreadable, malformed, unknown mode, wrong format) refuses.

use std::fs;
use std::path::Path;

use std::os::unix::fs::MetadataExt;

use serde_json::Value;

use super::portable_activate::ACTIVATED;

use crate::error::{Error, Result};

/// Receipt file name inside the managed state dir.
pub const FENCE_FILE: &str = "portable-execution-mode.json";
/// The only mode a manual activation can persist.
pub const MODE_DISABLED: &str = "disabled";
pub const FORMAT: &str = "cadence.portable-execution-mode/1";

/// The verified, immutable mode of a state dir at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// No receipt: normal daemon behaviour.
    Normal,
    /// A verified `disabled` receipt: manual, execution-fenced startup.
    ManualDisabled,
}

impl Mode {
    /// True when the execution fence is in force.
    pub fn fenced(self) -> bool {
        self == Mode::ManualDisabled
    }
}

/// Load the state dir's execution mode. The receipt and the inactive marker
/// are checked together, so neither can be dropped to start the daemon:
///
/// - no marker, no receipt: `Normal` (the unchanged daemon);
/// - a `committed_inactive` marker: refuse (the caller refuses start);
/// - an `activated` marker: the receipt must exist, parse, and carry the
///   marker's generation, or the daemon refuses;
/// - a receipt without an activated marker, or any malformed shape: refuse.
pub fn load(state_dir: &Path) -> Result<Mode> {
    let receipt = read_receipt(state_dir)?;
    let marker = state_dir.join(crate::backup::portable::INACTIVE_MARKER);
    let marker_state = match fs::read_to_string(&marker) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(refuse(format!("cannot read {}: {e}", marker.display()))),
        Ok(text) => {
            let value: Value = serde_json::from_str(&text)
                .map_err(|_| refuse("inactive marker is malformed; refusing to start".into()))?;
            Some(value)
        }
    };
    match (marker_state, receipt) {
        (None, None) => Ok(Mode::Normal),
        (None, Some(_)) => Err(refuse(
            "execution-mode receipt exists without an activated marker; refusing to start".into(),
        )),
        (Some(value), receipt) if value["state"].as_str() == Some(ACTIVATED) => {
            let generation = value["generation"].as_str().unwrap_or_default();
            match receipt {
                Some(r) if !generation.is_empty() && r.generation == generation => {
                    Ok(Mode::ManualDisabled)
                }
                _ => Err(refuse(
                    "activated root has no matching execution-mode receipt; refusing to start (never falls back to a normal daemon)".into(),
                )),
            }
        }
        (Some(_), _) => Err(refuse(
            "portable restore marker is present; activate it through the native operator path before starting the daemon".into(),
        )),
    }
}

/// Read the receipt, if any. Present but malformed refuses.
fn read_receipt(state_dir: &Path) -> Result<Option<Receipt>> {
    let path = state_dir.join(FENCE_FILE);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(refuse(format!("cannot read {}: {e}", path.display()))),
        Ok(meta)
            if !meta.is_file()
                || meta.nlink() != 1
                || meta.uid() != unsafe { libc::geteuid() }
                || meta.mode() & 0o777 != 0o600 =>
        {
            Err(refuse(format!(
                "{} is not an owner-private (0600) plain single-link file",
                path.display()
            )))
        }
        Ok(_) => {
            let text = fs::read_to_string(&path)
                .map_err(|e| refuse(format!("cannot read {}: {e}", path.display())))?;
            parse(&text).map(Some)
        }
    }
}

/// A verified receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub generation: String,
}

/// Parse a receipt. Closed: exact format, exact mode, no other keys.
fn parse(text: &str) -> Result<Receipt> {
    let value: Value = serde_json::from_str(text)
        .map_err(|e| refuse(format!("execution-mode receipt is malformed: {e}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| refuse("execution-mode receipt is not an object".to_string()))?;
    if obj.len() != 3
        || value["format"].as_str() != Some(FORMAT)
        || value["execution_mode"].as_str() != Some(MODE_DISABLED)
        || value["generation"].as_str().is_none_or(str::is_empty)
    {
        return Err(refuse(
            "execution-mode receipt has an unknown mode or shape; refusing to start".to_string(),
        ));
    }
    Ok(Receipt {
        generation: value["generation"].as_str().unwrap_or_default().to_string(),
    })
}

/// Refusal raised by the fence: a state dir in manual mode refuses an
/// execution-capable request before any write.
pub fn fence_refusal(method: &str) -> Error {
    refuse(format!(
        "{method} is execution-capable and this state dir is in manual execution-disabled mode (portable activation); nothing was started or written"
    ))
}

fn refuse(message: String) -> Error {
    Error::rejected(message)
}

/// Methods a manual execution-disabled daemon still serves. Closed and
/// read-only, plus `shutdown`. Every other method, including every
/// execution-capable or unknown future method, refuses before it runs.
const MANUAL_ALLOWED: &[&str] = &[
    "health",
    "daemon_info",
    "shutdown",
    "update_status",
    "model_defaults_get",
    "agent_list",
    "agent_show",
    "monitor_list",
    "monitor_show",
    "job_show",
    "task_show",
];

/// True when `method` may run while the execution fence is in force.
pub fn allows(method: &str) -> bool {
    MANUAL_ALLOWED.contains(&method)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"format":"cadence.portable-execution-mode/1","execution_mode":"disabled","generation":"g1"}"#;

    #[test]
    fn disabled_receipt_is_the_only_fenced_mode() {
        assert_eq!(parse(GOOD).unwrap().generation, "g1");
        assert!(parse(&GOOD.replace("disabled", "enabled")).is_err());
        assert!(parse(&GOOD.replace("disabled", "")).is_err());
        assert!(parse("{}").is_err());
        assert!(parse("not json").is_err());
        assert!(parse(&GOOD.replace('}', ",\"extra\":1}")).is_err());
    }

    #[test]
    fn manual_allowlist_is_closed() {
        assert!(allows("health"));
        assert!(!allows("approval_state"));
        assert!(!allows("agent_wait"));
        assert!(!allows("agent_events"));
        assert!(!allows("job_events"));
        assert!(allows("shutdown"));
        assert!(!allows("agent_resume"));
        assert!(!allows("agent_add"));
        assert!(!allows("task_dispatch"));
        assert!(!allows("message_read"));
        assert!(!allows("agent_send"));
        assert!(!allows("some_future_method"));
    }
}
