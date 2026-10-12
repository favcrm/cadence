//! CAD-913: local checks and the multipart body for `cadence wiki upload`
//! (remote only). The CLI validates the file and the logical path before a
//! single byte leaves the process: a symlink or non-regular file is refused,
//! a file over [`UPLOAD_CAP_BYTES`] is refused before any read of its
//! contents, and a logical path goes through the same grammar as every other
//! wiki path (`normalize`: no `..`, no absolute path, no empty segment).
//!
//! The body is one `multipart/form-data` request. Chunking is NOT
//! implemented: the proxy relay caps an upstream body at 16 MiB, so the file
//! cap sits below that with room for the multipart envelope.

use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::error::{Error, Result};

/// Largest local file a single upload request carries: 15 MiB, which keeps
/// the whole multipart body under the relay's 16 MiB upstream limit.
pub const UPLOAD_CAP_BYTES: u64 = 15 * 1024 * 1024;

/// Normalize a logical wiki path for an upload. The root and an empty path
/// are refused: an upload names a file, never a directory.
pub fn logical_upload_path(path: &str) -> Result<String> {
    let norm = crate::wiki::normalize(path)?;
    if norm.is_empty() {
        return Err(Error::rejected(
            "wiki upload refused: the wiki root is a directory, name a file path",
        ));
    }
    Ok(norm)
}

/// Split a normalized logical path into `(parent, name)`. The parent of a
/// top-level file is the empty root.
pub fn split_logical(norm: &str) -> (String, String) {
    match norm.rsplit_once('/') {
        Some((parent, name)) => (parent.to_string(), name.to_string()),
        None => (String::new(), norm.to_string()),
    }
}

/// The filename sent in the multipart part: the logical file name with any
/// byte outside `[A-Za-z0-9._-]` replaced, so no quote, CR or LF can reach a
/// header. The local file's own name is never sent.
pub fn part_file_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if clean.is_empty() {
        "upload".to_string()
    } else {
        clean
    }
}

/// Refuse a file whose size is over `cap`. Pure, so the refusal is testable
/// without a file on disk.
pub fn check_size(len: u64, cap: u64) -> Result<()> {
    if len > cap {
        return Err(Error::rejected(format!(
            "wiki upload refused: file is {len} bytes, over the {cap}-byte cap \
             (15 MiB); nothing was sent"
        )));
    }
    Ok(())
}

/// Open and read the local file as bytes, refusing before any network call:
/// a symlink (the final component is not followed), a non-regular file, or a
/// file over the cap. The lstat gives the early, named refusals; the file is
/// then opened with `O_NOFOLLOW | O_NONBLOCK` (so a swap to a symlink fails
/// the open, and a swap to a FIFO or device cannot block it), and the type and
/// size are re-checked on the opened handle with fstat. The read is bounded by
/// `cap + 1` on that same handle, so a file that grows after the check is still
/// refused.
pub fn read_local_file(path: &Path, cap: u64) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path).map_err(|e| {
        Error::rejected(format!("wiki upload: cannot read {}: {e}", path.display()))
    })?;
    if meta.file_type().is_symlink() {
        return Err(Error::rejected(format!(
            "wiki upload refused: {} is a symlink; pass the real file",
            path.display()
        )));
    }
    if !meta.is_file() {
        return Err(Error::rejected(format!(
            "wiki upload refused: {} is not a regular file",
            path.display()
        )));
    }
    let file: File = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| {
            if e.raw_os_error() == Some(libc::ELOOP) {
                Error::rejected(format!(
                    "wiki upload refused: {} is a symlink; pass the real file",
                    path.display()
                ))
            } else {
                Error::rejected(format!("wiki upload: cannot open {}: {e}", path.display()))
            }
        })?;
    // Judge the handle we will read, not the path: a swap after the lstat
    // is caught here.
    let opened = file.metadata().map_err(|e| {
        Error::rejected(format!("wiki upload: cannot read {}: {e}", path.display()))
    })?;
    if !opened.is_file() {
        return Err(Error::rejected(format!(
            "wiki upload refused: {} is not a regular file",
            path.display()
        )));
    }
    check_size(opened.len(), cap)?;
    let mut bytes = Vec::new();
    (&file).take(cap + 1).read_to_end(&mut bytes)?;
    check_size(bytes.len() as u64, cap)?;
    Ok(bytes)
}

/// Build one `multipart/form-data` body: the text `fields` in order, then a
/// single `file` part. The boundary must not occur in any field or in the
/// bytes; the caller draws it from random bytes.
pub fn build_multipart(
    boundary: &str,
    fields: &[(&str, &str)],
    file_name: &str,
    file: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(file.len() + 1024);
    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_refusals_name_the_guard_and_split_names_the_file() {
        for bad in ["", "/abs/x.pdf", "a/../b.pdf", "a//b.pdf", "a/b/"] {
            assert!(
                logical_upload_path(bad).is_err(),
                "{bad:?} must be refused before any request"
            );
        }
        let norm = logical_upload_path("sources/report.pdf").unwrap();
        assert_eq!(
            split_logical(&norm),
            ("sources".to_string(), "report.pdf".to_string())
        );
        assert_eq!(
            split_logical("top.pdf"),
            (String::new(), "top.pdf".to_string())
        );
        assert_eq!(part_file_name("a\"b\r\nc d.pdf"), "a_b__c_d.pdf");
    }

    #[test]
    fn size_cap_and_local_file_refusals_happen_before_a_read_of_the_body() {
        assert!(check_size(UPLOAD_CAP_BYTES, UPLOAD_CAP_BYTES).is_ok());
        let over = check_size(UPLOAD_CAP_BYTES + 1, UPLOAD_CAP_BYTES).unwrap_err();
        assert!(over.to_string().contains("nothing was sent"), "{over}");

        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        fs::write(&real, b"hello").unwrap();
        // A symlink to a real file is refused; the target is never opened.
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let err = read_local_file(&link, UPLOAD_CAP_BYTES).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
        // A directory is not a regular file.
        assert!(read_local_file(dir.path(), UPLOAD_CAP_BYTES).is_err());
        // The real file reads, and a cap below its size refuses it.
        assert_eq!(read_local_file(&real, UPLOAD_CAP_BYTES).unwrap(), b"hello");
        assert!(read_local_file(&real, 4).is_err());
    }

    #[test]
    fn a_fifo_or_non_regular_source_is_refused_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        // A FIFO with no writer would block a plain open forever; the
        // O_NONBLOCK open returns and the fstat type check refuses it.
        let fifo = dir.path().join("pipe.pdf");
        let c_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let err = read_local_file(&fifo, UPLOAD_CAP_BYTES).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{err}");
    }

    #[test]
    fn multipart_body_has_the_contract_fields_then_one_file_part() {
        // The envelope is not a body field: it travels in the
        // `X-Cadence-Wiki-Envelope` header.
        let fields = [
            ("version", "hosted-cadence-wiki.v1"),
            ("action", "write_blob"),
            ("path", "sources/a.pdf"),
        ];
        let body = build_multipart("BND", &fields, "a.pdf", b"%PDF-\x00\xff");
        let text = String::from_utf8_lossy(&body);
        // Each contract field appears exactly once, in order, then `file`.
        let mut at = 0;
        for (name, value) in fields {
            let needle = format!("name=\"{name}\"\r\n\r\n{value}\r\n");
            let pos = text[at..].find(&needle).unwrap_or_else(|| panic!("{name}"));
            at += pos + needle.len();
        }
        assert_eq!(text.matches("name=\"file\"").count(), 1);
        assert!(text.contains("filename=\"a.pdf\""));
        assert!(body.ends_with(b"\r\n--BND--\r\n"));
        // The raw bytes are carried verbatim, not re-encoded.
        let file_bytes: &[u8] = b"%PDF-\x00\xff";
        assert!(body.windows(file_bytes.len()).any(|w| w == file_bytes));
    }
}
