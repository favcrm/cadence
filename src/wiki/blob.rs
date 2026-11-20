//! Content-addressed wiki blob storage (`.blobs/<sha256>`), MIME sniffing,
//! atomic no-overwrite publication, and bounded range reads.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    allowed, blobs_dir, classify, ensure_layout, json_string_len, logical_kind, normalize,
    oversize, restore, rev_of, vault_dir, BodyRange, Caller, Kind, Op, View,
    NATIVE_TOOL_LINE_BOUND, TEXT_CAP,
};
use crate::error::{Error, Result};
use crate::issue::Pm;

/// `<state>/wiki-uploads/`. `put_blob` accepts a `tmp` only under it.
pub const UPLOAD_DIR: &str = "wiki-uploads";

pub(super) struct BlobView<'a> {
    path: &'a str,
    sha256: &'a str,
    size: u64,
    mime: &'a str,
    name: &'a str,
    rev: &'a str,
}

fn blob_str<'a>(meta: &'a Value, key: &str, path: &str) -> Result<&'a str> {
    meta.get(key).and_then(Value::as_str).ok_or_else(|| {
        Error::rejected(format!(
            "wiki read '{path}' refused: blob pointer '{key}' is not a string"
        ))
    })
}

pub(super) fn blob_view(meta: &Value) -> Result<BlobView<'_>> {
    let path = blob_str(meta, "path", "")?;
    let sha256 = blob_str(meta, "sha256", path)?;
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(Error::rejected(format!(
            "wiki read '{path}' refused: blob pointer sha256 is not 64 lowercase hex digits"
        )));
    }
    let size = meta.get("size").and_then(Value::as_u64).ok_or_else(|| {
        Error::rejected(format!(
            "wiki read '{path}' refused: blob pointer size is not a non-negative integer"
        ))
    })?;
    Ok(BlobView {
        mime: blob_str(meta, "mime", path)?,
        name: blob_str(meta, "name", path)?,
        rev: blob_str(meta, "rev", path)?,
        path,
        sha256,
        size,
    })
}

fn blob_ranged_value(view: &BlobView<'_>, start: u64, end: u64, data_base64: String) -> Value {
    let mut value = json!({
        "path": view.path,
        "kind": "blob",
        "sha256": view.sha256,
        "size": view.size,
        "mime": view.mime,
        "name": view.name,
        "rev": view.rev,
        "range": {"start": start, "end": end, "total": view.size},
    });
    value["data_base64"] = Value::String(data_base64);
    value
}

fn b64_len(n: usize) -> usize {
    n.div_ceil(3).saturating_mul(4)
}

fn decimal_len_u64(n: u64) -> usize {
    if n == 0 {
        1
    } else {
        (n.ilog10() as usize) + 1
    }
}

/// Compact length of [`blob_ranged_value`]. Base64 uses the standard
/// alphabet, which needs no JSON escape. Key order matches `json!`.
fn blob_line_len(view: &BlobView<'_>, start: u64, end: u64) -> usize {
    let Ok(n) = usize::try_from(end.saturating_sub(start)) else {
        return usize::MAX;
    };
    if n >= NATIVE_TOOL_LINE_BOUND {
        return usize::MAX;
    }
    "{\"data_base64\":".len()
        + 2
        + b64_len(n)
        + ",\"kind\":\"blob\",\"mime\":".len()
        + json_string_len(view.mime)
        + ",\"name\":".len()
        + json_string_len(view.name)
        + ",\"path\":".len()
        + json_string_len(view.path)
        + ",\"range\":{\"end\":".len()
        + decimal_len_u64(end)
        + ",\"start\":".len()
        + decimal_len_u64(start)
        + ",\"total\":".len()
        + decimal_len_u64(view.size)
        + "},\"rev\":".len()
        + json_string_len(view.rev)
        + ",\"sha256\":".len()
        + json_string_len(view.sha256)
        + ",\"size\":".len()
        + decimal_len_u64(view.size)
        + "}".len()
}

fn blob_fits(view: &BlobView<'_>, start: u64, end: u64) -> bool {
    if end < start {
        return false;
    }
    let len = blob_line_len(view, start, end);
    len.checked_add(1)
        .is_some_and(|n| n < NATIVE_TOOL_LINE_BOUND)
}

fn blob_end(view: &BlobView<'_>, start: u64, bytes: Option<u64>) -> Result<u64> {
    let total = view.size;
    let end = match bytes {
        Some(n) => start.saturating_add(n).min(total),
        None => {
            let mut lo = start;
            let mut hi = total;
            let mut best = None;
            while lo <= hi {
                let mid = lo + (hi - lo) / 2;
                if blob_fits(view, start, mid) {
                    best = Some(mid);
                    if mid == total {
                        break;
                    }
                    lo = mid + 1;
                } else if mid == start {
                    break;
                } else {
                    hi = mid - 1;
                }
            }
            best.ok_or_else(|| oversize(view.path))?
        }
    };
    if !blob_fits(view, start, end) {
        return Err(oversize(view.path));
    }
    Ok(end)
}

pub(super) fn blob_read_range(pm: &Pm, meta: &Value, range: BodyRange<'_>) -> Result<Value> {
    let view = blob_view(meta)?;
    if let Some(want) = range.rev {
        if want != view.rev {
            return Err(Error::rejected(format!(
                "wiki read '{}' refused: revision '{want}' is not the original \
                 revision '{}' — ranges are not combined across revisions",
                view.path, view.rev
            )));
        }
    }
    if range.offset > view.size {
        return Err(Error::rejected(format!(
            "wiki read '{}' refused: offset {} is past the original {} bytes",
            view.path, range.offset, view.size
        )));
    }
    let start = range.offset;
    let end = blob_end(&view, start, range.bytes)?;
    let bytes = read_blob_slice(pm, &view, start, end)?;
    let data_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let value = blob_ranged_value(&view, start, end, data_base64);
    debug_assert_eq!(
        blob_line_len(&view, start, end),
        serde_json::to_string(&value)
            .map(|encoded| encoded.len())
            .unwrap_or(usize::MAX)
    );
    Ok(value)
}

/// Read only the requested slice of an immutable, content-addressed object.
/// Upload verifies its digest; remote cat verifies the assembled bytes once.
pub(super) fn read_blob_slice(
    pm: &Pm,
    view: &BlobView<'_>,
    start: u64,
    end: u64,
) -> Result<Vec<u8>> {
    let vault = vault_dir(pm)?;
    let blobs = blobs_dir(&vault);
    match blobs.symlink_metadata() {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(Error::rejected(format!(
                "wiki read '{}' refused: blob store is not a real directory",
                view.path
            )))
        }
        Err(e) => {
            return Err(Error::rejected(format!(
                "wiki read '{}' refused: blob store is unavailable: {e}",
                view.path
            )))
        }
    }
    let object = blobs.join(view.sha256);
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&object)
        .map_err(|_| {
            Error::rejected(format!(
                "wiki read '{}' refused: blob object is not a regular file",
                view.path
            ))
        })?;
    let meta = file.metadata().map_err(|e| {
        Error::rejected(format!(
            "wiki read '{}' refused: blob object is unreadable: {e}",
            view.path
        ))
    })?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(Error::rejected(format!(
            "wiki read '{}' refused: blob object is not a regular file",
            view.path
        )));
    }
    if meta.len() != view.size {
        return Err(Error::rejected(format!(
            "wiki read '{}' refused: blob object is {} bytes, pointer size is {}",
            view.path,
            meta.len(),
            view.size
        )));
    }
    let len = usize::try_from(end - start).unwrap_or(usize::MAX);
    let mut buf = vec![0u8; len];
    if len > 0 {
        file.seek(SeekFrom::Start(start)).map_err(|e| {
            Error::rejected(format!(
                "wiki read '{}' refused: blob object is unreadable: {e}",
                view.path
            ))
        })?;
        file.read_exact(&mut buf).map_err(|_| {
            Error::rejected(format!(
                "wiki read '{}' refused: blob object is shorter than its pointer",
                view.path
            ))
        })?;
    }
    Ok(buf)
}

/// sha256 of a file, lowercase hex.
pub(super) fn sha256_file(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    io::copy(&mut f, &mut h)?;
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// A first-block MIME guess: magic bytes for the kinds the board
/// streams; UTF-8 → text/plain; else octet-stream. The pointer file
/// records it; the board's serve rules decide inline vs attachment.
pub(crate) fn sniff_mime(head: &[u8]) -> &'static str {
    match head {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xff, 0xd8, 0xff, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [b'R', b'I', b'F', b'F', ..] if head.len() >= 12 && &head[8..12] == b"WEBP" => "image/webp",
        [b'%', b'P', b'D', b'F', ..] => "application/pdf",
        [b'P', b'K', 0x03, 0x04, ..] => "application/zip",
        [0x00, 0x00, 0x00, ..] if head.len() >= 12 && &head[4..8] == b"ftyp" => "video/mp4",
        [0x1a, 0x45, 0xdf, 0xa3, ..] => "video/webm",
        _ => {
            if let Ok(text) = std::str::from_utf8(head) {
                let lower = text.trim_start().to_lowercase();
                if lower.starts_with("<?xml") && lower.contains("<svg") || lower.starts_with("<svg")
                {
                    "image/svg+xml"
                } else if lower.starts_with("<!doctype html") || lower.starts_with("<html") {
                    "text/html"
                } else {
                    "text/plain"
                }
            } else {
                "application/octet-stream"
            }
        }
    }
}

/// `wiki_put_blob` — verify a staged upload and land it: re-hash the
/// tmp file (the caller's `sha256` is advisory and must match),
/// enforce the cap BEFORE the move, sniff MIME, secret-scan text
/// content, then publish the bytes at `.blobs/<sha256>` and commit the
/// `<path>.blob` pointer. The tracker write lock is taken only for
/// the pointer's commit — never while the upload is being checked.
///
/// Blob bytes are immutable and content-addressed: publication stages
/// the verified tmp to a unique sibling on the blob filesystem and
/// then links it in under the hash name without ever overwriting or
/// partially exposing an existing object. A name collision is not
/// trusted — the existing entry must prove it is a confined regular
/// file holding exactly the upload's sha256 bytes before the upload
/// dedupes to it, else the call refuses. Once staged this call NEVER
/// unlinks `.blobs/<sha256>` — another pointer, history or a trash
/// entry may already reference it; a refusal removes only this call's
/// own unpublished staging file (unreferenced blob cleanup is GC
/// work, not this op's).
///
/// `tmp` must sit under `<state_dir>/wiki-uploads/` — a tmp anywhere
/// else is refused (the daemon never renames an arbitrary caller
/// path into the vault). `if_rev` is the pointer's
/// optimistic-concurrency token (`none` = the page must not exist).
pub fn put_blob(
    pm: &Pm,
    state_dir: &Path,
    caller: &Caller,
    path: &str,
    tmp: &Path,
    sha256: Option<&str>,
    if_rev: Option<&str>,
) -> Result<Value> {
    let norm = normalize(path)?;
    if norm.is_empty() {
        return Err(Error::rejected(
            "wiki put_blob '' refused: the root is a directory",
        ));
    }
    let segs: Vec<&str> = norm.split('/').collect();
    allowed(pm, caller, Op::Write, &segs)?;
    if classify(&segs) != View::Real {
        unreachable!()
    }
    let vault = vault_dir(pm)?;

    // The tmp must live under <state>/wiki-uploads/ — canonicalized
    // so `..` and symlink tricks cannot escape the check.
    let uploads = state_dir.join(UPLOAD_DIR);
    let uploads_canon = uploads.canonicalize().unwrap_or_else(|_| uploads.clone());
    let tmp_canon = tmp
        .canonicalize()
        .map_err(|e| Error::rejected(format!("wiki put_blob tmp {}: {e}", tmp.display())))?;
    if tmp_canon.parent() != Some(uploads_canon.as_path()) {
        return Err(Error::rejected(format!(
            "wiki put_blob refused: tmp must be a file directly under {}",
            uploads.display()
        )));
    }
    let meta = tmp_canon.symlink_metadata()?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(Error::rejected(
            "wiki put_blob refused: tmp is not a regular file",
        ));
    }
    let cap = pm.config.wiki.max_upload_bytes;
    if meta.len() > cap {
        let _ = fs::remove_file(&tmp_canon);
        return Err(Error::rejected(format!(
            "wiki put_blob '{norm}' refused: {} bytes over the {cap}-byte cap",
            meta.len()
        )));
    }
    let actual = sha256_file(&tmp_canon)?;
    if let Some(want) = sha256 {
        if !want.eq_ignore_ascii_case(&actual) {
            let _ = fs::remove_file(&tmp_canon);
            return Err(Error::rejected(format!(
                "wiki put_blob '{norm}' refused: sha256 mismatch (got {actual})"
            )));
        }
    }
    // MIME sniff + secret scan on text content. Read a bounded head
    // for sniffing; the scan runs on the full text when it is one.
    let mut head = vec![0u8; 8192.min(meta.len() as usize)];
    File::open(&tmp_canon)?.read_exact(&mut head)?;
    let mime = sniff_mime(&head);
    let mut secret_warnings = Vec::new();
    if mime == "text/plain" && meta.len() <= TEXT_CAP {
        if let Ok(text) = fs::read_to_string(&tmp_canon) {
            secret_warnings = crate::secret::guard(&format!("wiki/{norm}"), &text)?;
        }
    }

    // Land the blob: stage the verified tmp to a unique sibling name
    // on `.blobs/`'s own filesystem, then publish it at `<sha256>`
    // atomically and without overwrite. A partial copy never appears
    // under the hash name; an already-published object is verified
    // byte-for-byte before it is deduped — never blindly trusted by
    // name and never rewritten. From here on this call may only
    // unlink `stage` — `dest` is shared content another pointer may
    // already reference.
    ensure_layout(pm)?;
    let blobs = blobs_dir(&vault);
    let stage = blobs.join(format!(
        ".{actual}.upload-{}",
        uuid::Uuid::new_v4().simple()
    ));
    match fs::hard_link(&tmp_canon, &stage) {
        Ok(()) => {
            let _ = fs::remove_file(&tmp_canon);
        }
        Err(e) if e.kind() == ErrorKind::CrossesDevices => {
            // The state dir and the vault may live on different
            // mounts: copy bytes onto the blob filesystem. A failed
            // copy leaves the tmp in place and `dest` untouched — but
            // the private `stage` it may have partially written is
            // ours alone, so it is always removed.
            if let Err(e) = fs::copy(&tmp_canon, &stage) {
                let _ = fs::remove_file(&stage);
                return Err(e.into());
            }
            let _ = fs::remove_file(&tmp_canon);
        }
        Err(e) => return Err(e.into()),
    }
    let dest = blobs.join(&actual);
    publish_blob(&stage, &dest)?;
    // CAD-911 test seam — compiled ONLY into test builds (`cfg(test)`;
    // shipped daemons and libraries never carry it): a unit test that
    // armed this thread parks HERE — blob published, pointer lock not
    // yet taken, no lock held while parked — to interleave a second
    // real put_blob deterministically. Unarmed it is a no-op.
    #[cfg(test)]
    super::test_pause::at_publish_seam();

    // The pointer file — a normal tracker text write under the lock.
    let pointer = vault.join(format!("{norm}.blob"));
    let _lock = pm.lock()?;
    let gitignore = ensure_layout(pm)?;
    match logical_kind(&vault, &norm)? {
        Kind::Text => {
            return Err(Error::rejected(format!(
                "wiki put_blob '{norm}' refused: a text page exists there — rm it first"
            )))
        }
        Kind::Dir => {
            return Err(Error::rejected(format!(
                "wiki put_blob '{norm}' refused: a directory exists there"
            )))
        }
        Kind::None | Kind::Blob => {}
    }
    if let Some(parent) = pointer.parent() {
        fs::create_dir_all(parent)?;
    }
    let cur = rev_of(&pointer)?;
    if let Some(want) = if_rev {
        if want != cur {
            // Conflict. `publish_blob` already consumed this call's
            // private stage; `dest` is shared custody another pointer
            // may already reference — nothing here unlinks it.
            return Ok(json!({"conflict": "if_rev", "current_rev": cur, "path": norm}));
        }
    }
    let name = norm.rsplit('/').next().unwrap_or(&norm).to_string();
    let pointer_text = serde_json::to_string_pretty(&json!({
        "sha256": actual,
        "size": meta.len(),
        "mime": mime,
        "name": name,
    }))?;
    let before = fs::read(&pointer).ok();
    fs::write(&pointer, &pointer_text)?;
    let mut paths = vec![pointer.clone()];
    if let Some(g) = gitignore {
        paths.push(g);
    }
    let actor = caller.actor();
    if let Err(e) = crate::issue::write::commit_who(
        pm,
        &paths,
        &format!("wiki: put_blob {norm}"),
        &[],
        &actor,
        None,
    ) {
        restore(&pointer, before);
        return Err(e);
    }
    let mut out = json!({
        "path": norm,
        "kind": "blob",
        "sha256": actual,
        "size": meta.len(),
        "mime": mime,
        "name": name,
        "rev": rev_of(&pointer)?,
    });
    if !secret_warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&secret_warnings);
    }
    Ok(out)
}

/// Publish staged blob bytes at `dest` — atomically, never over an
/// existing object, and never trusting a name as proof of content.
/// `stage` already sits on `dest`'s filesystem (it is the unique
/// sibling name `put_blob` staged onto `.blobs/` itself), so a
/// `hard_link` lands the complete bytes at the hash name in one
/// atomic, no-overwrite step.
///
/// `AlreadyExists` is a collision claim, not a byte proof: the
/// existing entry is verified to be a confined regular non-symlink
/// file whose size and sha256 match the hash name exactly before the
/// upload is deduped to it. Foreign bytes, a directory, a symlink or
/// any size/hash mismatch refuse the upload — the existing object is
/// left byte-for-byte as found, no pointer is committed by this
/// caller, and only the private `stage` name is removed. A
/// `hard_link` that lands means `stage`'s own verified bytes now own
/// the name, so no re-check is needed.
///
/// Publication is link-based on purpose: it makes the no-overwrite
/// guarantee atomic on every filesystem the vault deploys to here
/// (Linux ext4/tmpfs). A filesystem without hard links cannot promise
/// that, so `publish_blob` surfaces the link error rather than
/// falling back to a copy or a rename that could overwrite or expose
/// partial bytes at the hash name.
fn publish_blob(stage: &Path, dest: &Path) -> Result<()> {
    match fs::hard_link(stage, dest) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            verify_existing_blob(stage, dest)?;
        }
        Err(e) => {
            // The staging name is ours alone — never leave it behind.
            let _ = fs::remove_file(stage);
            return Err(e.into());
        }
    }
    // Whether the link landed or the existing object verified as the
    // same content, our private staging name is ours alone — removed.
    let _ = fs::remove_file(stage);
    Ok(())
}

/// The `AlreadyExists` half of [`publish_blob`]: prove the entry at
/// `dest` is a regular, non-symlink file holding exactly `stage`'s
/// bytes — the same sha256 the name claims — before the upload
/// dedupes to it. Any other shape or content refuses. `stage` is
/// removed on EVERY failing exit — refusal or I/O error alike —
/// because it is this call's private name, while `dest` is shared
/// custody a failure must never touch.
fn verify_existing_blob(stage: &Path, dest: &Path) -> Result<()> {
    let out = verify_existing_blob_inner(stage, dest);
    if out.is_err() {
        let _ = fs::remove_file(stage);
    }
    out
}

fn verify_existing_blob_inner(stage: &Path, dest: &Path) -> Result<()> {
    let refuse = |why: String| -> Error {
        Error::rejected(format!(
            "wiki put_blob refused: existing blob object {} {why}",
            dest.display()
        ))
    };
    let meta = dest.symlink_metadata()?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(refuse("is not a regular file".to_string()));
    }
    let staged = stage.symlink_metadata()?;
    if meta.len() != staged.len() {
        return Err(refuse(format!(
            "holds {} bytes, expected {}",
            meta.len(),
            staged.len()
        )));
    }
    if sha256_file(dest)? != sha256_file(stage)? {
        return Err(refuse("content does not match its hash name".to_string()));
    }
    Ok(())
}
