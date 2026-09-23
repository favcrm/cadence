//! Verified backups, portable bundles and restore (CAD-314).
//!
//! `cadence backup` copies the live `cadence.sqlite3` with SQLite's
//! `VACUUM INTO` (a consistent snapshot of a WAL database, taken while
//! the daemon keeps writing), reopens the copy read-only, requires
//! `PRAGMA integrity_check` = `ok` and the source's schema version, and
//! writes a manifest beside it. Pruning (`--keep N`) only ever removes a
//! copy that one of our manifests names.
//!
//! `cadence export --bundle` puts one verified copy, its manifest, a
//! repo map (project → remote + local path from the tracker's
//! project.yaml files) and the briefings dir into one tar. Every text
//! member is secret-scanned first; a blocking finding refuses the export
//! and nothing is written. Provider logs are never included.
//!
//! `cadence restore` refuses a state dir with a running daemon or an
//! existing database, verifies checksum, integrity and schema, then
//! installs the database and prints a repo remap plan. It never reads or
//! writes the tracker git repo.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::rollout::{db_file, SCHEMA_VERSION};

/// Manifest `kind`; pruning recognises our backups by it.
pub const MANIFEST_KIND: &str = "cadence.backup/1";
/// Repo map `kind` inside a bundle.
pub const REPO_MAP_KIND: &str = "cadence.repo-map/1";
/// Default `--keep`.
pub const DEFAULT_KEEP: usize = 7;

const FILE_PREFIX: &str = "cadence-backup-";
const DB_SUFFIX: &str = ".sqlite3";
const MANIFEST_SUFFIX: &str = ".manifest.json";
const BUNDLE_DB: &str = "cadence.sqlite3";
const BUNDLE_MANIFEST: &str = "manifest.json";
const BUNDLE_REPO_MAP: &str = "repo-map.json";
const BUNDLE_BRIEFINGS: &str = "briefings";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub kind: String,
    /// The copy's file name, a sibling of the manifest (or the bundle
    /// member name).
    pub file: String,
    pub sha256: String,
    pub bytes: u64,
    pub schema_version: i64,
    pub build_commit: String,
    /// ISO-8601 UTC.
    pub created_at: String,
    pub created_at_epoch: f64,
    /// The live database the copy was taken from.
    pub source: String,
}

/// Default backup directory: `<state>/backups`.
pub fn default_dest(state_dir: &Path) -> PathBuf {
    state_dir.join("backups")
}

// ---------- backup ----------

/// `cadence backup`: one verified copy + manifest in `dest` (default
/// `<state>/backups`), then prune our oldest copies beyond `keep`.
pub fn backup(state_dir: &Path, dest: Option<&Path>, keep: usize) -> Result<Value> {
    if keep == 0 {
        return Err(Error::rejected("--keep must be at least 1"));
    }
    let source = live_db(state_dir)?;
    let dest = dest
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_dest(state_dir));
    create_private_dir(&dest)?;
    let now = crate::rollout::unix_now();
    let stamp = crate::issue::time::basic(now as i64);
    let unique = uuid::Uuid::new_v4().simple().to_string();
    let stem = format!("{FILE_PREFIX}{stamp}-{}", &unique[..8]);
    let file = format!("{stem}{DB_SUFFIX}");
    let manifest = copy_verified(&source, &dest, &file, now)?;
    let manifest_path = dest.join(format!("{stem}{MANIFEST_SUFFIX}"));
    write_new_file(&manifest_path, &manifest_bytes(&manifest)?)?;
    let pruned = prune(&dest, keep)?;
    Ok(json!({
        "backup": dest.join(&file),
        "manifest_path": manifest_path,
        "manifest": manifest,
        "verified": {"integrity_check": "ok", "schema_version": manifest.schema_version},
        "keep": keep,
        "pruned": pruned,
        "next": format!(
            "restore with `cadence restore {} --state-dir <empty dir>`",
            manifest_path.display()
        ),
    }))
}

/// The backup `cadence upgrade` takes before it installs anything. A
/// state dir with no database yet has nothing to protect; that is
/// reported, not refused.
pub fn pre_update(state_dir: &Path) -> Result<Value> {
    if !db_file(state_dir).exists() {
        return Ok(json!({
            "skipped": format!("no database at {} yet", db_file(state_dir).display()),
        }));
    }
    backup(state_dir, None, DEFAULT_KEEP).map_err(|e| {
        Error::rejected(format!(
            "upgrade refused before installing anything: the pre-upgrade backup failed ({e}). \
             Fix that, check with `cadence backup`, then retry the upgrade"
        ))
    })
}

fn live_db(state_dir: &Path) -> Result<PathBuf> {
    let source = db_file(state_dir);
    if !source.is_file() {
        return Err(Error::rejected(format!(
            "no database at {}: nothing to back up. Check --state-dir (the daemon \
             creates the database on `cadence daemon start`)",
            source.display()
        )));
    }
    Ok(source)
}

/// Snapshot `source` into `dir/file` and verify it. The copy appears
/// under its final name only after it verified.
fn copy_verified(source: &Path, dir: &Path, file: &str, now: f64) -> Result<Manifest> {
    let conn = open_source(source)?;
    let schema = schema_of(&conn)?.ok_or_else(|| {
        Error::rejected(format!(
            "{} has no cadence schema_version table; refusing to back up a \
             foreign database",
            source.display()
        ))
    })?;
    let target = dir.join(file);
    let partial = dir.join(format!(".{file}.partial"));
    let _ = std::fs::remove_file(&partial);
    let staged = (|| -> Result<(String, u64)> {
        conn.execute("VACUUM INTO ?1", [partial.to_string_lossy().as_ref()])?;
        drop(conn);
        // The snapshot keeps the source's WAL flag in its header; a
        // standalone copy is a rollback-journal database.
        let copy = Connection::open(&partial)?;
        copy.query_row("PRAGMA journal_mode=DELETE", [], |_| Ok(()))?;
        drop(copy);
        File::open(&partial)?.sync_all()?;
        let copy_schema = verify_integrity(&partial)?;
        if copy_schema != Some(schema) {
            return Err(Error::internal(format!(
                "backup copy has schema {copy_schema:?}, source has {schema}"
            )));
        }
        sha256_file(&partial)
    })();
    let (sha256, bytes) = match staged {
        Ok(done) => done,
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            return Err(e);
        }
    };
    if target.exists() {
        let _ = std::fs::remove_file(&partial);
        return Err(Error::rejected(format!(
            "{} already exists; refusing to overwrite it",
            target.display()
        )));
    }
    std::fs::rename(&partial, &target)?;
    Ok(Manifest {
        kind: MANIFEST_KIND.into(),
        file: file.into(),
        sha256,
        bytes,
        schema_version: schema,
        build_commit: crate::overview::BUILD_COMMIT.into(),
        created_at: crate::issue::time::iso(now as i64),
        created_at_epoch: now,
        source: source.display().to_string(),
    })
}

/// Read the live database without writing it: a read-only connection
/// never checkpoints or deletes the WAL. A WAL database whose `-shm` is
/// missing cannot be opened read-only, so fall back to a normal
/// connection (the snapshot is still read-only work).
fn open_source(source: &Path) -> Result<Connection> {
    let ro = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
        Ok(conn)
    });
    match ro {
        Ok(conn) => Ok(conn),
        Err(_) => {
            let conn = Connection::open(source)?;
            conn.busy_timeout(std::time::Duration::from_secs(10))?;
            Ok(conn)
        }
    }
}

fn schema_of(conn: &Connection) -> Result<Option<i64>> {
    let table: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if table.is_none() {
        return Ok(None);
    }
    Ok(conn
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .optional()?)
}

/// Reopen `path` read-only, require `PRAGMA integrity_check` = `ok`,
/// and return its schema version.
pub fn verify_integrity(path: &Path) -> Result<Option<i64>> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let rows: Vec<String> = {
        let mut stmt = conn.prepare("PRAGMA integrity_check")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    if rows != ["ok"] {
        return Err(Error::rejected(format!(
            "{} failed PRAGMA integrity_check: {}",
            path.display(),
            rows.join("; ")
        )));
    }
    schema_of(&conn)
}

fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        hasher.update(&buf[..n]);
    }
    Ok((hex(&hasher.finalize()), total))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn manifest_bytes(manifest: &Manifest) -> Result<Vec<u8>> {
    let mut out = serde_json::to_vec_pretty(manifest)?;
    out.push(b'\n');
    Ok(out)
}

fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| Error::rejected(format!("cannot create {}: {e}", dir.display())))
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| Error::rejected(format!("cannot create {}: {e}", path.display())))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// A manifest in `dir` that we wrote: our `kind`, a plain
/// `cadence-backup-*.sqlite3` file name, and the manifest's own name
/// derived from it. Anything else is not ours and is never touched.
fn our_manifest(dir: &Path, name: &str) -> Option<Manifest> {
    let stem = name.strip_suffix(MANIFEST_SUFFIX)?;
    if !stem.starts_with(FILE_PREFIX) {
        return None;
    }
    let path = dir.join(name);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    let expected = format!("{stem}{DB_SUFFIX}");
    (manifest.kind == MANIFEST_KIND && manifest.file == expected && plain_name(&manifest.file))
        .then_some(manifest)
}

fn plain_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none()
}

/// Remove our oldest backups beyond `keep`. Returns the removed copies.
fn prune(dir: &Path, keep: usize) -> Result<Vec<String>> {
    let mut ours: Vec<(f64, String, Manifest)> = Vec::new();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(manifest) = our_manifest(dir, &name) {
            ours.push((manifest.created_at_epoch, name, manifest));
        }
    }
    ours.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.1.cmp(&a.1))
    });
    let mut pruned = Vec::new();
    for (_, name, manifest) in ours.into_iter().skip(keep) {
        let copy = dir.join(&manifest.file);
        let is_file = std::fs::symlink_metadata(&copy)
            .map(|m| m.is_file())
            .unwrap_or(false);
        if is_file {
            std::fs::remove_file(&copy)?;
        }
        std::fs::remove_file(dir.join(&name))?;
        pruned.push(copy.display().to_string());
    }
    Ok(pruned)
}

// ---------- export ----------

/// `cadence export --bundle FILE`: one tar with a verified database
/// copy, its manifest, `repo-map.json` and the briefings dir. Refuses
/// before writing anything when a text member carries a blocking
/// secret finding.
pub fn export(state_dir: &Path, bundle: &Path, pm_dir: &Path) -> Result<Value> {
    if bundle.exists() {
        return Err(Error::rejected(format!(
            "{} already exists; export never overwrites. Pass a new --bundle path",
            bundle.display()
        )));
    }
    let source = live_db(state_dir)?;
    let staging =
        tempfile::tempdir().map_err(|e| Error::internal(format!("export staging dir: {e}")))?;
    let now = crate::rollout::unix_now();
    let manifest = copy_verified(&source, staging.path(), BUNDLE_DB, now)?;

    // Text members: (bundle path, bytes). Scanned before any write.
    let mut texts: Vec<(String, Vec<u8>)> = vec![
        (BUNDLE_MANIFEST.into(), manifest_bytes(&manifest)?),
        (BUNDLE_REPO_MAP.into(), repo_map_bytes(pm_dir)?),
    ];
    texts.extend(briefings(state_dir)?);
    let allow = crate::secret::Allowlist::load(state_dir)?;
    for (name, bytes) in &texts {
        let text = String::from_utf8_lossy(bytes);
        let block: Vec<_> = crate::secret::scan(&text, Some(name))?
            .into_iter()
            .filter(|f| !allow.permits(f) && f.severity == crate::secret::Severity::Block)
            .collect();
        if !block.is_empty() {
            return Err(crate::secret::refusal(
                &format!("export bundle member {name}"),
                &block,
            ));
        }
    }

    let partial = sibling_partial(bundle);
    let written = (|| -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)
            .map_err(|e| Error::rejected(format!("cannot create {}: {e}", partial.display())))?;
        let mut tar = tar::Builder::new(file);
        let db = std::fs::read(staging.path().join(BUNDLE_DB))?;
        append(&mut tar, BUNDLE_DB, &db, now)?;
        for (name, bytes) in &texts {
            append(&mut tar, name, bytes, now)?;
        }
        tar.into_inner()?.sync_all()?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    std::fs::rename(&partial, bundle)?;
    let members: Vec<&str> = std::iter::once(BUNDLE_DB)
        .chain(texts.iter().map(|(n, _)| n.as_str()))
        .collect();
    Ok(json!({
        "bundle": bundle,
        "manifest": manifest,
        "members": members,
        "secret_scan": {"scanned": texts.len(), "blocking": 0},
        "next": format!(
            "restore on the new host with `cadence restore {} --state-dir <empty dir>`",
            bundle.display()
        ),
    }))
}

fn sibling_partial(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".partial");
    PathBuf::from(name)
}

fn append(tar: &mut tar::Builder<File>, name: &str, bytes: &[u8], now: f64) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o600);
    header.set_mtime(now as u64);
    header.set_entry_type(tar::EntryType::Regular);
    tar.append_data(&mut header, name, bytes)?;
    Ok(())
}

/// `repo-map.json`: every project.yaml repo as project → remote + path.
/// A missing tracker is an empty map, not an error.
fn repo_map_bytes(pm_dir: &Path) -> Result<Vec<u8>> {
    let mut repos = Vec::new();
    for project in crate::issue::project::list(pm_dir)? {
        for repo in &project.repos {
            repos.push(json!({
                "project": project.key,
                "remote": repo.remote,
                "path": repo.path,
            }));
        }
    }
    let map = json!({"kind": REPO_MAP_KIND, "repos": repos});
    let mut out = serde_json::to_vec_pretty(&map)?;
    out.push(b'\n');
    Ok(out)
}

/// Regular files under `<state>/briefings`, as `briefings/<rel>`.
/// Symlinks are skipped: they could point anywhere.
fn briefings(state_dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let root = state_dir.join(BUNDLE_BRIEFINGS);
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                let rel = path.strip_prefix(&root).unwrap_or(&path);
                let Some(rel) = rel.to_str() else {
                    continue;
                };
                out.push((format!("{BUNDLE_BRIEFINGS}/{rel}"), std::fs::read(&path)?));
            }
        }
    }
    out.sort();
    Ok(out)
}

// ---------- restore ----------

struct Staged {
    manifest: Manifest,
    db: PathBuf,
    repo_map: Option<Value>,
    briefings: Vec<(PathBuf, Vec<u8>)>,
}

/// `cadence restore FILE`: FILE is an export bundle or a backup
/// manifest (`cadence-backup-*.manifest.json`).
pub fn restore(state_dir: &Path, file: &Path) -> Result<Value> {
    let state_arg = state_dir.display();
    match crate::rollout::daemon_lock_holder(state_dir) {
        Ok(None) => {}
        Ok(Some(pid)) => {
            return Err(Error::rejected(format!(
                "a daemon (pid {pid}) is running on {state_arg}; restore never replaces \
                 a live database. Restore into an empty dir with `cadence restore {} \
                 --state-dir <empty dir>`, or stop it with `cadence --state-dir {state_arg} \
                 daemon stop` first",
                file.display()
            )))
        }
        Err(e) => {
            return Err(Error::rejected(format!(
                "cannot prove no daemon is running on {state_arg} ({e}); restore refused"
            )))
        }
    }
    let target = db_file(state_dir);
    let existing: Vec<String> = ["", "-wal", "-shm"]
        .iter()
        .map(|s| format!("{}{s}", target.display()))
        .filter(|p| Path::new(p).exists())
        .collect();
    if !existing.is_empty() {
        return Err(Error::rejected(format!(
            "{} already exists; restore never overwrites a database. Restore with \
             `cadence restore {} --state-dir <empty dir>`, or move {} aside first",
            existing[0],
            file.display(),
            existing.join(", ")
        )));
    }
    create_private_dir(state_dir)?;
    let staging = tempfile::Builder::new()
        .prefix(".restore-")
        .tempdir_in(state_dir)
        .map_err(|e| Error::internal(format!("restore staging dir: {e}")))?;
    let staged = stage(file, staging.path())?;
    let manifest = &staged.manifest;

    let (sha, _) = sha256_file(&staged.db)?;
    if sha != manifest.sha256 {
        return Err(Error::rejected(format!(
            "checksum mismatch: {} has sha256 {sha}, the manifest says {}. The copy is \
             damaged or not the one the manifest describes; nothing was restored",
            file.display(),
            manifest.sha256
        )));
    }
    if manifest.schema_version > SCHEMA_VERSION {
        return Err(Error::rejected(format!(
            "the backup has schema {} but this binary supports up to {SCHEMA_VERSION}; \
             install a newer build (`cadence upgrade --latest-main`) and restore with it",
            manifest.schema_version
        )));
    }
    let schema = verify_integrity(&staged.db)?;
    if schema != Some(manifest.schema_version) {
        return Err(Error::rejected(format!(
            "the copy has schema {schema:?} but the manifest says {}; nothing was restored",
            manifest.schema_version
        )));
    }

    // `hard_link` fails when the target exists: no clobber even if a
    // database appeared since the check above.
    std::fs::hard_link(&staged.db, &target).map_err(|e| {
        Error::rejected(format!(
            "cannot install {} ({e}); nothing was restored",
            target.display()
        ))
    })?;
    let mut restored = Vec::new();
    let mut skipped = Vec::new();
    for (rel, bytes) in &staged.briefings {
        let dest = state_dir.join(rel);
        if dest.exists() {
            skipped.push(dest.display().to_string());
            continue;
        }
        if let Some(parent) = dest.parent() {
            create_private_dir(parent)?;
        }
        write_new_file(&dest, bytes)?;
        restored.push(dest.display().to_string());
    }
    let remap = remap_plan(staged.repo_map.as_ref());
    let mut next = format!("start the daemon: `cadence --state-dir {state_arg} daemon start`");
    if manifest.schema_version < SCHEMA_VERSION {
        next.push_str(&format!(
            ". The database is schema {} and this binary migrates it to {SCHEMA_VERSION}; \
             that crossing needs a rollout lease with a backup receipt \
             (`cadence rollout claim`, `cadence backup --dest <dir outside the state dir>`, \
             `cadence rollout backup --path <copy>`)",
            manifest.schema_version
        ));
    }
    Ok(json!({
        "restored": target,
        "from": file,
        "manifest": manifest,
        "verified": {"sha256": true, "integrity_check": "ok", "schema_version": schema},
        "briefings_restored": restored,
        "briefings_skipped_existing": skipped,
        "repo_remap": remap,
        "next": next,
    }))
}

fn stage(file: &Path, staging: &Path) -> Result<Staged> {
    let mut head = [0u8; 1];
    let mut f = File::open(file)
        .map_err(|e| Error::rejected(format!("cannot read {}: {e}", file.display())))?;
    let n = f.read(&mut head)?;
    if n == 1 && head[0] == b'{' {
        stage_manifest(file, staging)
    } else {
        stage_bundle(file, staging)
    }
}

fn parse_manifest(bytes: &[u8], from: &Path) -> Result<Manifest> {
    let manifest: Manifest = serde_json::from_slice(bytes).map_err(|e| {
        Error::rejected(format!(
            "{} is not a cadence backup manifest: {e}",
            from.display()
        ))
    })?;
    if manifest.kind != MANIFEST_KIND || !plain_name(&manifest.file) {
        return Err(Error::rejected(format!(
            "{} is not a {MANIFEST_KIND} manifest",
            from.display()
        )));
    }
    Ok(manifest)
}

fn stage_manifest(file: &Path, staging: &Path) -> Result<Staged> {
    let manifest = parse_manifest(&std::fs::read(file)?, file)?;
    let dir = file.parent().unwrap_or(Path::new("."));
    let copy = dir.join(&manifest.file);
    let db = staging.join(BUNDLE_DB);
    std::fs::copy(&copy, &db).map_err(|e| {
        Error::rejected(format!(
            "the manifest names {} but it cannot be read ({e})",
            copy.display()
        ))
    })?;
    Ok(Staged {
        manifest,
        db,
        repo_map: None,
        briefings: Vec::new(),
    })
}

fn stage_bundle(file: &Path, staging: &Path) -> Result<Staged> {
    let bad = |what: String| {
        Error::rejected(format!(
            "{} is not a cadence export bundle: {what}",
            file.display()
        ))
    };
    let mut archive = tar::Archive::new(File::open(file)?);
    let mut manifest = None;
    let mut db = None;
    let mut repo_map = None;
    let mut briefings = Vec::new();
    for entry in archive.entries().map_err(|e| bad(e.to_string()))? {
        let mut entry = entry.map_err(|e| bad(e.to_string()))?;
        let path = entry.path().map_err(|e| bad(e.to_string()))?.into_owned();
        if entry.header().entry_type() != tar::EntryType::Regular
            || !path.components().all(|c| matches!(c, Component::Normal(_)))
        {
            return Err(bad(format!("unexpected member {}", path.display())));
        }
        let name = path.to_string_lossy().to_string();
        if name == BUNDLE_DB {
            let dest = staging.join(BUNDLE_DB);
            let mut out = File::create(&dest)?;
            std::io::copy(&mut entry, &mut out)?;
            out.sync_all()?;
            db = Some(dest);
            continue;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        if name == BUNDLE_MANIFEST {
            manifest = Some(parse_manifest(&bytes, &path)?);
        } else if name == BUNDLE_REPO_MAP {
            repo_map = Some(serde_json::from_slice(&bytes).map_err(|e| bad(e.to_string()))?);
        } else if path.starts_with(BUNDLE_BRIEFINGS) && path.components().count() > 1 {
            briefings.push((path, bytes));
        } else {
            return Err(bad(format!("unexpected member {name}")));
        }
    }
    let manifest = manifest.ok_or_else(|| bad(format!("no {BUNDLE_MANIFEST}")))?;
    let db = db.ok_or_else(|| bad(format!("no {BUNDLE_DB}")))?;
    Ok(Staged {
        manifest,
        db,
        repo_map,
        briefings,
    })
}

/// One row per bundled repo: its recorded local path when that checkout
/// still has the same remote, else "choose folder".
fn remap_plan(map: Option<&Value>) -> Value {
    let Some(repos) = map.and_then(|m| m["repos"].as_array()) else {
        return json!([]);
    };
    let rows: Vec<Value> = repos
        .iter()
        .map(|repo| {
            let remote = repo["remote"].as_str();
            let recorded = repo["path"].as_str();
            let found = recorded
                .map(crate::issue::project::expand_home)
                .filter(|p| p.is_dir())
                .and_then(|p| {
                    let (_, actual) = crate::issue::project::repo_identity(&p)?;
                    let want = remote.map(crate::issue::project::normalize_remote);
                    (want.is_none() || actual == want).then_some(p)
                });
            json!({
                "project": repo["project"],
                "remote": remote,
                "recorded_path": recorded,
                "local_path": found,
                "action": if found.is_some() { "found" } else { "choose folder" },
            })
        })
        .collect();
    json!(rows)
}

#[cfg(test)]
mod tests;
