//! Entry publication, git commit proof, blob custody, and PDF extraction reconciliation.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

use super::super::{
    blobs_dir, ingest, mkdir, put_blob, read, rev_bytes, vault_dir, write, Caller, ABSENT,
    UPLOAD_DIR,
};
use super::{missing, open_nofollow, reject, sha256_bytes, sha256_file, stage_path};
use crate::error::{Error, Result};
use crate::issue::Pm;
use crate::store::wiki_transfers::{EntryOutcome, EntryRow};
use crate::store::Store;

pub(super) struct Published {
    pub(super) outcome: &'static str,
    pub(super) rev: Option<String>,
    pub(super) extraction: &'static str,
    pub(super) extraction_rev: Option<String>,
    pub(super) source_hash: Option<String>,
    pub(super) detail: Option<String>,
}

pub(super) fn publish_entry(
    store: &Store,
    pm: &Pm,
    state_dir: &Path,
    caller: &Caller,
    job_id: &str,
    entry: &EntryRow,
) -> Result<Published> {
    if entry.outcome == "unresolved" {
        return reconcile_fenced(pm, caller, entry);
    }
    if entry.kind == "dir" {
        return publish_dir(store, pm, caller, job_id, entry);
    }
    match publication(pm, caller, entry)? {
        Publication::Committed(rev) => return reconcile_match(pm, caller, entry, rev),
        Publication::Uncommitted => {
            note_attempt(store, job_id, entry)?;
            return Ok(unresolved(entry));
        }
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
        return publish_text(store, pm, state_dir, caller, job_id, entry);
    }
    publish_blob(store, pm, state_dir, caller, job_id, entry)
}

fn publish_text(
    store: &Store,
    pm: &Pm,
    state_dir: &Path,
    caller: &Caller,
    job_id: &str,
    entry: &EntryRow,
) -> Result<Published> {
    let bytes = fs::read(stage_path(state_dir, job_id, &entry.entry_id))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        reject(format!(
            "wiki transfer apply refused: '{}' is not UTF-8 text",
            entry.path
        ))
    })?;
    note_attempt(store, job_id, entry)?;
    let wrote = match write(pm, caller, &entry.path, text, Some(&entry.if_rev)) {
        Ok(value) => value,
        Err(error) => return classify_publish_error(pm, caller, entry, &error),
    };
    if wrote.get("conflict").is_some() {
        return classify_publish_conflict(pm, caller, entry, &wrote);
    }
    let Some(rev) = committed_content_rev(pm, entry)? else {
        return Ok(unresolved(entry));
    };
    Ok(Published {
        outcome: "applied",
        rev: Some(rev),
        extraction: "not_applicable",
        extraction_rev: None,
        source_hash: entry.sha256.clone(),
        detail: None,
    })
}

fn publish_dir(
    store: &Store,
    pm: &Pm,
    caller: &Caller,
    job_id: &str,
    entry: &EntryRow,
) -> Result<Published> {
    match publication(pm, caller, entry)? {
        Publication::Committed(_) => {
            return Ok(Published {
                outcome: "skipped",
                rev: None,
                extraction: "not_applicable",
                extraction_rev: None,
                source_hash: None,
                detail: None,
            });
        }
        Publication::Uncommitted => {
            note_attempt(store, job_id, entry)?;
            return Ok(unresolved(entry));
        }
        Publication::Absent => {}
    }
    match read(pm, caller, &entry.path) {
        Ok(page) if page["kind"] == "dir" => {
            note_attempt(store, job_id, entry)?;
            return Ok(unresolved(entry));
        }
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
    note_attempt(store, job_id, entry)?;
    let made = match mkdir(pm, caller, &entry.path) {
        Ok(value) => value,
        Err(error) => {
            return Ok(Published {
                outcome: "unresolved",
                rev: None,
                extraction: "not_applicable",
                extraction_rev: None,
                source_hash: None,
                detail: Some(format!(
                    "wiki transfer publication could not be proved ({error})"
                )),
            });
        }
    };
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

pub(super) enum Publication {
    /// Revision is computed from the same committed bytes that proved the
    /// manifest hash; a later HEAD must not substitute another file's revision.
    Committed(Option<String>),
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

/// Durable boundary before `write`, `mkdir`, or `put_blob`. A crash after
/// this row is indistinguishable from an unproved writer result, so resume
/// cannot treat it as a fresh pending entry.
pub(super) fn note_attempt(store: &Store, job_id: &str, entry: &EntryRow) -> Result<()> {
    store.wiki_transfer_record_outcome(
        job_id,
        &entry.entry_id,
        &EntryOutcome {
            outcome: "unresolved",
            rev: None,
            extraction: Some("not_applicable"),
            extraction_rev: None,
            source_hash: entry.sha256.as_deref(),
            detail: Some("wiki transfer publication attempt is unproved"),
        },
    )
}

/// An already-fenced attempt may accept only a committed matching proof.
/// Absence, a matching worktree, and a still-valid `if_rev` do not publish.
pub(super) fn reconcile_fenced(pm: &Pm, caller: &Caller, entry: &EntryRow) -> Result<Published> {
    match publication(pm, caller, entry)? {
        Publication::Committed(_) if entry.kind == "dir" => Ok(Published {
            outcome: "skipped",
            rev: None,
            extraction: "not_applicable",
            extraction_rev: None,
            source_hash: None,
            detail: None,
        }),
        Publication::Committed(rev) => reconcile_match(pm, caller, entry, rev),
        Publication::Uncommitted | Publication::Absent => Ok(unresolved(entry)),
    }
}

pub(super) fn publication(pm: &Pm, caller: &Caller, entry: &EntryRow) -> Result<Publication> {
    if entry.kind == "dir" {
        if committed_dir(pm, &entry.path)? {
            return Ok(Publication::Committed(None));
        }
        return match read(pm, caller, &entry.path) {
            Ok(page) if page["kind"] == "dir" => Ok(Publication::Uncommitted),
            Ok(_) => Ok(Publication::Absent),
            Err(error) if missing(&error) => Ok(Publication::Absent),
            Err(error) => Err(error),
        };
    }
    if let Some(rev) = committed_content_rev(pm, entry)? {
        return Ok(Publication::Committed(Some(rev)));
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
        Publication::Committed(rev) => reconcile_match(pm, caller, entry, rev),
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
        Ok(Publication::Committed(rev)) => return reconcile_match(pm, caller, entry, rev),
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

fn git_output(pm: &Pm, args: &[&str]) -> Result<Output> {
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
pub(super) fn committed_dir(pm: &Pm, vault_rel: &str) -> Result<bool> {
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
        Some(bytes) => rev_bytes(&bytes),
        None => ABSENT.to_string(),
    })
}

fn committed_content_rev(pm: &Pm, entry: &EntryRow) -> Result<Option<String>> {
    match entry.kind.as_str() {
        "text" => {
            let Some(bytes) = committed_blob(pm, &entry.path)? else {
                return Ok(None);
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                return Ok(None);
            };
            Ok((text.len() as u64 == entry.size
                && Some(sha256_bytes(text.as_bytes()).as_str()) == entry.sha256.as_deref())
            .then(|| rev_bytes(&bytes)))
        }
        "blob" => {
            let Some(bytes) = committed_blob(pm, &format!("{}.blob", entry.path))? else {
                return Ok(None);
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                return Ok(None);
            };
            let Ok(meta) = serde_json::from_str::<Value>(text) else {
                return Ok(None);
            };
            if meta["size"].as_u64() != Some(entry.size)
                || meta["sha256"].as_str() != entry.sha256.as_deref()
            {
                return Ok(None);
            }
            let hash = entry
                .sha256
                .as_deref()
                .ok_or_else(|| reject("missing transfer hash"))?;
            verify_blob_custody(pm, hash, entry.size)?;
            Ok(Some(rev_bytes(&bytes)))
        }
        _ => Ok(None),
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

pub(super) fn page_matches(page: &Value, kind: &str, size: u64, hash: Option<&str>) -> bool {
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

pub(super) fn reconcile_match(
    pm: &Pm,
    caller: &Caller,
    entry: &EntryRow,
    rev: Option<String>,
) -> Result<Published> {
    if entry.kind == "blob" && committed_blob_is_pdf(pm, caller, &entry.path)? {
        let hash = entry
            .sha256
            .as_deref()
            .ok_or_else(|| reject("missing transfer hash"))?;
        let (extraction, extraction_rev, detail) = extraction_of(pm, caller, &entry.path, hash);
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

pub(super) fn blob_is_pdf(page: &Value) -> bool {
    page["mime"].as_str() == Some("application/pdf")
}

fn committed_blob_is_pdf(pm: &Pm, caller: &Caller, path: &str) -> Result<bool> {
    Ok(blob_is_pdf(&read(pm, caller, path)?))
}

/// The generated extraction header is the only provenance. A matching body
/// marker is not a source hash or status. `None` means this header does not
/// name the source at this hash: apply then derives from the raw file only
/// when the header names that source, and index coverage stays unproved.
/// An exact header whose status is not `indexed`, `empty`, or `failed` is
/// `conflict` and is not retried.
pub(super) fn generated_extraction_status(
    text: &str,
    source_path: &str,
    sha: &str,
) -> Option<&'static str> {
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
        Some("indexed") => "indexed",
        Some("empty") => "empty",
        Some("conflict") => "conflict",
        _ => "failed",
    };
    let rev = read(pm, caller, &sidecar)
        .ok()
        .and_then(|page| page["rev"].as_str().map(str::to_string));
    let detail = result["reason"].as_str().map(str::to_string);
    (status, rev, detail)
}

fn publish_blob(
    store: &Store,
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
    if let Err(error) = note_attempt(store, job_id, entry) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
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
        return classify_publish_conflict(pm, caller, entry, &wrote);
    }
    let Some(rev) = committed_content_rev(pm, entry)? else {
        return Ok(unresolved(entry));
    };
    let hash = entry
        .sha256
        .as_deref()
        .ok_or_else(|| reject("missing transfer hash"))?;
    let pdf = match wrote["mime"].as_str() {
        Some(mime) => mime == "application/pdf",
        None => committed_blob_is_pdf(pm, caller, &entry.path)?,
    };
    let (extraction, extraction_rev, detail) = if pdf {
        extraction_of(pm, caller, &entry.path, hash)
    } else {
        ("not_applicable", None, None)
    };
    Ok(Published {
        outcome: "applied",
        rev: Some(rev),
        extraction,
        extraction_rev,
        source_hash: entry.sha256.clone(),
        detail,
    })
}
