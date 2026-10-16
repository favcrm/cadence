//! CAD-914 resumable wiki copy.
//!
//! One job is a manifest plus staged bytes under `<state>/wiki-transfers/`.
//! Publication uses the existing wiki write, mkdir and put_blob calls, one
//! entry at a time. The folder is not an atomic transaction.
//!
//! Crash boundaries:
//! - staged bytes are fsynced before the Store `received` row moves forward;
//!   a later call trusts the regular file when the row lags;
//! - a wiki commit that lands without its outcome row is recognized from
//!   the committed tree, not the worktree, and is not published again.
//!   Desired bytes that are absent or different in HEAD stay `unresolved`
//!   and keep their staged input; apply does not commit them itself;
//! - index coverage is computed from the live page revisions and the
//!   committed search snapshot. It is `pending` until those revisions are
//!   in the current tree's index, `superseded` when a later write replaced
//!   them, and never `covered` for a failed or conflicting extraction.
//! - a generated extraction page is recognized only by its header. Markers in
//!   the extracted body are not a source hash or extraction status.
//!
//! [`Manifest`] and [`ManifestEntry`] are the CLI wire types. [`manifest_digest`]
//! hashes the canonical JSON of a path-sorted manifest.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    allowed, blobs_dir, ingest, ls, mkdir, put_blob, read, vault_dir, write, Caller, Op, ABSENT,
    UPLOAD_DIR,
};
use crate::error::{Error, Result};
use crate::issue::Pm;
use crate::store::wiki_transfers::{self, EntryOutcome, EntryRow, JobHead};
use crate::store::Store;

/// Manifest schema this daemon accepts.
pub const MANIFEST_VERSION: u32 = 1;
/// Largest manifest, serialized.
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
/// Largest entry list.
pub const MAX_ENTRIES: usize = 256;
/// Largest one file, before the configured wiki caps.
pub const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;
/// Largest sum of file sizes in one manifest.
pub const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
/// Decoded chunk and export slice cap. Keeps a JSON reply under the native line bound.
pub const MAX_CHUNK_BYTES: usize = 16_384;
/// Entry id and job-relative path limits.
pub const MAX_ID_LEN: usize = 80;
pub const MAX_PATH_LEN: usize = 512;
pub const MAX_SOURCE_ID_LEN: usize = 128;
/// Status page size. Default and maximum are the same.
pub const STATUS_PAGE_MAX: u64 = 32;
/// Private staging directory under the daemon state dir.
pub const STAGING_DIR: &str = "wiki-transfers";

/// One file or directory in a transfer manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    pub id: String,
    pub path: String,
    pub kind: String,
    pub size: u64,
    pub sha256: Option<String>,
    pub if_rev: String,
}

/// Versioned manifest. `entries` are relative to `root` and sorted by `path`
/// before the digest is taken.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub source_id: String,
    pub root: String,
    pub entries: Vec<ManifestEntry>,
}

/// Caller, owner, and target for an operation on a job that already exists.
/// `begin` and `export` do not use this shape.
pub struct JobRequest<'a> {
    pub caller: &'a Caller,
    pub owner: &'a str,
    pub job_id: &'a str,
    pub root: &'a str,
}

/// Lowercase hex SHA-256 of the canonical manifest JSON.
pub fn manifest_digest(manifest: &Manifest) -> Result<String> {
    let bytes = canonical_manifest_json(manifest)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes.as_bytes());
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Canonical JSON: struct field order, entries sorted by relative path.
/// This is the bytes [`manifest_digest`] hashes.
pub fn canonical_manifest_json(manifest: &Manifest) -> Result<String> {
    let mut owned = manifest.clone();
    owned
        .entries
        .sort_by(|a, b| a.path.cmp(&b.path).then(a.id.cmp(&b.id)));
    Ok(serde_json::to_string(&owned)?)
}

fn reject(why: impl Into<String>) -> Error {
    Error::rejected(why.into())
}

fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn sha_ok(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn rev_ok(value: &str) -> bool {
    value == ABSENT
        || value
            .strip_prefix("fnv1a:")
            .is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn kind_for(relative: &str, kind: &str) -> Result<&'static str> {
    match kind {
        "dir" => Ok("dir"),
        "text" | "blob" => {
            let name = relative.rsplit('/').next().unwrap_or(relative);
            let lower = name.to_ascii_lowercase();
            let text =
                lower.ends_with(".md") || lower.ends_with(".markdown") || lower.ends_with(".txt");
            let expect = if text { "text" } else { "blob" };
            if kind != expect {
                return Err(reject(format!(
                    "wiki transfer refused: '{relative}' is {expect}, not {kind}"
                )));
            }
            Ok(expect)
        }
        other => Err(reject(format!(
            "wiki transfer refused: unsupported entry kind '{other}'"
        ))),
    }
}

fn full_path(root: &str, relative: &str) -> String {
    format!("{root}/{relative}")
}

fn text_cap() -> u64 {
    64 * 1024 * 1024
}

/// Validate the whole manifest before any wiki mutation. On success the
/// entries are sorted by relative path and the canonical JSON is returned
/// with its digest.
pub fn admit_manifest(
    pm: &Pm,
    caller: &Caller,
    manifest: &Manifest,
    raw_len: usize,
) -> Result<(Manifest, String, String)> {
    if raw_len > MAX_MANIFEST_BYTES {
        return Err(reject("wiki transfer refused: manifest exceeds 256KiB"));
    }
    if manifest.version != MANIFEST_VERSION {
        return Err(reject("wiki transfer refused: manifest version must be 1"));
    }
    if manifest.source_id.is_empty()
        || manifest.source_id.len() > MAX_SOURCE_ID_LEN
        || manifest.source_id.chars().any(|c| c.is_control())
    {
        return Err(reject(
            "wiki transfer refused: source_id is empty or too long",
        ));
    }
    if manifest.entries.len() > MAX_ENTRIES {
        return Err(reject(
            "wiki transfer refused: manifest has more than 256 entries",
        ));
    }
    let root = super::normalize(&manifest.root)?;
    if root.is_empty() || root.len() > MAX_PATH_LEN || root != manifest.root {
        return Err(reject(
            "wiki transfer refused: root must be a nonempty canonical directory path",
        ));
    }
    let root_segs: Vec<&str> = root.split('/').collect();
    allowed(caller, Op::Write, &root_segs)?;
    let vault = vault_dir(pm)?;
    refuse_special(&vault, &root, true)?;
    match read(pm, caller, &root) {
        Ok(page) if page["kind"] == "dir" => {}
        Ok(_) => {
            return Err(reject(format!(
                "wiki transfer refused: '{root}' is a file, not a directory"
            )))
        }
        Err(error) if missing(&error) => {}
        Err(error) => return Err(error),
    }
    let upload_cap = pm.config.wiki.max_upload_bytes;
    let mut owned = manifest.clone();
    owned.root = root.clone();
    let mut total = 0u64;
    let mut seen_id = Vec::new();
    let mut seen_path: Vec<(String, bool)> = Vec::new();
    for entry in &mut owned.entries {
        if !id_ok(&entry.id) {
            return Err(reject(format!(
                "wiki transfer refused: entry id '{}' is not a short ASCII id",
                entry.id
            )));
        }
        if seen_id.iter().any(|id: &String| id == &entry.id) {
            return Err(reject(format!(
                "wiki transfer refused: duplicate entry id '{}'",
                entry.id
            )));
        }
        seen_id.push(entry.id.clone());
        let relative = super::normalize(&entry.path)?;
        if relative.is_empty() || relative.len() > MAX_PATH_LEN || relative != entry.path {
            return Err(reject(
                "wiki transfer refused: entry path must be a nonempty canonical relative path",
            ));
        }
        entry.path = relative.clone();
        let kind = kind_for(&relative, &entry.kind)?;
        entry.kind = kind.to_string();
        let full = full_path(&root, &relative);
        super::normalize(&full)?;
        if seen_path.iter().any(|(path, _)| path == &full) {
            return Err(reject(format!(
                "wiki transfer refused: duplicate path '{full}'"
            )));
        }
        for (prior, prior_is_dir) in &seen_path {
            if (!prior_is_dir
                && full
                    .strip_prefix(prior)
                    .is_some_and(|rest| rest.starts_with('/')))
                || (kind != "dir"
                    && prior
                        .strip_prefix(&full)
                        .is_some_and(|rest| rest.starts_with('/')))
            {
                return Err(reject(format!(
                    "wiki transfer refused: '{full}' collides with '{prior}'"
                )));
            }
        }
        seen_path.push((full.clone(), kind == "dir"));
        let segs: Vec<&str> = full.split('/').collect();
        allowed(caller, Op::Write, &segs)?;
        refuse_special(&vault, &full, kind == "dir")?;
        if !rev_ok(&entry.if_rev) {
            return Err(reject(format!(
                "wiki transfer refused: if_rev for '{full}' is not 'none' or a content revision"
            )));
        }
        match kind {
            "dir" => {
                if entry.size != 0 || entry.sha256.is_some() {
                    return Err(reject(format!(
                        "wiki transfer refused: directory '{full}' carries a size or hash"
                    )));
                }
                if entry.if_rev != ABSENT {
                    return Err(reject(format!(
                        "wiki transfer refused: directory '{full}' is create-only"
                    )));
                }
            }
            "text" | "blob" => {
                let Some(hash) = entry.sha256.as_deref() else {
                    return Err(reject(format!(
                        "wiki transfer refused: '{full}' needs a lowercase sha256"
                    )));
                };
                if !sha_ok(hash) {
                    return Err(reject(format!(
                        "wiki transfer refused: '{full}' hash is not lowercase sha256"
                    )));
                }
                let cap = if kind == "text" {
                    MAX_FILE_BYTES.min(upload_cap).min(text_cap())
                } else {
                    MAX_FILE_BYTES.min(upload_cap)
                };
                if entry.size > cap {
                    return Err(reject(format!(
                        "wiki transfer refused: '{full}' is {} bytes over the {cap}-byte cap",
                        entry.size
                    )));
                }
                total = total.checked_add(entry.size).ok_or_else(|| {
                    reject("wiki transfer refused: manifest total size overflows")
                })?;
            }
            _ => unreachable!("kind_for"),
        }
    }
    if total > MAX_TOTAL_BYTES {
        return Err(reject(
            "wiki transfer refused: manifest exceeds the 256MiB total",
        ));
    }
    owned
        .entries
        .sort_by(|a, b| a.path.cmp(&b.path).then(a.id.cmp(&b.id)));
    let canonical = serde_json::to_string(&owned)?;
    if canonical.len() > MAX_MANIFEST_BYTES {
        return Err(reject("wiki transfer refused: manifest exceeds 256KiB"));
    }
    let digest = manifest_digest(&owned)?;
    Ok((owned, canonical, digest))
}

/// New jobs pin `if_rev` to the live revision. A resumed job keeps the
/// original manifest, including create-only pins for entries already published.
fn pin_live(pm: &Pm, caller: &Caller, manifest: &Manifest) -> Result<()> {
    for entry in &manifest.entries {
        let full = full_path(&manifest.root, &entry.path);
        let page = match read(pm, caller, &full) {
            Ok(page) => Some(page),
            Err(error) if missing(&error) => None,
            Err(error) => return Err(error),
        };
        if page
            .as_ref()
            .is_some_and(|page| page["kind"].as_str() != Some(entry.kind.as_str()))
        {
            return Err(reject(format!(
                "wiki transfer refused: '{full}' has a different destination kind"
            )));
        }
        if entry.kind == "dir" {
            continue;
        }
        let current = page
            .as_ref()
            .and_then(|page| page["rev"].as_str())
            .unwrap_or(ABSENT);
        let unchanged = entry.if_rev == ABSENT
            && page.as_ref().is_some_and(|page| {
                page_matches(page, &entry.kind, entry.size, entry.sha256.as_deref())
            });
        if entry.if_rev != current && !unchanged {
            return Err(reject(format!(
                "wiki transfer refused: '{full}' destination revision changed"
            )));
        }
    }
    Ok(())
}

fn current_rev(pm: &Pm, caller: &Caller, path: &str) -> Result<String> {
    match read(pm, caller, path) {
        Ok(page) => Ok(page["rev"].as_str().unwrap_or(ABSENT).to_string()),
        Err(error) if missing(&error) => Ok(ABSENT.to_string()),
        Err(error) => Err(error),
    }
}

/// Create the destination directory when the manifest named no root entry.
/// An existing directory is left untouched. A file is never overwritten.
fn ensure_root(pm: &Pm, caller: &Caller, root: &str) -> Result<bool> {
    let segs: Vec<&str> = root.split('/').collect();
    allowed(caller, Op::Write, &segs)?;
    match read(pm, caller, root) {
        Ok(page) if page["kind"] == "dir" => Ok(false),
        Ok(_) => Err(reject(format!(
            "wiki transfer finish refused: '{root}' is a file and was not overwritten"
        ))),
        Err(error) if missing(&error) => {
            mkdir(pm, caller, root)?;
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

fn missing(error: &Error) -> bool {
    let text = error.to_string();
    text.contains("no such") || text.contains("not found")
}

fn refuse_special(vault: &Path, norm: &str, dir_ok: bool) -> Result<()> {
    if vault
        .symlink_metadata()
        .is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return Err(reject("wiki transfer refused: the vault is a symlink"));
    }
    let mut cur = vault.to_path_buf();
    if norm.is_empty() {
        return Ok(());
    }
    let parts: Vec<&str> = norm.split('/').collect();
    for (index, seg) in parts.iter().enumerate() {
        cur.push(seg);
        match cur.symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(reject(format!(
                    "wiki transfer refused: '{norm}' resolves through a symlink"
                )));
            }
            Ok(meta) if !meta.is_file() && !meta.is_dir() => {
                return Err(reject(format!(
                    "wiki transfer refused: '{norm}' is not a regular file or directory"
                )));
            }
            Ok(meta) if meta.is_file() && index + 1 != parts.len() => {
                return Err(reject(format!(
                    "wiki transfer refused: '{norm}' passes through a file"
                )));
            }
            Ok(meta) if meta.is_file() && dir_ok => {
                return Err(reject(format!(
                    "wiki transfer refused: directory '{norm}' is a file"
                )));
            }
            Ok(_) | Err(_) => {}
        }
    }
    Ok(())
}

fn not_found() -> Error {
    reject("wiki transfer job not found")
}

fn require_job(store: &Store, request: &JobRequest<'_>) -> Result<JobHead> {
    let Some(job) = store.wiki_transfer_job(request.job_id)? else {
        return Err(not_found());
    };
    if job.owner_key != request.owner || job.root != request.root {
        return Err(not_found());
    }
    Ok(job)
}

fn recheck_job_paths(
    caller: &Caller,
    root: &str,
    entries: &[EntryRow],
    writing: bool,
) -> Result<()> {
    let root_segments: Vec<&str> = root.split('/').collect();
    allowed(
        caller,
        if writing { Op::Write } else { Op::Read },
        &root_segments,
    )?;
    for entry in entries {
        let segments: Vec<&str> = entry.path.split('/').collect();
        allowed(
            caller,
            if writing { Op::Write } else { Op::Read },
            &segments,
        )?;
    }
    Ok(())
}

fn stage_path(state_dir: &Path, job_id: &str, entry_id: &str) -> PathBuf {
    state_dir.join(STAGING_DIR).join(job_id).join(entry_id)
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = open_nofollow(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn open_nofollow(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                reject(format!(
                    "wiki transfer refused: {} is a symlink",
                    path.display()
                ))
            } else {
                Error::from(error)
            }
        })
}

fn lock_exclusive(file: &File) -> Result<()> {
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if rc != 0 {
        return Err(Error::internal("wiki transfer could not lock staged bytes"));
    }
    Ok(())
}

/// `{op:begin}`.
pub fn begin(
    store: &Store,
    pm: &Pm,
    caller: &Caller,
    owner: &str,
    job_id: Option<&str>,
    manifest: &Manifest,
    raw_len: usize,
) -> Result<Value> {
    let (manifest, canonical, digest) = admit_manifest(pm, caller, manifest, raw_len)?;
    let job_id = match job_id {
        Some(id) => {
            uuid::Uuid::parse_str(id)
                .map_err(|_| reject("wiki transfer refused: job_id must be a UUID"))?;
            id.to_string()
        }
        None => uuid::Uuid::new_v4().to_string(),
    };
    if let Some(existing) = store.wiki_transfer_job(&job_id)? {
        if existing.owner_key == owner
            && existing.root == manifest.root
            && existing.digest == digest
            && existing.manifest_json == canonical
        {
            return Ok(receipt(&existing, manifest.entries.len()));
        }
        return Err(reject(
            "wiki transfer refused: job_id is bound to a different manifest, actor or target",
        ));
    }
    pin_live(pm, caller, &manifest)?;
    let job = JobHead {
        job_id: job_id.clone(),
        digest: digest.clone(),
        manifest_json: canonical,
        source_id: manifest.source_id.clone(),
        root: manifest.root.clone(),
        owner_key: owner.to_string(),
        state: "open".into(),
    };
    let entries = manifest
        .entries
        .iter()
        .enumerate()
        .map(|(ordinal, entry)| EntryRow {
            entry_id: entry.id.clone(),
            ordinal: ordinal as i64,
            path: full_path(&manifest.root, &entry.path),
            rel_path: entry.path.clone(),
            kind: entry.kind.clone(),
            size: entry.size,
            sha256: entry.sha256.clone(),
            if_rev: entry.if_rev.clone(),
            received: 0,
            outcome: "pending".into(),
            rev: None,
            extraction: None,
            extraction_rev: None,
            source_hash: None,
            detail: None,
        })
        .collect::<Vec<_>>();
    store.wiki_transfer_insert(&job, &entries)?;
    Ok(receipt(&job, entries.len()))
}

fn receipt(job: &JobHead, entries: usize) -> Value {
    json!({
        "job_id": job.job_id,
        "digest": job.digest,
        "root": job.root,
        "source_id": job.source_id,
        "state": job.state,
        "entries": entries,
    })
}

/// `{op:chunk}`.
pub fn chunk(
    store: &Store,
    state_dir: &Path,
    request: &JobRequest<'_>,
    entry_id: &str,
    offset: u64,
    data_base64: &str,
) -> Result<Value> {
    let caller = request.caller;
    let job_id = request.job_id;
    let root = request.root;
    let job = require_job(store, request)?;
    if job.state != "open" {
        return Err(reject(
            "wiki transfer chunk refused: the job is already finished",
        ));
    }
    let entry = store
        .wiki_transfer_entry(job_id, entry_id)?
        .ok_or_else(not_found)?;
    recheck_job_paths(caller, root, std::slice::from_ref(&entry), true)?;
    if entry.kind == "dir" {
        return Err(reject(
            "wiki transfer chunk refused: directories have no bytes",
        ));
    }
    if matches!(
        entry.outcome.as_str(),
        "applied" | "skipped" | "conflict" | "failed"
    ) {
        return Err(reject(
            "wiki transfer chunk refused: the entry is already decided",
        ));
    }
    if data_base64.len() > MAX_CHUNK_BYTES.div_ceil(3) * 4 {
        return Err(reject("wiki transfer chunk exceeds its encoded byte bound"));
    }
    let data = STANDARD
        .decode(data_base64)
        .map_err(|_| reject("wiki transfer chunk refused: invalid base64"))?;
    if data.len() > MAX_CHUNK_BYTES {
        return Err(reject("wiki transfer chunk exceeds its decoded byte bound"));
    }
    let end = offset
        .checked_add(data.len() as u64)
        .filter(|end| *end <= entry.size)
        .ok_or_else(|| reject("wiki transfer chunk passes the declared size"))?;
    let path = stage_path(state_dir, job_id, entry_id);
    refuse_special(state_dir, &format!("{STAGING_DIR}/{job_id}"), true)?;
    let parent = path
        .parent()
        .ok_or_else(|| reject("invalid transfer stage path"))?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)?;
    lock_exclusive(&file)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > entry.size {
        return Err(reject("wiki transfer stage is not a bounded regular file"));
    }
    if offset > meta.len() {
        return Err(reject("wiki transfer chunk offset leaves a gap"));
    }
    let overlap = meta.len().saturating_sub(offset).min(data.len() as u64) as usize;
    if overlap != 0 {
        let mut staged = [0u8; MAX_CHUNK_BYTES];
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut staged[..overlap])?;
        if staged[..overlap] != data[..overlap] {
            return Err(reject("wiki transfer retry bytes differ from staged bytes"));
        }
    }
    if overlap < data.len() {
        file.seek(SeekFrom::End(0))?;
        file.write_all(&data[overlap..])?;
    }
    file.sync_all()?;
    File::open(parent)?.sync_all()?;
    let received = meta.len().max(end);
    store.wiki_transfer_note_received(job_id, entry_id, received)?;
    Ok(json!({"job_id": job_id, "entry_id": entry_id, "received": received, "size": entry.size}))
}

fn staged_len(state_dir: &Path, job_id: &str, entry_id: &str) -> Result<Option<(PathBuf, u64)>> {
    let path = stage_path(state_dir, job_id, entry_id);
    match path.symlink_metadata() {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(reject("wiki transfer refused: staged path is a symlink"))
        }
        Ok(meta) if meta.is_file() => Ok(Some((path, meta.len()))),
        Ok(_) => Err(reject(
            "wiki transfer refused: staged path is not a regular file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// `{op:apply}`.
pub fn apply(
    store: &Store,
    pm: &Pm,
    state_dir: &Path,
    request: &JobRequest<'_>,
    entry_id: &str,
) -> Result<Value> {
    let caller = request.caller;
    let job_id = request.job_id;
    let root = request.root;
    let job = require_job(store, request)?;
    let entry = store
        .wiki_transfer_entry(job_id, entry_id)?
        .ok_or_else(not_found)?;
    recheck_job_paths(caller, root, std::slice::from_ref(&entry), true)?;
    if job.state != "open"
        || matches!(
            entry.outcome.as_str(),
            "applied" | "skipped" | "conflict" | "failed"
        )
    {
        discard_stage(state_dir, job_id, entry_id)?;
        return Ok(entry_value(&entry));
    }
    if entry.kind != "dir" {
        let Some((path, len)) = staged_len(state_dir, job_id, entry_id)? else {
            return Err(reject(
                "wiki transfer apply refused: the entry has no staged bytes",
            ));
        };
        if len != entry.received {
            store.wiki_transfer_note_received(job_id, entry_id, len)?;
        }
        if len != entry.size {
            return Err(reject(
                "wiki transfer apply refused: staged bytes do not cover the declared size",
            ));
        }
        let actual = sha256_file(&path)?;
        if entry.sha256.as_deref() != Some(actual.as_str()) {
            store.wiki_transfer_record_outcome(
                job_id,
                &entry.entry_id,
                &EntryOutcome {
                    outcome: "failed",
                    rev: None,
                    extraction: Some("not_applicable"),
                    extraction_rev: None,
                    source_hash: None,
                    detail: Some("staged bytes do not match the manifest hash"),
                },
            )?;
            discard_stage(state_dir, job_id, entry_id)?;
            return Ok(json!({
                "job_id": job_id,
                "entry_id": entry_id,
                "path": entry.path,
                "outcome": "failed",
                "extraction": "not_applicable",
                "error": "staged bytes do not match the manifest hash",
            }));
        }
    }
    let outcome = publish_entry(pm, state_dir, caller, job_id, &entry)?;
    store.wiki_transfer_record_outcome(
        job_id,
        &entry.entry_id,
        &EntryOutcome {
            outcome: outcome.outcome,
            rev: outcome.rev.as_deref(),
            extraction: Some(outcome.extraction),
            extraction_rev: outcome.extraction_rev.as_deref(),
            source_hash: outcome.source_hash.as_deref(),
            detail: outcome.detail.as_deref(),
        },
    )?;
    // An unproved publication stays restartable. Known terminal outcomes
    // have already consumed the staged input.
    if outcome.outcome != "unresolved" {
        discard_stage(state_dir, job_id, entry_id)?;
    }
    let mut body = json!({
        "job_id": job_id,
        "entry_id": entry_id,
        "path": entry.path,
        "outcome": outcome.outcome,
        "extraction": outcome.extraction,
    });
    if let Some(rev) = outcome.rev {
        body["rev"] = json!(rev);
    }
    if let Some(detail) = outcome.detail {
        body["error"] = json!(detail);
    }
    Ok(body)
}

/// Progress is durable before private transfer input is discarded. Canonical
/// originals and Git revisions are never removed by transfer cleanup.
fn discard_stage(state_dir: &Path, job_id: &str, entry_id: &str) -> Result<()> {
    let path = stage_path(state_dir, job_id, entry_id);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if let Some(parent) = path.parent() {
        match fs::remove_dir(parent) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

struct Published {
    outcome: &'static str,
    rev: Option<String>,
    extraction: &'static str,
    extraction_rev: Option<String>,
    source_hash: Option<String>,
    detail: Option<String>,
}

fn publish_entry(
    pm: &Pm,
    state_dir: &Path,
    caller: &Caller,
    job_id: &str,
    entry: &EntryRow,
) -> Result<Published> {
    if entry.kind == "dir" {
        return publish_dir(pm, caller, entry);
    }
    match publication(pm, caller, entry)? {
        Publication::Committed => return reconcile_match(pm, caller, entry),
        Publication::Uncommitted => return Ok(unresolved(entry)),
        Publication::Absent => {}
    }
    let current = committed_rev(pm, entry)?;
    if current != entry.if_rev {
        return Ok(Published {
            outcome: "conflict",
            rev: Some(current),
            extraction: "conflict",
            extraction_rev: None,
            source_hash: entry.sha256.clone(),
            detail: Some("the destination revision changed before this entry was applied".into()),
        });
    }
    if entry.kind == "text" {
        let bytes = fs::read(stage_path(state_dir, job_id, &entry.entry_id))?;
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            reject(format!(
                "wiki transfer apply refused: '{}' is not UTF-8 text",
                entry.path
            ))
        })?;
        let wrote = write(pm, caller, &entry.path, text, Some(&entry.if_rev))?;
        if wrote.get("conflict").is_some() {
            return Ok(classify_publish_conflict(pm, caller, entry, &wrote)?);
        }
        if !committed_content_matches(pm, entry)? {
            return Ok(unresolved(entry));
        }
        return Ok(Published {
            outcome: "applied",
            rev: wrote["rev"].as_str().map(str::to_string),
            extraction: "not_applicable",
            extraction_rev: None,
            source_hash: entry.sha256.clone(),
            detail: None,
        });
    }
    publish_blob(pm, state_dir, caller, job_id, entry)
}

fn publish_dir(pm: &Pm, caller: &Caller, entry: &EntryRow) -> Result<Published> {
    match publication(pm, caller, entry)? {
        Publication::Committed => {
            return Ok(Published {
                outcome: "skipped",
                rev: None,
                extraction: "not_applicable",
                extraction_rev: None,
                source_hash: None,
                detail: None,
            });
        }
        Publication::Uncommitted => return Ok(unresolved(entry)),
        Publication::Absent => {}
    }
    match read(pm, caller, &entry.path) {
        Ok(page) if page["kind"] == "dir" => return Ok(unresolved(entry)),
        Ok(_) => {
            return Ok(Published {
                outcome: "conflict",
                rev: None,
                extraction: "conflict",
                extraction_rev: None,
                source_hash: None,
                detail: Some("a file exists where the directory was declared".into()),
            });
        }
        Err(error) if missing(&error) => {}
        Err(error) => return Err(error),
    }
    let made = mkdir(pm, caller, &entry.path)?;
    if !committed_dir(pm, &entry.path)? {
        return Ok(unresolved(entry));
    }
    Ok(Published {
        outcome: if made["created"] == true {
            "applied"
        } else {
            "skipped"
        },
        rev: None,
        extraction: "not_applicable",
        extraction_rev: None,
        source_hash: None,
        detail: None,
    })
}

enum Publication {
    /// HEAD contains the desired bytes or directory marker.
    Committed,
    /// The worktree matches, but HEAD does not. Not publication.
    Uncommitted,
    /// Neither tree has the desired content.
    Absent,
}

fn unresolved(entry: &EntryRow) -> Published {
    let detail = if entry.kind == "dir" {
        "wiki transfer publication is not committed"
    } else {
        "wiki transfer publication is not committed; staged bytes were kept"
    };
    Published {
        outcome: "unresolved",
        rev: None,
        extraction: "not_applicable",
        extraction_rev: None,
        source_hash: entry.sha256.clone(),
        detail: Some(detail.into()),
    }
}

fn publication(pm: &Pm, caller: &Caller, entry: &EntryRow) -> Result<Publication> {
    if entry.kind == "dir" {
        if committed_dir(pm, &entry.path)? {
            return Ok(Publication::Committed);
        }
        return match read(pm, caller, &entry.path) {
            Ok(page) if page["kind"] == "dir" => Ok(Publication::Uncommitted),
            Ok(_) => Ok(Publication::Absent),
            Err(error) if missing(&error) => Ok(Publication::Absent),
            Err(error) => Err(error),
        };
    }
    if committed_content_matches(pm, entry)? {
        return Ok(Publication::Committed);
    }
    if worktree_content_matches(pm, caller, entry)? {
        return Ok(Publication::Uncommitted);
    }
    Ok(Publication::Absent)
}

/// A returned `if_rev` conflict was decided before the pointer write. Absent
/// content there is a real conflict. An `Err` is not: another authorized
/// write can replace a commit before this probe, so absence is not proof
/// the publish never landed.
fn classify_publish_conflict(
    pm: &Pm,
    caller: &Caller,
    entry: &EntryRow,
    wrote: &Value,
) -> Result<Published> {
    match publication(pm, caller, entry)? {
        Publication::Committed => reconcile_match(pm, caller, entry),
        Publication::Uncommitted => Ok(unresolved(entry)),
        Publication::Absent => Ok(Published {
            outcome: "conflict",
            rev: wrote["current_rev"].as_str().map(str::to_string),
            extraction: "conflict",
            extraction_rev: None,
            source_hash: entry.sha256.clone(),
            detail: Some("the destination revision changed".into()),
        }),
    }
}

fn classify_publish_error(
    pm: &Pm,
    caller: &Caller,
    entry: &EntryRow,
    error: &Error,
) -> Result<Published> {
    let detail = match publication(pm, caller, entry) {
        Ok(Publication::Committed) => return reconcile_match(pm, caller, entry),
        Ok(Publication::Uncommitted | Publication::Absent) => format!(
            "wiki transfer publication could not be proved; staged bytes were kept ({error})"
        ),
        Err(probe) => format!(
            "wiki transfer publication could not be proved; staged bytes were kept ({error}; {probe})"
        ),
    };
    Ok(Published {
        outcome: "unresolved",
        rev: None,
        extraction: "not_applicable",
        extraction_rev: None,
        source_hash: entry.sha256.clone(),
        detail: Some(detail),
    })
}

fn tracker_rel(pm: &Pm, vault_rel: &str) -> Result<String> {
    let vault = vault_dir(pm)?;
    let abs = vault.join(vault_rel);
    let rel = abs.strip_prefix(&pm.dir).map_err(|_| {
        Error::internal(format!(
            "wiki path {} is outside tracker {}",
            abs.display(),
            pm.dir.display()
        ))
    })?;
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

fn git_output(pm: &Pm, args: &[&str]) -> Result<std::process::Output> {
    crate::reaper::output(Command::new("git").arg("-C").arg(&pm.dir).args(args))
        .map_err(|_| Error::rejected("`git` is required and was not found on PATH"))
}

fn git_path_missing(stderr: &str) -> bool {
    stderr.contains("does not exist")
        || stderr.contains("exists on disk, but not in")
        || stderr.contains("Not a valid object name")
}

/// Bytes of one committed vault path. `Ok(None)` is a path absent from HEAD,
/// including a crash that renamed the worktree file and never committed it.
fn committed_blob(pm: &Pm, vault_rel: &str) -> Result<Option<Vec<u8>>> {
    let spec = format!("HEAD:{}", tracker_rel(pm, vault_rel)?);
    let out = git_output(pm, &["cat-file", "blob", &spec])?;
    if out.status.success() {
        return Ok(Some(out.stdout));
    }
    let err = String::from_utf8_lossy(&out.stderr);
    if git_path_missing(&err) {
        return Ok(None);
    }
    Err(Error::internal(format!(
        "wiki transfer could not read committed '{vault_rel}': {}",
        err.trim()
    )))
}

/// A directory is committed when HEAD has a tree there, whether or not this
/// transfer is the call that added `.gitkeep`.
fn committed_dir(pm: &Pm, vault_rel: &str) -> Result<bool> {
    let spec = format!("HEAD:{}", tracker_rel(pm, vault_rel)?);
    let out = git_output(pm, &["cat-file", "-t", &spec])?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim() == "tree");
    }
    let err = String::from_utf8_lossy(&out.stderr);
    if git_path_missing(&err) {
        return Ok(false);
    }
    Err(Error::internal(format!(
        "wiki transfer could not read committed directory '{vault_rel}': {}",
        err.trim()
    )))
}

fn committed_rev(pm: &Pm, entry: &EntryRow) -> Result<String> {
    let rel = if entry.kind == "blob" {
        format!("{}.blob", entry.path)
    } else {
        entry.path.clone()
    };
    Ok(match committed_blob(pm, &rel)? {
        Some(bytes) => super::rev_bytes(&bytes),
        None => ABSENT.to_string(),
    })
}

fn committed_content_matches(pm: &Pm, entry: &EntryRow) -> Result<bool> {
    match entry.kind.as_str() {
        "text" => {
            let Some(bytes) = committed_blob(pm, &entry.path)? else {
                return Ok(false);
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                return Ok(false);
            };
            Ok(text.len() as u64 == entry.size
                && Some(sha256_bytes(text.as_bytes()).as_str()) == entry.sha256.as_deref())
        }
        "blob" => {
            let Some(bytes) = committed_blob(pm, &format!("{}.blob", entry.path))? else {
                return Ok(false);
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                return Ok(false);
            };
            let Ok(meta) = serde_json::from_str::<Value>(text) else {
                return Ok(false);
            };
            if meta["size"].as_u64() != Some(entry.size)
                || meta["sha256"].as_str() != entry.sha256.as_deref()
            {
                return Ok(false);
            }
            let hash = entry
                .sha256
                .as_deref()
                .ok_or_else(|| reject("missing transfer hash"))?;
            verify_blob_custody(pm, hash, entry.size)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn verify_blob_custody(pm: &Pm, hash: &str, size: u64) -> Result<()> {
    let blob = blobs_dir(&vault_dir(pm)?).join(hash);
    let file = open_nofollow(&blob)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() != size {
        return Err(reject("existing original has invalid custody or length"));
    }
    if sha256_file(&blob)? != hash {
        return Err(reject("existing original bytes do not match their hash"));
    }
    Ok(())
}

fn worktree_content_matches(pm: &Pm, caller: &Caller, entry: &EntryRow) -> Result<bool> {
    let page = match read(pm, caller, &entry.path) {
        Ok(page) => page,
        Err(error) if missing(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    if !page_matches(&page, &entry.kind, entry.size, entry.sha256.as_deref()) {
        return Ok(false);
    }
    if entry.kind == "blob" {
        let hash = entry
            .sha256
            .as_deref()
            .ok_or_else(|| reject("missing transfer hash"))?;
        verify_blob_custody(pm, hash, entry.size)?;
    }
    Ok(true)
}

fn page_matches(page: &Value, kind: &str, size: u64, hash: Option<&str>) -> bool {
    if page["kind"].as_str() != Some(kind) {
        return false;
    }
    match kind {
        "text" => page["text"].as_str().is_some_and(|text| {
            text.len() as u64 == size && Some(sha256_bytes(text.as_bytes()).as_str()) == hash
        }),
        "blob" => page["size"].as_u64() == Some(size) && page["sha256"].as_str() == hash,
        "dir" => true,
        _ => false,
    }
}

fn reconcile_match(pm: &Pm, caller: &Caller, entry: &EntryRow) -> Result<Published> {
    let rev = Some(committed_rev(pm, entry)?);
    if entry.kind == "blob" && is_pdf(&entry.path) {
        let hash = entry.sha256.clone().unwrap_or_default();
        let (extraction, extraction_rev, detail) = extraction_of(pm, caller, &entry.path, &hash);
        return Ok(Published {
            outcome: "skipped",
            rev,
            extraction,
            extraction_rev,
            source_hash: entry.sha256.clone(),
            detail,
        });
    }
    Ok(Published {
        outcome: "skipped",
        rev,
        extraction: "not_applicable",
        extraction_rev: None,
        source_hash: entry.sha256.clone(),
        detail: None,
    })
}

fn is_pdf(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase()
        .ends_with(".pdf")
}

/// The generated extraction header is the only provenance. A matching body
/// marker is not a source hash or status. `None` means this header does not
/// name the source at this hash: apply then derives from the raw file only
/// when the header names that source, and index coverage stays unproved.
/// An exact header whose status is not `indexed`, `empty`, or `failed` is
/// `conflict` and is not retried.
fn generated_extraction_status(text: &str, source_path: &str, sha: &str) -> Option<&'static str> {
    let header = ingest::extraction_header(text)?;
    if header.source_path != source_path || header.sha256 != sha {
        return None;
    }
    Some(match header.status {
        "indexed" => "indexed",
        "empty" => "empty",
        "failed" => "failed",
        _ => "conflict",
    })
}

fn extraction_of(
    pm: &Pm,
    caller: &Caller,
    path: &str,
    sha: &str,
) -> (&'static str, Option<String>, Option<String>) {
    let sidecar = format!("{path}.extracted.md");
    match read(pm, caller, &sidecar) {
        Ok(page) => {
            let text = page["text"].as_str().unwrap_or("");
            if let Some(status) = generated_extraction_status(text, path, sha) {
                return (status, page["rev"].as_str().map(str::to_string), None);
            }
            // A complete header for this source with a different hash is a
            // reimport. Anything else, including a foreign header whose body
            // quotes this path, is not ours to replace.
            match ingest::extraction_header(text) {
                Some(header) if header.source_path == path => {}
                _ => {
                    return (
                        "conflict",
                        page["rev"].as_str().map(str::to_string),
                        Some(
                            "a page at the extraction path was not generated from this source"
                                .into(),
                        ),
                    );
                }
            }
        }
        Err(error) if missing(&error) => {}
        Err(error) => return ("failed", None, Some(error.to_string())),
    }
    let result = ingest::pdf_result(pm, caller, path, sha);
    let status = match result["status"].as_str() {
        Some("indexed" | "empty" | "failed" | "conflict") => {
            match result["status"].as_str().unwrap() {
                "indexed" => "indexed",
                "empty" => "empty",
                "conflict" => "conflict",
                _ => "failed",
            }
        }
        _ => "failed",
    };
    let rev = read(pm, caller, &sidecar)
        .ok()
        .and_then(|page| page["rev"].as_str().map(str::to_string));
    let detail = result["reason"].as_str().map(str::to_string);
    (status, rev, detail)
}

fn publish_blob(
    pm: &Pm,
    state_dir: &Path,
    caller: &Caller,
    job_id: &str,
    entry: &EntryRow,
) -> Result<Published> {
    let stage = stage_path(state_dir, job_id, &entry.entry_id);
    let uploads = state_dir.join(UPLOAD_DIR);
    fs::create_dir_all(&uploads)?;
    let tmp = uploads.join(format!("transfer-{}", uuid::Uuid::new_v4().simple()));
    fs::copy(&stage, &tmp)?;
    let wrote = match put_blob(
        pm,
        state_dir,
        caller,
        &entry.path,
        &tmp,
        entry.sha256.as_deref(),
        Some(&entry.if_rev),
    ) {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            return classify_publish_error(pm, caller, entry, &error);
        }
    };
    if wrote.get("conflict").is_some() {
        return Ok(classify_publish_conflict(pm, caller, entry, &wrote)?);
    }
    if !committed_content_matches(pm, entry)? {
        return Ok(unresolved(entry));
    }
    let hash = wrote["sha256"].as_str().unwrap_or("").to_string();
    let (extraction, extraction_rev, detail) = if is_pdf(&entry.path) {
        extraction_of(pm, caller, &entry.path, &hash)
    } else {
        ("not_applicable", None, None)
    };
    Ok(Published {
        outcome: "applied",
        rev: wrote["rev"].as_str().map(str::to_string),
        extraction,
        extraction_rev,
        source_hash: Some(hash),
        detail,
    })
}

fn entry_value(entry: &EntryRow) -> Value {
    let mut body = json!({
        "entry_id": entry.entry_id,
        "path": entry.rel_path,
        "full_path": entry.path,
        "kind": entry.kind,
        "size": entry.size,
        "sha256": entry.sha256,
        "received": entry.received,
        "outcome": entry.outcome,
        "manifest_entry": manifest_entry(entry),
        "extraction": entry.extraction.clone().unwrap_or_else(|| "not_applicable".into()),
    });
    if let Some(rev) = &entry.rev {
        body["rev"] = json!(rev);
    }
    if let Some(detail) = &entry.detail {
        body["error"] = json!(detail);
    }
    body
}

struct JsonByteCount(usize);

impl Write for JsonByteCount {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Bound metadata by serialized bytes as well as entry count. Long paths and
/// escaped failure details must not turn a status/export page into an oversized
/// native response. Reserve room for the enclosing receipt and RPC envelope.
fn bounded_metadata_page(values: impl Iterator<Item = Value>) -> Result<Vec<Value>> {
    const BUDGET: usize = super::NATIVE_TOOL_LINE_BOUND - 4096;
    let capacity = values
        .size_hint()
        .1
        .unwrap_or(0)
        .min(STATUS_PAGE_MAX as usize);
    let mut page = Vec::with_capacity(capacity);
    let mut used = 2usize;
    for value in values {
        let mut count = JsonByteCount(0);
        serde_json::to_writer(&mut count, &value)
            .map_err(|error| Error::internal(format!("wiki transfer metadata: {error}")))?;
        let bytes = count.0.saturating_add(1);
        if used.saturating_add(bytes) > BUDGET {
            if page.is_empty() {
                return Err(reject(
                    "wiki transfer metadata exceeds the native response bound",
                ));
            }
            break;
        }
        used += bytes;
        page.push(value);
    }
    Ok(page)
}

/// Original admitted fields. These do not change after publication, so a
/// resumed `--update` reuses `if_rev` instead of discovering a new digest.
fn manifest_entry(entry: &EntryRow) -> Value {
    json!({
        "id": entry.entry_id,
        "path": entry.rel_path,
        "kind": entry.kind,
        "size": entry.size,
        "sha256": entry.sha256,
        "if_rev": entry.if_rev,
    })
}

/// `{op:finish}`. The index kick flag is true only the first time a closed
/// job still needs the coalesced rebuild.
pub fn finish(
    store: &Store,
    pm: &Pm,
    state_dir: &Path,
    request: &JobRequest<'_>,
) -> Result<(Value, bool)> {
    let caller = request.caller;
    let job_id = request.job_id;
    let root = request.root;
    let job = require_job(store, request)?;
    let entries = store.wiki_transfer_entries(job_id)?;
    recheck_job_paths(caller, root, &entries, true)?;
    if entries.iter().any(|entry| entry.outcome == "unresolved") {
        return Err(reject(
            "wiki transfer finish refused: publication is not committed",
        ));
    }
    if entries.iter().any(|entry| entry.outcome == "pending") {
        return Err(reject(
            "wiki transfer finish refused: entries are still pending",
        ));
    }
    let root_created = match ensure_root(pm, caller, root) {
        Ok(created) => created,
        Err(error) => {
            let kick = store.wiki_transfer_finish(job_id, "failed", false)?;
            return Ok((
                json!({
                    "job_id": job.job_id,
                    "digest": job.digest,
                    "root": job.root,
                    "state": "failed",
                    "index": "pending",
                    "error": error.to_string(),
                }),
                kick,
            ));
        }
    };
    let applied = entries
        .iter()
        .filter(|entry| matches!(entry.outcome.as_str(), "applied" | "skipped"))
        .count();
    let state = if applied == entries.len() {
        "complete"
    } else if applied == 0 {
        "failed"
    } else {
        "partial"
    };
    let wants_index = root_created
        || state != "failed"
        || entries.iter().any(|entry| {
            matches!(entry.outcome.as_str(), "applied" | "skipped")
                && (entry.kind == "text" || is_pdf(&entry.path))
        });
    let kick = store.wiki_transfer_finish(job_id, state, wants_index)?;
    let index = index_label(pm, state_dir, caller, &entries);
    Ok((
        json!({
            "job_id": job.job_id,
            "digest": job.digest,
            "root": job.root,
            "state": state,
            "index": index,
        }),
        kick,
    ))
}

/// `{op:status}` pages this caller's entries. Another owner gets the same
/// not-found refusal as a missing job.
pub fn status(
    store: &Store,
    pm: &Pm,
    state_dir: &Path,
    request: &JobRequest<'_>,
    offset: u64,
    limit: u64,
) -> Result<Value> {
    if limit == 0 || limit > STATUS_PAGE_MAX {
        return Err(reject(format!(
            "wiki transfer status refused: limit must be 1 to {STATUS_PAGE_MAX}"
        )));
    }
    let caller = request.caller;
    let job_id = request.job_id;
    let root = request.root;
    let job = require_job(store, request)?;
    let entries = store.wiki_transfer_entries(job_id)?;
    recheck_job_paths(caller, root, &entries, false)?;
    let start = usize::try_from(offset).unwrap_or(usize::MAX);
    let page = bounded_metadata_page(
        entries
            .iter()
            .skip(start)
            .take(limit as usize)
            .map(entry_value),
    )?;
    let next = start.saturating_add(page.len());
    let next_offset = if next < entries.len() {
        Some(next as u64)
    } else {
        None
    };
    let index = if job.state == "open" {
        "pending".to_string()
    } else {
        index_label(pm, state_dir, caller, &entries)
    };
    Ok(json!({
        "job_id": job.job_id,
        "digest": job.digest,
        "source_id": job.source_id,
        "root": job.root,
        "state": job.state,
        "index": index,
        "entries": page,
        "next_offset": next_offset,
    }))
}

/// Live index receipt. Failed extraction is never `covered`. A later
/// overwrite of a recorded revision is `superseded`. An intact revision
/// set is `covered` only when the current committed index tree contains
/// those revisions. The tree id and index counts stay out of the value.
fn index_label(pm: &Pm, state_dir: &Path, caller: &Caller, entries: &[EntryRow]) -> String {
    let mut needed = Vec::new();
    for entry in entries {
        if !matches!(entry.outcome.as_str(), "applied" | "skipped") {
            return "pending".into();
        }
        if entry.kind == "dir" {
            if !read(pm, caller, &entry.path).is_ok_and(|page| page["kind"] == "dir") {
                return "superseded".into();
            }
            continue;
        }
        let Some(recorded) = entry.rev.as_deref() else {
            return "pending".into();
        };
        if !current_rev(pm, caller, &entry.path).is_ok_and(|live| live == recorded) {
            return "superseded".into();
        }
        if entry.kind == "blob" && !is_pdf(&entry.path) {
            continue;
        }
        if entry.kind == "blob" {
            if entry.extraction.as_deref() != Some("indexed") {
                return "pending".into();
            }
            let sidecar = format!("{}.extracted.md", entry.path);
            let Some(extracted_rev) = entry.extraction_rev.as_deref() else {
                return "pending".into();
            };
            let matches = read(pm, caller, &sidecar).is_ok_and(|page| {
                page["rev"].as_str() == Some(extracted_rev)
                    && page["text"].as_str().is_some_and(|text| {
                        entry.source_hash.as_deref().is_some_and(|hash| {
                            generated_extraction_status(text, &entry.path, hash) == Some("indexed")
                        })
                    })
            });
            if !matches {
                return "superseded".into();
            }
            needed.push((sidecar, extracted_rev.to_string(), false));
        } else {
            needed.push((entry.path.clone(), recorded.to_string(), entry.size == 0));
        }
    }
    if needed.is_empty() {
        return "covered".into();
    }
    let Some(tree) = committed_tree(pm) else {
        return "pending".into();
    };
    if index_tree(state_dir).as_deref() != Some(tree.as_str()) {
        return "pending".into();
    }
    for (path, rev, empty) in needed {
        if !empty && indexed_page_rev(state_dir, &path).as_deref() != Some(rev.as_str()) {
            return "pending".into();
        }
    }
    "covered".into()
}

fn export_dir_entry(pm: &Pm, caller: &Caller, item: &Value) -> Value {
    let path = item["path"].as_str().unwrap_or("");
    let name = item["name"].as_str().unwrap_or("");
    let listed = item["kind"].as_str().unwrap_or("");
    let page = read(pm, caller, path).ok();
    let kind = page
        .as_ref()
        .and_then(|p| p["kind"].as_str())
        .unwrap_or(listed);
    let rev = page.as_ref().and_then(|p| p["rev"].as_str());
    let (size, sha) = match kind {
        "text" => page
            .as_ref()
            .and_then(|p| p["text"].as_str())
            .map(|text| (Some(text.len() as u64), Some(sha256_bytes(text.as_bytes()))))
            .unwrap_or((None, None)),
        "blob" => (
            page.as_ref().and_then(|p| p["size"].as_u64()),
            page.as_ref()
                .and_then(|p| p["sha256"].as_str())
                .map(str::to_string),
        ),
        _ => (None, None),
    };
    json!({
        "path": path,
        "name": name,
        "kind": kind,
        "rev": rev,
        "size": size,
        "sha256": sha,
    })
}

fn committed_tree(pm: &Pm) -> Option<String> {
    let vault = vault_dir(pm).ok()?;
    let rel = vault.strip_prefix(&pm.dir).ok()?;
    let rel = rel.to_str()?.replace('\\', "/");
    let tree = format!("HEAD:{rel}");
    crate::issue::git(&pm.dir, &["rev-parse", "--verify", &tree]).ok()
}

fn index_tree(state_dir: &Path) -> Option<String> {
    let conn = rusqlite::Connection::open_with_flags(
        super::index::db_path(state_dir),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    conn.query_row(
        "SELECT value FROM wiki_index_meta WHERE key='revision'",
        [],
        |row| row.get(0),
    )
    .ok()
}

fn indexed_page_rev(state_dir: &Path, path: &str) -> Option<String> {
    let conn = rusqlite::Connection::open_with_flags(
        super::index::db_path(state_dir),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    conn.query_row(
        "SELECT rev FROM wiki_index_chunks WHERE path=?1 LIMIT 1",
        [path],
        |row| row.get(0),
    )
    .ok()
}

/// `{op:export}` reads one caller-visible path. `rev` pins a file across slices.
pub fn export(
    pm: &Pm,
    caller: &Caller,
    path: &str,
    offset: u64,
    bytes: Option<u64>,
    rev: Option<&str>,
) -> Result<Value> {
    let want = bytes.unwrap_or(MAX_CHUNK_BYTES as u64);
    if want == 0 || want > MAX_CHUNK_BYTES as u64 {
        return Err(reject(format!(
            "wiki transfer export refused: bytes must be 1 to {MAX_CHUNK_BYTES}"
        )));
    }
    let page = read(pm, caller, path)?;
    let kind = page["kind"].as_str().unwrap_or("");
    if kind == "dir" {
        let listing = ls(pm, caller, path)?;
        let entries = listing["entries"].as_array().cloned().unwrap_or_default();
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let slice = bounded_metadata_page(
            entries
                .iter()
                .skip(start)
                .take(STATUS_PAGE_MAX as usize)
                .map(|item| export_dir_entry(pm, caller, item)),
        )?;
        let next = start.saturating_add(slice.len());
        let next_offset = if next < entries.len() {
            Some(next as u64)
        } else {
            None
        };
        return Ok(json!({
            "path": page["path"],
            "kind": "dir",
            "entries": slice,
            "next_offset": next_offset,
        }));
    }
    if kind == "text" {
        let full = &page;
        let text = full["text"].as_str().unwrap_or("");
        let live = full["rev"].as_str().unwrap_or("");
        if let Some(pinned) = rev {
            if pinned != live {
                return Err(reject(format!(
                    "wiki transfer export refused: revision '{pinned}' is not '{live}'"
                )));
            }
        }
        let total = text.len() as u64;
        if offset > total {
            return Err(reject(
                "wiki transfer export refused: offset is past the text",
            ));
        }
        // Base64 carries bytes, so a bounded slice may split a UTF-8 codepoint.
        // The client verifies the complete hash and UTF-8 before publication.
        let start = offset as usize;
        let end = start.saturating_add(want as usize).min(text.len());
        let slice = &text.as_bytes()[start..end];
        let end = end as u64;
        let next_offset = if end < total { Some(end) } else { None };
        return Ok(json!({
            "path": full["path"],
            "kind": "text",
            "rev": live,
            "size": total,
            "sha256": sha256_bytes(text.as_bytes()),
            "offset": offset,
            "data_base64": STANDARD.encode(slice),
            "next_offset": next_offset,
        }));
    }
    if kind == "blob" {
        let live = page["rev"].as_str().unwrap_or("");
        if let Some(pinned) = rev {
            if pinned != live {
                return Err(reject(format!(
                    "wiki transfer export refused: revision '{pinned}' is not '{live}'"
                )));
            }
        }
        let hash = page["sha256"].as_str().unwrap_or("");
        if !sha_ok(hash) {
            return Err(reject(
                "wiki transfer export refused: blob pointer has no hash",
            ));
        }
        let declared = page["size"].as_u64().unwrap_or(0);
        let file = blobs_dir(&vault_dir(pm)?).join(hash);
        let mut handle = open_nofollow(&file)?;
        let meta = handle.metadata()?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() != declared {
            return Err(reject(
                "wiki transfer export refused: blob bytes do not match the pointer",
            ));
        }
        // Originals are immutable and content-addressed at publication. The
        // exporter verifies the whole pinned SHA before its atomic local write;
        // rehashing a 100 MiB original for every 16 KiB slice would be quadratic.
        if offset > declared {
            return Err(reject(
                "wiki transfer export refused: offset is past the blob",
            ));
        }
        handle.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; want as usize];
        let n = handle.read(&mut buf)?;
        buf.truncate(n);
        let end = offset + n as u64;
        let next_offset = if end < declared { Some(end) } else { None };
        return Ok(json!({
            "path": page["path"],
            "kind": "blob",
            "rev": live,
            "size": declared,
            "sha256": hash,
            "offset": offset,
            "data_base64": STANDARD.encode(&buf),
            "next_offset": next_offset,
        }));
    }
    Err(reject(
        "wiki transfer export refused: unsupported page kind",
    ))
}

/// Owner key for a daemon caller. `binding` is accepted only after the RPC
/// layer has proved it came from the operator connection.
pub fn owner_for(actor: &str, binding: Option<&Value>) -> Result<String> {
    let checked = match binding {
        None => None,
        Some(value) => Some(checked_binding(value)?),
    };
    Ok(wiki_transfers::owner_key(actor, checked.as_ref()))
}

fn checked_binding(value: &Value) -> Result<Value> {
    let Some(obj) = value.as_object() else {
        return Err(reject(
            "wiki transfer refused: authority binding is not an object",
        ));
    };
    if obj.len() != 4 {
        return Err(reject(
            "wiki transfer refused: authority binding has unexpected fields",
        ));
    }
    let mut out = serde_json::Map::new();
    for key in ["issuer", "organization_id", "audience", "subject"] {
        let Some(text) = obj.get(key).and_then(Value::as_str) else {
            return Err(reject(format!(
                "wiki transfer refused: authority binding lacks {key}"
            )));
        };
        if text.is_empty() || text.len() > 256 || text.chars().any(|c| c.is_control()) {
            return Err(reject(format!(
                "wiki transfer refused: authority binding {key} is empty or too long"
            )));
        }
        out.insert(key.to_string(), json!(text));
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::{generated_extraction_status, sha256_bytes, stage_path, STANDARD};
    use crate::issue::Pm;
    use crate::store::Store;
    use crate::wiki::{blobs_dir, read, vault_dir, write, Caller, ABSENT};
    use base64::Engine;
    use std::fs;

    #[test]
    fn digest_follows_path_order_not_input_order() {
        let mut manifest = Manifest {
            version: 1,
            source_id: "src".into(),
            root: "global/box".into(),
            entries: vec![
                ManifestEntry {
                    id: "b".into(),
                    path: "b.txt".into(),
                    kind: "text".into(),
                    size: 1,
                    sha256: Some("ab".into()),
                    if_rev: "none".into(),
                },
                ManifestEntry {
                    id: "a".into(),
                    path: "a.txt".into(),
                    kind: "text".into(),
                    size: 1,
                    sha256: Some("cd".into()),
                    if_rev: "none".into(),
                },
            ],
        };
        let forward = manifest_digest(&manifest).unwrap();
        manifest.entries.swap(0, 1);
        assert_eq!(manifest_digest(&manifest).unwrap(), forward);
    }

    #[test]
    fn metadata_pages_bound_escaped_bytes_without_losing_entries() {
        let mut offset = 0;
        let mut seen = Vec::new();
        for _ in 0..32 {
            if offset == 32 {
                break;
            }
            let page = bounded_metadata_page((offset..32).map(|id| {
                let relative = format!("{id}/{}", "n".repeat(440));
                json!({
                    "entry_id": format!("entry-{id}"),
                    "path": relative,
                    "full_path": format!("global/qa/{relative}"),
                    "manifest_entry": {"path": relative},
                    "error": "\0".repeat(512),
                })
            }))
            .unwrap();
            offset += page.len();
            seen.extend(
                page.iter()
                    .map(|entry| entry["entry_id"].as_str().unwrap().to_string()),
            );
            let reply = json!({
                "job_id": "fixture",
                "root": "global/qa",
                "entries": page,
                "next_offset": if offset < 32 { Some(offset) } else { None },
            });
            assert!(
                serde_json::to_vec(&reply).unwrap().len() < super::super::NATIVE_TOOL_LINE_BOUND
            );
        }
        assert_eq!(
            seen,
            (0..32).map(|id| format!("entry-{id}")).collect::<Vec<_>>()
        );
    }

    fn generated_extraction_page(path: &str, sha: &str, status: &str, body: &str) -> String {
        let quoted_path = serde_json::to_string(path).unwrap();
        let name = path.rsplit('/').next().unwrap_or(path);
        format!(
            "---\nkind: extracted-source\nsource_path: {quoted_path}\nsource_sha256: {sha}\nextraction_status: {status}\n---\n\n# Source text: {name}\n\n{body}\n"
        )
    }

    #[test]
    fn stale_extraction_header_ignores_body_status_markers() {
        let path = "global/qa/scan.pdf";
        let live = "a".repeat(64);
        let stale = "b".repeat(64);
        let indexed = generated_extraction_page(
            path,
            &live,
            "indexed",
            "source_sha256: stale\nextraction_status: failed\n",
        );
        assert_eq!(
            super::generated_extraction_status(&indexed, path, &live),
            Some("indexed")
        );

        let lied = generated_extraction_page(
            path,
            &stale,
            "failed",
            &format!("source_sha256: {live}\nextraction_status: indexed\n"),
        );
        assert_eq!(super::generated_extraction_status(&lied, path, &live), None);
        assert_eq!(
            super::generated_extraction_status(&lied, path, &stale),
            Some("failed")
        );

        assert_eq!(
            super::generated_extraction_status(
                &generated_extraction_page(path, &live, "empty", ""),
                path,
                &live
            ),
            Some("empty")
        );
        assert_eq!(
            super::generated_extraction_status(
                &generated_extraction_page(path, &live, "failed", "extraction_status: indexed\n",),
                path,
                &live
            ),
            Some("failed")
        );

        let quoted = serde_json::to_string(path).unwrap();
        let unclosed = format!(
            "---\nkind: extracted-source\nsource_path: {quoted}\nsource_sha256: {live}\nextraction_status: indexed\n\nsource_sha256: {live}\nextraction_status: indexed\n---\n"
        );
        assert_eq!(
            super::generated_extraction_status(&unclosed, path, &live),
            None
        );
        let body_only = format!("source_sha256: {live}\nextraction_status: indexed\n");
        assert_eq!(
            super::generated_extraction_status(&body_only, path, &live),
            None
        );

        for status in ["indexed ", "indexedX", "emptyish", "failure", ""] {
            let page =
                generated_extraction_page(path, &live, status, "extraction_status: indexed\n");
            assert_eq!(
                super::generated_extraction_status(&page, path, &live),
                Some("conflict"),
                "{status:?}"
            );
        }

        let other = generated_extraction_page(
            "global/qa/other.pdf",
            &live,
            "indexed",
            &format!("source_path: {quoted}\nsource_sha256: {live}\nextraction_status: indexed\n"),
        );
        assert_eq!(
            super::generated_extraction_status(&other, path, &live),
            None
        );

        let escaped = "global/qa/a\"b\\c.pdf";
        assert_eq!(
            super::generated_extraction_status(
                &generated_extraction_page(escaped, &live, "indexed", "source_sha256: no\n"),
                escaped,
                &live
            ),
            Some("indexed")
        );
    }

    fn fixture() -> (tempfile::TempDir, Pm, Store, std::path::PathBuf) {
        let root = tempfile::TempDir::new().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let state = root.path().join("state");
        fs::create_dir_all(&state).unwrap();
        let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
        (root, pm, store, state)
    }

    fn head_commit(pm: &Pm) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&pm.dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn tracker_rel(pm: &Pm, vault_rel: &str) -> String {
        let vault = vault_dir(pm).unwrap();
        vault
            .join(vault_rel)
            .strip_prefix(&pm.dir)
            .unwrap()
            .to_str()
            .unwrap()
            .replace('\\', "/")
    }

    fn committed_bytes(pm: &Pm, vault_rel: &str) -> Option<Vec<u8>> {
        let spec = format!("HEAD:{}", tracker_rel(pm, vault_rel));
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&pm.dir)
            .args(["cat-file", "blob", &spec])
            .output()
            .unwrap();
        if out.status.success() {
            Some(out.stdout)
        } else {
            None
        }
    }

    fn commit_paths(pm: &Pm, rels: &[&str]) {
        let mut add = std::process::Command::new("git");
        add.arg("-C").arg(&pm.dir).args(["add", "--"]);
        add.args(rels);
        assert!(add.status().unwrap().success());
        let mut commit = std::process::Command::new("git");
        commit.arg("-C").arg(&pm.dir).args([
            "-c",
            "user.name=cadence",
            "-c",
            "user.email=cadence@localhost",
            "commit",
            "-q",
            "-m",
            "fixture publication",
            "--",
        ]);
        commit.args(rels);
        let status = commit.status().unwrap();
        assert!(status.success(), "fixture commit failed");
    }

    fn text_manifest(relative: &str, bytes: &[u8], if_rev: &str) -> Manifest {
        Manifest {
            version: 1,
            source_id: "src".into(),
            root: "global/box".into(),
            entries: vec![ManifestEntry {
                id: "note".into(),
                path: relative.into(),
                kind: "text".into(),
                size: bytes.len() as u64,
                sha256: Some(sha256_bytes(bytes)),
                if_rev: if_rev.into(),
            }],
        }
    }

    fn stage_and_apply(
        store: &Store,
        pm: &Pm,
        state: &std::path::Path,
        manifest: &Manifest,
        entry_id: &str,
        bytes: &[u8],
    ) -> Value {
        let caller = Caller::Operator;
        let owner = "transfer-reconcile";
        let started = begin(store, pm, &caller, owner, None, manifest, 128).unwrap();
        let job_id = started["job_id"].as_str().unwrap().to_string();
        let request = JobRequest {
            caller: &caller,
            owner,
            job_id: &job_id,
            root: "global/box",
        };
        if !bytes.is_empty() || manifest.entries[0].kind != "dir" {
            chunk(store, state, &request, entry_id, 0, &STANDARD.encode(bytes)).unwrap();
        }
        apply(store, pm, state, &request, entry_id).unwrap()
    }

    /// Desired bytes in the worktree, missing from HEAD, are not publication.
    #[test]
    fn uncommitted_text_match_is_unresolved_and_keeps_stage() {
        let (_root, pm, store, state) = fixture();
        let bytes = b"hello";
        let manifest = text_manifest("note.txt", bytes, ABSENT);
        let caller = Caller::Operator;
        let owner = "transfer-reconcile";
        let started = begin(&store, &pm, &caller, owner, None, &manifest, 128).unwrap();
        let job_id = started["job_id"].as_str().unwrap().to_string();
        let request = JobRequest {
            caller: &caller,
            owner,
            job_id: &job_id,
            root: "global/box",
        };
        chunk(&store, &state, &request, "note", 0, &STANDARD.encode(bytes)).unwrap();
        let dest = vault_dir(&pm).unwrap().join("global/box/note.txt");
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&dest, bytes).unwrap();
        let head = head_commit(&pm);
        let body = apply(&store, &pm, &state, &request, "note").unwrap();
        assert_eq!(body["outcome"], "unresolved", "{body}");
        assert!(stage_path(&state, &job_id, "note").is_file());
        assert_eq!(head_commit(&pm), head);
        assert!(committed_bytes(&pm, "global/box/note.txt").is_none());
        assert_eq!(fs::read(&dest).unwrap(), bytes);
        let finished = finish(&store, &pm, &state, &request);
        assert!(
            finished.is_err(),
            "unresolved publication must not finish: {finished:?}"
        );

        commit_paths(&pm, &[&tracker_rel(&pm, "global/box/note.txt")]);
        let committed_head = head_commit(&pm);
        let retry = apply(&store, &pm, &state, &request, "note").unwrap();
        assert_eq!(retry["outcome"], "skipped", "{retry}");
        assert_eq!(head_commit(&pm), committed_head);
        assert!(!stage_path(&state, &job_id, "note").exists());
        assert_eq!(
            committed_bytes(&pm, "global/box/note.txt").as_deref(),
            Some(bytes.as_slice())
        );
    }

    /// A committed file whose worktree was replaced still needs HEAD, not the dirty bytes.
    #[test]
    fn dirty_worktree_matching_desired_bytes_is_not_committed() {
        let (_root, pm, store, state) = fixture();
        let committed = write(
            &pm,
            &Caller::Operator,
            "global/box/note.txt",
            "nope!!",
            Some(ABSENT),
        )
        .unwrap();
        let rev = committed["rev"].as_str().unwrap().to_string();
        let bytes = b"hello";
        let manifest = text_manifest("note.txt", bytes, &rev);
        let caller = Caller::Operator;
        let owner = "transfer-reconcile";
        let started = begin(&store, &pm, &caller, owner, None, &manifest, 128).unwrap();
        let job_id = started["job_id"].as_str().unwrap().to_string();
        let request = JobRequest {
            caller: &caller,
            owner,
            job_id: &job_id,
            root: "global/box",
        };
        chunk(&store, &state, &request, "note", 0, &STANDARD.encode(bytes)).unwrap();
        let dest = vault_dir(&pm).unwrap().join("global/box/note.txt");
        fs::write(&dest, bytes).unwrap();
        let head = head_commit(&pm);
        let body = apply(&store, &pm, &state, &request, "note").unwrap();
        assert_eq!(body["outcome"], "unresolved", "{body}");
        assert_eq!(head_commit(&pm), head);
        assert_eq!(
            committed_bytes(&pm, "global/box/note.txt").as_deref(),
            Some(b"nope!!".as_slice())
        );
        assert!(stage_path(&state, &job_id, "note").is_file());
    }

    /// An already committed match skips and must not add another wiki commit.
    #[test]
    fn committed_text_retry_skips_without_a_new_commit() {
        let (_root, pm, store, state) = fixture();
        let wrote = write(
            &pm,
            &Caller::Operator,
            "global/box/note.txt",
            "hello",
            Some(ABSENT),
        )
        .unwrap();
        let rev = wrote["rev"].as_str().unwrap();
        let manifest = text_manifest("note.txt", b"hello", rev);
        let head = head_commit(&pm);
        let body = stage_and_apply(&store, &pm, &state, &manifest, "note", b"hello");
        assert_eq!(body["outcome"], "skipped", "{body}");
        assert_eq!(head_commit(&pm), head);
        let job_id = body["job_id"].as_str().unwrap();
        assert!(!stage_path(&state, job_id, "note").exists());
        let caller = Caller::Operator;
        let request = JobRequest {
            caller: &caller,
            owner: "transfer-reconcile",
            job_id,
            root: "global/box",
        };
        let retry = apply(&store, &pm, &state, &request, "note").unwrap();
        assert_eq!(retry["outcome"], "skipped", "{retry}");
        assert_eq!(head_commit(&pm), head);
    }

    #[test]
    fn uncommitted_blob_pointer_is_not_committed_publication() {
        let (_root, pm, store, state) = fixture();
        let bytes = b"blob-bytes";
        let hash = sha256_bytes(bytes);
        let manifest = Manifest {
            version: 1,
            source_id: "src".into(),
            root: "global/box".into(),
            entries: vec![ManifestEntry {
                id: "file".into(),
                path: "file.bin".into(),
                kind: "blob".into(),
                size: bytes.len() as u64,
                sha256: Some(hash.clone()),
                if_rev: ABSENT.into(),
            }],
        };
        let caller = Caller::Operator;
        let owner = "transfer-reconcile";
        let started = begin(&store, &pm, &caller, owner, None, &manifest, 128).unwrap();
        let job_id = started["job_id"].as_str().unwrap().to_string();
        let request = JobRequest {
            caller: &caller,
            owner,
            job_id: &job_id,
            root: "global/box",
        };
        chunk(&store, &state, &request, "file", 0, &STANDARD.encode(bytes)).unwrap();
        let vault = vault_dir(&pm).unwrap();
        let blob = blobs_dir(&vault).join(&hash);
        fs::create_dir_all(blob.parent().unwrap()).unwrap();
        fs::write(&blob, bytes).unwrap();
        let pointer = vault.join("global/box/file.bin.blob");
        fs::create_dir_all(pointer.parent().unwrap()).unwrap();
        fs::write(
            &pointer,
            serde_json::to_string_pretty(&json!({
                "sha256": hash,
                "size": bytes.len(),
                "mime": "application/octet-stream",
                "name": "file.bin",
            }))
            .unwrap(),
        )
        .unwrap();
        let head = head_commit(&pm);
        let body = apply(&store, &pm, &state, &request, "file").unwrap();
        assert_eq!(body["outcome"], "unresolved", "{body}");
        assert_eq!(head_commit(&pm), head);
        assert!(committed_bytes(&pm, "global/box/file.bin.blob").is_none());
        assert!(stage_path(&state, &job_id, "file").is_file());
    }

    #[test]
    fn uncommitted_directory_is_not_skipped() {
        let (_root, pm, store, state) = fixture();
        let manifest = Manifest {
            version: 1,
            source_id: "src".into(),
            root: "global/box".into(),
            entries: vec![ManifestEntry {
                id: "nested".into(),
                path: "nested".into(),
                kind: "dir".into(),
                size: 0,
                sha256: None,
                if_rev: ABSENT.into(),
            }],
        };
        let caller = Caller::Operator;
        let owner = "transfer-reconcile";
        let started = begin(&store, &pm, &caller, owner, None, &manifest, 128).unwrap();
        let job_id = started["job_id"].as_str().unwrap().to_string();
        let request = JobRequest {
            caller: &caller,
            owner,
            job_id: &job_id,
            root: "global/box",
        };
        let dir = vault_dir(&pm).unwrap().join("global/box/nested");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".gitkeep"), b"").unwrap();
        let head = head_commit(&pm);
        let body = apply(&store, &pm, &state, &request, "nested").unwrap();
        assert_eq!(body["outcome"], "unresolved", "{body}");
        assert_eq!(head_commit(&pm), head);
        assert!(committed_bytes(&pm, "global/box/nested/.gitkeep").is_none());
    }

    fn transfer_blob(
        store: &Store,
        pm: &Pm,
        state: &std::path::Path,
        relative: &str,
        entry_id: &str,
        bytes: &[u8],
        if_rev: &str,
    ) -> Value {
        let manifest = Manifest {
            version: 1,
            source_id: "src".into(),
            root: "global/box".into(),
            entries: vec![ManifestEntry {
                id: entry_id.into(),
                path: relative.into(),
                kind: "blob".into(),
                size: bytes.len() as u64,
                sha256: Some(sha256_bytes(bytes)),
                if_rev: if_rev.into(),
            }],
        };
        let caller = Caller::Operator;
        let owner = "transfer-reconcile";
        let started = begin(store, pm, &caller, owner, None, &manifest, 256).unwrap();
        let job_id = started["job_id"].as_str().unwrap().to_string();
        let request = JobRequest {
            caller: &caller,
            owner,
            job_id: &job_id,
            root: "global/box",
        };
        chunk(store, state, &request, entry_id, 0, &STANDARD.encode(bytes)).unwrap();
        apply(store, pm, state, &request, entry_id).unwrap()
    }

    #[test]
    fn foreign_and_manual_sidecar_markers_in_body_are_not_overwritten() {
        let (_root, pm, store, state) = fixture();
        let foreign_path = "global/box/doc.pdf.extracted.md";
        let manual_path = "global/box/notes.pdf.extracted.md";
        let doc_marker = serde_json::to_string("global/box/doc.pdf").unwrap();
        let notes_marker = serde_json::to_string("global/box/notes.pdf").unwrap();
        let foreign = format!(
            "---\nkind: extracted-source\nsource_path: \"global/box/other.pdf\"\nsource_sha256: {}\nextraction_status: indexed\n---\n\n# kept\n\nsource_path: {doc_marker}\nextraction_status: indexed\n",
            "a".repeat(64)
        );
        // Looks generated, but the frontmatter is not a source header. The
        // target marker sits only in the body, which the old body scan accepted.
        let manual = format!(
            "---\nkind: extracted-source\nsource_path: \"global/box/other.pdf\"\nextraction_status: indexed\n---\n\n# kept\n\nsource_path: {notes_marker}\nextraction_status: indexed\n"
        );
        write(&pm, &Caller::Operator, foreign_path, &foreign, Some(ABSENT)).unwrap();
        write(&pm, &Caller::Operator, manual_path, &manual, Some(ABSENT)).unwrap();
        let foreign_before = committed_bytes(&pm, foreign_path);
        let manual_before = committed_bytes(&pm, manual_path);
        let pdf = b"%PDF-1.4\nforeign\n";
        let doc = transfer_blob(&store, &pm, &state, "doc.pdf", "doc", pdf, ABSENT);
        let notes = transfer_blob(&store, &pm, &state, "notes.pdf", "notes", pdf, ABSENT);
        assert_eq!(doc["outcome"], "applied", "{doc}");
        assert_eq!(notes["outcome"], "applied", "{notes}");
        assert_eq!(doc["extraction"], "conflict", "{doc}");
        assert_eq!(notes["extraction"], "conflict", "{notes}");
        assert_eq!(committed_bytes(&pm, foreign_path), foreign_before);
        assert_eq!(committed_bytes(&pm, manual_path), manual_before);
        assert_eq!(
            read(&pm, &Caller::Operator, foreign_path).unwrap()["text"].as_str(),
            Some(foreign.as_str())
        );
        assert_eq!(
            read(&pm, &Caller::Operator, manual_path).unwrap()["text"].as_str(),
            Some(manual.as_str())
        );
        assert!(committed_bytes(&pm, "global/box/doc.pdf.blob").is_some());
        assert!(committed_bytes(&pm, "global/box/notes.pdf.blob").is_some());
    }

    #[test]
    fn same_source_old_sha_reimport_updates_generated_sidecar() {
        let (_root, pm, store, state) = fixture();
        let first_bytes = b"%PDF-1.4\nnot-a-real-pdf-v1\n";
        let second_bytes = b"%PDF-1.4\nnot-a-real-pdf-v2\n";
        let first = transfer_blob(
            &store,
            &pm,
            &state,
            "hello.pdf",
            "hello",
            first_bytes,
            ABSENT,
        );
        assert_eq!(first["outcome"], "applied", "{first}");
        assert_eq!(first["extraction"], "failed", "{first}");
        let pointer_rev = read(&pm, &Caller::Operator, "global/box/hello.pdf").unwrap()["rev"]
            .as_str()
            .unwrap()
            .to_string();
        let sidecar = "global/box/hello.pdf.extracted.md";
        let current = read(&pm, &Caller::Operator, sidecar).unwrap();
        assert_eq!(
            generated_extraction_status(
                current["text"].as_str().unwrap(),
                "global/box/hello.pdf",
                &sha256_bytes(first_bytes)
            ),
            Some("failed")
        );
        let old_sha = "b".repeat(64);
        let stale = generated_extraction_page(
            "global/box/hello.pdf",
            &old_sha,
            "indexed",
            "STALE-BODY\nsource_path: \"global/box/other.pdf\"\nextraction_status: failed\n",
        );
        write(
            &pm,
            &Caller::Operator,
            sidecar,
            &stale,
            current["rev"].as_str(),
        )
        .unwrap();
        let body = transfer_blob(
            &store,
            &pm,
            &state,
            "hello.pdf",
            "hello",
            second_bytes,
            &pointer_rev,
        );
        assert_eq!(body["outcome"], "applied", "{body}");
        assert_eq!(body["extraction"], "failed", "{body}");
        let page = read(&pm, &Caller::Operator, sidecar).unwrap();
        let text = page["text"].as_str().unwrap();
        assert!(!text.contains("STALE-BODY"), "{text}");
        assert_eq!(
            generated_extraction_status(text, "global/box/hello.pdf", &sha256_bytes(second_bytes)),
            Some("failed")
        );
        assert_eq!(
            committed_bytes(&pm, sidecar).as_deref(),
            Some(text.as_bytes())
        );
        let blobs = blobs_dir(&vault_dir(&pm).unwrap());
        assert_eq!(
            fs::read(blobs.join(sha256_bytes(first_bytes))).unwrap(),
            first_bytes
        );
        assert_eq!(
            fs::read(blobs.join(sha256_bytes(second_bytes))).unwrap(),
            second_bytes
        );
    }
}
