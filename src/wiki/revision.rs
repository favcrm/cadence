//! Historical wiki bytes addressed by an explicit selector.
//!
//! `fnv1a:` is the live page's compare-and-swap token. It is not approval
//! provenance. A selector of that form is resolved only inside the existing
//! history window, and only when every match has the same source bytes.
//! Differing bytes are refused rather than answered with the newest commit.
//! A full Git commit name is loaded from that commit alone.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::SystemTime;

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{allowed, git_rel, normalize, resolve, vault_dir, Caller, Op, HISTORY_CAP, TEXT_CAP};
use crate::error::{Error, Result};
use crate::issue::Pm;

pub(crate) struct Pinned {
    pub path: String,
    pub git: String,
    pub rev: String,
    pub sha256: String,
    pub body: Body,
}

pub(crate) enum Body {
    Text(String),
    Blob(BlobBody),
    Dir,
}

pub(crate) struct BlobBody {
    pub sha256: String,
    pub size: u64,
    pub mime: String,
    pub name: String,
}

struct Hit {
    git: String,
    rev: String,
    sha256: String,
    body: Body,
}

/// Resolve `at` to one historical object. Never reads the worktree as a
/// stand-in for a miss, an ambiguous FNV match, or a bad name.
pub(crate) fn pin(pm: &Pm, caller: &Caller, path: &str, at: &str) -> Result<Pinned> {
    let pinned = resolve_pin(pm, caller, path, at)?;
    if let Body::Blob(blob) = &pinned.body {
        let object = super::blobs_dir(&vault_dir(pm)?).join(&blob.sha256);
        let actual = super::transfer::sha256_file(&object)
            .map_err(|_| unavailable("historical blob bytes are unavailable"))?;
        if actual != blob.sha256 {
            return Err(unavailable(
                "historical blob bytes do not match the pointer digest",
            ));
        }
    }
    Ok(pinned)
}

fn resolve_pin(pm: &Pm, caller: &Caller, path: &str, at: &str) -> Result<Pinned> {
    let norm = normalize(path)?;
    if norm.is_empty() {
        return Err(unavailable("the wiki root has no historical file"));
    }
    let segs: Vec<&str> = norm.split('/').collect();
    allowed(caller, Op::Read, &segs)?;
    if !matches!(super::classify(&segs), super::View::Real) {
        return Err(unavailable(
            "profile and memory views have no content-addressed original",
        ));
    }
    let vault = vault_dir(pm)?;
    let file_rel = git_rel(pm, &resolve(&vault, &norm)?)?;
    let pointer_rel = git_rel(pm, &vault.join(format!("{norm}.blob")))?;
    let hits = if let Some(git) = commit_name(at) {
        match load_commit(pm, &file_rel, &pointer_rel, &git)? {
            Some(hit) => vec![hit],
            None => {
                return Err(unavailable(
                    "that commit does not contain this path's original",
                ))
            }
        }
    } else if fnv_name(at) {
        fnv_hits(pm, &file_rel, &pointer_rel, at)?
    } else {
        return Err(unavailable(
            "revision selector must be a 40-digit commit or an fnv1a: content token",
        ));
    };
    let Some(first) = hits.first() else {
        return Err(unavailable(
            "that content token is not inside the retained history of this path",
        ));
    };
    if hits.iter().any(|hit| hit.sha256 != first.sha256) {
        return Err(unavailable(
            "that content token matches more than one source digest; refusing to choose one",
        ));
    }
    // Same digest: keep the oldest retained commit, never the newest alias.
    // Only that hit's object is checked. An older commit that does not match
    // the selector must not abort resolution when its blob is missing.
    let hit = hits.into_iter().last().expect("hit");
    if let Body::Blob(blob) = &hit.body {
        confirm_selected_blob(&vault, blob)?;
    }
    Ok(Pinned {
        path: norm,
        git: hit.git,
        rev: hit.rev,
        sha256: hit.sha256,
        body: hit.body,
    })
}

/// One historical original resolved for memory evidence.
pub(crate) struct EvidenceText {
    pub path: String,
    pub git: String,
    pub sha256: String,
    pub body: EvidenceBody,
}

pub(crate) enum EvidenceBody {
    Text(String),
    Blob {
        size: u64,
        mime: String,
        name: String,
        data_base64: String,
    },
}

/// Largest accepted `.blob` pointer JSON (managed sidecars are a few hundred
/// bytes; the cap only makes this reader's I/O provably bounded).
const EVIDENCE_POINTER_CAP: u64 = 64 * 1024;

/// Historical original bytes for memory evidence, bounded before I/O.
///
/// Same path normalization, ACL and Git source as [`resolve_pin`], with these
/// restrictions. The selector must be an explicit 40-hex commit: resolving an
/// `fnv1a:` token inspects up to [`HISTORY_CAP`] historical bodies, so it
/// cannot promise a single bounded read, and the caller is told so rather than
/// handed a substituted revision.
///
/// A text page is refused from its Git blob size alone when that size exceeds
/// `max_bytes` (or [`TEXT_CAP`]) — its content is never read. A blob page is
/// refused from the pointer's recorded size when that size exceeds
/// `max_bytes` — its content-addressed object is never opened or read.
/// Within the bound, a blob is opened once under the store's custody checks,
/// at most `size + 1` bytes are read, the length must equal the pointer's
/// size, and the digest is computed over exactly the returned buffer.
///
/// `Ok(None)` means the pinned commit itself was read and demonstrably holds
/// neither a text page nor a blob pointer at this path — the definite-absence
/// answer. An unknown or non-commit selector, an ACL or view refusal, a
/// directory, an over-limit original, and any custody failure stay `Err`.
/// A miss never falls back to the live page.
pub(crate) fn evidence_text(
    pm: &Pm,
    caller: &Caller,
    path: &str,
    at: &str,
    max_bytes: usize,
) -> Result<Option<EvidenceText>> {
    let norm = normalize(path)?;
    if norm.is_empty() {
        return Err(unavailable("the wiki root has no historical file"));
    }
    let segs: Vec<&str> = norm.split('/').collect();
    allowed(caller, Op::Read, &segs)?;
    if !matches!(super::classify(&segs), super::View::Real) {
        return Err(unavailable(
            "profile and memory views have no content-addressed original",
        ));
    }
    let Some(git) = commit_name(at) else {
        return Err(Error::rejected(format!(
            "wiki evidence revision '{at}' refused: the bounded reader accepts only a \
             lowercase 40-hex commit selector — an fnv1a: content token or other name is \
             never resolved to a commit for evidence"
        )));
    };
    let vault = vault_dir(pm)?;
    let file_rel = git_rel(pm, &resolve(&vault, &norm)?)?;
    let pointer_rel = git_rel(pm, &vault.join(format!("{norm}.blob")))?;
    if object_type(pm, &git)? != "commit" {
        return Err(unavailable("revision selector is not a commit"));
    }
    let Some((blob, kind)) = tree_entry(pm, &git, &file_rel)? else {
        let Some((pointer_blob, pointer_kind)) = tree_entry(pm, &git, &pointer_rel)? else {
            // The commit was read and has neither entry: the path is
            // definitely absent at this revision, not merely unreadable.
            return Ok(None);
        };
        if pointer_kind != "blob" {
            return Err(unavailable("that path is not a readable source"));
        }
        return blob_evidence(pm, &vault, &norm, &git, &pointer_blob, max_bytes).map(Some);
    };
    if kind == "tree" {
        return Err(unavailable("a directory has no original text"));
    }
    if kind != "blob" {
        return Err(unavailable("that path is not a readable source"));
    }
    let size = blob_size(pm, &blob)?;
    if size > max_bytes as u64 {
        return Err(Error::rejected(format!(
            "wiki evidence revision '{path}' at {git} refused: the original is {size} \
             bytes, above the {max_bytes}-byte read bound"
        )));
    }
    if size > TEXT_CAP {
        return Err(unavailable("historical text is over the read cap"));
    }
    let bytes = cat_blob(pm, &blob)?;
    if bytes.len() as u64 != size {
        return Err(unavailable(
            "historical text size changed between its size probe and its read",
        ));
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        unavailable("historical original is not UTF-8 text and has no blob pointer")
    })?;
    Ok(Some(EvidenceText {
        path: norm,
        git,
        sha256: super::transfer::sha256_bytes(text.as_bytes()),
        body: EvidenceBody::Text(text),
    }))
}

/// A historical binary Wiki page within the bound: verify the pointer, then
/// the object's length, digest and bytes. The pointer's size is checked
/// against `max_bytes` before the object is opened. The object is then read
/// from the one descriptor opened under the store's custody checks, at most
/// `size + 1` bytes, and the digest is computed over exactly the buffer that
/// is returned — never over a separate pass that a concurrent overwrite could
/// invalidate.
fn blob_evidence(
    pm: &Pm,
    vault: &Path,
    path: &str,
    git: &str,
    pointer_blob: &str,
    max_bytes: usize,
) -> Result<EvidenceText> {
    let pointer_size = blob_size(pm, pointer_blob)?;
    if pointer_size > EVIDENCE_POINTER_CAP {
        return Err(unavailable("historical blob pointer is over the read cap"));
    }
    let bytes = cat_blob(pm, pointer_blob)?;
    if bytes.len() as u64 != pointer_size {
        return Err(unavailable(
            "historical blob pointer size changed between its size probe and its read",
        ));
    }
    let pointer: Value = serde_json::from_slice(&bytes)
        .map_err(|_| unavailable("historical blob pointer is not the stored JSON"))?;
    let sha = pointer_sha(&pointer)?;
    let size = pointer
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| unavailable("historical blob pointer has no size"))?;
    if size > max_bytes as u64 {
        return Err(Error::rejected(format!(
            "wiki evidence revision '{path}' at {git} refused: the blob original is {size} \
             bytes, above the {max_bytes}-byte read bound"
        )));
    }
    let blob = BlobBody {
        sha256: sha.clone(),
        size,
        mime: json_string(&pointer, "mime"),
        name: json_string(&pointer, "name"),
    };
    // Same symlink-refusing custody open as the regular revision path, but no
    // hash-to-EOF: the descriptor is read once, capped at one byte past the
    // expected length so an object extended after the length check is caught
    // without ever reading beyond `max_bytes + 1`.
    let (_object, file, _meta) = open_blob_object(vault, &blob)?;
    let len = usize::try_from(size)
        .map_err(|_| unavailable("historical blob is too large to read on this platform"))?;
    let cap = len.saturating_add(1);
    let mut data = Vec::with_capacity(cap);
    file.take(cap as u64)
        .read_to_end(&mut data)
        .map_err(|_| unavailable("historical blob bytes could not be read"))?;
    if data.len() != len {
        return Err(unavailable(
            "historical blob length does not match the pointer",
        ));
    }
    // Hash the returned buffer itself: the bytes that pass this check are the
    // bytes handed to the caller, so a concurrent same-length overwrite cannot
    // slip between verification and return.
    if super::transfer::sha256_bytes(&data) != sha {
        return Err(unavailable(
            "historical blob bytes do not match the pointer digest",
        ));
    }
    Ok(EvidenceText {
        path: path.to_string(),
        git: git.to_string(),
        sha256: sha,
        body: EvidenceBody::Blob {
            size,
            mime: blob.mime,
            name: blob.name,
            data_base64: base64::engine::general_purpose::STANDARD.encode(&data),
        },
    })
}

/// One historical original slice. `rev` is the requested selector, not the
/// live page token and not a newer commit that happens to share an FNV tag.
pub(crate) fn export_slice(
    pm: &Pm,
    caller: &Caller,
    path: &str,
    at: &str,
    offset: u64,
    want: u64,
) -> Result<Value> {
    // A repeated slice reuses the stamp for this blob's identity. The whole
    // object is hashed only when that stamp misses.
    let pinned = resolve_pin(pm, caller, path, at)?;
    match pinned.body {
        Body::Dir => Err(unavailable("a directory has no original byte slice")),
        Body::Text(text) => {
            let bytes = text.into_bytes();
            slice_of(
                Original {
                    path: &pinned.path,
                    kind: "text",
                    at,
                    sha256: &pinned.sha256,
                },
                &bytes,
                offset,
                want,
            )
        }
        Body::Blob(blob) => {
            let mut file = open_historical_blob(&vault_dir(pm)?, &blob)?;
            bounded_slice(
                Original {
                    path: &pinned.path,
                    kind: "blob",
                    at,
                    sha256: &blob.sha256,
                },
                blob.size,
                offset,
                want,
                |start, len| {
                    file.seek(SeekFrom::Start(start))
                        .map_err(|_| unavailable("historical blob bytes could not be read"))?;
                    let mut buf = vec![0u8; len];
                    file.read_exact(&mut buf)
                        .map_err(|_| unavailable("historical blob bytes could not be read"))?;
                    Ok(buf)
                },
            )
        }
    }
}

/// On-disk custody for the single hit [`resolve_pin`] kept.
fn confirm_selected_blob(vault: &Path, blob: &BlobBody) -> Result<()> {
    let object = super::blobs_dir(vault).join(&blob.sha256);
    let metadata = object
        .symlink_metadata()
        .map_err(|_| unavailable("historical blob bytes are not in the content-addressed store"))?;
    if !metadata.is_file() {
        return Err(unavailable("historical blob object is not a regular file"));
    }
    if metadata.len() != blob.size {
        return Err(unavailable(
            "historical blob size does not match the pointer",
        ));
    }
    Ok(())
}

const BLOB_STAMP_CAP: usize = 32;

struct BlobStamp {
    path: PathBuf,
    dev: u64,
    ino: u64,
    len: u64,
    mtime: SystemTime,
    sha256: String,
}

static BLOB_STAMPS: Mutex<Vec<BlobStamp>> = Mutex::new(Vec::new());

fn stamp_lock() -> std::sync::MutexGuard<'static, Vec<BlobStamp>> {
    BLOB_STAMPS.lock().unwrap_or_else(|err| err.into_inner())
}

fn stamp_matches(path: &Path, meta: &std::fs::Metadata, expected: &str) -> bool {
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    stamp_lock().iter().any(|stamp| {
        stamp.path == path
            && stamp.dev == meta.dev()
            && stamp.ino == meta.ino()
            && stamp.len == meta.len()
            && stamp.mtime == mtime
            && stamp.sha256 == expected
    })
}

fn remember_stamp(path: &Path, meta: &std::fs::Metadata, sha256: &str) {
    let Ok(mtime) = meta.modified() else {
        return;
    };
    let mut stamps = stamp_lock();
    if let Some(stamp) = stamps.iter_mut().find(|stamp| stamp.path == path) {
        stamp.dev = meta.dev();
        stamp.ino = meta.ino();
        stamp.len = meta.len();
        stamp.mtime = mtime;
        stamp.sha256 = sha256.to_string();
        return;
    }
    if stamps.len() == BLOB_STAMP_CAP {
        stamps.remove(0);
    }
    stamps.push(BlobStamp {
        path: path.to_path_buf(),
        dev: meta.dev(),
        ino: meta.ino(),
        len: meta.len(),
        mtime,
        sha256: sha256.to_string(),
    });
}

fn sha256_open(file: &mut File) -> Result<String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|_| unavailable("historical blob bytes could not be read"))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|_| unavailable("historical blob bytes could not be read"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn whole_blob_matches(
    path: &Path,
    file: &mut File,
    meta: &std::fs::Metadata,
    expected: &str,
) -> Result<()> {
    if stamp_matches(path, meta, expected) {
        return Ok(());
    }
    let actual = sha256_open(file)?;
    if actual != expected {
        return Err(unavailable(
            "historical blob bytes do not match the pointer digest",
        ));
    }
    remember_stamp(path, meta, expected);
    Ok(())
}

/// Open a published blob object with the store's custody checks and no read of
/// its bytes: `.blobs/` must be a real directory, the name must be a regular
/// non-symlink file, and the file's own length must equal the pointer's
/// recorded size. Hashing stays with the caller, so a bounded reader can cap
/// its own read instead of hashing to EOF.
fn open_blob_object(vault: &Path, blob: &BlobBody) -> Result<(PathBuf, File, std::fs::Metadata)> {
    let blobs = super::blobs_dir(vault);
    match blobs.symlink_metadata() {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err(unavailable("historical blob store is not a real directory")),
        Err(_) => {
            return Err(unavailable(
                "historical blob bytes are not in the content-addressed store",
            ))
        }
    }
    let object = blobs.join(&blob.sha256);
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&object)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                unavailable("historical blob object is not a regular file")
            } else {
                unavailable("historical blob bytes are not in the content-addressed store")
            }
        })?;
    let meta = file
        .metadata()
        .map_err(|_| unavailable("historical blob bytes could not be read"))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(unavailable("historical blob object is not a regular file"));
    }
    if meta.len() != blob.size {
        return Err(unavailable(
            "historical blob size does not match the pointer",
        ));
    }
    Ok((object, file, meta))
}

fn open_historical_blob(vault: &Path, blob: &BlobBody) -> Result<File> {
    let (object, mut file, meta) = open_blob_object(vault, blob)?;
    whole_blob_matches(&object, &mut file, &meta, &blob.sha256)?;
    Ok(file)
}

/// The historical original one slice belongs to: its path, its kind, the
/// requested selector and the verified digest of the whole original.
struct Original<'a> {
    path: &'a str,
    kind: &'a str,
    at: &'a str,
    sha256: &'a str,
}

fn slice_of(original: Original<'_>, bytes: &[u8], offset: u64, want: u64) -> Result<Value> {
    let size = bytes.len() as u64;
    bounded_slice(original, size, offset, want, |start, len| {
        let start = usize::try_from(start)
            .map_err(|_| unavailable("offset is past the historical original"))?;
        Ok(bytes[start..start + len].to_vec())
    })
}

fn bounded_slice(
    original: Original<'_>,
    size: u64,
    offset: u64,
    want: u64,
    read: impl FnOnce(u64, usize) -> Result<Vec<u8>>,
) -> Result<Value> {
    if offset > size {
        return Err(unavailable("offset is past the historical original"));
    }
    let end = offset.saturating_add(want).min(size);
    let len =
        usize::try_from(end - offset).map_err(|_| unavailable("historical slice is too large"))?;
    let data = read(offset, len)?;
    if data.len() != len {
        return Err(unavailable("historical original slice was short"));
    }
    let next_offset = (end < size).then_some(end);
    Ok(json!({
        "path": original.path,
        "kind": original.kind,
        "rev": original.at,
        "size": size,
        "sha256": original.sha256,
        "offset": offset,
        "data_base64": base64::engine::general_purpose::STANDARD.encode(&data),
        "next_offset": next_offset,
    }))
}

fn fnv_hits(pm: &Pm, file_rel: &Path, pointer_rel: &Path, token: &str) -> Result<Vec<Hit>> {
    let limit = format!("-{}", HISTORY_CAP);
    let out = git_text(
        &pm.dir,
        &[
            "log",
            &limit,
            "--format=%H",
            "--",
            &file_rel.to_string_lossy(),
            &pointer_rel.to_string_lossy(),
        ],
    )?;
    let mut commits = Vec::new();
    for line in out.lines() {
        let Some(git) = commit_name(line.trim()) else {
            continue;
        };
        commits.push(git);
    }
    // One body at a time: each match replaces the previous candidate, so a
    // repeated selector across the whole history window never keeps several
    // source bodies resident for a one-byte response.
    let mut hit: Option<Hit> = None;
    let mut digest: Option<String> = None;
    for git in commits {
        let Some(candidate) = load_commit(pm, file_rel, pointer_rel, &git)? else {
            continue;
        };
        if candidate.rev != token {
            continue;
        }
        match &digest {
            None => digest = Some(candidate.sha256.clone()),
            Some(seen) if seen != &candidate.sha256 => {
                return Err(unavailable(
                    "that content token matches more than one source digest; refusing to choose one",
                ));
            }
            Some(_) => {}
        }
        // Keep the oldest retained match, one body at a time.
        hit = Some(candidate);
    }
    Ok(hit.into_iter().collect())
}

fn load_commit(pm: &Pm, file_rel: &Path, pointer_rel: &Path, git: &str) -> Result<Option<Hit>> {
    if object_type(pm, git)? != "commit" {
        return Err(unavailable("revision selector is not a commit"));
    }
    if let Some((blob, kind)) = tree_entry(pm, git, file_rel)? {
        if kind == "tree" {
            return Ok(Some(Hit {
                git: git.to_string(),
                rev: String::new(),
                sha256: String::new(),
                body: Body::Dir,
            }));
        }
        if kind != "blob" {
            return Ok(None);
        }
        let bytes = cat_blob(pm, &blob)?;
        if bytes.len() as u64 > TEXT_CAP {
            return Err(unavailable("historical text is over the read cap"));
        }
        let text = String::from_utf8(bytes).map_err(|_| {
            unavailable("historical original is not UTF-8 text and has no blob pointer")
        })?;
        return Ok(Some(Hit {
            git: git.to_string(),
            rev: super::rev_bytes(text.as_bytes()),
            sha256: super::transfer::sha256_bytes(text.as_bytes()),
            body: Body::Text(text),
        }));
    }
    let Some((blob, kind)) = tree_entry(pm, git, pointer_rel)? else {
        return Ok(None);
    };
    if kind != "blob" {
        return Ok(None);
    }
    let bytes = cat_blob(pm, &blob)?;
    let pointer: Value = serde_json::from_slice(&bytes)
        .map_err(|_| unavailable("historical blob pointer is not the stored JSON"))?;
    let sha = pointer_sha(&pointer)?;
    let size = pointer
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| unavailable("historical blob pointer has no size"))?;
    Ok(Some(Hit {
        git: git.to_string(),
        rev: super::rev_bytes(&bytes),
        sha256: sha.clone(),
        body: Body::Blob(BlobBody {
            sha256: sha,
            size,
            mime: json_string(&pointer, "mime"),
            name: json_string(&pointer, "name"),
        }),
    }))
}

fn pointer_sha(pointer: &Value) -> Result<String> {
    let sha = pointer.get("sha256").and_then(Value::as_str).unwrap_or("");
    if sha.len() == 64
        && sha
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        Ok(sha.to_string())
    } else {
        Err(unavailable(
            "historical blob pointer digest is not lowercase sha256",
        ))
    }
}

fn json_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn tree_entry(pm: &Pm, git: &str, rel: &Path) -> Result<Option<(String, String)>> {
    let out = git_bytes(
        &pm.dir,
        &["ls-tree", "-z", git, "--", &rel.to_string_lossy()],
    )?;
    if out.is_empty() {
        return Ok(None);
    }
    let record = out.split(|b| *b == 0).next().unwrap_or(&out);
    let text = std::str::from_utf8(record).map_err(|_| unavailable("git ls-tree was not UTF-8"))?;
    let (meta, _) = text
        .split_once('\t')
        .ok_or_else(|| unavailable("git ls-tree record is malformed"))?;
    let mut parts = meta.split_whitespace();
    let _mode = parts.next();
    let kind = parts.next().unwrap_or("");
    let blob = parts.next().unwrap_or("");
    if !commit_name(blob).is_some() {
        return Err(unavailable("git ls-tree object name is malformed"));
    }
    Ok(Some((blob.to_string(), kind.to_string())))
}

/// Size of a named Git object, without materializing its content — the
/// pre-read probe that lets a bounded reader refuse an over-limit blob.
fn blob_size(pm: &Pm, blob: &str) -> Result<u64> {
    if commit_name(blob).is_none() {
        return Err(unavailable("blob name is malformed"));
    }
    let out = git_text(&pm.dir, &["cat-file", "-s", blob])?;
    out.trim()
        .parse::<u64>()
        .map_err(|_| unavailable("git did not report a numeric historical blob size"))
}

fn cat_blob(pm: &Pm, blob: &str) -> Result<Vec<u8>> {
    if commit_name(blob).is_none() {
        return Err(unavailable("blob name is malformed"));
    }
    git_bytes(&pm.dir, &["cat-file", "blob", blob])
}

fn object_type(pm: &Pm, git: &str) -> Result<String> {
    let out = git_text(&pm.dir, &["cat-file", "-t", git])?;
    Ok(out.trim().to_string())
}

fn commit_name(value: &str) -> Option<String> {
    if value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        Some(value.to_string())
    } else {
        None
    }
}

fn fnv_name(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("fnv1a:") else {
        return false;
    };
    hex.len() == 16
        && hex
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String> {
    let bytes = git_bytes(dir, args)?;
    String::from_utf8(bytes).map_err(|_| unavailable("git output was not UTF-8"))
}

fn git_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = crate::reaper::output(
        Command::new("git")
            .args(["--no-replace-objects", "-C"])
            .arg(dir)
            .args(args),
    )
    .map_err(|_| Error::rejected("git is required and was not found on PATH"))?;
    if !out.status.success() {
        return Err(unavailable(&format!(
            "git could not read that revision ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

fn unavailable(why: &str) -> Error {
    Error::rejected(format!("wiki revision unavailable: {why}"))
}

/// Live source digest of a real page. Text is the SHA-256 of its bytes.
/// A blob is the pointer's content digest, checked against the object.
pub(crate) fn live_source(pm: &Pm, caller: &Caller, path: &str) -> Result<LiveSource> {
    let page = super::read(pm, caller, path)?;
    match page["kind"].as_str() {
        Some("text") => {
            let sha = page["sha256"]
                .as_str()
                .ok_or_else(|| unavailable("live text has no source digest"))?;
            Ok(LiveSource {
                sha256: sha.to_string(),
            })
        }
        Some("blob") => {
            let sha = page["sha256"].as_str().unwrap_or("");
            if sha.len() != 64 {
                return Err(unavailable("live blob pointer has no digest"));
            }
            let object = super::blobs_dir(&vault_dir(pm)?).join(sha);
            let actual = super::transfer::sha256_file(&object)
                .map_err(|_| unavailable("live blob bytes are unavailable"))?;
            if actual != sha {
                return Err(unavailable(
                    "live blob bytes do not match the pointer digest",
                ));
            }
            Ok(LiveSource {
                sha256: sha.to_string(),
            })
        }
        Some("dir") => Err(Error::rejected(
            "wiki revision refused: a directory has no source digest",
        )),
        _ => Err(unavailable("that path is not a readable source")),
    }
}

pub(crate) struct LiveSource {
    pub sha256: String,
}
