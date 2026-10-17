//! Owner-published collection metadata.
//!
//! The sidecar lives under the vault's `.knowledge/` directory, which the
//! path grammar never addresses, and is committed in the tracker Git that
//! already versions wiki pages. Publisher, reviewer and review-project
//! labels are not consulted by [`super::allowed`]. An approval names one
//! Git commit and the SHA-256 of that commit's source bytes. A later edit
//! keeps neither, including when its FNV token collides.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::revision::{self, Pinned};
use super::{allowed, normalize, vault_dir, Caller, Op, ABSENT};
use crate::error::{Error, Result};
use crate::issue::Pm;

const DOC_CAP: usize = 256;
const LABEL_CAP: usize = 200;
const SIDECAR_CAP: usize = 256 * 1024;

struct Doc {
    path: String,
    source_type: Option<String>,
    version: Option<String>,
    applicability: Option<String>,
    supersedes: Option<String>,
    approved_git: Option<String>,
    approved_sha256: Option<String>,
    approved_by: Option<String>,
}

struct Meta {
    publisher: Option<String>,
    reviewer: Option<String>,
    review_project: Option<String>,
    documents: Vec<Doc>,
}

pub(crate) fn get(pm: &Pm, state_dir: &Path, caller: &Caller, path: &str) -> Result<Value> {
    let root = collection_root(path)?;
    require_read(caller, &root)?;
    let (meta, rev) = load(pm, &root)?;
    state_value(pm, state_dir, caller, &root, &meta, rev)
}

pub(crate) fn put(
    pm: &Pm,
    state_dir: &Path,
    caller: &Caller,
    path: &str,
    if_rev: &str,
    metadata: &Value,
    provenance: &str,
) -> Result<Value> {
    let root = collection_root(path)?;
    if !matches!(caller, Caller::Operator) {
        return Err(Error::rejected(
            "wiki collection refused: publishing collection metadata is the owner's, not a shared edit",
        ));
    }
    if provenance.is_empty() {
        return Err(Error::rejected(
            "wiki collection refused: server provenance is missing",
        ));
    }
    require_read(caller, &root)?;
    let meta = parse_client(pm, caller, &root, metadata, provenance)?;
    let vault = vault_dir(pm)?;
    let abs = sidecar_path(&vault, &root)?;
    let _lock = pm.lock()?;
    if abs
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(Error::rejected(
            "wiki collection refused: metadata sidecar is a symlink",
        ));
    }
    let current = match fs::read(&abs) {
        Ok(bytes) => super::rev_bytes(&bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ABSENT.to_string(),
        Err(e) => return Err(e.into()),
    };
    if if_rev != current {
        return Ok(json!({
            "conflict": "if_rev",
            "current_rev": if current == ABSENT { Value::Null } else { json!(current) },
            "path": root,
        }));
    }
    // Validate the state this publication would expose before it commits:
    // a reported refusal must not leave a committed publication behind.
    collection_paths(pm, &root, &meta)?;
    let bytes = encode(&meta)?;
    if let Some(parent) = abs.parent() {
        fs::create_dir_all(parent)?;
    }
    let before = fs::read(&abs).ok();
    let tmp = abs.with_extension("json.tmp");
    fs::write(&tmp, &bytes)?;
    fs::rename(&tmp, &abs)?;
    let committed = crate::issue::write::commit_who(
        pm,
        std::slice::from_ref(&abs),
        &format!("wiki: publish collection {root}"),
        &[],
        &caller.actor(),
        None,
    );
    if let Err(e) = committed {
        match before {
            Some(prior) => {
                let _ = fs::write(&abs, prior);
            }
            None => {
                let _ = fs::remove_file(&abs);
            }
        }
        return Err(e);
    }
    let rev = super::rev_bytes(&bytes);
    state_value(pm, state_dir, caller, &root, &meta, Some(rev))
}

pub(crate) fn root_and_project(pm: &Pm, caller: &Caller, path: &str) -> Result<Loaded> {
    let root = collection_root(path)?;
    require_read(caller, &root)?;
    let (meta, _) = load(pm, &root)?;
    let sidecar_sha256 = meta_sha(pm, &root)?;
    Ok(Loaded {
        root,
        review_project: meta.review_project,
        sidecar_sha256,
    })
}

pub(crate) struct Loaded {
    pub root: String,
    pub review_project: Option<String>,
    pub sidecar_sha256: Option<String>,
}

fn meta_sha(pm: &Pm, root: &str) -> Result<Option<String>> {
    let abs = sidecar_path(&vault_dir(pm)?, root)?;
    match fs::read(&abs) {
        Ok(bytes) => Ok(Some(super::transfer::sha256_bytes(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn collection_root(path: &str) -> Result<String> {
    let norm = super::normalize_dir(path)?;
    let segs: Vec<&str> = if norm.is_empty() {
        Vec::new()
    } else {
        norm.split('/').collect()
    };
    let ok = match segs.as_slice() {
        ["global", name] => !name.is_empty(),
        ["projects", project, name] => !project.is_empty() && !name.is_empty(),
        _ => false,
    };
    if ok {
        Ok(norm)
    } else {
        Err(Error::rejected(
            "wiki collection refused: only global/<collection> or projects/<project>/<collection> \
             have collection metadata; private areas are not shared collections",
        ))
    }
}

pub(crate) fn under_root(root: &str, path: &str) -> bool {
    path == root || path.starts_with(&format!("{root}/"))
}

fn require_read(caller: &Caller, root: &str) -> Result<()> {
    let segs: Vec<&str> = root.split('/').collect();
    allowed(caller, Op::Read, &segs)
}

fn parse_client(
    pm: &Pm,
    caller: &Caller,
    root: &str,
    metadata: &Value,
    provenance: &str,
) -> Result<Meta> {
    let obj = metadata
        .as_object()
        .ok_or_else(|| Error::rejected("wiki collection refused: metadata must be an object"))?;
    for key in obj.keys() {
        if !matches!(
            key.as_str(),
            "publisher" | "reviewer" | "review_project" | "documents"
        ) {
            return Err(Error::rejected(format!(
                "wiki collection refused: metadata field '{key}' is not accepted"
            )));
        }
    }
    let review_project = optional_label(obj.get("review_project"), "review_project")?;
    if let Some(project) = &review_project {
        let projects = crate::issue::project::list(&pm.dir)?;
        if !projects.iter().any(|item| item.key == *project) {
            return Err(Error::rejected(format!(
                "wiki collection refused: review project '{project}' does not exist"
            )));
        }
    }
    let rows = obj
        .get("documents")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::rejected("wiki collection refused: documents must be an array"))?;
    if rows.len() > DOC_CAP {
        return Err(Error::rejected(
            "wiki collection refused: a collection lists at most 256 documents",
        ));
    }
    let mut documents = Vec::new();
    for row in rows {
        documents.push(parse_doc(pm, caller, root, row, provenance)?);
    }
    let mut seen = Vec::new();
    for doc in &documents {
        if seen.iter().any(|path| path == &doc.path) {
            return Err(Error::rejected(format!(
                "wiki collection refused: '{}' is listed twice",
                doc.path
            )));
        }
        seen.push(doc.path.clone());
    }
    Ok(Meta {
        publisher: optional_label(obj.get("publisher"), "publisher")?,
        reviewer: optional_label(obj.get("reviewer"), "reviewer")?,
        review_project,
        documents,
    })
}

fn parse_doc(pm: &Pm, caller: &Caller, root: &str, row: &Value, provenance: &str) -> Result<Doc> {
    let obj = row
        .as_object()
        .ok_or_else(|| Error::rejected("wiki collection refused: a document must be an object"))?;
    for key in obj.keys() {
        if !matches!(
            key.as_str(),
            "path"
                | "source_type"
                | "version"
                | "applicability"
                | "supersedes"
                | "approved_revision"
                | "approved_sha256"
        ) {
            return Err(Error::rejected(format!(
                "wiki collection refused: document field '{key}' is not accepted"
            )));
        }
    }
    let path = obj
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected("wiki collection refused: a document needs a path"))?;
    let path = normalize(path)?;
    if !under_root(root, &path) || path == root {
        return Err(Error::rejected(format!(
            "wiki collection refused: '{path}' is outside this collection"
        )));
    }
    let segs: Vec<&str> = path.split('/').collect();
    allowed(caller, Op::Read, &segs)?;
    let supersedes = match optional_label(obj.get("supersedes"), "supersedes")? {
        Some(other) => {
            let other = normalize(&other)?;
            if !under_root(root, &other) {
                return Err(Error::rejected(
                    "wiki collection refused: supersedes must stay inside the collection",
                ));
            }
            Some(other)
        }
        None => None,
    };
    let approved = optional_label(obj.get("approved_revision"), "approved_revision")?;
    let claimed = optional_label(obj.get("approved_sha256"), "approved_sha256")?;
    let (approved_git, approved_sha256, approved_by) = if let Some(selector) = approved {
        let pinned: Pinned = revision::pin(pm, caller, &path, &selector)?;
        if pinned.sha256.is_empty() {
            return Err(Error::rejected(
                "wiki collection refused: a directory cannot be an approved source",
            ));
        }
        if claimed
            .as_ref()
            .is_some_and(|claimed| claimed != &pinned.sha256)
        {
            return Err(Error::rejected(
                "wiki collection refused: approved_sha256 does not match that revision's source bytes",
            ));
        }
        (
            Some(pinned.git),
            Some(pinned.sha256),
            Some(provenance.to_string()),
        )
    } else {
        if claimed.is_some() {
            return Err(Error::rejected(
                "wiki collection refused: a digest is not an approval without a resolved revision",
            ));
        }
        (None, None, None)
    };
    Ok(Doc {
        path,
        source_type: optional_label(obj.get("source_type"), "source_type")?,
        version: optional_label(obj.get("version"), "version")?,
        applicability: optional_label(obj.get("applicability"), "applicability")?,
        supersedes,
        approved_git,
        approved_sha256,
        approved_by,
    })
}

fn optional_label(value: Option<&Value>, field: &str) -> Result<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let text = text.trim();
            if text.is_empty() {
                return Ok(None);
            }
            if text.len() > LABEL_CAP || text.chars().any(char::is_control) {
                return Err(Error::rejected(format!(
                    "wiki collection refused: {field} must be at most {LABEL_CAP} non-control characters"
                )));
            }
            Ok(Some(text.to_string()))
        }
        Some(_) => Err(Error::rejected(format!(
            "wiki collection refused: {field} must be a string or null"
        ))),
    }
}

fn load(pm: &Pm, root: &str) -> Result<(Meta, Option<String>)> {
    let abs = sidecar_path(&vault_dir(pm)?, root)?;
    let bytes = match fs::read(&abs) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((
                Meta {
                    publisher: None,
                    reviewer: None,
                    review_project: None,
                    documents: Vec::new(),
                },
                None,
            ))
        }
        Err(e) => return Err(e.into()),
    };
    if bytes.len() > SIDECAR_CAP {
        return Err(Error::rejected(
            "wiki collection refused: metadata sidecar is over the 256KiB bound",
        ));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::rejected("wiki collection refused: metadata sidecar is not JSON"))?;
    let rev = super::rev_bytes(&bytes);
    Ok((parse_stored(&value)?, Some(rev)))
}

fn parse_stored(value: &Value) -> Result<Meta> {
    let obj = value
        .as_object()
        .ok_or_else(|| Error::rejected("wiki collection refused: stored metadata is malformed"))?;
    let documents = obj
        .get("documents")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Error::rejected("wiki collection refused: stored documents are malformed")
        })?;
    let mut docs = Vec::new();
    for row in documents {
        let path = row["path"].as_str().unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }
        docs.push(Doc {
            path,
            source_type: row["source_type"].as_str().map(str::to_string),
            version: row["version"].as_str().map(str::to_string),
            applicability: row["applicability"].as_str().map(str::to_string),
            supersedes: row["supersedes"].as_str().map(str::to_string),
            approved_git: row["approved_revision"].as_str().map(str::to_string),
            approved_sha256: row["approved_sha256"].as_str().map(str::to_string),
            approved_by: row["approved_by"].as_str().map(str::to_string),
        });
    }
    Ok(Meta {
        publisher: obj["publisher"].as_str().map(str::to_string),
        reviewer: obj["reviewer"].as_str().map(str::to_string),
        review_project: obj["review_project"].as_str().map(str::to_string),
        documents: docs,
    })
}

fn encode(meta: &Meta) -> Result<Vec<u8>> {
    let documents: Vec<Value> = meta
        .documents
        .iter()
        .map(|doc| {
            json!({
                "path": doc.path,
                "source_type": doc.source_type,
                "version": doc.version,
                "applicability": doc.applicability,
                "supersedes": doc.supersedes,
                "approved_revision": doc.approved_git,
                "approved_sha256": doc.approved_sha256,
                "approved_by": doc.approved_by,
            })
        })
        .collect();
    let value = json!({
        "publisher": meta.publisher,
        "reviewer": meta.reviewer,
        "review_project": meta.review_project,
        "documents": documents,
    });
    let mut bytes = serde_json::to_vec_pretty(&value)
        .map_err(|e| Error::internal(format!("wiki collection metadata: {e}")))?;
    bytes.push(b'\n');
    if bytes.len() > SIDECAR_CAP {
        return Err(Error::rejected(
            "wiki collection refused: metadata is over the 256KiB bound",
        ));
    }
    Ok(bytes)
}

fn state_value(
    pm: &Pm,
    state_dir: &Path,
    caller: &Caller,
    root: &str,
    meta: &Meta,
    rev: Option<String>,
) -> Result<Value> {
    let paths = collection_paths(pm, root, meta)?;
    let coverage = index_coverage(pm, state_dir);
    let mut documents = Vec::new();
    for path in &paths {
        documents.push(document_value(pm, caller, meta, path, coverage.as_ref())?);
    }
    let owner = matches!(caller, Caller::Operator);
    Ok(json!({
        "path": root,
        "rev": rev,
        "metadata": {
            "publisher": meta.publisher,
            "reviewer": meta.reviewer,
            "review_project": meta.review_project,
            "documents": meta.documents.iter().map(doc_meta).collect::<Vec<_>>(),
        },
        "documents": documents,
        "permissions": {"publish": owner, "review": owner},
    }))
}

/// The documents a collection state projects: the live files plus the
/// published metadata rows, sorted, deduped and bounded by [`DOC_CAP`].
/// Both the metadata publication and the read build their state through
/// this, so an oversized collection is refused before anything commits.
fn collection_paths(pm: &Pm, root: &str, meta: &Meta) -> Result<Vec<String>> {
    let mut paths = live_paths(pm, root)?;
    for doc in &meta.documents {
        if !paths.iter().any(|path| path == &doc.path) {
            paths.push(doc.path.clone());
        }
    }
    paths.sort();
    paths.dedup();
    if paths.len() > DOC_CAP {
        return Err(Error::rejected(
            "wiki collection refused: this collection has more than 256 documents",
        ));
    }
    Ok(paths)
}

fn doc_meta(doc: &Doc) -> Value {
    json!({
        "path": doc.path,
        "source_type": doc.source_type,
        "version": doc.version,
        "applicability": doc.applicability,
        "supersedes": doc.supersedes,
        "approved_revision": doc.approved_git,
        "approved_sha256": doc.approved_sha256,
    })
}

fn document_value(
    pm: &Pm,
    caller: &Caller,
    meta: &Meta,
    path: &str,
    coverage: Option<&rusqlite::Connection>,
) -> Result<Value> {
    let stored = meta.documents.iter().find(|doc| doc.path == path);
    let page = super::read(pm, caller, path).ok();
    let kind = page
        .as_ref()
        .and_then(|page| page["kind"].as_str())
        .unwrap_or("text");
    let rev = page.as_ref().and_then(|page| page["rev"].as_str());
    let sha256 = page.as_ref().and_then(live_sha);
    let processing = match &page {
        Some(page) => processing(pm, caller, path, page, coverage),
        None => "unsupported".to_string(),
    };
    let approval_current =
        stored.is_some_and(|doc| approval_current(pm, caller, doc, sha256.as_deref()));
    let metadata = match stored {
        Some(doc) => doc_meta(doc),
        None => json!({
            "path": path,
            "source_type": Value::Null,
            "version": Value::Null,
            "applicability": Value::Null,
            "supersedes": Value::Null,
            "approved_revision": Value::Null,
            "approved_sha256": Value::Null,
        }),
    };
    Ok(json!({
        "path": path,
        "kind": if kind == "dir" { "dir" } else { kind },
        "rev": rev,
        "sha256": sha256,
        "processing": processing,
        "metadata": metadata,
        "approval_current": approval_current,
    }))
}

fn live_sha(page: &Value) -> Option<String> {
    match page["kind"].as_str() {
        Some("text" | "blob") => page["sha256"].as_str().map(str::to_string),
        _ => None,
    }
}

fn approval_current(pm: &Pm, caller: &Caller, doc: &Doc, live_sha: Option<&str>) -> bool {
    let (Some(git), Some(want), Some(live)) = (
        doc.approved_git.as_deref(),
        doc.approved_sha256.as_deref(),
        live_sha,
    ) else {
        return false;
    };
    if want != live || doc.approved_by.is_none() {
        return false;
    }
    revision::pin(pm, caller, &doc.path, git)
        .is_ok_and(|pinned| pinned.git == git && pinned.sha256 == want)
}

fn processing(
    pm: &Pm,
    caller: &Caller,
    path: &str,
    page: &Value,
    coverage: Option<&rusqlite::Connection>,
) -> String {
    match page["kind"].as_str() {
        Some("text") => {
            let text = page["text"].as_str().unwrap_or("");
            if text.trim().is_empty() {
                return "empty".to_string();
            }
            if let Some(header) = super::ingest::extraction_header(text) {
                return match header.status {
                    "empty" => "empty".to_string(),
                    "failed" => "failed".to_string(),
                    "indexed" if indexed(coverage, path, page["rev"].as_str().unwrap_or("")) => {
                        "covered".to_string()
                    }
                    "indexed" => "pending".to_string(),
                    _ => "unsupported".to_string(),
                };
            }
            if indexed(coverage, path, page["rev"].as_str().unwrap_or("")) {
                "covered".to_string()
            } else {
                "pending".to_string()
            }
        }
        Some("blob") => {
            let mime = page["mime"].as_str().unwrap_or("");
            let name = page["name"].as_str().unwrap_or("");
            if mime != "application/pdf" && !name.to_ascii_lowercase().ends_with(".pdf") {
                return "unsupported".to_string();
            }
            let sidecar = format!("{path}.extracted.md");
            let Some(extracted) = super::read(pm, caller, &sidecar).ok() else {
                return "pending".to_string();
            };
            let text = extracted["text"].as_str().unwrap_or("");
            let Some(header) = super::ingest::extraction_header(text) else {
                return "pending".to_string();
            };
            if header.sha256 != page["sha256"].as_str().unwrap_or("") {
                return "pending".to_string();
            }
            match header.status {
                "empty" => "empty".to_string(),
                "failed" => "failed".to_string(),
                "indexed"
                    if indexed(coverage, &sidecar, extracted["rev"].as_str().unwrap_or("")) =>
                {
                    "covered".to_string()
                }
                "indexed" => "pending".to_string(),
                _ => "unsupported".to_string(),
            }
        }
        _ => "unsupported".to_string(),
    }
}

fn index_coverage(pm: &Pm, state_dir: &Path) -> Option<rusqlite::Connection> {
    let vault = vault_dir(pm).ok()?;
    let rel = vault.strip_prefix(&pm.dir).ok()?;
    let tree = format!("HEAD:{}", rel.to_string_lossy().replace('\\', "/"));
    let oid = crate::issue::git(&pm.dir, &["rev-parse", "--verify", &tree]).ok()?;
    let conn = rusqlite::Connection::open_with_flags(
        super::index::db_path(state_dir),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    let stored: String = conn
        .query_row(
            "SELECT value FROM wiki_index_meta WHERE key='revision'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_default();
    (stored == oid).then_some(conn)
}

fn indexed(conn: Option<&rusqlite::Connection>, path: &str, rev: &str) -> bool {
    let Some(conn) = conn else {
        return false;
    };
    if rev.is_empty() {
        return false;
    }
    conn.query_row(
        "SELECT rev FROM wiki_index_chunks WHERE path=?1 LIMIT 1",
        [path],
        |row| row.get::<_, String>(0),
    )
    .is_ok_and(|indexed| indexed == rev)
}

fn live_paths(pm: &Pm, root: &str) -> Result<Vec<String>> {
    let vault = vault_dir(pm)?;
    let dir = super::resolve(&vault, root)?;
    let mut out = Vec::new();
    walk_files(&dir, root, &mut out, 0)?;
    Ok(out)
}

fn walk_files(dir: &Path, prefix: &str, out: &mut Vec<String>, depth: usize) -> Result<()> {
    if depth > 32 || out.len() > DOC_CAP {
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let meta = match entry.path().symlink_metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            let next = format!("{prefix}/{name}");
            walk_files(&entry.path(), &next, out, depth + 1)?;
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        if let Some(stem) = name.strip_suffix(".blob") {
            out.push(format!("{prefix}/{stem}"));
        } else if !name.ends_with(".blob") {
            out.push(format!("{prefix}/{name}"));
        }
        if out.len() > DOC_CAP {
            break;
        }
    }
    Ok(())
}

fn sidecar_path(vault: &Path, root: &str) -> Result<PathBuf> {
    if vault
        .symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return Err(Error::rejected(
            "wiki collection refused: the vault is a symlink",
        ));
    }
    let mut parts: Vec<&str> = root.split('/').collect();
    let Some(last) = parts.pop() else {
        return Err(Error::rejected(
            "wiki collection refused: metadata path is not addressable",
        ));
    };
    if last.is_empty() || last.starts_with('.') || last.contains('\0') {
        return Err(Error::rejected(
            "wiki collection refused: metadata path is not addressable",
        ));
    }
    let mut abs = vault.join(".knowledge");
    for seg in parts {
        if seg.is_empty() || seg.starts_with('.') || seg.contains('\0') {
            return Err(Error::rejected(
                "wiki collection refused: metadata path is not addressable",
            ));
        }
        abs = abs.join(seg);
    }
    Ok(abs.join(format!("{last}.json")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refusal must not leave a publication behind: an oversize collection
    /// is validated before the sidecar is written or committed.
    #[test]
    fn an_oversize_collection_is_refused_before_the_publish() {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let vault = vault_dir(&pm).unwrap();
        let big = vault.join("global/big");
        fs::create_dir_all(&big).unwrap();
        for index in 0..=DOC_CAP {
            fs::write(big.join(format!("f{index}.md")), b"x").unwrap();
        }
        let state = root.path().join("state");
        fs::create_dir_all(&state).unwrap();
        let head = crate::issue::git(&pm.dir, &["rev-parse", "HEAD"]).ok();
        let metadata = json!({
            "publisher": null,
            "reviewer": null,
            "review_project": null,
            "documents": [],
        });
        let refused = put(
            &pm,
            &state,
            &Caller::Operator,
            "global/big",
            ABSENT,
            &metadata,
            "usr-publisher",
        );
        assert!(refused.is_err(), "an oversize collection was published");
        assert!(
            !vault.join(".knowledge").exists(),
            "a refused publish wrote a sidecar"
        );
        assert_eq!(
            crate::issue::git(&pm.dir, &["rev-parse", "HEAD"]).ok(),
            head,
            "a refused publish committed"
        );
    }
}
