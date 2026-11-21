//! Inactive restore, offline disabled activation, private staging, and staged verification for portable packages.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

use crate::error::{Error, Result};

use super::super::{inspect, quote_ident, require_ok, sync_dir};
use super::classify::{
    app_conversation_rows, check_tracker_private_content, count_where, hex_digest,
    pm_root_from_paths, walk_tracker_paths,
};
use super::{
    check_catalog, hash_file, inspect_package, omitted_names, rename_noreplace, sql_string,
    write_private, Member, NativeOperatorProof, PortableManifest, PortableRestoreOptions,
    VerifiedPackage, DISABLED, DISABLED_TEXT, INACTIVE_MARKER, NULLED, PKG_DB, PKG_TRACKER,
    TRACKER_DIR,
};

/// Name of the commit journal. Its presence means a restore is mid-publish
/// and the destination must not start until recovered by the operator.
const JOURNAL: &str = "portable-restore.journal";

/// Restore a portable package into `destination`, an explicit state dir.
///
/// Transaction, in order:
/// 1. Verify the package members and manifest (bytes, digests, paths, types).
/// 2. Take the destination's `cadence.lock` (the same `flock` a daemon holds)
///    and refuse a destination that is not empty or has leftovers.
/// 3. Stage the core store and tracker in a fresh private directory whose
///    name carries a random nonce, never a fixed name a caller could own.
/// 4. Validate the staged store (catalog, exact columns, authority empty,
///    agents disabled, live fields null, FK check, schema, integrity) and
///    re-hash every staged member.
/// 5. Write the journal, then rename the staged set into place, the marker
///    last inside the staged set so `cadence.sqlite3` never appears without
///    it. The marker is what stops a daemon from starting over inactive data.
///
/// Any failure before step 5 leaves the destination byte-identical.
pub fn restore_inactive(
    source: &Path,
    destination: &Path,
    opts: &PortableRestoreOptions,
) -> Result<Value> {
    if opts.replace {
        return Err(Error::rejected(
            "replacing a destination needs native operator admission, which is not wired yet; nothing was written",
        ));
    }
    // Verify once into the immutable snapshot and stage only from it: no
    // source path (or any of its ancestors) is read after verification.
    let verified = inspect_package(source)?;
    let manifest = verified.manifest.clone();
    refuse_nonempty(destination)?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(Error::rejected(
            "destination parent directory does not exist",
        ));
    }
    // No parent lock: the parent is not the daemon's state dir, and a flock
    // there would only create an unrelated `cadence.lock`. The destination
    // is new-only, so the no-replace rename at publish is the race guard: a
    // concurrent create makes it fail and nothing is merged.
    refuse_nonempty(destination)?;
    // The whole inactive set is staged beside the destination and published
    // with one no-replace rename: DB, tracker and marker appear together or
    // not at all.
    let stage = parent.join(format!(".portable-stage-{}", nonce()));
    fs::DirBuilder::new().mode(0o700).create(&stage)?;
    if let Err(error) = stage_and_validate(verified.snapshot_root(), &stage, &manifest) {
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }
    publish_stage(&stage, destination)?;
    Ok(json!({
        "restored": true,
        "state": "committed_inactive",
        "destination": destination,
        "pm_dir": manifest.pm_root.as_deref().map(|rel| destination.join(TRACKER_DIR).join(rel)),
        "tracker_revision": manifest.tracker_revision,
        "schema_version": manifest.schema_version,
        "members": manifest.members.len(),
    }))
}

/// AOS-339/CAD-1705: restore a package into a NEW directory and activate it,
/// manual mode with execution disabled, in one step. The destination must not
/// exist: this path can never activate, overwrite or touch an existing managed
/// root or a live state dir, so the terminal-anchored operator gate on
/// `portable_activate` keeps applying to every root that already exists. The
/// activation runs on the private stage before the no-replace publish, so a
/// refusal leaves nothing behind.
pub fn restore_activate_disabled(
    source: &Path,
    destination: &Path,
    reconciliation: &[crate::backup::portable_activate::Reconciliation],
) -> Result<Value> {
    let verified = inspect_package(source)?;
    let manifest = &verified.manifest;
    let digest = &verified.manifest_sha256;
    refuse_existing_activation(destination)?;
    // The package and the target must be disjoint: staging beside, inside or
    // over the package would read and write the same tree.
    let target = crate::backup::portable_replace::offline_activation_target(source, destination)?;
    let parent = target
        .parent()
        .ok_or_else(|| Error::rejected("destination has no parent directory"))?;
    let stage = parent.join(format!(".portable-stage-{}", nonce()));
    fs::DirBuilder::new().mode(0o700).create(&stage)?;
    let outcome = (|| {
        stage_and_validate(verified.snapshot_root(), &stage, manifest)?;
        let before = crate::backup::portable_replace::snapshot_target_dir(&stage)?;
        let mut report = crate::backup::portable_activate::activate_in_place(
            &stage,
            &before,
            digest,
            reconciliation,
        )?;
        publish_stage(&stage, &target)?;
        if let Some(obj) = report.as_object_mut() {
            obj.insert("restored".into(), json!(true));
            obj.insert("destination".into(), json!(target));
            obj.insert("package_digest".into(), json!(digest));
        }
        Ok(report)
    })();
    if outcome.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    outcome
}

/// AOS-339/CAD-1705: the read-only activation plan for a package. Reads the
/// package's own store and writes nothing persistent.
pub fn activate_preflight(source: &Path) -> Result<Value> {
    // Inspect the private snapshot before producing the reconciliation plan.
    let verified = inspect_package(source)?;
    let digest = &verified.manifest_sha256;
    let required = crate::backup::portable_activate::required_inventory(verified.snapshot_root())?;
    Ok(json!({
        "package_digest": digest,
        "execution_mode": "disabled",
        "bindings": [],
        "required_reconciliation": required.iter().map(|r| json!({
            "kind": r.kind, "id": r.id, "status": r.status, "action": "retain_inert"
        })).collect::<Vec<_>>(),
    }))
}

/// A destination that exists at all is refused here, unlike `restore_inactive`,
/// which accepts an empty directory.
fn refuse_existing_activation(destination: &Path) -> Result<()> {
    match destination.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::rejected(format!(
            "cannot inspect {}: {error}",
            destination.display()
        ))),
        Ok(_) => Err(Error::rejected(
            "destination exists; an offline activation restores into a new directory only",
        )),
    }
}

fn nonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Copy the core and tracker into `stage`, verify the copies, write the
/// inactive marker, and re-hash every staged member before commit.
fn stage_and_validate(source: &Path, stage: &Path, manifest: &PortableManifest) -> Result<()> {
    stage_members_and_validate(source, stage, manifest)?;
    // A managed root carries the daemon's zero-byte lock inode, created
    // here by the restore that publishes it (replacement never creates it).
    crate::backup::portable_replace::create_stage_lock(stage)?;
    write_private(
        &stage.join(INACTIVE_MARKER),
        serde_json::to_string_pretty(&restore_marker(manifest))?.as_bytes(),
    )?;
    verify_staged_members(stage, manifest)?;
    // The package purges the source UUID, so the receiver mints its own
    // coordination identity. This runs after the member hash check: it is the
    // receiver's materialization, not package content.
    ensure_coordination_metadata(&stage.join(PKG_DB), manifest.schema_version)?;
    Ok(())
}

/// The inactive marker for a fresh restore. Replacement writes its own
/// marker with a fresh generation.
fn restore_marker(manifest: &PortableManifest) -> Value {
    json!({
        "state": "committed_inactive",
        "format": manifest.format,
        "tracker_revision": manifest.tracker_revision,
        "source_uuid": manifest.source_uuid,
        "schema_version": manifest.schema_version,
        "members": manifest.members.len(),
        "activation": "not performed; native operator admission required",
    })
}

/// Copy and validate the core store and tracker into `stage`. No marker.
fn stage_members_and_validate(
    source: &Path,
    stage: &Path,
    manifest: &PortableManifest,
) -> Result<()> {
    let db = stage.join(PKG_DB);
    copy_private_file(&source.join(PKG_DB), &db)?;
    copy_tree(&source.join(PKG_TRACKER), &stage.join(TRACKER_DIR))?;
    validate_staged_store(&db, manifest)?;
    verify_tracker_content(&stage.join(TRACKER_DIR), manifest)?;
    if let Some(rel) = manifest.pm_root.as_deref() {
        crate::issue::Pm::at(&stage.join(TRACKER_DIR).join(rel))?;
    }
    Ok(())
}

/// Materialize the coordination singleton row the package deliberately
/// purges. The ledger head is reset to the genesis of database epoch 1 when
/// the staged schema already carries the v58 head columns; an older schema is
/// completed by the daemon's own migration on first open.
fn ensure_coordination_metadata(db: &Path, schema_version: i64) -> Result<()> {
    let conn = Connection::open(db)?;
    conn.execute(
        "INSERT OR IGNORE INTO connection_metadata(singleton, workspace_id) \
         VALUES(1, lower(hex(randomblob(16))))",
        [],
    )?;
    if schema_version >= 58 {
        let genesis = cadence_core::connection_dispatch::ledger::genesis_ledger_head_v1(
            cadence_core::connection_dispatch::Revision::new(1)
                .map_err(|_| Error::internal("fixture epoch"))?,
        );
        conn.execute(
            "UPDATE connection_metadata SET database_epoch=?1, ledger_sequence=?2, \
             ledger_digest=?3 WHERE singleton=1",
            rusqlite::params![
                genesis.database_epoch.get(),
                genesis.sequence.get(),
                genesis.digest.as_str()
            ],
        )?;
    }
    Ok(())
}

/// Re-hash every staged core and tracker member immediately before commit:
/// a source change after verify but before copy must not survive.
fn verify_staged_members(stage: &Path, manifest: &PortableManifest) -> Result<()> {
    let tracker_prefix = format!("{PKG_TRACKER}/");
    let mut seen = std::collections::BTreeSet::new();
    for m in manifest.members.iter() {
        let staged = match m.path.strip_prefix(&tracker_prefix) {
            Some(rel) => stage.join(TRACKER_DIR).join(rel),
            None => stage.join(&m.path),
        };
        if let Some(rel) = m.path.strip_prefix(&tracker_prefix) {
            // Re-read and re-classify the staged bytes that are about to be
            // published. Hashing and classification use the same buffer, so a
            // replacement after the earlier verification cannot publish
            // content no classification saw.
            let bytes = fs::read(&staged)
                .map_err(|_| Error::rejected(format!("staged member {} unreadable", m.path)))?;
            if hex_digest(&bytes) != m.sha256 || bytes.len() as u64 != m.bytes {
                return Err(Error::rejected(format!(
                    "staged member {} does not match the manifest",
                    m.path
                )));
            }
            seen.insert(rel.to_string());
            check_tracker_private_content("package", manifest.pm_root.as_deref(), rel, &bytes)?;
        } else {
            let (sha, bytes) = hash_file(&staged)?;
            if sha != m.sha256 || bytes != m.bytes {
                return Err(Error::rejected(format!(
                    "staged member {} does not match the manifest",
                    m.path
                )));
            }
        }
    }
    // An unlisted staged file could carry content no classification saw.
    let paths = walk_tracker_paths(&stage.join(TRACKER_DIR))?;
    if paths.len() != seen.len() || paths.iter().any(|path| !seen.contains(path)) {
        return Err(Error::rejected(
            "staged tracker member set differs from the manifest",
        ));
    }
    Ok(())
}

/// Publish the verified stage as the destination in one no-replace rename.
/// A destination that appears concurrently makes the rename fail, and the
/// stage is removed; nothing is ever merged into or overwritten inside an
/// existing destination.
fn publish_stage(stage: &Path, destination: &Path) -> Result<()> {
    if let Err(error) = rename_noreplace(stage, destination) {
        let _ = fs::remove_dir_all(stage);
        return Err(error);
    }
    if let Some(parent) = destination.parent() {
        sync_dir(parent);
    }
    Ok(())
}

/// A member must be a regular file with a single link: not a symlink,
/// device, fifo, socket or hardlink.
fn require_regular(path: &Path) -> Result<()> {
    let meta = path
        .symlink_metadata()
        .map_err(|_| Error::rejected(format!("{} is missing", path.display())))?;
    if !meta.is_file() || meta.nlink() != 1 {
        return Err(Error::rejected(format!(
            "{} is not a plain single-link regular file",
            path.display()
        )));
    }
    Ok(())
}

/// A destination that exists must be an empty directory. Checked before any
/// create or lock, so a refused restore leaves a nonempty destination untouched.
fn refuse_nonempty(destination: &Path) -> Result<()> {
    match destination.symlink_metadata() {
        Err(_) => Ok(()),
        Ok(meta) if !meta.is_dir() => {
            Err(Error::rejected("destination exists and is not a directory"))
        }
        Ok(_) => {
            let any = fs::read_dir(destination)?.next().is_some();
            if any {
                return Err(Error::rejected(
                    "destination is not empty; replacement needs native operator admission",
                ));
            }
            Ok(())
        }
    }
}

/// A coordination package exported before a table existed is still
/// restorable; the disabled-column checks below only apply where the table
/// is actually present, so an older package is not refused for a column its
/// schema never had.
pub(super) fn table_present(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

/// Validate the staged store on the receiver: integrity, FK, schema, the
/// exact closed catalog, and the cell policy. Source identity is read only
/// from the manifest, never from the staged store.
pub(super) fn validate_staged_store(db: &Path, manifest: &PortableManifest) -> Result<()> {
    let (integrity, schema) = inspect(db)?;
    require_ok(db, &integrity)?;
    if schema != manifest.schema_version {
        return Err(Error::rejected(
            "staged store schema does not match the manifest",
        ));
    }
    if schema > crate::rollout::SCHEMA_VERSION {
        return Err(Error::rejected("staged store is newer than this binary"));
    }
    check_catalog(db)?;
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let fk = conn
        .prepare("PRAGMA foreign_key_check")?
        .query_map([], |_| Ok(()))?
        .count();
    if fk > 0 {
        return Err(Error::rejected(format!(
            "staged store has {fk} foreign-key violation(s)"
        )));
    }
    for table in omitted_names() {
        let present: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?",
            [table],
            |r| r.get(0),
        )?;
        if present == 0 {
            continue;
        }
        let rows: i64 = conn.query_row(
            &format!("SELECT count(*) FROM {}", quote_ident(table)),
            [],
            |r| r.get(0),
        )?;
        if rows > 0 {
            return Err(Error::rejected(format!(
                "staged store carries omitted table {table} ({rows} rows)"
            )));
        }
    }
    // CAD-2215: a coordination store never carries an app installation
    // conversation (`threads.install_id`). A conversation is private member
    // work whose installation, grants and member provenance are all outside
    // this profile, so a package that still carries one was produced
    // without the export classification and cannot be imported as proven
    // clean. The `threads` row is not an omitted table, so the loop above
    // cannot see it.
    if app_conversation_rows(&conn)? > 0 {
        return Err(Error::rejected(
            "staged store carries app conversations; a coordination package cannot be proven \
             free of private member work",
        ));
    }
    // The same rule for the coordinated tables that can carry private
    // content: human-room topics and cloud-agent markers. Their provenance
    // tables are omitted, so these markers are the only evidence a package
    // could carry, and a package carrying them was produced without the
    // export classification.
    if count_where(&conn, "topics", "type='human_room'")? > 0 {
        return Err(Error::rejected(
            "staged store carries human-room topics; a coordination package cannot be proven \
             free of private conversation",
        ));
    }
    let cloud_provider = format!("provider={}", sql_string(crate::cloud_agent::PROVIDER));
    if count_where(&conn, "agents", &cloud_provider)? > 0
        || count_where(&conn, "messages", "source='cloud_agent'")? > 0
    {
        return Err(Error::rejected(
            "staged store carries cloud-agent state; a coordination package cannot be proven \
             free of private cloud content",
        ));
    }
    let enabled: i64 = conn.query_row("SELECT count(*) FROM agents WHERE enabled<>0", [], |r| {
        r.get(0)
    })?;
    if enabled > 0 {
        return Err(Error::rejected("staged store enables imported actors"));
    }
    for (table, column) in NULLED {
        let live: i64 = conn.query_row(
            &format!("SELECT count(*) FROM {table} WHERE {column} IS NOT NULL"),
            [],
            |r| r.get(0),
        )?;
        if live > 0 {
            return Err(Error::rejected(format!(
                "staged store carries live authority {table}.{column}"
            )));
        }
    }
    for (table, column) in DISABLED {
        if !table_present(&conn, table)? {
            continue;
        }
        let on: i64 = conn.query_row(
            &format!("SELECT count(*) FROM {table} WHERE {column}<>0"),
            [],
            |r| r.get(0),
        )?;
        if on > 0 {
            return Err(Error::rejected(format!(
                "staged store enables {table}.{column}"
            )));
        }
    }
    for (table, column, value) in DISABLED_TEXT {
        if !table_present(&conn, table)? {
            continue;
        }
        let on: i64 = conn.query_row(
            &format!("SELECT count(*) FROM {table} WHERE {column}<>?1"),
            rusqlite::params![value],
            |r| r.get(0),
        )?;
        if on > 0 {
            return Err(Error::rejected(format!(
                "staged store sets {table}.{column} to an enabled value"
            )));
        }
    }
    Ok(())
}

/// The tracker copy must match the manifest's tracker members: same set,
/// same bytes, same digests, and only regular files.
fn verify_tracker_content(tracker: &Path, manifest: &PortableManifest) -> Result<()> {
    // Actual members are relative to the tracker dir; manifest members carry
    // the package's `tracker/` prefix. Compare on the path below that prefix.
    let prefix = format!("{PKG_TRACKER}/");
    let paths = walk_tracker_paths(tracker)?;
    let actual_pm_root = pm_root_from_paths(paths.iter().map(String::as_str))?;
    if actual_pm_root != manifest.pm_root {
        return Err(Error::rejected(
            "tracker copy PM root does not match the manifest",
        ));
    }
    // Manifest members are named with the package's `tracker/` prefix, while
    // the staged tree is keyed by its tracker-relative path (and lives under
    // `TRACKER_DIR` on disk). Index by the relative path so the two namespaces
    // are compared exactly once.
    let mut expected: std::collections::BTreeMap<&str, &Member> = std::collections::BTreeMap::new();
    for m in manifest.members.iter() {
        if let Some(rel) = m.path.strip_prefix(&prefix) {
            expected.insert(rel, m);
        }
    }
    if paths.len() != expected.len() {
        return Err(Error::rejected(
            "tracker copy member set differs from the manifest",
        ));
    }
    for rel in &paths {
        let file = tracker.join(rel);
        // One read for both the digest comparison and the content
        // classification: the staged bytes that verify are the bytes that get
        // classified, so a concurrent replacement cannot slip unclassified
        // content past the check that just passed.
        let bytes = fs::read(&file)
            .map_err(|_| Error::rejected(format!("tracker member {rel} unreadable")))?;
        let sha = hex_digest(&bytes);
        let matches = expected
            .get(rel.as_str())
            .is_some_and(|m| m.sha256 == sha && m.bytes == bytes.len() as u64);
        if !matches {
            return Err(Error::rejected(format!(
                "tracker member {rel} does not match the manifest"
            )));
        }
        check_tracker_private_content("package", actual_pm_root.as_deref(), rel, &bytes)?;
    }
    Ok(())
}

fn copy_private_file(src: &Path, dst: &Path) -> Result<()> {
    require_regular(src)?;
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(src)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dst)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::DirBuilder::new().mode(0o700).create(dst)?;
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((s, d)) = stack.pop() {
        for entry in fs::read_dir(&s)? {
            let entry = entry?;
            let from = entry.path();
            let to = d.join(entry.file_name());
            let meta = from.symlink_metadata()?;
            if meta.is_dir() {
                fs::DirBuilder::new().mode(0o700).create(&to)?;
                stack.push((from, to));
            } else {
                copy_private_file(&from, &to)?;
            }
        }
    }
    Ok(())
}

/// True when the marker is an `activated` marker (its receipt is checked by
/// the fence loader, not here).
fn activated_root(state_dir: &Path) -> bool {
    fs::read_to_string(state_dir.join(INACTIVE_MARKER))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|v| v["state"].as_str() == Some(crate::backup::portable_activate::ACTIVATED))
}

/// Refuse daemon start while a portable restore is inactive or mid-publish.
/// Any journal, any marker and any unreadable or malformed marker refuses,
/// so a damaged marker or a crash mid-publish cannot let a daemon start.
pub fn refuse_if_inactive(state_dir: &Path) -> Result<()> {
    // An activated, execution-disabled root is admitted here only; the
    // fence loader (`portable_fence::load`) then requires its matching
    // receipt and refuses any mismatch. An inactive marker still refuses.
    if activated_root(state_dir) {
        return Ok(());
    }
    if state_dir.join(JOURNAL).symlink_metadata().is_ok() {
        return Err(Error::rejected(format!(
            "{} holds a portable restore journal (a publish did not finish); inspect and recover before starting the daemon",
            state_dir.display()
        )));
    }
    let marker = state_dir.join(INACTIVE_MARKER);
    match fs::symlink_metadata(&marker) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::rejected(format!(
            "cannot read {}: {e}; refusing to start",
            marker.display()
        ))),
        Ok(_) => {
            let parsed = fs::read_to_string(&marker)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok());
            let state = parsed
                .as_ref()
                .and_then(|v| v["state"].as_str())
                .unwrap_or("malformed");
            Err(Error::rejected(format!(
                "{} holds a portable restore ({state}); activate it through the native operator path before starting the daemon",
                state_dir.display()
            )))
        }
    }
}

pub fn activate(_state_dir: &Path, _proof: &NativeOperatorProof) -> Result<Value> {
    Err(Error::rejected(
        "activation requires native operator admission, which is not wired yet; the restored data stays inactive",
    ))
}

/// Closed verifier for replacement: the same package verification as
/// [`restore_inactive`], exposed crate-internally.
pub(crate) fn verify_package_for_replace(source: &Path) -> Result<VerifiedPackage> {
    inspect_package(source)
}

/// Stage and validate the complete inactive member set into the empty
/// private `stage` dir. Writes no marker: replacement writes its own.
pub(crate) fn stage_inactive_members(
    source: &Path,
    stage: &Path,
    manifest: &PortableManifest,
) -> Result<()> {
    stage_members_and_validate(source, stage, manifest)?;
    verify_staged_members(stage, manifest)
}
