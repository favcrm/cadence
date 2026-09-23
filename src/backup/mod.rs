//! Backup, export and restore of the daemon store (CAD-314).
//!
//! - [`backup`] copies the live store with SQLite's online backup API from
//!   a read-only connection. In WAL mode that reader never blocks the
//!   daemon's writer, and the copy is one consistent snapshot. The copy is
//!   switched to a single self-contained file, checked with
//!   `PRAGMA integrity_check`, hashed, and described by a manifest (format,
//!   schema version, sha256, binary versions, repo remotes). The manifest
//!   is written last, then the pair is re-verified from disk. `keep` prunes
//!   the oldest backups *with the same reason* in that directory. Age comes
//!   from the stamp in the file name (then mtime), never from manifest
//!   content; the pair just written is never pruned; a copy is deleted
//!   only when it is the regular file named `<manifest stem>.sqlite3` and
//!   its sha256 and size match the manifest. A hand-made copy is never
//!   pruned.
//! - [`export`] is the portable form: the same snapshot with endpoint
//!   tokens nulled and freed pages dropped (`VACUUM`), then every text cell
//!   is run through the CAD-109 secret scan. One blocking finding refuses
//!   the export and removes the bundle directory. Nothing else in the
//!   state dir is ever read into the bundle.
//! - [`restore`] verifies a manifest and its copy, refuses a schema newer
//!   than this binary, refuses a state dir whose daemon holds
//!   `cadence.lock` (and holds that lock itself while it works), refuses
//!   to replace an existing store without `force` (and backs that store up
//!   first when forced), then rewrites repo paths by matching remote URLs.
//! - [`before_self_update`] is the backup a self-update takes before it
//!   swaps the binary. `cadence upgrade` (`upgrade::run`) calls it before
//!   it installs anything or moves the link, and refuses on `Err`.
//!
//! Nothing here opens the live store for writing.

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
/// running turn reports with. `pid` is a process on the source host.
const SCRUB_COLUMNS: &[(&str, &str)] = &[
    ("agents", "generation"),
    ("agents", "pid"),
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
    "operator config: secret-allowlist.toml, intake-relay.yaml",
    ".env files",
    "state-dir folders: private/, sessions/, briefings/, reviews/, agents/, roles/, backups/",
    "provider auth (Claude, Codex, Devin, Cursor sign-in state in their own dirs): never read",
    "endpoint tokens: agents.generation (turn-token generation), messages.turn_id (turn tokens) and agents.pid are set to NULL",
    "turn tokens and generations elsewhere (event payloads, message text): every known value is replaced with [redacted]; the export refuses if any remain",
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
}

/// The default backup directory: `<state dir>/backups`.
pub fn default_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("backups")
}

// ---------- backup ----------

/// Take a verified backup of `<state_dir>/cadence.sqlite3` into `dir`,
/// then prune that directory to the newest `keep` backups with `reason`.
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
    let taken = (|| -> Result<Manifest> {
        snapshot(&live, &partial)?;
        let (integrity, schema) = inspect(&partial)?;
        require_ok(&partial, &integrity)?;
        let repos = discover_repos(&partial)?;
        fs::rename(&partial, &db)?;
        let (sha256, bytes) = hash_file(&db)?;
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
        };
        write_private(&manifest_path, &serde_json::to_vec_pretty(&manifest)?)?;
        sync_dir(dir);
        verify(&manifest_path)?;
        Ok(manifest)
    })();
    let manifest = match taken {
        Ok(manifest) => manifest,
        Err(error) => {
            for path in [&partial, &db, &manifest_path] {
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
    // The pair this call promises must still be on disk.
    for path in [&db, &manifest_path] {
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
        "manifest": manifest_path,
        "db": db,
        "sha256": manifest.sha256,
        "bytes": manifest.bytes,
        "schema_version": manifest.schema_version,
        "integrity_check": manifest.integrity_check,
        "reason": manifest.reason,
        "repos": manifest.repos,
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
/// `<manifest stem>.sqlite3` next to its manifest and its sha256 and size
/// match; otherwise the pair is reported under `skipped` and left alone.
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
        match fs::symlink_metadata(&db) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // A manifest without its copy is inert: drop it.
            }
            Ok(meta) if meta.file_type().is_file() => {
                let matches = hash_file(&db)
                    .is_ok_and(|(sha, size)| sha == manifest.sha256 && size == manifest.bytes);
                if !matches {
                    out.skipped.push(json!({"manifest": manifest_path, "db": db,
                        "why": "the copy does not match its manifest (sha256/size)"}));
                    continue;
                }
                // The copy goes first: a manifest without its copy is
                // inert, a copy without its manifest is never pruned again.
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

/// Read a manifest and prove its copy: format, schema no newer than this
/// binary, sha256, integrity, and the schema recorded inside the copy.
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
    Ok((manifest, db))
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
    // known value everywhere before the columns are nulled.
    let redaction = redact_turn_tokens(&conn)?;
    let mut scrubbed = Vec::new();
    for (table, column) in SCRUB_COLUMNS {
        if has_column(&conn, table, column)? {
            conn.execute(&format!("UPDATE {table} SET {column}=NULL"), [])?;
            scrubbed.push(format!("{table}.{column}"));
        }
    }
    conn.execute_batch("VACUUM")?;
    conn.close().map_err(|(_, e)| e)?;
    if let Some(matcher) = &redaction.matcher {
        refuse_remaining_tokens(db, matcher)?;
    }
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

/// Shortest value treated as a token: shorter strings are too likely to
/// occur in unrelated text.
const MIN_TOKEN_LEN: usize = 8;

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

/// Every turn token and generation the snapshot knows: `messages.turn_id`,
/// `agents.generation`, every `"turn_id": "…"` value in any text cell
/// (events outlive their messages), and the generation spelled inside
/// each `<prefix>-<generation>-<uuid>` token. Each occurrence in any text
/// cell is replaced with [`REDACTED`].
fn redact_turn_tokens(conn: &Connection) -> Result<Redaction> {
    let mut values = std::collections::BTreeSet::new();
    for (table, column) in [("messages", "turn_id"), ("agents", "generation")] {
        if has_column(conn, table, column)? {
            let mut stmt = conn.prepare(&format!(
                "SELECT DISTINCT {column} FROM {table} WHERE {column} IS NOT NULL"
            ))?;
            for value in stmt.query_map([], |r| r.get::<_, String>(0))? {
                values.insert(value?);
            }
        }
    }
    let keyed = regex::Regex::new(r#""turn_id"\s*:\s*"([^"\\]+)""#)
        .map_err(|e| Error::internal(format!("turn_id pattern: {e}")))?;
    each_text_cell(conn, |_, _, _, text| {
        for caps in keyed.captures_iter(text) {
            values.insert(caps[1].to_string());
        }
        Ok(())
    })?;
    let generations: Vec<String> = values
        .iter()
        .filter_map(|token| {
            let mut parts = token.splitn(3, '-');
            let (_prefix, generation, _id) = (parts.next()?, parts.next()?, parts.next()?);
            Some(generation.to_string())
        })
        .collect();
    values.extend(generations);
    values.retain(|v| v.len() >= MIN_TOKEN_LEN && v != REDACTED);
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

/// Fail closed: after redaction, no known token or generation may be
/// left anywhere in the file.
fn refuse_remaining_tokens(db: &Path, matcher: &aho_corasick::AhoCorasick) -> Result<()> {
    let conn = crate::store::open_read_only(db)?;
    let mut left: Vec<String> = Vec::new();
    each_text_cell(&conn, |table, column, rowid, text| {
        if matcher.is_match(text) && left.len() < LIST_CAP {
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
pub fn restore(source: &Path, state_dir: &Path, opts: &RestoreOptions) -> Result<Value> {
    let manifest_path = if source.is_dir() {
        source.join(BUNDLE_MANIFEST)
    } else {
        source.to_path_buf()
    };
    let (manifest, src_db) = verify(&manifest_path)?;
    let (mappings, unmapped) = plan_remap(&manifest.repos, &opts.repos)?;
    ensure_private_dir(state_dir)?;
    let _lock = lock_state_dir(state_dir)?;
    let leftovers = interrupted_restore_leftovers(state_dir);
    if !leftovers.is_empty() {
        return Err(Error::rejected(format!(
            "an earlier restore into {} was interrupted: {} may hold the previous store. \
             Refusing to restore over it. Inspect them, move the store back to \
             cadence.sqlite3 (and its -wal/-shm) or somewhere safe, then retry",
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
    let shm = sidecar(&live, "-shm");
    let mut pre_restore = None;
    if live.exists() || wal.exists() {
        if !opts.force {
            return Err(Error::rejected(format!(
                "{} already holds a cadence database; refusing to replace it. \
                 Pass --force to replace it: a verified pre-restore backup is \
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
    let partial = state_dir.join(format!(".{BUNDLE_DB}.restore-partial"));
    let _ = fs::remove_file(&partial);
    let placed = (|| -> Result<Vec<usize>> {
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
        install_no_clobber(&partial, &live, &[&live, &wal, &shm])?;
        sync_dir(state_dir);
        Ok(rows)
    })();
    let rows = match placed {
        Ok(rows) => rows,
        Err(error) => {
            let _ = fs::remove_file(&partial);
            return Err(error);
        }
    };
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
/// pre-restore backup before this point).
/// Files an interrupted `restore --force` left behind: the previous store
/// (or its sidecars) renamed aside as `cadence.sqlite3*.replaced-*`.
/// A restore refuses while any exist, and `daemon start` warns.
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
    out.sort();
    out
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

fn install_no_clobber(partial: &Path, live: &Path, old: &[&Path]) -> Result<()> {
    let tag = format!(
        ".replaced-{}-{}",
        crate::issue::time::basic(epoch_now() as i64),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for path in old {
        if fs::symlink_metadata(path).is_err() {
            continue;
        }
        let aside = sidecar(path, &tag);
        if let Err(e) = fs::rename(path, &aside) {
            return Err(put_back(
                &moved,
                format!("could not move {} aside ({e})", path.display()),
            ));
        }
        moved.push((path.to_path_buf(), aside));
    }
    if let Err(e) = fs::hard_link(partial, live) {
        return Err(put_back(
            &moved,
            format!("could not install {} ({e})", live.display()),
        ));
    }
    let _ = fs::remove_file(partial);
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
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
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
