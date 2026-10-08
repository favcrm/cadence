//! Fixed private protected-Pi protocol. No public/network authority endpoint.
//! The service owns provisioning, process custody and the one-use current CAS;
//! these messages contain selectors, never caller attestations or credentials.
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
#[cfg(target_os = "linux")]
use std::time::Duration;
use std::time::Instant;

pub(crate) const ENDPOINT: &str = "/run/cadence/private/pi-launch.sock";
pub(crate) const SUPERVISOR_UID: u32 = 21000;
pub(crate) const GUEST_UID: u32 = 21001;
pub(crate) const MAX_REQUEST: usize = 4096;
pub(crate) const MAX_FRAME: usize = 8 * 1024 * 1024;
pub(crate) const DEADLINE_SECS: u64 = 30;

#[cfg(test)]
#[path = "authority/request_bound_check.rs"]
mod request_bound_check;

#[path = "transport.rs"]
mod transport;
pub(crate) type RemoteLaunch = transport::RemoteLaunch;
pub(crate) type RemoteControl = transport::RemoteControl;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Role {
    Master,
    Worker,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Selection {
    pub alias_sha256: String,
    pub generation: String,
    pub role: Role,
    pub model: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    Provision {
        version: u32,
        selection: Selection,
        alias: String,
    },
    Arm {
        version: u32,
        selection: Selection,
    },
    Consume {
        version: u32,
        selection: Selection,
        operation: String,
    },
    Launch {
        version: u32,
        selection: Selection,
        operation: String,
    },
    Interrupt {
        version: u32,
        selection: Selection,
        operation: String,
    },
    Retire {
        version: u32,
        selection: Selection,
        operation: String,
    },
    Status {
        version: u32,
        selection: Selection,
        operation: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphFile {
    /// Canonical relative name under the fixed image root; no symlinks.
    pub path: String,
    pub size: u64,
    pub mode: u32,
    pub sha256: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImageProfile {
    pub helper_sha256: [u8; 32],
    pub node_sha256: [u8; 32],
    pub cli: String,
    /// Explicit approved immutable entrypoints, not config discovery.
    pub extensions: Vec<String>,
    /// Complete, sorted graph inventory, including Node and all Pi modules.
    pub files: Vec<GraphFile>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Authorized {
    pub version: u32,
    pub selection: Selection,
    pub alias: String,
    /// Non-bearer operation identifier. Only this retained connection may consume.
    pub operation: String,
    pub supervisor: u32,
    pub guest: u32,
    pub guest_gid: u32,
    pub shared_gid: u32,
    pub image: ImageProfile,
}
/// Public scope descriptor, NOT a caller capability or owned-process proof.
/// Shared unchanged with the root carrier and purpose signature payload.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperationScope {
    pub(crate) version: u32,
    pub(crate) binding_json: String,
    pub(crate) global: String,
    pub(crate) company: String,
    pub(crate) epoch: u64,
    pub(crate) lineage: String,
    pub(crate) database_epoch: u64,
    pub(crate) alias: String,
    pub(crate) selection: Selection,
    pub(crate) helper_sha256: [u8; 32],
    pub(crate) node_sha256: [u8; 32],
    pub(crate) profile_sha256: [u8; 32],
    pub(crate) policy_sha256: [u8; 32],
}
/// Original externally signed public operation, never echoed public TRUST.
/// Only LaunchPermit projects this from its retained authenticated origin.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SignedOperation {
    pub(crate) authorization: String,
    pub(crate) scope: OperationScope,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Response {
    Authorized {
        launch: Authorized,
        signed: Box<SignedOperation>,
    },
    Consumed {
        version: u32,
        selection: Selection,
        operation: String,
    },
    Started {
        version: u32,
        selection: Selection,
        operation: String,
        pid: u32,
    },
    State {
        version: u32,
        selection: Selection,
        operation: String,
        pid: u32,
        phase: ProcessPhase,
    },
    Refused,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProcessPhase {
    Running,
    Exited,
}
pub(crate) fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "protected Pi owner authorization refused/UNKNOWN",
    )
}
impl Selection {
    pub(crate) fn validate(&self) -> io::Result<()> {
        let hex = |s: &str, n| {
            s.len() == n
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if !hex(&self.alias_sha256, 64) || !hex(&self.generation, 32) || !self.model.contains('/') {
            // A protected operation authenticates provider/model, never a bare
            // id that Pi may resolve to a different registered provider.
            return Err(refused());
        }
        super::Routing::for_agent(self.role == Role::Master, &self.model).map_err(|_| refused())?;
        Ok(())
    }
}
impl Authorized {
    pub(crate) fn validate(&self, selection: &Selection) -> io::Result<()> {
        use sha2::{Digest, Sha256};
        selection.validate()?;
        let alias_hash: String = Sha256::digest(self.alias.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if self.version != 1
            || &self.selection != selection
            || self.supervisor != SUPERVISOR_UID
            || self.guest != GUEST_UID
            || self.guest_gid == 0
            || self.shared_gid == 0
            || self.alias.is_empty()
            || self.alias.len() > 192
            || self.alias.contains(['\0', '\n', '\r'])
            || alias_hash != selection.alias_sha256
            || self.operation.len() != 32
            || !self
                .operation
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(refused());
        }
        self.image.validate()
    }
}
impl ImageProfile {
    pub(crate) fn validate(&self) -> io::Result<()> {
        let relative = |s: &str| {
            !s.is_empty()
                && s.len() <= 512
                && !s.contains('\0')
                && s.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
                && !s.starts_with('/')
        };
        if self.helper_sha256 == [0; 32]
            || self.node_sha256 == [0; 32]
            || !relative(&self.cli)
            || self.extensions.is_empty()
            || self.extensions.len() > 32
            || self.files.is_empty()
            || self.files.len() > 16384
        {
            return Err(refused());
        }
        let mut prior = "";
        let mut bytes = 0u64;
        for file in &self.files {
            if !relative(&file.path)
                || file.path.as_str() <= prior
                || ![0o644, 0o755].contains(&file.mode)
                || file.sha256 == [0; 32]
                || file.size > 512 * 1024 * 1024
            {
                return Err(refused());
            }
            prior = &file.path;
            bytes = bytes.checked_add(file.size).ok_or_else(refused)?;
        }
        if bytes > 1024 * 1024 * 1024 {
            return Err(refused());
        }
        let node = self
            .files
            .iter()
            .find(|f| f.path == "node")
            .ok_or_else(refused)?;
        if node.sha256 != self.node_sha256 || node.mode != 0o755 {
            return Err(refused());
        }
        let entry = |name: &str| relative(name) && self.files.iter().any(|f| f.path == name);
        if !entry(&self.cli) {
            return Err(refused());
        }
        let mut unique = std::collections::BTreeSet::new();
        for ext in &self.extensions {
            if !entry(ext) || !unique.insert(ext) {
                return Err(refused());
            }
        }
        Ok(())
    }
}
/// Created privately after close_fds in the helper; CLOEXEC and never sent onward.
/// Client authenticates the fixed endpoint's topology and kernel peer. Server
/// MUST independently authenticate owned callers; SO_PEERCRED alone is insufficient.
pub(crate) struct Channel {
    stream: UnixStream,
    until: Instant,
    consumed: bool,
}
impl Channel {
    #[cfg(target_os = "linux")]
    pub(crate) fn connect() -> io::Result<Self> {
        // Resolve each parent without symlinks, including the private principal.
        for (path, uid, mode) in [
            ("/run", 0, None),
            ("/run/cadence", 0, None),
            ("/run/cadence/private", SUPERVISOR_UID, Some(0o700)),
        ] {
            let m = std::fs::symlink_metadata(path)?;
            if !m.is_dir()
                || m.uid() != uid
                || m.mode() & 0o022 != 0
                || mode.is_some_and(|v| m.mode() & 0o7777 != v)
            {
                return Err(refused());
            }
        }
        let before = std::fs::symlink_metadata(ENDPOINT)?;
        use std::os::unix::fs::FileTypeExt;
        if !before.file_type().is_socket()
            || before.uid() != SUPERVISOR_UID
            || before.mode() & 0o7777 != 0o600
        {
            return Err(refused());
        }
        let until = Instant::now() + Duration::from_secs(DEADLINE_SECS);
        let raw = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (to, from) in address.sun_path.iter_mut().zip(ENDPOINT.bytes()) {
            *to = from as libc::c_char;
        }
        if unsafe {
            libc::connect(
                fd.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        } != 0
        {
            if io::Error::last_os_error().raw_os_error() != Some(libc::EINPROGRESS) {
                return Err(io::Error::last_os_error());
            }
            let mut event = libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            if unsafe { libc::poll(&mut event, 1, (DEADLINE_SECS * 1000) as i32) } != 1 {
                return Err(refused());
            }
            let mut error = 0i32;
            let mut len = std::mem::size_of::<i32>() as libc::socklen_t;
            if unsafe {
                libc::getsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    (&mut error as *mut i32).cast(),
                    &mut len,
                )
            } != 0
                || error != 0
            {
                return Err(refused());
            }
        }
        let stream = UnixStream::from(fd);
        stream.set_nonblocking(false)?;
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        } != 0
            || len as usize != std::mem::size_of::<libc::ucred>()
            // SO_PEERCRED reflects ROOT's creation/listen credentials, not
            // the sealed21000 proxy that accepts and forwards this stream.
            // Root UID is endpoint provenance only: the actual dispatcher
            // must ALSO verify runtime/current purpose and owned kernel caller.
            // Root outside the helper's child PID namespace is reported as
            // pid0. Never use that translated observation as authority.
            || cred.uid != 0
        {
            return Err(refused());
        }
        let after = std::fs::symlink_metadata(ENDPOINT)?;
        if (
            before.dev(),
            before.ino(),
            before.ctime(),
            before.ctime_nsec(),
        ) != (after.dev(), after.ino(), after.ctime(), after.ctime_nsec())
        {
            return Err(refused());
        }
        Ok(Self {
            stream,
            until,
            consumed: false,
        })
    }
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn connect() -> io::Result<Self> {
        Err(refused())
    }
    pub(crate) fn bound_until(&mut self, until: Instant) -> io::Result<()> {
        self.until = self.until.min(until);
        self.timeout()
    }
    fn timeout(&self) -> io::Result<()> {
        let remaining = self
            .until
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(refused)?;
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.set_write_timeout(Some(remaining))
    }
    fn write_frame(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            self.timeout()?;
            let n = self.stream.write(bytes)?;
            if n == 0 {
                return Err(refused());
            }
            bytes = &bytes[n..];
        }
        Ok(())
    }
    fn read_frame(&mut self, mut bytes: &mut [u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            self.timeout()?;
            let n = self.stream.read(bytes)?;
            if n == 0 {
                return Err(refused());
            }
            bytes = &mut bytes[n..];
        }
        Ok(())
    }
    pub(crate) fn exchange(&mut self, request: &Request) -> io::Result<Response> {
        if self.consumed {
            return Err(refused());
        }
        // Burn BEFORE sending. Lost ACK remains UNKNOWN; never retry/reconnect.
        if matches!(request, Request::Consume { .. } | Request::Retire { .. }) {
            self.consumed = true;
        }
        let frame = serde_json::to_vec(request).map_err(|_| refused())?;
        if frame.len() > MAX_REQUEST {
            return Err(refused());
        }
        self.write_frame(&(frame.len() as u32).to_be_bytes())?;
        self.write_frame(&frame)?;
        let mut size = [0; 4];
        self.read_frame(&mut size)?;
        let size = u32::from_be_bytes(size) as usize;
        if size == 0 || size > MAX_FRAME {
            return Err(refused());
        }
        let mut frame = vec![0; size];
        self.read_frame(&mut frame)?;
        self.timeout()?;
        serde_json::from_slice(&frame).map_err(|_| refused())
    }
    pub(crate) fn authorize(
        &mut self,
        request: &Request,
        selection: &Selection,
    ) -> io::Result<Authorized> {
        self.authorize_signed(request, selection)
            .map(|(launch, _)| launch)
    }
    /// Helper MUST additionally authenticate signed against independently
    /// qualified fixed-media trust BEFORE any view/graph/Node effects.
    pub(crate) fn authorize_signed(
        &mut self,
        request: &Request,
        selection: &Selection,
    ) -> io::Result<(Authorized, SignedOperation)> {
        match self.exchange(request)? {
            Response::Authorized { launch, signed } => {
                launch.validate(selection)?;
                if signed.authorization.is_empty()
                    || signed.authorization.len() > 32768
                    || signed.scope.version != 1
                    || signed.scope.selection != *selection
                    || signed.scope.alias != launch.alias
                {
                    return Err(refused());
                }
                Ok((launch, *signed))
            }
            _ => Err(refused()),
        }
    }
    pub(crate) fn consume(mut self, launch: &Authorized) -> io::Result<()> {
        match self.exchange(&Request::Consume {
            version: 1,
            selection: launch.selection.clone(),
            operation: launch.operation.clone(),
        })? {
            Response::Consumed {
                version: 1,
                selection,
                operation,
            } if selection == launch.selection && operation == launch.operation => Ok(()),
            _ => Err(refused()),
        }
    }
}
