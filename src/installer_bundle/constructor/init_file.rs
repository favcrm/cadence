//! Finite transport for the original consumed Init file. This module cannot
//! elect a file or grant: dispatcher retains actual creation/caller custody.
use super::super::{refused, Result};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};
#[repr(C)]
struct Aligned([usize; 32]);

/// JSON requests must carry NO descriptors. Adopt every visible kernel FD
/// before refusal so injected/truncated ancillary input never leaks handles.
pub(super) fn read_plain(stream: &UnixStream, bytes: &mut [u8]) -> Result<usize> {
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut control = Aligned([0; 32]);
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.0.as_mut_ptr().cast();
    msg.msg_controllen = std::mem::size_of_val(&control);
    let n = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
    if n <= 0 {
        return Err(refused());
    }
    let mut fds = Vec::new();
    let mut ancillary = false;
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            ancillary = true;
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let len = (*c).cmsg_len.saturating_sub(libc::CMSG_LEN(0) as usize);
                for i in 0..len / std::mem::size_of::<RawFd>() {
                    let fd = std::ptr::read_unaligned(libc::CMSG_DATA(c).cast::<RawFd>().add(i));
                    fds.push(OwnedFd::from_raw_fd(fd));
                }
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    if ancillary || msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
        return Err(refused());
    }
    Ok(n as usize)
}

/// Called once AFTER the exact InitFile JSON reply, under the SAME request
/// budget and already-burned delivery. No reconnect, partial-success or retry
/// of an ambiguous send; Root keeps the original File throughout.
pub(super) fn send(stream: &UnixStream, file: &File, until: Instant) -> Result<()> {
    let mut tag = [0x44u8];
    let mut iov = libc::iovec {
        iov_base: tag.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = Aligned([0; 32]);
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.0.as_mut_ptr().cast();
    msg.msg_controllen = unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as usize };
    unsafe {
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as usize;
        std::ptr::write_unaligned(libc::CMSG_DATA(c).cast::<RawFd>(), file.as_raw_fd());
    }
    loop {
        if Instant::now() >= until {
            return Err(refused());
        }
        let n = unsafe {
            libc::sendmsg(
                stream.as_raw_fd(),
                &msg,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if n == 1 {
            return if Instant::now() < until {
                Ok(())
            } else {
                Err(refused())
            };
        }
        if n >= 0
            || !matches!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EINTR | libc::EAGAIN)
            )
        {
            return Err(refused());
        }
        // EINTR/EAGAIN did not transfer this one-byte rights frame. They share
        // the original deadline; a positive/ambiguous partial send never loops.
        std::thread::sleep(Duration::from_millis(1));
    }
}
