//! CAD-2215: the one fixed owner-selected, path-only keyring record for the
//! scoped-reader cursor codec.
//!
//! The record is exactly `<state-dir>/scoped-cursors.json`; a caller never
//! supplies a path or an identity. It names raw 32-byte key files by
//! absolute path, and this module never creates, repairs, rotates or
//! generates anything. Every admitted file is opened descriptor-relative
//! with [`super::encrypted_fs::owner_file`] conventions: owner 0600, one
//! link, a regular file, no symlink in the resolved chain, and an
//! owner-only 0700 parent. Key bytes only ever live in zeroizing buffers and
//! are never formatted, logged or persisted in the record.
//!
//! Key files inside the state directory or inside the tracker are refused:
//! a store backup or a tracked project must never carry plaintext key
//! material. Missing configuration is `Ok(None)` so an ordinary daemon stays
//! healthy without it; a present-but-invalid record is the same generic
//! refusal as an unusable key.

use super::encrypted_fs::{owner_file, Directories};
use crate::daemon::scoped_cursor::{CursorKeyId, CursorKeyring, KEY_BYTES, KEY_ID_BYTES};
use crate::error::{Error, Result};
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use zeroize::Zeroizing;

/// The one fixed record name below the state directory.
pub(crate) const RECORD: &str = "scoped-cursors.json";
/// Bounded record size: a few dozen keys are far below this.
const LIMIT: u64 = 64 * 1024;

fn refused() -> Error {
    Error::invalid(
        "scoped_cursor_unavailable",
        "scoped cursor keyring unavailable",
    )
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    identity_key_file: PathBuf,
    active_key_id: String,
    keys: Vec<KeyEntry>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyEntry {
    id: String,
    key_file: PathBuf,
}

/// The admitted material, still unbound to a workspace. The caller builds
/// the codec from its own `connection_metadata.workspace_id`; the identity
/// key stays zeroizing even here.
pub(crate) struct Material {
    pub(crate) keyring: CursorKeyring,
    pub(crate) active_key_id: CursorKeyId,
    pub(crate) identity_key: Zeroizing<[u8; KEY_BYTES]>,
}

fn state_absolute(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(|_| refused())?.join(path)
    };
    if path
        .components()
        .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err(refused());
    }
    Ok(path)
}

/// A key path is accepted only as an already-absolute name with no `.`/`..`
/// component: the record is reproducible, not relative to some cwd. Reserved
/// artifact tree components are refused exactly as the encrypted-config
/// record refuses them, so a key can never be published inside a bundle.
fn key_absolute(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err(refused());
    }
    if path.components().any(|c| {
        matches!(c, Component::Normal(part)
            if matches!(part.to_str(), Some("backups" | "exports" | "app-records" | "workspace-apps")))
    }) {
        return Err(refused());
    }
    Ok(path.to_path_buf())
}

fn private_directory(dirs: &Directories) -> Result<()> {
    let meta = dirs.last().metadata().map_err(|_| refused())?;
    if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o7777 != 0o700 {
        return Err(refused());
    }
    Ok(())
}

/// An existing tracker root is the same owner-controlled, symlink-free chain
/// key admission requires: pin it and recheck before any overlap comparison,
/// so a symlinked tracker cannot point at the key directory and fold the keys
/// into the tracked/portable tree. A missing tracker is the ordinary absent
/// case; every other inspection failure refuses.
fn admit_tracker(root: &Path) -> Result<PathBuf> {
    let root = state_absolute(root)?;
    match std::fs::symlink_metadata(&root) {
        Ok(_) => {
            let (pinned, _) = Directories::open(&root, false).map_err(|_| refused())?;
            pinned.check().map_err(|_| refused())?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(refused()),
    }
    Ok(root)
}

/// Key files must live outside the state directory and tracker: neither
/// backups nor tracked projects may carry plaintext key material.
fn refuse_location(path: &Path, state: &Path, tracker: Option<&Path>) -> Result<()> {
    if path.starts_with(state) || tracker.is_some_and(|tracker| path.starts_with(tracker)) {
        return Err(refused());
    }
    Ok(())
}

/// Exactly 32 raw bytes from an owner-only, single-link regular file.
fn read_secret(path: &Path) -> Result<Zeroizing<[u8; KEY_BYTES]>> {
    let file = owner_file(path).map_err(|_| refused())?;
    if file.metadata().map_err(|_| refused())?.len() != KEY_BYTES as u64 {
        return Err(refused());
    }
    let mut bytes = Zeroizing::new([0u8; KEY_BYTES]);
    let mut handle = &file;
    handle.read_exact(&mut bytes[..]).map_err(|_| refused())?;
    // The length check above can race a rewrite; an extra byte proves the
    // file is not exactly the admitted key.
    let mut extra = [0u8; 1];
    if handle.read(&mut extra).map_err(|_| refused())? != 0 {
        return Err(refused());
    }
    Ok(bytes)
}

/// Exactly 32 lowercase hex characters.
fn parse_key_id(text: &str) -> Option<CursorKeyId> {
    let bytes = text.as_bytes();
    if bytes.len() != KEY_ID_BYTES * 2 {
        return None;
    }
    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        }
    }
    let mut id = [0u8; KEY_ID_BYTES];
    for (slot, pair) in id.iter_mut().zip(bytes.as_chunks::<2>().0) {
        *slot = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    CursorKeyId::from_bytes(&id).ok()
}

/// Read the fixed record if it exists, admitting every key it names.
///
/// `Ok(None)` means no record: the daemon starts and ordinary operator
/// features work, while the scoped codec stays unavailable. A present but
/// unreadable, malformed, duplicated, unsafe or unavailable record refuses.
pub(crate) fn load(state_dir: &Path, tracker_root: Option<&Path>) -> Result<Option<Material>> {
    let state_path = state_absolute(state_dir)?;
    match std::fs::symlink_metadata(&state_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(refused()),
        Ok(_) => {}
    }
    // Probe with the legacy policy first: an absent record must not turn a
    // working daemon's non-private state dir into a startup failure.
    let (probe, _) = Directories::legacy_metadata(&state_path).map_err(|_| refused())?;
    if probe
        .private_record(OsStr::new(RECORD))
        .map_err(|_| refused())?
        .is_none()
    {
        return Ok(None);
    }
    drop(probe);
    // A present record always receives the strict private chain.
    let (state, _) = Directories::open(&state_path, false).map_err(|_| refused())?;
    private_directory(&state)?;
    let file = state
        .private_record(OsStr::new(RECORD))
        .map_err(|_| refused())?
        .ok_or_else(refused)?;
    if file.metadata().map_err(|_| refused())?.len() > LIMIT {
        return Err(refused());
    }
    let mut text = Vec::new();
    (&file)
        .take(LIMIT + 1)
        .read_to_end(&mut text)
        .map_err(|_| refused())?;
    if text.len() as u64 > LIMIT {
        return Err(refused());
    }
    drop(file);
    state.check().map_err(|_| refused())?;

    let record: Record = serde_json::from_slice(&text).map_err(|_| refused())?;
    if record.version != 1 || record.keys.is_empty() {
        return Err(refused());
    }
    let tracker = match tracker_root {
        Some(root) => Some(admit_tracker(root)?),
        None => None,
    };
    let identity_path = key_absolute(&record.identity_key_file)?;
    refuse_location(&identity_path, &state_path, tracker.as_deref())?;
    let active_key_id = parse_key_id(&record.active_key_id).ok_or_else(refused)?;

    let mut keyring = CursorKeyring::new();
    let mut seen = Vec::with_capacity(record.keys.len());
    for entry in &record.keys {
        let id = parse_key_id(&entry.id).ok_or_else(refused)?;
        let path = key_absolute(&entry.key_file)?;
        refuse_location(&path, &state_path, tracker.as_deref())?;
        if path == identity_path || seen.iter().any(|other| other == &path) {
            return Err(refused());
        }
        seen.push(path.clone());
        let key = read_secret(&path)?;
        keyring.insert_key(id, &key[..]).map_err(|_| refused())?;
    }
    let identity_key = read_secret(&identity_path)?;
    Ok(Some(Material {
        keyring,
        active_key_id,
        identity_key,
    }))
}
