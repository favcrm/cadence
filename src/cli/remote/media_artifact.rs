//! Remote platform media artifact download, atomic private spooling, and
//! digest-verified publication.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::ffi::CString;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine;
use cadence_agent::fd_fs::{self, DirFd};
use cadence_agent::remote_cli::{call_verb, RemoteAnswer, RemoteTarget};
use cadence_agent::{Error, Result};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::{remote_failure, valid_media_artifact_ref};

/// Maximum size of a retained media artifact (`10 MiB`, matching `ARTIFACT_BYTES_MAX`).
#[cfg_attr(not(test), allow(dead_code))]
pub(super) const PLATFORM_MEDIA_ARTIFACT_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// Video is streamed into custody with a separate 100 MiB limit.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) const PLATFORM_VIDEO_ARTIFACT_MAX_BYTES: u64 = 100 * 1024 * 1024;

pub(super) fn remote_platform_artifact<E, P>(
    target: &RemoteTarget,
    reference: &str,
    out: &Path,
    wake_timeout: u64,
    eprint_line: E,
    print_json: P,
) -> Result<i32>
where
    E: Fn(&str),
    P: Fn(&Value),
{
    if !valid_media_artifact_ref(reference) {
        return Err(Error::invalid(
            "platform_artifact",
            "artifact ref must be med_<job-id>.<64-lowercase-hex-sha256>",
        ));
    }
    if fs::symlink_metadata(out).is_ok() {
        return Err(Error::rejected(format!(
            "cannot create {}: file already exists; nothing was requested or written",
            out.display()
        )));
    }
    eprint_line(&format!(
        "cadence platform org={} operation=artifact ref={reference}",
        target.org
    ));
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(wake_timeout))
        .ok_or_else(|| Error::rejected("invalid wake timeout"))?;
    let receipt = collect_platform_artifact(reference, out, |offset| {
        let mut args = Map::new();
        args.insert("operation".into(), json!("artifact"));
        args.insert("ref".into(), json!(reference));
        args.insert("offset".into(), json!(offset));
        match call_verb(
            target,
            "platform_call",
            args,
            deadline,
            wake_timeout,
            &eprint_line,
        )? {
            RemoteAnswer::Ok(body) => Ok(body),
            RemoteAnswer::Conflict(_) => {
                Err(Error::rejected("remote media artifact read conflicted"))
            }
        }
    });
    let receipt = match receipt {
        Ok(receipt) => receipt,
        Err(error) => return remote_failure(error),
    };
    print_json(&receipt);
    Ok(0)
}

// A private directory on the destination filesystem keeps the spool name
// inaccessible to other users. Both it and the destination directory stay
// open: renaming a directory cannot substitute the verified file at publish.
struct MediaArtifactSpool {
    parent: DirFd,
    directory: DirFd,
    directory_name: CString,
    file: Option<File>,
}

impl MediaArtifactSpool {
    fn new(out: &Path) -> Result<Self> {
        let parent_path = out
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = DirFd::open_dir(parent_path)
            .map_err(|e| Error::rejected(format!("cannot open destination directory: {e}")))?;
        let directory_name = CString::new(format!(
            ".cadence-media-artifact-{}",
            uuid::Uuid::new_v4().simple()
        ))
        .expect("generated directory name contains no NUL");
        parent.mkdir(&directory_name, 0o700).map_err(|e| {
            Error::rejected(format!("cannot create private media artifact spool: {e}"))
        })?;
        let directory = parent.open_child_dir(&directory_name).map_err(|e| {
            Error::rejected(format!("cannot open private media artifact spool: {e}"))
        })?;
        let stat = directory
            .stat()
            .map_err(|e| Error::rejected(format!("cannot inspect media artifact spool: {e}")))?;
        // A foreign user could have replaced the name between mkdirat and openat.
        let euid = fd_fs::geteuid();
        #[allow(clippy::useless_conversion)]
        let mode: u32 = stat.st_mode.into();
        if stat.st_uid != euid || mode & 0o077 != 0 {
            return Err(Error::rejected(
                "media artifact spool directory is not private",
            ));
        }
        let mut spool = Self {
            parent,
            directory,
            directory_name,
            file: None,
        };
        let file = spool
            .directory
            .open_file_nofollow(
                c"artifact",
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o600,
            )
            .map_err(|e| {
                Error::rejected(format!("cannot open private media artifact spool: {e}"))
            })?;
        spool.file = Some(file);
        Ok(spool)
    }

    fn parent_matches(&self, out: &Path) -> bool {
        let parent = out
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        match (
            self.parent.stat(),
            DirFd::open_dir(parent).and_then(|dir| dir.stat()),
        ) {
            (Ok(pinned), Ok(current)) => {
                pinned.st_dev == current.st_dev && pinned.st_ino == current.st_ino
            }
            _ => false,
        }
    }

    fn publish(&self, out: &Path) -> Result<()> {
        self.file
            .as_ref()
            .expect("opened spool")
            .sync_all()
            .map_err(|e| {
                Error::rejected(format!(
                    "cannot sync media artifact spool: {e}; nothing was written"
                ))
            })?;
        let leaf = out
            .file_name()
            .and_then(|name| CString::new(name.as_bytes()).ok())
            .ok_or_else(|| Error::rejected("media artifact destination needs a valid file name"))?;
        if !self.parent_matches(out) {
            return Err(Error::rejected(
                "destination directory changed during download; nothing was written",
            ));
        }
        // Prefer a no-replace rename: some filesystems cannot make hard links.
        // Both directories are pinned and the source is private.
        let renamed = self
            .directory
            .rename_child_to_noreplace(c"artifact", &self.parent, &leaf)
            .map_err(|e| Error::rejected(format!("cannot create {}: {e}", out.display())))?;
        if !renamed {
            self.directory
                .link_child_to(c"artifact", &self.parent, &leaf)
                .map_err(|e| Error::rejected(format!("cannot create {}: {e}", out.display())))?;
        }
        let same_file = match (
            self.file.as_ref().expect("opened spool").metadata(),
            fs::symlink_metadata(out),
        ) {
            (Ok(verified), Ok(current)) => {
                verified.dev() == current.dev() && verified.ino() == current.ino()
            }
            _ => false,
        };
        if !same_file {
            return Err(Error::unknown(format!(
                "destination changed during publication; the verified artifact may be in the original directory, not {}; inspect before retrying",
                out.display()
            )));
        }
        Ok(())
    }
}

impl Drop for MediaArtifactSpool {
    fn drop(&mut self) {
        let _ = self.directory.unlink_child(c"artifact");
        // Remove the directory only if its original name still identifies our
        // pinned directory. A renamed or substituted directory is left alone.
        if let Ok(current) = self.parent.open_child_dir(&self.directory_name) {
            if let (Ok(a), Ok(b)) = (current.stat(), self.directory.stat()) {
                if a.st_dev == b.st_dev && a.st_ino == b.st_ino {
                    let _ = self.parent.rmdir_child(&self.directory_name);
                }
            }
        }
    }
}

pub(super) fn collect_platform_artifact(
    reference: &str,
    out: &Path,
    mut fetch: impl FnMut(u64) -> Result<Value>,
) -> Result<Value> {
    if !valid_media_artifact_ref(reference) {
        return Err(Error::invalid(
            "platform_artifact",
            "artifact ref must be med_<job-id>.<64-lowercase-hex-sha256>",
        ));
    }
    if fs::symlink_metadata(out).is_ok() {
        return Err(Error::rejected(format!(
            "cannot create {}: file already exists; nothing was written",
            out.display()
        )));
    }
    let (expected_job, expected_digest) = reference.split_once('.').expect("validated ref");
    let mut spool = MediaArtifactSpool::new(out)?;
    let mut offset = 0u64;
    let mut pinned_total: Option<u64> = None;
    let mut pinned_mime: Option<String> = None;
    let mut hasher = Sha256::new();
    loop {
        let reply = fetch(offset)?;
        let data = reply
            .get("data")
            .filter(|v| v.is_object())
            .unwrap_or(&reply);
        if data.get("ref").and_then(Value::as_str) != Some(reference)
            || data.get("jobId").and_then(Value::as_str) != Some(expected_job)
            || data.get("digest").and_then(Value::as_str) != Some(expected_digest)
        {
            return Err(Error::rejected(
                "remote media artifact response does not match the requested ref/digest; nothing was written",
            ));
        }
        let mime = data
            .get("mime")
            .and_then(Value::as_str)
            .filter(|m| matches!(*m, "image/png" | "image/jpeg" | "video/mp4" | "video/webm"))
            .ok_or_else(|| {
                Error::rejected(
                    "remote media artifact has an unsupported or missing MIME type; nothing was written",
                )
            })?;
        if let Some(prev) = pinned_mime.as_deref() {
            if prev != mime {
                return Err(Error::rejected(
                    "remote media artifact MIME changed across pages; nothing was written",
                ));
            }
        } else {
            pinned_mime = Some(mime.to_string());
        }
        let page_offset = data.get("offset").and_then(Value::as_u64).ok_or_else(|| {
            Error::rejected("remote media artifact page is missing offset; nothing was written")
        })?;
        let total = data
            .get("total")
            .and_then(Value::as_u64)
            .filter(|t| {
                let max = if mime.starts_with("video/") {
                    PLATFORM_VIDEO_ARTIFACT_MAX_BYTES
                } else {
                    PLATFORM_MEDIA_ARTIFACT_MAX_BYTES
                };
                *t > 0 && *t <= max
            })
            .ok_or_else(|| {
                Error::rejected(
                    "remote media artifact total size is missing or out of bounds; nothing was written",
                )
            })?;
        if let Some(prev_total) = pinned_total {
            if prev_total != total {
                return Err(Error::rejected(
                    "remote media artifact total size changed across pages; nothing was written",
                ));
            }
        } else {
            pinned_total = Some(total);
        }
        if page_offset != offset {
            return Err(Error::rejected(
                "remote media artifact page offset is not contiguous; nothing was written",
            ));
        }
        let encoded = data
            .get("content_base64")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::rejected(
                    "remote media artifact page is missing content_base64; nothing was written",
                )
            })?;
        let chunk = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| {
                Error::rejected(
                    "remote media artifact content_base64 is invalid; nothing was written",
                )
            })?;
        if let Some(declared_bytes) = data.get("bytes").and_then(Value::as_u64) {
            if declared_bytes != chunk.len() as u64 {
                return Err(Error::rejected(
                    "remote media artifact page byte count does not match decoded bytes; nothing was written",
                ));
            }
        }
        if chunk.is_empty() || offset + (chunk.len() as u64) > total {
            return Err(Error::rejected(
                "remote media artifact page made no progress or exceeded total; nothing was written",
            ));
        }
        spool
            .file
            .as_mut()
            .expect("opened spool")
            .write_all(&chunk)
            .map_err(|e| {
                Error::rejected(format!(
                    "cannot write media artifact spool: {e}; nothing was written"
                ))
            })?;
        hasher.update(&chunk);
        offset += chunk.len() as u64;
        if offset == total {
            break;
        }
    }
    let total = pinned_total.unwrap_or(0);
    let got_digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got_digest != expected_digest || offset != total {
        return Err(Error::rejected(
            "the media artifact's bytes do not match its SHA-256 digest; nothing was written",
        ));
    }
    spool.publish(out)?;
    Ok(json!({
        "ref": reference,
        "jobId": expected_job,
        "digest": expected_digest,
        "mime": pinned_mime.unwrap_or_else(|| "image/png".into()),
        "bytes": total,
        "written": out.display().to_string(),
    }))
}
