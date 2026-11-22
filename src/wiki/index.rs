//! Rebuildable full-text index of the canonical wiki text tree.
//!
//! The tracker Git tree is the source of truth. The SQLite database is a
//! disposable search cache: a changed wiki tree rebuilds it in one transaction,
//! including moves and removals. Separate project and personal corpora prevent
//! inaccessible pages from influencing a caller's search ranking; every hit is
//! also checked against the wiki's path allowlist at query time.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};

use super::{allowed, normalize_dir, vault_dir, Caller, Op, SEARCH_CAP};
use crate::error::{Error, Result};
use crate::issue::Pm;

mod scan;

use scan::{rebuild, source_revision, stale_reason, stored_meta, Scan, Source};

// v4 repartitions project memory and personal wiki paths, including older
// cached rows, before a search can score a corpus for a member.
pub(super) const SCHEMA: &str = "wiki-fts-v4";

/// The FTS table for paths without a project or personal owner.
pub(super) const INDEX_GLOBAL: &str = "wiki_index_global";
pub(super) const CHUNK_BYTES: usize = 4096;

/// `bm25` uses table-wide statistics, so project and personal pages need
/// separate corpora rather than post-ranking path filtering.
pub(super) fn table_for(path: &str) -> String {
    if let Some(key) = super::project_of_path(path) {
        format!("wiki_index_p_{}", hex_of(key))
    } else if let Some(owner) = path
        .strip_prefix("users/")
        .and_then(|rest| rest.split('/').next())
        .filter(|owner| !owner.is_empty())
    {
        format!("wiki_index_u_{}", hex_of(owner))
    } else {
        INDEX_GLOBAL.to_string()
    }
}

/// A table name suffix. Hex, not the key itself: a project key is user data,
/// and a table name is interpolated into SQL.
fn hex_of(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(text.len() * 2);
    for byte in text.bytes() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

/// Every chunk table currently in the database.
pub(super) fn chunk_tables(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            // The virtual tables only: an FTS5 table's shadow tables share its
            // prefix, and dropping the virtual table already drops them — a
            // second drop would fail the rebuild.
            "SELECT name FROM sqlite_master WHERE type='table'
             AND sql LIKE 'CREATE VIRTUAL TABLE%'
             AND (name = ?1 OR name LIKE 'wiki_index_p_%' OR name LIKE 'wiki_index_u_%')",
        )
        .map_err(db_error)?;
    let names = stmt
        .query_map([INDEX_GLOBAL], |row| row.get::<_, String>(0))
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    Ok(names)
}

pub(super) fn create_chunks(conn: &Connection, table: &str) -> Result<()> {
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE {table} USING fts5(
             path UNINDEXED, title, heading, body, line UNINDEXED,
             rev UNINDEXED, source UNINDEXED, tokenize='unicode61 remove_diacritics 2'
         );"
    ))
    .map_err(db_error)
}

pub(super) fn db_error(error: rusqlite::Error) -> Error {
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
        "CREATE TABLE IF NOT EXISTS wiki_index_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )
    .map_err(db_error)?;
    // The v2 index kept every page in one table, which is what made a private
    // page part of the corpus that ranks everyone else's. It is derived data:
    // drop it and let the next refresh rebuild into the partitioned tables.
    let legacy: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='wiki_index_chunks')",
            [],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if legacy {
        conn.execute_batch(
            "DROP TABLE wiki_index_chunks;
             DELETE FROM wiki_index_meta WHERE key IN ('revision','schema');",
        )
        .map_err(db_error)?;
    }
    let global_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [INDEX_GLOBAL],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if !global_exists {
        conn.execute_batch("DELETE FROM wiki_index_meta WHERE key IN ('revision','schema');")
            .map_err(db_error)?;
        create_chunks(&conn, INDEX_GLOBAL)?;
    }
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
    let chunks_exist: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [INDEX_GLOBAL],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if !chunks_exist {
        return Err(Error::internal("wiki index FTS table is missing"));
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
pub(super) fn meta_get(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM wiki_index_meta WHERE key=?1",
        [key],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

/// Upsert one `wiki_index_meta` key inside `tx`.
pub(super) fn meta_put(tx: &rusqlite::Transaction, key: &str, value: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO wiki_index_meta(key,value) VALUES (?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )
    .map_err(db_error)?;
    Ok(())
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
pub(super) fn epoch_now() -> f64 {
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
#[derive(Default)]
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
    /// shutdown after `close`. Refusal is reported, never a panic.
    pub(crate) fn spawn(self: &Arc<Self>) -> Option<crate::thread_budget::Budgeted<()>> {
        let me = Arc::clone(self);
        match crate::thread_budget::spawn_named("wiki-index-refresh".to_string(), move || me.run())
        {
            Ok(handle) => Some(handle),
            Err(e) => {
                eprintln!("cadence: wiki index refresh not started: {e}");
                None
            }
        }
    }
}

pub fn search(pm: &Pm, state_dir: &Path, caller: &Caller, q: &str, base: &str) -> Result<Value> {
    search_with_hook(pm, state_dir, caller, q, base, || {})
}

fn search_with_hook(
    pm: &Pm,
    state_dir: &Path,
    caller: &Caller,
    q: &str,
    base: &str,
    mut after_hits: impl FnMut(),
) -> Result<Value> {
    let norm = normalize_dir(base)?;
    let segs = if norm.is_empty() {
        Vec::new()
    } else {
        norm.split('/').collect::<Vec<_>>()
    };
    allowed(pm, caller, Op::Read, &segs)?;
    let expression = query_terms(q)?;
    let vault = vault_dir(pm)?;
    // A write may commit between the first freshness check and the
    // query. Pin hits and metadata to one SQLite read transaction,
    // compare Git before and after it, then retry on any mismatch.
    // Without the explicit transaction a background refresh could
    // advance metadata after the rows are read and make old hits look
    // current against the new Git tree.
    for _ in 0..3 {
        let mut conn = ensure_current(pm, state_dir)?;
        let before = source_revision(pm, &vault)?;
        let tx = conn.transaction().map_err(db_error)?;
        // Read metadata first to establish the SQLite snapshot before
        // any concurrent index commit can land.
        let indexed = stored_meta(&tx);
        let matches = search_from_connection(&tx, pm, caller, &norm, &expression)?;
        after_hits();
        let after = source_revision(pm, &vault)?;
        let current = matches!(after, Source::Untracked)
            || (before == after
                && indexed
                    .as_ref()
                    .is_some_and(|(schema, rev)| schema == SCHEMA && rev == after.value()));
        tx.rollback().map_err(db_error)?;
        if current {
            return Ok(json!({"q":q,"base":norm,"matches":matches}));
        }
    }
    Err(Error::internal(
        "wiki search: committed tree changed repeatedly during query; retry",
    ))
}

fn search_from_connection(
    conn: &Connection,
    pm: &Pm,
    caller: &Caller,
    norm: &str,
    expression: &str,
) -> Result<Vec<Value>> {
    // CAD-2187: authorization runs before ranking. Each project has its own
    // table, so a query scores only the corpus it may read — the alternative
    // (one table plus a `WHERE`) removes private rows from the result but not
    // from the statistics `bm25` scores with, which is an observable channel.
    let readable: Vec<String> =
        crate::project_policy::readable(pm, &super::policy_spelling(caller))
            .into_iter()
            .collect();
    let existing = chunk_tables(conn)?;
    let mut tables: Vec<String> = vec![INDEX_GLOBAL.to_string()];
    for key in &readable {
        let table = table_for(&format!("projects/{key}"));
        if existing.contains(&table) {
            tables.push(table);
        }
    }
    match caller {
        Caller::Operator => tables.extend(
            existing
                .iter()
                .filter(|table| table.starts_with("wiki_index_u_"))
                .cloned(),
        ),
        Caller::User(owner) => {
            let table = format!("wiki_index_u_{}", hex_of(owner));
            if existing.contains(&table) {
                tables.push(table);
            }
        }
        _ => {}
    }
    let mut matches = Vec::new();
    for table in tables {
        let sql = format!(
            "SELECT path,line,rev,heading,source,
                    snippet({table},3,'','','…',32),
                    bm25({table},0.0,6.0,3.0,1.0)
             FROM {table} WHERE {table} MATCH ?1
             ORDER BY 7, path, line"
        );
        let mut stmt = conn.prepare(&sql).map_err(db_error)?;
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
        let mut found = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_error)?;
        matches.append(&mut found);
    }
    // Scores come from different corpora, so they order within a project and
    // not across one: the tie-break is the path, which is stable and readable.
    matches.sort_by(|a, b| {
        a.6.partial_cmp(&b.6)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
    });
    let mut out = Vec::new();
    for (path, line, rev, heading, source, snippet, score) in matches {
        if !norm.is_empty() && path != norm && !path.starts_with(&format!("{norm}/")) {
            continue;
        }
        if super::project_of_path(&path).is_some_and(|key| {
            readable
                .binary_search_by(|project| project.as_str().cmp(key))
                .is_err()
        }) {
            continue;
        }
        // The table already bounds the corpus by project; the coarse table
        // still decides `users/` and `agents/` — and re-checking costs nothing.
        let rel = path.split('/').collect::<Vec<_>>();
        if !super::allowed_by_table(caller, Op::Read, &rel).is_ok() {
            continue;
        }
        out.push(json!({"path":path,"line":line,"text":snippet.trim(),
                        "heading":heading,"rev":rev,"source":source,"score":-score}));
        if out.len() >= SEARCH_CAP {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CAD-2187: a private project's pages are not in a caller's results, and
    /// the filter is a literal project prefix rather than a pattern — a
    /// readable `a-b` must not admit the resembling `axb`. (CAD-2215 refuses a
    /// tracker holding a key outside `[a-z0-9-]`, so `a_b` is no longer a key.)
    #[test]
    fn search_returns_only_the_projects_the_caller_may_read() {
        let root = tempfile::TempDir::new().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        for (key, policy) in [
            (
                "secret",
                "policy:\n  version: 3\n  visibility: private\n  members:\n  - users/mia\n",
            ),
            (
                "a-b",
                "policy:\n  version: 3\n  visibility: private\n  members:\n  - users/rob\n",
            ),
            (
                "axb",
                "policy:\n  version: 3\n  visibility: private\n  members:\n  - users/mia\n",
            ),
        ] {
            let dir = pm.dir.join(key);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("project.yaml"),
                format!(
                    "key: {key}\nprefix: {}\n{policy}",
                    key.replace('-', "").to_uppercase()
                ),
            )
            .unwrap();
        }
        for path in [
            "projects/secret/theodolite.md",
            "projects/a-b/theodolite.md",
            "projects/axb/theodolite.md",
            "global/theodolite.md",
        ] {
            super::super::write(&pm, &Caller::Operator, path, "# Page\ntheodolite\n", None)
                .unwrap();
        }
        let state = root.path().join("state");
        let paths = |caller: &Caller| -> Vec<String> {
            let found = search_with_hook(&pm, &state, caller, "theodolite", "", || {}).unwrap();
            found["matches"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["path"].as_str().unwrap().to_string())
                .collect()
        };

        // The grant reads its project; a stranger reads neither it nor the
        // project whose key merely resembles it.
        let mia = Caller::User("mia".to_string());
        let rob = Caller::User("rob".to_string());
        let theirs = paths(&mia);
        assert!(
            theirs.iter().any(|p| p.starts_with("projects/secret/")),
            "{theirs:?}"
        );
        assert!(
            theirs.iter().any(|p| p.starts_with("projects/axb/")),
            "{theirs:?}"
        );
        assert!(
            !theirs.iter().any(|p| p.starts_with("projects/a-b/")),
            "{theirs:?}"
        );
        let mine = paths(&rob);
        assert!(
            mine.iter().any(|p| p.starts_with("projects/a-b/")),
            "{mine:?}"
        );
        assert!(
            !mine.iter().any(|p| p.starts_with("projects/secret/")),
            "{mine:?}"
        );
        assert!(
            !mine.iter().any(|p| p.starts_with("projects/axb/")),
            "{mine:?}"
        );
        // `global/` has no project, so every caller still searches it.
        for found in [&theirs, &mine] {
            assert!(found.iter().any(|p| p.starts_with("global/")), "{found:?}");
        }
    }

    #[test]
    fn search_retries_when_a_removal_commits_after_hits_are_read() {
        let root = tempfile::TempDir::new().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        super::super::write(
            &pm,
            &Caller::Operator,
            "global/old.md",
            "# Old\nretired theodolite\n",
            None,
        )
        .unwrap();
        let state = root.path().join("state");
        let mut removed = false;
        let result = search_with_hook(&pm, &state, &Caller::Operator, "theodolite", "", || {
            if !removed {
                super::super::rm(&pm, &Caller::Operator, "global/old.md").unwrap();
                removed = true;
            }
        })
        .unwrap();
        assert!(result["matches"].as_array().unwrap().is_empty(), "{result}");
    }
}
