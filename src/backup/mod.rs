//! Backup, export and restore of the daemon store (CAD-314)
//! plus per-installation app record files (CAD-767).
//!
//! - [`backup`] copies the live store with SQLite's online backup API from
//!   a read-only connection. In WAL mode that reader never blocks the
//!   daemon's writer, and the copy is one consistent snapshot. The copy is
//!   switched to a single self-contained file, checked with
//!   `PRAGMA integrity_check`, hashed, and described by a manifest (format,
//!   schema version, sha256, binary versions, repo remotes). Each
//!   `<state_dir>/app-records/<install_id>.sqlite3` present at backup time
//!   is copied the same way — online backup from a read-only connection,
//!   so WAL contents ride along and the daemon's writer is never blocked —
//!   then integrity-checked, hashed, and bound into the same manifest by
//!   installation ID, file-schema version, digest/size, integrity and the
//!   core copy's hash (`core_sha256`). The manifest also records the core
//!   snapshot's installation catalog (`app_installations`: distinct
//!   installation IDs across core tables carrying `install_id`), read from
//!   the snapshot copy itself: a record file whose installation is absent
//!   from the catalog — forged, deleted, or never installed — refuses as
//!   orphaned, while catalog installations without files are allowed
//!   (contexts and capabilities predate the first record write, and
//!   no-App installs stay compatible). The manifest is written last, then
//!   the whole set is re-verified from disk — entries against the backup
//!   core copy's catalog, never the manifest text alone. A failed partial
//!   backup removes every partial, copy and manifest it staged, so it
//!   never appears complete. `keep` prunes the oldest backups *with the
//!   that directory. Age comes from the stamp in the file name (then
//!   mtime), never from manifest content; the set just written is never
//!   pruned; a copy is deleted only when it is the regular file the
//!   manifest names and its sha256 and size match the manifest. A hand-made
//!   copy is never pruned.
//! - [`export`] is the portable form of the core store only: the same
//!   snapshot with endpoint tokens nulled and freed pages dropped
//!   (`VACUUM`), then every text cell is run through the CAD-109 secret
//!   scan. One blocking finding refuses the export and removes the bundle
//!   directory. App record files are never read into an export bundle.
//!   Nothing else in the state dir is ever read into the bundle.
//! - [`restore`] verifies a manifest and every copy it names, refuses a
//!   schema newer than this binary (core or record-file), refuses a
//!   mixed-generation set (a record entry stamped with another core
//!   snapshot's hash, or outside the core copy's installation catalog),
//!   refuses a state dir whose daemon holds `cadence.lock` (and holds that
//!   lock itself while it works), refuses interrupted-restore asides,
//!   refuses to replace an existing store or record file without `force`
//!   (and backs the existing store — core plus record files — up first
//!   when forced), then rewrites repo paths by matching remote URLs. Core
//!   and record copies are staged and jointly verified before anything
//!   activates — including a catalog check of every entry against the
//!   staged core copy that will actually go live — then the complete
//!   staged state activates at once ([`activate_all`]: every live file
//!   moves aside first, every staged file links in, and any failure
//!   renames every aside back, so a returned error never leaves a partial
//!   replacement). Record-file identity (filename versus the file's own
//!   `record_identity` row) and file schema are verified before
//!   activation; the installation catalog proof itself lives in the RPC
//!   layer (`RecordStore::open` checks identity and schema, the record
//!   RPCs resolve the installation through the workspace catalog and
//!   prove the live context per action).
//! - [`before_self_update`] is the backup a self-update takes before it
//!   swaps the binary. `cadence upgrade` (`upgrade::run`) calls it before
//!   it installs anything or moves the link, and refuses on `Err`.
//!
//! Cross-file reconciliation: core and app files are snapshotted in
//! sequence, never in one transaction — the manifest is the atomic commit
//! point, binding each record entry to the core generation it was taken
//! with. A backup that fails partway removes everything it staged. A
//! restore stages everything (core remap included) and proves the staged
//! core's catalog admits every record entry before moving a single live
//! file; activation then moves all live files aside and links all staged
//! files in, rolling every aside back on any failure. If activation fails,
//! the verified pre-restore backup remains the crash-independent recovery
//! path, and any `.replaced-*` asides a killed process leaves behind
//! refuse the next restore and the next daemon start until the operator
//! puts the previous files back. Core-only manifests (written before
//! CAD-767, or from a state with no `app-records/`) verify and restore
//! with an empty record set; a state with no record files backs up and
//! restores the same way. Older binaries ignore the new manifest fields
//! and restore core only (their pruning leaves record copies behind
//! rather than deleting blindly).
//!
//! Nothing here opens the live store for writing. Record bodies never
//! enter manifests, logs or errors — only installation IDs, digests,
//! sizes, schema versions and file names.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::backup::Backup;
use rusqlite::types::ValueRef;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::rollout::{db_file, SCHEMA_VERSION};
use crate::secret::{self, Allowlist, Severity};

#[cfg(test)]
mod tests;

/// Manifest format tag. A reader refuses any other value.
pub const FORMAT: &str = "cadence.backup/1";
/// Default retention per reason: the nightly schedule keeps a week.
pub const DEFAULT_KEEP: usize = 7;
/// The store's file name inside an export bundle.
pub const BUNDLE_DB: &str = "cadence.sqlite3";
/// The manifest's file name inside an export bundle.
pub const BUNDLE_MANIFEST: &str = "manifest.json";
/// Reason label of the backup a self-update takes.
pub const PRE_UPDATE: &str = "pre-update";
/// Reason label of the backup `restore --force` takes of the store it replaces.
pub const PRE_RESTORE: &str = "pre-restore";

/// Columns that hold filesystem paths. Restore rewrites them by prefix;
/// backup reads them to find which repos the store refers to. Paths inside
/// JSON params and event payloads are history and stay as written.
const PATH_COLUMNS: &[(&str, &str)] = &[
    ("agents", "cwd"),
    ("jobs", "repo"),
    ("jobs", "spec_path"),
    ("tasks", "worktree"),
    ("tasks", "spec_path"),
];

/// Columns an export sets to NULL. `generation` is the live endpoint
/// generation every turn token is bound to; without it no recorded token
/// validates. `messages.turn_id` is the turn token itself — the bearer a
/// running turn reports with. `pid` is a process on the source host,
/// and `pid_start` its start time there (CAD-385).
const SCRUB_COLUMNS: &[(&str, &str)] = &[
    ("agents", "generation"),
    ("agents", "pid"),
    ("agents", "pid_start"),
    ("messages", "turn_id"),
];

/// What an export bundle contains, recorded in its manifest.
const EXPORT_CONTAINS: &[&str] = &[
    "cadence.sqlite3: online-backup snapshot of the store, scrubbed and VACUUMed",
    "manifest.json: this file",
];

/// What an export bundle leaves out, recorded in its manifest.
const EXPORT_EXCLUDES: &[&str] = &[
    "every other state-dir file: ui.json, slots.json, daemon-instance, logs, cadence.lock, the socket",
    "app record files: <state dir>/app-records/*.sqlite3 (per-installation SQLite, covered by `cadence backup`, never exported)",
    "operator config: secret-allowlist.toml, intake-relay.yaml",
    ".env files",
    "state-dir folders: private/, sessions/, briefings/, reviews/, agents/, roles/, backups/",
    "provider auth (Claude, Codex, Devin, Cursor sign-in state in their own dirs): never read",
    "endpoint tokens: agents.generation (turn-token generation), messages.turn_id (turn tokens), agents.pid and agents.pid_start are set to NULL",
    "turn tokens and generations elsewhere (event payloads, message text): every token-shaped value (<prefix>-<hex12|hex32>-<hex32>), its generation, and every hex12|hex32 value under a JSON \"generation\" key is replaced with [redacted] wherever it appears; the export refuses if any remain",
    "freed database pages: VACUUM drops deleted rows",
    "the tracker (PM dir): a git repo with its own remote",
];

/// How long an online backup retries a busy or locked source.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(60);

/// How many finding locations an error or a result lists.
const LIST_CAP: usize = 20;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Backup,
    Export,
}

/// One repo checkout the store refers to. `remote` is `origin` with any
/// userinfo stripped.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Repo {
    pub path: String,
    pub remote: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Versions {
    /// `cadence --version` of the binary that wrote the copy.
    pub cadence: String,
    /// The schema that binary migrates to.
    pub binary_schema: i64,
    /// The SQLite library that wrote the copy.
    pub sqlite: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ExportInfo {
    pub contains: Vec<String>,
    pub excludes: Vec<String>,
    pub scrubbed: Vec<String>,
    pub scanned_cells: u64,
    pub scan_warnings: usize,
    /// Distinct turn tokens and generations redacted, and the text cells
    /// they were redacted from (CAD-396).
    #[serde(default)]
    pub redacted_tokens: usize,
    #[serde(default)]
    pub redacted_cells: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Manifest {
    pub format: String,
    pub kind: Kind,
    pub reason: String,
    /// RFC 3339 UTC, second precision.
    pub created_at: String,
    /// Unix seconds with sub-second precision; retention orders by it.
    pub created_epoch: f64,
    /// The copy's file name, in the manifest's own directory.
    pub db_file: String,
    pub sha256: String,
    pub bytes: u64,
    /// `schema_version` read from the copy itself.
    pub schema_version: i64,
    pub integrity_check: String,
    pub versions: Versions,
    pub repos: Vec<Repo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<ExportInfo>,
    /// Per-installation record snapshots (CAD-767), sorted by
    /// installation ID. Absent on core-only manifests written before
    /// CAD-767 or from a state with no record files; an empty list and
    /// a missing field both mean "no record files".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub app_records: Vec<AppRecordFile>,
    /// The core snapshot's installation catalog at backup time: sorted
    /// distinct installation IDs across every core table carrying an
    /// `install_id` column (contexts, capabilities, runs, bindings,
    /// grants). Absent on older manifests. Restore proves the staged
    /// core copy yields the same catalog before activating anything.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub app_installations: Vec<String>,
}

/// One installation's record-file snapshot inside a backup manifest.
/// The entry binds the installation ID to the copy's file name,
/// file-schema version, digest/size and integrity verdict, taken with
/// the core snapshot this manifest describes; `core_sha256` repeats the
/// manifest's core copy hash so a record entry lifted from another backup
/// (same installation, different generation) refuses. Record bodies never
/// appear here — only identity, shape and hashes.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppRecordFile {
    pub install_id: String,
    /// The copy's file name, in the manifest's own directory.
    pub db_file: String,
    pub sha256: String,
    pub bytes: u64,
    /// `record_schema.version` read from the copy itself.
    pub file_schema: i64,
    pub integrity_check: String,
    /// The manifest's core copy hash at backup time. Manifests written
    /// before this binding (including the unmerged CAD-767 draft head)
    /// carry `""` and refuse whenever they name record entries.
    #[serde(default)]
    pub core_sha256: String,
}

/// The default backup directory: `<state dir>/backups`.
pub fn default_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("backups")
}

// ---------- app record files (CAD-767) ----------

/// Live per-installation record-file IDs, sorted. A missing
/// `app-records/` directory means no record files (core-only state).
/// WAL sidecars (`*.sqlite3-wal`/`-shm`) and dotfiles (staging partials
/// from an interrupted restore) are not live databases and are ignored —
/// the online backup reads WAL state through the database file itself.
/// Anything else that cannot be an installation file refuses the backup:
/// a non-identifier filename or a symlink or non-regular file. The error
/// names the installation ID or file name, never a record body.
fn inventory_app_records(state_dir: &Path) -> Result<Vec<String>> {
    let dir = state_dir.join(crate::store::app_records::RECORDS_DIR);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(Error::rejected(format!(
                "cannot inventory {}: {e}",
                dir.display()
            )))
        }
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Some(install) = name.strip_suffix(".sqlite3") else {
            continue;
        };
        if install.is_empty() {
            return Err(Error::rejected(format!(
                "orphaned record file {name:?} in {}; remove it after inspection",
                dir.display()
            )));
        }
        if crate::proto::identifier(install, "installation ID").is_err() {
            return Err(Error::rejected(format!(
                "orphaned record file {name:?} in {}; remove it after inspection",
                dir.display()
            )));
        }
        let path = entry.path();
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_file() => {}
            _ => {
                return Err(Error::rejected(format!(
                    "orphaned record file {name:?} in {}; remove it after inspection",
                    dir.display()
                )))
            }
        }
        out.push(install.to_string());
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// `(integrity_check, file_schema, install_id)` of a record file,
/// read-only. Anything SQLite cannot read, or whose schema/identity rows
/// do not match, is corruption or a foreign file — refused with an
/// explicit recovery error that never carries a record body.
fn inspect_app_record(db: &Path) -> Result<(String, i64, String)> {
    const CORRUPT: &str =
        "record file is corrupt or foreign; restore the installation backup or remove the file after inspection";
    let conn =
        crate::store::open_read_only(db).map_err(|_| Error::rejected(CORRUPT.to_string()))?;
    let integrity: Vec<String> = conn
        .prepare("PRAGMA integrity_check")
        .map_err(|_| Error::rejected(CORRUPT.to_string()))?
        .query_map([], |r| r.get(0))
        .map_err(|_| Error::rejected(CORRUPT.to_string()))?
        .collect::<rusqlite::Result<_>>()
        .map_err(|_| Error::rejected(CORRUPT.to_string()))?;
    let integrity = integrity.join("; ");
    let schema: i64 = conn
        .query_row("SELECT version FROM record_schema", [], |r| r.get(0))
        .map_err(|_| Error::rejected(CORRUPT.to_string()))?;
    let identity: String = conn
        .query_row("SELECT install_id FROM record_identity", [], |r| r.get(0))
        .map_err(|_| Error::rejected(CORRUPT.to_string()))?;
    Ok((integrity, schema, identity))
}

fn newer_record_schema_refusal(schema: i64) -> Result<()> {
    if schema > crate::store::app_records::FILE_SCHEMA {
        return Err(Error::rejected(format!(
            "record file schema {schema} is newer than this binary's {}; restore it with the cadence build that wrote it or a newer one",
            crate::store::app_records::FILE_SCHEMA
        )));
    }
    Ok(())
}

/// The core snapshot's installation catalog: sorted distinct
/// installation IDs across every core table carrying an `install_id`
/// column (contexts, capabilities, runs, bindings, grants — discovered
/// through `sqlite_master`, so older schemas without app tables yield
/// whatever they carry and future tables are covered). Read from a core
/// copy, read-only, never the live store. A stored ID outside the
/// installation grammar refuses — core data must name installations the
/// same way files do. Errors name the table or installation, never
/// record bodies (which live outside core by design).
fn core_installations(db: &Path) -> Result<Vec<String>> {
    let conn = crate::store::open_read_only(db).map_err(|e| {
        Error::rejected(format!(
            "cannot read installation catalog from {}: {e}",
            db.display()
        ))
    })?;
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut set = std::collections::BTreeSet::new();
    for table in &tables {
        let columns: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info({})", quote_ident(table)))?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        if !columns.iter().any(|c| c == "install_id") {
            continue;
        }
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT install_id FROM {}",
            quote_ident(table)
        ))?;
        let ids = stmt
            .query_map([], |r| r.get::<_, Option<String>>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in ids.into_iter().flatten() {
            if crate::proto::identifier(&id, "installation ID").is_err() {
                return Err(Error::rejected(format!(
                    "core catalog in {} names an invalid installation in {table:?}; inspect the store before backing up",
                    db.display()
                )));
            }
            set.insert(id);
        }
    }
    Ok(set.into_iter().collect())
}

/// Snapshot every inventoried record file into `dir` under
/// `<stem>.app-<install_id>.sqlite3`, bound into manifest entries sorted
/// by installation ID. Every file's installation must appear in
/// `catalog` (the core snapshot's installation set): a forged file for a
/// deleted or never-installed ID refuses as orphaned. Catalog
/// installations without files are allowed — contexts and capabilities
/// predate the first record write, and no-App installs stay compatible.
/// A file created after `inventoried` refuses the backup so a partial set
/// never appears complete. Each entry stamps `core_sha256` with the core
/// copy's hash. Staged copies are caller-cleaned on error (see `backup`).
fn snapshot_app_records(
    state_dir: &Path,
    dir: &Path,
    stem: &str,
    inventoried: &[String],
    catalog: &[String],
    core_sha256: &str,
) -> Result<Vec<AppRecordFile>> {
    for install_id in inventoried {
        if !catalog.contains(install_id) {
            return Err(Error::rejected(format!(
                "record file for installation {install_id:?} has no installation in the core snapshot; restore the installation backup or remove the file after inspection"
            )));
        }
    }
    let mut out = Vec::new();
    for install_id in inventoried {
        let live = crate::store::app_records::record_db_path(state_dir, install_id)?;
        let db_name = format!("{stem}.app-{install_id}.sqlite3");
        let partial = dir.join(format!(".{db_name}.partial"));
        let dest = dir.join(&db_name);
        // snapshot() maps a missing source to a rejected open; name the
        // installation explicitly so the refusal is actionable.
        if !live.is_file() {
            return Err(Error::rejected(format!(
                "record file for installation {install_id:?} disappeared during backup; retry the backup"
            )));
        }
        snapshot(&live, &partial).map_err(|e| {
            let _ = fs::remove_file(&partial);
            Error::rejected(format!(
                "cannot snapshot record file for installation {install_id:?}: {e}"
            ))
        })?;
        let staged = (|| -> Result<AppRecordFile> {
            let (integrity, file_schema, identity) = inspect_app_record(&partial)?;
            require_ok(&partial, &integrity)?;
            if identity != *install_id {
                return Err(Error::rejected(format!(
                    "record file identity differs from installation {install_id:?}; restore the installation backup or remove the file after inspection"
                )));
            }
            if file_schema != crate::store::app_records::FILE_SCHEMA {
                newer_record_schema_refusal(file_schema)?;
                return Err(Error::rejected(format!(
                    "record file schema for installation {install_id:?} is unsupported; restore the installation backup or remove the file after inspection"
                )));
            }
            fs::rename(&partial, &dest)?;
            let (sha256, bytes) = hash_file(&dest)?;
            Ok(AppRecordFile {
                install_id: install_id.clone(),
                db_file: db_name.clone(),
                sha256,
                bytes,
                file_schema,
                integrity_check: integrity,
                core_sha256: core_sha256.to_string(),
            })
        })();
        match staged {
            Ok(entry) => out.push(entry),
            Err(error) => {
                let _ = fs::remove_file(&partial);
                let _ = fs::remove_file(&dest);
                return Err(error);
            }
        }
    }
    // A file created after the inventory is a partial set: refuse rather
    // than report complete without it.
    let current = inventory_app_records(state_dir)?;
    if current != inventoried {
        for entry in &out {
            let _ = fs::remove_file(dir.join(&entry.db_file));
        }
        return Err(Error::rejected(
            "record files changed during backup; retry the backup".to_string(),
        ));
    }
    Ok(out)
}

/// Staged app-record copies (plus their partials) for `stem` in `dir`,
/// for failure cleanup. Matches only files `snapshot_app_records` writes.
fn manifest_app_candidates(dir: &Path, stem: &str) -> Vec<PathBuf> {
    let prefix = format!("{stem}.app-");
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (name.starts_with(&prefix) && name.ends_with(".sqlite3"))
                || (name.starts_with(&format!(".{prefix}")) && name.ends_with(".partial"))
        })
        .map(|e| e.path())
        .collect()
}

// ---------- backup ----------

/// Take a verified backup of `<state_dir>/cadence.sqlite3` plus every
/// `<state_dir>/app-records/<install_id>.sqlite3` into `dir`, then prune
/// that directory to the newest `keep` backups with `reason`.
///
/// Each record file is snapshotted with the same online-backup API as the
/// core store, so WAL contents ride along without blocking the daemon.
/// The core snapshot's installation catalog (distinct installation IDs
/// across core tables carrying `install_id`) gates every file: a record
/// file whose installation is absent from the catalog — forged, deleted,
/// or never installed — refuses as orphaned. Catalog installations
/// without files are allowed (contexts predate the first record write;
/// no-App installs stay compatible). A record file that is missing
/// mid-snapshot, corrupt, foreign (identity or schema mismatch), or newly
/// created after the inventory refuses the whole backup — the error names
/// the installation ID, never a record body — and everything staged is
/// removed, so a partial backup never appears complete. A state with no
/// `app-records/` backs up core-only.
pub fn backup(state_dir: &Path, dir: &Path, keep: usize, reason: &str) -> Result<Value> {
    validate_reason(reason)?;
    if keep == 0 {
        return Err(Error::rejected("--keep must be at least 1"));
    }
    let live = db_file(state_dir);
    if !live.is_file() {
        return Err(Error::rejected(format!(
            "no cadence database at {}",
            live.display()
        )));
    }
    ensure_private_dir(dir)?;
    let now = epoch_now();
    let stem = format!(
        "cadence-{reason}-{}-{}",
        crate::issue::time::basic(now as i64),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let db_name = format!("{stem}.sqlite3");
    let partial = dir.join(format!(".{db_name}.partial"));
    let db = dir.join(&db_name);
    let manifest_path = dir.join(format!("{stem}.manifest.json"));
    // The live inventory: files created after this point refuse the
    // backup below instead of silently missing from it.
    let inventoried = inventory_app_records(state_dir)?;
    let taken = (|| -> Result<Manifest> {
        snapshot(&live, &partial)?;
        let (integrity, schema) = inspect(&partial)?;
        require_ok(&partial, &integrity)?;
        let repos = discover_repos(&partial)?;
        fs::rename(&partial, &db)?;
        let (sha256, bytes) = hash_file(&db)?;
        // The catalog comes from the snapshot copy, so it describes the
        // exact core generation the record files are bound to.
        let catalog = core_installations(&db)?;
        let app_records =
            snapshot_app_records(state_dir, dir, &stem, &inventoried, &catalog, &sha256)?;
        let manifest = Manifest {
            format: FORMAT.into(),
            kind: Kind::Backup,
            reason: reason.into(),
            created_at: crate::issue::time::iso(now as i64),
            created_epoch: now,
            db_file: db_name.clone(),
            sha256,
            bytes,
            schema_version: schema,
            integrity_check: integrity,
            versions: versions(),
            repos,
            export: None,
            app_records,
            app_installations: catalog,
        };
        write_private(&manifest_path, &serde_json::to_vec_pretty(&manifest)?)?;
        sync_dir(dir);
        verify(&manifest_path)?;
        Ok(manifest)
    })();
    let manifest = match taken {
        Ok(manifest) => manifest,
        Err(error) => {
            let mut staged: Vec<PathBuf> = vec![partial, db, manifest_path];
            for entry in manifest_app_candidates(dir, &stem) {
                staged.push(entry);
            }
            for path in &staged {
                let _ = fs::remove_file(path);
            }
            return Err(error);
        }
    };
    let pruned = prune(dir, reason, keep, &manifest_path).map_err(|e| {
        Error::rejected(format!(
            "backup {} was written and verified, but pruning old {reason:?} backups in {} \
             failed: {e}",
            manifest_path.display(),
            dir.display()
        ))
    })?;
    // The set this call promises must still be on disk.
    let mut promised = vec![db, manifest_path];
    for entry in &manifest.app_records {
        promised.push(dir.join(&entry.db_file));
    }
    for path in &promised {
        if !is_regular_file(path) {
            return Err(Error::internal(format!(
                "backup {} vanished after pruning {}; nothing is backed up",
                path.display(),
                dir.display()
            )));
        }
    }
    Ok(json!({
        "backup": true,
        "manifest": promised[1],
        "db": promised[0],
        "sha256": manifest.sha256,
        "bytes": manifest.bytes,
        "schema_version": manifest.schema_version,
        "integrity_check": manifest.integrity_check,
        "reason": manifest.reason,
        "repos": manifest.repos,
        "app_records": manifest.app_records,
        "keep": keep,
        "pruned": pruned.removed,
        "prune_skipped": pruned.skipped,
    }))
}

/// The backup a self-update takes before it replaces the binary. An
/// install with no store yet has nothing to lose, so that is not an error.
/// A caller must abort the update on `Err`.
pub fn before_self_update(state_dir: &Path) -> Result<Value> {
    if !db_file(state_dir).is_file() {
        return Ok(json!({"skipped": "no database", "state_dir": state_dir}));
    }
    backup(state_dir, &default_dir(state_dir), DEFAULT_KEEP, PRE_UPDATE)
}

struct Pruned {
    removed: Vec<PathBuf>,
    /// Old manifests whose copy is not the file they describe (a symlink,
    /// another file, changed bytes): left alone.
    skipped: Vec<Value>,
}

/// `(stamp, uuid)` of a file cadence names `cadence-<reason>-<stamp>-<uuid8>`
/// — the name `backup` writes. Anything else is not ours.
fn backup_stem_parts<'a>(stem: &'a str, reason: &str) -> Option<(&'a str, &'a str)> {
    let rest = stem.strip_prefix("cadence-")?.strip_prefix(reason)?;
    let rest = rest.strip_prefix('-')?;
    let (stamp, id) = rest.split_once('-')?;
    let stamp_ok = stamp.len() == 16
        && stamp.as_bytes()[8] == b'T'
        && stamp.ends_with('Z')
        && stamp
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 8 || i == 15 || b.is_ascii_digit());
    let id_ok = id.len() == 8 && id.bytes().all(|b| b.is_ascii_hexdigit());
    (stamp_ok && id_ok).then_some((stamp, id))
}

/// An old backup pair of ours, ordered newest first by `age`
/// (file-name stamp, manifest mtime, uuid).
struct Candidate {
    age: (String, SystemTime, String),
    manifest_path: PathBuf,
    db: PathBuf,
    manifest: Manifest,
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// Remove the oldest backups with `reason` beyond `keep`. `current` (the
/// manifest just written) is never removed and counts as one of `keep`.
/// Age is the stamp in the file name, then the manifest's mtime — never
/// manifest content. A copy is deleted only when it is the regular file
/// the manifest names next to its manifest and its sha256 and size match;
/// otherwise the set is reported under `skipped` and left alone. Record
/// copies go before the core copy: a manifest without its copies is
/// inert, a copy without its manifest is never pruned again.
fn prune(dir: &Path, reason: &str, keep: usize, current: &Path) -> Result<Pruned> {
    let mut found: Vec<Candidate> = Vec::new();
    for entry in fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".manifest.json") else {
            continue;
        };
        let Some((stamp, id)) = backup_stem_parts(stem, reason) else {
            continue;
        };
        let path = entry.path();
        if path == current || !is_regular_file(&path) {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) else {
            continue;
        };
        if manifest.format != FORMAT
            || manifest.kind != Kind::Backup
            || manifest.reason != reason
            || manifest.db_file != format!("{stem}.sqlite3")
        {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        let db = dir.join(&manifest.db_file);
        found.push(Candidate {
            age: (stamp.to_string(), mtime, id.to_string()),
            manifest_path: path,
            db,
            manifest,
        });
    }
    found.sort_by(|a, b| b.age.cmp(&a.age));
    let mut out = Pruned {
        removed: Vec::new(),
        skipped: Vec::new(),
    };
    for Candidate {
        manifest_path,
        db,
        manifest,
        ..
    } in found.into_iter().skip(keep.saturating_sub(1))
    {
        // Record copies first: every listed file must be the regular
        // file the manifest names with matching sha256/size, else the
        // whole set is left alone for inspection.
        let mut app_paths: Vec<PathBuf> = Vec::new();
        let mut app_skipped = false;
        for entry in &manifest.app_records {
            if !plain_file_name(&entry.db_file) {
                out.skipped.push(
                    json!({"manifest": manifest_path, "db": dir.join(&entry.db_file),
                    "why": "the manifest names a record copy that is not a plain file name"}),
                );
                app_skipped = true;
                break;
            }
            let path = dir.join(&entry.db_file);
            match fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Ok(meta) if meta.file_type().is_file() => {
                    let matches = hash_file(&path)
                        .is_ok_and(|(sha, size)| sha == entry.sha256 && size == entry.bytes);
                    if !matches {
                        out.skipped
                            .push(json!({"manifest": manifest_path, "db": path,
                            "why": "the record copy does not match its manifest (sha256/size)"}));
                        app_skipped = true;
                        break;
                    }
                    app_paths.push(path);
                }
                Ok(_) => {
                    out.skipped
                        .push(json!({"manifest": manifest_path, "db": path,
                        "why": "the record copy is not a regular file"}));
                    app_skipped = true;
                    break;
                }
                Err(e) => {
                    return Err(Error::internal(format!("pruning {}: {e}", path.display())));
                }
            }
        }
        if app_skipped {
            continue;
        }
        match fs::symlink_metadata(&db) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // A manifest without its core copy is inert: drop it
                // with any matching record copies, so no copy is left
                // without its manifest.
                for path in &app_paths {
                    fs::remove_file(path)
                        .map_err(|e| Error::internal(format!("pruning {}: {e}", path.display())))?;
                    out.removed.push(path.clone());
                }
            }
            Ok(meta) if meta.file_type().is_file() => {
                let matches = hash_file(&db)
                    .is_ok_and(|(sha, size)| sha == manifest.sha256 && size == manifest.bytes);
                if !matches {
                    out.skipped.push(json!({"manifest": manifest_path, "db": db,
                        "why": "the copy does not match its manifest (sha256/size)"}));
                    continue;
                }
                // Record copies go first, then the core copy: a manifest
                // without its copies is inert, a copy without its
                // manifest is never pruned again.
                for path in &app_paths {
                    fs::remove_file(path)
                        .map_err(|e| Error::internal(format!("pruning {}: {e}", path.display())))?;
                    out.removed.push(path.clone());
                }
                fs::remove_file(&db)
                    .map_err(|e| Error::internal(format!("pruning {}: {e}", db.display())))?;
                out.removed.push(db);
            }
            Ok(_) => {
                out.skipped.push(json!({"manifest": manifest_path, "db": db,
                    "why": "the copy is not a regular file"}));
                continue;
            }
            Err(e) => {
                return Err(Error::internal(format!("pruning {}: {e}", db.display())));
            }
        }
        match fs::remove_file(&manifest_path) {
            Ok(()) => out.removed.push(manifest_path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(Error::internal(format!(
                    "pruning {}: {e}",
                    manifest_path.display()
                )))
            }
        }
    }
    Ok(out)
}

// ---------- verify ----------

/// Read a manifest and prove every copy it names: format, core schema no
/// newer than this binary, sha256/size, integrity, and the schema recorded
/// inside each copy. Each record entry additionally proves its
/// installation ID (filename grammar, manifest binding and the
/// `record_identity` row inside the copy) and its file schema. A missing,
/// truncated, corrupt, wrong-identity or mismatched copy refuses before
/// anything is activated. Core-only manifests (no `app_records` field)
/// verify with an empty record set.
pub fn verify(manifest_path: &Path) -> Result<(Manifest, PathBuf)> {
    let bytes = fs::read(manifest_path).map_err(|e| {
        Error::rejected(format!(
            "cannot read manifest {}: {e}",
            manifest_path.display()
        ))
    })?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| {
        Error::rejected(format!(
            "{} is not a cadence backup manifest: {e}",
            manifest_path.display()
        ))
    })?;
    if manifest.format != FORMAT {
        return Err(Error::rejected(format!(
            "{} has format {:?}; this binary reads {FORMAT:?}",
            manifest_path.display(),
            manifest.format
        )));
    }
    newer_schema_refusal(manifest.schema_version)?;
    if !plain_file_name(&manifest.db_file) {
        return Err(Error::rejected(format!(
            "{} names db_file {:?}; it must be a plain file name next to the manifest",
            manifest_path.display(),
            manifest.db_file
        )));
    }
    let db = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(&manifest.db_file);
    let (sha256, size) = hash_file(&db)?;
    if sha256 != manifest.sha256 || size != manifest.bytes {
        return Err(Error::rejected(format!(
            "{} does not match its manifest: sha256 {sha256} ({size} bytes), \
             manifest records {} ({} bytes)",
            db.display(),
            manifest.sha256,
            manifest.bytes
        )));
    }
    let (integrity, schema) = inspect(&db)?;
    require_ok(&db, &integrity)?;
    if schema != manifest.schema_version {
        return Err(Error::rejected(format!(
            "{} holds schema {schema}, its manifest records {}",
            db.display(),
            manifest.schema_version
        )));
    }
    verify_app_records(manifest_path, &manifest)?;
    Ok((manifest, db))
}

/// Prove every record copy a manifest names. Duplicate installation IDs
/// or file names, a non-identifier installation, a non-plain file name,
/// a sha/size mismatch, a failed integrity check, a schema newer than
/// this binary (or differing from the manifest entry), an identity row
/// that disagrees with the entry, a record entry lifted from another core
/// generation (`core_sha256` differs from the manifest's core hash), a
/// record entry outside the backup core copy's installation catalog, or a
/// recorded catalog that disagrees with that copy all refuse. The catalog
/// is read from the backup's own core copy — never trusted from the
/// manifest alone — so core from backup A with records from backup B
/// cannot pass. Errors name the installation ID or file name, never a
/// record body.
fn verify_app_records(manifest_path: &Path, manifest: &Manifest) -> Result<()> {
    use std::collections::BTreeSet;
    let dir = manifest_path.parent().unwrap_or(Path::new("."));
    // The catalog this backup's core copy carries. Every entry must be
    // a member; catalog installations without entries are allowed (see
    // `snapshot_app_records`: contexts predate the first record write).
    let catalog = core_installations(&dir.join(&manifest.db_file)).map_err(|e| {
        Error::rejected(format!(
            "{} core copy has no readable installation catalog: {e}",
            manifest_path.display()
        ))
    })?;
    if !manifest.app_installations.is_empty() && manifest.app_installations != catalog {
        return Err(Error::rejected(format!(
            "{} records an installation catalog that differs from its core copy",
            manifest_path.display()
        )));
    }
    let mut installs = BTreeSet::new();
    let mut files = BTreeSet::new();
    for entry in &manifest.app_records {
        if crate::proto::identifier(&entry.install_id, "installation ID").is_err() {
            return Err(Error::rejected(format!(
                "{} names an invalid installation {:?}",
                manifest_path.display(),
                entry.install_id
            )));
        }
        if !plain_file_name(&entry.db_file) {
            return Err(Error::rejected(format!(
                "{} names record db_file {:?}; it must be a plain file name next to the manifest",
                manifest_path.display(),
                entry.db_file
            )));
        }
        if !installs.insert(&entry.install_id) {
            return Err(Error::rejected(format!(
                "{} lists installation {:?} twice",
                manifest_path.display(),
                entry.install_id
            )));
        }
        if !files.insert(&entry.db_file) {
            return Err(Error::rejected(format!(
                "{} lists record file {:?} twice",
                manifest_path.display(),
                entry.db_file
            )));
        }
        if entry.db_file == manifest.db_file {
            return Err(Error::rejected(format!(
                "{} lists record file {:?} that collides with the core copy",
                manifest_path.display(),
                entry.db_file
            )));
        }
        let path = dir.join(&entry.db_file);
        let (sha256, size) = hash_file(&path).map_err(|e| {
            Error::rejected(format!(
                "cannot read record copy {} for installation {:?}: {e}",
                path.display(),
                entry.install_id
            ))
        })?;
        if sha256 != entry.sha256 || size != entry.bytes {
            return Err(Error::rejected(format!(
                "record copy {} for installation {:?} does not match its manifest: sha256 {sha256} ({size} bytes), manifest records {} ({} bytes)",
                path.display(),
                entry.install_id,
                entry.sha256,
                entry.bytes
            )));
        }
        let (integrity, file_schema, identity) = inspect_app_record(&path).map_err(|e| {
            Error::rejected(format!(
                "record copy {} for installation {:?} is corrupt or foreign: {e}",
                path.display(),
                entry.install_id
            ))
        })?;
        require_ok(&path, &integrity).map_err(|e| {
            Error::rejected(format!(
                "record copy {} for installation {:?}: {e}",
                path.display(),
                entry.install_id
            ))
        })?;
        if integrity != entry.integrity_check {
            return Err(Error::rejected(format!(
                "record copy {} for installation {:?} holds integrity {integrity:?}, its manifest records {:?}",
                path.display(),
                entry.install_id,
                entry.integrity_check
            )));
        }
        newer_record_schema_refusal(entry.file_schema)?;
        newer_record_schema_refusal(file_schema)?;
        if file_schema != entry.file_schema {
            return Err(Error::rejected(format!(
                "record copy {} for installation {:?} holds file schema {file_schema}, its manifest records {}",
                path.display(),
                entry.install_id,
                entry.file_schema
            )));
        }
        if identity != entry.install_id {
            return Err(Error::rejected(format!(
                "record copy {} identity differs from installation {:?}; restore the installation backup or remove the file after inspection",
                path.display(),
                entry.install_id
            )));
        }
        if entry.core_sha256 != manifest.sha256 {
            return Err(Error::rejected(format!(
                "record entry for installation {:?} was taken with a different core snapshot; refusing a mixed-generation backup",
                entry.install_id
            )));
        }
        if !catalog.contains(&entry.install_id) {
            return Err(Error::rejected(format!(
                "record entry for installation {:?} has no installation in the backup core snapshot; refusing a mixed-generation backup",
                entry.install_id
            )));
        }
    }
    Ok(())
}

fn newer_schema_refusal(schema: i64) -> Result<()> {
    if schema > SCHEMA_VERSION {
        return Err(Error::rejected(format!(
            "backup schema {schema} is newer than this binary's schema {SCHEMA_VERSION}; \
             restore it with the cadence build that wrote it or a newer one"
        )));
    }
    Ok(())
}

// ---------- export ----------

/// Write a portable bundle (`cadence.sqlite3` + `manifest.json`) into the
/// new directory `out`. Fails closed: a blocking secret-scan finding, or
/// any other error, removes `out` again.
pub fn export(state_dir: &Path, out: &Path) -> Result<Value> {
    let live = db_file(state_dir);
    if !live.is_file() {
        return Err(Error::rejected(format!(
            "no cadence database at {}",
            live.display()
        )));
    }
    if out.symlink_metadata().is_ok() {
        return Err(Error::rejected(format!(
            "{} already exists; export writes a new directory",
            out.display()
        )));
    }
    // A malformed allowlist refuses here, before anything is written.
    let allow = Allowlist::load(state_dir)?;
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::DirBuilder::new().mode(0o700).create(out)?;
    let result = export_into(&live, out, &allow);
    if result.is_err() {
        let _ = fs::remove_dir_all(out);
    }
    result
}

fn export_into(live: &Path, out: &Path, allow: &Allowlist) -> Result<Value> {
    let partial = out.join(format!(".{BUNDLE_DB}.partial"));
    snapshot(live, &partial)?;
    let scrub = scrub(&partial)?;
    let scrubbed = scrub.columns.clone();
    let scan = scan_db(&partial, allow)?;
    let (integrity, schema) = inspect(&partial)?;
    require_ok(&partial, &integrity)?;
    let repos = discover_repos(&partial)?;
    let db = out.join(BUNDLE_DB);
    fs::rename(&partial, &db)?;
    let (sha256, bytes) = hash_file(&db)?;
    let now = epoch_now();
    let manifest = Manifest {
        format: FORMAT.into(),
        kind: Kind::Export,
        reason: "export".into(),
        created_at: crate::issue::time::iso(now as i64),
        created_epoch: now,
        db_file: BUNDLE_DB.into(),
        sha256,
        bytes,
        schema_version: schema,
        integrity_check: integrity,
        versions: versions(),
        repos,
        export: Some(ExportInfo {
            contains: EXPORT_CONTAINS.iter().map(|s| s.to_string()).collect(),
            excludes: EXPORT_EXCLUDES.iter().map(|s| s.to_string()).collect(),
            scrubbed: scrubbed.clone(),
            redacted_tokens: scrub.redacted_values,
            redacted_cells: scrub.redacted_cells,
            scanned_cells: scan.cells,
            scan_warnings: scan.warnings.len(),
        }),
        app_records: Vec::new(),
        app_installations: Vec::new(),
    };
    let text = serde_json::to_string_pretty(&manifest)?;
    secret::guard_with("export manifest", &text, allow)?;
    let manifest_path = out.join(BUNDLE_MANIFEST);
    write_private(&manifest_path, text.as_bytes())?;
    sync_dir(out);
    verify(&manifest_path)?;
    Ok(json!({
        "export": true,
        "bundle": out,
        "manifest": manifest_path,
        "sha256": manifest.sha256,
        "bytes": manifest.bytes,
        "schema_version": manifest.schema_version,
        "repos": manifest.repos,
        "scrubbed": scrubbed,
        "redacted": {"tokens": scrub.redacted_values, "cells": scrub.redacted_cells},
        "scan": {
            "cells": scan.cells,
            "warnings": scan.warnings.len(),
            "warning_locations": scan.warnings.iter().take(LIST_CAP).collect::<Vec<_>>(),
            "rules": format!("gitleaks {} + cadence", secret::GITLEAKS_VERSION),
        },
    }))
}

/// Null the token-bearing columns, then rebuild the file so freed pages
/// (deleted rows, old values) are not carried along.
fn scrub(db: &Path) -> Result<Scrub> {
    let conn = Connection::open(db)?;
    // Turn tokens live on in event payloads and prose long after the
    // messages row, and a token spells out its generation. Redact every
    // token-shaped value everywhere before the columns are nulled.
    let shape = token_shape();
    let redaction = redact_turn_tokens(&conn, &shape)?;
    let mut scrubbed = Vec::new();
    for (table, column) in SCRUB_COLUMNS {
        if has_column(&conn, table, column)? {
            conn.execute(&format!("UPDATE {table} SET {column}=NULL"), [])?;
            scrubbed.push(format!("{table}.{column}"));
        }
    }
    conn.execute_batch("VACUUM")?;
    conn.close().map_err(|(_, e)| e)?;
    refuse_remaining_tokens(db, &shape, redaction.matcher.as_ref())?;
    Ok(Scrub {
        columns: scrubbed,
        redacted_values: redaction.values,
        redacted_cells: redaction.cells,
    })
}

struct Scrub {
    columns: Vec<String>,
    redacted_values: usize,
    redacted_cells: u64,
}

struct Redaction {
    values: usize,
    cells: u64,
    matcher: Option<aho_corasick::AhoCorasick>,
}

/// What a redacted turn token or generation reads as in an export.
pub const REDACTED: &str = "[redacted]";

fn user_tables(conn: &Connection) -> Result<Vec<String>> {
    Ok(conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type='table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Visit every TEXT cell: `(table, column, rowid, text)`.
fn each_text_cell(
    conn: &Connection,
    mut f: impl FnMut(&str, &str, i64, &str) -> Result<()>,
) -> Result<()> {
    for table in user_tables(conn)? {
        let mut stmt = conn.prepare(&format!("SELECT rowid, * FROM {}", quote_ident(&table)))?;
        let columns: Vec<String> = stmt
            .column_names()
            .iter()
            .skip(1)
            .map(|s| s.to_string())
            .collect();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let rowid: i64 = row.get(0)?;
            for (i, column) in columns.iter().enumerate() {
                let text = match row.get_ref(i + 1)? {
                    ValueRef::Text(bytes) | ValueRef::Blob(bytes) => String::from_utf8_lossy(bytes),
                    _ => continue,
                };
                f(&table, column, rowid, &text)?;
            }
        }
    }
    Ok(())
}

/// The registry's turn-token shape (CAD-407); group 1 is the generation.
fn token_shape() -> regex::Regex {
    regex::Regex::new(&crate::adapter::registry::turn_token_pattern())
        .expect("the registry's turn-token pattern compiles")
}

/// An endpoint generation logged on its own under a JSON `"generation"`
/// key — the `ready` and `pane_root` event payloads — at any JSON escape
/// depth (CAD-424). Group 1 is the generation. `owner_generation` and
/// other keys that merely end in `generation` do not match.
fn logged_generation_shape() -> regex::Regex {
    regex::Regex::new(&format!(
        r#"\\*"generation\\*"\s*:\s*\\*"({})\\*""#,
        crate::adapter::registry::TURN_TOKEN_GENERATION
    ))
    .expect("the logged-generation pattern compiles")
}

/// Every turn token in the snapshot and the generation each is bound to.
/// A value counts only when it has the registry's token shape
/// (`<prefix>-<hex12|hex32>-<hex32>`), and then wherever it stands: any
/// table or column, under any JSON key or none, inside escaped JSON
/// (CAD-407). Prose under a `"turn_id"` key is not a token. Generations
/// — spelled inside each token, a generation-shaped `agents.generation`,
/// or a generation-shaped value under a JSON `"generation"` key (CAD-424:
/// an old endpoint's `ready` event outlives every token naming it) — are
/// redacted where they stand alone too. Each occurrence in any text cell
/// is replaced with [`REDACTED`].
fn redact_turn_tokens(conn: &Connection, shape: &regex::Regex) -> Result<Redaction> {
    let generation_shape = regex::Regex::new(&format!(
        "^(?:{})$",
        crate::adapter::registry::TURN_TOKEN_GENERATION
    ))
    .map_err(|e| Error::internal(format!("generation pattern: {e}")))?;
    let mut values = std::collections::BTreeSet::new();
    if has_column(conn, "agents", "generation")? {
        let mut stmt =
            conn.prepare("SELECT DISTINCT generation FROM agents WHERE generation IS NOT NULL")?;
        for value in stmt.query_map([], |r| r.get::<_, String>(0))? {
            let value = value?;
            if generation_shape.is_match(&value) {
                values.insert(value);
            }
        }
    }
    // messages.turn_id is a text cell like any other.
    let logged_generation = logged_generation_shape();
    each_text_cell(conn, |_, _, _, text| {
        for caps in shape.captures_iter(text) {
            values.insert(caps[0].to_string());
            values.insert(caps[1].to_string());
        }
        for caps in logged_generation.captures_iter(text) {
            values.insert(caps[1].to_string());
        }
        Ok(())
    })?;
    if values.is_empty() {
        return Ok(Redaction {
            values: 0,
            cells: 0,
            matcher: None,
        });
    }
    let matcher = aho_corasick::AhoCorasick::builder()
        .match_kind(aho_corasick::MatchKind::LeftmostLongest)
        .build(&values)
        .map_err(|e| Error::internal(format!("token matcher: {e}")))?;
    let mut updates: Vec<(String, String, i64, String)> = Vec::new();
    each_text_cell(conn, |table, column, rowid, text| {
        if matcher.is_match(text) {
            let replaced = matcher.replace_all(text, &vec![REDACTED; values.len()]);
            updates.push((table.into(), column.into(), rowid, replaced));
        }
        Ok(())
    })?;
    let tx = conn.unchecked_transaction()?;
    for (table, column, rowid, text) in &updates {
        tx.execute(
            &format!(
                "UPDATE {} SET {}=?1 WHERE rowid=?2",
                quote_ident(table),
                quote_ident(column)
            ),
            params![text, rowid],
        )?;
    }
    tx.commit()?;
    Ok(Redaction {
        values: values.len(),
        cells: updates.len() as u64,
        matcher: Some(matcher),
    })
}

/// Fail closed: after redaction, no token-shaped value, no redacted
/// generation and no generation under a `"generation"` key may be left
/// anywhere in the file. Each cell is read through JSON `\uXXXX` escapes,
/// so a token or generation spelled with them — which cannot be redacted
/// in place — refuses the export.
fn refuse_remaining_tokens(
    db: &Path,
    shape: &regex::Regex,
    matcher: Option<&aho_corasick::AhoCorasick>,
) -> Result<()> {
    let escape = regex::Regex::new(r"\\u([0-9a-fA-F]{4})")
        .map_err(|e| Error::internal(format!("escape pattern: {e}")))?;
    let logged_generation = logged_generation_shape();
    let conn = crate::store::open_read_only(db)?;
    let mut left: Vec<String> = Vec::new();
    each_text_cell(&conn, |table, column, rowid, text| {
        let text = escape.replace_all(text, |caps: &regex::Captures| {
            u32::from_str_radix(&caps[1], 16)
                .ok()
                .and_then(char::from_u32)
                .map_or_else(|| caps[0].to_string(), String::from)
        });
        let found = shape.is_match(&text)
            || logged_generation.is_match(&text)
            || matcher.is_some_and(|m| m.is_match(text.as_ref()));
        if found && left.len() < LIST_CAP {
            left.push(format!("{table}.{column} rowid {rowid}"));
        }
        Ok(())
    })?;
    if !left.is_empty() {
        return Err(Error::rejected(format!(
            "export refused: turn tokens are still present after redaction ({}); \
             nothing was written",
            left.join("; ")
        )));
    }
    Ok(())
}

struct DbScan {
    cells: u64,
    warnings: Vec<Value>,
}

/// Scan every text cell of every table. Any blocking finding refuses.
fn scan_db(db: &Path, allow: &Allowlist) -> Result<DbScan> {
    let conn = crate::store::open_read_only(db)?;
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type='table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut cells = 0u64;
    let mut block: Vec<Value> = Vec::new();
    let mut warnings: Vec<Value> = Vec::new();
    for table in &tables {
        let mut stmt = conn.prepare(&format!("SELECT rowid, * FROM {}", quote_ident(table)))?;
        let columns: Vec<String> = stmt
            .column_names()
            .iter()
            .skip(1)
            .map(|s| s.to_string())
            .collect();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let rowid: i64 = row.get(0)?;
            for (i, column) in columns.iter().enumerate() {
                let text = match row.get_ref(i + 1)? {
                    ValueRef::Text(bytes) | ValueRef::Blob(bytes) => String::from_utf8_lossy(bytes),
                    _ => continue,
                };
                cells += 1;
                for finding in secret::scan(&text, None)? {
                    if allow.permits(&finding) {
                        continue;
                    }
                    let at = json!({
                        "at": format!("{table}.{column}"),
                        "rowid": rowid,
                        "rule": finding.rule,
                        "redacted": finding.redacted,
                        "fingerprint": finding.fingerprint,
                    });
                    match finding.severity {
                        Severity::Block => block.push(at),
                        Severity::Warn => warnings.push(at),
                    }
                }
            }
        }
    }
    if !block.is_empty() {
        let list = block
            .iter()
            .take(LIST_CAP)
            .map(|f| {
                format!(
                    "{} rowid {}: rule {} ({}) fingerprint {}",
                    f["at"].as_str().unwrap_or_default(),
                    f["rowid"],
                    f["rule"].as_str().unwrap_or_default(),
                    f["redacted"].as_str().unwrap_or_default(),
                    f["fingerprint"].as_str().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let more = block.len().saturating_sub(LIST_CAP);
        let more = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        return Err(Error::invalid(
            "secret_detected",
            format!(
                "export refused: {} credential-shaped value(s) in the store ({list}{more}). \
                 Nothing was written. Redact those rows, or have the operator allowlist a \
                 false positive by rule and fingerprint in <state dir>/{}.",
                block.len(),
                secret::ALLOWLIST_FILE
            ),
        ));
    }
    Ok(DbScan { cells, warnings })
}

// ---------- restore ----------

#[derive(Default)]
pub struct RestoreOptions {
    /// Replace an existing store (after a verified pre-restore backup).
    pub force: bool,
    /// Checkouts on this host; each is matched to a recorded repo by its
    /// `origin` remote.
    pub repos: Vec<PathBuf>,
}

struct Mapping {
    remote: String,
    from: String,
    to: String,
}

/// Restore a backup (its manifest) or an export bundle (its directory)
/// into `state_dir`.
///
/// Core and record copies are jointly verified before anything is
/// activated; record copies are then staged to `app-records/` partials,
/// re-verified (sha, integrity, file schema, identity), and installed
/// without ever deleting the file being replaced first. A manifest with
/// no `app_records` restores core-only and leaves existing record files
/// alone. A manifest with record entries refuses without `--force` when
/// the target already holds a store or any of the listed record files, so
/// a wrong-identity, stale, truncated or mismatched snapshot never
/// overwrites a healthy current file. With `--force`, the existing store
/// (core plus record files) is backed up first; if activation then fails
/// midway, the verified pre-restore backup is the recovery path and any
/// `.replaced-*` asides refuse the next restore and daemon start.
pub fn restore(source: &Path, state_dir: &Path, opts: &RestoreOptions) -> Result<Value> {
    let manifest_path = if source.is_dir() {
        source.join(BUNDLE_MANIFEST)
    } else {
        source.to_path_buf()
    };
    let (manifest, src_db) = verify(&manifest_path)?;
    let manifest_dir = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    // Export bundles never carry record files; a backup manifest with
    // record entries restored from its own directory does.
    if source.is_dir() && !manifest.app_records.is_empty() {
        return Err(Error::rejected(format!(
            "{} is an export bundle and cannot carry record files",
            source.display()
        )));
    }
    let (mappings, unmapped) = plan_remap(&manifest.repos, &opts.repos)?;
    ensure_private_dir(state_dir)?;
    let _lock = lock_state_dir(state_dir)?;
    let leftovers = interrupted_restore_leftovers(state_dir);
    if !leftovers.is_empty() {
        return Err(Error::rejected(format!(
            "an earlier restore into {} was interrupted: {} may hold the previous store. \
             Refusing to restore over it. Inspect them, move the store back to \
             cadence.sqlite3 (and its -wal/-shm) or each record file back to \
             app-records/<install_id>.sqlite3 (and its -wal/-shm) or somewhere safe, then retry",
            state_dir.display(),
            leftovers
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let live = db_file(state_dir);
    let wal = sidecar(&live, "-wal");
    // Existing record files for the installations this manifest lists.
    // Unlisted files are left alone — they may belong to installations
    // the snapshot predates, and user data is never deleted here.
    let mut existing_records: Vec<(String, PathBuf)> = Vec::new();
    for entry in &manifest.app_records {
        let dest = crate::store::app_records::record_db_path(state_dir, &entry.install_id)?;
        if dest.exists()
            || fs::symlink_metadata(sidecar(&dest, "-wal")).is_ok()
            || fs::symlink_metadata(sidecar(&dest, "-shm")).is_ok()
        {
            existing_records.push((entry.install_id.clone(), dest));
        }
    }
    // A target whose record files exist but whose core store is gone
    // cannot take the pre-restore backup below (which needs the core
    // DB): refuse even with --force so nothing is overwritten without a
    // backup. The operator moves the record files aside after inspection.
    if !manifest.app_records.is_empty() && !live.exists() && !existing_records.is_empty() {
        return Err(Error::rejected(format!(
            "{} holds record files but no cadence database; refusing to replace them without a pre-restore backup. Move app-records/ aside after inspection, then retry",
            state_dir.display()
        )));
    }
    let mut pre_restore = None;
    if live.exists() || wal.exists() || !existing_records.is_empty() {
        if !opts.force {
            return Err(Error::rejected(format!(
                "{} already holds a cadence database or record files; refusing to replace them. \
                 Pass --force to replace them: a verified pre-restore backup is \
                 taken into {} first",
                state_dir.display(),
                default_dir(state_dir).display()
            )));
        }
        if live.exists() {
            let taken = backup(
                state_dir,
                &default_dir(state_dir),
                DEFAULT_KEEP,
                PRE_RESTORE,
            )?;
            pre_restore = Some(taken["manifest"].clone());
        }
    }
    // Phase 1: stage the core copy. No target file is touched: the copy
    // is hashed, remapped and integrity-checked as a partial.
    let partial = state_dir.join(format!(".{BUNDLE_DB}.restore-partial"));
    let _ = fs::remove_file(&partial);
    let rows = (|| -> Result<Vec<usize>> {
        copy_private(&src_db, &partial)?;
        let (sha256, _) = hash_file(&partial)?;
        if sha256 != manifest.sha256 {
            return Err(Error::rejected(format!(
                "{} changed while it was being restored",
                src_db.display()
            )));
        }
        let rows = apply_remap(&partial, &mappings)?;
        let (integrity, _) = inspect(&partial)?;
        require_ok(&partial, &integrity)?;
        sync_file(&partial)?;
        Ok(rows)
    })();
    let rows = match rows {
        Ok(rows) => rows,
        Err(error) => {
            let _ = fs::remove_file(&partial);
            return Err(error);
        }
    };
    // Phase 1b: the staged core's installation catalog must admit every
    // record entry — checked here, before any target mutation, against
    // the file that will actually activate (not just the manifest text).
    // `verify` already proved the backup dir's core copy; the staged
    // partial is hash-equal to it, and the per-entry `core_sha256`
    // binding plus this catalog check refuse core-from-A with records
    // from-B. Catalog installations without entries are allowed:
    // contexts predate the first record write.
    if !manifest.app_records.is_empty() {
        let staged_catalog = core_installations(&partial).map_err(|e| {
            let _ = fs::remove_file(&partial);
            Error::rejected(format!(
                "staged core copy has no readable installation catalog: {e}"
            ))
        })?;
        if !manifest.app_installations.is_empty() && manifest.app_installations != staged_catalog {
            let _ = fs::remove_file(&partial);
            return Err(Error::rejected(
                "backup manifest records an installation catalog that differs from its staged core copy; refusing a mixed-generation restore"
                    .to_string(),
            ));
        }
        for entry in &manifest.app_records {
            if entry.core_sha256 != manifest.sha256 {
                let _ = fs::remove_file(&partial);
                return Err(Error::rejected(format!(
                    "record entry for installation {:?} was taken with a different core snapshot; refusing a mixed-generation restore",
                    entry.install_id
                )));
            }
            if !staged_catalog.contains(&entry.install_id) {
                let _ = fs::remove_file(&partial);
                return Err(Error::rejected(format!(
                    "record entry for installation {:?} has no installation in the staged core snapshot; refusing a mixed-generation restore",
                    entry.install_id
                )));
            }
        }
    }
    // Phase 1c: stage every record copy. Every staged partial is
    // re-verified (sha, integrity, file schema, identity) before anything
    // activates; a failure here removes the staged partials and leaves
    // the target — core included — untouched.
    struct StagedRecord {
        install_id: String,
        partial: PathBuf,
        dest: PathBuf,
        sha256: String,
        bytes: u64,
        file_schema: i64,
    }
    let mut staged_records: Vec<StagedRecord> = Vec::new();
    // Owned sidecar paths: `sidecar(&live, ..)` would borrow temporaries.
    let live_wal = sidecar(&live, "-wal");
    let live_shm = sidecar(&live, "-shm");
    let records_dir = state_dir.join(crate::store::app_records::RECORDS_DIR);
    if !manifest.app_records.is_empty() {
        ensure_private_dir(&records_dir)?;
        for entry in &manifest.app_records {
            let src = manifest_dir.join(&entry.db_file);
            let dest = crate::store::app_records::record_db_path(state_dir, &entry.install_id)?;
            let app_partial = sidecar(&dest, ".restore-partial");
            let _ = fs::remove_file(&app_partial);
            let staged = (|| -> Result<StagedRecord> {
                copy_private(&src, &app_partial)?;
                let (sha256, size) = hash_file(&app_partial)?;
                if sha256 != entry.sha256 || size != entry.bytes {
                    return Err(Error::rejected(format!(
                        "record copy {} for installation {:?} changed while it was being restored",
                        src.display(),
                        entry.install_id
                    )));
                }
                let (integrity, file_schema, identity) =
                    inspect_app_record(&app_partial).map_err(|e| {
                        Error::rejected(format!(
                            "record copy {} for installation {:?} is corrupt or foreign: {e}",
                            src.display(),
                            entry.install_id
                        ))
                    })?;
                require_ok(&app_partial, &integrity)?;
                if file_schema != entry.file_schema || identity != entry.install_id {
                    return Err(Error::rejected(format!(
                        "record copy {} for installation {:?} mismatches its manifest",
                        src.display(),
                        entry.install_id
                    )));
                }
                newer_record_schema_refusal(file_schema)?;
                sync_file(&app_partial)?;
                Ok(StagedRecord {
                    install_id: entry.install_id.clone(),
                    partial: app_partial.clone(),
                    dest,
                    sha256: entry.sha256.clone(),
                    bytes: entry.bytes,
                    file_schema: entry.file_schema,
                })
            })();
            match staged {
                Ok(staged) => staged_records.push(staged),
                Err(error) => {
                    let _ = fs::remove_file(&app_partial);
                    for staged in &staged_records {
                        let _ = fs::remove_file(&staged.partial);
                    }
                    let _ = fs::remove_file(&partial);
                    return Err(error);
                }
            }
        }
    }
    // Phase 2: activate the complete staged state at once. Core plus
    // every record file move aside first, then link in; any failure
    // renames every aside back, so a returned error never leaves a
    // partial replacement of a healthy target. Only a process crash
    // between the moves and the cleanup leaves asides behind — those
    // refuse the next restore and daemon start until recovered.
    let mut pairs: Vec<(PathBuf, PathBuf, Vec<PathBuf>)> = vec![(
        partial.clone(),
        live.clone(),
        vec![live.clone(), live_wal, live_shm],
    )];
    for staged in &staged_records {
        pairs.push((
            staged.partial.clone(),
            staged.dest.clone(),
            vec![
                staged.dest.clone(),
                sidecar(&staged.dest, "-wal"),
                sidecar(&staged.dest, "-shm"),
            ],
        ));
    }
    if let Err(error) = activate_all(&pairs) {
        let _ = fs::remove_file(&partial);
        for staged in &staged_records {
            let _ = fs::remove_file(&staged.partial);
        }
        return Err(error);
    }
    sync_dir(state_dir);
    let mut restored_records = Vec::new();
    if !staged_records.is_empty() {
        sync_dir(&records_dir);
        for staged in &staged_records {
            // Tighten to the record file's owner-only mode; a restored
            // copy inherits the backup dir's mode via hard link.
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&staged.dest, fs::Permissions::from_mode(0o600));
            restored_records.push(json!({"install_id": staged.install_id, "db": staged.dest, "sha256": staged.sha256, "bytes": staged.bytes, "file_schema": staged.file_schema}));
        }
    }
    let mut remapped = Vec::new();
    let mut unchanged = Vec::new();
    for (mapping, rows) in mappings.iter().zip(rows) {
        let entry = json!({"remote": mapping.remote, "from": mapping.from,
                           "to": mapping.to, "rows": rows});
        if mapping.from == mapping.to {
            unchanged.push(entry);
        } else {
            remapped.push(entry);
        }
    }
    let mut out = json!({
        "restored": live,
        "from": manifest_path,
        "kind": manifest.kind,
        "schema_version": manifest.schema_version,
        "sha256": manifest.sha256,
        "remapped": remapped,
        "unchanged": unchanged,
        "unmapped": unmapped,
        "app_records": restored_records,
        "pre_restore_backup": pre_restore,
    });
    if manifest.schema_version < SCHEMA_VERSION {
        out["note"] = json!(format!(
            "schema {} is older than this binary's {SCHEMA_VERSION}: the daemon migrates it \
             only under a rollout lease with a backup receipt (`cadence rollout claim`, \
             `cadence rollout backup --path <copy outside the state dir>`)",
            manifest.schema_version
        ));
    }
    Ok(out)
}

/// Match each recorded repo to a checkout on this host by remote. A
/// `--repo` checkout wins. Otherwise the recorded path is kept when it is
/// still a checkout of the same remote. Anything else is unmapped and its
/// paths stay as written.
fn plan_remap(recorded: &[Repo], candidates: &[PathBuf]) -> Result<(Vec<Mapping>, Vec<Repo>)> {
    let mut by_key: BTreeMap<String, PathBuf> = BTreeMap::new();
    for candidate in candidates {
        let dir = candidate.canonicalize().map_err(|e| {
            Error::rejected(format!(
                "--repo {} is not a readable directory: {e}",
                candidate.display()
            ))
        })?;
        let root = repo_root(&dir).ok_or_else(|| {
            Error::rejected(format!(
                "--repo {} is not inside a git checkout",
                candidate.display()
            ))
        })?;
        let remote = origin(&root).ok_or_else(|| {
            Error::rejected(format!(
                "--repo {} has no origin remote to match by",
                root.display()
            ))
        })?;
        let key = remote_key(&remote);
        if let Some(previous) = by_key.get(&key) {
            if previous != &root {
                return Err(Error::rejected(format!(
                    "--repo {} and {} are both checkouts of {key}; pass one",
                    previous.display(),
                    root.display()
                )));
            }
        }
        by_key.insert(key, root);
    }
    let mut mappings = Vec::new();
    let mut unmapped = Vec::new();
    for repo in recorded {
        let key = remote_key(&repo.remote);
        let to = by_key.get(&key).cloned().or_else(|| {
            let path = Path::new(&repo.path);
            let same = repo_root(path).as_deref() == Some(path)
                && origin(path).map(|o| remote_key(&o)).as_deref() == Some(key.as_str());
            same.then(|| path.to_path_buf())
        });
        match to {
            Some(to) => mappings.push(Mapping {
                remote: repo.remote.clone(),
                from: repo.path.clone(),
                to: to.to_string_lossy().into_owned(),
            }),
            None => unmapped.push(repo.clone()),
        }
    }
    Ok((mappings, unmapped))
}

/// Rewrite every path column by its longest matching recorded root.
/// Returns the rows rewritten per mapping.
fn apply_remap(db: &Path, mappings: &[Mapping]) -> Result<Vec<usize>> {
    let mut rows = vec![0usize; mappings.len()];
    if mappings.iter().all(|m| m.from == m.to) {
        return Ok(rows);
    }
    let conn = Connection::open(db)?;
    let tx = conn.unchecked_transaction()?;
    for (table, column) in PATH_COLUMNS {
        if !has_column(&tx, table, column)? {
            continue;
        }
        let values: Vec<(i64, String)> = tx
            .prepare(&format!(
                "SELECT rowid, {column} FROM {table} WHERE {column} IS NOT NULL"
            ))?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (rowid, value) in values {
            let best = mappings
                .iter()
                .enumerate()
                .filter(|(_, m)| m.from != m.to && under(&value, &m.from))
                .max_by_key(|(_, m)| m.from.len());
            if let Some((i, mapping)) = best {
                let rewritten = format!("{}{}", mapping.to, &value[mapping.from.len()..]);
                tx.execute(
                    &format!("UPDATE {table} SET {column}=?1 WHERE rowid=?2"),
                    params![rewritten, rowid],
                )?;
                rows[i] += 1;
            }
        }
    }
    tx.commit()?;
    conn.close().map_err(|(_, e)| e)?;
    Ok(rows)
}

/// `path` is `root` or lies below it.
fn under(path: &str, root: &str) -> bool {
    path == root
        || (path.starts_with(root) && (root.ends_with('/') || path[root.len()..].starts_with('/')))
}

/// Put `partial` at `live` without ever deleting the store it replaces
/// first. Each existing file in `old` (the store and its sidecars — a
/// stale `-wal` must never be replayed onto the restored file) is renamed
/// aside; the new file is then linked in with `hard_link`, which refuses
/// an existing target. On failure every aside file is renamed back. On
/// success the aside files are removed (`--force` took a verified
/// pre-restore backup before this point). Record files install the same
/// way, one installation at a time.
/// Files an interrupted `restore --force` left behind: the previous store
/// (or its sidecars) renamed aside as `cadence.sqlite3*.replaced-*` in the
/// state dir, or a previous record file renamed aside as
/// `app-records/*.sqlite3*.replaced-*`.
/// A restore refuses while any exist, and so does the daemon
/// ([`refuse_interrupted_restore`]).
pub fn interrupted_restore_leftovers(state_dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(state_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with(BUNDLE_DB) && name.contains(".replaced-")
        })
        .map(|e| e.path())
        .collect();
    let records_dir = state_dir.join(crate::store::app_records::RECORDS_DIR);
    if let Ok(entries) = fs::read_dir(&records_dir) {
        out.extend(
            entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().contains(".replaced-"))
                .map(|e| e.path()),
        );
    }
    out.sort();
    out
}

/// CAD-407: refuse to open the store while an interrupted restore's aside
/// files exist. The daemon checks this before anything opens or creates
/// `cadence.sqlite3`: a daemon started over them would build a fresh,
/// empty store next to the previous one. The error names every leftover
/// and the `mv` that recovers each case. CAD-767: record-file asides in
/// `app-records/` refuse the same way — each is put back into its own
/// directory, never the state-dir root.
pub fn refuse_interrupted_restore(state_dir: &Path) -> Result<()> {
    let leftovers = interrupted_restore_leftovers(state_dir);
    if leftovers.is_empty() {
        return Ok(());
    }
    let quote = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', r"'\''"));
    let put_back = leftovers
        .iter()
        .map(|aside| {
            let name = aside.file_name().unwrap_or_default().to_string_lossy();
            let original = name.split(".replaced-").next().unwrap_or_default();
            let parent = aside.parent().unwrap_or(state_dir);
            format!("mv {} {}", quote(aside), quote(&parent.join(original)))
        })
        .collect::<Vec<_>>()
        .join(" && ");
    let listed = leftovers
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let live = db_file(state_dir);
    let recovery = if live.exists() {
        let aside_dir = default_dir(state_dir);
        format!(
            "{live} exists too. If it is the store you restored, move the aside files \
             out of the state dir: mkdir -p {dir} && mv {files} {dir}. If it is not (for \
             example an empty store created after the interruption), move {live} and its \
             -wal/-shm out of the state dir first, then put the previous store back: {put_back}",
            live = live.display(),
            dir = quote(&aside_dir),
            files = leftovers
                .iter()
                .map(|p| quote(p))
                .collect::<Vec<_>>()
                .join(" "),
        )
    } else {
        format!("Put the previous store back: {put_back}")
    };
    Err(Error::rejected(format!(
        "an interrupted restore left {listed} in {}; refusing to open the store there. \
         {recovery}. Then start the daemon again",
        state_dir.display()
    )))
}

/// Rename every aside file back after a failed install. The outcome is
/// part of the error: a failed rollback says where the previous store now
/// is instead of claiming it was put back.
fn put_back(moved: &[(PathBuf, PathBuf)], what: String) -> Error {
    let failed: Vec<String> = moved
        .iter()
        .rev()
        .filter_map(|(from, aside)| {
            fs::rename(aside, from).err().map(|e| {
                format!(
                    "{} is still at {} ({e}); move it back by hand",
                    from.display(),
                    aside.display()
                )
            })
        })
        .collect();
    if failed.is_empty() {
        Error::internal(format!("{what}; the previous store was put back"))
    } else {
        Error::internal(format!(
            "{what}; ROLLBACK FAILED: {}. Do not start a daemon on this state dir \
             until the store is back in place",
            failed.join("; ")
        ))
    }
}

/// Single-pair [`activate_all`], kept for the focused no-clobber tests.
#[cfg(test)]
fn install_no_clobber(partial: &Path, live: &Path, old: &[&Path]) -> Result<()> {
    activate_all(&[(
        partial.to_path_buf(),
        live.to_path_buf(),
        old.iter().map(|p| (*p).to_path_buf()).collect(),
    )])
}

/// Activate staged `pairs` — `(partial, live, olds)` — without ever
/// deleting a file being replaced first. Every existing file in every
/// `olds` (each live file and its `-wal`/`-shm` sidecars — a stale `-wal`
/// must never replay onto a restored file) is renamed aside under one
/// tag; every partial is then hard-linked into place, which refuses an
/// existing target; partials and asides are removed only after every
/// link succeeded. Any failure first unlinks every live path this call
/// created for a previously absent file (those have no aside to put
/// back), then renames every aside back — so a returned error leaves the
/// previous files in place and no new live file behind, never a partial
/// replacement spanning core and record files. Callers stage and fully
/// verify every partial before calling: nothing here checks content.
/// A process crash mid-activation leaves `.replaced-*` asides behind,
/// which refuse the next restore and daemon start until the operator
/// recovers them (see [`interrupted_restore_leftovers`]).
fn activate_all(pairs: &[(PathBuf, PathBuf, Vec<PathBuf>)]) -> Result<()> {
    let tag = format!(
        ".replaced-{}-{}",
        crate::issue::time::basic(epoch_now() as i64),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (_, _, olds) in pairs {
        for old in olds {
            if fs::symlink_metadata(old).is_err() {
                continue;
            }
            let aside = sidecar(old, &tag);
            if let Err(e) = fs::rename(old, &aside) {
                return Err(put_back(
                    &moved,
                    format!("could not move {} aside ({e})", old.display()),
                ));
            }
            moved.push((old.clone(), aside));
        }
    }
    let mut linked: Vec<PathBuf> = Vec::new();
    for (partial, live, _) in pairs {
        if let Err(e) = fs::hard_link(partial, live) {
            for live in linked.iter().rev() {
                if !moved.iter().any(|(from, _)| from == live) {
                    let _ = fs::remove_file(live);
                }
            }
            return Err(put_back(
                &moved,
                format!("could not install {} ({e})", live.display()),
            ));
        }
        linked.push(live.clone());
    }
    for (partial, _, _) in pairs {
        let _ = fs::remove_file(partial);
    }
    for (_, aside) in &moved {
        let _ = fs::remove_file(aside);
    }
    Ok(())
}

/// Hold the daemon singleton lock for the restore. A running daemon
/// holds it for its whole life, so failing to take it means one is up;
/// holding it means none can start mid-restore.
fn lock_state_dir(state_dir: &Path) -> Result<File> {
    let path = state_dir.join("cadence.lock");
    // O_NOFOLLOW: a planted symlink must not redirect the lock (or its
    // creation) outside the state dir.
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|e| {
            Error::rejected(format!(
                "cannot open {} ({e}); it must be a regular file, not a symlink",
                path.display()
            ))
        })?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err(Error::rejected(format!(
            "a cadence daemon is running on {} (it holds cadence.lock); \
             refusing to restore over it. Stop it with `cadence daemon stop` first",
            state_dir.display()
        )));
    }
    Ok(file)
}

// ---------- repos and remotes ----------

/// Every repo checkout the store's path columns point into, with its
/// credential-free `origin`. Paths that no longer exist, or that are not
/// in a checkout with an origin, are skipped.
fn discover_repos(db: &Path) -> Result<Vec<Repo>> {
    let conn = crate::store::open_read_only(db)?;
    let mut values: Vec<String> = Vec::new();
    for (table, column) in PATH_COLUMNS {
        if !has_column(&conn, table, column)? {
            continue;
        }
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT {column} FROM {table} WHERE {column} IS NOT NULL"
        ))?;
        for value in stmt.query_map([], |r| r.get::<_, String>(0))? {
            values.push(value?);
        }
    }
    values.sort();
    values.dedup();
    let mut roots: BTreeMap<PathBuf, String> = BTreeMap::new();
    for value in values {
        let path = Path::new(&value);
        let dir = if path.is_dir() {
            path
        } else {
            match path.parent() {
                Some(parent) if parent.is_dir() => parent,
                _ => continue,
            }
        };
        let Ok(dir) = dir.canonicalize() else {
            continue;
        };
        if roots.keys().any(|root| dir.starts_with(root)) {
            continue;
        }
        let Some(root) = repo_root(&dir) else {
            continue;
        };
        if roots.contains_key(&root) {
            continue;
        }
        if let Some(remote) = origin(&root) {
            roots.insert(root, strip_credentials(&remote));
        }
    }
    Ok(roots
        .into_iter()
        .map(|(path, remote)| Repo {
            path: path.to_string_lossy().into_owned(),
            remote,
        })
        .collect())
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = crate::reaper::spawn(
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .ok()?
    .wait_with_output()
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// The main checkout a path belongs to: a linked worktree resolves to the
/// checkout that owns its object store.
fn repo_root(dir: &Path) -> Option<PathBuf> {
    let common = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = PathBuf::from(common);
    if common.file_name()? != ".git" {
        return None;
    }
    common.parent()?.canonicalize().ok()
}

fn origin(root: &Path) -> Option<String> {
    git(root, &["remote", "get-url", "origin"])
}

/// A remote URL without userinfo: `https://user:token@host/p` becomes
/// `https://host/p`. The scp form (`git@host:p`) has no secret to strip.
pub fn strip_credentials(url: &str) -> String {
    let url = url.trim();
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("{scheme}://{host}{path}")
}

/// The identity two remotes are compared by: lower-case `host/path` with
/// no scheme, user, port, trailing slash or `.git`. `https://github.com/o/r`,
/// `git@github.com:o/r.git` and `ssh://git@github.com:22/o/r` are the same.
pub fn remote_key(url: &str) -> String {
    let url = url.trim();
    let (host, path) = if let Some((_, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        (host.split(':').next().unwrap_or(host), path)
    } else if let Some((left, path)) = url.split_once(':').filter(|(l, _)| !l.contains('/')) {
        (left.rsplit_once('@').map_or(left, |(_, host)| host), path)
    } else {
        ("", url)
    };
    let path = path.trim_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    format!("{host}/{path}").to_ascii_lowercase()
}

// ---------- sqlite and file helpers ----------

/// Copy `src` into the new file `dst` with the online backup API, from a
/// read-only connection, then make `dst` a single rollback-journal file.
fn snapshot(src: &Path, dst: &Path) -> Result<()> {
    let from = crate::store::open_read_only(src)
        .map_err(|e| Error::rejected(format!("cannot open {} read-only: {e}", src.display())))?;
    drop(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dst)?,
    );
    let mut to = Connection::open(dst)?;
    {
        // -1: every page in one step, so the copy is one read snapshot
        // and a concurrent write cannot restart it. A WAL reader does not
        // block the writer. Busy/locked steps retry, bounded.
        use rusqlite::backup::StepResult;
        let backup = Backup::new(&from, &mut to)?;
        let deadline = std::time::Instant::now() + SNAPSHOT_TIMEOUT;
        loop {
            match backup.step(-1)? {
                StepResult::Done => break,
                _ if std::time::Instant::now() >= deadline => {
                    return Err(Error::rejected(format!(
                        "online backup of {} did not finish within {}s (store busy)",
                        src.display(),
                        SNAPSHOT_TIMEOUT.as_secs()
                    )));
                }
                _ => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
    drop(from);
    to.pragma_update(None, "journal_mode", "DELETE")?;
    to.close().map_err(|(_, e)| e)?;
    sync_file(dst)
}

/// `(integrity_check, schema_version)` of a store file, read-only.
fn inspect(db: &Path) -> Result<(String, i64)> {
    let conn = crate::store::open_read_only(db)
        .map_err(|e| Error::rejected(format!("cannot open {}: {e}", db.display())))?;
    let integrity: Vec<String> = conn
        .prepare("PRAGMA integrity_check")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let schema: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .map_err(|e| {
            Error::rejected(format!(
                "{} is not a cadence store (schema_version: {e})",
                db.display()
            ))
        })?;
    Ok((integrity.join("; "), schema))
}

fn require_ok(db: &Path, integrity: &str) -> Result<()> {
    if integrity != "ok" {
        return Err(Error::rejected(format!(
            "{} failed PRAGMA integrity_check: {integrity}",
            db.display()
        )));
    }
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", quote_ident(table)))?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(names.iter().any(|name| name == column))
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut file = File::open(path)
        .map_err(|e| Error::rejected(format!("cannot read {}: {e}", path.display())))?;
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

/// Write `bytes` to `path` atomically, owner-only.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = sidecar(path, ".partial");
    let _ = fs::remove_file(&tmp);
    {
        use std::io::Write;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn copy_private(src: &Path, dst: &Path) -> Result<()> {
    let mut from = File::open(src)?;
    let mut to = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dst)?;
    std::io::copy(&mut from, &mut to)?;
    Ok(())
}

fn ensure_private_dir(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    Ok(())
}

fn sync_file(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

/// Best effort: persist a rename in `dir`.
fn sync_dir(dir: &Path) {
    if let Ok(dir) = File::open(dir) {
        let _ = dir.sync_all();
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn plain_file_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains('/') && !name.contains('\0')
}

fn validate_reason(reason: &str) -> Result<()> {
    let ok = !reason.is_empty()
        && reason.len() <= 32
        && reason
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !ok {
        return Err(Error::rejected(format!(
            "--reason {reason:?}: use 1-32 characters of a-z, 0-9 and '-'"
        )));
    }
    Ok(())
}

fn versions() -> Versions {
    Versions {
        cadence: format!(
            "{}+{}",
            env!("CARGO_PKG_VERSION"),
            crate::overview::BUILD_COMMIT
        ),
        binary_schema: SCHEMA_VERSION,
        sqlite: rusqlite::version().to_string(),
    }
}

fn epoch_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
