//! Read-only package verification, transfer manifest admission, private snapshot streaming, and `PackageRoot` FD-anchored traversal.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::classify::{check_tracker_private_content, hex_digest, pm_root_from_paths};
use super::restore::validate_staged_store;
use super::tracker::{check_tracker_content, check_tracker_path, pm_root_of};
use super::{
    safe_relative, write_private_mode, Member, PortableManifest, PKG_DB, PKG_MANIFEST, PKG_TRACKER,
    PORTABLE_FORMAT,
};
use crate::error::{Error, Result};
use crate::fd_fs::DirFd;

/// Read-only inspection through the same integrity, catalog and authority
/// checks as inactive restore. A valid manifest is not source authenticity,
/// present-day source fidelity, or destination/import admission.
/// A portable package that passed every inspection check: the verified
/// manifest, the exact manifest bytes and their sha256, and the inspection
/// report `inspect-portable` prints. Transfer callers bind the digest of
/// these exact bytes — the file is hashed once, never re-read.
pub struct VerifiedPackage {
    pub manifest: PortableManifest,
    pub manifest_bytes: Vec<u8>,
    pub manifest_sha256: String,
    pub report: Value,
    /// The private copy every verified member was written to and re-read from.
    /// Consumers must open or stream members from here, never from the mutable
    /// caller-supplied package.
    snapshot: tempfile::TempDir,
}

impl VerifiedPackage {
    /// Root of the private verified snapshot. Same-UID processes are not
    /// isolated from it.
    pub fn snapshot_root(&self) -> &Path {
        self.snapshot.path()
    }
}

/// `inspect_portable` plus the verified package internals a remote transfer
/// needs (the exact manifest bytes and member list). One verification pass.
pub fn verify_transfer_package(source: &Path) -> Result<VerifiedPackage> {
    let inspected = inspect_package(source)?;
    if inspected.manifest_bytes.len() > TRANSFER_MANIFEST_MAX {
        return Err(Error::rejected(
            "package manifest exceeds the transfer limit",
        ));
    }
    if inspected.manifest.members.len() > TRANSFER_MEMBERS_MAX {
        return Err(Error::rejected(
            "package member count exceeds the transfer limit",
        ));
    }
    admit_transfer_manifest(&inspected.manifest_bytes, &inspected.manifest.members)?;
    Ok(inspected)
}

/// Transfer-only admission that mirrors the hosted manifest parser's closed
/// schema and inventory bounds, so the client never offers a package the
/// server always refuses. Pure over the manifest bytes: nothing here reads a
/// member file. Shared local-restore verification is unchanged.
pub(super) fn admit_transfer_manifest(manifest_bytes: &[u8], members: &[Member]) -> Result<()> {
    let raw: Value = serde_json::from_slice(manifest_bytes)
        .map_err(|_| Error::rejected("package manifest is not valid JSON"))?;
    let top = raw
        .as_object()
        .ok_or_else(|| Error::rejected("package manifest is not a JSON object"))?;
    if top
        .keys()
        .any(|key| !TRANSFER_MANIFEST_KEYS.contains(&key.as_str()))
    {
        return Err(Error::rejected(
            "package manifest has a key the transfer schema does not admit",
        ));
    }
    if let Some(raw_members) = top.get("members").and_then(Value::as_array) {
        for raw_member in raw_members {
            let closed = raw_member.as_object().is_some_and(|object| {
                object.len() == 3
                    && ["path", "bytes", "sha256"]
                        .iter()
                        .all(|key| object.contains_key(*key))
            });
            if !closed {
                return Err(Error::rejected(
                    "package member has a key the transfer schema does not admit",
                ));
            }
        }
    }
    let inventory_bytes = transfer_inventory_bytes(manifest_bytes, members)?;
    for member in members {
        if member.path.encode_utf16().count() > TRANSFER_MEMBER_PATH_MAX_UNITS {
            return Err(Error::rejected(
                "package member path exceeds the transfer limit",
            ));
        }
        if member.bytes > TRANSFER_MEMBER_MAX_BYTES {
            return Err(Error::rejected(
                "package member exceeds the transfer size limit",
            ));
        }
    }
    if inventory_bytes > TRANSFER_INVENTORY_MAX_BYTES {
        return Err(Error::rejected(
            "package inventory exceeds the transfer size limit",
        ));
    }
    Ok(())
}

/// Hosted inventory total: declared member bytes plus the manifest's own bytes
/// (the sealed inventory adds `manifest.json` as one more member). The single
/// source for both transfer admission and the preflight comparison, so the
/// client refuses exactly what the server refuses. Errors on overflow.
pub fn transfer_inventory_bytes(manifest_bytes: &[u8], members: &[Member]) -> Result<u64> {
    members
        .iter()
        .try_fold(manifest_bytes.len() as u64, |sum, m| {
            sum.checked_add(m.bytes)
        })
        .ok_or_else(|| Error::rejected("package inventory exceeds the transfer size limit"))
}

/// The 17 top-level keys the hosted manifest parser admits.
const TRANSFER_MANIFEST_KEYS: &[&str] = &[
    "format",
    "scope",
    "full_workspace",
    "created_at",
    "source_uuid",
    "cadence",
    "schema_version",
    "tracker_revision",
    "pm_root",
    "tables",
    "omitted_tables",
    "omitted_rows",
    "nulled_columns",
    "disabled_columns",
    "exclusions",
    "redacted_message_bodies",
    "members",
];
/// Hosted member path bound, counted in UTF-16 code units (JavaScript length).
pub const TRANSFER_MEMBER_PATH_MAX_UNITS: usize = 512;
/// Hosted per-member byte bound (512 MiB).
pub const TRANSFER_MEMBER_MAX_BYTES: u64 = 512 * 1024 * 1024;
/// Hosted inventory total bound (2 GiB): members plus the manifest's own bytes.
pub const TRANSFER_INVENTORY_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Transfer bounds the hosted packed-v1 endpoint enforces. The CLI refuses a
/// package outside them before any byte is sent; the server stays the
/// enforcement of record.
pub const TRANSFER_MANIFEST_MAX: usize = 4 * 1024 * 1024;
/// Logical data members cap (19,998). The sealed inventory adds `manifest.json`
/// as one separate entry (19,999 total), which is not counted here. The hosted
/// manifest parser admits 19,998 declared members, so this client-side refusal
/// matches the server bound instead of admitting what the server rejects.
pub const TRANSFER_MEMBERS_MAX: usize = 19998;

/// Manifest structure checks that read no member: format/scope, the admitted
/// PM root, every member path, the exact PM root derived from the member
/// inventory, and the presence of the core store. Shared by every verifier so
/// inspection, transfer, activation and restore admit the same shape.
fn admit_manifest(manifest_bytes: &[u8]) -> Result<PortableManifest> {
    let manifest: PortableManifest = serde_json::from_slice(manifest_bytes)
        .map_err(|e| Error::rejected(format!("malformed manifest: {e}")))?;
    if manifest.format != PORTABLE_FORMAT
        || manifest.scope != "coordination"
        || manifest.full_workspace
    {
        return Err(Error::rejected(
            "manifest is not a coordination portable package",
        ));
    }
    if !matches!(manifest.pm_root.as_deref(), None | Some(".") | Some("pm")) {
        return Err(Error::rejected(
            "manifest pm_root is not an admitted PM root",
        ));
    }
    // CAD-2215: the PM root is decided by the MEMBER INVENTORY that is about
    // to be verified, never by the manifest's own field. A manifest that says
    // `null` while its tracker carries `pm.yaml` would otherwise skip every
    // project record in the package.
    let tracker_prefix = format!("{PKG_TRACKER}/");
    let mut seen = std::collections::BTreeSet::new();
    for m in &manifest.members {
        let rel = Path::new(&m.path);
        if !safe_relative(rel) {
            return Err(Error::rejected(format!("unsafe member path {}", m.path)));
        }
        if m.path != PKG_DB && !m.path.starts_with(&tracker_prefix) {
            return Err(Error::rejected(format!(
                "member {} is outside the core store and tracker",
                m.path
            )));
        }
        if !seen.insert(m.path.clone()) {
            return Err(Error::rejected(format!("duplicate member {}", m.path)));
        }
    }
    if !seen.contains(PKG_DB) {
        return Err(Error::rejected("package has no store member"));
    }
    let tracker_paths: Vec<&str> = manifest
        .members
        .iter()
        .filter_map(|m| m.path.strip_prefix(&tracker_prefix))
        .collect();
    if pm_root_from_paths(tracker_paths.iter().copied())? != manifest.pm_root {
        return Err(Error::rejected(
            "package manifest pm_root does not match its tracker members",
        ));
    }
    Ok(manifest)
}

/// Copy every manifest member into a fresh 0700 private directory, verifying
/// and classifying the SAME bytes it writes. Inspection, activation preflight
/// and remote transfer then read only this snapshot, so a later change to the
/// mutable source package cannot be validated as clean and then streamed or
/// planned as private.
fn snapshot_members(root: &PackageRoot, manifest: &PortableManifest) -> Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new()
        .prefix("cadence-verified-")
        .tempdir()?;
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))?;
    let prefix = format!("{PKG_TRACKER}/");
    for m in &manifest.members {
        let rel = Path::new(&m.path);
        let dst = dir.path().join(rel);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        if m.path == PKG_DB {
            // The store can be large: hash and copy in one streamed pass,
            // refusing the moment the streamed bytes disagree.
            stream_member(root.open_member(rel)?, &dst, m)?;
            continue;
        }
        // One read through the pinned descriptor: the bytes hashed are the
        // bytes classified and the bytes written into the snapshot.
        let bytes = read_member(root, rel, m.bytes)?;
        if hex_digest(&bytes) != m.sha256 || bytes.len() as u64 != m.bytes {
            return Err(Error::rejected(format!(
                "member {} changed (digest or size mismatch)",
                m.path
            )));
        }
        if let Some(tracked) = m.path.strip_prefix(&prefix) {
            check_tracker_path(tracked)?;
            check_tracker_content(tracked, &bytes)?;
            check_tracker_private_content("package", manifest.pm_root.as_deref(), tracked, &bytes)?;
        }
        write_private_mode(&dst, &bytes, 0o600)?;
    }
    Ok(dir)
}

/// Hash-and-copy one large member in a single pass into the private snapshot.
/// `input` is already an opened, fstat-checked member of the pinned root.
fn stream_member(mut input: std::fs::File, dst: &Path, m: &Member) -> Result<()> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dst)?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > m.bytes {
            return Err(Error::rejected(format!(
                "member {} changed (digest or size mismatch)",
                m.path
            )));
        }
        hasher.update(&buf[..n]);
        output.write_all(&buf[..n])?;
    }
    if total != m.bytes || format!("{:x}", hasher.finalize()) != m.sha256 {
        return Err(Error::rejected(format!(
            "member {} changed (digest or size mismatch)",
            m.path
        )));
    }
    output.sync_all()?;
    Ok(())
}

/// A descriptor-pinned package directory. Every manifest and member read walks
/// from this descriptor with `openat(O_NOFOLLOW)`, and the leaf is fstat'ed on
/// the opened inode, so a symlink swapped into the leaf or any ancestor after
/// the pin cannot redirect a read outside the package. The package may be
/// owned by a different unprivileged UID, so path-based checks are not enough.
struct PackageRoot {
    dir: DirFd,
}

impl PackageRoot {
    fn open(root: &Path) -> Result<Self> {
        // Traverse the caller's spelling component by component with pinned,
        // no-follow directory handles. `..` returns to the pinned parent only
        // after the preceding component was itself opened as a real
        // directory, so `a/../b` keeps POSIX traversal semantics and a
        // symlinked or missing `a` refuses instead of being skipped as text.
        // `canonicalize` is never used: it would follow a swapped ancestor.
        let mut stack: Vec<DirFd> = if root.is_absolute() {
            vec![DirFd::open_root()?]
        } else {
            vec![DirFd::open_nofollow(Path::new("."))?]
        };
        for component in root.components() {
            match component {
                std::path::Component::RootDir => {
                    stack.truncate(1);
                    stack[0] = DirFd::open_root()?;
                }
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    if stack.len() > 1 {
                        stack.pop();
                    } else {
                        stack[0] = stack[0].open_child_dir(c"..").map_err(|_| {
                            Error::rejected("package parent directory is unavailable")
                        })?;
                    }
                }
                std::path::Component::Normal(name) => {
                    let encoded = CString::new(name.as_bytes())
                        .map_err(|_| Error::rejected("package path has NUL"))?;
                    let child = stack
                        .last()
                        .expect("the stack starts with a root handle")
                        .open_child_dir(&encoded)
                        .map_err(|_| {
                            Error::rejected(format!(
                                "package {} is not reachable through real directories",
                                root.display()
                            ))
                        })?;
                    stack.push(child);
                }
                std::path::Component::Prefix(_) => {
                    return Err(Error::rejected("unsupported package path prefix"))
                }
            }
        }
        let current = stack
            .pop()
            .ok_or_else(|| Error::rejected("package path is empty"))?;
        if current.stat()?.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(Error::rejected("package is not a plain directory"));
        }
        Ok(Self { dir: current })
    }

    /// Open one package-relative member through the pinned descriptor.
    fn open_member(&self, rel: &Path) -> Result<std::fs::File> {
        let mut parts = Vec::new();
        for component in rel.components() {
            match component {
                std::path::Component::Normal(name) => parts.push(name.to_owned()),
                _ => return Err(Error::rejected("unsafe package member path")),
            }
        }
        let Some((leaf, dirs)) = parts.split_last() else {
            return Err(Error::rejected("empty package member path"));
        };
        let mut dir = self.dir.try_clone()?;
        for name in dirs {
            let encoded = CString::new(name.as_bytes())
                .map_err(|_| Error::rejected("package member path has NUL"))?;
            dir = dir.open_child_dir(&encoded).map_err(|_| {
                Error::rejected(format!(
                    "package member {} is not reachable through a real directory",
                    rel.display()
                ))
            })?;
        }
        let encoded =
            CString::new(leaf.as_bytes()).map_err(|_| Error::rejected("member path has NUL"))?;
        let file = dir.open_readonly_nofollow(&encoded).map_err(|_| {
            Error::rejected(format!(
                "package member {} is not a plain readable file",
                rel.display()
            ))
        })?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.nlink() != 1 {
            return Err(Error::rejected(format!(
                "{} is not a plain single-link regular file",
                rel.display()
            )));
        }
        Ok(file)
    }
}

/// Every package-relative regular file, walked through pinned directory
/// handles. Each directory is enumerated via `fdopendir` on a duplicated
/// descriptor and each step descends with `openat(O_NOFOLLOW)`, so no path
/// component can be swapped mid-walk on either Linux or macOS.
#[cfg(unix)]
fn package_paths(root: &PackageRoot) -> Result<Vec<String>> {
    fn walk(dir: &DirFd, prefix: &str, out: &mut Vec<String>) -> Result<()> {
        let entries = dir
            .read_dir()
            .map_err(|_| Error::rejected("cannot enumerate a package directory"))?;
        for entry in entries {
            let encoded =
                entry.map_err(|_| Error::rejected("cannot enumerate a package directory"))?;
            let stat = dir
                .fstatat_nofollow(&encoded)
                .map_err(|_| Error::rejected("package entry cannot be classified"))?;
            let rel = format!("{prefix}{}", String::from_utf8_lossy(encoded.as_bytes()));
            match stat.st_mode & libc::S_IFMT {
                libc::S_IFLNK => {
                    return Err(Error::rejected("package contains a symlink"));
                }
                libc::S_IFDIR => {
                    let child = dir
                        .open_child_dir(&encoded)
                        .map_err(|_| Error::rejected("package directory is not reachable"))?;
                    walk(&child, &format!("{rel}/"), out)?;
                }
                libc::S_IFREG => out.push(rel),
                _ => {
                    return Err(Error::rejected("package contains a non-regular file"));
                }
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(&root.dir, "", &mut out)?;
    out.sort();
    Ok(out)
}

/// Read a pinned member fully.
fn read_member(root: &PackageRoot, rel: &Path, limit: u64) -> Result<Vec<u8>> {
    // Check the opened inode's size before allocating, and never read more
    // than the declared length: a sparse or oversized source cannot make this
    // allocate beyond the manifest's own bound.
    let file = root.open_member(rel)?;
    if file.metadata()?.len() != limit {
        return Err(Error::rejected(format!(
            "member {} changed (digest or size mismatch)",
            rel.display()
        )));
    }
    let mut bytes = Vec::with_capacity(limit.min(1024 * 1024) as usize);
    let read = file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if read as u64 != limit {
        return Err(Error::rejected(format!(
            "member {} changed (digest or size mismatch)",
            rel.display()
        )));
    }
    Ok(bytes)
}

/// The manifest itself, capped before allocation.
fn read_manifest_file(root: &PackageRoot) -> Result<Vec<u8>> {
    let file = root.open_member(Path::new(PKG_MANIFEST))?;
    let declared = file.metadata()?.len();
    if declared > TRANSFER_MANIFEST_MAX as u64 {
        return Err(Error::rejected(
            "package manifest exceeds the transfer limit",
        ));
    }
    let mut bytes = Vec::with_capacity(declared as usize);
    let read = file
        .take(TRANSFER_MANIFEST_MAX as u64 + 1)
        .read_to_end(&mut bytes)?;
    if read as u64 != declared {
        return Err(Error::rejected("package manifest changed while being read"));
    }
    Ok(bytes)
}

/// Hash the manifest through the pinned package root without admitting its contents.
pub(crate) fn package_manifest_digest(source: &Path) -> Result<String> {
    let root = PackageRoot::open(source)?;
    Ok(hex_digest(&read_manifest_file(&root)?))
}

/// Hosted transfer bounds, enforced before a snapshot is created so a hostile
/// manifest cannot make inspection copy or allocate beyond them.
fn enforce_transfer_bounds(manifest: &PortableManifest) -> Result<()> {
    if manifest.members.len() > TRANSFER_MEMBERS_MAX {
        return Err(Error::rejected(
            "package member count exceeds the transfer limit",
        ));
    }
    let mut total = 0u64;
    for member in &manifest.members {
        if member.bytes > TRANSFER_MEMBER_MAX_BYTES {
            return Err(Error::rejected(
                "package member exceeds the transfer byte limit",
            ));
        }
        if member.path.encode_utf16().count() > TRANSFER_MEMBER_PATH_MAX_UNITS {
            return Err(Error::rejected(
                "package member path exceeds the transfer limit",
            ));
        }
        total = total
            .checked_add(member.bytes)
            .ok_or_else(|| Error::rejected("package byte total overflows"))?;
    }
    if total > TRANSFER_INVENTORY_MAX_BYTES {
        return Err(Error::rejected(
            "package byte total exceeds the transfer limit",
        ));
    }
    Ok(())
}

pub(in crate::backup) fn inspect_package(source: &Path) -> Result<VerifiedPackage> {
    let root = PackageRoot::open(source)?;
    // Reject linked directories before reading any listed member through them.
    // (Member reads walk the pinned descriptor below, so this only enumerates
    // names for the unlisted-member check.)
    let paths = package_paths(&root)?;
    let manifest_bytes = read_manifest_file(&root)?;
    let manifest_sha256 = format!("{:x}", Sha256::digest(&manifest_bytes));
    let manifest = admit_manifest(&manifest_bytes)?;
    // Hosted transfer bounds are enforced before the snapshot copies or
    // allocates anything.
    enforce_transfer_bounds(&manifest)?;
    // The manifest's member set must be exactly what the package holds (plus
    // the manifest itself); an unlisted file could carry content no
    // classification saw.
    for path in &paths {
        if path != PKG_MANIFEST && !manifest.members.iter().any(|m| &m.path == path) {
            return Err(Error::rejected(format!(
                "package contains unlisted member {path}"
            )));
        }
    }
    // Verify and classify each member while copying it into a private
    // snapshot, then use that snapshot rather than the mutable source.
    // This does not isolate the copy from other processes with the same UID.
    let snapshot = snapshot_members(&root, &manifest)?;
    let snapshot_db = snapshot.path().join(PKG_DB);
    // Ordinary read-only SQLite opens can still create WAL/SHM files. Native
    // exports are rollback-journal single-file databases; admit that header
    // before any SQLite connection touches the snapshot.
    let mut header = [0u8; 20];
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&snapshot_db)?
        .read_exact(&mut header)?;
    if &header[..16] != b"SQLite format 3\0" || header[18] != 1 || header[19] != 1 {
        return Err(Error::rejected(
            "portable inspection requires a single-file rollback-journal SQLite database",
        ));
    }
    validate_staged_store(&snapshot_db, &manifest)?;
    let snapshot_tracker = snapshot.path().join(PKG_TRACKER);
    if pm_root_of(&snapshot_tracker)? != manifest.pm_root {
        return Err(Error::rejected(
            "package tracker PM root differs from manifest",
        ));
    }
    if hex_digest(&read_manifest_file(&root)?) != manifest_sha256 {
        return Err(Error::rejected(
            "package manifest changed during inspection",
        ));
    }
    let bytes = manifest.members.iter().try_fold(0u64, |total, member| {
        total
            .checked_add(member.bytes)
            .ok_or_else(|| Error::rejected("package byte total overflows"))
    })?;
    let report = json!({
        "status": "verified_coordination_package",
        "portable": true,
        "bundle": source,
        "manifest_sha256": manifest_sha256,
        "format": manifest.format,
        "scope": manifest.scope,
        "full_workspace": manifest.full_workspace,
        "cadence": manifest.cadence,
        "inspector_revision": crate::overview::BUILD_COMMIT,
        "schema_version": manifest.schema_version,
        "tracker_revision": manifest.tracker_revision,
        "members": manifest.members.len(),
        "bytes": bytes,
        "tables": manifest.tables,
        "omitted_tables": manifest.omitted_tables,
        "omitted_rows": manifest.omitted_rows,
        "nulled_columns": manifest.nulled_columns,
        "disabled_columns": manifest.disabled_columns,
        "exclusions": manifest.exclusions,
        "redacted_message_bodies": manifest.redacted_message_bodies,
        "verification": {
            "member_paths_hashes_sizes": true,
            "exact_member_set": true,
            "database_integrity_catalog_authority": true
        },
        "source_fidelity": "not_independently_established",
        "import_ready": false
    });
    Ok(VerifiedPackage {
        manifest,
        manifest_bytes,
        manifest_sha256,
        report,
        snapshot,
    })
}

/// Independently inspect a portable coordination package, read-only. Prints
/// integrity, schema and exclusion evidence; does not restore, start a daemon,
/// or establish import readiness.
pub fn inspect_portable(source: &Path) -> Result<Value> {
    Ok(inspect_package(source)?.report)
}
