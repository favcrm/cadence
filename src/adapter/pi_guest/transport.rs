//! Exactly three stdio pipes from the root-created selected helper. No PID
//! adoption, authority FD, executable choice or inherited guest credential.
//! Retained control connection delegates ONLY interrupt/retire/status to root.
use super::{
    refused, Authorized, Channel, ProcessPhase, Request, Response, Selection, DEADLINE_SECS,
};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(crate) struct RemoteLaunch {
    stdin: File,
    stdout: File,
    stderr: File,
    control: RemoteControl,
}
pub(crate) struct RemoteControl {
    channel: Mutex<Channel>,
    selection: Selection,
    operation: String,
    pid: u32,
}
impl RemoteLaunch {
    pub(crate) fn into_parts(self) -> (File, File, File, RemoteControl) {
        (self.stdin, self.stdout, self.stderr, self.control)
    }
}
impl RemoteControl {
    /// Observation for the adapter identity only. Never used to signal/adopt.
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }
    fn request(&self, request: Request) -> io::Result<ProcessPhase> {
        self.request_until(request, Instant::now() + Duration::from_secs(DEADLINE_SECS))
    }
    fn request_until(&self, request: Request, until: Instant) -> io::Result<ProcessPhase> {
        let until = until.min(Instant::now() + Duration::from_secs(DEADLINE_SECS));
        let mut channel = loop {
            if Instant::now() >= until {
                return Err(refused());
            }
            match self.channel.try_lock() {
                Ok(channel) => break channel,
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(std::sync::TryLockError::Poisoned(_)) => return Err(refused()),
            }
        };
        if channel.consumed {
            return Err(refused());
        }
        // Fresh IO window cannot renew the external runtime/current authority.
        channel.until = until;
        let result = (|| match channel.exchange(&request)? {
            Response::State {
                version: 1,
                selection,
                operation,
                pid,
                phase,
            } if selection == self.selection && operation == self.operation && pid == self.pid => {
                Ok(phase)
            }
            _ => Err(refused()),
        })();
        if result.is_err() {
            channel.consumed = true;
        }
        result
    }
    pub(crate) fn interrupt(&self) -> io::Result<()> {
        self.request(Request::Interrupt {
            version: 1,
            selection: self.selection.clone(),
            operation: self.operation.clone(),
        })
        .map(|_| ())
    }
    pub(crate) fn retire(&self) -> io::Result<()> {
        if self.request(Request::Retire {
            version: 1,
            selection: self.selection.clone(),
            operation: self.operation.clone(),
        })? != ProcessPhase::Exited
        {
            return Err(refused());
        }
        Ok(())
    }
    pub(crate) fn status_until(&self, until: Instant) -> io::Result<ProcessPhase> {
        self.request_until(
            Request::Status {
                version: 1,
                selection: self.selection.clone(),
                operation: self.operation.clone(),
            },
            until,
        )
    }
}
impl Drop for RemoteControl {
    fn drop(&mut self) {
        // Root MUST treat owned control EOF as cancellation of its actual child,
        // not trusted external retirement. No blocking RPC or guessed-PID kill.
        if let Ok(channel) = self.channel.get_mut() {
            let _ = channel.stream.shutdown(std::net::Shutdown::Both);
        }
    }
}
impl Channel {
    pub(crate) fn launch(mut self, launch: &Authorized) -> io::Result<RemoteLaunch> {
        launch.validate(&launch.selection)?;
        match self.exchange(&Request::Launch {
            version: 1,
            selection: launch.selection.clone(),
            operation: launch.operation.clone(),
        })? {
            Response::Started {
                version: 1,
                selection,
                operation,
                pid,
            } if selection == launch.selection && operation == launch.operation && pid > 0 => {
                let [stdin, stdout, stderr] = receive_stdio(&self.stream, self.until)?;
                Ok(RemoteLaunch {
                    stdin,
                    stdout,
                    stderr,
                    control: RemoteControl {
                        channel: Mutex::new(self),
                        selection,
                        operation,
                        pid,
                    },
                })
            }
            _ => Err(refused()),
        }
    }
}
#[cfg(target_os = "linux")]
fn receive_stdio(stream: &UnixStream, until: Instant) -> io::Result<[File; 3]> {
    // Max-control space deliberately bounded; EVERY received right is adopted
    // before validation, so mismatches/truncation cannot leak descriptors.
    let mut control = [0usize; 16];
    let mut tag = 0u8;
    let mut iov = libc::iovec {
        iov_base: (&mut tag as *mut u8).cast(),
        iov_len: 1,
    };
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control);
    let remaining = until
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(refused)?;
    stream.set_read_timeout(Some(remaining))?;
    let n = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
    let mut fds = Vec::new();
    let mut valid = true;
    let mut count = 0;
    unsafe {
        let mut c = libc::CMSG_FIRSTHDR(&message);
        while !c.is_null() {
            count += 1;
            if (*c).cmsg_len < libc::CMSG_LEN(0) as usize {
                valid = false;
                break;
            }
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let size = (*c).cmsg_len - libc::CMSG_LEN(0) as usize;
                if !size.is_multiple_of(std::mem::size_of::<i32>()) {
                    valid = false;
                }
                for i in 0..size / std::mem::size_of::<i32>() {
                    let raw = *libc::CMSG_DATA(c).cast::<i32>().add(i);
                    if raw < 0 {
                        valid = false;
                    } else {
                        fds.push(OwnedFd::from_raw_fd(raw));
                    }
                }
            } else {
                // Still walk/adopt any later rights before refusing extra
                // credentials/control: an invalid ancillary cannot leak FDs.
                valid = false;
            }
            c = libc::CMSG_NXTHDR(&message, c);
        }
    }
    if n != 1
        || tag != 0x50
        || !valid
        || count != 1
        || fds.len() != 3
        || message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0
        || Instant::now() >= until
    {
        return Err(refused());
    }
    for (i, fd) in fds.iter().enumerate() {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        let descriptor_flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) } != 0
            || stat.st_mode & libc::S_IFMT != libc::S_IFIFO
            || flags < 0
            || flags & libc::O_ACCMODE
                != if i == 0 {
                    libc::O_WRONLY
                } else {
                    libc::O_RDONLY
                }
            || descriptor_flags < 0
            || descriptor_flags & libc::FD_CLOEXEC == 0
        {
            return Err(refused());
        }
    }
    let mut files = fds.into_iter().map(File::from);
    Ok([
        files.next().ok_or_else(refused)?,
        files.next().ok_or_else(refused)?,
        files.next().ok_or_else(refused)?,
    ])
}
#[cfg(not(target_os = "linux"))]
fn receive_stdio(_stream: &UnixStream, _until: Instant) -> io::Result<[File; 3]> {
    Err(refused())
}
