//! Rebuildable full-text index of the canonical wiki text tree.
//!
//! The tracker Git tree is the source of truth. The SQLite database is a
//! disposable search cache: a changed wiki tree rebuilds it in one transaction,
//! including moves and removals. No caller identity is stored in the index;
//! every hit is checked against the wiki's existing path allowlist at query time.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, TransactionBehavior};
use serde_json::{json, Value};

use super::{allowed, normalize, rev_of, vault_dir, Caller, Op, SEARCH_CAP, TEXT_CAP};
use crate::error::{Error, Result};
use crate::issue::Pm;

const SCHEMA: &str = "wiki-fts-v1";
const CHUNK_BYTES: usize = 4096;

fn db_error(error: rusqlite::Error) -> Error {
    Error::internal(format!("wiki index: {error}"))
}

fn open(state_dir: &Path) -> Result<Connection> {
    std::fs::create_dir_all(state_dir)?;
    let db = state_dir.join("wiki-search.sqlite3");
    let conn = Connection::open(db).map_err(db_error)?;
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
    Ok(conn)
}

/// A Git tree oid changes only when the wiki changes, unlike the tracker HEAD,
/// which advances for every issue and report. A vault outside the tracker has
/// no Git tree to observe, so it is rebuilt on each query until that layout has
/// a committed source of truth.
fn source_revision(pm: &Pm, vault: &Path) -> Option<String> {
    let rel = vault.strip_prefix(&pm.dir).ok()?;
    let rel = rel.to_str()?.replace('\\', "/");
    let tree = format!("HEAD:{rel}");
    Some(crate::issue::git(&pm.dir, &["rev-parse", &tree]).unwrap_or_else(|_| "absent".into()))
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
    let Ok(meta) = serde_json::from_str::<Value>(&raw) else {
        return false;
    };
    let Some(sha) = meta["sha256"].as_str() else {
        return false;
    };
    text.lines()
        .any(|line| line == format!("source_sha256: {sha}"))
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

fn rebuild(conn: &mut Connection, vault: &Path, revision: Option<&str>) -> Result<()> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(db_error)?;
    tx.execute("DELETE FROM wiki_index_chunks", [])
        .map_err(db_error)?;
    let mut insert = tx
        .prepare("INSERT INTO wiki_index_chunks(path,title,heading,body,line,rev,source) VALUES (?1,?2,?3,?4,?5,?6,?7)")
        .map_err(db_error)?;
    for (path, file) in pages(vault)? {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        if !current_extraction(vault, &path, &text) {
            continue;
        }
        let title = text
            .lines()
            .find_map(|line| line.strip_prefix("# "))
            .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(&path));
        let rev = rev_of(&file)?;
        let source = path.strip_suffix(".extracted.md").unwrap_or(&path);
        for (line, heading, body) in chunks(&text) {
            insert
                .execute(params![
                    path,
                    title,
                    heading,
                    body,
                    line as i64,
                    rev,
                    source
                ])
                .map_err(db_error)?;
        }
    }
    drop(insert);
    tx.execute(
        "INSERT INTO wiki_index_meta(key,value) VALUES ('revision',?1)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [revision.unwrap_or("untracked")],
    )
    .map_err(db_error)?;
    tx.execute(
        "INSERT INTO wiki_index_meta(key,value) VALUES ('schema',?1)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [SCHEMA],
    )
    .map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    Ok(())
}

fn ensure_current(pm: &Pm, state_dir: &Path) -> Result<Connection> {
    let vault = vault_dir(pm)?;
    let mut conn = open(state_dir)?;
    let revision = source_revision(pm, &vault);
    let stored: Option<(String, String)> = conn
        .query_row(
            "SELECT (SELECT value FROM wiki_index_meta WHERE key='schema'),
                    (SELECT value FROM wiki_index_meta WHERE key='revision')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();
    if revision.is_none()
        || stored.as_ref().is_none_or(|(schema, old)| {
            schema != SCHEMA || Some(old.as_str()) != revision.as_deref()
        })
    {
        rebuild(&mut conn, &vault, revision.as_deref())?;
    }
    Ok(conn)
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

pub fn search(pm: &Pm, state_dir: &Path, caller: &Caller, q: &str, base: &str) -> Result<Value> {
    let norm = normalize(base)?;
    let segs = if norm.is_empty() {
        Vec::new()
    } else {
        norm.split('/').collect::<Vec<_>>()
    };
    allowed(caller, Op::Read, &segs)?;
    let expression = query_terms(q)?;
    let conn = ensure_current(pm, state_dir)?;
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
    Ok(json!({"q":q,"base":norm,"matches":matches}))
}
