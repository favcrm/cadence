//! CAD-2215: the explicit operator initialization and lazy load of the
//! scoped-cursor codec.
//!
//! The keyring record is fixed at `<state-dir>/scoped-cursors.json` and is
//! read and proven for every handler access through
//! [`super::Shared::scoped_cursor_codec`], so a changed record, workspace or
//! keyring is never silently accepted. Nothing here generates keys, repairs
//! a ring or falls back to an empty codec: a missing record,
//! `connection_metadata.cursor_keyring_fingerprint = NULL`, unreadable
//! material or a fingerprint mismatch is one generic `unavailable` refusal.
//!
//! [`initialize`] is the only writer. It reads the fixed record, binds the
//! codec to this store's existing `workspace_id`, then either sets the NULL
//! fingerprint once or confirms the identical value. A different workspace,
//! key set or fingerprint refuses and is never rewritten.

use std::path::Path;

use serde_json::{json, Value};

use super::scoped_cursor::ScopedCursorCodec;
use crate::error::{Error, Result};
use crate::platform::scoped_cursor_config as record;
use crate::store::{CursorKeyringInit, Store};

pub(super) fn unavailable() -> Error {
    Error::invalid(
        "scoped_cursor_unavailable",
        "scoped cursor keyring unavailable",
    )
}

/// Load the fixed record, bind it to `workspace`, and prove the persisted
/// fingerprint. This never initializes: an unbound store refuses.
pub(crate) fn load_codec(
    state_dir: &Path,
    tracker_root: &Path,
    store: &Store,
) -> Result<ScopedCursorCodec> {
    let (workspace, fingerprint) = store.cursor_keyring_metadata().map_err(|_| unavailable())?;
    let fingerprint = fingerprint.ok_or_else(unavailable)?;
    let material = record::load(state_dir, Some(tracker_root))
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    let codec = ScopedCursorCodec::new(
        &workspace,
        material.keyring,
        material.active_key_id,
        &material.identity_key[..],
    )
    .map_err(|_| unavailable())?;
    codec
        .verify_keyring_fingerprint(&fingerprint)
        .map_err(|_| unavailable())?;
    Ok(codec)
}

/// The explicit `scoped_cursor_initialize` body: the operator answer names
/// whether the NULL fingerprint was bound or an identical value confirmed.
pub(crate) fn initialize(state_dir: &Path, tracker_root: &Path, store: &Store) -> Result<Value> {
    let (workspace, _) = store.cursor_keyring_metadata().map_err(|_| unavailable())?;
    let material = record::load(state_dir, Some(tracker_root))
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    let codec = ScopedCursorCodec::new(
        &workspace,
        material.keyring,
        material.active_key_id,
        &material.identity_key[..],
    )
    .map_err(|_| unavailable())?;
    let outcome = store
        .initialize_cursor_keyring(&workspace, codec.keyring_fingerprint())
        .map_err(|_| unavailable())?;
    Ok(json!({
        // `true` bound the NULL fingerprint; `false` confirmed the same
        // value idempotently.
        "initialized": matches!(outcome, CursorKeyringInit::Initialized),
        "fingerprint": codec.keyring_fingerprint(),
    }))
}
