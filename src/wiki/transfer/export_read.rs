//! File bodies for `{op:export}`.
//!
//! Directory pages stay in the parent. A real directory is `Ok(None)`;
//! missing paths, symlinks, and unreadable files stay errors.
//!
//! Text revisions and SHA-256 come from one read of one file descriptor,
//! through [`super::super::rev_bytes`] and [`super::sha256_bytes`]. The
//! process keeps a small metadata cache (revision, digest, and the fd
//! stamp) and serves later slices with a bounded read. The cache stores
//! no page bytes, no descriptors, and no permission decisions. Every
//! call checks the caller and resolves the path again.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};

use super::super::{
    allowed, blob_view, classify, memory_file, normalize, profile_file, read_blob_slice, resolve,
    rev_bytes, vault_dir, Caller, Op, View, TEXT_CAP, VIEW_CAP,
};
use super::{open_nofollow, reject, sha256_bytes, sha_ok, MAX_CHUNK_BYTES};
use crate::error::Result;
use crate::issue::Pm;

/// How many validated text stamps to remember. Each entry is a path,
/// a revision, a digest, and the fd stamp. Not the file.
const META_CACHE_MAX: usize = 32;
/// Pointer files are small JSON. Refuse anything larger before parsing.
const POINTER_CAP: u64 = 64 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Utf8Failure {
    /// Real wiki text: the historical "not UTF-8 (is it a blob?)" refusal.
    BlobHint,
    /// Profile and memory views: the historical read_to_string message.
    Stream,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

struct MetaEntry {
    path: PathBuf,
    stamp: Stamp,
    rev: String,
    sha256: String,
}

enum Located {
    Dir,
    Text {
        norm: String,
        abs: PathBuf,
        cap: u64,
        utf8: Utf8Failure,
    },
    Blob {
        norm: String,
        pointer: PathBuf,
    },
}

struct Identity {
    rev: String,
    sha256: String,
    size: u64,
    /// Whole file from the miss that just hashed it. Never cached.
    body: Option<Vec<u8>>,
}

fn meta_cache() -> &'static Mutex<VecDeque<MetaEntry>> {
    static CACHE: OnceLock<Mutex<VecDeque<MetaEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(VecDeque::with_capacity(META_CACHE_MAX)))
}

fn cache_lock() -> std::sync::MutexGuard<'static, VecDeque<MetaEntry>> {
    meta_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn cached(path: &Path, stamp: &Stamp) -> Option<(String, String)> {
    let mut guard = cache_lock();
    let pos = guard
        .iter()
        .position(|entry| entry.path == path && entry.stamp == *stamp)?;
    let entry = guard.remove(pos)?;
    let found = (entry.rev.clone(), entry.sha256.clone());
    guard.push_back(entry);
    Some(found)
}

fn remember(path: PathBuf, stamp: Stamp, rev: String, sha256: String) {
    let mut guard = cache_lock();
    guard.retain(|entry| entry.path != path);
    while guard.len() >= META_CACHE_MAX {
        guard.pop_front();
    }
    guard.push_back(MetaEntry {
        path,
        stamp,
        rev,
        sha256,
    });
}

fn stamp_of(meta: &std::fs::Metadata) -> Stamp {
    Stamp {
        dev: meta.dev(),
        ino: meta.ino(),
        size: meta.len(),
        mtime: meta.mtime(),
        mtime_nsec: meta.mtime_nsec(),
        ctime: meta.ctime(),
        ctime_nsec: meta.ctime_nsec(),
    }
}

fn changed() -> crate::error::Error {
    reject("wiki transfer export refused: the file changed while it was being read")
}

fn not_file(norm: &str) -> crate::error::Error {
    reject(format!("wiki read '{norm}': not a file"))
}

fn missing(norm: &str) -> crate::error::Error {
    reject(format!("wiki read '{norm}': no such file"))
}

fn over_cap(norm: &str, cap: u64) -> crate::error::Error {
    reject(format!(
        "wiki read '{norm}' refused: file over the {cap}-byte cap"
    ))
}

fn not_utf8(norm: &str, kind: Utf8Failure) -> crate::error::Error {
    match kind {
        Utf8Failure::BlobHint => reject(format!(
            "wiki read '{norm}' refused: not UTF-8 text (is it a blob?)"
        )),
        Utf8Failure::Stream => reject(format!(
            "wiki read '{norm}': stream did not contain valid UTF-8"
        )),
    }
}

fn rev_mismatch(pinned: &str, live: &str) -> crate::error::Error {
    reject(format!(
        "wiki transfer export refused: revision '{pinned}' is not '{live}'"
    ))
}

/// Fd stamp and path stamp. A replacement changes the path inode while
/// the already-open descriptor still names the old file; both must match.
fn paired_stamp(file: &File, path: &Path) -> Result<Stamp> {
    let fd_meta = file.metadata()?;
    if fd_meta.file_type().is_symlink() || !fd_meta.is_file() {
        return Err(changed());
    }
    let path_meta = path.symlink_metadata().map_err(|_| changed())?;
    if path_meta.file_type().is_symlink() || !path_meta.is_file() {
        return Err(changed());
    }
    let fd_stamp = stamp_of(&fd_meta);
    let path_stamp = stamp_of(&path_meta);
    if fd_stamp != path_stamp {
        return Err(changed());
    }
    Ok(fd_stamp)
}

fn confirm(file: &File, path: &Path, expected: &Stamp) -> Result<()> {
    if paired_stamp(file, path)? != *expected {
        return Err(changed());
    }
    Ok(())
}

fn want_ok(want: u64) -> Result<()> {
    if want == 0 || want > MAX_CHUNK_BYTES as u64 {
        return Err(reject(format!(
            "wiki transfer export refused: bytes must be 1 to {MAX_CHUNK_BYTES}"
        )));
    }
    Ok(())
}

fn locate(pm: &Pm, caller: &Caller, path: &str) -> Result<Located> {
    let norm = normalize(path)?;
    if norm.is_empty() {
        return Err(reject("wiki read '' refused: the root is a directory"));
    }
    let segs: Vec<&str> = norm.split('/').collect();
    allowed(caller, Op::Read, &segs)?;
    match classify(&segs) {
        View::Profile { alias } => {
            if segs.len() != 4 {
                return Err(reject(format!(
                    "wiki read '{norm}' refused: profile/ reads are profile/<file>"
                )));
            }
            let abs = profile_file(pm, &alias, segs[3])?;
            Ok(Located::Text {
                norm,
                abs,
                cap: VIEW_CAP,
                utf8: Utf8Failure::Stream,
            })
        }
        View::Memory { alias } => {
            if segs.len() != 5 {
                return Err(reject(format!(
                    "wiki read '{norm}' refused: memory/ reads are \
                     memory/<project>/<lesson>.md"
                )));
            }
            let abs = memory_file(pm, &alias, segs[3], segs[4])?;
            Ok(Located::Text {
                norm,
                abs,
                cap: TEXT_CAP,
                utf8: Utf8Failure::Stream,
            })
        }
        View::Real => locate_real(pm, norm),
    }
}

fn locate_real(pm: &Pm, norm: String) -> Result<Located> {
    let vault = vault_dir(pm)?;
    let abs = resolve(&vault, &norm)?;
    match abs.symlink_metadata() {
        Ok(meta) if meta.file_type().is_symlink() => Err(not_file(&norm)),
        Ok(meta) if meta.is_dir() => Ok(Located::Dir),
        Ok(meta) if meta.is_file() => Ok(Located::Text {
            norm,
            abs,
            cap: TEXT_CAP,
            utf8: Utf8Failure::BlobHint,
        }),
        Ok(_) => Err(not_file(&norm)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let pointer = vault.join(format!("{norm}.blob"));
            match pointer.symlink_metadata() {
                Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
                    Ok(Located::Blob { norm, pointer })
                }
                _ => Err(missing(&norm)),
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn read_fd(file: &mut File, path: &Path, size: u64, cap: u64, norm: &str) -> Result<Vec<u8>> {
    if size > cap {
        return Err(over_cap(norm, cap));
    }
    let n = usize::try_from(size).map_err(|_| over_cap(norm, cap))?;
    let mut buf = vec![0u8; n];
    if n > 0 {
        file.seek(SeekFrom::Start(0))?;
        if let Err(error) = file.read_exact(&mut buf) {
            if paired_stamp(file, path).is_err() {
                return Err(changed());
            }
            return Err(error.into());
        }
    }
    Ok(buf)
}

fn text_identity(
    file: &mut File,
    abs: &Path,
    norm: &str,
    cap: u64,
    utf8: Utf8Failure,
    before: Stamp,
) -> Result<Identity> {
    if before.size > cap {
        return Err(over_cap(norm, cap));
    }
    if let Some((rev, sha256)) = cached(abs, &before) {
        return Ok(Identity {
            rev,
            sha256,
            size: before.size,
            body: None,
        });
    }
    let body = read_fd(file, abs, before.size, cap, norm)?;
    confirm(file, abs, &before)?;
    if std::str::from_utf8(&body).is_err() {
        return Err(not_utf8(norm, utf8));
    }
    let rev = rev_bytes(&body);
    let sha256 = sha256_bytes(&body);
    confirm(file, abs, &before)?;
    remember(abs.to_path_buf(), before, rev.clone(), sha256.clone());
    Ok(Identity {
        rev,
        sha256,
        size: before.size,
        body: Some(body),
    })
}

fn text_slice(
    file: &mut File,
    abs: &Path,
    before: &Stamp,
    ident: &Identity,
    offset: u64,
    want: u64,
) -> Result<Vec<u8>> {
    if offset > ident.size {
        return Err(reject(
            "wiki transfer export refused: offset is past the text",
        ));
    }
    let end = offset.saturating_add(want).min(ident.size);
    let start = usize::try_from(offset)
        .map_err(|_| reject("wiki transfer export refused: offset is past the text"))?;
    let end_at = usize::try_from(end)
        .map_err(|_| reject("wiki transfer export refused: offset is past the text"))?;
    confirm(file, abs, before)?;
    let slice = if let Some(body) = ident.body.as_ref() {
        body[start..end_at].to_vec()
    } else {
        let mut buf = vec![0u8; end_at - start];
        if !buf.is_empty() {
            file.seek(SeekFrom::Start(offset))?;
            if let Err(error) = file.read_exact(&mut buf) {
                if confirm(file, abs, before).is_err() {
                    return Err(changed());
                }
                return Err(error.into());
            }
        }
        buf
    };
    confirm(file, abs, before)?;
    Ok(slice)
}

fn text_value(
    abs: &Path,
    norm: &str,
    cap: u64,
    utf8: Utf8Failure,
    pinned: Option<&str>,
    range: Option<(u64, u64)>,
) -> Result<Option<Value>> {
    let mut file = open_nofollow(abs)?;
    let fd_meta = file.metadata()?;
    if fd_meta.is_dir() && !fd_meta.file_type().is_symlink() {
        return Ok(None);
    }
    if fd_meta.file_type().is_symlink() || !fd_meta.is_file() {
        return Err(not_file(norm));
    }
    let before = paired_stamp(&file, abs)?;
    let ident = text_identity(&mut file, abs, norm, cap, utf8, before)?;
    if let Some(pinned) = pinned {
        if pinned != ident.rev {
            return Err(rev_mismatch(pinned, &ident.rev));
        }
    }
    let Some((offset, want)) = range else {
        confirm(&file, abs, &before)?;
        return Ok(Some(meta_json(
            norm,
            "text",
            &ident.rev,
            ident.size,
            &ident.sha256,
        )));
    };
    let slice = text_slice(&mut file, abs, &before, &ident, offset, want)?;
    let end = offset + slice.len() as u64;
    Ok(Some(slice_json(
        norm,
        "text",
        &ident.rev,
        ident.size,
        &ident.sha256,
        offset..end,
        &slice,
    )))
}

fn pointer_bytes(file: &mut File, pointer: &Path, before: &Stamp) -> Result<Vec<u8>> {
    if before.size > POINTER_CAP {
        return Err(reject(
            "wiki transfer export refused: blob pointer has no hash",
        ));
    }
    let n = usize::try_from(before.size)
        .map_err(|_| reject("wiki transfer export refused: blob pointer has no hash"))?;
    let mut buf = vec![0u8; n];
    if n > 0 {
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut buf)
            .map_err(|_| reject("wiki transfer export refused: blob pointer has no hash"))?;
    }
    confirm(file, pointer, before)?;
    Ok(buf)
}

fn blob_parts(bytes: &[u8]) -> (String, u64) {
    let meta: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
    let sha = meta
        .get("sha256")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let size = meta.get("size").and_then(Value::as_u64).unwrap_or(0);
    (sha, size)
}

fn blob_value(
    pm: &Pm,
    norm: &str,
    pointer: &Path,
    pinned: Option<&str>,
    range: Option<(u64, u64)>,
) -> Result<Value> {
    let mut file = open_nofollow(pointer)?;
    let before = paired_stamp(&file, pointer)?;
    let bytes = pointer_bytes(&mut file, pointer, &before)?;
    let rev = rev_bytes(&bytes);
    let (sha, declared) = blob_parts(&bytes);
    if let Some(pinned) = pinned {
        if pinned != rev {
            return Err(rev_mismatch(pinned, &rev));
        }
    }
    if !sha_ok(&sha) {
        return Err(reject(
            "wiki transfer export refused: blob pointer has no hash",
        ));
    }
    let Some((offset, want)) = range else {
        confirm(&file, pointer, &before)?;
        return Ok(meta_json(norm, "blob", &rev, declared, &sha));
    };
    if offset > declared {
        return Err(reject(
            "wiki transfer export refused: offset is past the blob",
        ));
    }
    let end = offset.saturating_add(want).min(declared);
    confirm(&file, pointer, &before)?;
    let view_meta = json!({
        "path": norm,
        "sha256": sha,
        "size": declared,
        "mime": "",
        "name": "",
        "rev": rev,
    });
    let view = blob_view(&view_meta)?;
    let data = read_blob_slice(pm, &view, offset, end)?;
    if data.len() as u64 != end - offset {
        return Err(reject(
            "wiki transfer export refused: blob bytes do not match the pointer",
        ));
    }
    confirm(&file, pointer, &before)?;
    Ok(slice_json(
        norm,
        "blob",
        &rev,
        declared,
        &sha,
        offset..end,
        &data,
    ))
}

fn meta_json(path: &str, kind: &str, rev: &str, size: u64, sha256: &str) -> Value {
    json!({
        "path": path,
        "kind": kind,
        "rev": rev,
        "size": size,
        "sha256": sha256,
    })
}

fn slice_json(
    path: &str,
    kind: &str,
    rev: &str,
    size: u64,
    sha256: &str,
    range: std::ops::Range<u64>,
    bytes: &[u8],
) -> Value {
    let next_offset = (range.end < size).then_some(range.end);
    json!({
        "path": path,
        "kind": kind,
        "rev": rev,
        "size": size,
        "sha256": sha256,
        "offset": range.start,
        "data_base64": STANDARD.encode(bytes),
        "next_offset": next_offset,
    })
}

fn dispatch(
    pm: &Pm,
    located: Located,
    pinned: Option<&str>,
    range: Option<(u64, u64)>,
) -> Result<Option<Value>> {
    match located {
        Located::Dir => Ok(None),
        Located::Text {
            norm,
            abs,
            cap,
            utf8,
        } => text_value(&abs, &norm, cap, utf8, pinned, range),
        Located::Blob { norm, pointer } => blob_value(pm, &norm, &pointer, pinned, range).map(Some),
    }
}

/// One file slice. `Ok(None)` means the path is a real directory.
/// `rev` is compared with the revision just computed from the file;
/// the returned `rev` is that computed value.
pub(super) fn file_export(
    pm: &Pm,
    caller: &Caller,
    path: &str,
    offset: u64,
    want: u64,
    rev: Option<&str>,
) -> Result<Option<Value>> {
    want_ok(want)?;
    dispatch(pm, locate(pm, caller, path)?, rev, Some((offset, want)))
}

/// File metadata without a body: `path`, `kind`, `rev`, `size`, `sha256`.
/// `Ok(None)` means a real directory. Uses the same stamp cache as
/// [`file_export`] and does not open blob objects.
pub(super) fn file_metadata(pm: &Pm, caller: &Caller, path: &str) -> Result<Option<Value>> {
    dispatch(pm, locate(pm, caller, path)?, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiki::{blobs_dir, mkdir, put_blob, vault_dir, write, UPLOAD_DIR};
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;

    fn fixture() -> (tempfile::TempDir, Pm) {
        let root = tempfile::TempDir::new().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        (root, pm)
    }

    fn decode(page: &Value) -> Vec<u8> {
        STANDARD
            .decode(page["data_base64"].as_str().unwrap())
            .unwrap()
    }

    #[test]
    fn bounded_slices_match_bytes_checksum_and_directory_is_none() {
        let (_root, pm) = fixture();
        let caller = Caller::Operator;
        let mut body = "a".repeat(MAX_CHUNK_BYTES - 1);
        body.push('\u{e9}');
        body.push_str(&"b".repeat(400));
        let path = "global/cad914-export-slices.txt";
        write(&pm, &caller, path, &body, None).unwrap();

        let want = MAX_CHUNK_BYTES as u64;
        let first = file_export(&pm, &caller, path, 0, want, None)
            .unwrap()
            .unwrap();
        let rev = first["rev"].as_str().unwrap().to_string();
        let sha = first["sha256"].as_str().unwrap().to_string();
        assert_eq!(rev, rev_bytes(body.as_bytes()));
        assert_eq!(sha, sha256_bytes(body.as_bytes()));
        assert_eq!(first["size"].as_u64(), Some(body.len() as u64));
        assert_eq!(first["offset"].as_u64(), Some(0));
        assert_eq!(first["kind"], "text");
        assert_eq!(first["next_offset"].as_u64(), Some(want));
        let raw1 = decode(&first);
        assert_eq!(raw1.len(), want as usize);
        assert_eq!(raw1.last().copied(), Some(0xC3));

        let next = first["next_offset"].as_u64().unwrap();
        let second = file_export(&pm, &caller, path, next, want, Some(&rev))
            .unwrap()
            .unwrap();
        assert_eq!(second["rev"], rev);
        assert_eq!(second["sha256"], sha);
        assert!(second["next_offset"].is_null());
        let raw2 = decode(&second);
        assert_eq!(raw2.first().copied(), Some(0xA9));
        let mut joined = raw1;
        joined.extend_from_slice(&raw2);
        assert_eq!(joined, body.as_bytes());
        assert_eq!(std::str::from_utf8(&joined).unwrap(), body);

        let again = file_export(&pm, &caller, path, 0, want, Some(&rev))
            .unwrap()
            .unwrap();
        assert_eq!(decode(&again), &body.as_bytes()[..want as usize]);

        let meta = file_metadata(&pm, &caller, path).unwrap().unwrap();
        assert_eq!(meta["rev"], rev);
        assert_eq!(meta["sha256"], sha);
        assert_eq!(meta["size"].as_u64(), Some(body.len() as u64));
        assert_eq!(meta["path"], path);
        assert!(meta.get("data_base64").is_none());
        assert!(meta.get("offset").is_none());
        assert!(meta.get("next_offset").is_none());

        let eof = file_export(&pm, &caller, path, body.len() as u64, want, Some(&rev))
            .unwrap()
            .unwrap();
        assert_eq!(eof["data_base64"], "");
        assert!(eof["next_offset"].is_null());
        assert_eq!(eof["rev"], rev);
        assert!(file_export(&pm, &caller, path, body.len() as u64 + 1, want, Some(&rev)).is_err());

        write(&pm, &caller, "global/cad914-empty.txt", "", None).unwrap();
        let empty = file_export(&pm, &caller, "global/cad914-empty.txt", 0, want, None)
            .unwrap()
            .unwrap();
        assert_eq!(empty["data_base64"], "");
        assert_eq!(empty["size"].as_u64(), Some(0));
        assert!(empty["next_offset"].is_null());
        assert_eq!(empty["rev"].as_str().unwrap(), rev_bytes(b""));
        assert_eq!(empty["sha256"].as_str().unwrap(), sha256_bytes(b""));

        mkdir(&pm, &caller, "global/cad914-folder").unwrap();
        assert!(
            file_export(&pm, &caller, "global/cad914-folder", 0, want, None)
                .unwrap()
                .is_none()
        );
        assert!(file_metadata(&pm, &caller, "global/cad914-folder")
            .unwrap()
            .is_none());
        assert!(file_export(&pm, &caller, "", 0, want, None).is_err());
    }

    fn export_text(pm: &Pm, path: &str, rev: Option<&str>) -> Result<Option<Value>> {
        file_export(pm, &Caller::Operator, path, 0, MAX_CHUNK_BYTES as u64, rev)
    }

    #[test]
    fn stale_revision_after_inplace_change_returns_no_new_bytes() {
        let (_root, pm) = fixture();
        let path = "global/cad914-inplace.txt";
        let old = "OLD-REVISION-TEXT";
        let new = "NEW-REVISION-TEXT";
        assert_eq!(old.len(), new.len());
        write(&pm, &Caller::Operator, path, old, None).unwrap();
        let abs = vault_dir(&pm).unwrap().join(path);
        let ino = fs::metadata(&abs).unwrap().ino();

        let first = export_text(&pm, path, None).unwrap().unwrap();
        let old_rev = first["rev"].as_str().unwrap().to_string();
        let old_sha = first["sha256"].as_str().unwrap().to_string();
        assert_eq!(decode(&first), old.as_bytes());
        assert_eq!(old_rev, rev_bytes(old.as_bytes()));

        let before = fs::metadata(&abs).unwrap();
        let mut file = fs::OpenOptions::new().write(true).open(&abs).unwrap();
        file.write_all(new.as_bytes()).unwrap();
        file.sync_all().unwrap();
        let after = fs::metadata(&abs).unwrap();
        assert_eq!(after.ino(), ino);
        assert_ne!(
            (
                before.mtime(),
                before.mtime_nsec(),
                before.ctime(),
                before.ctime_nsec()
            ),
            (
                after.mtime(),
                after.mtime_nsec(),
                after.ctime(),
                after.ctime_nsec()
            )
        );

        let stale = export_text(&pm, path, Some(&old_rev)).unwrap_err();
        assert!(!stale.to_string().contains(new));
        let live = export_text(&pm, path, None).unwrap().unwrap();
        assert_eq!(decode(&live), new.as_bytes());
        assert_eq!(live["rev"].as_str().unwrap(), rev_bytes(new.as_bytes()));
        assert_eq!(
            live["sha256"].as_str().unwrap(),
            sha256_bytes(new.as_bytes())
        );
        assert_ne!(live["rev"].as_str().unwrap(), old_rev);
        assert_ne!(live["sha256"].as_str().unwrap(), old_sha);
        assert!(export_text(&pm, path, Some(&old_rev)).is_err());
    }

    #[test]
    fn stale_revision_after_replacement_returns_no_new_bytes() {
        let (_root, pm) = fixture();
        let path = "global/cad914-replace.txt";
        let old = "OLD-REVISION-TEXT";
        let new = "NEW-REVISION-TEXT";
        write(&pm, &Caller::Operator, path, old, None).unwrap();
        let abs = vault_dir(&pm).unwrap().join(path);
        let old_rev = export_text(&pm, path, None).unwrap().unwrap()["rev"]
            .as_str()
            .unwrap()
            .to_string();

        let replacement = abs.with_extension("replacement");
        fs::write(&replacement, new).unwrap();
        fs::rename(&replacement, &abs).unwrap();

        let stale = export_text(&pm, path, Some(&old_rev)).unwrap_err();
        assert!(!stale.to_string().contains(new));
        let live = export_text(&pm, path, None).unwrap().unwrap();
        assert_eq!(decode(&live), new.as_bytes());
        assert_eq!(live["rev"].as_str().unwrap(), rev_bytes(new.as_bytes()));
        assert_ne!(live["rev"].as_str().unwrap(), old_rev);
    }

    #[test]
    fn blob_directory_symlink_returns_no_outside_bytes() {
        let (root, pm) = fixture();
        let caller = Caller::Operator;
        let inside = b"0123456789abcdef";
        let outside = b"OUTSIDE-LEAK!!!!";
        assert_eq!(inside.len(), outside.len());
        let state = root.path().join("state");
        let uploads = state.join(UPLOAD_DIR);
        fs::create_dir_all(&uploads).unwrap();
        let tmp = uploads.join("doc.bin");
        fs::write(&tmp, inside).unwrap();
        let path = "global/cad914-doc.bin";
        let published = put_blob(&pm, &state, &caller, path, &tmp, None, None).unwrap();
        let sha = published["sha256"].as_str().unwrap().to_string();
        assert_eq!(sha, sha256_bytes(inside));

        let want = 8u64;
        let first = file_export(&pm, &caller, path, 0, want, None)
            .unwrap()
            .unwrap();
        let rev = first["rev"].as_str().unwrap().to_string();
        let pointer = vault_dir(&pm).unwrap().join(format!("{path}.blob"));
        assert_eq!(rev, rev_bytes(&fs::read(&pointer).unwrap()));
        assert_eq!(first["sha256"], sha);
        assert_eq!(decode(&first), &inside[..8]);
        let second = file_export(&pm, &caller, path, 8, want, Some(&rev))
            .unwrap()
            .unwrap();
        assert_eq!(second["rev"], rev);
        assert_eq!(second["sha256"], sha);
        assert!(second["next_offset"].is_null());
        let mut joined = decode(&first);
        joined.extend_from_slice(&decode(&second));
        assert_eq!(joined, inside);

        let blobs = blobs_dir(&vault_dir(&pm).unwrap());
        let parked = blobs.with_file_name(".blobs-parked");
        let outside_dir = tempfile::TempDir::new().unwrap();
        fs::write(outside_dir.path().join(&sha), outside).unwrap();
        fs::rename(&blobs, &parked).unwrap();
        std::os::unix::fs::symlink(outside_dir.path(), &blobs).unwrap();

        match file_export(&pm, &caller, path, 0, want, Some(&rev)) {
            Ok(value) => panic!("blob symlink returned {value:?}"),
            Err(error) => {
                let msg = error.to_string();
                assert!(!msg.contains("OUTSIDE-LEAK!!!!"), "{msg}");
            }
        }
        let meta = file_metadata(&pm, &caller, path).unwrap().unwrap();
        let rendered = meta.to_string();
        assert!(!rendered.contains("OUTSIDE-LEAK!!!!"));
        assert_eq!(meta["sha256"], sha);
        assert!(meta.get("data_base64").is_none());
    }

    #[test]
    fn cache_hit_rechecks_caller_and_vault() {
        let (_a_root, pm_a) = fixture();
        let (_b_root, pm_b) = fixture();
        let path = "global/cad914-same.txt";
        let body_a = "vault-a-body";
        let body_b = "vault-b-body";
        write(&pm_a, &Caller::Operator, path, body_a, None).unwrap();
        write(&pm_b, &Caller::Operator, path, body_b, None).unwrap();
        let abs_a = vault_dir(&pm_a).unwrap().join(path);
        let abs_b = vault_dir(&pm_b).unwrap().join(path);
        assert_ne!(abs_a, abs_b);

        let page_a = export_text(&pm_a, path, None).unwrap().unwrap();
        let page_b = export_text(&pm_b, path, None).unwrap().unwrap();
        let again_a = export_text(&pm_a, path, None).unwrap().unwrap();
        assert_eq!(decode(&page_a), body_a.as_bytes());
        assert_eq!(decode(&page_b), body_b.as_bytes());
        assert_eq!(decode(&again_a), body_a.as_bytes());
        assert_ne!(page_a["rev"], page_b["rev"]);
        assert_eq!(again_a["rev"], page_a["rev"]);

        let secret = "SECRET-BODY-TOKEN";
        let private = "users/alice/cad914-secret.txt";
        write(&pm_a, &Caller::Operator, private, secret, None).unwrap();
        let owned = file_export(
            &pm_a,
            &Caller::Operator,
            private,
            0,
            MAX_CHUNK_BYTES as u64,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(decode(&owned), secret.as_bytes());
        let encoded = owned["data_base64"].as_str().unwrap().to_string();
        for caller in [Caller::Public, Caller::User("bob".to_string())] {
            let denied =
                file_export(&pm_a, &caller, private, 0, MAX_CHUNK_BYTES as u64, None).unwrap_err();
            let msg = denied.to_string();
            assert!(!msg.contains(secret), "{msg}");
            assert!(!msg.contains(&encoded), "{msg}");
            assert!(file_metadata(&pm_a, &caller, private).is_err());
        }
        let member = file_export(
            &pm_a,
            &Caller::User("alice".to_string()),
            private,
            0,
            MAX_CHUNK_BYTES as u64,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(decode(&member), secret.as_bytes());
        assert_eq!(member["rev"], owned["rev"]);
    }
}
