//! Wiki Git tree scanning, SQLite FTS chunk rebuild, and staleness detection.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use rusqlite::{params, Connection, TransactionBehavior};
use serde_json::Value;

use super::super::{normalize, TEXT_CAP};
use super::{
    chunk_tables, create_chunks, db_error, epoch_now, meta_get, meta_put, table_for, CHUNK_BYTES,
    INDEX_GLOBAL, SCHEMA,
};
use crate::error::{Error, Result};
use crate::issue::Pm;

/// What the index freshness is keyed on. The committed Git tree of the vault
/// changes only when the wiki changes — unlike the tracker HEAD, which advances
/// for every issue and report. A vault outside the tracker has no Git tree to
/// observe (`Untracked`): the index then rebuilds on each query until that
/// layout has a committed source of truth, and never reports itself current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Source {
    /// `git rev-parse HEAD:<vault-rel>` — the committed tree oid, or
    /// `"absent"` when the vault is not yet committed.
    Tree(String),
    /// The vault is not inside the tracker's Git — no revision exists.
    Untracked,
}

impl Source {
    /// The meta `revision` value: the oid, `"absent"`, or `"untracked"`.
    pub(super) fn value(&self) -> &str {
        match self {
            Source::Tree(t) => t.as_str(),
            Source::Untracked => "untracked",
        }
    }
}

pub(super) fn source_revision(pm: &Pm, vault: &Path) -> Result<Source> {
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
pub(super) struct Scan {
    /// Source files whose normalized name is indexable (`.extracted.md`
    /// included, `.blob` and dotfiles excluded).
    pub(super) pages: usize,
    /// Chunks committed.
    pub(super) chunks: usize,
    /// Pages skipped for a refused read or a provenance/mixed-source rule.
    skipped: usize,
}

/// One row of a chunk table, before it is written.
struct Chunk {
    path: String,
    title: String,
    heading: String,
    body: String,
    line: i64,
    rev: String,
    source: String,
}

pub(super) fn rebuild(
    conn: &mut Connection,
    pm: &Pm,
    vault: &Path,
    source: &Source,
) -> Result<Scan> {
    let started = Instant::now();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(db_error)?;
    // Rows are grouped per project so each table holds exactly one project's
    // corpus. The tables are rebuilt, not patched: a project that lost its last
    // page must not leave its statistics behind.
    let mut rows: std::collections::BTreeMap<String, Vec<Chunk>> =
        std::collections::BTreeMap::new();
    let mut scan = Scan::default();
    {
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
            let rev = super::super::rev_bytes(&bytes);
            let Ok(text) = String::from_utf8(bytes) else {
                scan.skipped += 1;
                continue;
            };
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
            scan.pages += 1;
            let title = text
                .lines()
                .find_map(|line| line.strip_prefix("# "))
                .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(&path));
            let page_source = path.strip_suffix(".extracted.md").unwrap_or(&path);
            for (line, heading, body) in chunks(&text) {
                rows.entry(table_for(&path)).or_default().push(Chunk {
                    path: path.clone(),
                    title: title.to_string(),
                    heading,
                    body,
                    line: line as i64,
                    rev: rev.clone(),
                    source: page_source.to_string(),
                });
                scan.chunks += 1;
            }
        }
    }
    // Replace the tables inside the same transaction, so a reader on another
    // connection sees either the old complete snapshot or this one — never
    // half, and never a table that belongs to nobody.
    for table in chunk_tables(&tx)? {
        tx.execute_batch(&format!("DROP TABLE {table};"))
            .map_err(db_error)?;
    }
    for (table, rows) in &rows {
        create_chunks(&tx, table)?;
        let mut insert = tx
            .prepare(&format!(
                "INSERT INTO {table}(path,title,heading,body,line,rev,source) VALUES (?1,?2,?3,?4,?5,?6,?7)"
            ))
            .map_err(db_error)?;
        for chunk in rows {
            insert
                .execute(params![
                    chunk.path,
                    chunk.title,
                    chunk.heading,
                    chunk.body,
                    chunk.line,
                    chunk.rev,
                    chunk.source
                ])
                .map_err(db_error)?;
        }
    }
    if !rows.contains_key(INDEX_GLOBAL) {
        create_chunks(&tx, INDEX_GLOBAL)?;
    }
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
pub(super) fn stored_meta(conn: &Connection) -> Option<(String, String)> {
    let (schema, revision) = (meta_get(conn, "schema")?, meta_get(conn, "revision")?);
    Some((schema, revision))
}

/// Why the committed snapshot does not match the stored one — the
/// freshness reason the health surface reports and `ensure_current`
/// acts on. `None` = the index already mirrors the committed tree.
pub(super) fn stale_reason(source: &Source, stored: Option<&(String, String)>) -> Option<String> {
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
