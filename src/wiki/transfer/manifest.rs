//! Manifest canonicalization, digesting, admission, and path/revision checks.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::Path;

use sha2::{Digest, Sha256};

use super::super::{allowed, normalize, read, vault_dir, Caller, Op, ABSENT};
use super::{
    missing, page_matches, reject, Manifest, MANIFEST_VERSION, MAX_ENTRIES, MAX_FILE_BYTES,
    MAX_ID_LEN, MAX_MANIFEST_BYTES, MAX_PATH_LEN, MAX_SOURCE_ID_LEN, MAX_TOTAL_BYTES,
};
use crate::error::Result;
use crate::issue::Pm;
use crate::store::wiki_transfers::EntryRow;

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

fn id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub(super) fn sha_ok(value: &str) -> bool {
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

pub(super) fn full_path(root: &str, relative: &str) -> String {
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
    let root = normalize(&manifest.root)?;
    if root.is_empty() || root.len() > MAX_PATH_LEN || root != manifest.root {
        return Err(reject(
            "wiki transfer refused: root must be a nonempty canonical directory path",
        ));
    }
    let root_segs: Vec<&str> = root.split('/').collect();
    allowed(pm, caller, Op::Write, &root_segs)?;
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
        let relative = normalize(&entry.path)?;
        if relative.is_empty() || relative.len() > MAX_PATH_LEN || relative != entry.path {
            return Err(reject(
                "wiki transfer refused: entry path must be a nonempty canonical relative path",
            ));
        }
        entry.path = relative.clone();
        let kind = kind_for(&relative, &entry.kind)?;
        entry.kind = kind.to_string();
        let full = full_path(&root, &relative);
        normalize(&full)?;
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
        allowed(pm, caller, Op::Write, &segs)?;
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
pub(super) fn pin_live(pm: &Pm, caller: &Caller, manifest: &Manifest) -> Result<()> {
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

pub(super) fn refuse_special(vault: &Path, norm: &str, dir_ok: bool) -> Result<()> {
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

pub(super) fn recheck_job_paths(
    pm: &Pm,
    caller: &Caller,
    root: &str,
    entries: &[EntryRow],
    writing: bool,
) -> Result<()> {
    let root_segments: Vec<&str> = root.split('/').collect();
    allowed(
        pm,
        caller,
        if writing { Op::Write } else { Op::Read },
        &root_segments,
    )?;
    for entry in entries {
        let segments: Vec<&str> = entry.path.split('/').collect();
        allowed(
            pm,
            caller,
            if writing { Op::Write } else { Op::Read },
            &segments,
        )?;
    }
    Ok(())
}
