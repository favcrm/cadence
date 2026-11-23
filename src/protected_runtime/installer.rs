//! The fixed, owner-launched native installer mode of the pinned root owner
//! binary (`--owner-launched-install`). It has no caller-selected argv, env,
//! path, command or socket. The owner writes one strict capsule after the
//! allocating intent commits; this process verifies the exact fixed profile,
//! its own/parent executable, the supervisor grant socket topology, performs
//! the existing `challenge-v1`/`install-v1` conversation with a locally pinned
//! issuer key, and exits. It never claims launch/readiness success: only an
//! exact `ok consumed-v1` ACK ends the process, and the coordinator still calls
//! the owner's actual `start_runtime`.
//!
//! Not reachable from the old `installer_bundle`/`installer_client` factories:
//! those remain unchanged and refuse. No secret, grant or capsule is printed.

use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use ring::signature::{Ed25519KeyPair, KeyPair};

use crate::daemon::supervisor_grant::{
    canonical_challenge_bytes, canonical_challenge_value, encode_grant_envelope,
    grant_signing_material, parse_challenge_bytes, Lineage, Pins, SupervisorChallenge,
};
use crate::error::{Error, Result};
use crate::protected_runtime::peer;
use crate::protected_runtime::profile::LocalProfile;
use crate::protected_runtime::registry;
use crate::protected_runtime::strict;
use crate::protected_runtime::{INSTALLER_LAUNCH_ARG, MAX_FRAME_BYTES, MAX_SAFE_INT};

const CAPSULE_WAIT: Duration = Duration::from_secs(60);
const DIALOGUE_BUDGET: Duration = Duration::from_secs(18);
const MAX_RESPONSE_BYTES: usize = 4096;
const MAX_GRANT_BYTES: usize = 24 * 1024;

fn unknown() -> Error {
    Error::unknown("owner-launched installer refused — no retry or launch evidence")
}

pub(crate) fn run() -> i32 {
    match run_inner() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

fn run_inner() -> Result<()> {
    require_root_identity()?;
    require_fixed_argv()?;
    let profile = LocalProfile::load_production()?;
    verify_self(&profile)?;
    verify_parent(&profile)?;
    let issuer = GrantIssuerKey::load(&profile)?;

    let capsule_bytes = read_stdin_to_eof(CAPSULE_WAIT, MAX_FRAME_BYTES + 1)?;
    let capsule = parse_capsule(&capsule_bytes, &profile)?;

    let deadline = Deadline::new(DIALOGUE_BUDGET);
    let topology = Topology::capture(&profile)?;

    let challenge_stream = topology.connect(deadline)?;
    admit_supervisor(&challenge_stream, &profile)?;
    let challenge_request = format!(
        "challenge-v1 {} {} {}\n",
        capsule.operation_id, capsule.installer_generation, capsule.recipient_generation
    );
    write_frame(&challenge_stream, challenge_request.as_bytes(), deadline)?;
    let challenge_response = read_response(&challenge_stream, deadline)?;
    topology.recheck(&profile)?;
    admit_supervisor(&challenge_stream, &profile)?;
    drop(challenge_stream);

    let expected_challenge_ack = format!(
        "ok challenge-v1 {} {} {} {} {}\n",
        capsule.operation_id,
        capsule.installer_generation,
        capsule.recipient_generation,
        capsule.challenge.recipient.pid,
        capsule.challenge.recipient.starttime
    );
    if challenge_response.as_slice() != expected_challenge_ack.as_bytes() {
        return Err(unknown());
    }

    let now = wall_clock_seconds()?;
    let nbf = now;
    let exp = now
        .checked_add(120)
        .filter(|v| *v <= MAX_SAFE_INT)
        .ok_or_else(unknown)?;
    let (header, payload, material) =
        grant_signing_material(&capsule.challenge, nbf, exp, issuer.kid())?;
    let signature = issuer.sign_material(&material);
    let envelope = encode_grant_envelope(&header, &payload, &signature)?;
    if envelope.len() > MAX_GRANT_BYTES || !wire_token(&envelope) {
        return Err(unknown());
    }

    let install_stream = topology.connect(deadline)?;
    admit_supervisor(&install_stream, &profile)?;
    let install_request = format!(
        "install-v1 {} {} {} {}\n",
        capsule.operation_id, capsule.installer_generation, capsule.recipient_generation, envelope
    );
    write_frame(&install_stream, install_request.as_bytes(), deadline)?;
    let install_response = read_response(&install_stream, deadline)?;
    topology.recheck(&profile)?;
    admit_supervisor(&install_stream, &profile)?;

    let expected_install_ack = format!(
        "ok consumed-v1 {} {} {}\n",
        capsule.operation_id, capsule.installer_generation, capsule.recipient_generation
    );
    if install_response.as_slice() != expected_install_ack.as_bytes() {
        return Err(unknown());
    }
    Ok(())
}

fn require_root_identity() -> Result<()> {
    let mut ids = [0u32; 3];
    if unsafe { libc::getresuid(&mut ids[0], &mut ids[1], &mut ids[2]) } != 0 || ids != [0u32; 3] {
        return Err(Error::rejected(
            "installer requires root real/effective/saved uid",
        ));
    }
    Ok(())
}

fn require_fixed_argv() -> Result<()> {
    let args: Vec<OsString> = std::env::args_os().collect();
    if args.len() != 2 || args[1].as_os_str() != OsStr::new(INSTALLER_LAUNCH_ARG) {
        return Err(Error::rejected(
            "installer argv is not the fixed launch contract",
        ));
    }
    Ok(())
}

fn verify_self(profile: &LocalProfile) -> Result<()> {
    let pid = std::process::id();
    let start =
        peer::starttime(pid).ok_or_else(|| Error::rejected("installer starttime unreadable"))?;
    let measured = format!("sha256:{}", peer::hex(&peer::exe_digest(pid)?));
    if measured != profile.installer().exe_digest {
        return Err(Error::rejected(
            "running installer is not the pinned installer binary",
        ));
    }
    if peer::starttime(pid) != Some(start) {
        return Err(Error::rejected(
            "installer pid changed during self-measurement",
        ));
    }
    Ok(())
}

fn verify_parent(profile: &LocalProfile) -> Result<()> {
    let ppid = unsafe { libc::getppid() };
    if ppid <= 1 {
        return Err(Error::rejected("installer has no owner parent"));
    }
    let status = std::fs::read_to_string(format!("/proc/{ppid}/status"))
        .map_err(|_| Error::rejected("owner parent status unreadable"))?;
    let uid = status
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or_else(|| Error::rejected("owner parent uid unreadable"))?;
    if uid != 0 {
        return Err(Error::rejected("installer parent is not root"));
    }
    let start = peer::starttime(ppid as u32)
        .ok_or_else(|| Error::rejected("owner parent starttime unreadable"))?;
    let measured = format!("sha256:{}", peer::hex(&peer::exe_digest(ppid as u32)?));
    if measured.as_str() != profile.owner_exe_digest() {
        return Err(Error::rejected(
            "installer parent is not the pinned owner binary",
        ));
    }
    if peer::starttime(ppid as u32) != Some(start) {
        return Err(Error::rejected("owner parent changed during measurement"));
    }
    Ok(())
}

/// The typed local grant issuer. It is the only object that can sign a grant
/// document; there is no generic sign/get-key API.
struct GrantIssuerKey {
    pair: Ed25519KeyPair,
    kid: String,
}

impl GrantIssuerKey {
    fn load(profile: &LocalProfile) -> Result<Self> {
        let pin = &profile.installer().grant_issuer;
        let path = &pin.private_key_file;
        let meta = std::fs::symlink_metadata(path)
            .map_err(|_| Error::rejected("grant issuer key is missing"))?;
        if !meta.is_file() || meta.is_symlink() {
            return Err(Error::rejected("grant issuer key must be a regular file"));
        }
        if meta.uid() != 0 || meta.mode() & 0o077 != 0 {
            return Err(Error::rejected(
                "grant issuer key must be root-owned and not group/other accessible",
            ));
        }
        if meta.len() == 0 || meta.len() > 4096 {
            return Err(Error::rejected(
                "grant issuer key size is outside the bound",
            ));
        }
        let bytes = zeroize::Zeroizing::new(
            std::fs::read(path).map_err(|_| Error::rejected("grant issuer key is unreadable"))?,
        );
        // OpenSSL emits PKCS#8 v1; ring's strict parser accepts only v2. Both
        // formats derive the public key, which MUST equal the profile pin.
        let pair = Ed25519KeyPair::from_pkcs8_maybe_unchecked(&bytes[..])
            .map_err(|_| Error::rejected("grant issuer key is not Ed25519 PKCS#8"))?;
        let public: [u8; 32] = pair
            .public_key()
            .as_ref()
            .try_into()
            .map_err(|_| Error::rejected("grant issuer key has no 32-byte public half"))?;
        let pinned = profile
            .issuer_keys()
            .iter()
            .find(|key| key.kid.as_str() == pin.kid.as_str())
            .ok_or_else(|| Error::rejected("grant issuer kid is not pinned"))?;
        if public != pinned.x {
            return Err(Error::rejected(
                "grant issuer key does not match the pinned issuer public key",
            ));
        }
        Ok(Self {
            pair,
            kid: pin.kid.clone(),
        })
    }

    fn kid(&self) -> &str {
        &self.kid
    }

    fn sign_material(&self, material: &[u8]) -> [u8; 64] {
        let signature = self.pair.sign(material);
        let mut out = [0u8; 64];
        out.copy_from_slice(signature.as_ref());
        out
    }
}

struct Capsule {
    operation_id: String,
    installer_generation: String,
    recipient_generation: String,
    challenge: SupervisorChallenge,
}

fn parse_capsule(bytes: &[u8], profile: &LocalProfile) -> Result<Capsule> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES || bytes.last() != Some(&b'\n') {
        return Err(unknown());
    }
    let body = &bytes[..bytes.len() - 1];
    if body.contains(&b'\n') || body.contains(&b'\r') || body.contains(&0) {
        return Err(unknown());
    }
    let value = strict::parse(body)?;
    let m = strict::object(
        &value,
        &[
            "v",
            "kind",
            "runtime_id",
            "operation_id",
            "generation",
            "installer_generation",
            "recipient_generation",
            "recipient_nonce",
            "profile_digest",
            "grant_socket",
            "challenge",
        ],
    )?;
    if strict::safe_u64(m, "v")? != 1 || strict::string(m, "kind")? != "owner_launched_install" {
        return Err(Error::rejected(
            "installer capsule version/kind is not fixed",
        ));
    }
    let runtime_id = canonical_uuid(strict::string(m, "runtime_id")?)?;
    let operation_id = canonical_uuid(strict::string(m, "operation_id")?)?;
    let generation = strict::positive_u64(m, "generation")?;
    let installer_generation = strict::string(m, "installer_generation")?;
    if !strict::is_lower_hex(installer_generation, 32) {
        return Err(Error::rejected(
            "installer generation is not 32 lowercase hex",
        ));
    }
    if installer_generation != profile.installer().generation.as_str() {
        return Err(Error::rejected(
            "capsule installer generation is not pinned",
        ));
    }
    let recipient_generation = strict::string(m, "recipient_generation")?;
    if !strict::is_lower_hex(recipient_generation, 32) {
        return Err(Error::rejected(
            "recipient generation is not 32 lowercase hex",
        ));
    }
    let recipient_nonce = canonical_uuid(strict::string(m, "recipient_nonce")?)?;
    let profile_digest = strict::string(m, "profile_digest")?;
    strict::digest(profile_digest)?;
    if profile_digest != profile.digest() {
        return Err(Error::rejected(
            "capsule profile digest is not this fixed profile",
        ));
    }
    let grant_socket = strict::string(m, "grant_socket")?;
    if Path::new(grant_socket) != profile.supervisor_grant_socket().as_path() {
        return Err(Error::rejected("capsule grant socket is not the fixed one"));
    }

    let challenge_value = m
        .get("challenge")
        .ok_or_else(|| Error::rejected("capsule challenge missing"))?;
    let canonical_challenge = registry::canonical(challenge_value)?;
    let challenge = parse_challenge_bytes(&canonical_challenge)?;
    if canonical_challenge_bytes(&challenge)? != canonical_challenge
        || canonical_challenge_value(&challenge) != challenge_value.clone()
    {
        return Err(Error::rejected("capsule challenge is not canonical"));
    }

    if challenge.launch.request.challenge != operation_id
        || challenge.launch.request.identity.instance != runtime_id
        || challenge.launch.request.identity.generation != generation
        || challenge.launch.request.identity.company != profile.workspace_id()
        || challenge.launch.request.identity.backend != "native"
        || challenge.launch.request.purpose != "fresh_start"
        || challenge.launch.request.image != profile.image().reference
        || challenge.launch.epoch == 0
        || challenge.recipient.generation != recipient_generation
        || challenge.recipient.nonce != recipient_nonce
    {
        return Err(Error::rejected(
            "capsule challenge does not bind this runtime/operation/recipient",
        ));
    }

    let expected_pins = Pins {
        source: profile.image().source.clone(),
        image: profile.image().reference.clone(),
        helper: profile.image().helper.clone(),
        node: profile.image().node.clone(),
        pi_graph: profile.image().pi_graph.clone(),
        policy: profile.image().policy.clone(),
    };
    let expected_lineage = Lineage {
        reference: profile.lineage_reference().to_string(),
        database_epoch: profile.database_epoch(),
    };
    if challenge.pins != expected_pins || challenge.lineage != expected_lineage {
        return Err(Error::rejected(
            "capsule pins/lineage are not this fixed profile",
        ));
    }

    Ok(Capsule {
        operation_id,
        installer_generation: installer_generation.to_string(),
        recipient_generation: recipient_generation.to_string(),
        challenge,
    })
}

fn canonical_uuid(value: &str) -> Result<String> {
    if !strict::is_uuid(value) || value.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(Error::rejected("value is not a canonical lowercase UUID"));
    }
    Ok(value.to_string())
}

fn wire_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_FRAME_BYTES
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn wall_clock_seconds() -> Result<u64> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| unknown())?
        .as_secs();
    if seconds > MAX_SAFE_INT {
        return Err(unknown());
    }
    Ok(seconds)
}

fn read_stdin_to_eof(budget: Duration, max: usize) -> Result<Vec<u8>> {
    let deadline = Deadline::new(budget);
    let mut out = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        deadline.wait(libc::STDIN_FILENO, libc::POLLIN)?;
        let remaining = max.saturating_sub(out.len());
        if remaining == 0 {
            return Err(Error::rejected("installer capsule exceeds the bound"));
        }
        let want = chunk.len().min(remaining);
        let n = unsafe { libc::read(libc::STDIN_FILENO, chunk.as_mut_ptr().cast(), want) };
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
        out.extend_from_slice(&chunk[..n as usize]);
        chunk.fill(0);
        if out.len() >= max {
            return Err(Error::rejected("installer capsule exceeds the bound"));
        }
    }
    if out.is_empty() || out.len() > MAX_FRAME_BYTES {
        return Err(Error::rejected("installer capsule is outside the bound"));
    }
    Ok(out)
}

#[derive(Clone, Copy)]
struct Deadline(Instant);

impl Deadline {
    fn new(budget: Duration) -> Self {
        Self(Instant::now() + budget)
    }

    fn remaining(self) -> Result<Duration> {
        self.0
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(unknown)
    }

    fn wait(self, fd: RawFd, events: i16) -> Result<()> {
        loop {
            let left = self.remaining()?;
            let ms = left.as_millis().saturating_add(1).min(i32::MAX as u128) as i32;
            let mut pollfd = libc::pollfd {
                fd,
                events,
                revents: 0,
            };
            let rc = unsafe { libc::poll(&mut pollfd, 1, ms) };
            self.remaining()?;
            if rc > 0 && pollfd.revents & libc::POLLNVAL == 0 {
                return Ok(());
            }
            if rc < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(unknown());
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Node {
    dev: u64,
    ino: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    ctime: i64,
    ctime_nsec: i64,
}

fn node(file: &File) -> Result<Node> {
    let st = file.metadata().map_err(|_| unknown())?;
    Ok(Node {
        dev: st.dev(),
        ino: st.ino(),
        uid: st.uid(),
        gid: st.gid(),
        mode: st.mode(),
        ctime: st.ctime(),
        ctime_nsec: st.ctime_nsec(),
    })
}

fn open_at(parent: RawFd, name: &OsStr, directory: bool) -> Result<File> {
    let name = CString::new(name.as_bytes()).map_err(|_| unknown())?;
    let flags = libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | (if directory {
            libc::O_RDONLY | libc::O_DIRECTORY
        } else {
            libc::O_PATH
        });
    let fd = unsafe { libc::openat(parent, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(unknown());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_root() -> Result<File> {
    let path = CString::new("/").map_err(|_| unknown())?;
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(unknown());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

struct Topology {
    held: Vec<File>,
    snapshots: Vec<Node>,
}

impl Topology {
    fn capture(profile: &LocalProfile) -> Result<Self> {
        let socket = profile.supervisor_grant_socket();
        let relative = socket.strip_prefix("/").map_err(|_| unknown())?;
        let parts: Vec<&OsStr> = relative.iter().collect();
        if parts.len() != 3
            || parts[0] != OsStr::new("run")
            || parts[1] != OsStr::new("cadence-supervisor")
            || parts[2] != OsStr::new("grant.sock")
        {
            return Err(Error::rejected(
                "grant socket path is not the fixed topology",
            ));
        }
        let root = open_root()?;
        let run = open_at(root.as_raw_fd(), parts[0], true)?;
        let supervisor = open_at(run.as_raw_fd(), parts[1], true)?;
        let socket_file = open_at(supervisor.as_raw_fd(), parts[2], false)?;
        let held = vec![root, run, supervisor, socket_file];
        let snapshots = held.iter().map(node).collect::<Result<Vec<_>>>()?;

        let root_node = snapshots[0];
        if root_node.uid != 0 || root_node.gid != 0 || root_node.mode != (libc::S_IFDIR | 0o755) {
            return Err(Error::rejected(
                "root is not the fixed root-owned 0755 directory",
            ));
        }
        let run_node = snapshots[1];
        if run_node.uid != 0 || run_node.gid != 0 || run_node.mode != (libc::S_IFDIR | 0o755) {
            return Err(Error::rejected(
                "/run is not the fixed root-owned 0755 directory",
            ));
        }
        let supervisor_node = snapshots[2];
        let supervisor_mode = supervisor_node.mode & 0o777;
        if supervisor_node.uid != 0
            || supervisor_node.gid != profile.supervisor().gid
            || (supervisor_node.mode & libc::S_IFMT) != libc::S_IFDIR
            || supervisor_mode & 0o007 != 0
            || supervisor_mode & 0o070 == 0
            || supervisor_mode & 0o200 == 0
        {
            return Err(Error::rejected(
                "grant socket parent is not the root:supervisor 0770 directory",
            ));
        }
        let socket_node = snapshots[3];
        if socket_node.uid != profile.supervisor().uid
            || socket_node.gid != profile.supervisor().gid
            || (socket_node.mode & libc::S_IFMT) != libc::S_IFSOCK
            || (socket_node.mode & 0o777) != 0o600
        {
            return Err(Error::rejected(
                "grant socket is not the pinned supervisor 0600 socket",
            ));
        }
        Ok(Self { held, snapshots })
    }

    fn recheck(&self, profile: &LocalProfile) -> Result<()> {
        let current = Self::capture(profile)?;
        if current.snapshots != self.snapshots {
            return Err(Error::rejected("grant socket topology changed"));
        }
        for (file, before) in self.held.iter().zip(&self.snapshots) {
            if node(file)? != *before {
                return Err(Error::rejected("held grant socket topology changed"));
            }
        }
        Ok(())
    }

    fn connect(&self, deadline: Deadline) -> Result<UnixStream> {
        let parent = self
            .held
            .get(self.held.len().saturating_sub(2))
            .ok_or_else(unknown)?
            .as_raw_fd();
        connect_unix(
            Path::new(&format!("/proc/self/fd/{parent}/grant.sock")),
            deadline,
        )
    }
}

fn admit_supervisor(stream: &UnixStream, profile: &LocalProfile) -> Result<()> {
    let (uid, gid, _pid) = peer::credentials_full(stream)?;
    if uid != profile.supervisor().uid || gid != profile.supervisor().gid {
        return Err(Error::rejected(
            "grant socket peer is not the pinned supervisor uid/gid",
        ));
    }
    let bare = profile
        .supervisor()
        .exe_digest
        .strip_prefix("sha256:")
        .ok_or_else(|| Error::rejected("supervisor executable pin is not sha256"))?;
    if !strict::is_lower_hex(bare, 64) {
        return Err(Error::rejected("supervisor executable pin is not 64 hex"));
    }
    peer::admit_exe(stream, uid, bare)?;
    Ok(())
}

fn connect_unix(path: &Path, deadline: Deadline) -> Result<UnixStream> {
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

fn write_frame(stream: &UnixStream, frame: &[u8], deadline: Deadline) -> Result<()> {
    if frame.is_empty() || frame.len() > MAX_FRAME_BYTES || frame.last() != Some(&b'\n') {
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

fn read_response(stream: &UnixStream, deadline: Deadline) -> Result<Vec<u8>> {
    let mut response = Vec::with_capacity(256);
    let mut buf = [0u8; 1024];
    loop {
        deadline.wait(stream.as_raw_fd(), libc::POLLIN)?;
        let want = buf.len().min(
            MAX_RESPONSE_BYTES
                .saturating_add(1)
                .saturating_sub(response.len()),
        );
        if want == 0 {
            return Err(unknown());
        }
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
        if response.len() > MAX_RESPONSE_BYTES {
            return Err(unknown());
        }
    }
    if response.last() != Some(&b'\n')
        || response[..response.len().saturating_sub(1)].contains(&b'\n')
    {
        return Err(unknown());
    }
    Ok(response)
}

#[cfg(test)]
#[path = "installer_capsule_acceptance_20261027.rs"]
mod installer_capsule_acceptance_20261027;
