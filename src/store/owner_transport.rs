//! Client for the fixed private Store owner service. This is NOT a generic
//! authority RPC: finite startup/acquire/consume/current/database_current, exact
//! bindings, one retained authenticated channel, no credentials, no
//! reconnect/retry after ACK loss.
//! The constructor service must admit actual enrolled caller custody and relay
//! durable external consume/current with exclusive database-directory custody.
//! A socket peer/Binding cannot issue a grant locally.

use super::Binding;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

const ENDPOINT: &str = "/run/cadence/private/store-owner.sock";
const OWNER_UID: u32 = 21000;
// ROOT creates/listens before handing an accepted FD to an optional sealed
// proxy. SO_PEERCRED identifies that listener, not the socket inode's owner.
// This authenticates transport only; the service must retain real caller
// custody, qualified runtime scope and actual durable external facts.
const SERVER_UID: u32 = 0;
const SERVER_GID: u32 = 0;
const MAX_FRAME: usize = 32 * 1024;
const IO_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request<'a> {
    /// No caller fields: the authenticated owner elects the current startup
    /// purpose, DB binding and durable grant reference for this enrolled peer.
    Startup { version: u8, sequence: u64 },
    Acquire {
        version: u8,
        sequence: u64,
        binding: &'a Binding,
    },
    Consume {
        version: u8,
        sequence: u64,
        grant: &'a str,
        binding: &'a Binding,
    },
    Current {
        version: u8,
        sequence: u64,
        grant: &'a str,
        binding: &'a Binding,
    },
    /// Exact original consumed opening tuple, not another activation/consume.
    /// ROOT must recheck live runtime/kernel custody and durable consumed state.
    DatabaseCurrent {
        version: u8,
        sequence: u64,
        grant: &'a str,
        binding: &'a Binding,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    version: u8,
    sequence: u64,
    grant: String,
    binding: Binding,
    outcome: Outcome,
    phase: Phase,
}
#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Issued,
    Consumed,
}
#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Issued,
    Consumed,
    Current,
    DatabaseCurrent,
    Unknown,
}
#[derive(Clone, Copy)]
enum ExchangeKind {
    Consume,
    ActivationCurrent,
    DatabaseCurrent,
}

#[cfg(target_os = "linux")]
struct Channel {
    stream: std::os::unix::net::UnixStream,
    sequence: u64,
    socket: (u64, u64),
    peer_pid: i32,
    last_wall: i64,
}
#[cfg(not(target_os = "linux"))]
struct Channel;

/// Opaque capability from an authenticated owner reply. No Default/Clone,
/// public fields, deserialization or local literal/synthetic issuance exists.
/// The grant id is only correlation on THIS channel, never a bearer credential.
pub(crate) struct StoreOwnerGrant {
    binding: Binding,
    grant: String,
    channel: Mutex<Channel>,
    consumed: AtomicBool,
    unknown: AtomicBool,
}
impl StoreOwnerGrant {
    /// Fetch and acquire startup authority only from the fixed private owner
    /// service. Never accepts caller fields, a snapshot marker, UID or lease as
    /// authority. The returned binding remains sealed inside this capability.
    pub(crate) fn startup() -> Result<Self> {
        let mut channel = Channel::connect()?;
        let response = channel.exchange(&Request::Startup {
            version: 1,
            sequence: 1,
        })?;
        if !matches!(
            response.binding.purpose,
            super::Purpose::Init | super::Purpose::Restore | super::Purpose::Open
        ) {
            return Err(Error::rejected(
                "Store startup grant authorizes a different operation",
            ));
        }
        Self::issued(channel, response)
    }

    pub(crate) fn acquire(binding: Binding) -> Result<Self> {
        binding.validate()?;
        if binding.path != super::DATABASE_PATH {
            return Err(Error::rejected(
                "Store owner selector names a different protected database path",
            ));
        }
        let mut channel = Channel::connect()?;
        let response = channel.exchange(&Request::Acquire {
            version: 1,
            sequence: 1,
            binding: &binding,
        })?;
        if response.binding != binding {
            return Err(Error::rejected("Store owner issuance binding unknown"));
        }
        Self::issued(channel, response)
    }

    fn issued(mut channel: Channel, response: Response) -> Result<Self> {
        if response.version != 1
            || response.sequence != 1
            || response.binding.path != super::DATABASE_PATH
            || response.outcome != Outcome::Issued
            || response.phase != Phase::Issued
            || !super::identifier(&response.grant)
        {
            return Err(Error::rejected("Store owner issuance binding unknown"));
        }
        response.binding.validate()?;
        channel.set_sequence(1);
        Ok(Self {
            binding: response.binding,
            grant: response.grant,
            channel: Mutex::new(channel),
            consumed: AtomicBool::new(false),
            unknown: AtomicBool::new(false),
        })
    }
    pub(crate) fn binding(&self) -> &Binding {
        &self.binding
    }
    pub(crate) fn consume(&self) -> Result<()> {
        if self.consumed.swap(true, Ordering::SeqCst) || self.unknown.load(Ordering::SeqCst) {
            return Err(Error::rejected(
                "Store owner permit spent or UNKNOWN; replay refused",
            ));
        }
        self.request(ExchangeKind::Consume)
    }
    /// Short activation/maintenance check, even after consumption. This never
    /// extends the original permit's deadline during opening or closure effects.
    pub(crate) fn recheck(&self) -> Result<()> {
        self.request(ExchangeKind::ActivationCurrent)
    }
    /// Business transactions use a DIFFERENT finite owner exchange. Local burn
    /// or an expired tuple alone is insufficient: ROOT must freshly corroborate
    /// actual consumed phase, live runtime and the same kernel/DB/lineage scope.
    pub(super) fn recheck_database(&self) -> Result<()> {
        self.request(ExchangeKind::DatabaseCurrent)
    }
    fn validate_for(&self, kind: ExchangeKind) -> Result<()> {
        if self.unknown.load(Ordering::SeqCst) {
            return Err(Error::rejected("Store owner outcome UNKNOWN; no replay"));
        }
        if matches!(kind, ExchangeKind::DatabaseCurrent) {
            if !self.consumed.load(Ordering::SeqCst)
                || !matches!(
                    self.binding.purpose,
                    super::Purpose::Init | super::Purpose::Restore | super::Purpose::Open
                )
            {
                return Err(Error::rejected(
                    "database current requires a consumed opening grant",
                ));
            }
            self.binding.validate_stable()
        } else {
            self.binding.validate()
        }
    }
    fn request(&self, kind: ExchangeKind) -> Result<()> {
        if self.unknown.load(Ordering::SeqCst) {
            return Err(Error::rejected("Store owner outcome UNKNOWN; no replay"));
        }
        let result = (|| {
            self.validate_for(kind)?;
            let mut channel = self
                .channel
                .lock()
                .map_err(|_| Error::rejected("Store owner channel poisoned"))?;
            // Serialize UNKNOWN publication with exchanges on this channel:
            // a queued business recheck cannot race past a failed predecessor.
            let result = (|| {
                self.validate_for(kind)?;
                let sequence = channel.next_sequence()?;
                let (request, outcome) = match kind {
                    ExchangeKind::Consume => (
                        Request::Consume {
                            version: 1,
                            sequence,
                            grant: &self.grant,
                            binding: &self.binding,
                        },
                        Outcome::Consumed,
                    ),
                    ExchangeKind::ActivationCurrent => (
                        Request::Current {
                            version: 1,
                            sequence,
                            grant: &self.grant,
                            binding: &self.binding,
                        },
                        Outcome::Current,
                    ),
                    ExchangeKind::DatabaseCurrent => (
                        Request::DatabaseCurrent {
                            version: 1,
                            sequence,
                            grant: &self.grant,
                            binding: &self.binding,
                        },
                        Outcome::DatabaseCurrent,
                    ),
                };
                let response = channel.exchange(&request)?;
                if response.version != 1
                    || response.sequence != sequence
                    || response.grant != self.grant
                    || response.binding != self.binding
                    || response.outcome != outcome
                    || response.phase
                        != if self.consumed.load(Ordering::SeqCst) {
                            Phase::Consumed
                        } else {
                            Phase::Issued
                        }
                {
                    return Err(Error::rejected(
                        "Store owner current/consumption is UNKNOWN",
                    ));
                }
                self.validate_for(kind)
            })();
            if result.is_err() {
                self.unknown.store(true, Ordering::SeqCst);
            }
            result
        })();
        if result.is_err() {
            self.unknown.store(true, Ordering::SeqCst);
        }
        result
    }
}

#[cfg(target_os = "linux")]
impl Channel {
    fn endpoint() -> Result<(u64, u64)> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let parent = std::fs::symlink_metadata("/run/cadence/private")?;
        let socket = std::fs::symlink_metadata(ENDPOINT)?;
        if !parent.is_dir()
            || parent.uid() != OWNER_UID
            || parent.mode() & 0o7777 != 0o700
            || !socket.file_type().is_socket()
            || socket.uid() != OWNER_UID
            || socket.mode() & 0o7777 != 0o600
        {
            return Err(Error::rejected(
                "private Store owner endpoint custody refused",
            ));
        }
        for path in ["/", "/run", "/run/cadence"] {
            let m = std::fs::symlink_metadata(path)?;
            if !m.is_dir() || !matches!(m.uid(), 0 | OWNER_UID) || m.mode() & 0o022 != 0 {
                return Err(Error::rejected(
                    "private Store owner endpoint ancestor custody refused",
                ));
            }
        }
        Ok((socket.dev(), socket.ino()))
    }
    fn peer(stream: &std::os::unix::net::UnixStream) -> Result<i32> {
        use std::os::fd::AsRawFd;
        let mut cred = libc::ucred {
            pid: 0,
            uid: u32::MAX,
            gid: u32::MAX,
        };
        let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut size,
            )
        };
        if rc != 0
            || size as usize != std::mem::size_of::<libc::ucred>()
            || cred.pid <= 0
            || cred.uid != SERVER_UID
            || cred.gid != SERVER_GID
        {
            return Err(Error::rejected(
                "Store owner kernel peer authentication refused",
            ));
        }
        Ok(cred.pid)
    }
    fn connect() -> Result<Self> {
        let socket = Self::endpoint()?;
        let stream = Self::connect_bounded()?;
        if Self::endpoint()? != socket {
            return Err(Error::rejected("Store owner endpoint replaced"));
        }
        let peer_pid = Self::peer(&stream)?;
        Ok(Self {
            stream,
            socket,
            peer_pid,
            sequence: 0,
            last_wall: super::super::super::now() as i64,
        })
    }
    fn connect_bounded() -> Result<std::os::unix::net::UnixStream> {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        let fd = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (target, byte) in address.sun_path.iter_mut().zip(ENDPOINT.bytes()) {
            *target = byte as libc::c_char;
        }
        let rc = unsafe {
            libc::connect(
                fd.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        };
        if rc != 0 {
            let error = std::io::Error::last_os_error();
            // Linux EAGAIN for a full Unix listener queue did NOT initiate a
            // connection. Refuse rather than treating it as success/retrying.
            if error.raw_os_error() != Some(libc::EINPROGRESS) {
                return Err(error.into());
            }
            let deadline = std::time::Instant::now() + IO_BUDGET;
            loop {
                let timeout = deadline
                    .checked_duration_since(std::time::Instant::now())
                    .filter(|d| !d.is_zero())
                    .ok_or_else(|| Error::rejected("Store owner connect deadline"))?;
                let mut poll = libc::pollfd {
                    fd: fd.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let rc = unsafe {
                    libc::poll(
                        &mut poll,
                        1,
                        timeout.as_millis().clamp(1, i32::MAX as u128) as i32,
                    )
                };
                if rc < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error.into());
                }
                if rc == 0 {
                    return Err(Error::rejected("Store owner connect deadline"));
                }
                let mut error: i32 = 0;
                let mut length = std::mem::size_of::<i32>() as libc::socklen_t;
                if unsafe {
                    libc::getsockopt(
                        fd.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_ERROR,
                        (&mut error as *mut i32).cast(),
                        &mut length,
                    )
                } != 0
                {
                    return Err(std::io::Error::last_os_error().into());
                }
                if error != 0 {
                    return Err(std::io::Error::from_raw_os_error(error).into());
                }
                break;
            }
        }
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(std::os::unix::net::UnixStream::from(fd))
    }
    fn set_sequence(&mut self, sequence: u64) {
        self.sequence = sequence;
    }
    fn next_sequence(&mut self) -> Result<u64> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| Error::rejected("Store owner sequence overflow"))?;
        Ok(self.sequence)
    }
    fn check(&mut self) -> Result<()> {
        let now = super::super::super::now() as i64;
        if now < self.last_wall {
            return Err(Error::rejected(
                "Store owner wall clock regressed; current UNKNOWN",
            ));
        }
        self.last_wall = now;
        if Self::endpoint()? != self.socket || Self::peer(&self.stream)? != self.peer_pid {
            return Err(Error::rejected("Store owner channel custody changed"));
        }
        Ok(())
    }
    fn exchange(&mut self, request: &Request<'_>) -> Result<Response> {
        use std::io::Write;
        let deadline = std::time::Instant::now() + IO_BUDGET;
        let frame = serde_json::to_vec(request)?;
        if frame.len() > MAX_FRAME {
            return Err(Error::rejected("Store owner request too large"));
        }
        self.check()?;
        let mut wire = (frame.len() as u32).to_be_bytes().to_vec();
        wire.extend(frame);
        let mut remaining = wire.as_slice();
        while !remaining.is_empty() {
            self.stream.set_write_timeout(Some(
                deadline
                    .checked_duration_since(std::time::Instant::now())
                    .filter(|d| !d.is_zero())
                    .ok_or_else(|| Error::rejected("Store owner write deadline"))?,
            ))?;
            let n = self.stream.write(remaining)?;
            if n == 0 {
                return Err(Error::rejected("Store owner channel lost; outcome UNKNOWN"));
            }
            remaining = &remaining[n..];
        }
        let mut length = [0u8; 4];
        self.read_until(&mut length, deadline)?;
        let n = u32::from_be_bytes(length) as usize;
        if n == 0 || n > MAX_FRAME {
            return Err(Error::rejected("Store owner reply too large"));
        }
        let mut reply = vec![0; n];
        self.read_until(&mut reply, deadline)?;
        self.check()?;
        // No unbounded read_to_end, trailing bytes, extra fields or caller key.
        let response: Response = serde_json::from_slice(&reply)?;
        Ok(response)
    }
    fn read_until(&mut self, mut bytes: &mut [u8], deadline: std::time::Instant) -> Result<()> {
        use std::io::Read;
        while !bytes.is_empty() {
            self.stream.set_read_timeout(Some(
                deadline
                    .checked_duration_since(std::time::Instant::now())
                    .filter(|d| !d.is_zero())
                    .ok_or_else(|| Error::rejected("Store owner read deadline"))?,
            ))?;
            let n = self.stream.read(bytes)?;
            if n == 0 {
                return Err(Error::rejected("Store owner ACK lost; outcome UNKNOWN"));
            }
            bytes = &mut bytes[n..];
        }
        Ok(())
    }
}
#[cfg(not(target_os = "linux"))]
impl Channel {
    fn connect() -> Result<Self> {
        Err(Error::rejected("authenticated Store owner requires Linux"))
    }
    fn set_sequence(&mut self, _: u64) {}
    fn next_sequence(&mut self) -> Result<u64> {
        Err(Error::rejected("authenticated Store owner requires Linux"))
    }
    fn exchange(&mut self, _: &Request<'_>) -> Result<Response> {
        Err(Error::rejected("authenticated Store owner requires Linux"))
    }
}
