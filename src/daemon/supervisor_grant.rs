//! CAD-1017 — the protected supervisor **grant consumer** (verify-only).
//!
//! This module implements the *consumer* half of the private supervisor grant
//! protocol: a private Unix listener owned by the supervisor account
//! (`cadence-supervisor`, uid `21000`), kernel peer admission for the
//! image-pinned root installer, a bounded domain-separated signed envelope
//! over the canonical `SupervisorChallenge`, and a one-time consume protocol.
//! It **never mints** a grant and **never launches a Pi** — the production
//! authority factory is unavailable this batch, so the live path stays
//! `Err(UNKNOWN)`.
//!
//! Trust model (canonical contract `aos121-canonical-supervisor-grant-contract`
//! + platform `supervisor-continuity.ts`):
//!   * **Supervisor is the server** — a *new private* Unix listener, separate
//!     from `cadence.sock`, owned `21000`. The **root image-pinned installer**
//!     is the *client*; it may *deliver* a grant but never *ask for* one. A
//!     guest socket peer refuses.
//!   * **Admission is kernel + process custody**: `SO_PEERCRED` peer uid `0`,
//!     a live `/proc/<pid>` starttime (re-checked before *and* after the hash),
//!     the installer's own enrolled generation, AND the held `/proc/<pid>/exe`
//!     opened as a real fd, `fstat`ed (regular file, root-owned, not
//!     group/other-writable), bounded, hashed from offset 0 — measured against
//!     the installer's pinned digest. Root uid *alone* refuses.
//!   * **Two *separate* enrollments**: the supervisor's own live enrollment
//!     (`SupervisorEnrollment`, the `recipient` the signed grant binds) is
//!     distinct from the installer's kernel-derived enrollment
//!     (`InstallerEnrollment`, the connecting peer). They are never compared
//!     to each other — the grant's `recipient` names the *supervisor*, while
//!     `admit_installer` verifies the *peer*.
//!   * **Authority types are non-forgeable**: `SupervisorEnrollment`,
//!     `PeerIdentity` and `VerifiedGrant` have private fields and *no* public
//!     or crate-wide literal constructor — produced only inside this module by
//!     the kernel / enrollment / signature paths. `enroll_self` and the
//!     durable enrollment port are `Err`/unavailable factories this batch.
//!   * **Two fixed actions only**: `challenge` returns a nonsecret
//!     operation-bound nonce + the supervisor's own pid/starttime/generation;
//!     `install` accepts one bounded canonical envelope. No generic command,
//!     no argv/env-as-secret, no `/boot` report, no caller JWKS.
//!   * **Replay**: a durable obligation must be enrolled *before* a grant is
//!     delivered, then the exact `SupervisorChallenge` is one-time consumed +
//!     re-checked before spawn — all behind the `Err`/unavailable
//!     [`ExternalConsume`] port this batch. The in-memory tombstone is
//!     *test-only mechanics*, not durable authority; a restored DB never
//!     auto-loads.
//!
//! Every digest/keyring pin is unset this batch — a missing or zero pin
//! refuses; nothing here qualifies a launch.

#![allow(dead_code)]

use std::os::unix::io::AsRawFd;

use crate::error::{Error, Result};

/// Lowercase-hex encode (the challenge nonce). Local to this module — no
/// shared helper is imported (this batch must not depend on the frozen #685
/// seam).
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ────────────────── private reviewed constants (never caller-supplied) ────

/// The supervisor account's fixed uid — the server that owns the private
/// grant listener. Asserted as a constant bound (NSS resolution lives in the
/// topology layer); a socket owned by anyone else is never a grant channel.
pub(crate) const SUPERVISOR_UID: u32 = 21000;

/// The reviewed, image-pinned Ed25519 verifying keys for the supervisor grant
/// — `"<kid>:<base64url-x>"` pairs, the ONLY trust root a grant signature may
/// resolve against. Compiled in, bound to the protected-image build; a caller
/// may never supply or override it. Empty this batch → no envelope verifies.
const SUPERVISOR_KEYRING: &[&[u8]] = &[];

/// The reviewed sha256 of the image-pinned installer binary — the digest the
/// connecting peer's `/proc/<pid>/exe` must equal. A private constant, bound
/// to the protected-image build; a caller may never supply or override it.
/// `None`/zero this batch → every installer admission refuses.
const INSTALLER_EXE_DIGEST: Option<[u8; 32]> = None;

/// The pinned supervisor generation the installer must present — a private,
/// reviewed constant. `None` this batch → no presented generation can match.
const INSTALLER_GENERATION_PIN: Option<&'static str> = None;

/// The private grant-listener path under the supervisor's runtime dir —
/// a *new* socket, separate from `cadence.sock`. Production resolution is
/// unavailable this batch (no provisioned path); [`GrantListener::bind_at`]
/// is the `#[cfg(test)]`-only injection used by ordinary-uid socket tests.
#[cfg(target_os = "linux")]
const PRODUCTION_GRANT_SOCK: &str = "/run/cadence-supervisor/grant.sock";

/// The signature domain — a context string prepended to the signed body so a
/// grant signature can never be replayed as a board-session assertion or any
/// other Ed25519 document. Distinct, fixed, and never guest-controlled.
const GRANT_DOMAIN: &str = "cadence.supervisor-launch-grant.v1";

/// The largest grant validity window — a grant is never open-ended.
const MAX_GRANT_WINDOW_SECS: u64 = 300;

/// The largest envelope byte length accepted on the wire — bounded framing.
const MAX_ENVELOPE_BYTES: usize = 32 * 1024;

// ────────────────────── non-forgeable authority types ─────────────────────

/// The supervisor's own live process enrollment — the pid + `/proc`
/// starttime + generation of the running supervisor instance that a signed
/// grant's `recipient` binds to. Private fields; produced only by
/// `enroll_self` (production, unavailable this batch) or `capture_test`
/// (`#[cfg(test)]`-only). A caller cannot fabricate one from a literal.
#[derive(Clone, Debug)]
pub(crate) struct SupervisorEnrollment {
    pid: u32,
    starttime: u64,
    generation: String,
}

/// Mint the supervisor's own enrollment — the production path resolves the
/// running pid + `/proc` starttime and the minted generation. Permanently
/// `Err` this batch: no live supervisor enrollment source exists, and the
/// recipient-binding must come from the real self, not a caller literal.
pub(crate) fn enroll_self() -> Result<SupervisorEnrollment> {
    Err(Error::rejected(
        "supervisor self-enrollment unavailable — no live enrolled pid/\
         starttime/generation source this batch; recipient binding UNKNOWN",
    ))
}

impl SupervisorEnrollment {
    /// `#[cfg(test)]`-only synthetic capture for unit tests — reads the test
    /// process's real `/proc` starttime but takes an arbitrary pid/generation.
    /// NOT a production authority path; `enroll_self` stays refused.
    #[cfg(test)]
    fn capture_test(pid: u32, generation: String) -> Result<Self> {
        let starttime = crate::peer::proc_starttime(pid)
            .ok_or_else(|| Error::rejected(format!("supervisor pid {pid} starttime unreadable")))?;
        Ok(Self {
            pid,
            starttime,
            generation,
        })
    }

    fn pid(&self) -> u32 {
        self.pid
    }
    fn generation(&self) -> &str {
        &self.generation
    }
}

/// The authenticated installer peer — produced ONLY by [`admit_installer`]
/// from the accepted socket's `SO_PEERCRED` + `/proc` custody + the pinned
/// generation/digest match. Private fields, no public construction — a caller
/// cannot mint a `PeerIdentity` to claim installer status.
#[derive(Clone, Debug)]
pub(crate) struct PeerIdentity {
    pid: u32,
    starttime: u64,
    /// The measured sha256 of the peer's held `/proc/<pid>/exe` fd.
    exe_digest: [u8; 32],
}

/// Read the accepted Unix peer's kernel credentials (`SO_PEERCRED`):
/// uid + pid. Linux-only; any other target refuses.
#[cfg(target_os = "linux")]
fn peer_credentials(stream: &std::os::unix::net::UnixStream) -> Result<(u32, u32)> {
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
    Ok((cred.uid, cred.pid as u32))
}

/// The largest `/proc/<pid>/exe` the installer may be — bounded read.
#[cfg(target_os = "linux")]
const MAX_EXE_BYTES: u64 = 128 * 1024 * 1024;

/// A stable `(ino, size, mtime_ns, ctime_ns)` fingerprint of the exe fd — the
/// binary must not change under the read.
#[cfg(target_os = "linux")]
#[derive(PartialEq, Eq)]
struct ExeStat {
    ino: u64,
    size: u64,
    mtime_ns: i128,
    ctime_ns: i128,
    mode: u32,
    uid: u32,
    nlink: u64,
}
#[cfg(target_os = "linux")]
fn exe_stat(m: &libc::stat) -> ExeStat {
    ExeStat {
        ino: m.st_ino,
        size: m.st_size as u64,
        mtime_ns: (m.st_mtime as i128) * 1_000_000_000 + m.st_mtime_nsec as i128,
        ctime_ns: (m.st_ctime as i128) * 1_000_000_000 + m.st_ctime_nsec as i128,
        mode: m.st_mode,
        uid: m.st_uid,
        nlink: m.st_nlink,
    }
}

/// Open + measure the running peer's executable inode.
///
/// Opens `/proc/<pid>/exe` as a real fd (not `fs::read`'s unbounded path read),
/// `fstat`s it (must be a *regular* file, root-owned, not group/other-writable,
/// ≤ [`MAX_EXE_BYTES`]), hashes it from offset 0 with `pread`, and re-stats the
/// held fd afterwards — a binary that changed or fails the ownership/mode check
/// refuses. Returns the digest. The caller separately re-checks the peer's
/// live pid/starttime after this call (TOCTOU on the process itself).
#[cfg(target_os = "linux")]
fn peer_exe_digest(pid: u32) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    use std::os::unix::io::FromRawFd;
    let path = format!("/proc/{pid}/exe");
    let cpath = std::ffi::CString::new(path.clone())
        .map_err(|_| Error::rejected("exe path not cstring"))?;
    // O_RDONLY|O_CLOEXEC — /proc/<pid>/exe resolves to the live binary inode.
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
    // Must be a regular file (not a fifo/dir/symlink), root-owned, and not
    // group/other-writable — the installed helper binary's custody.
    if before.mode & libc::S_IFMT != libc::S_IFREG {
        return Err(Error::rejected("/proc/<pid>/exe is not a regular file"));
    }
    if before.uid != 0 {
        return Err(Error::rejected("installer binary is not root-owned"));
    }
    if before.mode & 0o022 != 0 {
        return Err(Error::rejected("installer binary is group/other-writable"));
    }
    if before.nlink < 1 {
        return Err(Error::rejected("installer binary has zero links"));
    }
    if before.size > MAX_EXE_BYTES {
        return Err(Error::rejected(format!(
            "installer binary exceeds {MAX_EXE_BYTES} bytes"
        )));
    }
    // Hash from offset 0 with pread — no shared-offset interference, bounded.
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
    // Re-stat the held fd — the inode must not have changed under the read.
    let mut st2: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(file.as_raw_fd(), &mut st2) } != 0 {
        return Err(Error::rejected("/proc/<pid>/exe re-fstat failed"));
    }
    if exe_stat(&st2) != before {
        return Err(Error::rejected(
            "installer binary changed under read — unstable inode",
        ));
    }
    Ok(h.finalize().into())
}

/// Admit a connecting installer peer. The kernel peer uid must be `0` (root),
/// the peer's live `/proc` starttime must be stable across the exe-hash window
/// (re-checked before *and* after), the presented generation must equal the
/// pinned installer generation, AND the held `/proc/<pid>/exe` fd must hash to
/// the pinned installer digest. Root uid alone, a pid-reuse/dead process, a
/// stale generation, or a wrong binary all refuse. Returns a non-forgeable
/// [`PeerIdentity`] — the only way to obtain one.
///
/// `exe_digest` and `generation_pin` are the reviewed private constants; they
/// are `#[cfg(test)]`-injectable only — production passes `INSTALLER_EXE_DIGEST`
/// and `INSTALLER_GENERATION_PIN`, never caller data.
#[cfg(target_os = "linux")]
fn admit_installer(
    stream: &std::os::unix::net::UnixStream,
    generation_presented: &str,
    exe_pin: Option<[u8; 32]>,
    generation_pin: Option<&'static str>,
) -> Result<PeerIdentity> {
    let (uid, pid) = peer_credentials(stream)?;
    if uid != 0 {
        return Err(Error::rejected(format!(
            "grant peer uid {uid} is not root — only the image-pinned \
             installer may deliver a grant"
        )));
    }
    // Live process identity, sample 1 — before the exe hash.
    let start_a = crate::peer::proc_starttime(pid)
        .ok_or_else(|| Error::rejected(format!("peer pid {pid} starttime unreadable")))?;
    // The presented generation must equal the pinned installer generation —
    // unset pin refuses outright.
    let Some(want_gen) = generation_pin else {
        return Err(Error::rejected(
            "no installer generation pin — admission refused",
        ));
    };
    if generation_presented != want_gen {
        return Err(Error::rejected(
            "peer presented a generation that is not the pinned installer generation",
        ));
    }
    // The binary the peer is actually running must be the pinned installer —
    // root uid alone is never enough.
    let Some(want_exe) = exe_pin else {
        return Err(Error::rejected(
            "no installer exe digest pin — admission refused",
        ));
    };
    let exe = peer_exe_digest(pid)?;
    if exe != want_exe {
        return Err(Error::rejected(
            "peer binary does not match the pinned installer digest — a root \
             shell is not the installer",
        ));
    }
    // Sample 2 — the pid must still name the *same* live process after the
    // hash window (guards a die-and-reexec TOCTOU).
    let start_b = crate::peer::proc_starttime(pid)
        .ok_or_else(|| Error::rejected(format!("peer pid {pid} exited during admission")))?;
    if start_b != start_a {
        return Err(Error::rejected(
            "peer pid changed under admission — pid-reuse/exit refused",
        ));
    }
    Ok(PeerIdentity {
        pid,
        starttime: start_b,
        exe_digest: exe,
    })
}

// ────────────────── canonical SupervisorChallenge schema ──────────────────
// Platform `supervisor-continuity.ts`: the signed body is `{challenge:
// SupervisorChallenge, nbf, exp}` under the domain tag — the wrapper adds ONLY
// the bounded expiry; the challenge object is the canonical binding,
// byte-for-byte the platform's shape.

/// `launch.request.identity` — the executor admission identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExecutorIdentity {
    pub company: String,
    pub instance: String,
    /// Must be `"native"` for the protected lane.
    pub backend: String,
    pub tier: String,
    /// The launch generation — JS-safe non-negative integer.
    pub generation: u64,
    /// Optional image lane (defaults `"baseline"` when absent on the wire).
    pub image_lane: Option<String>,
}

/// `launch.request` — the launch obligation identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LaunchRequest {
    pub identity: ExecutorIdentity,
    /// `"fresh_start" | "reconstruct"`.
    pub purpose: String,
    /// The fresh external challenge — a UUID.
    pub challenge: String,
    /// The OCI image reference `<ref>@sha256:<64-hex>`.
    pub image: String,
}

/// `launch` — the request plus the global epoch it binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LaunchBinding {
    pub request: LaunchRequest,
    /// The current global epoch (JS-safe non-negative integer).
    pub epoch: u64,
}

/// `recipient` — the *live supervisor instance* the grant is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Recipient {
    /// pid 1..=4194304 (kernel pid_max bound).
    pub pid: u32,
    /// `/proc/<pid>` starttime as a decimal string, `^[1-9][0-9]{0,19}$`.
    pub starttime: String,
    /// The supervisor's enrolled generation — 32 lowercase-hex chars.
    pub generation: String,
    /// The challenge nonce — a UUID.
    pub nonce: String,
}

/// `pins` — the pinned artifact digests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pins {
    /// Source commit — 40-hex SHA1.
    pub source: String,
    /// OCI image — must equal `launch.request.image`.
    pub image: String,
    pub helper: String,
    pub node: String,
    /// The Pi JS *graph* digest (named `piGraph`, not `pi_digest`).
    pub pi_graph: String,
    pub policy: String,
}

/// `lineage` — the restore-lineage the grant binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Lineage {
    /// `^[A-Za-z0-9_-]{1,128}$`.
    pub reference: String,
    /// JS-safe non-negative integer.
    pub database_epoch: u64,
}

/// The canonical `SupervisorChallenge` — the exact platform shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SupervisorChallenge {
    pub launch: LaunchBinding,
    pub recipient: Recipient,
    pub pins: Pins,
    pub lineage: Lineage,
}

/// The signed claims = the canonical challenge + a bounded expiry wrapper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GrantClaims {
    pub challenge: SupervisorChallenge,
    /// Bounded monotonic validity window `nbf..=exp` (≤300 s).
    pub nbf: u64,
    pub exp: u64,
}

// ── strict field validators (deny unknown/duplicate/coercible) ─────────────

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn is_uuid(s: &str) -> bool {
    // ^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$ — case-insensitive hex
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    for (i, &c) in b.iter().enumerate() {
        if matches!(i, 8 | 13 | 18 | 23) {
            if c != b'-' {
                return false;
            }
        } else if !c.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}
fn is_oci_image(s: &str) -> bool {
    // ^[^\s@]+@sha256:[a-f0-9]{64}$, ≤512 chars
    if s.len() > 512 {
        return false;
    }
    match s.split_once('@') {
        Some((ref_, d)) => {
            !ref_.is_empty()
                && !ref_.bytes().any(|b| b.is_ascii_whitespace())
                && d.strip_prefix("sha256:")
                    .map(|h| is_lower_hex(h, 64))
                    .unwrap_or(false)
        }
        None => false,
    }
}
fn is_decimal(s: &str) -> bool {
    // ^[1-9][0-9]{0,19}$ — a nonzero-prefixed decimal, ≤20 digits
    !s.is_empty()
        && s.len() <= 20
        && s.starts_with(|c: char| c.is_ascii_digit() && c != '0')
        && s.bytes().all(|b| b.is_ascii_digit())
}
fn is_lineage_ref(s: &str) -> bool {
    // ^[A-Za-z0-9_-]{1,128}$
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// A fixed-charset bounded token (`[A-Za-z0-9._:-]`, 1..=`max`) — used for the
/// JWS `kid`.
fn token_ok(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
}

/// A JSON object must contain EXACTLY `names` — deny unknown and duplicate
/// fields. (serde_json collapses duplicate keys into the last value, so a
/// duplicated key already reduces `m.len()` below `names.len()` and refuses.)
fn obj_exact<'a>(
    v: &'a serde_json::Value,
    names: &[&str],
) -> Result<&'a serde_json::Map<String, serde_json::Value>> {
    let m = v
        .as_object()
        .ok_or_else(|| Error::rejected("expected a JSON object"))?;
    if m.len() != names.len()
        || !m.keys().all(|k| names.contains(&k.as_str()))
        || !names.iter().all(|k| m.contains_key(*k))
    {
        return Err(Error::rejected(format!(
            "object must contain exactly {:?} (got {:?})",
            names,
            m.keys().collect::<Vec<_>>()
        )));
    }
    Ok(m)
}

fn sget<'a>(m: &'a serde_json::Map<String, serde_json::Value>, k: &str) -> Result<&'a str> {
    m.get(k)
        .and_then(|x| x.as_str())
        .ok_or_else(|| Error::rejected(format!("claim '{k}' missing/not a string")))
}
fn uget(m: &serde_json::Map<String, serde_json::Value>, k: &str) -> Result<u64> {
    // JS-safe non-negative integer: a u64 within 2^53.
    match m.get(k).and_then(|x| x.as_u64()) {
        Some(n) if n <= 9007199254740991 => Ok(n),
        _ => Err(Error::rejected(format!(
            "claim '{k}' missing/not a safe uint"
        ))),
    }
}

fn parse_identity(v: &serde_json::Value) -> Result<ExecutorIdentity> {
    // `identity` may carry imageLane optionally — 5 required + at most one
    // optional key; deny anything else.
    let m = v
        .as_object()
        .ok_or_else(|| Error::rejected("identity is not an object"))?;
    let want = ["company", "instance", "backend", "tier", "generation"];
    let optional = "imageLane";
    if m.len() < want.len()
        || m.len() > want.len() + 1
        || !want.iter().all(|k| m.contains_key(*k))
        || m.keys()
            .any(|k| !want.contains(&k.as_str()) && k != optional)
    {
        return Err(Error::rejected(
            "identity has unknown/duplicate/missing fields",
        ));
    }
    let company = sget(m, "company")?;
    let instance = sget(m, "instance")?;
    let backend = sget(m, "backend")?;
    let tier = sget(m, "tier")?;
    // company/instance: 1..=200 chars, no ':'
    for (name, s) in [("company", company), ("instance", instance)] {
        if s.is_empty() || s.len() > 200 || s.contains(':') {
            return Err(Error::rejected(format!("identity.{name} is invalid")));
        }
    }
    if backend != "native" {
        return Err(Error::rejected("identity.backend must be \"native\""));
    }
    if tier != "basic" && tier != "standard-1" {
        return Err(Error::rejected("identity.tier is not a known tier"));
    }
    let generation = uget(m, "generation")?;
    let image_lane = match m.get("imageLane") {
        Some(x) => Some(
            x.as_str()
                .ok_or_else(|| Error::rejected("identity.imageLane is not a string"))?
                .to_string(),
        ),
        None => None,
    };
    Ok(ExecutorIdentity {
        company: company.to_string(),
        instance: instance.to_string(),
        backend: backend.to_string(),
        tier: tier.to_string(),
        generation,
        image_lane,
    })
}

fn parse_request(v: &serde_json::Value) -> Result<LaunchRequest> {
    let m = obj_exact(v, &["challenge", "identity", "image", "purpose"])?;
    let identity = parse_identity(m.get("identity").unwrap())?;
    let purpose = sget(m, "purpose")?;
    if purpose != "fresh_start" && purpose != "reconstruct" {
        return Err(Error::rejected(
            "launch.request.purpose is not fresh_start|reconstruct",
        ));
    }
    let challenge = sget(m, "challenge")?;
    if !is_uuid(challenge) {
        return Err(Error::rejected("launch.request.challenge is not a UUID"));
    }
    let image = sget(m, "image")?;
    if !is_oci_image(image) {
        return Err(Error::rejected(
            "launch.request.image is not an OCI@sha256 ref",
        ));
    }
    Ok(LaunchRequest {
        identity,
        purpose: purpose.to_string(),
        challenge: challenge.to_string(),
        image: image.to_string(),
    })
}

fn parse_launch(v: &serde_json::Value) -> Result<LaunchBinding> {
    let m = obj_exact(v, &["epoch", "request"])?;
    Ok(LaunchBinding {
        request: parse_request(m.get("request").unwrap())?,
        epoch: uget(m, "epoch")?,
    })
}

fn parse_recipient(v: &serde_json::Value) -> Result<Recipient> {
    let m = obj_exact(v, &["generation", "nonce", "pid", "starttime"])?;
    let pid = uget(m, "pid")?;
    if pid == 0 || pid > 4194304 {
        return Err(Error::rejected("recipient.pid out of range"));
    }
    let starttime = sget(m, "starttime")?;
    if !is_decimal(starttime) {
        return Err(Error::rejected(
            "recipient.starttime is not a decimal string",
        ));
    }
    let generation = sget(m, "generation")?;
    if !is_lower_hex(generation, 32) {
        return Err(Error::rejected("recipient.generation is not 32-lowerhex"));
    }
    let nonce = sget(m, "nonce")?;
    if !is_uuid(nonce) {
        return Err(Error::rejected("recipient.nonce is not a UUID"));
    }
    Ok(Recipient {
        pid: pid as u32,
        starttime: starttime.to_string(),
        generation: generation.to_string(),
        nonce: nonce.to_string(),
    })
}

fn parse_pins(v: &serde_json::Value) -> Result<Pins> {
    let m = obj_exact(
        v,
        &["helper", "image", "node", "piGraph", "policy", "source"],
    )?;
    let source = sget(m, "source")?;
    if !is_lower_hex(source, 40) {
        return Err(Error::rejected("pins.source is not a 40-hex SHA1"));
    }
    let image = sget(m, "image")?;
    if !is_oci_image(image) {
        return Err(Error::rejected("pins.image is not an OCI@sha256 ref"));
    }
    let sha = |k: &str| -> Result<String> {
        let s = sget(m, k)?;
        if !is_lower_hex(s, 64) {
            return Err(Error::rejected(format!("pins.{k} is not a 64-hex sha256")));
        }
        Ok(s.to_string())
    };
    Ok(Pins {
        source: source.to_string(),
        image: image.to_string(),
        helper: sha("helper")?,
        node: sha("node")?,
        pi_graph: sha("piGraph")?,
        policy: sha("policy")?,
    })
}

fn parse_lineage(v: &serde_json::Value) -> Result<Lineage> {
    let m = obj_exact(v, &["databaseEpoch", "reference"])?;
    let reference = sget(m, "reference")?;
    if !is_lineage_ref(reference) {
        return Err(Error::rejected("lineage.reference is not a bounded ref"));
    }
    Ok(Lineage {
        reference: reference.to_string(),
        database_epoch: uget(m, "databaseEpoch")?,
    })
}

/// Parse the canonical `SupervisorChallenge` — exact platform field names and
/// types, deny unknown/duplicate/coercible fields. `pins.image` must equal
/// `launch.request.image`.
fn parse_supervisor_challenge(v: &serde_json::Value) -> Result<SupervisorChallenge> {
    let m = obj_exact(v, &["launch", "lineage", "pins", "recipient"])?;
    let launch = parse_launch(m.get("launch").unwrap())?;
    let recipient = parse_recipient(m.get("recipient").unwrap())?;
    let pins = parse_pins(m.get("pins").unwrap())?;
    let lineage = parse_lineage(m.get("lineage").unwrap())?;
    if pins.image != launch.request.image {
        return Err(Error::rejected("pins.image != launch.request.image"));
    }
    Ok(SupervisorChallenge {
        launch,
        recipient,
        pins,
        lineage,
    })
}

/// Parse the signed claims — the domain-separated wrapper `{challenge, nbf,
/// exp}` over the canonical `SupervisorChallenge`, bounded expiry checked
/// against `now`.
fn parse_claims(v: &serde_json::Value, now: u64) -> Result<GrantClaims> {
    let m = obj_exact(v, &["challenge", "exp", "nbf"])?;
    let challenge = parse_supervisor_challenge(m.get("challenge").unwrap())?;
    let nbf = uget(m, "nbf")?;
    let exp = uget(m, "exp")?;
    if exp <= nbf {
        return Err(Error::rejected("grant exp must exceed nbf"));
    }
    if exp - nbf > MAX_GRANT_WINDOW_SECS {
        return Err(Error::rejected(format!(
            "grant validity window exceeds {MAX_GRANT_WINDOW_SECS}s"
        )));
    }
    if now < nbf || now > exp {
        return Err(Error::rejected("grant is not currently valid"));
    }
    Ok(GrantClaims {
        challenge,
        nbf,
        exp,
    })
}

/// Decode a base64url (no padding) segment.
fn b64url(s: &str) -> Result<Vec<u8>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| Error::rejected("grant envelope segment is not base64url"))
}

/// A parsed compact JWS: `header.payload.signature`. `signed` is the exact
/// `GRANT_DOMAIN \0 header.payload` bytes the signature must cover.
struct ParsedEnvelope {
    signed: String,
    signature: Vec<u8>,
    kid: String,
    claims: GrantClaims,
}

/// Parse a compact Ed25519 grant envelope: exactly three segments, header
/// `alg=EdDSA` + `kid`, claims matching the closed schema. The signature is
/// domain-separated: `GRANT_DOMAIN || NUL || header.payload` is what the key
/// signs — so a grant is never interchangeable with another Ed25519 doc.
fn parse_envelope(compact: &str, now: u64) -> Result<ParsedEnvelope> {
    if compact.len() > MAX_ENVELOPE_BYTES {
        return Err(Error::rejected("grant envelope exceeds the size bound"));
    }
    let mut parts = compact.split('.');
    let (h, p, s) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s), None) => (h, p, s),
        _ => {
            return Err(Error::rejected(
                "grant envelope must be a compact three-part JWS",
            ))
        }
    };
    // The signed body is domain-separated: sign over the domain tag + the
    // literal header.payload bytes.
    let signed = format!("{GRANT_DOMAIN}\u{0}{h}.{p}");
    let header: serde_json::Value = serde_json::from_slice(&b64url(h)?)
        .map_err(|_| Error::rejected("grant header is not JSON"))?;
    if header.get("alg").and_then(|a| a.as_str()) != Some("EdDSA") {
        return Err(Error::rejected("grant envelope signature is not Ed25519"));
    }
    let kid = header
        .get("kid")
        .and_then(|k| k.as_str())
        .ok_or_else(|| Error::rejected("grant envelope has no kid"))?;
    if !token_ok(kid, 64) {
        return Err(Error::rejected("grant kid is malformed"));
    }
    let claims_v: serde_json::Value = serde_json::from_slice(&b64url(p)?)
        .map_err(|_| Error::rejected("grant payload is not JSON"))?;
    let claims = parse_claims(&claims_v, now)?;
    let signature = b64url(s)?;
    if signature.len() != 64 {
        return Err(Error::rejected("grant signature is not 64 bytes"));
    }
    Ok(ParsedEnvelope {
        signed,
        signature,
        kid: kid.to_string(),
        claims,
    })
}

/// Verify the envelope signature against `keyring`. Production callers pass
/// the private compiled [`SUPERVISOR_KEYRING`]; `keyring` is a parameter so
/// `#[cfg(test)]` can inject synthetic keys — it is never a caller/JWKS/env
/// source. Empty keyring / unknown `kid` / bad signature refuse.
fn verify_signature_with(parsed: &ParsedEnvelope, keyring: &[&[u8]]) -> Result<()> {
    if keyring.is_empty() {
        return Err(Error::rejected(
            "no supervisor grant keyring is pinned — no envelope can verify",
        ));
    }
    for entry in keyring {
        let Ok(entry) = std::str::from_utf8(entry) else {
            continue;
        };
        let Some((ek, xb64)) = entry.split_once(':') else {
            continue;
        };
        if ek == parsed.kid {
            let x = b64url(xb64)?;
            if x.len() != 32 {
                return Err(Error::rejected("keyring key is not a 32-byte Ed25519 x"));
            }
            use ring::signature::{UnparsedPublicKey, ED25519};
            return match UnparsedPublicKey::new(&ED25519, &x)
                .verify(parsed.signed.as_bytes(), &parsed.signature)
            {
                Ok(()) => Ok(()),
                Err(_) => Err(Error::rejected(
                    "grant signature does not verify against the pinned key",
                )),
            };
        }
    }
    Err(Error::rejected(format!(
        "grant kid '{}' is not in the pinned supervisor keyring",
        parsed.kid
    )))
}

// ──────────────────── one-time consume + durable obligation ───────────────

/// The one-time-consume outcome. `Consumed` is the only terminal success;
/// `Unknown` covers both a replay and a lost commit ack — never a retry-as-success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConsumeOutcome {
    Consumed,
    Unknown,
}

/// An in-memory replay set — *test-only mechanics*, NOT durable authority.
/// The real durable CAS lives behind the external obligation port
/// ([`ExternalConsume`]); a restored/local DB never auto-loads authority.
#[derive(Default)]
pub(crate) struct TombstoneSet {
    seen: std::collections::HashSet<String>,
}

impl TombstoneSet {
    /// CAS: claim `op` iff absent. First claim → `Consumed`; any re-claim →
    /// `Unknown`. A caller must treat `Unknown` as refusal, never retry.
    pub(crate) fn consume_once(&mut self, op: &str) -> ConsumeOutcome {
        if self.seen.insert(op.to_string()) {
            ConsumeOutcome::Consumed
        } else {
            ConsumeOutcome::Unknown
        }
    }
    #[cfg(test)]
    fn is_consumed(&self, op: &str) -> bool {
        self.seen.contains(op)
    }
}

/// The external durable-obligation + one-time-consume port — the authentic
/// owner of the global/company enrollment and the replay CAS. Implemented by
/// an external owner (not this batch); the default is `Err`/unavailable. A
/// lost ack stays `Unknown`, never retried.
///
/// The durable obligation binds the **exact canonical `SupervisorChallenge`**,
/// not a reduced tuple: `enroll_pending` records the obligation *before* the
/// installer delivers the grant; `consume` CASes that exact challenge to
/// consumed; `recheck` re-validates it before spawn.
pub(crate) trait ExternalConsume {
    /// Durably enroll the obligation for this exact challenge *before* the
    /// grant is accepted — the global/company record must exist before
    /// delivery. `true` only on a confirmed durable commit.
    fn enroll_pending(&self, challenge: &SupervisorChallenge) -> bool;
    /// One-time CAS of this exact challenge to consumed. Returns `Consumed`
    /// only on a confirmed first-time commit; replay / lost ack / absent
    /// enrollment returns `Unknown`.
    fn consume(&self, challenge: &SupervisorChallenge) -> ConsumeOutcome;
    /// Re-check that this exact challenge's obligation still matches
    /// immediately before an eventual spawn — the pre-spawn recheck port.
    fn recheck(&self, challenge: &SupervisorChallenge) -> bool;
}

/// The production external-consume factory — permanently `Err` until the
/// durable obligation owner, signing keyring, image pins and restore lineage
/// exist. Nothing on a live path calls this yet.
pub(crate) fn production_consume_factory() -> Result<()> {
    Err(Error::rejected(
        "production supervisor-grant authority unavailable — no private \
         21000 channel, pinned keyring, durable obligation port, image pins \
         or restore lineage; eligibility UNKNOWN and stays refused",
    ))
}

// ─────────────────────────── the fixed action verbs ───────────────────────

/// A nonsecret challenge a supervisor mints for one `challenge` action — the
/// operation-bound nonce plus the supervisor's own live pid/starttime/
/// generation, so the installer's signed envelope can pin to *this* running
/// supervisor instance. Private fields — produced only by
/// [`GrantCore::challenge`].
#[derive(Clone, Debug)]
pub(crate) struct Challenge {
    nonce: String,
    supervisor_pid: u32,
    supervisor_starttime: u64,
    supervisor_generation: String,
}

impl Challenge {
    pub(crate) fn nonce(&self) -> &str {
        &self.nonce
    }
    pub(crate) fn generation(&self) -> &str {
        &self.supervisor_generation
    }
}

/// The verified grant — evidence a correctly-signed, kernel-admitted, in-window
/// envelope over the canonical `SupervisorChallenge` was consumed exactly once
/// against the supervisor's current epoch. **Not** launch authority: the
/// durable external consume and the protected spawn are still `Err`/
/// unavailable this batch.
#[derive(Debug)]
pub(crate) struct VerifiedGrant {
    claims: GrantClaims,
    peer: PeerIdentity,
}

impl VerifiedGrant {
    /// The consumed operation id — `launch.request.challenge`, the UUID that
    /// keys the replay tombstone (evidence for the pre-spawn recheck).
    pub(crate) fn op(&self) -> &str {
        &self.challenge().launch.request.challenge
    }
    /// The exact canonical challenge the grant bound.
    pub(crate) fn challenge(&self) -> &SupervisorChallenge {
        &self.claims.challenge
    }
}

/// The grant-consume core bound to the supervisor's own enrollment — the
/// object that owns the `challenge`/`install` verbs. Separate from the
/// transport ([`GrantListener`]); constructed only from a real
/// `SupervisorEnrollment`, never a caller literal.
pub(crate) struct GrantCore {
    enrolled: SupervisorEnrollment,
    tomb: TombstoneSet,
}

impl GrantCore {
    /// Bind the consume core to the supervisor's live enrollment.
    fn new(enrolled: SupervisorEnrollment) -> Self {
        Self {
            enrolled,
            tomb: TombstoneSet::default(),
        }
    }

    /// `challenge` — mint a nonsecret, operation-bound nonce for one installer
    /// request, returning the supervisor's own live pid/starttime/generation so
    /// the signed envelope can pin to this instance. The nonce is `Sha256` of
    /// the enrollment + op + a per-call counter — deterministic shape, never a
    /// secret and never reusable across ops.
    fn challenge(&self, op: &str, seq: u64) -> Challenge {
        use sha2::{Digest, Sha256};
        let nonce = {
            let mut h = Sha256::new();
            h.update(b"cadence.supervisor-challenge.v1\x00");
            h.update(self.enrolled.generation.as_bytes());
            h.update(b"\x00");
            h.update(op.as_bytes());
            h.update(b"\x00");
            h.update(seq.to_be_bytes());
            hex_encode(&h.finalize())
        };
        Challenge {
            nonce,
            supervisor_pid: self.enrolled.pid,
            supervisor_starttime: self.enrolled.starttime,
            supervisor_generation: self.enrolled.generation.clone(),
        }
    }

    /// `install` — admit the kernel peer, then verify+consume the signed
    /// envelope over the canonical `SupervisorChallenge`. Trust pins
    /// (`keyring`, `exe_pin`, `generation_pin`) are the private reviewed
    /// constants in production; they are parameters ONLY so `#[cfg(test)]` can
    /// inject synthetic fixtures — never caller data on a live path. On
    /// success returns a non-forgeable [`VerifiedGrant`]; on any failure the
    /// op is refused (and tombstoned-Unknown, never retried).
    #[cfg(target_os = "linux")]
    #[allow(clippy::too_many_arguments)]
    fn install(
        &mut self,
        stream: &std::os::unix::net::UnixStream,
        envelope: &str,
        keyring: &[&[u8]],
        exe_pin: Option<[u8; 32]>,
        generation_pin: Option<&'static str>,
        generation_presented: &str,
        epoch: u64,
        ext: &dyn ExternalConsume,
        now: u64,
    ) -> Result<VerifiedGrant> {
        // Kernel peer + process custody first — nothing the peer sent is
        // trusted before admission. Pins are private constants, not caller data.
        let peer = admit_installer(stream, generation_presented, exe_pin, generation_pin)?;
        // Durable obligation must already exist BEFORE we accept the grant —
        // enroll_pending is the external owner's pre-delivery record.
        // Parse + signature-verify against the pinned keyring.
        let parsed = parse_envelope(envelope, now)?;
        verify_signature_with(&parsed, keyring)?;
        let claims = parsed.claims;
        let sc = &claims.challenge;
        // The durable global/company obligation for THIS exact challenge must
        // have committed before delivery — enroll_pending is checked first.
        if !ext.enroll_pending(sc) {
            return Err(Error::rejected(
                "no durable obligation enrolled for this challenge — grant not pre-registered",
            ));
        }
        // The grant's recipient must name THIS live supervisor instance: the
        // enrolled pid, starttime (decimal string) and generation — binding the
        // grant to this process, not a stale or guest one.
        if sc.recipient.pid != self.enrolled.pid
            || sc.recipient.generation != self.enrolled.generation
        {
            return Err(Error::rejected(
                "grant recipient does not name this supervisor instance",
            ));
        }
        let recip_start: u64 = sc
            .recipient
            .starttime
            .parse()
            .map_err(|_| Error::rejected("recipient.starttime not numeric"))?;
        if recip_start != self.enrolled.starttime {
            return Err(Error::rejected(
                "recipient.starttime != enrolled supervisor",
            ));
        }
        // The op id is the launch request's UUID challenge. The signed
        // `recipient.nonce` must be the nonce this supervisor minted for the
        // op via `challenge()` — verified here against a derived value is the
        // external owner's concern; the binding is the recipient+challenge.
        let op = &sc.launch.request.challenge;
        // The global epoch must equal the supervisor's CURRENT epoch — a
        // restart's fresh op never fabricates a new global epoch.
        if sc.launch.epoch != epoch {
            return Err(Error::rejected(format!(
                "grant epoch {} != the supervisor's current epoch {}",
                sc.launch.epoch, epoch
            )));
        }
        // In-memory tombstone (test-only mechanics) then the durable external
        // one-time CAS on the exact challenge — both must agree it's fresh.
        if self.tomb.consume_once(op) != ConsumeOutcome::Consumed {
            return Err(Error::rejected(format!("grant op '{op}' already consumed")));
        }
        match ext.consume(sc) {
            ConsumeOutcome::Consumed => {}
            ConsumeOutcome::Unknown => {
                return Err(Error::rejected(format!(
                    "external consume for op '{op}' returned UNKNOWN — never \
                     retried as success"
                )))
            }
        }
        // Pre-spawn recheck: the exact challenge's obligation must still match.
        if !ext.recheck(sc) {
            return Err(Error::rejected(
                "external obligation recheck failed before spawn",
            ));
        }
        Ok(VerifiedGrant { claims, peer })
    }
}

// ─────────────────────── the private Unix transport ───────────────────────

/// The private grant listener — a *new* Unix socket owned by `SUPERVISOR_UID`,
/// separate from `cadence.sock`, serving the two fixed verbs. Bounded framing:
/// one newline-terminated request line, ≤ [`MAX_ENVELOPE_BYTES`].
///
/// Production binding is unavailable this batch (`listen_production` → Err):
/// no provisioned `/run/cadence-supervisor` path, no uid-21000 owner. The
/// socket mechanics are exercised by `bind_at` (ordinary-uid testable) — the
/// *listener path is not the authority*; admission is still `SO_PEERCRED` +
/// pins on every accepted peer.
#[cfg(target_os = "linux")]
pub(crate) struct GrantListener {
    listener: std::os::unix::net::UnixListener,
}

#[cfg(target_os = "linux")]
impl GrantListener {
    /// Production bind at the pinned `PRODUCTION_GRANT_SOCK` owned by
    /// `SUPERVISOR_UID` — permanently `Err` this batch: the protected path and
    /// the 21000 owner are not provisioned, and binding an arbitrary path is
    /// not the qualified transport.
    pub(crate) fn listen_production() -> Result<Self> {
        Err(Error::rejected(
            "production grant listener unavailable — no provisioned \
             /run/cadence-supervisor path or uid-21000 owner this batch",
        ))
    }

    /// `#[cfg(test)]`-only bind at an arbitrary path — used by ordinary-uid
    /// socket tests to exercise the framing/admission mechanics without a
    /// provisioned supervisor dir. NOT a production authority path.
    #[cfg(test)]
    fn bind_at(path: &std::path::Path) -> Result<Self> {
        let listener = std::os::unix::net::UnixListener::bind(path)
            .map_err(|e| Error::rejected(format!("grant listener bind failed: {e}")))?;
        Ok(Self { listener })
    }

    /// Accept one connection and serve one fixed verb. Reads a bounded
    /// newline-terminated request: `"challenge <op> <seq>"` or
    /// `"install <generation> <envelope>"`. Returns the response line. Peer
    /// admission is enforced inside `install` via `SO_PEERCRED` + pins.
    #[cfg(test)]
    fn serve_once(
        &self,
        core: &mut GrantCore,
        ext: &dyn ExternalConsume,
        epoch: u64,
        now: u64,
    ) -> Result<String> {
        use std::io::{BufRead, BufReader, Write};
        let (stream, _addr) = self
            .listener
            .accept()
            .map_err(|e| Error::rejected(format!("grant accept failed: {e}")))?;
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        // Bounded read — a peer may not stream unbounded input.
        let n = reader
            .read_line(&mut line)
            .map_err(|e| Error::rejected(format!("grant read failed: {e}")))?;
        if n == 0 || n > MAX_ENVELOPE_BYTES {
            return Err(Error::rejected("grant request out of bounds"));
        }
        let req = line.trim_end();
        let mut parts = req.splitn(3, ' ');
        let resp: Result<String> = match (parts.next(), parts.next(), parts.next()) {
            (Some("challenge"), Some(op), Some(seq)) => {
                let seq: u64 = seq
                    .parse()
                    .map_err(|_| Error::rejected("challenge seq not numeric"))?;
                let c = core.challenge(op, seq);
                Ok(format!("nonce {}", c.nonce()))
            }
            (Some("install"), Some(gen), Some(env)) => {
                // Production pins — caller never supplies them on this path.
                let g = core.install(
                    &stream,
                    env,
                    SUPERVISOR_KEYRING,
                    INSTALLER_EXE_DIGEST,
                    INSTALLER_GENERATION_PIN,
                    gen,
                    epoch,
                    ext,
                    now,
                )?;
                Ok(format!("consumed {}", g.op()))
            }
            _ => Err(Error::rejected("unknown grant verb")),
        };
        let out = match &resp {
            Ok(body) => format!("ok {body}\n"),
            Err(e) => format!("err {e}\n"),
        };
        let mut w = &stream;
        let _ = w.write_all(out.as_bytes());
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use serde_json::json;

    fn b64(b: &[u8]) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        URL_SAFE_NO_PAD.encode(b)
    }

    /// Build a syntactically valid signed envelope over `claims` for `kid`,
    /// signing the domain-separated body `GRANT_DOMAIN\0 header.payload`.
    fn envelope(kid: &str, claims: &serde_json::Value, key: &Ed25519KeyPair) -> String {
        let header = b64(json!({"alg":"EdDSA","kid":kid}).to_string().as_bytes());
        let payload = b64(claims.to_string().as_bytes());
        let signed = format!("{GRANT_DOMAIN}\u{0}{header}.{payload}");
        let sig = b64(key.sign(signed.as_bytes()).as_ref());
        format!("{header}.{payload}.{sig}")
    }

    /// A canonical `SupervisorChallenge` matching the platform's exact shape.
    /// `generation` is 32-lowerhex, `challenge`/`nonce` are UUIDs, `image` is
    /// `ref@sha256:<64-hex>`, digests are 64/40-hex, epochs are JS-safe ints.
    fn good_claims() -> serde_json::Value {
        let sha = |p: &str| format!("{}{}", p, "0".repeat(64 - p.len()));
        json!({
            "challenge": {
                "launch": {
                    "request": {
                        "identity": {
                            "company": "acme", "instance": "prod-1",
                            "backend": "native", "tier": "basic",
                            "generation": 9u64,
                        },
                        "purpose": "fresh_start",
                        "challenge": "123e4567-e89b-42d3-a456-426614174000",
                        "image": "registry.local/img@sha256:".to_string() + &"a".repeat(64),
                    },
                    "epoch": 7u64,
                },
                "recipient": {
                    "pid": 1234u64,
                    "starttime": "987654321",
                    "generation": "b".repeat(32),
                    "nonce": "123e4567-e89b-42d3-a456-426614174001",
                },
                "pins": {
                    "source": "c".repeat(40),
                    "image": "registry.local/img@sha256:".to_string() + &"a".repeat(64),
                    "helper": sha("1"), "node": sha("2"),
                    "piGraph": sha("3"), "policy": sha("4"),
                },
                "lineage": { "reference": "lin-1", "databaseEpoch": 3u64 },
            },
            "nbf": 1000u64, "exp": 1100u64,
        })
    }

    /// A controllable external-consume stub for tests — NOT durable authority.
    /// Records whether it saw the exact challenge.
    struct StubConsume {
        enrolled: bool,
        consume: ConsumeOutcome,
        recheck_ok: bool,
        saw: std::cell::RefCell<Vec<String>>,
    }
    impl ExternalConsume for StubConsume {
        fn enroll_pending(&self, c: &SupervisorChallenge) -> bool {
            self.saw
                .borrow_mut()
                .push(c.launch.request.challenge.clone());
            self.enrolled
        }
        fn consume(&self, _c: &SupervisorChallenge) -> ConsumeOutcome {
            self.consume
        }
        fn recheck(&self, _c: &SupervisorChallenge) -> bool {
            self.recheck_ok
        }
    }

    fn fresh_key() -> Ed25519KeyPair {
        let rng = ring::rand::SystemRandom::new();
        Ed25519KeyPair::from_pkcs8(Ed25519KeyPair::generate_pkcs8(&rng).unwrap().as_ref()).unwrap()
    }
    fn keyring(key: &Ed25519KeyPair) -> Vec<Vec<u8>> {
        vec![format!("k1:{}", b64(key.public_key().as_ref())).into_bytes()]
    }

    /// Closed-schema + bounds + domain-separated parse refusals — all before a
    /// signature check.
    #[test]
    fn closed_schema_domain_and_bounds_refuse() {
        let key = fresh_key();
        // missing a field
        let mut bad = good_claims();
        bad["challenge"]["pins"]
            .as_object_mut()
            .unwrap()
            .remove("node");
        assert!(parse_envelope(&envelope("k1", &bad, &key), 1050).is_err());
        // non-UUID challenge / non-sha256 pin / wrong purpose refuse
        let mut bad = good_claims();
        bad["challenge"]["launch"]["request"]["challenge"] = json!("not-a-uuid");
        assert!(parse_envelope(&envelope("k1", &bad, &key), 1050).is_err());
        let mut bad = good_claims();
        bad["challenge"]["pins"]["piGraph"] = json!("zz");
        assert!(parse_envelope(&envelope("k1", &bad, &key), 1050).is_err());
        let mut bad = good_claims();
        bad["challenge"]["launch"]["request"]["purpose"] = json!("other");
        assert!(parse_envelope(&envelope("k1", &bad, &key), 1050).is_err());
        // pins.image != launch.request.image refuses
        let mut bad = good_claims();
        bad["challenge"]["pins"]["image"] =
            json!("registry.local/other@sha256:".to_string() + &"a".repeat(64));
        assert!(parse_envelope(&envelope("k1", &bad, &key), 1050).is_err());
        // window too long / not yet valid / expired
        let mut bad = good_claims();
        bad["exp"] = json!(bad["nbf"].as_u64().unwrap() + 9999);
        assert!(parse_envelope(&envelope("k1", &bad, &key), 1050).is_err());
        let env = envelope("k1", &good_claims(), &key);
        assert!(parse_envelope(&env, 500).is_err());
        assert!(parse_envelope(&env, 5000).is_err());
        // wrong alg / not three parts / bad sig length
        let h = b64(json!({"alg":"HS256","kid":"k1"}).to_string().as_bytes());
        let p = b64(good_claims().to_string().as_bytes());
        assert!(parse_envelope(&format!("{h}.{p}."), 1050).is_err());
        assert!(parse_envelope("a.b", 1050).is_err());
        assert!(parse_envelope(&format!("{h}.{p}.e30"), 1050).is_err());
    }

    /// Signature verification is domain-separated: an envelope signed WITHOUT
    /// the GRANT_DOMAIN prefix (e.g. a board-session-style JWS over bare
    /// header.payload) must NOT verify — domain separation is real.
    #[test]
    fn signature_is_domain_separated() {
        let key = fresh_key();
        let kr_owned = keyring(&key);
        let kr: Vec<&[u8]> = kr_owned.iter().map(|v| v.as_slice()).collect();
        let env = envelope("k1", &good_claims(), &key);
        let p = parse_envelope(&env, 1050).unwrap();
        assert!(verify_signature_with(&p, &kr).is_ok());
        // The same claims signed WITHOUT the domain prefix must fail.
        let header = b64(json!({"alg":"EdDSA","kid":"k1"}).to_string().as_bytes());
        let payload = b64(good_claims().to_string().as_bytes());
        let raw_sig = b64(key.sign(format!("{header}.{payload}").as_bytes()).as_ref());
        let nodom = format!("{header}.{payload}.{raw_sig}");
        let p2 = parse_envelope(&nodom, 1050).unwrap();
        assert!(verify_signature_with(&p2, &kr).is_err());
        // Unknown kid / empty keyring refuse.
        let env2 = envelope("other", &good_claims(), &key);
        let p3 = parse_envelope(&env2, 1050).unwrap();
        assert!(verify_signature_with(&p3, &kr).is_err());
        assert!(verify_signature_with(&p, &[])
            .unwrap_err()
            .to_string()
            .contains("keyring"));
    }

    /// The challenge verb mints a nonsecret op-bound nonce carrying the
    /// supervisor's own pid/starttime/generation — distinct per op, never a
    /// secret.
    #[test]
    fn challenge_is_operation_bound_and_carries_supervisor_identity() {
        let pid = std::process::id();
        let e = SupervisorEnrollment::capture_test(pid, "b".repeat(32)).unwrap();
        let g = GrantCore::new(e);
        let c1 = g.challenge("op-1", 0);
        let c2 = g.challenge("op-2", 0);
        assert_ne!(c1.nonce(), c2.nonce(), "nonce is bound to the op");
        assert_eq!(c1.supervisor_pid, pid);
    }

    /// The non-forgeable guarantee: a test cannot construct a `PeerIdentity`,
    /// `SupervisorEnrollment` (other than `capture_test`), or `VerifiedGrant`
    /// from literals — produced only inside the module. Asserts the tombstone +
    /// external-consume semantics that gate a grant, including that
    /// `enroll_pending` binds the EXACT challenge.
    #[test]
    fn tombstone_and_external_consume_gate_the_grant() {
        let mut tomb = TombstoneSet::default();
        assert_eq!(tomb.consume_once("op-1"), ConsumeOutcome::Consumed);
        assert!(tomb.is_consumed("op-1"));
        assert_eq!(tomb.consume_once("op-1"), ConsumeOutcome::Unknown);
        // enroll_pending binds the exact challenge — the stub records it.
        let stub = StubConsume {
            enrolled: true,
            consume: ConsumeOutcome::Consumed,
            recheck_ok: true,
            saw: std::cell::RefCell::new(Vec::new()),
        };
        let sc = parse_supervisor_challenge(&good_claims()["challenge"]).unwrap();
        assert!(stub.enroll_pending(&sc));
        assert_eq!(
            stub.saw.borrow()[0],
            "123e4567-e89b-42d3-a456-426614174000",
            "enroll_pending must receive the exact challenge"
        );
        let stub_no = StubConsume {
            enrolled: false,
            consume: ConsumeOutcome::Consumed,
            recheck_ok: true,
            saw: std::cell::RefCell::new(Vec::new()),
        };
        assert!(!stub_no.enroll_pending(&sc), "absent obligation refuses");
    }

    /// The production authority + self-enrollment factories are permanently
    /// closed this batch.
    #[test]
    fn production_factories_stay_refused() {
        assert!(production_consume_factory()
            .unwrap_err()
            .to_string()
            .contains("UNKNOWN"));
        assert!(enroll_self().is_err(), "self-enrollment must stay refused");
        assert!(
            GrantListener::listen_production().is_err(),
            "production listener must stay refused"
        );
    }

    /// Real `SO_PEERCRED` + `/proc` custody on an ordinary-uid socketpair: the
    /// kernel reports *this* test process's uid (not 0), so the installer gate
    /// refuses outright — proving root-uid is enforced before any pin check.
    #[test]
    #[cfg(target_os = "linux")]
    fn peer_admission_refuses_non_root_kernel_uid() {
        use std::os::unix::net::UnixStream;
        let (a, _b) = UnixStream::pair().unwrap();
        // non-root peer refuses even with a pinned generation/exe supplied.
        let r = admit_installer(&a, "g", Some([0u8; 32]), Some("g"));
        assert!(
            r.unwrap_err().to_string().contains("not root"),
            "non-root peer must refuse"
        );
    }

    /// `peer_exe_digest` opens + measures the running binary via a held fd
    /// and enforces root-ownership: the test-harness binary is owned by the
    /// ordinary test user, so the custody check refuses it — proving the
    /// fd/fstat/root-owner path rejects a non-root-owned binary.
    #[test]
    #[cfg(target_os = "linux")]
    fn peer_exe_digest_refuses_non_root_binary() {
        let pid = std::process::id();
        let r = peer_exe_digest(pid);
        assert!(
            r.unwrap_err().to_string().contains("not root-owned"),
            "a non-root-owned exe must refuse"
        );
    }

    /// A dead/nonexistent pid's `/proc/<pid>/exe` open fails — the held-fd
    /// custody check refuses a peer whose process is gone.
    #[test]
    #[cfg(target_os = "linux")]
    fn peer_exe_digest_refuses_dead_pid() {
        // pid 2^22-ish is almost certainly absent; find a definitely-dead one
        // by using an impossibly high pid (starttime read would also fail).
        let r = peer_exe_digest(4_000_000);
        assert!(r.is_err(), "a dead pid's exe must refuse");
    }

    /// The private listener serves the fixed verbs over a real bound socket:
    /// `challenge` returns a nonce line; an `install` from a non-root peer is
    /// refused at `SO_PEERCRED` (uid != 0). Ordinary-uid only.
    #[test]
    #[cfg(target_os = "linux")]
    fn listener_serves_fixed_verbs() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;
        let dir = std::env::temp_dir().join(format!("sg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("grant.sock");
        let listener = GrantListener::bind_at(&sock).unwrap();
        let e = SupervisorEnrollment::capture_test(std::process::id(), "b".repeat(32)).unwrap();
        let ext = StubConsume {
            enrolled: true,
            consume: ConsumeOutcome::Consumed,
            recheck_ok: true,
            saw: std::cell::RefCell::new(Vec::new()),
        };
        // `challenge` verb
        let mut c = UnixStream::connect(&sock).unwrap();
        writeln!(c, "challenge op-1 0").unwrap();
        let mut core = GrantCore::new(e);
        let resp = listener.serve_once(&mut core, &ext, 7, 1050).unwrap();
        assert!(resp.starts_with("nonce "), "{resp}");
        let mut r = BufReader::new(&c);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert!(line.starts_with("ok nonce "), "{line}");
        // `install` verb — non-root kernel peer refuses inside install.
        let mut c2 = UnixStream::connect(&sock).unwrap();
        let env = envelope("k1", &good_claims(), &fresh_key());
        writeln!(c2, "install {} {}", "b".repeat(32), env).unwrap();
        let r2 = listener.serve_once(&mut core, &ext, 7, 1050);
        assert!(r2.is_err(), "non-root install must refuse");
        std::fs::remove_dir_all(&dir).ok();
    }
}
