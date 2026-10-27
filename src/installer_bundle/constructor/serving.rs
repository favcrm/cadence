//! Physical serving capture from the ACTUAL own-created sealed daemon only.
//! DTOs, enrollment ACKs, ready flags, paths or claimed PID/hash alone are not
//! factories. Exact listener objects, their kernel creator and namespace/caller
//! custody remain retained. No generic exec/operator route is enabled.
use super::super::{refused, Deadline, Result};
use super::{layout::Layout, OwnedDaemon};
use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Instant;
const PATHS: [&str; 2] = [
    "/srv/cadence/protected/store/cadence.sock",
    "/srv/cadence/guest-views/cadence.sock",
];
struct Endpoint {
    listener: UnixListener,
    path: File,
    stamp: (u64, u64),
    peer: UnixStream,
    gid: u32,
    mode: u32,
}
pub(super) struct OwnedServing {
    endpoints: Vec<Endpoint>,
}
fn int(fd: RawFd, key: i32) -> Result<i32> {
    let mut value = 0i32;
    let mut len = std::mem::size_of_val(&value) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            key,
            (&mut value as *mut i32).cast(),
            &mut len,
        )
    } != 0
        || len as usize != std::mem::size_of_val(&value)
    {
        return Err(refused());
    }
    Ok(value)
}
fn cred(fd: RawFd) -> Result<(i32, u32, u32)> {
    let mut value: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&value) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut value as *mut libc::ucred).cast(),
            &mut len,
        )
    } != 0
        || len as usize != std::mem::size_of_val(&value)
    {
        return Err(refused());
    }
    Ok((value.pid, value.uid, value.gid))
}
impl OwnedServing {
    pub(super) fn capture(
        daemon: &OwnedDaemon,
        layout: &Layout,
        fds: Vec<File>,
        until: Instant,
    ) -> Result<Self> {
        daemon.recheck(until)?;
        layout.recheck(Deadline(until))?;
        if fds.len() != 2 {
            return Err(refused());
        }
        let mut endpoints = Vec::new();
        for (i, file) in fds.into_iter().enumerate() {
            let listener = UnixListener::from(std::os::fd::OwnedFd::from(file));
            if listener.local_addr().map_err(|_| refused())?.as_pathname()
                != Some(std::path::Path::new(PATHS[i]))
                || int(listener.as_raw_fd(), libc::SO_TYPE)? != libc::SOCK_STREAM
                || int(listener.as_raw_fd(), libc::SO_ACCEPTCONN)? != 1
            {
                return Err(refused());
            }
            let path = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(PATHS[i])
                .map_err(|_| refused())?;
            let m = path.metadata().map_err(|_| refused())?;
            // The connection's ACTUAL server creator is admitted against the
            // retained own-created daemon. No claimed PID/UID creates admission.
            let peer = UnixStream::connect(PATHS[i]).map_err(|_| refused())?;
            daemon.require_peer(&peer, until)?;
            if cred(listener.as_raw_fd())? != cred(peer.as_raw_fd())? {
                return Err(refused());
            }
            endpoints.push(Endpoint {
                listener,
                path,
                stamp: (m.dev(), m.ino()),
                peer,
                gid: if i == 0 { 21000 } else { daemon.shared_gid() },
                mode: if i == 0 { 0o600 } else { 0o660 },
            });
        }
        let owned = Self { endpoints };
        owned.recheck(daemon, layout, until)?;
        Ok(owned)
    }
    pub(super) fn recheck(
        &self,
        daemon: &OwnedDaemon,
        layout: &Layout,
        until: Instant,
    ) -> Result<()> {
        daemon.recheck(until)?;
        layout.recheck(Deadline(until))?;
        for (i, e) in self.endpoints.iter().enumerate() {
            let held = e.path.metadata().map_err(|_| refused())?;
            let named = std::fs::symlink_metadata(PATHS[i]).map_err(|_| refused())?;
            for m in [&held, &named] {
                if !m.file_type().is_socket()
                    || (m.dev(), m.ino()) != e.stamp
                    || m.uid() != 21000
                    || m.gid() != e.gid
                    || m.mode() & 0o7777 != e.mode
                    || m.nlink() != 1
                {
                    return Err(refused());
                }
            }
            if e.listener
                .local_addr()
                .map_err(|_| refused())?
                .as_pathname()
                != Some(std::path::Path::new(PATHS[i]))
                || int(e.listener.as_raw_fd(), libc::SO_ACCEPTCONN)? != 1
                || cred(e.listener.as_raw_fd())? != cred(e.peer.as_raw_fd())?
            {
                return Err(refused());
            }
            daemon.require_peer(&e.peer, until)?;
        }
        daemon.recheck(until)?;
        layout.recheck(Deadline(until))
    }
}
