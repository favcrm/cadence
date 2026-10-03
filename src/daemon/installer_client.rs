//! CAD-1113: fixed, private, verify-only installer client transport.
//!
//! No CLI, HTTP, environment, path or command selection. Production admission
//! refuses before reading stdin: carrier drop/observer retirement, operational
//! trust and exact durable enrollments are not supplied by this source batch.
//! The ordinary-UID tests exercise sockets, not those unavailable authorities.
//! `Consumed` below is an acknowledgement only, NEVER protected-launch success.
#![allow(dead_code)]

use super::{installer_enrollment_wire as receipt, supervisor_grant as grant};
use crate::error::{Error, Result};
use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

const MAX_FRAME: usize = 32 * 1024; // includes every separator and newline
const BUDGET: Duration = Duration::from_secs(10);
const SOCKET: &str = "/run/cadence-supervisor/grant.sock";

fn unknown() -> Error {
    // Never interpolate a peer response, capsule, grant, receipt or OS path.
    Error::unknown("installer transport UNKNOWN — refused, no retry or launch evidence")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Challenge,
    Install,
}
impl Action {
    fn verb(self) -> &'static str {
        match self {
            Self::Challenge => "challenge-v1",
            Self::Install => "install-v1",
        }
    }
}

/// Two distinct generations: the admitted installer and the grant recipient.
/// Caller syntax is correlation data, not an enrollment or a trust pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Correlation {
    pub(super) operation: String,
    pub(super) installer_generation: String,
    pub(super) recipient_generation: String,
}
fn hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn operation(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, &c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_digit() || (b'a'..=b'f').contains(&c)
            }
        })
}

/// Shared transport schema only; cryptographic parsing remains in the merged
/// grant/receipt consumers. The server admits the kernel peer independently.
pub(super) struct Request<'a> {
    pub(super) action: Action,
    pub(super) correlation: Correlation,
    pub(super) envelope: Option<&'a str>,
}
impl<'a> Request<'a> {
    pub(super) fn parse(line: &'a str) -> Result<Self> {
        // Input excludes newline. No trim/normalization or extra fields.
        if line.len() + 1 > MAX_FRAME || !line.is_ascii() {
            return Err(unknown());
        }
        let fields: Vec<_> = line.split(' ').collect();
        let action = match fields.first().copied() {
            Some("challenge-v1") if fields.len() == 4 => Action::Challenge,
            Some("install-v1") if fields.len() == 5 => Action::Install,
            _ => return Err(unknown()),
        };
        if !operation(fields[1]) || !hex(fields[2], 32) || !hex(fields[3], 32) {
            return Err(unknown());
        }
        let envelope = (action == Action::Install).then(|| fields[4]);
        if envelope.is_some_and(|e| {
            e.is_empty()
                || !e
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        }) {
            return Err(unknown());
        }
        Ok(Self {
            action,
            correlation: Correlation {
                operation: fields[1].into(),
                installer_generation: fields[2].into(),
                recipient_generation: fields[3].into(),
            },
            envelope,
        })
    }
    fn frame(&self) -> Vec<u8> {
        let c = &self.correlation;
        let mut s = format!(
            "{} {} {} {}",
            self.action.verb(),
            c.operation,
            c.installer_generation,
            c.recipient_generation
        );
        if let Some(e) = self.envelope {
            s.push(' ');
            s.push_str(e);
        }
        s.push('\n');
        s.into_bytes()
    }
    pub(super) fn acknowledgement(&self, pid: u32, starttime: u64) -> String {
        let c = &self.correlation;
        match self.action {
            Action::Challenge => format!(
                "ok challenge-v1 {} {} {} {pid} {starttime}\n",
                c.operation, c.installer_generation, c.recipient_generation
            ),
            Action::Install => format!(
                "ok consumed-v1 {} {} {}\n",
                c.operation, c.installer_generation, c.recipient_generation
            ),
        }
    }
}

/// Host stdin is exactly one newline-terminated frame, followed by EOF.
/// challenge-v1 has the shared four fields; install-v1 adds grant AND receipt.
/// Secret storage has no Debug implementation, and never escapes into errors.
struct Capsule(Vec<u8>);
impl Drop for Capsule {
    fn drop(&mut self) {
        // Best-effort storage cleanup, not a claim of hardened memory custody.
        self.0.fill(0);
    }
}
impl Capsule {
    fn parts(&self) -> Result<(Request<'_>, Option<&str>)> {
        let text = std::str::from_utf8(&self.0).map_err(|_| unknown())?;
        let line = text.strip_suffix('\n').ok_or_else(unknown)?;
        if line.contains(['\r', '\n', '\t', '\0']) {
            return Err(unknown());
        }
        if line.starts_with("install-v1 ") {
            let (wire, compact_receipt) = line.rsplit_once(' ').ok_or_else(unknown)?;
            if compact_receipt.is_empty() {
                return Err(unknown());
            }
            let req = Request::parse(wire)?;
            if req.action != Action::Install {
                return Err(unknown());
            }
            Ok((req, Some(compact_receipt)))
        } else {
            Ok((Request::parse(line)?, None))
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Deadline(Instant);
impl Deadline {
    pub(super) fn until(until: Instant) -> Self {
        Self(until)
    }
    fn new(budget: Duration) -> Self {
        Self(Instant::now() + budget)
    }
    pub(super) fn remaining(self) -> Result<Duration> {
        self.0
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(unknown)
    }
    pub(super) fn wait(self, fd: RawFd, events: i16) -> Result<()> {
        loop {
            let left = self.remaining()?;
            // Round up fractional milliseconds without extending the absolute deadline.
            let ms = left.as_millis().saturating_add(1).min(i32::MAX as u128) as i32;
            let mut p = libc::pollfd {
                fd,
                events,
                revents: 0,
            };
            let rc = unsafe { libc::poll(&mut p, 1, ms) };
            self.remaining()?;
            if rc > 0 && p.revents & libc::POLLNVAL == 0 {
                return Ok(());
            }
            if rc < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(unknown());
        }
    }
}

fn read_stdin(fd: RawFd, deadline: Deadline) -> Result<Capsule> {
    let mut capsule = Capsule(Vec::with_capacity(1024));
    let mut chunk = [0u8; 1024];
    loop {
        deadline.wait(fd, libc::POLLIN)?;
        // Read at most one byte over the hard cap to detect oversize, boundedly.
        let want = chunk.len().min(MAX_FRAME + 1 - capsule.0.len());
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), want) };
        deadline.remaining()?;
        if n == 0 {
            break;
        }
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(unknown());
        }
        capsule.0.extend_from_slice(&chunk[..n as usize]);
        chunk.fill(0);
        if capsule.0.len() > MAX_FRAME {
            return Err(unknown());
        }
    }
    capsule.parts()?;
    Ok(capsule)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Node {
    dev: u64,
    ino: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    ctime: (i64, i64),
}
fn node(file: &File) -> Result<Node> {
    use std::os::unix::fs::MetadataExt;
    let st = file.metadata().map_err(|_| unknown())?; // fstat of the held fd
    Ok(Node {
        dev: st.dev(),
        ino: st.ino(),
        uid: st.uid(),
        gid: st.gid(),
        mode: st.mode(),
        ctime: (st.ctime(), st.ctime_nsec()),
    })
}
fn open_at(parent: RawFd, name: &std::ffi::OsStr, directory: bool) -> Result<File> {
    let name = CString::new(name.as_bytes()).map_err(|_| unknown())?;
    let flags = libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | if directory {
            libc::O_RDONLY | libc::O_DIRECTORY
        } else {
            libc::O_PATH
        };
    let fd = unsafe { libc::openat(parent, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(unknown());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Anchored traversal retains EVERY descriptor (including the O_PATH socket).
/// Production only uses the fixed root/run/supervisor/socket table below.
pub(super) struct Topology {
    held: Vec<File>,
    snapshots: Vec<Node>,
}
pub(super) const POLICY: &[(&str, u32, u32, u32)] = &[
    ("/", 0, 0, libc::S_IFDIR | 0o755),
    ("run", 0, 0, libc::S_IFDIR | 0o755),
    ("cadence-supervisor", 21000, 21000, libc::S_IFDIR | 0o700),
    ("grant.sock", 21000, 21000, libc::S_IFSOCK | 0o600),
];
impl Topology {
    pub(super) fn capture(policy: &[(&str, u32, u32, u32)]) -> Result<Self> {
        let mut held = Vec::new();
        let mut snapshots = Vec::new();
        for (name, uid, gid, mode) in policy {
            let parent = held.last().map_or(libc::AT_FDCWD, |f: &File| f.as_raw_fd());
            let f = open_at(
                parent,
                std::ffi::OsStr::new(name),
                mode & libc::S_IFMT == libc::S_IFDIR,
            )?;
            let n = node(&f)?;
            if (n.uid, n.gid, n.mode) != (*uid, *gid, *mode) {
                return Err(unknown());
            }
            held.push(f);
            snapshots.push(n);
        }
        if held.len() < 2 {
            return Err(unknown());
        }
        Ok(Self { held, snapshots })
    }
    pub(super) fn recheck(&self, policy: &[(&str, u32, u32, u32)]) -> Result<()> {
        let current = Self::capture(policy)?;
        if current.snapshots != self.snapshots {
            return Err(unknown());
        }
        for (f, before) in self.held.iter().zip(&self.snapshots) {
            if node(f)? != *before {
                return Err(unknown());
            }
        }
        Ok(())
    }
    pub(super) fn connect(&self, deadline: Deadline) -> Result<UnixStream> {
        // The connect follows the held parent, not a re-resolved /run path.
        let parent = self.held[self.held.len() - 2].as_raw_fd();
        connect(
            Path::new(&format!("/proc/self/fd/{parent}/grant.sock")),
            deadline,
        )
    }
}
fn connect(path: &Path, deadline: Deadline) -> Result<UnixStream> {
    deadline.remaining()?;
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= addr.sun_path.len() || bytes.contains(&0) {
        return Err(unknown());
    }
    for (to, from) in addr.sun_path.iter_mut().zip(bytes) {
        *to = *from as libc::c_char;
    }
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(unknown());
    }
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    let rc = unsafe {
        libc::connect(
            fd,
            (&addr as *const libc::sockaddr_un).cast(),
            std::mem::size_of_val(&addr) as libc::socklen_t,
        )
    };
    if rc != 0 {
        let error = std::io::Error::last_os_error().raw_os_error();
        // AF_UNIX EAGAIN (full backlog) is NOT an in-progress connect.
        // Refuse it and EINTR without retry; only EINPROGRESS may complete.
        if error != Some(libc::EINPROGRESS) {
            return Err(unknown());
        }
        deadline.wait(fd, libc::POLLOUT)?;
        let mut error: libc::c_int = 0;
        let mut len = std::mem::size_of_val(&error) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut error as *mut libc::c_int).cast(),
                &mut len,
            )
        } != 0
            || error != 0
        {
            return Err(unknown());
        }
    }
    deadline.remaining()?;
    Ok(stream)
}

pub(super) fn write_frame(stream: &UnixStream, frame: &[u8], deadline: Deadline) -> Result<()> {
    if frame.len() > MAX_FRAME || frame.last() != Some(&b'\n') {
        return Err(unknown());
    }
    let mut offset = 0;
    while offset < frame.len() {
        deadline.wait(stream.as_raw_fd(), libc::POLLOUT)?;
        let n = unsafe {
            libc::send(
                stream.as_raw_fd(),
                frame[offset..].as_ptr().cast(),
                frame.len() - offset,
                libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
            )
        };
        deadline.remaining()?;
        if n > 0 {
            offset += n as usize;
        } else if n < 0
            && matches!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
            )
        {
            continue;
        } else {
            return Err(unknown());
        }
    }
    Ok(())
}
pub(super) fn read_response(stream: &UnixStream, deadline: Deadline) -> Result<Vec<u8>> {
    let mut response = Vec::with_capacity(256);
    let mut buf = [0; 1024];
    loop {
        deadline.wait(stream.as_raw_fd(), libc::POLLIN)?;
        let want = buf.len().min(MAX_FRAME + 1 - response.len());
        let n = unsafe {
            libc::recv(
                stream.as_raw_fd(),
                buf.as_mut_ptr().cast(),
                want,
                libc::MSG_DONTWAIT,
            )
        };
        deadline.remaining()?;
        if n == 0 {
            break;
        }
        if n < 0 {
            if matches!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err(unknown());
        }
        response.extend_from_slice(&buf[..n as usize]);
        if response.len() > MAX_FRAME {
            return Err(unknown());
        }
    }
    // EOF is part of the one-response frame. This rejects trailing frames as
    // well as an acknowledgement that never completes under the same deadline.
    if response.last() != Some(&b'\n') || response[..response.len() - 1].contains(&b'\n') {
        return Err(unknown());
    }
    Ok(response)
}

/// Private immutable enrolled identity, NOT derived from the capsule or peer.
/// No production constructor is available. Test literals confer no authority.
pub(super) struct Enrollment {
    pub(super) pid: u32,
    pub(super) starttime: u64,
    pub(super) generation: String,
    pub(super) digest: [u8; 32],
}
pub(super) fn measured_process(pid: u32, enrollment: &Enrollment, generation: &str) -> Result<()> {
    if pid != enrollment.pid || generation != enrollment.generation || enrollment.digest == [0; 32]
    {
        return Err(unknown());
    }
    let before = crate::peer::proc_starttime(pid).ok_or_else(unknown)?;
    if before != enrollment.starttime {
        return Err(unknown());
    }
    let digest = grant::peer_exe_digest(pid).map_err(|_| unknown())?;
    if digest != enrollment.digest || crate::peer::proc_starttime(pid) != Some(before) {
        return Err(unknown());
    }
    Ok(())
}
pub(super) fn admit_supervisor(
    stream: &UnixStream,
    enrollment: &Enrollment,
    generation: &str,
) -> Result<()> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    } != 0
        || len as usize != std::mem::size_of_val(&cred)
        || cred.uid != grant::SUPERVISOR_UID
        || cred.gid != grant::SUPERVISOR_UID
        || cred.pid <= 0
    {
        return Err(unknown());
    }
    measured_process(cred.pid as u32, enrollment, generation)
}
fn admit_self(enrollment: &Enrollment, generation: &str) -> Result<()> {
    let mut ids = [0; 3];
    if unsafe { libc::getresuid(&mut ids[0], &mut ids[1], &mut ids[2]) } != 0 || ids != [21000; 3] {
        return Err(unknown());
    }
    if unsafe { libc::getresgid(&mut ids[0], &mut ids[1], &mut ids[2]) } != 0 || ids != [21000; 3] {
        return Err(unknown());
    }
    if unsafe { libc::getgroups(0, std::ptr::null_mut()) } != 0 {
        return Err(unknown());
    }
    // Inspect actual kernel capability sets, never a caller report. The
    // privileged carrier/drop and physical observer retirement are still
    // unavailable prerequisites; these checks do not invent that history.
    use std::io::Read;
    let mut status = Vec::new();
    File::open("/proc/self/status")
        .map_err(|_| unknown())?
        .take(64 * 1024 + 1)
        .read_to_end(&mut status)
        .map_err(|_| unknown())?;
    if status.len() > 64 * 1024 {
        return Err(unknown());
    }
    let status = std::str::from_utf8(&status).map_err(|_| unknown())?;
    for key in ["CapInh:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:"] {
        let mut fields = status.lines().filter_map(|line| line.strip_prefix(key));
        let value = fields.next().ok_or_else(unknown)?.trim();
        if value.len() != 16 || !value.bytes().all(|b| b == b'0') || fields.next().is_some() {
            return Err(unknown());
        }
    }
    measured_process(std::process::id(), enrollment, generation)
}

struct Admission {
    installer: Enrollment,
    supervisor: Enrollment,
    correlation: Correlation,
    binding_json: Vec<u8>, // exact externally enrolled prepared binding, no ledger
}
fn production_admission() -> Result<Admission> {
    Err(unknown()) // no root/drop/capability/observer-retirement/enrollment source
}

#[derive(Debug, PartialEq, Eq)]
enum TransportEvidence {
    Recipient,
    ConsumedAcknowledgement, // never grant, launch, retirement or eligibility
}
fn classify(
    request: &Request<'_>,
    response: &[u8],
    supervisor: &Enrollment,
) -> Result<TransportEvidence> {
    // Exact byte equality includes both generations and the operation. For
    // challenge, pid/starttime also equal the separately measured enrollment.
    if response
        != request
            .acknowledgement(supervisor.pid, supervisor.starttime)
            .as_bytes()
    {
        return Err(unknown());
    }
    Ok(match request.action {
        Action::Challenge => TransportEvidence::Recipient,
        Action::Install => TransportEvidence::ConsumedAcknowledgement,
    })
}

/// Only production entry: fixed host stdin, fixed socket, no caller knobs.
/// Admission factories run BEFORE any caller proof. No automatic retry exists.
pub(crate) fn from_host_stdin() -> Result<()> {
    let deadline = Deadline::new(BUDGET);
    let admission = production_admission()?;
    let keys = receipt::production_trust_set()?;
    grant::production_consume_factory()?;
    let stdin = std::io::stdin();
    let capsule = read_stdin(stdin.as_raw_fd(), deadline)?;
    let (request, compact_receipt) = capsule.parts()?;
    if request.correlation != admission.correlation {
        return Err(unknown());
    }
    admit_self(
        &admission.installer,
        &request.correlation.installer_generation,
    )?;
    if let Some(compact) = compact_receipt {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| unknown())?
            .as_millis();
        let verified = receipt::verify_receipt_format(
            compact,
            keys,
            u64::try_from(now).map_err(|_| unknown())?,
            BUDGET.saturating_sub(deadline.remaining()?).as_millis() as u64,
        )
        .map_err(|_| unknown())?;
        if verified.binding_json() != admission.binding_json {
            return Err(unknown());
        }
        grant::check_transport_binding(
            request.envelope.ok_or_else(unknown)?,
            now as u64 / 1000,
            &request.correlation.operation,
            &request.correlation.recipient_generation,
        )
        .map_err(|_| unknown())?;
    }
    let topology = Topology::capture(POLICY)?;
    let stream = topology.connect(deadline)?;
    topology.recheck(POLICY)?;
    admit_supervisor(
        &stream,
        &admission.supervisor,
        &request.correlation.recipient_generation,
    )?;
    deadline.remaining()?;
    let mut frame = request.frame();
    let written = write_frame(&stream, &frame, deadline);
    frame.fill(0);
    written?;
    let response = read_response(&stream, deadline)?;
    topology.recheck(POLICY)?;
    admit_supervisor(
        &stream,
        &admission.supervisor,
        &request.correlation.recipient_generation,
    )?;
    admit_self(
        &admission.installer,
        &request.correlation.installer_generation,
    )?;
    deadline.remaining()?;
    let _ack_only = classify(&request, &response, &admission.supervisor)?;
    // Even a valid consumed acknowledgement cannot qualify a protected launch.
    Err(unknown())
}

#[cfg(test)]
mod tests;
