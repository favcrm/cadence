//! Hosted wiki blob range download, private spooling, SHA-256 verification,
//! and stdout streaming (`cadence wiki cat`).

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::fs::Permissions;
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::time::Instant;

use base64::Engine;
use cadence_agent::remote_cli::{call_verb, RemoteAnswer, RemoteTarget};
use cadence_agent::wiki::cli::WikiAction;
use cadence_agent::wiki::NATIVE_TOOL_LINE_BOUND;
use cadence_agent::{Error, Result};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::cli::{eprint_line, Commands};

/// Remote blob cats stay under the default wiki upload cap. A larger
/// pointer is refused before a range is fetched or a spool is created.
const REMOTE_BLOB_SPOOL_BOUND: u64 = 100 * 1024 * 1024;

/// Normal remote `wiki cat` of a blob (no `--meta`, no range flags).
/// Discovery already returned pointer metadata. This fetches bounded
/// ranges pinned to that revision, spools them privately, and writes
/// stdout only after the spool matches the pointer SHA-256.
pub(super) fn remote_blob_cat(
    target: &RemoteTarget,
    command: &Commands,
    discovered: &Value,
    deadline: Instant,
    wake_timeout: u64,
) -> Result<bool> {
    let Commands::Wiki {
        action:
            WikiAction::Cat {
                path,
                meta,
                offset,
                bytes,
                rev,
            },
    } = command
    else {
        return Ok(false);
    };
    if *meta || offset.is_some() || bytes.is_some() || rev.is_some() || discovered["kind"] != "blob"
    {
        return Ok(false);
    }
    stream_remote_blob(target, path, discovered, deadline, wake_timeout)?;
    Ok(true)
}

fn stream_remote_blob(
    target: &RemoteTarget,
    path: &str,
    discovered: &Value,
    deadline: Instant,
    wake_timeout: u64,
) -> Result<()> {
    let mut spool = collect_remote_blob(discovered, |offset, rev| {
        remote_blob_range(target, path, offset, rev, deadline, wake_timeout)
    })?;
    // The spool stays bounded in memory by streaming through `io::copy`.
    // A reader that stopped early (`| head -c …`) is a success: the bytes
    // it wanted were already written, so the download is not retried.
    let mut stdout = io::stdout().lock();
    match io::copy(&mut spool, &mut stdout).and_then(|_| stdout.flush()) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(Error::rejected(format!(
            "remote wiki cat: cannot write stdout: {error}"
        ))),
    }
}

pub(super) fn collect_remote_blob(
    discovered: &Value,
    mut fetch: impl FnMut(u64, &str) -> Result<Value>,
) -> Result<tempfile::NamedTempFile> {
    let sha = blob_meta_str(discovered, "sha256")?;
    if sha.len() != 64 || !sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(Error::rejected(
            "remote wiki cat refused: blob sha256 is not 64 lowercase hex digits; nothing was written",
        ));
    }
    let rev = blob_meta_str(discovered, "rev")?;
    let canon = blob_meta_str(discovered, "path")?;
    let size = discovered["size"].as_u64().ok_or_else(|| {
        Error::rejected(
            "remote wiki cat refused: blob size is not a non-negative integer; nothing was written",
        )
    })?;
    if size > REMOTE_BLOB_SPOOL_BOUND {
        return Err(Error::rejected(format!(
            "remote wiki cat refused: blob is {size} bytes, over the {REMOTE_BLOB_SPOOL_BOUND}-byte spool bound; nothing was written"
        )));
    }
    let mut spool = tempfile::Builder::new()
        .prefix(".cadence-wiki-blob-")
        .permissions(Permissions::from_mode(0o600))
        .tempfile()
        .map_err(|e| {
            Error::rejected(format!(
                "remote wiki cat refused: cannot open a private spool: {e}; nothing was written"
            ))
        })?;
    let mut offset = 0u64;
    let mut hasher = Sha256::new();
    loop {
        let page = fetch(offset, rev)?;
        if !blob_range_fits(&page) {
            return Err(Error::rejected(
                "remote wiki cat refused: complete blob range exceeds the native bound; nothing was written",
            ));
        }
        if page.get("data_base64").is_none() {
            return Err(Error::rejected(
                "remote wiki cat refused: the server returned blob metadata without original bytes (data_base64); nothing was written",
            ));
        }
        for key in ["kind", "mime", "name", "rev", "sha256", "size"] {
            if page.get(key) != discovered.get(key) {
                return Err(Error::rejected(format!(
                    "remote wiki cat refused: blob range changed '{key}'; nothing was written"
                )));
            }
        }
        if page["path"].as_str() != Some(canon) {
            return Err(Error::rejected(
                "remote wiki cat refused: blob range path does not match the pointer; nothing was written",
            ));
        }
        let start = page["range"]["start"].as_u64().ok_or_else(|| {
            Error::rejected(
                "remote wiki cat refused: blob range start is missing; nothing was written",
            )
        })?;
        let end = page["range"]["end"].as_u64().ok_or_else(|| {
            Error::rejected(
                "remote wiki cat refused: blob range end is missing; nothing was written",
            )
        })?;
        let total = page["range"]["total"].as_u64().ok_or_else(|| {
            Error::rejected(
                "remote wiki cat refused: blob range total is missing; nothing was written",
            )
        })?;
        if start != offset || total != size || end < start || end > size {
            return Err(Error::rejected(format!(
                "remote wiki cat refused: blob range {start}..{end} of {total} does not continue offset {offset} of {size}; nothing was written"
            )));
        }
        let encoded = page["data_base64"].as_str().ok_or_else(|| {
            Error::rejected(
                "remote wiki cat refused: data_base64 is not a string; nothing was written",
            )
        })?;
        if encoded.len() >= NATIVE_TOOL_LINE_BOUND {
            return Err(Error::rejected(
                "remote wiki cat refused: encoded blob range exceeds the native bound; nothing was written",
            ));
        }
        let chunk = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| {
                Error::rejected(
                    "remote wiki cat refused: data_base64 is not standard base64; nothing was written",
                )
            })?;
        if chunk.len() as u64 != end - start || chunk.len() >= NATIVE_TOOL_LINE_BOUND {
            return Err(Error::rejected(
                "remote wiki cat refused: blob range bytes do not match the declared slice; nothing was written",
            ));
        }
        if end == offset && size != offset {
            return Err(Error::rejected(
                "remote wiki cat refused: blob range made no progress; nothing was written",
            ));
        }
        spool.write_all(&chunk).map_err(|e| {
            Error::rejected(format!(
                "remote wiki cat refused: cannot spool blob bytes: {e}; nothing was written"
            ))
        })?;
        hasher.update(&chunk);
        offset = end;
        if offset == size {
            break;
        }
    }
    let got: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got != sha || offset != size {
        return Err(Error::rejected(
            "remote wiki cat refused: spooled bytes do not match the pointer sha256; nothing was written",
        ));
    }
    spool.seek(SeekFrom::Start(0)).map_err(|e| {
        Error::rejected(format!(
            "remote wiki cat refused: cannot read the blob spool: {e}; nothing was written"
        ))
    })?;
    Ok(spool)
}

fn blob_range_fits(page: &Value) -> bool {
    struct Count(usize);
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() >= NATIVE_TOOL_LINE_BOUND - 1 - self.0 {
                return Err(io::ErrorKind::InvalidData.into());
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Count(0), page).is_ok()
}

fn blob_meta_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            Error::rejected(format!(
                "remote wiki cat refused: blob metadata '{key}' is missing; nothing was written"
            ))
        })
}

fn remote_blob_range(
    target: &RemoteTarget,
    path: &str,
    offset: u64,
    rev: &str,
    deadline: Instant,
    wake_timeout: u64,
) -> Result<Value> {
    let mut args = Map::new();
    args.insert("path".into(), json!(path));
    args.insert("offset".into(), json!(offset));
    args.insert("rev".into(), json!(rev));
    match call_verb(target, "read", args, deadline, wake_timeout, eprint_line)? {
        RemoteAnswer::Ok(body) => Ok(body),
        RemoteAnswer::Conflict(_) => Err(Error::rejected(
            "remote wiki cat refused: blob range returned a conflict; nothing was written",
        )),
    }
}
