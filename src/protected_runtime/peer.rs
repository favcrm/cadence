//! Actual kernel peer measurement for the private owner sockets.
//!
//! Authentication is never a field, role, request id or a supervisor-supplied
//! PID. A peer is admitted because the kernel's `SO_PEERCRED` names its live
//! uid/pid, `/proc/<pid>/exe` is held as a real fd, `fstat`ed (regular file,
//! root-owned, not group/other writable), hashed from offset 0 and compared to
//! the pinned digest, the live starttime is stable before *and* after the hash,
//! and the presented generation equals the enrolled one. Root uid alone refuses.

use std::os::unix::io::{AsRawFd, FromRawFd};

use crate::error::{Error, Result};

use super::MAX_EXE_BYTES;

/// A measured peer process. Private fields: produced only by [`admit_pinned`]
/// or [`admit_uid`] from the kernel; no caller literal exists.
#[derive(Clone, Debug)]
pub(crate) struct Peer {
    pub(crate) uid: u32,
    pub(crate) pid: u32,
    pub(crate) starttime: u64,
    pub(crate) exe_digest: [u8; 32],
}

/// `SO_PEERCRED` — the accepted peer's kernel uid + pid.
pub(crate) fn credentials(stream: &std::os::unix::net::UnixStream) -> Result<(u32, u32)> {
    let (uid, _gid, pid) = credentials_full(stream)?;
    Ok((uid, pid))
}

/// `SO_PEERCRED` — uid, gid and pid. The gid is the peer's effective group at
/// connect time, used only for the configured supervisor role check.
pub(crate) fn credentials_full(stream: &std::os::unix::net::UnixStream) -> Result<(u32, u32, u32)> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 || len as usize != std::mem::size_of::<libc::ucred>() || cred.pid <= 0 {
        return Err(Error::rejected("socket peer credentials unreadable"));
    }
    Ok((cred.uid, cred.gid, cred.pid as u32))
}

/// `/proc/<pid>/stat` field 22 — the process start time in clock ticks.
pub(crate) fn starttime(pid: u32) -> Option<u64> {
    crate::peer::proc_starttime(pid)
}

/// Fingerprint used to prove the executable inode did not change under the
/// read.
#[derive(PartialEq, Eq)]
struct ExeStat {
    ino: u64,
    size: u64,
    mtime_ns: i128,
    ctime_ns: i128,
}

fn exe_stat(m: &libc::stat) -> ExeStat {
    ExeStat {
        ino: m.st_ino,
        size: m.st_size as u64,
        mtime_ns: (m.st_mtime as i128) * 1_000_000_000 + m.st_mtime_nsec as i128,
        ctime_ns: (m.st_ctime as i128) * 1_000_000_000 + m.st_ctime_nsec as i128,
    }
}

/// Open and measure the running peer's executable inode. The held fd is
/// `fstat`ed before and after the hash; a non-regular, non-root-owned,
/// group/other-writable, oversized or unstable binary refuses.
pub(crate) fn exe_digest(pid: u32) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};

    let path = format!("/proc/{pid}/exe");
    let cpath =
        std::ffi::CString::new(path).map_err(|_| Error::rejected("exe path not cstring"))?;
    let raw = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC, 0) };
    if raw < 0 {
        return Err(Error::rejected(format!(
            "/proc/{pid}/exe open failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: `raw` is a freshly opened owned fd.
    let file = unsafe { std::fs::File::from_raw_fd(raw) };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(file.as_raw_fd(), &mut st) } != 0 {
        return Err(Error::rejected(format!(
            "/proc/{pid}/exe fstat failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    let before = exe_stat(&st);
    if st.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(Error::rejected("/proc/<pid>/exe is not a regular file"));
    }
    if st.st_uid != 0 {
        return Err(Error::rejected("peer executable is not root-owned"));
    }
    if st.st_mode & 0o022 != 0 {
        return Err(Error::rejected("peer executable is group/other writable"));
    }
    if st.st_nlink < 1 {
        return Err(Error::rejected("peer executable has zero links"));
    }
    if before.size == 0 || before.size > MAX_EXE_BYTES {
        return Err(Error::rejected("peer executable size is outside the bound"));
    }
    let mut h = Sha256::new();
    let mut off: u64 = 0;
    let mut buf = vec![0u8; 64 * 1024];
    while off < before.size {
        let want = std::cmp::min(buf.len() as u64, before.size - off) as usize;
        let n = unsafe {
            libc::pread(
                file.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                want,
                off as libc::off_t,
            )
        };
        if n <= 0 {
            return Err(Error::rejected("/proc/<pid>/exe read failed"));
        }
        h.update(&buf[..n as usize]);
        off += n as u64;
    }
    let mut st2: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(file.as_raw_fd(), &mut st2) } != 0 {
        return Err(Error::rejected("/proc/<pid>/exe re-fstat failed"));
    }
    if exe_stat(&st2) != before {
        return Err(Error::rejected(
            "peer executable changed under read — unstable inode",
        ));
    }
    Ok(h.finalize().into())
}

pub(crate) fn hex(digest: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Admit a pinned peer (supervisor or installer): kernel uid, held exe digest,
/// stable starttime and enrolled generation must all match. A missing pin, a
/// root shell, a different binary or a stale generation refuses.
pub(crate) fn admit_pinned(
    stream: &std::os::unix::net::UnixStream,
    expected_uid: u32,
    expected_exe_hex: &str,
    presented_generation: &str,
    pinned_generation: &str,
) -> Result<Peer> {
    if expected_uid == 0 {
        return Err(Error::rejected(
            "refusing to pin root uid as a peer identity — a root shell is not the measured role",
        ));
    }
    let (uid, pid) = credentials(stream)?;
    if uid != expected_uid {
        return Err(Error::rejected(format!(
            "peer uid {uid} is not the measured role uid {expected_uid}"
        )));
    }
    if presented_generation != pinned_generation {
        return Err(Error::rejected(
            "peer presented a generation that is not the pinned enrollment",
        ));
    }
    let start_a = starttime(pid)
        .ok_or_else(|| Error::rejected(format!("peer pid {pid} starttime unreadable")))?;
    let measured = hex(&exe_digest(pid)?);
    if measured != expected_exe_hex {
        return Err(Error::rejected(
            "peer binary does not match the pinned executable digest",
        ));
    }
    let start_b = starttime(pid)
        .ok_or_else(|| Error::rejected(format!("peer pid {pid} exited during admission")))?;
    if start_b != start_a {
        return Err(Error::rejected(
            "peer pid changed under admission — pid reuse/exit refused",
        ));
    }
    Ok(Peer {
        uid,
        pid,
        starttime: start_b,
        exe_digest: hex_to_32(expected_exe_hex)?,
    })
}

/// Admit any process of the enrolled runtime uid (the operations socket). The
/// caller is then *that runtime*, not the supervisor and not the operator; the
/// listener carries the rest of the identity from its enrollment.
pub(crate) fn admit_uid(
    stream: &std::os::unix::net::UnixStream,
    expected_uid: u32,
) -> Result<(u32, u32, u64)> {
    let (uid, pid) = credentials(stream)?;
    if uid != expected_uid {
        return Err(Error::rejected(
            "socket peer is not the enrolled runtime uid",
        ));
    }
    // The pid must still be live and in the runtime's private PID namespace —
    // a died/reused pid names no live caller.
    let start =
        starttime(pid).ok_or_else(|| Error::rejected("runtime peer starttime unreadable"))?;
    Ok((uid, pid, start))
}

/// Admit a peer by kernel uid **and** the pinned executable digest, with a
/// stable live starttime. Used for the enrolled supervisor/installer roles and
/// (uid 0) the owner client. A same-uid shell that is not the pinned binary
/// refuses.
pub(crate) fn admit_exe(
    stream: &std::os::unix::net::UnixStream,
    expected_uid: u32,
    expected_exe_hex: &str,
) -> Result<Peer> {
    let (uid, pid) = credentials(stream)?;
    if uid != expected_uid {
        return Err(Error::rejected(format!(
            "peer uid {uid} is not the measured role uid {expected_uid}"
        )));
    }
    let start_a = starttime(pid)
        .ok_or_else(|| Error::rejected(format!("peer pid {pid} starttime unreadable")))?;
    let measured = hex(&exe_digest(pid)?);
    if measured != expected_exe_hex {
        return Err(Error::rejected(
            "peer binary does not match the pinned executable digest",
        ));
    }
    let start_b = starttime(pid)
        .ok_or_else(|| Error::rejected(format!("peer pid {pid} exited during admission")))?;
    if start_b != start_a {
        return Err(Error::rejected(
            "peer pid changed under admission — pid reuse/exit refused",
        ));
    }
    Ok(Peer {
        uid,
        pid,
        starttime: start_b,
        exe_digest: hex_to_32(expected_exe_hex)?,
    })
}

/// Admit the root owner binary by uid 0 **and** the pinned executable digest.
/// This is the only path where uid 0 is accepted: the broker-fence listener
/// uses it to authenticate the owner client. A root shell (any other binary)
/// fails the digest; a different uid fails the uid.
pub(crate) fn admit_root_exe(
    stream: &std::os::unix::net::UnixStream,
    expected_exe_hex: &str,
) -> Result<Peer> {
    admit_exe(stream, 0, expected_exe_hex)
}

fn hex_to_32(s: &str) -> Result<[u8; 32]> {
    let bytes = super::strict::hex_bytes(s)?;
    bytes
        .try_into()
        .map_err(|_| Error::rejected("digest is not 32 bytes"))
}
