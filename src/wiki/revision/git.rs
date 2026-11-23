//! Historical Git commit lookup, tree inspection, and blob reading helpers for wiki revisions.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use super::super::{HISTORY_CAP, TEXT_CAP};
use super::{BlobBody, Body, Hit};
use crate::error::{Error, Result};
use crate::issue::Pm;

pub(super) fn fnv_hits(
    pm: &Pm,
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

pub(super) fn load_commit(
    pm: &Pm,
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
            rev: super::super::rev_bytes(text.as_bytes()),
            sha256: super::super::transfer::sha256_bytes(text.as_bytes()),
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
        rev: super::super::rev_bytes(&bytes),
        sha256: sha.clone(),
        body: Body::Blob(BlobBody {
            sha256: sha,
            size,
            mime: json_string(&pointer, "mime"),
            name: json_string(&pointer, "name"),
        }),
    }))
}

pub(super) fn pointer_sha(pointer: &Value) -> Result<String> {
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

pub(super) fn json_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(super) fn tree_entry(pm: &Pm, git: &str, rel: &Path) -> Result<Option<(String, String)>> {
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
pub(super) fn blob_size(pm: &Pm, blob: &str) -> Result<u64> {
    if commit_name(blob).is_none() {
        return Err(unavailable("blob name is malformed"));
    }
    let out = git_text(&pm.dir, &["cat-file", "-s", blob])?;
    out.trim()
        .parse::<u64>()
        .map_err(|_| unavailable("git did not report a numeric historical blob size"))
}

pub(super) fn cat_blob(pm: &Pm, blob: &str) -> Result<Vec<u8>> {
    if commit_name(blob).is_none() {
        return Err(unavailable("blob name is malformed"));
    }
    git_bytes(&pm.dir, &["cat-file", "blob", blob])
}

pub(super) fn object_type(pm: &Pm, git: &str) -> Result<String> {
    let out = git_text(&pm.dir, &["cat-file", "-t", git])?;
    Ok(out.trim().to_string())
}

pub(super) fn commit_name(value: &str) -> Option<String> {
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

pub(super) fn fnv_name(value: &str) -> bool {
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

pub(super) fn unavailable(why: &str) -> Error {
    Error::rejected(format!("wiki revision unavailable: {why}"))
}
