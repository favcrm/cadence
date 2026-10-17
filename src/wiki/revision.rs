//! Historical wiki bytes addressed by an explicit selector.
//!
//! `fnv1a:` is the live page's compare-and-swap token. It is not approval
//! provenance. A selector of that form is resolved only inside the existing
//! history window, and only when every match has the same source bytes.
//! Differing bytes are refused rather than answered with the newest commit.
//! A full Git commit name is loaded from that commit alone.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::Command;

use base64::Engine;
use serde_json::{json, Value};

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
        match load_commit(pm, &vault, &file_rel, &pointer_rel, &git)? {
            Some(hit) => vec![hit],
            None => {
                return Err(unavailable(
                    "that commit does not contain this path's original",
                ))
            }
        }
    } else if fnv_name(at) {
        fnv_hits(pm, &vault, &file_rel, &pointer_rel, at)?
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
    let hit = hits.into_iter().last().expect("hit");
    Ok(Pinned {
        path: norm,
        git: hit.git,
        rev: hit.rev,
        sha256: hit.sha256,
        body: hit.body,
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
    // Objects are immutable and content-addressed. Check the bounded slice and
    // carry its strong digest; do not hash a whole 100MiB object per 16KiB slice.
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
            let object = super::blobs_dir(&vault_dir(pm)?).join(&blob.sha256);
            let mut file = std::fs::File::open(&object).map_err(|_| {
                unavailable("historical blob bytes are not in the content-addressed store")
            })?;
            let on_disk = file
                .metadata()
                .map(|meta| meta.len())
                .map_err(|_| unavailable("historical blob bytes could not be read"))?;
            if on_disk != blob.size {
                return Err(unavailable(
                    "historical blob size does not match the pointer",
                ));
            }
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

fn fnv_hits(
    pm: &Pm,
    vault: &Path,
    file_rel: &Path,
    pointer_rel: &Path,
    token: &str,
) -> Result<Vec<Hit>> {
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
        let Some(candidate) = load_commit(pm, vault, file_rel, pointer_rel, &git)? else {
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

fn load_commit(
    pm: &Pm,
    vault: &Path,
    file_rel: &Path,
    pointer_rel: &Path,
    git: &str,
) -> Result<Option<Hit>> {
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
    let object = super::blobs_dir(vault).join(&sha);
    let metadata = object
        .symlink_metadata()
        .map_err(|_| unavailable("historical blob bytes are not in the content-addressed store"))?;
    if !metadata.is_file() {
        return Err(unavailable("historical blob object is not a regular file"));
    }
    let size = pointer
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| unavailable("historical blob pointer has no size"))?;
    if metadata.len() != size {
        return Err(unavailable(
            "historical blob size does not match the pointer",
        ));
    }
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
    let out = crate::reaper::output(Command::new("git").arg("-C").arg(dir).args(args))
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
