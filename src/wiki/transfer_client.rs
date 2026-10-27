//! `cadence wiki cp` — resumable manifest copy (CAD-914).
//!
//! One operand is a local path and the other is `wiki:<canonical-path>`.
//! Local Unix RPC and a remote org both speak the `wiki_transfer` ops.
//! Remote delivery is the existing wiki `write` (begin/chunk/apply/finish)
//! and `job_status` (status/export) actions; each request mints a fresh
//! envelope through [`crate::remote_cli::call_verb`]. The source tree is
//! walked and the manifest checked before any mutating op.

use std::ffi::CString;
use std::fs::{self, File};
use std::io::{Read, Seek, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::wiki::transfer::{manifest_digest, Manifest, ManifestEntry};
use crate::wiki::ABSENT;

const MAX_ENTRIES: usize = 256;
const MANIFEST_BYTES: usize = 256 * 1024;
const FILE_CAP: u64 = 100 * 1024 * 1024;
const TOTAL_CAP: u64 = 256 * 1024 * 1024;
const TEXT_CAP: u64 = 64 * 1024 * 1024;
const CHUNK_BYTES: usize = 16_384;
const PATH_CAP: usize = 512;
const ID_CAP: usize = 80;
const SOURCE_CAP: usize = 128;
const STATUS_PAGE: u64 = 32;

/// A copy the CLI already parsed. `wiki:` is stripped; paths are not yet
/// canonical.
pub struct CopyRequest<'a> {
    pub source: &'a str,
    pub dest: &'a str,
    pub recursive: bool,
    pub update: bool,
    pub resume: Option<&'a str>,
    pub wait_index: bool,
}

pub fn copy_local(state_dir: &Path, request: &CopyRequest<'_>) -> Result<i32> {
    let deadline = Instant::now() + Duration::from_secs(crate::remote_cli::WAKE_TIMEOUT_DEFAULT);
    let mut channel = LocalChannel {
        state_dir,
        deadline,
    };
    execute(request, &mut channel, deadline)
}

pub fn copy_remote(
    target: &crate::remote_cli::RemoteTarget,
    wake_timeout: u64,
    request: &CopyRequest<'_>,
) -> Result<i32> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(wake_timeout))
        .ok_or_else(|| Error::rejected("invalid wiki copy timeout"))?;
    let mut channel = RemoteChannel {
        target,
        wake_timeout,
        deadline,
    };
    execute(request, &mut channel, deadline)
}

fn remaining_time(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| {
            Error::busy_coded(
                "timeout",
                "wiki cp command deadline elapsed; no transfer write was replayed",
            )
        })
}

fn ensure_deadline(deadline: Instant) -> Result<()> {
    remaining_time(deadline).map(|_| ())
}

trait Channel {
    /// `write` selects the mutating wiki action (`write` / local RPC).
    /// Reads use `job_status` remotely and the same RPC locally.
    fn transfer(&mut self, root: &str, op: Value, write: bool) -> Result<Value>;
    /// Bounded ordinary read (`offset` 0, `bytes` 0) for a file, including
    /// a large text page: metadata only, and no parent read scope. A
    /// directory is enumerated. Absence is a new import root. Remote read
    /// and enumerate may wake; transfer `job_status` / `write` must not.
    fn discover(&mut self, path: &str) -> Result<()>;
    /// Current destination revision from an existing read, never a new if_rev guess.
    fn current_rev(&mut self, path: &str) -> Result<String>;
}

struct LocalChannel<'a> {
    state_dir: &'a Path,
    deadline: Instant,
}

impl Channel for LocalChannel<'_> {
    fn transfer(&mut self, root: &str, mut op: Value, _write: bool) -> Result<Value> {
        let target = if op["op"] == "export" {
            op["path"]
                .as_str()
                .ok_or_else(|| Error::rejected("wiki export omitted its target"))?
        } else {
            root
        };
        op["root"] = json!(target);
        crate::client::rpc_timeout(
            self.state_dir,
            "wiki_transfer",
            op,
            remaining_time(self.deadline)?,
        )
    }

    fn discover(&mut self, path: &str) -> Result<()> {
        let read = self.read_page_bounded(path);
        discover_read_or_list(read, || self.list_page(path))
    }

    fn current_rev(&mut self, path: &str) -> Result<String> {
        // Same bounded original read as remote: a whole page can exceed the
        // reply line, and only `rev` is needed to pin an update.
        match self.read_page_bounded(path) {
            Ok(body) => rev_from_read(&body, path),
            Err(error) if is_absent(&error) || is_directory_range(&error) => Ok(ABSENT.to_string()),
            Err(error) => Err(error),
        }
    }
}

impl LocalChannel<'_> {
    fn read_page_bounded(&self, path: &str) -> Result<Value> {
        crate::client::rpc_timeout(
            self.state_dir,
            "wiki_read",
            json!({"path": path, "offset": 0, "bytes": 0}),
            remaining_time(self.deadline)?,
        )
    }

    fn list_page(&self, path: &str) -> Result<Value> {
        crate::client::rpc_timeout(
            self.state_dir,
            "wiki_ls",
            json!({"path": path}),
            remaining_time(self.deadline)?,
        )
    }
}

struct RemoteChannel<'a> {
    target: &'a crate::remote_cli::RemoteTarget,
    wake_timeout: u64,
    deadline: Instant,
}

impl RemoteChannel<'_> {
    /// Existing read/enumerate actions. These may wake. Transfer ops must not.
    fn guarded(&self, verb: &str, args: Map<String, Value>, retry_wake: bool) -> Result<Value> {
        ensure_deadline(self.deadline)?;
        match crate::remote_cli::call_bounded(
            self.target,
            verb,
            args,
            self.deadline,
            self.wake_timeout,
            retry_wake,
            |_| {},
        )? {
            crate::remote_cli::RemoteAnswer::Ok(body)
            | crate::remote_cli::RemoteAnswer::Conflict(body) => Ok(body),
        }
    }
}

impl Channel for RemoteChannel<'_> {
    fn transfer(&mut self, root: &str, op: Value, write: bool) -> Result<Value> {
        let verb = if write { "write" } else { "job_status" };
        let path = match op.get("op").and_then(Value::as_str) {
            Some("export") => op.get("path").and_then(Value::as_str).unwrap_or(root),
            Some("begin") => op
                .get("manifest")
                .and_then(|manifest| manifest.get("root"))
                .and_then(Value::as_str)
                .unwrap_or(root),
            _ => op.get("root").and_then(Value::as_str).unwrap_or(root),
        };
        let mut args = Map::new();
        args.insert("path".into(), json!(path));
        args.insert("transfer".into(), op);
        // A busy or unknown transfer result is not replayed.
        self.guarded(verb, args, false)
    }

    fn discover(&mut self, path: &str) -> Result<()> {
        let mut args = Map::new();
        args.insert("path".into(), json!(path));
        args.insert("offset".into(), json!(0));
        args.insert("bytes".into(), json!(0));
        // Wake only this ordinary read. `job_status` and `write` stay unwoken.
        let read = self.guarded("read", args, true);
        discover_read_or_list(read, || {
            let mut args = Map::new();
            args.insert("path".into(), json!(path));
            self.guarded("enumerate", args, true)
        })
    }

    fn current_rev(&mut self, path: &str) -> Result<String> {
        // A whole-page `read` carries the original text. Remote replies stop
        // at 8 MiB, while admitted text is up to 64 MiB, so pin `rev` from
        // the existing bounded original read (offset 0, zero bytes).
        let mut args = Map::new();
        args.insert("path".into(), json!(path));
        args.insert("offset".into(), json!(0));
        args.insert("bytes".into(), json!(0));
        match self.guarded("read", args, true) {
            Ok(body) => rev_from_read(&body, path),
            // A directory has no original range. Create-only pins use ABSENT;
            // file updates never target a directory entry.
            Err(error) if is_absent(&error) || is_directory_range(&error) => Ok(ABSENT.to_string()),
            Err(error) => Err(error),
        }
    }
}

fn execute(request: &CopyRequest<'_>, channel: &mut dyn Channel, deadline: Instant) -> Result<i32> {
    let source_wiki = wiki_operand(request.source);
    let dest_wiki = wiki_operand(request.dest);
    match (source_wiki, dest_wiki) {
        (None, Some(dest)) => import_local(request, dest, channel, deadline),
        (Some(source), None) => export_wiki(request, source, channel),
        _ => Err(Error::rejected(
            "wiki cp needs exactly one wiki:<path> operand — the other is a local path",
        )),
    }
}

fn import_local(
    request: &CopyRequest<'_>,
    wiki_dest: &str,
    channel: &mut dyn Channel,
    deadline: Instant,
) -> Result<i32> {
    if let Some(job) = request.resume {
        check_token(job, "job id")?;
    }
    let local = guard_local(Path::new(request.source), false)?;
    // Refuse a symlinked ancestor before metadata, hashing, or discovery.
    // `guard_local` is lexical; leaf `O_NOFOLLOW` does not confine `./link/child`.
    let meta = contained_source_meta(&local)?;
    let (root, file_name) = if meta.is_dir() {
        if !request.recursive {
            return Err(Error::rejected(
                "wiki cp of a directory needs --recursive — nothing was sent",
            ));
        }
        (canonical_wiki(wiki_dest)?, None)
    } else if meta.is_file() {
        let dest = canonical_wiki(wiki_dest)?;
        let (parent, name) = split_wiki(&dest)?;
        (parent, Some(name))
    } else {
        return Err(refused_path(&local, "not a regular file or directory"));
    };

    let walked = walk_local(&local, file_name.as_deref())?;
    // An empty directory is a valid manifest. Finish materializes `root`.
    channel.discover(&root)?;
    let source_id = source_identity("local", &local)?;
    let manifest = if let Some(job) = request.resume {
        let admitted = admitted_manifest(channel, &root, job)?;
        if admitted.source_id != source_id || admitted.root != root {
            return Err(Error::rejected(
                "wiki cp --resume refused: the local source or destination does not match                  the admitted manifest — revisions were not recomputed",
            ));
        }
        ensure_source_matches(&admitted, &walked)?;
        admitted
    } else {
        let mut entries = Vec::with_capacity(walked.len());
        for item in &walked {
            let full = join_wiki(&root, &item.rel);
            canonical_wiki(&full)?;
            let if_rev = if request.update && item.kind != "dir" {
                channel.current_rev(&full)?
            } else {
                ABSENT.to_string()
            };
            entries.push(ManifestEntry {
                id: entry_id(&item.rel),
                path: item.rel.clone(),
                kind: item.kind.clone(),
                size: item.size,
                sha256: item.sha256.clone(),
                if_rev,
            });
        }
        Manifest {
            version: 1,
            source_id,
            root: root.clone(),
            entries,
        }
    };
    check_manifest(&manifest)?;

    let mut begin = json!({"op": "begin", "manifest": manifest});
    if let Some(job) = request.resume {
        begin["job_id"] = json!(job);
    }
    let receipt = channel.transfer(&root, begin, true)?;
    let job_id = required_str(&receipt, "job_id")?;
    let digest = required_str(&receipt, "digest")?;
    check_token(&job_id, "job id")?;
    println!(
        "{}",
        serde_json::to_string(&json!({"job_id": job_id, "digest": digest, "root": root}))
            .map_err(|e| Error::rejected(format!("wiki cp receipt: {e}")))?
    );

    let progress = status_received(channel, &root, &job_id)?;
    let mut failures: Vec<String> = Vec::new();
    for item in &walked {
        let id = entry_id(&item.rel);
        let entry_progress = progress
            .get(&id)
            .ok_or_else(|| Error::rejected("wiki cp job status omitted an admitted entry"))?;
        if !entry_progress.decided {
            if let Err(error) = send_entry(
                channel,
                &root,
                &job_id,
                item,
                &local,
                entry_progress.received,
            ) {
                // A dropped chunk is ambiguous: do not finish and do not retry.
                return Err(Error::rejected(format!(
                    "wiki cp job {job_id} stopped on '{}': {error} — resume with --resume {job_id}; \
                     this attempt was not retried",
                    item.rel
                )));
            }
        }
        match channel.transfer(
            &root,
            json!({"op": "apply", "job_id": job_id, "root": root, "entry_id": id}),
            true,
        ) {
            Ok(body) => {
                // One apply. `unresolved` is not terminal: the stage stays,
                // finish would be refused, and a later resume may apply again.
                // Do not loop apply or recompute the admitted `if_rev` here.
                if body["outcome"].as_str() == Some("unresolved") {
                    let detail = body["error"]
                        .as_str()
                        .filter(|text| !text.is_empty())
                        .unwrap_or("publication is not committed; staged bytes were kept");
                    let mut notes = vec![detail.to_string()];
                    notes.extend(failures);
                    return Err(Error::rejected(format!(
                        "wiki cp job {job_id} stopped on '{}': unresolved — {}; \
                         finish was not called; this attempt was not retried — resume with \
                         --resume {job_id}; admitted if_rev was not recomputed",
                        item.rel,
                        notes.join("; ")
                    )));
                }
                if let Some(why) = entry_failure(&body, &item.rel) {
                    failures.push(why);
                }
            }
            Err(error) => {
                return Err(Error::rejected(format!(
                    "wiki cp job {job_id} apply '{}' was not confirmed: {error} — not retried",
                    item.rel
                )));
            }
        }
    }

    let finished = channel.transfer(
        &root,
        json!({"op": "finish", "job_id": job_id, "root": root}),
        true,
    )?;
    if let Some(why) = job_failure(&finished) {
        failures.push(why);
    }
    println!(
        "{}",
        serde_json::to_string(&finished)
            .map_err(|e| Error::rejected(format!("wiki cp finish: {e}")))?
    );
    if request.wait_index {
        match wait_index(channel, &root, &job_id, deadline) {
            Ok(body) => println!(
                "{}",
                serde_json::to_string(&body)
                    .map_err(|e| Error::rejected(format!("wiki cp index: {e}")))?
            ),
            Err(error) => {
                failures.push(error.to_string());
            }
        }
    }
    if failures.is_empty() {
        Ok(0)
    } else {
        eprintln!(
            "{}",
            serde_json::to_string(&json!({
                "job_id": job_id,
                "failures": failures,
            }))
            .unwrap_or_else(|_| format!("wiki cp job {job_id} completed with entry failures"))
        );
        Ok(1)
    }
}

fn send_entry(
    channel: &mut dyn Channel,
    root: &str,
    job_id: &str,
    item: &Walked,
    local_root: &Path,
    received: u64,
) -> Result<()> {
    if item.kind == "dir" {
        return Ok(());
    }
    if item.size == 0 {
        let body = channel.transfer(
            root,
            json!({
                "op": "chunk",
                "job_id": job_id,
                "root": root,
                "entry_id": entry_id(&item.rel),
                "offset": 0,
                "data_base64": "",
            }),
            true,
        )?;
        if body["received"].as_u64() != Some(0) {
            return Err(Error::rejected(format!(
                "chunk '{}' did not confirm an empty file — not retried",
                item.rel
            )));
        }
        return Ok(());
    }
    if received > item.size {
        return Err(Error::rejected(format!(
            "staged length {received} exceeds source size {}",
            item.size
        )));
    }
    let path = if item.single_file {
        local_root.to_path_buf()
    } else {
        local_root.join(&item.rel)
    };
    // Re-check the ancestor chain, then open only through no-follow fds.
    let mut file = open_contained(&path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut hashed = 0u64;
    while hashed < received {
        let want =
            usize::try_from((received - hashed).min(CHUNK_BYTES as u64)).unwrap_or(CHUNK_BYTES);
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            return Err(Error::rejected(format!(
                "source '{}' shrank during copy",
                item.rel
            )));
        }
        hasher.update(&buf[..n]);
        hashed += n as u64;
    }
    let mut offset = received;
    while offset < item.size {
        let want =
            usize::try_from((item.size - offset).min(CHUNK_BYTES as u64)).unwrap_or(CHUNK_BYTES);
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            return Err(Error::rejected(format!(
                "source '{}' shrank during copy",
                item.rel
            )));
        }
        hasher.update(&buf[..n]);
        let data_base64 = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
        let body = channel.transfer(
            root,
            json!({
                "op": "chunk",
                "job_id": job_id,
                "root": root,
                "entry_id": entry_id(&item.rel),
                "offset": offset,
                "data_base64": data_base64,
            }),
            true,
        )?;
        let got = body["received"].as_u64().ok_or_else(|| {
            Error::rejected(format!(
                "chunk '{}' did not confirm received bytes — not retried",
                item.rel
            ))
        })?;
        if got != offset + n as u64 {
            return Err(Error::rejected(format!(
                "chunk '{}' confirmed {got} bytes, expected {} — not retried",
                item.rel,
                offset + n as u64
            )));
        }
        offset = got;
    }
    let digest = hex_encode(hasher.finalize());
    if item.sha256.as_deref() != Some(digest.as_str()) {
        return Err(Error::rejected(format!(
            "source '{}' changed after the manifest was pinned",
            item.rel
        )));
    }
    Ok(())
}

fn export_wiki(
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
    println!(
        "{}",
        serde_json::to_string(&json!({
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
        .map_err(|e| Error::rejected(format!("wiki cp export plan: {e}")))?
    );
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
        eprintln!(
            "{}",
            serde_json::to_string(&json!({"op": "export", "failures": failures}))
                .unwrap_or_else(|_| "wiki cp export incomplete".to_string())
        );
        Ok(1)
    }
}

#[derive(Clone)]
struct ExportEntry {
    path: String,
    kind: String,
    size: u64,
    sha256: Option<String>,
    rev: String,
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
fn write_export_entry(
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

fn check_export_dest(
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

fn admitted_manifest(channel: &mut dyn Channel, root: &str, job_id: &str) -> Result<Manifest> {
    let mut entries = Vec::new();
    let mut source_id: Option<String> = None;
    let mut digest: Option<String> = None;
    let mut offset = 0u64;
    loop {
        let page = channel.transfer(
            root,
            json!({
                "op": "status", "job_id": job_id, "root": root,
                "offset": offset, "limit": STATUS_PAGE,
            }),
            false,
        )?;
        if page["root"].as_str() != Some(root) || page["job_id"].as_str() != Some(job_id) {
            return Err(Error::rejected(
                "wiki cp resume status changed its job or root",
            ));
        }
        for (field, pinned) in [("source_id", &mut source_id), ("digest", &mut digest)] {
            let value = page[field]
                .as_str()
                .ok_or_else(|| Error::rejected("wiki cp resume status omitted its identity"))?;
            match pinned {
                Some(original) if original.as_str() != value => {
                    return Err(Error::rejected(
                        "wiki cp resume status changed its identity",
                    ));
                }
                None => *pinned = Some(value.to_string()),
                _ => {}
            }
        }
        let page_entries = page["entries"]
            .as_array()
            .filter(|page| page.len() <= STATUS_PAGE as usize)
            .ok_or_else(|| Error::rejected("wiki cp resume status has an invalid entry page"))?;
        for entry in page_entries {
            let admitted: ManifestEntry = serde::Deserialize::deserialize(&entry["manifest_entry"])
                .map_err(|_| {
                    Error::rejected("wiki cp resume status omitted original manifest fields")
                })?;
            if entry["entry_id"].as_str() != Some(admitted.id.as_str()) {
                return Err(Error::rejected(
                    "wiki cp resume status changed an entry identity",
                ));
            }
            entries.push(admitted);
        }
        if entries.len() > MAX_ENTRIES {
            return Err(Error::rejected(
                "wiki cp resume status exceeds the manifest bound",
            ));
        }
        match page.get("next_offset") {
            Some(Value::Null) => break,
            Some(next)
                if next.as_u64() == Some(offset + page_entries.len() as u64)
                    && !page_entries.is_empty() =>
            {
                offset += page_entries.len() as u64;
            }
            _ => {
                return Err(Error::rejected(
                    "wiki cp resume status has invalid pagination",
                ))
            }
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path).then(a.id.cmp(&b.id)));
    let manifest = Manifest {
        version: 1,
        source_id: source_id
            .ok_or_else(|| Error::rejected("wiki cp resume source identity missing"))?,
        root: root.to_string(),
        entries,
    };
    check_manifest(&manifest)?;
    if Some(manifest_digest(&manifest)?) != digest {
        return Err(Error::rejected(
            "wiki cp resume manifest does not match its admitted digest",
        ));
    }
    Ok(manifest)
}

fn ensure_source_matches(manifest: &Manifest, walked: &[Walked]) -> Result<()> {
    if manifest.entries.len() != walked.len() {
        return Err(Error::rejected(
            "wiki cp --resume refused: the local tree does not match the admitted manifest",
        ));
    }
    for (admitted, item) in manifest.entries.iter().zip(walked) {
        if admitted.path != item.rel
            || admitted.kind != item.kind
            || admitted.size != item.size
            || admitted.sha256 != item.sha256
            || admitted.id != entry_id(&item.rel)
        {
            return Err(Error::rejected(format!(
                "wiki cp --resume refused: '{}' does not match the admitted manifest —                  if_rev was not recomputed",
                item.rel
            )));
        }
    }
    Ok(())
}

fn rev_from_read(body: &Value, path: &str) -> Result<String> {
    if body["kind"] == "dir" {
        return Ok(ABSENT.to_string());
    }
    body["rev"]
        .as_str()
        .filter(|rev| !rev.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            Error::rejected(format!(
                "wiki cp --update could not pin the current revision of '{path}'"
            ))
        })
}

fn wait_index(
    channel: &mut dyn Channel,
    root: &str,
    job_id: &str,
    deadline: Instant,
) -> Result<Value> {
    loop {
        if Instant::now() >= deadline {
            return Err(Error::rejected(format!(
                "wiki cp job {job_id} index was not covered before the command deadline"
            )));
        }
        let mut saw_entry_error = false;
        let mut offset = 0u64;
        let mut last = json!({});
        loop {
            let page = channel.transfer(
                root,
                json!({
                    "op": "status",
                    "job_id": job_id,
                    "root": root,
                    "offset": offset,
                    "limit": STATUS_PAGE,
                }),
                false,
            )?;
            if page["job_id"].as_str().is_some_and(|id| id != job_id) {
                return Err(Error::rejected(
                    "wiki cp status returned a different job — not this command's receipt",
                ));
            }
            if let Some(entries) = page["entries"].as_array() {
                for entry in entries {
                    if status_entry_failed(entry) {
                        saw_entry_error = true;
                    }
                }
            }
            last = page;
            match last["next_offset"].as_u64() {
                Some(next) if next != offset => offset = next,
                _ => break,
            }
        }
        match last["index"].as_str().unwrap_or("pending") {
            "covered" if !saw_entry_error => return Ok(last),
            "covered" => {
                return Err(Error::rejected(format!(
                    "wiki cp job {job_id} index is covered but an entry failed"
                )))
            }
            "superseded" => {
                return Err(Error::rejected(format!(
                    "wiki cp job {job_id} index is superseded"
                )))
            }
            "pending" => {}
            other => {
                return Err(Error::rejected(format!(
                    "wiki cp job {job_id} index status '{other}'"
                )))
            }
        }
        if last["state"].as_str() == Some("failed") || saw_entry_error {
            return Err(Error::rejected(format!(
                "wiki cp job {job_id} did not become index-ready"
            )));
        }
        let slice = Duration::from_millis(250);
        if deadline.saturating_duration_since(Instant::now()) <= slice {
            return Err(Error::rejected(format!(
                "wiki cp job {job_id} index was not covered before the command deadline"
            )));
        }
        std::thread::sleep(slice);
    }
}

struct EntryProgress {
    received: u64,
    decided: bool,
}

/// `pending` and `unresolved` still need apply. `unresolved` means the
/// publication is not committed and the stage was kept; it is not a
/// decision, so resume may apply once. Any other outcome is refused.
fn outcome_is_decided(outcome: Option<&str>) -> Result<bool> {
    match outcome {
        Some("pending" | "unresolved") => Ok(false),
        Some("applied" | "skipped" | "conflict" | "failed") => Ok(true),
        _ => Err(Error::rejected(
            "wiki cp progress has an unknown entry outcome",
        )),
    }
}

fn status_received(
    channel: &mut dyn Channel,
    root: &str,
    job_id: &str,
) -> Result<std::collections::BTreeMap<String, EntryProgress>> {
    let mut out = std::collections::BTreeMap::new();
    let mut offset = 0u64;
    loop {
        let page = channel.transfer(
            root,
            json!({"op": "status", "job_id": job_id, "root": root, "offset": offset, "limit": STATUS_PAGE}),
            false,
        )?;
        if page["root"].as_str() != Some(root) || page["job_id"].as_str() != Some(job_id) {
            return Err(Error::rejected(
                "wiki cp progress belongs to a different job or root",
            ));
        }
        let entries = page["entries"]
            .as_array()
            .filter(|entries| entries.len() <= STATUS_PAGE as usize)
            .ok_or_else(|| Error::rejected("wiki cp progress has an invalid entry page"))?;
        for entry in entries {
            let id = entry["entry_id"]
                .as_str()
                .ok_or_else(|| Error::rejected("wiki cp progress omitted entry identity"))?;
            let received = entry["received"]
                .as_u64()
                .ok_or_else(|| Error::rejected("wiki cp progress omitted acknowledged bytes"))?;
            let decided = outcome_is_decided(entry["outcome"].as_str())?;
            if out
                .insert(id.to_string(), EntryProgress { received, decided })
                .is_some()
                || out.len() > MAX_ENTRIES
            {
                return Err(Error::rejected(
                    "wiki cp progress repeats or exceeds its admitted entries",
                ));
            }
        }
        match page.get("next_offset") {
            Some(Value::Null) => break,
            Some(next)
                if next.as_u64() == Some(offset + entries.len() as u64) && !entries.is_empty() =>
            {
                offset += entries.len() as u64;
            }
            _ => return Err(Error::rejected("wiki cp progress has invalid pagination")),
        }
    }
    Ok(out)
}

struct Walked {
    rel: String,
    kind: String,
    size: u64,
    sha256: Option<String>,
    single_file: bool,
}

fn walk_local(root: &Path, file_name: Option<&str>) -> Result<Vec<Walked>> {
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

fn push_file(out: &mut Vec<Walked>, path: &Path, rel: &str, single_file: bool) -> Result<()> {
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

fn check_manifest(manifest: &Manifest) -> Result<()> {
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

fn check_export_bounds(entries: &[ExportEntry]) -> Result<()> {
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

fn is_real_dir(path: &Path) -> bool {
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

fn parse_kind(raw: &str) -> Result<String> {
    match raw {
        "dir" | "text" | "blob" => Ok(raw.to_string()),
        _ => Err(Error::rejected(format!(
            "wiki cp refused unsupported entry kind '{raw}'"
        ))),
    }
}

fn entry_failure(body: &Value, path: &str) -> Option<String> {
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
        Some("applied" | "skipped") => None,
        Some("conflict" | "failed" | "error") => Some(format!(
            "{path}: {}",
            body["outcome"].as_str().unwrap_or("failed")
        )),
        Some(other) => Some(format!("{path}: {other}")),
        None if body.get("error").is_some() => Some(format!("{path}: error")),
        None => Some(format!("{path}: apply omitted outcome")),
    }
}

fn status_entry_failed(body: &Value) -> bool {
    matches!(
        body["outcome"].as_str(),
        Some("conflict" | "failed" | "error")
    ) || body.get("conflict").is_some()
        || matches!(
            body["extraction"].as_str(),
            Some("failed" | "empty" | "conflict")
        )
}

fn job_failure(body: &Value) -> Option<String> {
    if body["index"] == "superseded" {
        return Some("job destination was superseded; no newer content was overwritten".into());
    }
    match body["state"].as_str() {
        Some("complete") => None,
        Some(other) => Some(format!("job state {other}")),
        None => Some("finish omitted state".to_string()),
    }
}

fn source_identity(kind: &str, path: &Path) -> Result<String> {
    let abs = lexical_abs(path)?;
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(abs.to_string_lossy().as_bytes());
    let id = hex_encode(hasher.finalize());
    check_token(&id, "source id")?;
    Ok(id)
}

fn entry_id(rel: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(rel.as_bytes());
    hex_encode(hasher.finalize())
}

fn plan_digest(entries: &[ExportEntry]) -> String {
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

fn wiki_operand(raw: &str) -> Option<&str> {
    raw.strip_prefix("wiki:")
}

fn canonical_wiki(path: &str) -> Result<String> {
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

fn split_wiki(path: &str) -> Result<(String, String)> {
    match path.rsplit_once('/') {
        Some((parent, name)) if !parent.is_empty() && !name.is_empty() => {
            Ok((canonical_wiki(parent)?, name.to_string()))
        }
        _ => Err(Error::rejected(
            "wiki cp file destination needs a parent directory — nothing was sent",
        )),
    }
}

fn join_wiki(root: &str, rel: &str) -> String {
    if rel.is_empty() {
        root.to_string()
    } else if root.is_empty() {
        rel.to_string()
    } else {
        format!("{root}/{rel}")
    }
}

fn check_token(value: &str, what: &str) -> Result<()> {
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

fn required_str(body: &Value, key: &str) -> Result<String> {
    body[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Error::rejected(format!("wiki cp response omitted {key}")))
}

fn optional_sha(body: &Value) -> Result<Option<String>> {
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

fn is_absent(error: &Error) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    text.contains("no such") || text.contains("not found") || text.contains("does not exist")
}

/// Bounded `read` refuses a directory (`a range read needs a text page`).
/// That is the directory case of discovery and an update pin, not a missing file.
fn is_directory_range(error: &Error) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    text.contains("range") && text.contains("director")
}

/// File discovery is the existing bounded read. A directory (range refusal,
/// or an explicit `kind: dir`) is enumerated with the ordinary list action.
/// Absence is success so a new import root does not require a parent read.
fn discover_read_or_list(read: Result<Value>, list: impl FnOnce() -> Result<Value>) -> Result<()> {
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

fn refused_path(path: &Path, why: &str) -> Error {
    Error::rejected(format!(
        "wiki cp refused '{}': {why} — nothing was sent",
        path.display()
    ))
}

fn guard_local(path: &Path, _write: bool) -> Result<PathBuf> {
    let abs = lexical_abs(path)?;
    if crate::master::caller_is_master() {
        let cwd = lexical_abs(&std::env::current_dir().map_err(|e| {
            Error::rejected(format!("wiki cp cannot resolve the working directory: {e}"))
        })?)?;
        if !abs.starts_with(&cwd) {
            return Err(Error::rejected(format!(
                "wiki cp refused '{}': outside the registered working directory",
                path.display()
            )));
        }
    }
    Ok(abs)
}

fn lexical_abs(path: &Path) -> Result<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut out = PathBuf::new();
    for component in abs.components() {
        match component {
            Component::RootDir => out.push(component),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Err(Error::rejected(format!(
                        "wiki cp refused '{}'",
                        path.display()
                    )));
                }
            }
            Component::Normal(seg) => out.push(seg),
            Component::Prefix(_) => {
                return Err(Error::rejected(format!(
                    "wiki cp refused '{}'",
                    path.display()
                )))
            }
        }
    }
    Ok(out)
}

fn symlink_meta(path: &Path) -> Result<fs::Metadata> {
    fs::symlink_metadata(path)
        .map_err(|e| Error::rejected(format!("wiki cp cannot stat {}: {e}", path.display())))
}

fn path_utf8(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| Error::rejected("wiki cp refused a non-UTF-8 path"))
}

/// Metadata of `path` after every existing ancestor, including the leaf,
/// has been opened with `O_NOFOLLOW`. A missing path is an error. This
/// is the import admission check: nothing is hashed or listed first.
fn contained_source_meta(path: &Path) -> Result<fs::Metadata> {
    let file = open_contained(path)?;
    file.metadata()
        .map_err(|e| Error::rejected(format!("wiki cp cannot stat {}: {e}", path.display())))
}

/// Open `path` by walking from the filesystem root with `openat` and
/// `O_NOFOLLOW` on every component. The returned fd is the only handle;
/// a later path open is not used for source bytes.
fn open_contained(path: &Path) -> Result<File> {
    refuse_symlink_chain(path)?;
    let mut current = open_root_dir()?;
    let mut cur = PathBuf::new();
    cur.push(Component::RootDir);
    let abs = lexical_abs(path)?;
    for component in abs.components() {
        let Component::Normal(seg) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(refused_path(path, "unsupported path"));
        };
        cur.push(seg);
        let is_last = cur == abs;
        let name = CString::new(os_str_bytes(seg)).map_err(|_| {
            Error::rejected(format!(
                "wiki cp refused '{}': path contains an interior nul — nothing was sent",
                path.display()
            ))
        })?;
        // Ancestors are directories, so they must not use O_NONBLOCK
        // (some kernels return ENOTSUP). The leaf uses O_NONBLOCK so a
        // fifo cannot stall admission; a directory leaf that a kernel
        // refuses that way is retried below. Symlinks stay ELOOP.
        let flags = if is_last {
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK
        } else {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        };
        let mut next = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if next < 0 && is_last {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ENOTSUP) {
                let dir_flags =
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
                next = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), dir_flags) };
            }
        }
        if next < 0 {
            let err = std::io::Error::last_os_error();
            // Linux reports a symlink as ELOOP. A kernel that returns the
            // link descriptor is still refused by fstat below.
            if err.raw_os_error() == Some(libc::ELOOP) || is_symlink_at(current.as_raw_fd(), &name)
            {
                return Err(refused_path(&cur, "symlink"));
            }
            return Err(Error::rejected(format!(
                "wiki cp cannot read {}: {err}",
                cur.display()
            )));
        }
        let opened = unsafe { OwnedFd::from_raw_fd(next) };
        if is_symlink_fd(opened.as_raw_fd()) {
            return Err(refused_path(&cur, "symlink"));
        }
        current = opened;
    }
    let file = File::from(current);
    let meta = file
        .metadata()
        .map_err(|e| Error::rejected(format!("wiki cp cannot stat {}: {e}", path.display())))?;
    if meta.file_type().is_symlink() {
        return Err(refused_path(path, "symlink"));
    }
    if !meta.is_file() && !meta.is_dir() {
        return Err(refused_path(path, "not a regular file or directory"));
    }
    Ok(file)
}

fn open_root_dir() -> Result<OwnedFd> {
    let fd = unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(Error::rejected(
            "wiki cp cannot open the filesystem root — nothing was sent",
        ));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn os_str_bytes(seg: &std::ffi::OsStr) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    seg.as_bytes()
}

fn is_symlink_fd(fd: std::os::fd::RawFd) -> bool {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstat(fd, &mut st) };
    rc == 0 && (st.st_mode & libc::S_IFMT) == libc::S_IFLNK
}

fn is_symlink_at(dirfd: std::os::fd::RawFd, name: &std::ffi::CStr) -> bool {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstatat(dirfd, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
    rc == 0 && (st.st_mode & libc::S_IFMT) == libc::S_IFLNK
}

/// Names in a directory fd from [`open_contained`]. `fdopendir` consumes
/// `file`. `.` and `..` are omitted. The caller's path is not opened again.
fn list_nofollow_dir(file: File, path: &Path) -> Result<Vec<std::ffi::OsString>> {
    use std::ffi::{CStr, OsStr};
    use std::os::fd::IntoRawFd;
    use std::os::unix::ffi::OsStrExt;

    struct CloseDir(*mut libc::DIR);
    impl Drop for CloseDir {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { libc::closedir(self.0) };
            }
        }
    }

    let fd = file.into_raw_fd();
    let dir = unsafe { libc::fdopendir(fd) };
    if dir.is_null() {
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(Error::rejected(format!(
            "wiki cp cannot read {}: {err}",
            path.display()
        )));
    }
    let dir = CloseDir(dir);
    let mut names = Vec::new();
    loop {
        clear_errno();
        let ent = unsafe { libc::readdir(dir.0) };
        if ent.is_null() {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error().unwrap_or(0) != 0 {
                return Err(Error::rejected(format!(
                    "wiki cp cannot read {}: {err}",
                    path.display()
                )));
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        names.push(OsStr::from_bytes(bytes).to_os_string());
    }
    Ok(names)
}

fn clear_errno() {
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "redox"))]
    unsafe {
        *libc::__errno_location() = 0;
    }
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    unsafe {
        *libc::__error() = 0;
    }
}

/// Private export stage in a held directory fd. `Drop` unlinks only the
/// stage name. After `renameat` that name is already gone, so cleanup
/// cannot remove the published file.
struct PrivateStage {
    dir: OwnedFd,
    name: CString,
    file: File,
}

impl Drop for PrivateStage {
    fn drop(&mut self) {
        unsafe {
            libc::unlinkat(self.dir.as_raw_fd(), self.name.as_ptr(), 0);
        }
    }
}

impl PrivateStage {
    fn create(dir: &File, display: &Path) -> Result<Self> {
        let flags =
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let mut last = None;
        for _ in 0..8 {
            let label = format!(".cadence-transfer-{}", hex_encode(stage_nonce()));
            let name = CString::new(label).map_err(|_| {
                Error::rejected(format!(
                    "cannot stage {}: bad stage name",
                    display.display()
                ))
            })?;
            let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags, 0o600) };
            if fd >= 0 {
                let dup = unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
                if dup < 0 {
                    let err = std::io::Error::last_os_error();
                    unsafe {
                        libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0);
                        libc::close(fd);
                    }
                    return Err(Error::rejected(format!(
                        "cannot stage {}: {err}",
                        display.display()
                    )));
                }
                return Ok(Self {
                    dir: unsafe { OwnedFd::from_raw_fd(dup) },
                    name,
                    file: File::from(unsafe { OwnedFd::from_raw_fd(fd) }),
                });
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EEXIST) {
                return Err(Error::rejected(format!(
                    "cannot stage {}: {err}",
                    display.display()
                )));
            }
            last = Some(err);
        }
        Err(Error::rejected(format!(
            "cannot stage {}: {}",
            display.display(),
            last.map(|err| err.to_string())
                .unwrap_or_else(|| "export stage was not created".to_string())
        )))
    }
}

fn stage_nonce() -> [u8; 16] {
    let mut nonce = [0u8; 16];
    if getrandom::fill(&mut nonce).is_ok() {
        return nonce;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let mixed = (u128::from(std::process::id()) << 96) ^ nanos;
    nonce.copy_from_slice(&mixed.to_ne_bytes());
    nonce
}

/// Create-only links the stage under `dest` and leaves an existing name
/// untouched. `--update` renames onto that name. Both operations use the
/// held directory fd, never a reopened path.
fn commit_stage(
    stage: &PrivateStage,
    dest: &std::ffi::CStr,
    update: bool,
    display: &Path,
) -> Result<()> {
    let rc = if update {
        unsafe {
            libc::renameat(
                stage.dir.as_raw_fd(),
                stage.name.as_ptr(),
                stage.dir.as_raw_fd(),
                dest.as_ptr(),
            )
        }
    } else {
        unsafe {
            libc::linkat(
                stage.dir.as_raw_fd(),
                stage.name.as_ptr(),
                stage.dir.as_raw_fd(),
                dest.as_ptr(),
                0,
            )
        }
    };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        if !update
            && (err.kind() == std::io::ErrorKind::AlreadyExists
                || err.raw_os_error() == Some(libc::EEXIST))
        {
            return Err(Error::rejected(format!(
                "wiki cp refused to overwrite '{}' — pass --update",
                display.display()
            )));
        }
        return Err(Error::rejected(format!(
            "cannot commit {}: {err}",
            display.display()
        )));
    }
    if !update {
        unsafe {
            libc::unlinkat(stage.dir.as_raw_fd(), stage.name.as_ptr(), 0);
        }
    }
    Ok(())
}

fn hash_open_file(file: &mut File) -> Result<(u64, String)> {
    file.rewind()?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((size, hex_encode(hasher.finalize())))
}

struct ExportAnchor {
    dir: File,
    root: PathBuf,
    single_name: Option<CString>,
}

impl ExportAnchor {
    /// Hold the directory that will contain published names. A single file
    /// whose destination is not already a real directory is published in
    /// its parent; every other plan publishes inside `dest`.
    fn open(dest: &Path, scope: &str, entries: &[ExportEntry]) -> Result<Self> {
        let single = entries.len() == 1 && entries[0].path.is_empty();
        if single && !is_real_dir(dest) {
            let parent = match dest.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent,
                _ => Path::new("/"),
            };
            let leaf = dest
                .file_name()
                .ok_or_else(|| refused_path(dest, "unsupported path"))?;
            return Ok(Self {
                dir: hold_export_dir(parent)?,
                root: parent.to_path_buf(),
                single_name: Some(leaf_name(leaf, dest)?),
            });
        }
        let single_name = if single {
            let base = scope
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or("export");
            Some(leaf_name(std::ffi::OsStr::new(base), dest)?)
        } else {
            None
        };
        Ok(Self {
            dir: hold_export_dir(dest)?,
            root: dest.to_path_buf(),
            single_name,
        })
    }
}

/// Walk `path` from `/` with the same `O_NOFOLLOW` directory opens as
/// [`open_contained`]. Missing components are created with `mkdirat` on
/// the held parent. The returned fd, not the path, is the destination.
fn hold_export_dir(path: &Path) -> Result<File> {
    let abs = lexical_abs(path)?;
    let mut current = File::from(open_root_dir()?);
    let mut built = PathBuf::new();
    built.push(Component::RootDir);
    for component in abs.components() {
        let Component::Normal(seg) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(refused_path(path, "unsupported path"));
        };
        let name = component_cstr(seg, path)?;
        built.push(seg);
        current = open_or_create_dir(&current, &name, &built)?;
    }
    Ok(current)
}

fn hold_relative_dir(root: &File, rel: &Path) -> Result<File> {
    if rel.as_os_str().is_empty() {
        return root.try_clone().map_err(|err| {
            Error::rejected(format!("wiki cp cannot hold export directory: {err}"))
        });
    }
    let mut current = root
        .try_clone()
        .map_err(|err| Error::rejected(format!("wiki cp cannot hold export directory: {err}")))?;
    let mut built = PathBuf::new();
    for component in rel.components() {
        let Component::Normal(seg) = component else {
            return Err(refused_path(rel, "unsupported path"));
        };
        let bytes = os_str_bytes(seg);
        if bytes.is_empty()
            || bytes == b"."
            || bytes == b".."
            || bytes.starts_with(b".")
            || bytes.contains(&b'/')
            || bytes.contains(&0)
        {
            return Err(refused_path(rel, "unsupported path"));
        }
        let name = component_cstr(seg, rel)?;
        built.push(seg);
        current = open_or_create_dir(&current, &name, &built)?;
    }
    Ok(current)
}

enum DirFail {
    Missing,
    Symlink,
    NotDir,
    Io(std::io::Error),
}

fn open_dir_at(parent: &File, name: &std::ffi::CStr) -> std::result::Result<File, DirFail> {
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ELOOP) || is_symlink_at(parent.as_raw_fd(), name) {
            return Err(DirFail::Symlink);
        }
        if err.raw_os_error() == Some(libc::ENOENT) {
            return Err(DirFail::Missing);
        }
        if err.raw_os_error() == Some(libc::ENOTDIR) {
            return Err(DirFail::NotDir);
        }
        return Err(DirFail::Io(err));
    }
    let file = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    if is_symlink_fd(file.as_raw_fd()) {
        return Err(DirFail::Symlink);
    }
    match file.metadata() {
        Ok(meta) if meta.is_dir() => Ok(file),
        Ok(_) => Err(DirFail::NotDir),
        Err(err) => Err(DirFail::Io(err)),
    }
}

fn open_or_create_dir(parent: &File, name: &std::ffi::CStr, display: &Path) -> Result<File> {
    match open_dir_at(parent, name) {
        Ok(file) => Ok(file),
        Err(DirFail::Missing) => {
            let rc = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o777) };
            let created = rc == 0;
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::EEXIST) {
                    return Err(Error::rejected(format!(
                        "wiki cp cannot create {}: {err}",
                        display.display()
                    )));
                }
            }
            let child = match open_dir_at(parent, name) {
                Ok(file) => file,
                Err(DirFail::Symlink) => return Err(refused_path(display, "symlink")),
                Err(DirFail::NotDir) => return Err(refused_path(display, "not a directory")),
                Err(DirFail::Missing) => {
                    return Err(Error::rejected(format!(
                        "wiki cp cannot create {}",
                        display.display()
                    )))
                }
                Err(DirFail::Io(err)) => {
                    return Err(Error::rejected(format!(
                        "wiki cp cannot create {}: {err}",
                        display.display()
                    )))
                }
            };
            if created {
                parent.sync_all().map_err(|err| {
                    Error::rejected(format!("wiki cp cannot sync {}: {err}", display.display()))
                })?;
            }
            Ok(child)
        }
        Err(DirFail::Symlink) => Err(refused_path(display, "symlink")),
        Err(DirFail::NotDir) => Err(refused_path(display, "not a directory")),
        Err(DirFail::Io(err)) => Err(Error::rejected(format!(
            "wiki cp cannot open {}: {err}",
            display.display()
        ))),
    }
}

fn component_cstr(seg: &std::ffi::OsStr, display: &Path) -> Result<CString> {
    CString::new(os_str_bytes(seg)).map_err(|_| {
        Error::rejected(format!(
            "wiki cp refused '{}': path contains an interior nul — nothing was sent",
            display.display()
        ))
    })
}

fn leaf_name(seg: &std::ffi::OsStr, display: &Path) -> Result<CString> {
    let bytes = os_str_bytes(seg);
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&0) {
        return Err(refused_path(display, "unsupported path"));
    }
    component_cstr(seg, display)
}

fn refuse_export_leaf(
    dir: &File,
    name: &std::ffi::CStr,
    display: &Path,
    update: bool,
) -> Result<()> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOENT) {
            return Ok(());
        }
        return Err(Error::rejected(format!(
            "wiki cp cannot stat {}: {err}",
            display.display()
        )));
    }
    let kind = st.st_mode & libc::S_IFMT;
    if kind == libc::S_IFLNK {
        return Err(refused_path(display, "symlink"));
    }
    if kind != libc::S_IFREG {
        return Err(refused_path(display, "not a regular file"));
    }
    if !update {
        return Err(Error::rejected(format!(
            "wiki cp refused to overwrite '{}' — pass --update",
            display.display()
        )));
    }
    Ok(())
}

fn refuse_symlink_chain(path: &Path) -> Result<()> {
    let mut cur = PathBuf::new();
    if path.is_absolute() {
        cur.push(Component::RootDir);
    }
    let parent = path.parent().unwrap_or(path);
    for component in parent.components() {
        match component {
            Component::RootDir => {}
            Component::CurDir => continue,
            Component::ParentDir => {
                return Err(refused_path(path, "path escapes its parent"));
            }
            Component::Normal(_) => cur.push(component),
            Component::Prefix(_) => return Err(refused_path(path, "unsupported path")),
        }
        if cur.as_os_str().is_empty() {
            continue;
        }
        if let Ok(meta) = fs::symlink_metadata(&cur) {
            if meta.file_type().is_symlink() {
                return Err(refused_path(&cur, "symlink"));
            }
        }
    }
    Ok(())
}

fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_preserves_nested_empty_entries_and_refuses_symlinks() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("empty")).unwrap();
        fs::write(root.path().join("note.txt"), "").unwrap();
        fs::write(root.path().join("blob.bin"), b"abc").unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        fs::write(root.path().join("nested/child.txt"), b"nested").unwrap();
        std::os::unix::fs::symlink("note.txt", root.path().join("link.txt")).unwrap();
        assert!(walk_local(root.path(), None).is_err());

        fs::remove_file(root.path().join("link.txt")).unwrap();
        let first = walk_local(root.path(), None).unwrap();
        let rels: Vec<_> = first.iter().map(|e| e.rel.clone()).collect();
        assert_eq!(
            rels,
            vec![
                "blob.bin".to_string(),
                "empty".to_string(),
                "nested".to_string(),
                "nested/child.txt".to_string(),
                "note.txt".to_string()
            ]
        );
        assert_eq!(first[1].kind, "dir");
        assert_eq!(first[1].sha256, None);
        assert_eq!(first[2].kind, "dir");
        assert_eq!(first[4].kind, "text");
        assert_eq!(first[4].size, 0);
        assert_eq!(
            first[4].sha256.as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(first[0].kind, "blob");
    }

    /// `./linked/subdir` must be refused before any child is hashed, even
    /// when the leaf directory itself is a real directory outside cwd.
    #[test]
    fn source_ancestor_symlink_is_refused_before_discovery() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(outside.path().join("subdir")).unwrap();
        fs::write(outside.path().join("subdir/secret.txt"), b"outside-cwd").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
        let linked = root.path().join("linked/subdir");

        let err = match walk_local(&linked, None) {
            Ok(_) => panic!("symlinked ancestor was walked"),
            Err(error) => error.to_string(),
        };
        assert!(!err.contains("outside-cwd"), "{err}");

        assert!(contained_source_meta(&linked).is_err());
        let secret = linked.join("secret.txt");
        let mut admitted = Vec::new();
        let err = push_file(&mut admitted, &secret, "secret.txt", true)
            .unwrap_err()
            .to_string();
        assert!(admitted.is_empty(), "{err}");
        assert!(!err.contains("outside-cwd"), "{err}");

        fs::create_dir(root.path().join("real")).unwrap();
        fs::create_dir(root.path().join("real/empty")).unwrap();
        fs::write(root.path().join("real/note.txt"), "café").unwrap();
        let walked = walk_local(&root.path().join("real"), None).unwrap();
        let rels: Vec<_> = walked.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(rels, vec!["empty", "note.txt"]);
        assert_eq!(walked[0].kind, "dir");
        assert_eq!(walked[1].kind, "text");
    }

    /// A destination created after preflight keeps its bytes unless `--update`.
    #[test]
    fn export_create_only_refuses_a_destination_created_during_download() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("page.txt");
        let raced = b"created-after-preflight";
        let entry = ExportEntry {
            path: "page.txt".to_string(),
            kind: "text".to_string(),
            size: 4,
            sha256: Some(
                "12a435ec8454c6d1c90a1d92812b09af11bee711fbe524d56a8f26ea7c5ccee8".to_string(),
            ),
            rev: "fnv1a:0123456789abcdef".to_string(),
        };
        let listed = [entry.clone(), empty_dir_entry()];
        // Preflight sees no destination. The channel plants one on the first read.
        check_export_dest(dir.path(), "global/docs", &listed, false).unwrap();
        assert!(!dest.exists());
        write_export_entry(
            &mut RaceChannel {
                dest: dest.clone(),
                raced: raced.to_vec(),
            },
            "global/docs",
            dir.path(),
            &listed[0],
            &listed,
            false,
        )
        .unwrap_err();
        assert_eq!(fs::read(&dest).unwrap(), raced);

        fs::remove_file(&dest).unwrap();
        write_export_entry(
            &mut RaceChannel {
                dest: dest.clone(),
                raced: raced.to_vec(),
            },
            "global/docs",
            dir.path(),
            &listed[0],
            &listed,
            true,
        )
        .unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"wiki");
        assert!(
            fs::read_dir(dir.path()).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".cadence-transfer-")),
            "failed create-only persist left its private stage"
        );
    }

    fn empty_dir_entry() -> ExportEntry {
        ExportEntry {
            path: "empty".to_string(),
            kind: "dir".to_string(),
            size: 0,
            sha256: None,
            rev: ABSENT.to_string(),
        }
    }

    struct RaceChannel {
        dest: PathBuf,
        raced: Vec<u8>,
    }

    impl Channel for RaceChannel {
        fn transfer(&mut self, _root: &str, _op: Value, _write: bool) -> Result<Value> {
            if !self.dest.exists() {
                fs::write(&self.dest, &self.raced).unwrap();
            }
            Ok(json!({
                "kind": "text",
                "rev": "fnv1a:0123456789abcdef",
                "size": 4,
                "sha256": "12a435ec8454c6d1c90a1d92812b09af11bee711fbe524d56a8f26ea7c5ccee8",
                "data_base64": base64::engine::general_purpose::STANDARD.encode(b"wiki"),
            }))
        }
        fn discover(&mut self, _path: &str) -> Result<()> {
            Ok(())
        }
        fn current_rev(&mut self, _path: &str) -> Result<String> {
            Ok(ABSENT.to_string())
        }
    }

    #[test]
    fn unresolved_status_is_retryable_and_unknown_outcome_is_refused() {
        struct Page {
            outcome: &'static str,
        }
        impl Channel for Page {
            fn transfer(&mut self, root: &str, op: Value, _write: bool) -> Result<Value> {
                assert_eq!(op["op"], "status");
                Ok(json!({
                    "root": root,
                    "job_id": "job-unresolved",
                    "entries": [{
                        "entry_id": "note",
                        "received": 5,
                        "outcome": self.outcome,
                    }],
                    "next_offset": null,
                }))
            }
            fn discover(&mut self, _path: &str) -> Result<()> {
                Ok(())
            }
            fn current_rev(&mut self, _path: &str) -> Result<String> {
                Err(Error::rejected(
                    "current_rev recomputed an admitted revision",
                ))
            }
        }
        let mut unresolved = Page {
            outcome: "unresolved",
        };
        let progress = status_received(&mut unresolved, "global/notes", "job-unresolved").unwrap();
        assert!(!progress["note"].decided);
        let mut unknown = Page { outcome: "guessed" };
        assert!(status_received(&mut unknown, "global/notes", "job-unresolved").is_err());
    }

    fn wiki_slice() -> Value {
        json!({
            "kind": "text",
            "rev": "fnv1a:0123456789abcdef",
            "size": 4,
            "sha256": "12a435ec8454c6d1c90a1d92812b09af11bee711fbe524d56a8f26ea7c5ccee8",
            "data_base64": base64::engine::general_purpose::STANDARD.encode(b"wiki"),
        })
    }

    fn text_entry(path: &str) -> ExportEntry {
        ExportEntry {
            path: path.to_string(),
            kind: "text".to_string(),
            size: 4,
            sha256: Some(
                "12a435ec8454c6d1c90a1d92812b09af11bee711fbe524d56a8f26ea7c5ccee8".to_string(),
            ),
            rev: "fnv1a:0123456789abcdef".to_string(),
        }
    }

    fn stage_leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| {
                let name = entry.unwrap().file_name();
                let text = name.to_string_lossy().into_owned();
                text.starts_with(".cadence-transfer-").then_some(text)
            })
            .collect()
    }

    /// The admitted directory is renamed to a symlink during the download.
    /// Publication stays on the original inode: create-only leaves raced
    /// bytes, and `--update` replaces only those bytes.
    #[test]
    fn export_parent_symlink_swap_cannot_publish_outside() {
        struct Swap {
            dest: PathBuf,
            outside: PathBuf,
            real: PathBuf,
            plant: Option<Vec<u8>>,
            swapped: bool,
        }
        impl Channel for Swap {
            fn transfer(&mut self, _root: &str, _op: Value, _write: bool) -> Result<Value> {
                if !self.swapped {
                    fs::rename(&self.dest, &self.real).unwrap();
                    std::os::unix::fs::symlink(&self.outside, &self.dest).unwrap();
                    if let Some(bytes) = &self.plant {
                        fs::write(self.real.join("page.txt"), bytes).unwrap();
                    }
                    self.swapped = true;
                }
                Ok(wiki_slice())
            }
            fn discover(&mut self, _path: &str) -> Result<()> {
                Ok(())
            }
            fn current_rev(&mut self, _path: &str) -> Result<String> {
                Ok(ABSENT.to_string())
            }
        }

        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("dest");
        let outside = root.path().join("outside");
        let real = root.path().join("real");
        fs::create_dir(&dest).unwrap();
        fs::create_dir(&outside).unwrap();
        let entry = text_entry("page.txt");
        let listed = [entry.clone()];
        check_export_dest(&dest, "global/docs", &listed, false).unwrap();
        let raced = b"created-after-preflight";
        write_export_entry(
            &mut Swap {
                dest: dest.clone(),
                outside: outside.clone(),
                real: real.clone(),
                plant: Some(raced.to_vec()),
                swapped: false,
            },
            "global/docs",
            &dest,
            &entry,
            &listed,
            false,
        )
        .unwrap_err();
        assert!(dest.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(fs::read(real.join("page.txt")).unwrap(), raced);
        assert!(!outside.join("page.txt").exists());
        assert!(stage_leftovers(&real).is_empty());
        assert!(stage_leftovers(&outside).is_empty());

        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("dest");
        let outside = root.path().join("outside");
        let real = root.path().join("real");
        fs::create_dir(&dest).unwrap();
        fs::create_dir(&outside).unwrap();
        write_export_entry(
            &mut Swap {
                dest: dest.clone(),
                outside: outside.clone(),
                real: real.clone(),
                plant: None,
                swapped: false,
            },
            "global/docs",
            &dest,
            &entry,
            &listed,
            true,
        )
        .unwrap();
        assert!(dest.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(fs::read(real.join("page.txt")).unwrap(), b"wiki");
        assert!(!outside.join("page.txt").exists());
        assert!(stage_leftovers(&real).is_empty());
        assert!(stage_leftovers(&outside).is_empty());
    }

    /// A child directory is created on the held parent, then its path is
    /// replaced with a symlink before publication. A destination that is
    /// already a symlink is not a place to create directories.
    #[test]
    fn export_created_directory_does_not_follow_replaced_parent() {
        struct SwapChild {
            dest: PathBuf,
            outside: PathBuf,
            swapped: bool,
        }
        impl Channel for SwapChild {
            fn transfer(&mut self, _root: &str, _op: Value, _write: bool) -> Result<Value> {
                if !self.swapped {
                    let nested = self.dest.join("nested");
                    let saved = self.dest.join("nested-real");
                    fs::rename(&nested, &saved).unwrap();
                    std::os::unix::fs::symlink(&self.outside, &nested).unwrap();
                    self.swapped = true;
                }
                Ok(wiki_slice())
            }
            fn discover(&mut self, _path: &str) -> Result<()> {
                Ok(())
            }
            fn current_rev(&mut self, _path: &str) -> Result<String> {
                Ok(ABSENT.to_string())
            }
        }

        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("dest");
        let outside = root.path().join("outside");
        fs::create_dir(&dest).unwrap();
        fs::create_dir(&outside).unwrap();
        let entry = text_entry("nested/page.txt");
        let listed = [entry.clone()];
        check_export_dest(&dest, "global/docs", &listed, true).unwrap();
        write_export_entry(
            &mut SwapChild {
                dest: dest.clone(),
                outside: outside.clone(),
                swapped: false,
            },
            "global/docs",
            &dest,
            &entry,
            &listed,
            true,
        )
        .unwrap();
        assert!(dest
            .join("nested")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read(dest.join("nested-real/page.txt")).unwrap(),
            b"wiki"
        );
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        assert!(stage_leftovers(&dest.join("nested-real")).is_empty());

        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("dest");
        let outside = root.path().join("outside");
        let real = root.path().join("real");
        fs::create_dir(&dest).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::rename(&dest, &real).unwrap();
        std::os::unix::fs::symlink(&outside, &dest).unwrap();
        struct Idle(u32);
        impl Channel for Idle {
            fn transfer(&mut self, _root: &str, _op: Value, _write: bool) -> Result<Value> {
                self.0 += 1;
                Err(Error::rejected(
                    "transfer ran after a symlinked destination",
                ))
            }
            fn discover(&mut self, _path: &str) -> Result<()> {
                Ok(())
            }
            fn current_rev(&mut self, _path: &str) -> Result<String> {
                Ok(ABSENT.to_string())
            }
        }
        let dir_entry = ExportEntry {
            path: "nested".to_string(),
            kind: "dir".to_string(),
            size: 0,
            sha256: None,
            rev: ABSENT.to_string(),
        };
        let listed = [dir_entry];
        let mut idle = Idle(0);
        write_export_entry(&mut idle, "global/docs", &dest, &listed[0], &listed, false)
            .unwrap_err();
        assert_eq!(idle.0, 0);
        assert!(!outside.join("nested").exists());
        assert!(!real.join("nested").exists());
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
    }
}
