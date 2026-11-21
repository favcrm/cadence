//! Wiki-to-local export pipeline for `wiki_transfer` (`cadence wiki cp`).

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::io::{Read, Seek, Write};
use std::path::Path;

use base64::Engine;
use serde_json::{json, Value};

use super::fs_guard::{
    commit_stage, guard_local, hash_open_file, hold_relative_dir, leaf_name, refuse_export_leaf,
    refused_path, symlink_meta, ExportAnchor, PrivateStage,
};
use super::{
    canonical_wiki, check_export_bounds, is_absent, is_real_dir, join_wiki, optional_sha,
    parse_kind, plan_digest, required_str, Channel, CopyRequest, CHUNK_BYTES, MAX_ENTRIES,
};
use crate::error::{Error, Result};
use crate::wiki::ABSENT;

pub(super) fn export_wiki(
    request: &CopyRequest<'_>,
    wiki_src: &str,
    channel: &mut dyn Channel,
) -> Result<i32> {
    if request.resume.is_some() {
        return Err(Error::rejected(
            "wiki cp --resume applies to a local-to-wiki import job, not an export",
        ));
    }
    if request.wait_index {
        return Err(Error::rejected(
            "wiki cp --wait-index applies to an import job, not an export",
        ));
    }
    let root_or_file = canonical_wiki(wiki_src)?;
    let dest = guard_local(Path::new(request.dest), true)?;
    channel.discover(&root_or_file)?;
    let listed = list_export(channel, &root_or_file, request.recursive)?;
    let digest = plan_digest(&listed);
    let plan = serde_json::to_string(&json!({
        "op": "export",
        "root": root_or_file,
        "digest": digest,
        "entries": listed.iter().map(|e| json!({
            "path": e.path,
            "kind": e.kind,
            "rev": e.rev,
            "size": e.size,
            "sha256": e.sha256,
        })).collect::<Vec<_>>(),
    }))
    .map_err(|e| Error::rejected(format!("wiki cp export plan: {e}")))?;
    // The plan precedes the publication: a closed stdout must not panic
    // the export into an unexecuted, successful-looking exit.
    crate::output::print_line(&plan);
    check_export_dest(&dest, &root_or_file, &listed, request.update)?;
    // One no-follow directory fd for the whole publication. Child directories
    // and file names are created relative to it, including an empty source.
    let anchor = ExportAnchor::open(&dest, &root_or_file, &listed)?;
    let mut failures = Vec::new();
    for entry in &listed {
        if let Err(error) = write_anchored(channel, &root_or_file, &anchor, entry, request.update) {
            failures.push(format!("{}: {error}", entry.path));
            break;
        }
    }
    if failures.is_empty() {
        Ok(0)
    } else {
        // The failure report precedes the nonzero status: a closed stderr
        // must not panic it into success.
        let report = serde_json::to_string(&json!({"op": "export", "failures": failures}))
            .unwrap_or_else(|_| "wiki cp export incomplete".to_string());
        crate::output::eprint_line(&report);
        Ok(1)
    }
}

#[derive(Clone)]
pub(in crate::wiki) struct ExportEntry {
    pub(super) path: String,
    pub(super) kind: String,
    pub(super) size: u64,
    pub(super) sha256: Option<String>,
    pub(super) rev: String,
}

fn list_export(
    channel: &mut dyn Channel,
    wiki_src: &str,
    recursive: bool,
) -> Result<Vec<ExportEntry>> {
    let first = export_call(channel, wiki_src, wiki_src, 0, Some(1), None)?;
    let kind = parse_kind(first["kind"].as_str().unwrap_or(""))?;
    if kind == "dir" {
        if !recursive {
            return Err(Error::rejected(
                "wiki cp of a wiki directory needs --recursive — nothing was written",
            ));
        }
        let mut out = Vec::new();
        collect_dir(channel, wiki_src, wiki_src, "", &mut out)?;
        out.sort_by(|a, b| a.path.cmp(&b.path));
        check_export_bounds(&out)?;
        Ok(out)
    } else {
        let size = first["size"]
            .as_u64()
            .ok_or_else(|| Error::rejected(format!("wiki export '{wiki_src}' omitted size")))?;
        let rev = required_str(&first, "rev")?;
        let sha = optional_sha(&first)?;
        let entry = ExportEntry {
            path: String::new(),
            kind,
            size,
            sha256: sha,
            rev,
        };
        check_export_bounds(std::slice::from_ref(&entry))?;
        Ok(vec![entry])
    }
}

fn collect_dir(
    channel: &mut dyn Channel,
    scope: &str,
    dir: &str,
    rel: &str,
    out: &mut Vec<ExportEntry>,
) -> Result<()> {
    let mut offset = 0u64;
    loop {
        let page = export_call(channel, scope, dir, offset, Some(1), None)?;
        let entries = page["entries"].as_array().ok_or_else(|| {
            Error::rejected(format!(
                "wiki export '{dir}' did not return directory entries"
            ))
        })?;
        for raw in entries {
            let name = raw["name"]
                .as_str()
                .ok_or_else(|| Error::rejected(format!("wiki export '{dir}' entry has no name")))?;
            if name.is_empty()
                || name.contains('/')
                || name == "."
                || name == ".."
                || name.starts_with('.')
            {
                return Err(Error::rejected(format!(
                    "wiki export '{dir}' refused entry '{name}'"
                )));
            }
            let child_rel = if rel.is_empty() {
                name.to_string()
            } else {
                format!("{rel}/{name}")
            };
            let child = join_wiki(dir, name);
            canonical_wiki(&child)?;
            let kind = parse_kind(
                raw["kind"]
                    .as_str()
                    .ok_or_else(|| Error::rejected("wiki export entry omitted kind"))?,
            )?;
            if kind == "dir" {
                out.push(ExportEntry {
                    path: child_rel.clone(),
                    kind,
                    size: 0,
                    sha256: None,
                    rev: ABSENT.to_string(),
                });
                collect_dir(channel, scope, &child, &child_rel, out)?;
            } else {
                let meta = export_call(channel, scope, &child, 0, Some(1), None)?;
                out.push(ExportEntry {
                    path: child_rel,
                    kind,
                    size: meta["size"].as_u64().ok_or_else(|| {
                        Error::rejected(format!("wiki export '{child}' omitted size"))
                    })?,
                    sha256: optional_sha(&meta)?,
                    rev: required_str(&meta, "rev")?,
                });
            }
            if out.len() > MAX_ENTRIES {
                return Err(Error::rejected(
                    "wiki cp refused: the export exceeds 256 entries — nothing was written",
                ));
            }
        }
        match page.get("next_offset") {
            Some(Value::Null) => break,
            Some(next)
                if !entries.is_empty() && next.as_u64() == Some(offset + entries.len() as u64) =>
            {
                offset += entries.len() as u64;
            }
            _ => return Err(Error::rejected("wiki export returned invalid pagination")),
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn write_export_entry(
    channel: &mut dyn Channel,
    scope: &str,
    dest_root: &Path,
    entry: &ExportEntry,
    all: &[ExportEntry],
    update: bool,
) -> Result<()> {
    let anchor = ExportAnchor::open(dest_root, scope, all)?;
    write_anchored(channel, scope, &anchor, entry, update)
}

fn write_anchored(
    channel: &mut dyn Channel,
    scope: &str,
    anchor: &ExportAnchor,
    entry: &ExportEntry,
    update: bool,
) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;

    if entry.kind == "dir" {
        if anchor.single_name.is_some() {
            return Err(Error::rejected(
                "wiki export refused a directory where a file destination was selected",
            ));
        }
        let _created = hold_relative_dir(&anchor.dir, Path::new(&entry.path))?;
        return Ok(());
    }

    let (parent, name, display) = if let Some(single) = &anchor.single_name {
        let parent = anchor.dir.try_clone().map_err(|err| {
            Error::rejected(format!("wiki cp cannot hold export directory: {err}"))
        })?;
        let display = anchor
            .root
            .join(std::ffi::OsStr::from_bytes(single.as_bytes()));
        (parent, single.clone(), display)
    } else {
        let rel = Path::new(&entry.path);
        let parent_rel = rel.parent().unwrap_or(Path::new(""));
        let leaf = rel
            .file_name()
            .ok_or_else(|| refused_path(rel, "unsupported path"))?;
        let parent = hold_relative_dir(&anchor.dir, parent_rel)?;
        let name = leaf_name(leaf, rel)?;
        (parent, name, anchor.root.join(rel))
    };
    refuse_export_leaf(&parent, &name, &display, update)?;
    let mut stage = PrivateStage::create(&parent, &display)?;
    let mut got = 0u64;
    while got < entry.size {
        let ask = (entry.size - got).min(CHUNK_BYTES as u64);
        let page = export_call(
            channel,
            scope,
            &export_path(scope, entry),
            got,
            Some(ask),
            Some(&entry.rev),
        )?;
        let page_rev = required_str(&page, "rev")?;
        if page_rev != entry.rev {
            return Err(Error::rejected(format!(
                "export revision changed for '{}'",
                entry.path
            )));
        }
        let data = page["data_base64"].as_str().unwrap_or("");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| Error::rejected("export returned undecodable bytes"))?;
        if bytes.is_empty() || (bytes.len() as u64 != ask && got + bytes.len() as u64 != entry.size)
        {
            return Err(Error::rejected("export returned a short slice"));
        }
        stage.file.write_all(&bytes)?;
        got += bytes.len() as u64;
    }
    if got != entry.size {
        return Err(Error::rejected("export length mismatch"));
    }
    stage.file.sync_all()?;
    let (_, file_hash) = hash_open_file(&mut stage.file)?;
    if let Some(want) = &entry.sha256 {
        if want != &file_hash {
            return Err(Error::rejected(format!(
                "export hash mismatch for '{}'",
                entry.path
            )));
        }
    }
    if entry.kind == "text" {
        let mut text = Vec::new();
        stage.file.rewind()?;
        stage.file.read_to_end(&mut text)?;
        if std::str::from_utf8(&text).is_err() {
            return Err(Error::rejected(format!(
                "export '{}' is not UTF-8 text",
                entry.path
            )));
        }
    }
    commit_stage(&stage, &name, update, &display)?;
    parent
        .sync_all()
        .map_err(|err| Error::rejected(format!("cannot sync {}: {err}", display.display())))?;
    Ok(())
}

fn export_path(scope: &str, entry: &ExportEntry) -> String {
    if entry.path.is_empty() {
        scope.to_string()
    } else {
        join_wiki(scope, &entry.path)
    }
}

fn export_call(
    channel: &mut dyn Channel,
    scope: &str,
    path: &str,
    offset: u64,
    bytes: Option<u64>,
    rev: Option<&str>,
) -> Result<Value> {
    let mut op = json!({"op": "export", "path": path, "offset": offset});
    if let Some(bytes) = bytes {
        op["bytes"] = json!(bytes);
    }
    if let Some(rev) = rev {
        op["rev"] = json!(rev);
    }
    let body = match channel.transfer(scope, op, false) {
        Ok(body) => body,
        Err(error) if is_absent(&error) => {
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    if body.get("conflict").is_some() || body.get("error").is_some() {
        return Err(Error::rejected(format!(
            "wiki export '{path}' refused: {}",
            body.get("error")
                .and_then(Value::as_str)
                .unwrap_or("conflict")
        )));
    }
    Ok(body)
}

pub(super) fn check_export_dest(
    dest: &Path,
    scope: &str,
    entries: &[ExportEntry],
    update: bool,
) -> Result<()> {
    let single = entries.len() == 1 && entries[0].path.is_empty();
    if !single {
        if let Ok(meta) = symlink_meta(dest) {
            if meta.file_type().is_symlink() {
                return Err(refused_path(dest, "symlink"));
            }
            if !meta.is_dir() {
                return Err(refused_path(dest, "export destination is not a directory"));
            }
        }
    }
    for entry in entries {
        let path = if single {
            if is_real_dir(dest) {
                let name = scope.rsplit('/').next().unwrap_or("export");
                dest.join(name)
            } else {
                dest.to_path_buf()
            }
        } else {
            dest.join(&entry.path)
        };
        if entry.kind == "dir" {
            if let Ok(meta) = symlink_meta(&path) {
                if meta.file_type().is_symlink() || !meta.is_dir() {
                    return Err(refused_path(&path, "conflicts with the exported directory"));
                }
            }
            continue;
        }
        if let Ok(meta) = symlink_meta(&path) {
            if meta.file_type().is_symlink() {
                return Err(refused_path(&path, "symlink"));
            }
            if !update {
                return Err(Error::rejected(format!(
                    "wiki cp refused to overwrite '{}' — pass --update",
                    path.display()
                )));
            }
            if !meta.is_file() {
                return Err(refused_path(&path, "not a regular file"));
            }
        }
    }
    Ok(())
}
