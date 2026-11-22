//! Local filesystem walk, manifest validation, and wiki path helpers for `wiki_transfer`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::io::Read;
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::fs_guard::{
    contained_source_meta, hash_open_file, lexical_abs, list_nofollow_dir, open_contained,
    path_utf8, refused_path, symlink_meta,
};
use super::{
    ExportEntry, FILE_CAP, ID_CAP, MANIFEST_BYTES, MAX_ENTRIES, PATH_CAP, SOURCE_CAP, TEXT_CAP,
    TOTAL_CAP,
};
use crate::error::{Error, Result};
use crate::wiki::transfer::Manifest;

pub(super) struct Walked {
    pub(super) rel: String,
    pub(super) kind: String,
    pub(super) size: u64,
    pub(super) sha256: Option<String>,
    pub(super) single_file: bool,
}

pub(super) fn walk_local(root: &Path, file_name: Option<&str>) -> Result<Vec<Walked>> {
    let mut out = Vec::new();
    if let Some(name) = file_name {
        push_file(&mut out, root, name, true)?;
    } else {
        walk_dir(root, Path::new(""), &mut out)?;
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    if out.len() > MAX_ENTRIES {
        return Err(Error::rejected(
            "wiki cp refused: more than 256 entries — nothing was sent",
        ));
    }
    let mut total = 0u64;
    for item in &out {
        total = total.saturating_add(item.size);
    }
    if total > TOTAL_CAP {
        return Err(Error::rejected(
            "wiki cp refused: the tree exceeds 256 MiB — nothing was sent",
        ));
    }
    Ok(out)
}

fn walk_dir(root: &Path, rel: &Path, out: &mut Vec<Walked>) -> Result<()> {
    let full = if rel.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    // Open with O_NOFOLLOW on every component, then list that fd. A path
    // listing would follow an ancestor swapped for a symlink after the check.
    let opened = open_contained(&full)?;
    let meta = opened
        .metadata()
        .map_err(|e| Error::rejected(format!("wiki cp cannot stat {}: {e}", full.display())))?;
    if meta.file_type().is_symlink() {
        return Err(refused_path(&full, "symlink"));
    }
    if !meta.is_dir() {
        return Err(refused_path(&full, "not a directory"));
    }
    if !rel.as_os_str().is_empty() {
        let rel_s = path_utf8(rel)?;
        out.push(Walked {
            rel: rel_s,
            kind: "dir".to_string(),
            size: 0,
            sha256: None,
            single_file: false,
        });
    }
    let mut names = list_nofollow_dir(opened, &full)?;
    names.sort();
    for name in names {
        let child = rel.join(name);
        let abs = root.join(&child);
        let meta = contained_source_meta(&abs)?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            return Err(refused_path(&abs, "symlink"));
        } else if ft.is_dir() {
            walk_dir(root, &child, out)?;
        } else if ft.is_file() {
            push_file(out, &abs, &path_utf8(&child)?, false)?;
        } else {
            return Err(refused_path(&abs, "not a regular file or directory"));
        }
    }
    Ok(())
}

pub(super) fn push_file(
    out: &mut Vec<Walked>,
    path: &Path,
    rel: &str,
    single_file: bool,
) -> Result<()> {
    if rel.is_empty()
        || rel.starts_with('.')
        || rel.split('/').any(|s| s.is_empty() || s.starts_with('.'))
    {
        return Err(Error::rejected(format!(
            "wiki cp refused internal path '{rel}' — nothing was sent"
        )));
    }
    // Hash and UTF-8 checks read this fd. A second path open would follow
    // an ancestor symlink planted after admission.
    let mut file = open_contained(path)?;
    let meta = file
        .metadata()
        .map_err(|e| Error::rejected(format!("wiki cp cannot stat {}: {e}", path.display())))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(refused_path(path, "not a regular file"));
    }
    let kind = kind_for_name(rel);
    let cap = if kind == "text" { TEXT_CAP } else { FILE_CAP };
    if meta.len() > cap {
        return Err(Error::rejected(format!(
            "wiki cp refused '{}': {} bytes exceeds the {}-byte cap — nothing was sent",
            rel,
            meta.len(),
            cap
        )));
    }
    let (size, sha) = if kind == "text" {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| Error::rejected(format!("wiki cp cannot read {}: {e}", path.display())))?;
        if bytes.len() as u64 > cap {
            return Err(Error::rejected(format!(
                "wiki cp refused '{rel}': {} bytes exceeds the {cap}-byte cap — nothing was sent",
                bytes.len()
            )));
        }
        if std::str::from_utf8(&bytes).is_err() {
            return Err(Error::rejected(format!(
                "wiki cp refused '{rel}': text is not UTF-8 — nothing was sent"
            )));
        }
        (bytes.len() as u64, hex_encode(Sha256::digest(&bytes)))
    } else {
        let hashed = hash_open_file(&mut file)?;
        if hashed.0 > cap {
            return Err(Error::rejected(format!(
                "wiki cp refused '{rel}': {} bytes exceeds the {cap}-byte cap — nothing was sent",
                hashed.0
            )));
        }
        hashed
    };
    out.push(Walked {
        rel: rel.to_string(),
        kind,
        size,
        sha256: Some(sha),
        single_file,
    });
    Ok(())
}

pub(super) fn check_manifest(manifest: &Manifest) -> Result<()> {
    if manifest.version != 1 {
        return Err(Error::rejected("wiki cp manifest version must be 1"));
    }
    check_token(&manifest.source_id, "source id")?;
    if manifest.source_id.len() > SOURCE_CAP {
        return Err(Error::rejected("wiki cp source id is too long"));
    }
    canonical_wiki(&manifest.root)?;
    if manifest.entries.len() > MAX_ENTRIES {
        return Err(Error::rejected("wiki cp manifest has too many entries"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for entry in &manifest.entries {
        check_token(&entry.id, "entry id")?;
        if entry.id.len() > ID_CAP || entry.path.is_empty() || entry.path.len() > PATH_CAP {
            return Err(Error::rejected(format!(
                "wiki cp refused entry '{}'",
                entry.path
            )));
        }
        if !seen.insert(entry.path.clone()) {
            return Err(Error::rejected(format!(
                "wiki cp refused duplicate path '{}' — nothing was sent",
                entry.path
            )));
        }
        canonical_wiki(&join_wiki(&manifest.root, &entry.path))?;
        match entry.kind.as_str() {
            "dir" => {
                if entry.size != 0 || entry.sha256.is_some() {
                    return Err(Error::rejected(format!(
                        "wiki cp directory '{}' cannot carry file bytes",
                        entry.path
                    )));
                }
            }
            "text" | "blob" => {
                let sha = entry.sha256.as_deref().ok_or_else(|| {
                    Error::rejected(format!("wiki cp file '{}' is missing sha256", entry.path))
                })?;
                if sha.len() != 64
                    || !sha
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                {
                    return Err(Error::rejected(format!(
                        "wiki cp file '{}' has a bad sha256",
                        entry.path
                    )));
                }
            }
            _ => {
                return Err(Error::rejected(
                    "wiki cp manifest has an unsupported entry kind",
                ))
            }
        }
    }
    let encoded = serde_json::to_vec(manifest)
        .map_err(|e| Error::rejected(format!("wiki cp manifest: {e}")))?;
    if encoded.len() > MANIFEST_BYTES {
        return Err(Error::rejected(
            "wiki cp refused: the manifest exceeds 256 KiB — nothing was sent",
        ));
    }
    Ok(())
}

pub(in crate::wiki) fn check_export_bounds(entries: &[ExportEntry]) -> Result<()> {
    if entries.len() > MAX_ENTRIES {
        return Err(Error::rejected("wiki cp export exceeds 256 entries"));
    }
    let total: u64 = entries.iter().fold(0, |n, e| n.saturating_add(e.size));
    if total > TOTAL_CAP {
        return Err(Error::rejected("wiki cp export exceeds 256 MiB"));
    }
    for entry in entries {
        if entry.kind != "dir" && (entry.sha256.is_none() || entry.size > FILE_CAP) {
            return Err(Error::rejected(
                "wiki export file lacks its checksum or exceeds the file cap",
            ));
        }
    }
    Ok(())
}

pub(in crate::wiki) fn is_real_dir(path: &Path) -> bool {
    symlink_meta(path)
        .ok()
        .is_some_and(|meta| meta.is_dir() && !meta.file_type().is_symlink())
}

fn kind_for_name(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if name.ends_with(".md") || name.ends_with(".markdown") || name.ends_with(".txt") {
        "text".to_string()
    } else {
        "blob".to_string()
    }
}

pub(in crate::wiki) fn parse_kind(raw: &str) -> Result<String> {
    match raw {
        "dir" | "text" | "blob" => Ok(raw.to_string()),
        _ => Err(Error::rejected(format!(
            "wiki cp refused unsupported entry kind '{raw}'"
        ))),
    }
}

/// The proven failure of one decided entry: a checked conflict, a failed or
/// conflicting extraction, or the server's own `failed`. `applied` and
/// `skipped` are success. The caller refuses any other outcome as an
/// untrusted post-dispatch answer before it gets here.
pub(super) fn entry_failure(body: &Value, path: &str) -> Option<String> {
    if body.get("conflict").is_some() {
        return Some(format!("{path}: conflict"));
    }
    if matches!(body["extraction"].as_str(), Some("failed" | "conflict")) {
        return Some(format!(
            "{path}: original stored; extraction {}",
            body["extraction"]
        ));
    }
    match body["outcome"].as_str() {
        Some("conflict" | "failed") => Some(format!(
            "{path}: {}",
            body["outcome"].as_str().unwrap_or("failed")
        )),
        _ => None,
    }
}

pub(super) fn status_entry_failed(body: &Value) -> bool {
    matches!(
        body["outcome"].as_str(),
        Some("conflict" | "failed" | "error")
    ) || body.get("conflict").is_some()
        || matches!(
            body["extraction"].as_str(),
            Some("failed" | "empty" | "conflict")
        )
}

pub(super) fn source_identity(kind: &str, path: &Path) -> Result<String> {
    let abs = lexical_abs(path)?;
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(abs.to_string_lossy().as_bytes());
    let id = hex_encode(hasher.finalize());
    check_token(&id, "source id")?;
    Ok(id)
}

pub(super) fn entry_id(rel: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(rel.as_bytes());
    hex_encode(hasher.finalize())
}

pub(in crate::wiki) fn plan_digest(entries: &[ExportEntry]) -> String {
    let mut hasher = Sha256::new();
    for entry in entries {
        hasher.update(entry.path.as_bytes());
        hasher.update([0]);
        hasher.update(entry.rev.as_bytes());
        hasher.update([0]);
        hasher.update(entry.sha256.as_deref().unwrap_or("").as_bytes());
        hasher.update([0]);
    }
    hex_encode(hasher.finalize())
}

pub(super) fn wiki_operand(raw: &str) -> Option<&str> {
    raw.strip_prefix("wiki:")
}

pub(in crate::wiki) fn canonical_wiki(path: &str) -> Result<String> {
    if path.len() > PATH_CAP {
        return Err(Error::rejected(format!(
            "wiki path '{path}' refused: path is too long"
        )));
    }
    let norm = crate::wiki::normalize(path)?;
    if norm.is_empty() {
        return Err(Error::rejected(
            "wiki cp refused the wiki root — name a directory under it",
        ));
    }
    if norm.len() > PATH_CAP {
        return Err(Error::rejected(format!(
            "wiki path '{norm}' refused: path is too long"
        )));
    }
    Ok(norm)
}

pub(super) fn split_wiki(path: &str) -> Result<(String, String)> {
    match path.rsplit_once('/') {
        Some((parent, name)) if !parent.is_empty() && !name.is_empty() => {
            Ok((canonical_wiki(parent)?, name.to_string()))
        }
        _ => Err(Error::rejected(
            "wiki cp file destination needs a parent directory — nothing was sent",
        )),
    }
}

pub(in crate::wiki) fn join_wiki(root: &str, rel: &str) -> String {
    if rel.is_empty() {
        root.to_string()
    } else if root.is_empty() {
        rel.to_string()
    } else {
        format!("{root}/{rel}")
    }
}

pub(super) fn check_token(value: &str, what: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > ID_CAP.max(SOURCE_CAP)
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(Error::rejected(format!("wiki cp refused {what} '{value}'")));
    }
    Ok(())
}

pub(in crate::wiki) fn required_str(body: &Value, key: &str) -> Result<String> {
    body[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Error::rejected(format!("wiki cp response omitted {key}")))
}

pub(in crate::wiki) fn optional_sha(body: &Value) -> Result<Option<String>> {
    match body.get("sha256") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(sha))
            if sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            Ok(Some(sha.to_ascii_lowercase()))
        }
        _ => Err(Error::rejected("wiki export returned a bad sha256")),
    }
}

pub(in crate::wiki) fn is_absent(error: &Error) -> bool {
    // A hosted board names a missing path by code (CAD-2260); a daemon says it
    // in its message.
    if error.code() == Some("not_found") {
        return true;
    }
    let text = error.to_string().to_ascii_lowercase();
    text.contains("no such") || text.contains("not found") || text.contains("does not exist")
}

/// Bounded `read` refuses a directory (`a range read needs a text page`).
/// That is the directory case of discovery and an update pin, not a missing file.
pub(super) fn is_directory_range(error: &Error) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    text.contains("range") && text.contains("director")
}

/// File discovery is the existing bounded read. A directory (range refusal,
/// or an explicit `kind: dir`) is enumerated with the ordinary list action.
/// Absence is success so a new import root does not require a parent read.
pub(super) fn discover_read_or_list(
    read: Result<Value>,
    list: impl FnOnce() -> Result<Value>,
) -> Result<()> {
    let list_directory = match read {
        Ok(body) => body.get("kind").and_then(Value::as_str) == Some("dir"),
        Err(error) if is_absent(&error) => return Ok(()),
        Err(error) if is_directory_range(&error) => true,
        Err(error) => return Err(error),
    };
    if !list_directory {
        return Ok(());
    }
    match list() {
        Ok(_) => Ok(()),
        Err(error) if is_absent(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

pub(in crate::wiki) fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}
