//! CAD-1163: manual activation of an inactive portable root, execution
//! disabled.
//!
//! Activation keeps every imported actor disabled and every execution
//! worker suppressed. It persists the execution-mode receipt FIRST, then
//! replaces the inactive marker with an `activated` marker carrying the same
//! generation. A crash between the two leaves the inactive marker in place,
//! which still refuses startup. An `activated` marker without a matching
//! receipt refuses too, so deleting the receipt never re-enables a daemon.
//!
//! Authority: the daemon RPC (`portable_activate`) authorizes the request
//! with the operator connection and the same origin checks as replacement,
//! then calls [`activate`] with the canonical controller state. This module
//! never trusts a caller flag. The exact bound request is the target's full
//! inventory digest and generation, the package digest, `execution_mode =
//! "disabled"`, an empty binding list and the reconciliation list.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::portable::{verify_package_for_replace, INACTIVE_MARKER};
use super::portable_fence::{FENCE_FILE, FORMAT, MODE_DISABLED};
use super::portable_replace::{
    boundaries, lock_target, package_digest, replace_marker, snapshot_target, TargetSnapshot,
};
use crate::error::{Error, Result};

/// Marker state for an activated, execution-disabled root.
pub const ACTIVATED: &str = "activated_manual_disabled";

/// The closed non-terminal status sets, pinned to the literals the store's
/// own writers and terminal filters use (see `store/messages.rs`,
/// `store/plans.rs`, `store/monitors.rs`, `store/mod.rs::is_task_terminal`).
/// A row whose status is outside its set refuses the whole snapshot.
const KIND_STATES: &[(&str, &[&str])] = &[
    // messages: live = not completed|failed|interrupted|cancelled
    (
        "message",
        &["queued", "submitting", "running", "submitted", "unknown"],
    ),
    // jobs: non-terminal = not done|failed|cancelled
    ("job", &["draft", "open"]),
    // tasks: non-terminal = not verified|done|cancelled|failed
    (
        "task",
        &["draft", "revising", "dispatched", "running", "review"],
    ),
    // monitors: terminal = off
    ("monitor", &["active", "degraded"]),
];

/// The reconciliation kinds the operator may retain.
fn known_states(kind: &str) -> Option<&'static [&'static str]> {
    KIND_STATES
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, s)| *s)
}

/// Activation request. Parsed strictly by the RPC handler.
#[derive(Debug, Clone)]
pub struct ActivateRequest {
    pub destination: PathBuf,
    pub controller_state: PathBuf,
    pub expected_package_digest: String,
    pub expected_destination_digest: String,
    pub expected_generation: String,
    pub execution_mode: String,
    pub bindings: Vec<Value>,
    pub reconciliation: Vec<Reconciliation>,
}

/// One explicit reconciliation entry: a kind, its opaque ID and the status
/// the operator saw in the exact snapshot. Only `retain_inert` is accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    pub kind: String,
    pub id: String,
    pub status: String,
}

/// Parse the request's `reconciliation` array. Unknown fields fail.
pub fn parse_reconciliation(value: &Value) -> Result<Vec<Reconciliation>> {
    let rows = value
        .as_array()
        .ok_or_else(|| Error::rejected("'reconciliation' must be an array"))?;
    rows.iter()
        .map(|row| {
            let obj = row
                .as_object()
                .ok_or_else(|| Error::rejected("reconciliation entry must be an object"))?;
            if obj.len() != 4 || obj.get("action").and_then(Value::as_str) != Some("retain_inert") {
                return Err(Error::rejected(
                    "reconciliation entry must be exactly {kind,id,status,action:'retain_inert'}",
                ));
            }
            let text = |k: &str| -> Result<String> {
                obj.get(k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| Error::rejected(format!("reconciliation '{k}' is required")))
            };
            let kind = text("kind")?;
            if known_states(&kind).is_none() {
                return Err(Error::rejected(format!(
                    "reconciliation kind '{kind}' is not a known retainable kind"
                )));
            }
            Ok(Reconciliation {
                kind,
                id: text("id")?,
                status: text("status")?,
            })
        })
        .collect()
}

/// Refuse anything that is not exactly the supported first mode.
fn check_request(req: &ActivateRequest) -> Result<()> {
    if req.execution_mode != MODE_DISABLED {
        return Err(Error::rejected(
            "execution-enabled activation is not implemented; only execution_mode 'disabled' is supported",
        ));
    }
    if !req.bindings.is_empty() {
        return Err(Error::rejected(
            "execution bindings are not implemented; the bindings list must be empty for a disabled activation",
        ));
    }
    Ok(())
}

/// Activate `req.destination` as an execution-disabled root. The request
/// must already have been authorized by the live controller.
pub fn activate(source: &Path, req: &ActivateRequest) -> Result<Value> {
    check_request(req)?;
    let manifest_bytes = fs::read(source.join("manifest.json"))
        .map_err(|e| Error::rejected(format!("cannot read package manifest: {e}")))?;
    if format!("{:x}", Sha256::digest(&manifest_bytes)) != req.expected_package_digest {
        return Err(Error::rejected(
            "package digest does not match expected_package_digest; nothing was written",
        ));
    }
    if package_digest(source)? != req.expected_package_digest {
        return Err(Error::rejected(
            "package digest changed; nothing was written",
        ));
    }
    verify_package_for_replace(source)?;
    let (_controller, target) = boundaries(&req.controller_state, &req.destination, source)?;
    // Only a managed inactive root can be activated. The lock is held for
    // the whole transition; a running daemon holds it and refuses here.
    let _lock = lock_target(&target)?;
    let before = snapshot_target(&target)?;
    if before.digest != req.expected_destination_digest
        || before.generation != req.expected_generation
    {
        return Err(Error::rejected(
            "destination digest or generation does not match the request; a stale or replayed request is refused; nothing was written",
        ));
    }
    activate_in_place(
        &target,
        &before,
        &req.expected_package_digest,
        &req.reconciliation,
    )
}

/// AOS-339/CAD-1705: the activation itself, on a managed `committed_inactive`
/// root whose caller holds whatever authority the situation requires. The live
/// RPC passes a destination the operator named; the offline restore passes the
/// private stage it just built and has not published yet. The supplied plan
/// must equal the live required inventory before any receipt or marker write.
pub(crate) fn activate_in_place(
    target: &Path,
    before: &TargetSnapshot,
    expected_package_digest: &str,
    reconciliation: &[Reconciliation],
) -> Result<Value> {
    match_plan(&required_inventory(target)?, reconciliation)?;
    let marker = read_marker(target)?;
    // The marker's generation is the fresh, nonce-derived metadata written
    // by replacement. It is a different domain from the request's snapshot
    // generation (sha256 of the inventory digest), so it is checked for
    // shape only and never compared to `before.generation`.
    let marker_generation_ok = match marker.get("generation") {
        None => true,
        Some(value) => value.as_str().is_some_and(is_sha256_hex),
    };
    if marker["state"].as_str() != Some("committed_inactive") || !marker_generation_ok {
        return Err(Error::rejected(
            "destination is not a committed inactive root at this generation; nothing was written",
        ));
    }
    if target.join(FENCE_FILE).symlink_metadata().is_ok() {
        return Err(Error::rejected(
            "an execution-mode receipt already exists; nothing was written",
        ));
    }
    let generation = fresh_generation(&before.generation, expected_package_digest);
    // 1. Receipt first, fsynced, atomically renamed into place.
    write_receipt(target, &generation)?;
    // 2. Only then the activated marker. Same generation as the receipt.
    let activated = json!({
        "state": ACTIVATED,
        "format": marker["format"],
        "generation": generation,
        "replaces_generation": before.generation,
        "execution_mode": MODE_DISABLED,
        "bindings": [],
        "reconciliation": reconciliation.iter().map(|r| json!({
            "kind": r.kind, "id": r.id, "status": r.status, "action": "retain_inert"
        })).collect::<Vec<_>>(),
        "actors_enabled": false,
        "activation": "manual, execution disabled",
    });
    replace_marker(target, &activated)?;
    Ok(json!({
        "activated": true,
        "state": ACTIVATED,
        "execution_mode": MODE_DISABLED,
        "bindings": 0,
        "reconciliation_retained": reconciliation.len(),
        "generation": generation,
        "replaces_generation": before.generation,
        "destination": target,
        "actors_enabled": false,
        "pending_effects_replayed": 0,
    }))
}

/// Fresh, non-replayable generation: depends on the old generation, the
/// package and a random nonce, so an old expected generation never matches.
fn fresh_generation(old: &str, package: &str) -> String {
    let mut h = Sha256::new();
    h.update(uuid::Uuid::new_v4().simple().to_string().as_bytes());
    h.update(old.as_bytes());
    h.update(package.as_bytes());
    format!("{:x}", h.finalize())
}

/// A 64-char lowercase hex sha256, the only shape a marker generation takes.
fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn read_marker(target: &Path) -> Result<Value> {
    let text = fs::read_to_string(target.join(INACTIVE_MARKER))
        .map_err(|e| Error::rejected(format!("inactive marker unreadable: {e}")))?;
    serde_json::from_str(&text)
        .map_err(|e| Error::rejected(format!("inactive marker malformed: {e}")))
}

/// Write the receipt with a unique temp file, fsync, then rename. The
/// receipt is owner-only and the temp file is never adopted.
fn write_receipt(target: &Path, generation: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let body = serde_json::to_string_pretty(&json!({
        "format": FORMAT,
        "execution_mode": MODE_DISABLED,
        "generation": generation,
    }))?;
    let tmp = target.join(format!(
        ".{FENCE_FILE}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        file.write_all(body.as_bytes())?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.sync_all()?;
    }
    fs::rename(&tmp, target.join(FENCE_FILE)).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// The required reconciliation inventory: every selected non-terminal row of
/// the four kinds, read read-only from the target's store. A row whose status
/// is outside its kind's closed set refuses the whole inventory.
pub fn required_inventory(destination: &Path) -> Result<Vec<Reconciliation>> {
    let db = destination.join("cadence.sqlite3");
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| Error::rejected(format!("cannot open the target store read-only: {e}")))?;
    let mut rows = Vec::new();
    for (kind, table) in [
        ("message", "messages"),
        ("job", "jobs"),
        ("task", "tasks"),
        ("monitor", "monitors"),
    ] {
        let states = known_states(kind).expect("kind is in KIND_STATES");
        let mut stmt = conn
            .prepare(&format!("SELECT id, state FROM {table} ORDER BY id"))
            .map_err(|e| Error::rejected(format!("{table} unreadable: {e}")))?;
        let found = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| Error::rejected(format!("{table} unreadable: {e}")))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| Error::rejected(format!("{table} unreadable: {e}")))?;
        for (id, status) in found {
            if states.contains(&status.as_str()) {
                rows.push(Reconciliation {
                    kind: kind.into(),
                    id,
                    status,
                });
            } else if !is_terminal(kind, &status) {
                return Err(Error::rejected(format!(
                    "{kind} '{id}' has unknown status '{status}'; refusing the snapshot"
                )));
            }
        }
    }
    Ok(rows)
}

/// Terminal statuses per kind. A status here is history and needs no entry.
fn is_terminal(kind: &str, status: &str) -> bool {
    match kind {
        "message" => matches!(status, "completed" | "failed" | "interrupted" | "cancelled"),
        "job" => matches!(status, "done" | "failed" | "cancelled"),
        "task" => matches!(status, "verified" | "done" | "cancelled" | "failed"),
        "monitor" => status == "off",
        _ => false,
    }
}

/// The supplied plan must equal the required inventory exactly: same kind,
/// ID and current status, no duplicates, no extras, no missing rows.
pub fn match_plan(required: &[Reconciliation], supplied: &[Reconciliation]) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for row in supplied {
        if !seen.insert((row.kind.as_str(), row.id.as_str())) {
            return Err(Error::rejected(format!(
                "duplicate reconciliation entry for {} '{}'; nothing was written",
                row.kind, row.id
            )));
        }
        if !required.contains(row) {
            return Err(Error::rejected(format!(
                "reconciliation entry {} '{}' with status '{}' is not in the required inventory (fabricated, wrong status, or terminal); nothing was written",
                row.kind, row.id, row.status
            )));
        }
    }
    if let Some(missing) = required.iter().find(|r| !supplied.contains(r)) {
        return Err(Error::rejected(format!(
            "required {} '{}' with status '{}' is missing from the reconciliation plan; nothing was written",
            missing.kind, missing.id, missing.status
        )));
    }
    Ok(())
}
