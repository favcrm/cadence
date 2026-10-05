//! Packet-bound fixed control and actual accepted-FD transport. No PID/UID
//! description or JSON can stand in for the UnixStream received by SCM_RIGHTS.
use super::super::{refused, Deadline, Result};
use serde::{Deserialize, Serialize};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixDatagram;
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Domain {
    Pi,
    Store,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Packet {
    Boot {
        version: u8,
        image_attestation: String,
    },
    Accepted {
        version: u8,
        domain: Domain,
    },
    Close {
        version: u8,
        binding: crate::store::Binding,
    },
    Witness {
        version: u8,
        binding: crate::store::Binding,
    },
    Closed {
        version: u8,
        attempt: String,
    },
    Witnessed {
        version: u8,
        witness: serde_json::Value,
    },
    Task {
        version: u8,
        task: String,
        alias: String,
        model: String,
        prompt: String,
    },
    Cancel {
        version: u8,
        task: String,
    },
    Retire {
        version: u8,
        task: String,
    },
    Retired {
        version: u8,
        task: String,
    },
    TaskEvent {
        version: u8,
        task: String,
        part: u64,
        bytes: String,
    },
    Failed {
        version: u8,
    },
}
#[repr(C)]
struct Aligned([usize; 32]);
pub(super) fn send(
    socket: &UnixDatagram,
    packet: &Packet,
    fds: &[RawFd],
    deadline: Deadline,
) -> Result<()> {
    if fds.len() > 1 {
        return Err(refused());
    }
    let mut bytes = serde_json::to_vec(packet).map_err(|_| refused())?;
    if bytes.is_empty() || bytes.len() > 65536 {
        return Err(refused());
    }
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut control = Aligned([0; 32]);
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    if !fds.is_empty() {
        unsafe {
            msg.msg_control = control.0.as_mut_ptr().cast();
            msg.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as usize;
            let c = libc::CMSG_FIRSTHDR(&msg);
            (*c).cmsg_level = libc::SOL_SOCKET;
            (*c).cmsg_type = libc::SCM_RIGHTS;
            (*c).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as usize;
            std::ptr::write_unaligned(libc::CMSG_DATA(c).cast::<RawFd>(), fds[0]);
        }
    }
    loop {
        deadline.check()?;
        let n = unsafe {
            libc::sendmsg(
                socket.as_raw_fd(),
                &msg,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if n == bytes.len() as isize {
            return Ok(());
        }
        if n >= 0 {
            return Err(refused());
        }
        if !matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EAGAIN | libc::EINTR)
        ) {
            return Err(refused());
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
pub(crate) fn receive(socket: &UnixDatagram) -> Result<Option<(Packet, Vec<OwnedFd>)>> {
    let mut bytes = vec![0u8; 65537];
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
    let n = unsafe {
        libc::recvmsg(
            socket.as_raw_fd(),
            &mut msg,
            libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
        )
    };
    if n < 0 {
        return if matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EAGAIN | libc::EINTR)
        ) {
            Ok(None)
        } else {
            Err(refused())
        };
    }
    // Collect every delivered descriptor before validation, so refusal closes
    // all of them, including a truncated/unexpected ancillary message.
    let mut fds = Vec::new();
    let mut valid = true;
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let base = libc::CMSG_LEN(0) as usize;
                let len = (*c).cmsg_len.saturating_sub(base);
                if !len.is_multiple_of(std::mem::size_of::<RawFd>()) {
                    valid = false
                }
                for i in 0..len / std::mem::size_of::<RawFd>() {
                    let fd = std::ptr::read_unaligned(libc::CMSG_DATA(c).cast::<RawFd>().add(i));
                    fds.push(OwnedFd::from_raw_fd(fd));
                }
            } else {
                valid = false
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    if !valid || msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 || n <= 0 || n > 65536 {
        return Err(refused());
    }
    bytes.truncate(n as usize);
    let packet: Packet = serde_json::from_slice(&bytes).map_err(|_| refused())?;
    let expected = if matches!(&packet, Packet::Accepted { version: 1, .. }) {
        1
    } else {
        0
    };
    if fds.len() != expected {
        return Err(refused());
    }
    if expected == 1 {
        let mut ty = 0i32;
        let mut len = std::mem::size_of_val(&ty) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fds[0].as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&mut ty as *mut i32).cast(),
                &mut len,
            )
        } != 0
            || ty != libc::SOCK_STREAM
        {
            return Err(refused());
        }
    }
    Ok(Some((packet, fds)))
}
pub(crate) fn send_child(socket: &UnixDatagram, packet: &Packet) -> Result<()> {
    send(
        socket,
        packet,
        &[],
        Deadline(std::time::Instant::now() + std::time::Duration::from_secs(10)),
    )
}
pub(crate) fn receive_child(socket: &UnixDatagram) -> Result<Option<Packet>> {
    match receive(socket)? {
        Some((packet, fds)) if fds.is_empty() => Ok(Some(packet)),
        None => Ok(None),
        _ => Err(refused()),
    }
}
