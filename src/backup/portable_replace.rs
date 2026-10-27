//! CAD-1163: native-authorized replacement of an offline portable root.
//!
//! Authority is never inferred here. The daemon handler
//! (`src/daemon/portable_replace_rpc.rs`) runs `operator_connection` on the
//! live controller first, then canonicalises the request against the
//! controller's own `state_dir`, and only then mints a [`NativeOperatorProof`].
//! The proof is an opaque value bound to that canonical request; `replace`
//! rechecks every bound field before any write.
//!
//! Scope: only a managed portable root is replaceable. A managed root holds
//! `cadence.sqlite3`, the inactive marker, `cadence.lock` and the
//! `portable-tracker/` tree, and nothing else. Any other entry, and any other
//! layout, refuses as unsupported. The lock file is part of the managed root
//! and is created by the restore that publishes it, never by replacement, so
//! preflight and the in-operation inventory agree byte for byte.
//!
//! Order: verify package -> stage inactive members -> lock the target's
//! existing `cadence.lock` -> bind the full inventory -> write a fresh
//! generation marker into the stage (atomic replace) -> verify marker and
//! inventory -> revalidate the target -> exchange -> move the old root to a
//! private rollback sibling without overwrite -> verify it. The old root is
//! never deleted here, and nothing on a failure path touches it.

use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[cfg(not(target_os = "linux"))]
use super::portable::NO_ATOMIC_PUBLISH;
use super::portable::{stage_inactive_members, verify_package_for_replace, INACTIVE_MARKER};
use crate::error::{Error, Result};

/// Private rollback sibling prefix. Retained, never deleted here.
const ROLLBACK_PREFIX: &str = ".portable-rollback-";
const LOCK_FILE: &str = "cadence.lock";
const DB_FILE: &str = "cadence.sqlite3";
const TRACKER_DIR: &str = "portable-tracker";
const MANIFEST: &str = "manifest.json";

/// Native operator proof. Opaque: its only field is private, and it is minted
/// by [`NativeOperatorProof::mint`], which is `pub(crate)`. Call sites are
/// confined to the daemon handler module by convention and review, not by
/// the type system alone. The proof binds the exact canonical request it
/// was minted for; `replace_authorized` refuses any other request.
#[derive(Debug)]
pub struct NativeOperatorProof {
    bound: Bound,
}

/// The canonical tuple the proof attests. Compared field by field.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Bound {
    action: &'static str,
    source: PathBuf,
    destination: PathBuf,
    controller_state: PathBuf,
    expected_package_digest: String,
    expected_destination_digest: String,
    expected_generation: String,
}

impl NativeOperatorProof {
    /// Mint a proof for `req`. Crate-internal: the daemon handler calls this
    /// only after `operator_connection` has passed and `req` has been built
    /// from the controller's own canonical state dir.
    pub(crate) fn mint(req: &ReplaceRequest) -> Self {
        Self {
            bound: Bound {
                action: ACTION,
                source: req.source.clone(),
                destination: req.destination.clone(),
                controller_state: req.controller_state.clone(),
                expected_package_digest: req.expected_package_digest.clone(),
                expected_destination_digest: req.expected_destination_digest.clone(),
                expected_generation: req.expected_generation.clone(),
            },
        }
    }
}

const ACTION: &str = "replace";

/// The canonical request. `controller_state` is always the live controller's
/// own state dir, set by the handler, never a caller field.
#[derive(Debug, Clone)]
pub struct ReplaceRequest {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub controller_state: PathBuf,
    pub expected_package_digest: String,
    pub expected_destination_digest: String,
    pub expected_generation: String,
}

/// Full managed-root inventory. `digest` covers every entry: directories
/// (path, mode) and files (path, mode, size, sha256), in sorted order.
/// `generation` is the sha256 of `digest` and changes with every write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSnapshot {
    pub digest: String,
    pub generation: String,
    pub entries: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Dir(u32),
    File(u32, u64, String),
}

/// Read-only report of the values an operator copies into the request. It
/// carries no authority and changes nothing, and it uses the same inventory
/// the mutating path binds.
pub fn preflight(source: &Path, destination: &Path, controller_state: &Path) -> Result<Value> {
    let manifest_digest = package_digest(source)?;
    verify_package_for_replace(source)?;
    let (_, target) = boundaries(controller_state, destination, source)?;
    let snap = snapshot_target(&target)?;
    Ok(json!({
        "package_digest": manifest_digest,
        "destination": target,
        "destination_digest": snap.digest,
        "destination_generation": snap.generation,
        "destination_entries": snap.entries.len(),
    }))
}

/// sha256 of the exact `manifest.json` bytes: the package identity.
pub fn package_digest(source: &Path) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(&read_manifest_bytes(source)?)
    ))
}

fn read_manifest_bytes(source: &Path) -> Result<Vec<u8>> {
    let path = source.join(MANIFEST);
    let meta = path
        .symlink_metadata()
        .map_err(|e| Error::rejected(format!("cannot read package manifest: {e}")))?;
    if !meta.is_file() || meta.nlink() != 1 {
        return Err(Error::rejected(
            "package manifest is not a plain single-link file",
        ));
    }
    fs::read(&path).map_err(|e| Error::rejected(format!("cannot read package manifest: {e}")))
}

/// Replacement. The proof must bind exactly `req`; any mismatch refuses
/// before any write.
pub fn replace_authorized(proof: &NativeOperatorProof, req: &ReplaceRequest) -> Result<Value> {
    if proof.bound.action != ACTION
        || proof.bound
            != (Bound {
                action: ACTION,
                source: req.source.clone(),
                destination: req.destination.clone(),
                controller_state: req.controller_state.clone(),
                expected_package_digest: req.expected_package_digest.clone(),
                expected_destination_digest: req.expected_destination_digest.clone(),
                expected_generation: req.expected_generation.clone(),
            })
    {
        return Err(Error::rejected(
            "operator proof does not bind this request; nothing was written",
        ));
    }
    // Bind the package to the exact manifest bytes read here, and parse the
    // verified manifest from those same bytes. A source change between the
    // digest and verify steps cannot swap in an unapproved package.
    let manifest_bytes = read_manifest_bytes(&req.source)?;
    if format!("{:x}", Sha256::digest(&manifest_bytes)) != req.expected_package_digest {
        return Err(Error::rejected(
            "package digest does not match expected_package_digest; nothing was written",
        ));
    }
    let manifest = verify_package_for_replace(&req.source)?;
    let (_controller, target) = boundaries(&req.controller_state, &req.destination, &req.source)?;
    let parent = target
        .parent()
        .ok_or_else(|| Error::rejected("destination has no parent directory"))?
        .to_path_buf();
    // Refuse anything that is not a managed root before taking the lock.
    let _lock = lock_target(&target)?;
    let before = snapshot_target(&target)?;
    if before.digest != req.expected_destination_digest {
        return Err(Error::rejected(
            "destination digest does not match expected_destination_digest; nothing was written",
        ));
    }
    if before.generation != req.expected_generation {
        return Err(Error::rejected(
            "destination generation does not match expected_generation; a replayed or stale proof is refused; nothing was written",
        ));
    }
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let stage = parent.join(format!(".portable-replace-stage-{nonce}"));
    let rollback = parent.join(format!("{ROLLBACK_PREFIX}{nonce}"));
    fs::DirBuilder::new().mode(0o700).create(&stage)?;
    if let Err(error) = stage_inactive_members(&req.source, &stage, &manifest) {
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }
    // The fresh root is a managed root too: it carries its own stable,
    // zero-byte, owner-only daemon lock inode, created here in the private
    // stage. The live target's lock is never created, moved or unlinked.
    if let Err(error) = create_stage_lock(&stage) {
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }
    // Fresh generation, bound to the complete staged inventory before the
    // marker is written and to the old generation it replaces.
    let staged_members = snapshot_target_dir(&stage)?;
    let fresh = {
        let mut h = Sha256::new();
        h.update(nonce.as_bytes());
        h.update(staged_members.digest.as_bytes());
        h.update(before.generation.as_bytes());
        format!("{:x}", h.finalize())
    };
    let marker = json!({
        "state": "committed_inactive",
        "format": manifest.format,
        "generation": fresh,
        "replaces_generation": before.generation,
        "tracker_revision": manifest.tracker_revision,
        "schema_version": manifest.schema_version,
        "activation": "not performed; native operator admission required",
    });
    // Atomic replace of the marker inside the private stage: a unique temp
    // file, fsynced, then renamed onto the marker name. No create_new onto
    // the existing name, and no window in which the marker is missing.
    if let Err(error) = replace_marker(&stage, &marker) {
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }
    // Verify the new marker and the complete staged inventory (marker
    // included) before any exchange.
    let verified = snapshot_target_dir(&stage)?;
    if verified.generation == before.generation {
        let _ = fs::remove_dir_all(&stage);
        return Err(Error::rejected(
            "fresh generation equals the replaced generation; refusing to publish",
        ));
    }
    verify_marker(&stage, &fresh, &before.generation)?;
    // Revalidate the target: identical inventory and lock, or refuse.
    let now = snapshot_target(&target)?;
    if now != before {
        let _ = fs::remove_dir_all(&stage);
        return Err(Error::rejected(
            "target changed after it was bound; nothing was replaced",
        ));
    }
    if let Err(error) = exchange(&stage, &target) {
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }
    // After exchange `stage` names the OLD root. Move it to the rollback
    // sibling with a no-replace rename; never overwrite and never delete it.
    if let Err(error) = rename_noreplace(&stage, &rollback) {
        return Err(Error::internal(format!(
            "replaced, but the old root could not be retained at {}: {error}; it is still at {}; do not start a daemon on this state dir until resolved",
            rollback.display(),
            stage.display()
        )));
    }
    let retained = snapshot_target(&rollback)?;
    if retained.digest != before.digest {
        return Err(Error::internal(format!(
            "replaced, but the retained rollback at {} does not match its verified digest; do not delete it",
            rollback.display()
        )));
    }
    let after = snapshot_target(&target)?;
    Ok(json!({
        "replaced": true,
        "state": "committed_inactive",
        "destination": target,
        "generation": after.generation,
        "replaces_generation": before.generation,
        "rollback": {
            "path": rollback,
            "digest": before.digest,
            "entries": retained.entries.len(),
            "mode": "owner-only tree; retained and never deleted automatically",
        },
        "actors_enabled": false,
        "pending_messages_consumed": 0,
    }))
}

/// Canonicalise every path, refuse symlink components, and enforce the
/// overlap rules: destination, controller state and source must be pairwise
/// disjoint, and the destination must not sit inside the source package or
/// the tracker. The controller state is always the one passed in by the
/// handler, which uses its own `state_dir`.
pub(crate) fn boundaries(
    controller: &Path,
    target: &Path,
    source: &Path,
) -> Result<(PathBuf, PathBuf)> {
    let controller = canonical_dir(controller, "controller state dir")?;
    let source = canonical_dir(source, "source package")?;
    let parent = target
        .parent()
        .ok_or_else(|| Error::rejected("destination has no parent directory"))?;
    let parent = canonical_dir(parent, "destination parent")?;
    let name = target
        .file_name()
        .ok_or_else(|| Error::rejected("destination has no name"))?;
    let target = parent.join(name);
    if target.symlink_metadata().is_ok() {
        let meta = target.symlink_metadata()?;
        if meta.file_type().is_symlink() {
            return Err(Error::rejected("destination is a symlink"));
        }
    }
    let overlaps = |a: &Path, b: &Path| a == b || a.starts_with(b) || b.starts_with(a);
    if overlaps(&target, &controller) {
        return Err(Error::rejected(
            "destination equals, contains or lies inside the controller state dir; refusing",
        ));
    }
    if overlaps(&target, &source) {
        return Err(Error::rejected(
            "destination equals, contains or lies inside the source package; refusing",
        ));
    }
    if overlaps(&controller, &source) {
        return Err(Error::rejected(
            "controller state dir and source package overlap; refusing",
        ));
    }
    if target.join(TRACKER_DIR).starts_with(&source) || source.starts_with(target.join(TRACKER_DIR))
    {
        return Err(Error::rejected(
            "source package lies inside the destination tracker; refusing",
        ));
    }
    Ok((controller, target))
}

fn canonical_dir(path: &Path, what: &str) -> Result<PathBuf> {
    let meta = path
        .symlink_metadata()
        .map_err(|e| Error::rejected(format!("{what}: {e}")))?;
    if meta.file_type().is_symlink() {
        return Err(Error::rejected(format!("{what} is a symlink")));
    }
    let canon = path
        .canonicalize()
        .map_err(|e| Error::rejected(format!("{what}: {e}")))?;
    if !canon.is_dir() {
        return Err(Error::rejected(format!("{what} is not a directory")));
    }
    Ok(canon)
}

/// CAD-1705: canonicalise an offline activation target against its source
/// package and refuse any overlap, so the restore can never stage into, or
/// publish over, the package it reads.
pub(crate) fn offline_activation_target(source: &Path, target: &Path) -> Result<PathBuf> {
    let source = canonical_dir(source, "source package")?;
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = canonical_dir(parent, "destination parent")?;
    let name = target
        .file_name()
        .ok_or_else(|| Error::rejected("destination has no name"))?;
    let target = parent.join(name);
    let overlaps = |a: &Path, b: &Path| a == b || a.starts_with(b) || b.starts_with(a);
    if overlaps(&target, &source) {
        return Err(Error::rejected(
            "destination equals, contains or lies inside the source package; refusing",
        ));
    }
    Ok(target)
}

/// Full inventory of a managed root: every directory and file. Refuses any
/// symlink, hardlink, foreign owner, non-0700 directory, or unknown entry.
/// The managed top layout is fixed (see [`managed_top`]); nothing else is
/// accepted at the top level.
pub fn snapshot_target(root: &Path) -> Result<TargetSnapshot> {
    let snap = snapshot_target_dir(root)?;
    require_managed_layout(&snap)?;
    Ok(snap)
}

pub(crate) fn snapshot_target_dir(root: &Path) -> Result<TargetSnapshot> {
    let meta = root
        .symlink_metadata()
        .map_err(|e| Error::rejected(format!("cannot stat {}: {e}", root.display())))?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(Error::rejected(format!(
            "{} is not a plain directory",
            root.display()
        )));
    }
    let uid = unsafe { libc::geteuid() };
    if meta.uid() != uid || (meta.mode() & 0o777) != 0o700 {
        return Err(Error::rejected(format!(
            "{} must be owned by the operator and mode 0700",
            root.display()
        )));
    }
    let mut entries = BTreeMap::new();
    walk(root, root, uid, &mut entries)?;
    let mut h = Sha256::new();
    for (rel, entry) in &entries {
        match entry {
            Entry::Dir(mode) => h.update(format!("d\0{rel}\0{mode:o}\n").as_bytes()),
            Entry::File(mode, size, sha) => {
                h.update(format!("f\0{rel}\0{mode:o}\0{size}\0{sha}\n").as_bytes())
            }
        }
    }
    let digest = format!("{:x}", h.finalize());
    let generation = format!("{:x}", Sha256::digest(digest.as_bytes()));
    Ok(TargetSnapshot {
        digest,
        generation,
        entries,
    })
}

fn walk(root: &Path, dir: &Path, uid: u32, out: &mut BTreeMap<String, Entry>) -> Result<()> {
    let mut children: Vec<_> = fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
    children.sort_by_key(|e| e.file_name());
    for child in children {
        let path = child.path();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let meta = path.symlink_metadata()?;
        if meta.file_type().is_symlink() {
            return Err(Error::rejected(format!("{rel} is a symlink")));
        }
        if meta.uid() != uid {
            return Err(Error::rejected(format!("{rel} is foreign-owned")));
        }
        let mode = meta.mode() & 0o777;
        if meta.is_dir() {
            if mode != 0o700 {
                return Err(Error::rejected(format!("{rel} is not mode 0700")));
            }
            out.insert(rel.clone(), Entry::Dir(mode));
            walk(root, &path, uid, out)?;
        } else if meta.is_file() {
            if meta.nlink() != 1 {
                return Err(Error::rejected(format!("{rel} is hardlinked")));
            }
            let (sha, size) = hash_file(&path)?;
            out.insert(rel, Entry::File(mode, size, sha));
        } else {
            return Err(Error::rejected(format!("{rel} is not a regular file")));
        }
    }
    Ok(())
}

/// Fixed managed layout. A managed root holds exactly these entries at the
/// top level: the core store and its sidecars, the inactive marker, the lock
/// file, and the tracker tree. Anything else is unsupported.
fn require_managed_layout(snap: &TargetSnapshot) -> Result<()> {
    let unsupported = || {
        Error::rejected(
            "unsupported destination layout for replacement: only a managed portable root (cadence.sqlite3, the inactive marker, cadence.lock and portable-tracker/) can be replaced; nothing was written",
        )
    };
    for rel in snap.entries.keys() {
        let top = rel.split('/').next().unwrap_or(rel);
        let ok = matches!(top, DB_FILE | LOCK_FILE | TRACKER_DIR)
            || top == INACTIVE_MARKER
            || top == crate::backup::portable_fence::FENCE_FILE
            || top.starts_with("cadence.sqlite3-");
        if !ok {
            return Err(unsupported());
        }
    }
    let has = |name: &str| snap.entries.contains_key(name);
    if !(has(DB_FILE) && has(INACTIVE_MARKER) && has(LOCK_FILE)) {
        return Err(unsupported());
    }
    Ok(())
}

/// Lock the managed root's existing `cadence.lock`. Replacement never creates
/// it: a root without a lock file is not managed and refuses. The inode is
/// the one the daemon takes, so holding it refuses a daemon start.
pub(crate) fn lock_target(target: &Path) -> Result<File> {
    let path = target.join(LOCK_FILE);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|e| {
            Error::rejected(format!(
                "cannot open the managed lock {} ({e}); a destination without its lock is not a managed root",
                path.display()
            ))
        })?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err(Error::rejected(format!(
            "a cadence daemon is running on {} (it holds cadence.lock); refusing to replace it",
            target.display()
        )));
    }
    Ok(file)
}

/// Create the stage's `cadence.lock` once, zero bytes, mode 0600, with
/// `O_EXCL` so it can never adopt an existing inode.
pub(crate) fn create_stage_lock(stage: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(stage.join(LOCK_FILE))?;
    file.sync_all()?;
    Ok(())
}

/// Write `marker` into the private stage by a unique temp file, fsync, and an
/// atomic rename onto the marker name. The marker is never absent.
pub(crate) fn replace_marker(stage: &Path, marker: &Value) -> Result<()> {
    let tmp = stage.join(format!(
        ".{INACTIVE_MARKER}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        file.write_all(serde_json::to_string_pretty(marker)?.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&tmp, stage.join(INACTIVE_MARKER)).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// The staged marker must carry the fresh generation and the replaced one.
fn verify_marker(stage: &Path, fresh: &str, replaced: &str) -> Result<()> {
    let text = fs::read_to_string(stage.join(INACTIVE_MARKER))
        .map_err(|e| Error::rejected(format!("staged marker unreadable: {e}")))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| Error::rejected(format!("staged marker malformed: {e}")))?;
    if value["generation"].as_str() != Some(fresh)
        || value["replaces_generation"].as_str() != Some(replaced)
        || value["state"].as_str() != Some("committed_inactive")
    {
        return Err(Error::rejected(
            "staged marker does not carry the fresh generation; nothing was published",
        ));
    }
    Ok(())
}

/// Atomic two-path exchange. `RENAME_EXCHANGE` comes from the pinned libc.
#[cfg(target_os = "linux")]
fn exchange(a: &Path, b: &Path) -> Result<()> {
    rename2(a, b, libc::RENAME_EXCHANGE)
}

/// Rename with `RENAME_NOREPLACE`: fails instead of overwriting a foreign
/// directory at the destination.
#[cfg(target_os = "linux")]
fn rename_noreplace(src: &Path, dst: &Path) -> Result<()> {
    rename2(src, dst, libc::RENAME_NOREPLACE)
}

/// Other platforms refuse before anything is exchanged or published.
#[cfg(not(target_os = "linux"))]
fn exchange(_a: &Path, _b: &Path) -> Result<()> {
    Err(Error::rejected(NO_ATOMIC_PUBLISH))
}

#[cfg(not(target_os = "linux"))]
fn rename_noreplace(_src: &Path, _dst: &Path) -> Result<()> {
    Err(Error::rejected(NO_ATOMIC_PUBLISH))
}

#[cfg(target_os = "linux")]
fn rename2(src: &Path, dst: &Path, flags: libc::c_uint) -> Result<()> {
    let s =
        CString::new(src.as_os_str().as_bytes()).map_err(|_| Error::rejected("path has NUL"))?;
    let d =
        CString::new(dst.as_os_str().as_bytes()).map_err(|_| Error::rejected("path has NUL"))?;
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            s.as_ptr(),
            libc::AT_FDCWD,
            d.as_ptr(),
            flags,
        )
    };
    if rc != 0 {
        return Err(Error::rejected(format!(
            "rename of {} to {} failed: {}",
            src.display(),
            dst.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), total))
}
