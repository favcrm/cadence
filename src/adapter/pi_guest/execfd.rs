//! Fd-based digest custody — hash an opened executable on its *fd* (never a
//! pathname `lstat` that a swap can defeat) and the `OpenKind` an `openat2`
//! caller asks for.
//!
//! Every read of the fd is **offset-preserving** (`pread`): a shared
//! file-description offset must never be advanced as a side-effect of
//! verification, and the digest is taken over the *whole* file from offset 0.
//! The digest window is closed by a before/after `fstat` pair — if the inode
//! changes (size, mtime, ctime, inode number) while it is hashed, the digest
//! refuses rather than pinning a file observed mid-mutation.

use crate::error::{Error, Result};

/// A minimal `fstat` snapshot used for the before/after digest stability
/// check: if any of these change while we hash, the inode was mutated under
/// us and the digest is untrustworthy.
#[derive(Clone, Copy, PartialEq, Eq)]
struct StatStamp {
    ino: u64,
    size: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}

fn stamp(file: &std::fs::File) -> Result<StatStamp> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata()?;
    Ok(StatStamp {
        ino: m.ino(),
        size: m.size(),
        mtime: m.mtime(),
        mtime_ns: m.mtime_nsec(),
        ctime: m.ctime(),
        ctime_ns: m.ctime_nsec(),
    })
}

/// Streamed sha256 of the *whole* file, read via `pread` from offset 0 in
/// bounded chunks. Rejects a file larger than a sane exec ceiling up front so
/// the digest never streams an unbounded or growing blob. A `StatStamp` is
/// taken before and after; any drift refuses.
pub(crate) fn sha256_fd(file: &std::fs::File, size: u64) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::FileExt;
    /// Refuse to hash something implausibly large for a binary — an exec'd
    /// ELF is tens of MB at most; this bounds the read loop and catches a
    /// grow-under-us file before the digest window opens.
    const MAX_EXEC_BYTES: u64 = 512 * 1024 * 1024;
    if size > MAX_EXEC_BYTES {
        return Err(Error::rejected(format!(
            "exec object is {size} bytes — above the {MAX_EXEC_BYTES} bound; \
             refusing to digest an unbounded blob"
        )));
    }
    let before = stamp(file)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 128 * 1024];
    let mut off = 0u64;
    while off < size {
        let want = std::cmp::min(buf.len() as u64, size - off) as usize;
        let n = file
            .read_at(&mut buf[..want], off)
            .map_err(|e| Error::internal(e.to_string()))?;
        if n == 0 {
            // Truncated under us between the stat and the read.
            return Err(Error::rejected(
                "exec object shrank while hashing — inode not stable",
            ));
        }
        h.update(&buf[..n]);
        off += n as u64;
    }
    let after = stamp(file)?;
    if before != after {
        return Err(Error::rejected(
            "exec object changed while hashing (ino/size/timestamps drifted) — \
             the measured digest is untrustworthy",
        ));
    }
    Ok(h.finalize().into())
}

/// The kind of object `open_at2` is asked for — drives the `O_DIRECTORY` /
/// regular-file choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenKind {
    /// A directory — `O_DIRECTORY`.
    Dir,
    /// A regular file to be executed — `O_RDONLY | O_CLOEXEC`. `fstat` in the
    /// caller proves it is not a fifo/socket/dir before exec.
    ExecFile,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digest is taken with `pread` over the whole file from offset 0 and
    /// leaves the shared file offset untouched.
    #[test]
    fn hash_preserves_offset_and_covers_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obj");
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let size = file.metadata().unwrap().len();
        let d1 = sha256_fd(&file, size).unwrap();
        // Independent reference digest over the raw bytes.
        use sha2::Digest;
        let d2: [u8; 32] = sha2::Sha256::digest(&data).into();
        assert_eq!(d1, d2, "fd digest must cover the whole file from 0");
        // And the shared offset is still 0 — a plain read starts at the head.
        use std::io::Read;
        let mut head = [0u8; 4];
        let mut f = &file;
        f.read_exact(&mut head).unwrap();
        assert_eq!(&head, &data[0..4]);
    }

    /// A file whose size changes during the digest window is refused — the
    /// before/after stat stamp catches a mutation the path-verify can't.
    #[test]
    fn hash_refuses_a_mutating_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obj");
        std::fs::write(&path, b"steady").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        // Claim a size larger than the real file so `read_at` hits EOF mid-way
        // -> the "shrank while hashing" refusal path.
        let e = sha256_fd(&file, 1 << 20).unwrap_err();
        assert!(
            e.to_string().contains("shrank") || e.to_string().contains("not stable"),
            "{e}"
        );
    }
}
