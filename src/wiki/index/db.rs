//! SQLite FTS schema, table naming, connection setup, and metadata helpers for the wiki index.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags};

use crate::error::{Error, Result};

// v4 repartitions project memory and personal wiki paths, including older
// cached rows, before a search can score a corpus for a member.
pub(super) const SCHEMA: &str = "wiki-fts-v4";

/// The FTS table for paths without a project or personal owner.
pub(super) const INDEX_GLOBAL: &str = "wiki_index_global";
pub(super) const CHUNK_BYTES: usize = 4096;

/// `bm25` uses table-wide statistics, so project and personal pages need
/// separate corpora rather than post-ranking path filtering.
pub(in super::super) fn table_for(path: &str) -> String {
    if let Some(key) = super::super::project_of_path(path) {
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
pub(super) fn hex_of(text: &str) -> String {
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

pub(super) fn open(state_dir: &Path) -> Result<Connection> {
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

pub(super) fn open_status(state_dir: &Path) -> Result<Option<Connection>> {
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
pub(super) fn open_repair(state_dir: &Path) -> Result<Connection> {
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

/// Wall-clock seconds for the `last_refresh_at` meta value.
pub(super) fn epoch_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
