//! Rebuildable full-text index of the canonical wiki text tree.
//!
//! The tracker Git tree is the source of truth. The SQLite database is a
//! disposable search cache: a changed wiki tree rebuilds it in one transaction,
//! including moves and removals. No caller identity is stored in the index;
//! every hit is checked against the wiki's existing path allowlist at query time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::{params, Connection, OpenFlags, TransactionBehavior};
use serde_json::{json, Value};

use super::{allowed, normalize, vault_dir, Caller, Op, SEARCH_CAP, TEXT_CAP};
use crate::error::{Error, Result};
use crate::issue::Pm;

const SCHEMA: &str = "wiki-fts-v1";
const CHUNK_BYTES: usize = 4096;

fn db_error(error: rusqlite::Error) -> Error {
    Error::internal(format!("wiki index: {error}"))
}

/// The index file path under the daemon state dir — named so the
/// operator doc, a backup and a test all agree on the disposable file.
pub fn db_path(state_dir: &Path) -> PathBuf {
    state_dir.join("wiki-search.sqlite3")
}

fn open(state_dir: &Path) -> Result<Connection> {
    std::fs::create_dir_all(state_dir)?;
    let conn = Connection::open(db_path(state_dir)).map_err(db_error)?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(db_error)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS wiki_index_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE VIRTUAL TABLE IF NOT EXISTS wiki_index_chunks USING fts5(
             path UNINDEXED, title, heading, body, line UNINDEXED,
             rev UNINDEXED, source UNINDEXED, tokenize='unicode61 remove_diacritics 2'
         );",
    )
    .map_err(db_error)?;
    let check: String = conn
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(db_error)?;
    if check != "ok" {
        return Err(Error::internal(format!(
            "wiki index integrity check: {check}"
        )));
    }
    Ok(conn)
}

fn open_status(state_dir: &Path) -> Result<Option<Connection>> {
    let path = db_path(state_dir);
    if !path.is_file() {
        return Ok(None);
    }
    let conn =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(db_error)?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(db_error)?;
    let check: String = conn
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(db_error)?;
    if check != "ok" {
        return Err(Error::internal(format!(
            "wiki index integrity check: {check}"
        )));
    }
    Ok(Some(conn))
}

/// The database is derived data. Preserve a broken copy for diagnosis and
/// recreate it from the wiki tree; only the refresh/search path repairs it.
fn open_repair(state_dir: &Path) -> Result<Connection> {
    match open(state_dir) {
        Ok(conn) => Ok(conn),
        Err(first) => {
            let diagnosis = first.to_string();
            if !diagnosis.contains("not a database")
                && !diagnosis.contains("malformed")
                && !diagnosis.contains("integrity check")
            {
                return Err(first); // Busy, permissions and I/O errors are not corruption.
            }
            let path = db_path(state_dir);
            if !path.is_file() {
                return Err(first);
            }
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let quarantine = state_dir.join(format!("wiki-search.sqlite3.corrupt-{stamp}"));
            std::fs::rename(&path, &quarantine)?;
            open(state_dir).map_err(|e| {
                Error::internal(format!(
                    "wiki index repair failed after saving {} ({first}): {e}",
                    quarantine.display()
                ))
            })
        }
    }
}

/// Read one `wiki_index_meta` key; `None` when absent.
fn meta_get(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM wiki_index_meta WHERE key=?1",
        [key],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

/// Upsert one `wiki_index_meta` key inside `tx`.
fn meta_put(tx: &rusqlite::Transaction, key: &str, value: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO wiki_index_meta(key,value) VALUES (?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )
    .map_err(db_error)?;
    Ok(())
}

/// What the index freshness is keyed on. The committed Git tree of the vault
/// changes only when the wiki changes — unlike the tracker HEAD, which advances
/// for every issue and report. A vault outside the tracker has no Git tree to
/// observe (`Untracked`): the index then rebuilds on each query until that
/// layout has a committed source of truth, and never reports itself current.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// `git rev-parse HEAD:<vault-rel>` — the committed tree oid, or
    /// `"absent"` when the vault is not yet committed.
    Tree(String),
    /// The vault is not inside the tracker's Git — no revision exists.
    Untracked,
}

impl Source {
    /// The meta `revision` value: the oid, `"absent"`, or `"untracked"`.
    fn value(&self) -> &str {
        match self {
            Source::Tree(t) => t.as_str(),
            Source::Untracked => "untracked",
        }
    }
}

fn source_revision(pm: &Pm, vault: &Path) -> Result<Source> {
    let Ok(rel) = vault.strip_prefix(&pm.dir) else {
        return Ok(Source::Untracked);
    };
    let Some(rel) = rel.to_str().map(|s| s.replace('\\', "/")) else {
        return Ok(Source::Untracked);
    };
    let tree = format!("HEAD:{rel}");
    match crate::issue::git(&pm.dir, &["rev-parse", "--verify", &tree]) {
        Ok(oid) => Ok(Source::Tree(oid)),
        Err(e) => {
            // A tracked vault can be absent before its first wiki write;
            // a failed Git command against an existing path is an error.
            let listing = crate::issue::git(&pm.dir, &["ls-tree", "HEAD", &rel])?;
            if listing.is_empty() {
                Ok(Source::Tree("absent".to_string()))
            } else {
                Err(e)
            }
        }
    }
}

fn pages(vault: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut stack = ["global", "projects", "agents", "users"]
        .into_iter()
        .map(|root| vault.join(root))
        .collect::<Vec<_>>();
    let mut found = Vec::new();
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let meta = path.symlink_metadata()?;
            if meta.file_type().is_symlink() || entry.file_name().to_string_lossy().starts_with('.')
            {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() && meta.len() <= TEXT_CAP {
                let rel = path
                    .strip_prefix(vault)
                    .map_err(|e| Error::internal(e.to_string()))?;
                let name = rel.to_string_lossy().replace('\\', "/");
                if !name.ends_with(".blob") && normalize(&name).is_ok() {
                    found.push((name, path));
                }
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

/// A generated extraction is searchable only while its raw pointer still
/// names the exact bytes it describes. This also handles a failed conversion,
/// a removed PDF, and a PDF move without returning old source text.
fn current_extraction(vault: &Path, path: &str, text: &str) -> bool {
    let Some(source) = path.strip_suffix(".extracted.md") else {
        return true;
    };
    if !text.starts_with("---\nkind: extracted-source\n") {
        return true; // An ordinary page happens to use this suffix.
    }
    let pointer = vault.join(format!("{source}.blob"));
    let Ok(raw) = std::fs::read_to_string(pointer) else {
        return false;
    };
    current_extraction_with_pointer(path, text, Some(&raw))
}

fn current_extraction_with_pointer(path: &str, text: &str, pointer: Option<&str>) -> bool {
    let Some(source) = path.strip_suffix(".extracted.md") else {
        return true;
    };
    if !text.starts_with("---\nkind: extracted-source\n") {
        return true;
    }
    let Some(raw) = pointer else {
        return false;
    };
    let Ok(meta) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    let Some(sha) = meta["sha256"].as_str() else {
        return false;
    };
    let Ok(source_path) = serde_json::to_string(source) else {
        return false;
    };
    text.starts_with(&format!(
        "---\nkind: extracted-source\nsource_path: {source_path}\nsource_sha256: {sha}\n"
    ))
}

/// Read immutable Git objects for one captured wiki tree. A concurrent
/// writer may advance HEAD or change the worktree, but neither can change
/// these objects. Git's NUL record separator handles every accepted path.
fn git_bytes(pm: &Pm, args: &[&str]) -> Result<Vec<u8>> {
    let out = crate::reaper::output(Command::new("git").arg("-C").arg(&pm.dir).args(args))?;
    if !out.status.success() {
        return Err(Error::internal(format!(
            "wiki index git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

fn tracked_entries(pm: &Pm, tree: &str) -> Result<Vec<(String, String)>> {
    if tree == "absent" {
        return Ok(Vec::new());
    }
    let listing = git_bytes(pm, &["ls-tree", "-r", "-z", tree])?;
    let mut found = Vec::new();
    for record in listing.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let Some(tab) = record.iter().position(|b| *b == b'\t') else {
            return Err(Error::internal("wiki index: invalid git tree record"));
        };
        let header = std::str::from_utf8(&record[..tab])
            .map_err(|e| Error::internal(format!("wiki index tree header: {e}")))?;
        let Some(oid) = header
            .strip_prefix("100644 blob ")
            .or_else(|| header.strip_prefix("100755 blob "))
        else {
            continue; // A symlink or gitlink is never a wiki page.
        };
        let Ok(path) = std::str::from_utf8(&record[tab + 1..]) else {
            continue;
        };
        if !matches!(
            path.split('/').next(),
            Some("global" | "projects" | "agents" | "users")
        ) || path.split('/').any(|seg| seg.starts_with('.'))
        {
            continue;
        }
        let logical = path.strip_suffix(".blob").unwrap_or(path);
        if normalize(logical).is_ok() {
            found.push((path.to_string(), oid.to_string()));
        }
    }
    Ok(found)
}

fn split_line(line: &str) -> Vec<&str> {
    if line.len() <= CHUNK_BYTES {
        return vec![line];
    }
    let mut out = Vec::new();
    let mut start = 0;
    while start < line.len() {
        let mut end = (start + CHUNK_BYTES).min(line.len());
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        out.push(&line[start..end]);
        start = end;
    }
    out
}

fn chunks(text: &str) -> Vec<(usize, String, String)> {
    let mut out = Vec::new();
    let mut body = String::new();
    let mut heading = String::new();
    let mut start_line = 1;
    for (i, line) in text.lines().enumerate() {
        if line.starts_with('#') && !body.is_empty() {
            out.push((start_line, heading.clone(), std::mem::take(&mut body)));
        }
        if line.starts_with('#') {
            heading = line.trim_start_matches('#').trim().to_string();
        }
        for part in split_line(line) {
            if !body.is_empty() && body.len() + part.len() + 1 > CHUNK_BYTES {
                out.push((start_line, heading.clone(), std::mem::take(&mut body)));
            }
            if body.is_empty() {
                start_line = i + 1;
            }
            body.push_str(part);
            body.push('\n');
        }
    }
    if !body.is_empty() {
        out.push((start_line, heading, body));
    }
    out
}

/// What one rebuild counted — fed to `wiki_index_meta` and to the
/// operator health surface.
#[derive(Debug, Default, Clone, Copy)]
struct Scan {
    /// Source files whose normalized name is indexable (`.extracted.md`
    /// included, `.blob` and dotfiles excluded).
    pages: usize,
    /// Chunks committed.
    chunks: usize,
    /// Pages skipped for a refused read or a provenance/mixed-source rule.
    skipped: usize,
}

fn rebuild(conn: &mut Connection, pm: &Pm, vault: &Path, source: &Source) -> Result<Scan> {
    let started = Instant::now();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(db_error)?;
    tx.execute("DELETE FROM wiki_index_chunks", [])
        .map_err(db_error)?;
    let mut scan = Scan::default();
    {
        let mut insert = tx
            .prepare("INSERT INTO wiki_index_chunks(path,title,heading,body,line,rev,source) VALUES (?1,?2,?3,?4,?5,?6,?7)")
            .map_err(db_error)?;
        let entries = match source {
            Source::Tree(tree) => tracked_entries(pm, tree)?,
            Source::Untracked => pages(vault)?
                .into_iter()
                .map(|(path, file)| (path, file.to_string_lossy().into_owned()))
                .collect(),
        };
        let mut pointers = HashMap::new();
        if matches!(source, Source::Tree(_)) {
            for (path, oid) in &entries {
                if path.ends_with(".blob") {
                    let bytes = git_bytes(pm, &["cat-file", "blob", oid])?;
                    if bytes.len() <= 4096 {
                        if let Ok(text) = String::from_utf8(bytes) {
                            pointers.insert(path.clone(), text);
                        }
                    }
                }
            }
        }
        for (path, location) in entries {
            if path.ends_with(".blob") {
                continue;
            }
            let bytes = match source {
                Source::Tree(_) => git_bytes(pm, &["cat-file", "blob", &location])?,
                Source::Untracked => std::fs::read(&location)?,
            };
            if bytes.len() as u64 > TEXT_CAP {
                scan.skipped += 1;
                continue;
            }
            let rev = super::rev_bytes(&bytes);
            let Ok(text) = String::from_utf8(bytes) else {
                scan.skipped += 1;
                continue;
            };
            scan.pages += 1;
            let extraction_ok = match source {
                Source::Tree(_) => {
                    let pointer = format!("{}.blob", path.trim_end_matches(".extracted.md"));
                    current_extraction_with_pointer(
                        &path,
                        &text,
                        pointers.get(&pointer).map(String::as_str),
                    )
                }
                Source::Untracked => current_extraction(vault, &path, &text),
            };
            if !extraction_ok {
                scan.skipped += 1;
                continue;
            }
            let title = text
                .lines()
                .find_map(|line| line.strip_prefix("# "))
                .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(&path));
            let page_source = path.strip_suffix(".extracted.md").unwrap_or(&path);
            for (line, heading, body) in chunks(&text) {
                insert
                    .execute(params![
                        path,
                        title,
                        heading,
                        body,
                        line as i64,
                        rev,
                        page_source
                    ])
                    .map_err(db_error)?;
                scan.chunks += 1;
            }
        }
    }
    // Rows + revision commit atomically: a reader on another connection
    // sees either the old complete snapshot or this one — never half.
    meta_put(&tx, "revision", source.value())?;
    meta_put(&tx, "schema", SCHEMA)?;
    meta_put(&tx, "last_refresh_at", &format!("{:.3}", epoch_now()))?;
    meta_put(
        &tx,
        "last_refresh_ms",
        &started.elapsed().as_millis().to_string(),
    )?;
    meta_put(&tx, "pages_indexed", &scan.pages.to_string())?;
    meta_put(&tx, "chunks_indexed", &scan.chunks.to_string())?;
    meta_put(&tx, "skipped_pages", &scan.skipped.to_string())?;
    tx.commit().map_err(db_error)?;
    Ok(scan)
}

/// The stored `(schema, revision)` pair — `None` when either key is
/// absent (a fresh or wiped index).
fn stored_meta(conn: &Connection) -> Option<(String, String)> {
    let (schema, revision) = (meta_get(conn, "schema")?, meta_get(conn, "revision")?);
    Some((schema, revision))
}

/// Why the committed snapshot does not match the stored one — the
/// freshness reason the health surface reports and `ensure_current`
/// acts on. `None` = the index already mirrors the committed tree.
fn stale_reason(source: &Source, stored: Option<&(String, String)>) -> Option<String> {
    match source {
        Source::Untracked => Some(
            "the vault is not inside the tracker's Git — no committed tree \
             to prove freshness against; every query rebuilds"
                .to_string(),
        ),
        Source::Tree(tree) => match stored {
            None => Some("no index snapshot yet".to_string()),
            Some((schema, _)) if schema != SCHEMA => Some(format!(
                "schema changed ({schema} → {SCHEMA}); a rebuild rewrites the index"
            )),
            Some((_, old)) if old != tree => Some(format!(
                "the wiki tree advanced past the indexed revision ({old} → {tree})"
            )),
            Some(_) => None,
        },
    }
}

/// One process-wide writer for the index: two threads reaching
/// `ensure_current` together take turns — the second sees the first's
/// committed revision and skips its own rebuild (the "no rebuild storm"
/// guarantee, also exercised between the refresh worker and a search).
static INDEX_WRITE: Mutex<()> = Mutex::new(());

/// True when `conn`'s index already mirrors `source` — schema current,
/// revision equal, and the vault tracked.
fn is_current(conn: &Connection, source: &Source) -> bool {
    !matches!(source, Source::Untracked)
        && stored_meta(conn).is_some_and(|(schema, old)| schema == SCHEMA && old == source.value())
}

/// A committed-wiki refresh that leaves the index current for `source`.
/// Returns the count of rows committed plus the wall-clock duration —
/// `None` when no rebuild was needed. Serialized on [`INDEX_WRITE`];
/// a second caller re-checks inside the lock and no-ops when the first
/// already landed the revision.
fn refresh_locked(pm: &Pm, state_dir: &Path, force: bool) -> Result<Option<(Scan, Duration)>> {
    let vault = vault_dir(pm)?;
    let _write = INDEX_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let source = source_revision(pm, &vault)?;
    let mut conn = open_repair(state_dir)?;
    if !force && is_current(&conn, &source) {
        return Ok(None);
    }
    let started = Instant::now();
    let scan = rebuild(&mut conn, pm, &vault, &source)?;
    Ok(Some((scan, started.elapsed())))
}

/// The search path's correctness fallback: open the index, rebuild it
/// when it does not already mirror the committed wiki tree (or on every
/// query for an untracked vault), and hand back the connection. A read
/// never waits behind a refresh the writer already finished — the lock
/// is dropped before the query runs.
fn ensure_current(pm: &Pm, state_dir: &Path) -> Result<Connection> {
    let _ = refresh_locked(pm, state_dir, false)?;
    open(state_dir)
}

fn query_terms(q: &str) -> Result<String> {
    if q.len() > 256 {
        return Err(Error::rejected("wiki search refused: query over 256 bytes"));
    }
    let terms = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .take(12)
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Err(Error::rejected("wiki search refused: empty query"));
    }
    Ok(terms.join(" AND "))
}

/// Wall-clock seconds for the `last_refresh_at` meta value.
fn epoch_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// What one refresh run recorded — the fields `wiki_index_meta` keeps
/// and the health surface reports.
#[derive(Debug, Clone)]
struct Run {
    at: f64,
    duration_ms: u64,
    outcome: &'static str,
    detail: Option<String>,
}

/// The shared scheduler state a mutation signals and the refresh
/// worker drains — plus the last run's record, which the health
/// surface reads.
struct RefreshState {
    /// A mutation is committed and waiting for the worker.
    needed: bool,
    /// The daemon is closing — the worker exits instead of running.
    closing: bool,
    /// The refresh worker is mid-rebuild right now.
    running: bool,
    /// The most recent finished (or failed) refresh.
    last: Option<Run>,
}

impl Default for RefreshState {
    fn default() -> Self {
        Self {
            needed: false,
            closing: false,
            running: false,
            last: None,
        }
    }
}

/// The post-commit eager refresh: one coalesced rebuild per burst of
/// committed wiki mutations, on a dedicated worker, with the query-time
/// tree check kept as the correctness fallback.
///
/// `kick()` is cheap and infallible — a mutation calls it after its
/// commit lands, never holds the write path for the rebuild, and never
/// fails the write if the refresh later fails (that failure is recorded
/// in `last` and surfaces via [`status`]). The worker takes
/// [`INDEX_WRITE`], so a burst collapses to one rebuild and a search
/// that lands mid-refresh sees either the prior complete snapshot or
/// the new one — never a torn index.
pub struct IndexRefresh {
    state_dir: PathBuf,
    /// The daemon's tracker dir — `Pm::at` is re-opened per run so a
    /// worker rebuild observes the live tracker; the lease, when held,
    /// is re-attached so the rebuild reads through the same fence.
    pm_dir: PathBuf,
    /// The hosting lease a `Pm` opened for refresh must carry — `None`
    /// for an unleased daemon.
    lease: Option<crate::lease::PmLease>,
    state: Mutex<RefreshState>,
    wake: Condvar,
}

impl IndexRefresh {
    /// A detached scheduler over the daemon's tracker dir and lease.
    /// [`Self::spawn`] runs the worker; `lease` is the daemon's
    /// [`crate::lease::LeaseCtl::pm_lease`] when one is held.
    pub fn new(
        state_dir: PathBuf,
        pm_dir: PathBuf,
        lease: Option<crate::lease::PmLease>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state_dir,
            pm_dir,
            lease,
            state: Mutex::new(RefreshState::default()),
            wake: Condvar::new(),
        })
    }

    /// Open the tracker for one refresh, carrying the daemon's lease so
    /// a fenced daemon's read-side rebuild still refuses.
    fn open_pm(&self) -> Result<Pm> {
        let mut pm = Pm::at(&self.pm_dir)?;
        if let Some(lease) = &self.lease {
            pm.attach_lease(lease.clone());
        }
        Ok(pm)
    }

    /// Signal that a wiki mutation committed: mark refresh needed and
    /// wake the worker. Multiple commits before the worker runs collapse
    /// into one rebuild.
    pub fn kick(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.needed = true;
        self.wake.notify_all();
    }

    /// The worker loop: wait for a kick, run one coalesced refresh,
    /// record the outcome, repeat until `close`. A failed refresh is
    /// captured into `last` (and therefore `status`), never panics the
    /// worker. A new commit kicks it again; a search retries inline and
    /// refuses stale results if repair still fails.
    fn run(&self) {
        loop {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            while !s.needed && !s.closing {
                s = self.wake.wait(s).unwrap_or_else(|e| e.into_inner());
            }
            if s.closing {
                return;
            }
            // Take the job: clear needed up front so a kick that lands
            // during the rebuild re-marks it for the next pass, and mark
            // running so a second caller cannot double-run.
            s.needed = false;
            s.running = true;
            drop(s);

            let at = epoch_now();
            let pm = match self.open_pm() {
                Ok(pm) => pm,
                Err(e) => {
                    self.finish(
                        Run {
                            at,
                            duration_ms: 0,
                            outcome: "error",
                            detail: Some(format!("cannot open tracker: {e}")),
                        },
                        true,
                    );
                    continue;
                }
            };
            let run = match refresh_locked(&pm, &self.state_dir, false) {
                Ok(Some((scan, dur))) => Run {
                    at,
                    duration_ms: dur.as_millis() as u64,
                    outcome: "ok",
                    detail: Some(format!(
                        "rebuilt: {} pages, {} chunks",
                        scan.pages, scan.chunks
                    )),
                },
                Ok(None) => Run {
                    at,
                    duration_ms: 0,
                    outcome: "ok",
                    detail: Some("already current".to_string()),
                },
                Err(e) => Run {
                    at,
                    duration_ms: 0,
                    outcome: "error",
                    detail: Some(e.to_string()),
                },
            };
            self.finish(run, true);
            // On failure, stay idle until a new commit kicks us. Search
            // still retries synchronously and refuses stale results.
        }
    }

    /// Store the finished run's record (used by `run`, and by
    /// [`Self::refresh`] when an operator drives a rebuild directly).
    fn finish(&self, run: Run, worker: bool) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if worker {
            s.running = false;
        }
        s.last = Some(run);
    }

    /// Run one refresh inline (the operator's explicit `refresh` verb).
    /// Goes through the same [`INDEX_WRITE`] as the worker, and always
    /// rebuilds from the committed tree, including when already current.
    pub fn refresh(&self) -> Result<Value> {
        let at = epoch_now();
        let pm = self.open_pm()?;
        match refresh_locked(&pm, &self.state_dir, true) {
            Ok(Some((scan, dur))) => {
                self.finish(
                    Run {
                        at,
                        duration_ms: dur.as_millis() as u64,
                        outcome: "ok",
                        detail: Some(format!(
                            "rebuilt: {} pages, {} chunks",
                            scan.pages, scan.chunks
                        )),
                    },
                    false,
                );
                Ok(json!({"refreshed": true, "pages": scan.pages,
                          "chunks": scan.chunks, "duration_ms": dur.as_millis() as u64}))
            }
            Ok(None) => {
                self.finish(
                    Run {
                        at,
                        duration_ms: 0,
                        outcome: "ok",
                        detail: Some("already current".to_string()),
                    },
                    false,
                );
                Ok(json!({"refreshed": false, "reason": "already current"}))
            }
            Err(e) => {
                self.finish(
                    Run {
                        at,
                        duration_ms: 0,
                        outcome: "error",
                        detail: Some(e.to_string()),
                    },
                    false,
                );
                Err(e)
            }
        }
    }

    /// The operator health surface: schema, committed vs indexed tree,
    /// freshness and its reason, the last refresh's timing and outcome,
    /// indexed page/chunk counts, skipped pages and the last error.
    /// Read-only — it never rebuilds; the committed tree is read live so
    /// a stale index is reported, not hidden.
    pub fn status(&self) -> Result<Value> {
        let pm = self.open_pm()?;
        let vault = vault_dir(&pm)?;
        let source = source_revision(&pm, &vault)?;
        let (conn, index_error) = match open_status(&self.state_dir) {
            Ok(conn) => (conn, None),
            Err(e) => (None, Some(e.to_string())),
        };
        let stored = conn.as_ref().and_then(stored_meta);
        let fresh = index_error
            .clone()
            .or_else(|| stale_reason(&source, stored.as_ref()));
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let last = s.last.clone().map(|r| {
            json!({"at": r.at, "duration_ms": r.duration_ms,
                   "outcome": r.outcome, "detail": r.detail})
        });
        let indexed = stored.as_ref().map(|(_, rev)| rev.clone());
        let schema_stored = stored.as_ref().map(|(sc, _)| sc.clone());
        let num = |k: &str| {
            conn.as_ref()
                .and_then(|c| meta_get(c, k))
                .and_then(|v| v.parse::<u64>().ok())
        };
        Ok(json!({
            "schema": SCHEMA,
            "schema_stored": schema_stored,
            // The live committed tree vs the tree the index last committed.
            "wiki_tree": source.value(),
            "indexed_tree": indexed,
            "tracked": !matches!(source, Source::Untracked),
            // current = the index mirrors the committed tree.
            "current": fresh.is_none(),
            "stale_reason": fresh,
            "pages_indexed": num("pages_indexed"),
            "chunks_indexed": num("chunks_indexed"),
            "skipped_pages": num("skipped_pages"),
            "last_successful_refresh_at": conn.as_ref().and_then(|c| meta_get(c, "last_refresh_at")).and_then(|v| v.parse::<f64>().ok()),
            "last_successful_refresh_ms": num("last_refresh_ms"),
            "index_error": index_error,
            "last_error": s.last.as_ref().filter(|r| r.outcome == "error").and_then(|r| r.detail.clone()),
            "last_refresh": last,
            "refresh_pending": s.needed,
            "refresh_running": s.running,
            // Where the disposable index file lives, for backup docs.
            "index_file": db_path(&self.state_dir),
            "repair": "`cadence wiki index refresh` rebuilds now; a corrupt or \
                       absent file is rebuilt on the next search or refresh",
        }))
    }

    /// Stop the worker after the current run. Idempotent.
    pub fn close(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.closing = true;
        self.wake.notify_all();
    }

    /// Spawn the refresh worker thread; the returned handle is joined at
    /// shutdown after `close`.
    pub fn spawn(self: &Arc<Self>) -> thread::JoinHandle<()> {
        let me = Arc::clone(self);
        thread::Builder::new()
            .name("wiki-index-refresh".to_string())
            .spawn(move || me.run())
            .expect("spawn wiki-index-refresh")
    }
}

pub fn search(pm: &Pm, state_dir: &Path, caller: &Caller, q: &str, base: &str) -> Result<Value> {
    let norm = normalize(base)?;
    let segs = if norm.is_empty() {
        Vec::new()
    } else {
        norm.split('/').collect::<Vec<_>>()
    };
    allowed(caller, Op::Read, &segs)?;
    let expression = query_terms(q)?;
    let vault = vault_dir(pm)?;
    // A write may commit between the first freshness check and the
    // query. Validate the same connection's indexed revision after the
    // read; retry against the new committed tree or fail under churn.
    for _ in 0..3 {
        let conn = ensure_current(pm, state_dir)?;
        let matches = search_from_connection(&conn, caller, &norm, &expression)?;
        let source = source_revision(pm, &vault)?;
        if matches!(source, Source::Untracked) || is_current(&conn, &source) {
            return Ok(json!({"q":q,"base":norm,"matches":matches}));
        }
    }
    Err(Error::internal(
        "wiki search: committed tree changed repeatedly during query; retry",
    ))
}

fn search_from_connection(
    conn: &Connection,
    caller: &Caller,
    norm: &str,
    expression: &str,
) -> Result<Vec<Value>> {
    let mut stmt = conn
        .prepare(
            "SELECT path,line,rev,heading,source,
                    snippet(wiki_index_chunks,3,'','','…',32),
                    bm25(wiki_index_chunks,0.0,6.0,3.0,1.0)
             FROM wiki_index_chunks WHERE wiki_index_chunks MATCH ?1
             ORDER BY 7, path, line",
        )
        .map_err(db_error)?;
    let rows = stmt
        .query_map([expression], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, f64>(6)?,
            ))
        })
        .map_err(db_error)?;
    let mut matches = Vec::new();
    for row in rows {
        let (path, line, rev, heading, source, snippet, score) = row.map_err(db_error)?;
        if !norm.is_empty() && path != norm && !path.starts_with(&format!("{norm}/")) {
            continue;
        }
        let rel = path.split('/').collect::<Vec<_>>();
        if allowed(caller, Op::Read, &rel).is_err() {
            continue;
        }
        matches.push(json!({"path":path,"line":line,"text":snippet.trim(),
                            "heading":heading,"rev":rev,"source":source,"score":-score}));
        if matches.len() >= SEARCH_CAP {
            break;
        }
    }
    Ok(matches)
}
