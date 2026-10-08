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

// `AsRawFd` is only used by the Linux cfg'd peer/exe/listener paths; guarding
// the import keeps macOS (and other unix targets) free of an unused-import
// `-D warnings` failure while Linux still gets it.
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;

use crate::error::{Error, Result};

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

/// The absolute whole-request deadline for the grant listener — the total
/// elapsed time a peer may hold the connection while sending one request,
/// regardless of how slowly bytes trickle in. Monotonic `Instant`-based.
#[cfg(target_os = "linux")]
const REQUEST_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// The absolute deadline for writing one response — a peer that stops reading
/// must not hold the writer.
#[cfg(target_os = "linux")]
const RESPONSE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

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
// NOTE: `st_nlink` is u64 on x86_64/most ABIs but u32 on some 32-bit/arm
// targets, and `st_uid`/`st_mode` widths also vary — keep the stability
// fingerprint to the inode/size/timestamps (the fields that must not change
// under the read). Ownership + link-count + writability are checked against
// the raw `libc::stat` in `peer_exe_digest`, not folded into the equality
// fingerprint (avoids a cross-ABI widening/cast lint trap).
#[derive(PartialEq, Eq)]
struct ExeStat {
    ino: u64,
    size: u64,
    mtime_ns: i128,
    ctime_ns: i128,
}
#[cfg(target_os = "linux")]
fn exe_stat(m: &libc::stat) -> ExeStat {
    ExeStat {
        ino: m.st_ino,
        size: m.st_size as u64,
        mtime_ns: (m.st_mtime as i128) * 1_000_000_000 + m.st_mtime_nsec as i128,
        ctime_ns: (m.st_ctime as i128) * 1_000_000_000 + m.st_ctime_nsec as i128,
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
pub(super) fn peer_exe_digest(pid: u32) -> Result<[u8; 32]> {
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
    // Must be a regular file (not a fifo/dir/symlink), root-owned, not
    // group/other-writable, and linked — the installed helper binary's
    // custody. Read these off the raw stat (widths vary across ABIs).
    if st.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(Error::rejected("/proc/<pid>/exe is not a regular file"));
    }
    if st.st_uid != 0 {
        return Err(Error::rejected("installer binary is not root-owned"));
    }
    if st.st_mode & 0o022 != 0 {
        return Err(Error::rejected("installer binary is group/other-writable"));
    }
    if st.st_nlink < 1 {
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

pub(crate) fn parse_launch(v: &serde_json::Value) -> Result<LaunchBinding> {
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

pub(crate) fn parse_lineage(v: &serde_json::Value) -> Result<Lineage> {
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
}

/// The external durable-obligation + one-time-consume port — the authentic
/// owner of the global/company enrollment and the replay CAS. Implemented by
/// an external owner (not this batch); the default is `Err`/unavailable. A
/// lost ack stays `Unknown`, never retried.
///
/// The durable obligation binds the **exact canonical `SupervisorChallenge`**
/// and must exist **before** the installer delivers a grant: `install` only
/// *queries* an already-enrolled challenge via `enrolled`, never enrolls at
/// receipt. `enroll_pending` is the external owner's pre-delivery record;
/// `consume` CASes that exact challenge to consumed; `recheck` re-validates
/// it (including the recipient nonce) before spawn.
pub(crate) trait ExternalConsume {
    /// Durably enroll the obligation for this exact challenge. Called by the
    /// external owner *before* a grant is delivered — never from `install`.
    /// `true` only on a confirmed durable commit.
    fn enroll_pending(&self, challenge: &SupervisorChallenge) -> bool;
    /// Query whether this exact challenge is already durably enrolled — the
    /// pre-delivery gate. `install` refuses a challenge that is not enrolled.
    fn enrolled(&self, challenge: &SupervisorChallenge) -> bool;
    /// One-time CAS of this exact challenge to consumed. Returns `Consumed`
    /// only on a confirmed first-time commit; replay / lost ack / absent
    /// enrollment returns `Unknown`.
    fn consume(&self, challenge: &SupervisorChallenge) -> ConsumeOutcome;
    /// Re-check that this exact challenge's obligation (including the
    /// recipient nonce) still matches before an eventual spawn.
    fn recheck(&self, challenge: &SupervisorChallenge) -> bool;
    /// The current global epoch — the external owner's authoritative value,
    /// never caller-supplied.
    fn current_epoch(&self) -> u64;
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

/// Correlation preflight using the EXISTING grant parser, not a second codec.
/// This is syntax/binding evidence only. Signature, kernel admission and durable
/// consume remain mandatory in `GrantCore::install`; this never consumes.
#[cfg(target_os = "linux")]
pub(super) fn check_transport_binding(
    envelope: &str,
    now: u64,
    operation: &str,
    recipient_generation: &str,
) -> Result<()> {
    let parsed = parse_envelope(envelope, now)?;
    if parsed.claims.challenge.launch.request.challenge != operation
        || parsed.claims.challenge.recipient.generation != recipient_generation
    {
        return Err(Error::rejected("grant transport correlation mismatch"));
    }
    Ok(())
}

/// Private signed-format evidence for the separate CAD-1113 enrolled route.
/// No peer, consume, launch or enrollment authority; fields cannot be literal.
pub(super) struct VerifiedEnvelope {
    claims: GrantClaims,
}
impl VerifiedEnvelope {
    pub(super) fn claims(&self) -> &GrantClaims {
        &self.claims
    }
}
pub(super) fn production_grant_keyring() -> Result<&'static [&'static [u8]]> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        crate::installer_bundle::constructor::grant_keys()
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    Err(Error::unknown(
        "qualified constructor grant keyring unavailable",
    ))
}
/// Reuses BOTH existing parsers. The receipt binding bytes are already verified
/// canonical; parsing its challenge preserves optional imageLane presence.
/// Low-level dependency-explicit crypto mechanics, not an admission facade.
pub(super) fn verify_enrolled_format(
    envelope: &str,
    keys: &[&[u8]],
    now: u64,
    canonical_binding: &[u8],
) -> Result<VerifiedEnvelope> {
    let parsed = parse_envelope(envelope, now)?;
    verify_signature_with(&parsed, keys)?;
    let value: serde_json::Value = serde_json::from_slice(canonical_binding)
        .map_err(|_| Error::unknown("canonical enrollment binding unavailable"))?;
    let expected = parse_supervisor_challenge(
        value
            .get("challenge")
            .ok_or_else(|| Error::unknown("canonical challenge unavailable"))?,
    )?;
    if parsed.claims.challenge != expected {
        return Err(Error::unknown("full challenge mismatch"));
    }
    Ok(VerifiedEnvelope {
        claims: parsed.claims,
    })
}

// ─────────────────────────── the fixed action verbs ───────────────────────

/// The verified grant — evidence a correctly-signed, kernel-admitted,
/// in-window envelope over the canonical `SupervisorChallenge` was consumed
/// exactly once against the supervisor's current epoch. **Not** launch
/// authority: the durable external consume and the protected spawn are still
/// `Err`/unavailable this batch.
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

/// The supervisor's live identity the `challenge` verb echoes — the values a
/// signed grant's `recipient` must bind. Private fields; produced only inside
/// the module from the enrolled supervisor, never a caller literal. There is
/// no supervisor-minted nonce: the canonical `recipient.nonce` is the
/// externally-enrolled pending nonce, not something this endpoint invents.
#[derive(Clone, Debug)]
pub(crate) struct SupervisorRecipient {
    pid: u32,
    starttime: u64,
    generation: String,
}

/// The grant-consume core bound to the supervisor's own enrollment — the
/// object that owns the `challenge`/`install` verbs. Separate from the
/// transport ([`GrantListener`]); constructed only from a real
/// `SupervisorEnrollment`, never a caller literal. `GrantCore` has **no**
/// caller-injectable trust — every trust input is a private constant resolved
/// inside `install`/`challenge`.
pub(crate) struct GrantCore {
    enrolled: SupervisorEnrollment,
    tomb: TombstoneSet,
}

impl GrantCore {
    /// Bind the consume core to the supervisor's live enrollment. Production
    /// enrollment comes only from `enroll_self` (unavailable this batch).
    fn new(enrolled: SupervisorEnrollment) -> Self {
        Self {
            enrolled,
            tomb: TombstoneSet::default(),
        }
    }

    /// `challenge` — admit the kernel installer and return the supervisor's
    /// live `recipient` identity (pid/starttime/generation). The supervisor
    /// does NOT mint a nonce: the canonical `recipient.nonce` is assigned by
    /// the external enrollment owner and bound at `install`. Peer admission
    /// (`SO_PEERCRED` + `/proc` custody + pins) runs here exactly as in
    /// `install` — a `challenge` request is not a lighter-trust path.
    #[cfg(target_os = "linux")]
    fn challenge(
        &self,
        stream: &std::os::unix::net::UnixStream,
        generation_presented: &str,
    ) -> Result<SupervisorRecipient> {
        // Admit the kernel peer with the same pinned custody as `install` —
        // a challenge is served only to the pinned installer, never a guest.
        admit_installer(
            stream,
            generation_presented,
            INSTALLER_EXE_DIGEST,
            INSTALLER_GENERATION_PIN,
        )?;
        Ok(SupervisorRecipient {
            pid: self.enrolled.pid,
            starttime: self.enrolled.starttime,
            generation: self.enrolled.generation.clone(),
        })
    }

    /// `install` — admit the kernel peer, then verify+consume the signed
    /// envelope over the canonical `SupervisorChallenge`. ALL trust inputs
    /// (keyring, exe pin, generation pin) are the private reviewed constants —
    /// `install` takes no caller key/pin/epoch authority. The durable external
    /// port supplies the current epoch via `ext.current_epoch()` and the
    /// already-enrolled obligation via `ext.enrolled`. On success returns a
    /// non-forgeable [`VerifiedGrant`]; any failure refuses (a consumed op is
    /// tombstoned-Unknown, never retried).
    #[cfg(target_os = "linux")]
    fn install(
        &mut self,
        stream: &std::os::unix::net::UnixStream,
        envelope: &str,
        generation_presented: &str,
        ext: &dyn ExternalConsume,
        now: u64,
    ) -> Result<VerifiedGrant> {
        // Kernel peer + process custody first — pins are private constants.
        let peer = admit_installer(
            stream,
            generation_presented,
            INSTALLER_EXE_DIGEST,
            INSTALLER_GENERATION_PIN,
        )?;
        // Parse + signature-verify against the private compiled keyring.
        let parsed = parse_envelope(envelope, now)?;
        verify_signature_with(&parsed, SUPERVISOR_KEYRING)?;
        let claims = parsed.claims;
        let sc = &claims.challenge;
        // The durable global/company obligation for THIS exact challenge must
        // ALREADY be enrolled — delivery is only honoured after a pre-existing
        // enrollment; install never enrolls at receipt.
        if !ext.enrolled(sc) {
            return Err(Error::rejected(
                "no durable obligation enrolled for this challenge — grant not pre-registered",
            ));
        }
        // The grant's recipient must name THIS live supervisor instance: the
        // enrolled pid, starttime (decimal string) and generation — binding
        // the grant to this process, not a stale or guest one.
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
        // The grant's `recipient.nonce` must equal the nonce the external owner
        // enrolled for this exact challenge — recheck binds the full tuple
        // including the nonce; a stale/mismatched nonce refuses.
        if !ext.recheck(sc) {
            return Err(Error::rejected(
                "grant recipient.nonce/obligation does not match the enrolled pending",
            ));
        }
        // The global epoch must equal the supervisor's CURRENT epoch — the
        // external owner reports it; install never trusts a caller epoch.
        let epoch = ext.current_epoch();
        if sc.launch.epoch != epoch {
            return Err(Error::rejected(format!(
                "grant epoch {} != the supervisor's current epoch {}",
                sc.launch.epoch, epoch
            )));
        }
        // In-memory tombstone (test-only mechanics) then the durable external
        // one-time CAS on the exact challenge — both must agree it's fresh.
        let op = &sc.launch.request.challenge;
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

    /// Separate typed enrolled route. No legacy UID0 admission is widened.
    /// All production receiver factories refuse, before any request is read.
    #[cfg(target_arch = "x86_64")]
    pub(super) fn serve_enrolled_once(&self) -> Result<()> {
        // Missing authority refuses before accept/read, not after consume.
        let until = std::time::Instant::now() + REQUEST_BUDGET;
        let receiver = super::installer_enrolled::production_enrolled_receiver()?;
        // This separately typed loop owns the listener (no legacy dispatch).
        self.listener
            .set_nonblocking(true)
            .map_err(|_| Error::unknown("enrolled listener UNKNOWN"))?;
        super::installer_client::Deadline::until(until)
            .wait(self.listener.as_raw_fd(), libc::POLLIN)?;
        let (stream, _) = self
            .listener
            .accept()
            .map_err(|_| Error::unknown("enrolled accept UNKNOWN"))?;
        receiver.serve_until(&stream, until)
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

    /// Read one newline-terminated request into `buf`, incrementally, under a
    /// single **absolute** `Instant` deadline for the whole request plus a
    /// hard byte cap (≤ `MAX_ENVELOPE_BYTES` + 1). The socket read timeout is
    /// recomputed to the *remaining* budget before each `read`, so a slow
    /// trickle (a byte every few seconds) cannot hold the server — the total
    /// elapsed time is bounded, not per-read. Rejects an oversize line, a
    /// missing newline, a closed stream, or an elapsed deadline; never
    /// allocates unboundedly before the bound is checked.
    ///
    /// `budget` is the whole-request deadline; `#[cfg(test)]` injects a short
    /// one for the slow-trickle regression. Production callers pass the fixed
    /// `REQUEST_BUDGET`.
    #[cfg(target_os = "linux")]
    fn read_request(
        stream: &std::os::unix::net::UnixStream,
        budget: std::time::Duration,
    ) -> Result<String> {
        use std::io::Read;
        let deadline = std::time::Instant::now() + budget;
        let mut buf: Vec<u8> = Vec::with_capacity(1024);
        let mut chunk = [0u8; 4096];
        let mut s = stream;
        loop {
            // Recompute the remaining budget and refuse the whole request once
            // it elapses — the deadline is absolute, not per-read.
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(Error::rejected("grant request deadline elapsed"));
            }
            let remaining_budget = deadline - now;
            s.set_read_timeout(Some(remaining_budget))
                .map_err(|e| Error::rejected(format!("grant read deadline failed: {e}")))?;
            // Bound the read window: never read past MAX_ENVELOPE_BYTES + 1, so
            // a peer streaming more than the cap is refused before the buffer
            // grows unboundedly — and the cap is checked *before* each read.
            let remaining_bytes = MAX_ENVELOPE_BYTES + 1 - buf.len();
            if remaining_bytes == 0 {
                return Err(Error::rejected("grant request exceeds the size bound"));
            }
            let want = std::cmp::min(remaining_bytes, chunk.len());
            match s.read(&mut chunk[..want]) {
                Ok(0) => {
                    return Err(Error::rejected(
                        "grant request closed without a newline terminator",
                    ))
                }
                Ok(n) => {
                    let got = &chunk[..n];
                    if let Some(pos) = got.iter().position(|&b| b == b'\n') {
                        if pos + 1 != got.len() {
                            return Err(Error::rejected("trailing grant request bytes"));
                        }
                        buf.extend_from_slice(&got[..pos]);
                        break;
                    }
                    buf.extend_from_slice(got);
                }
                // A WouldBlock/TimedOut is the deadline firing — refuse.
                Err(e) => return Err(Error::rejected(format!("grant request read failed: {e}"))),
            }
        }
        if buf.len() > MAX_ENVELOPE_BYTES {
            return Err(Error::rejected("grant request exceeds the size bound"));
        }
        String::from_utf8(buf).map_err(|_| Error::rejected("grant request is not UTF-8"))
    }

    /// Accept one connection and serve one fixed verb, under a bounded
    /// incremental read + read deadline. `challenge` admits the kernel
    /// installer (same `SO_PEERCRED` + `/proc` + pins custody as `install`)
    /// and returns the supervisor's live `recipient` identity — the nonce is
    /// NOT minted here (the canonical `recipient.nonce` is the externally
    /// enrolled pending nonce, bound at `install`). `install` verifies +
    /// consumes the signed envelope.
    #[cfg(test)]
    fn serve_once(
        &self,
        core: &mut GrantCore,
        ext: &dyn ExternalConsume,
        now: u64,
    ) -> Result<String> {
        use std::io::Write;
        let (stream, _addr) = self
            .listener
            .accept()
            .map_err(|e| Error::rejected(format!("grant accept failed: {e}")))?;
        // The whole request is bounded by an absolute deadline — a slow
        // trickle cannot hold the server. Bound the response write too.
        stream
            .set_write_timeout(Some(RESPONSE_BUDGET))
            .map_err(|e| Error::rejected(format!("grant write deadline failed: {e}")))?;
        let req = Self::read_request(&stream, REQUEST_BUDGET)?;
        let req = req.as_str(); // no whitespace normalization on correlated frames
        let mut parts = req.splitn(2, ' ');
        let verb = parts.next();
        let rest = parts.next().unwrap_or("");
        // Compute the response as a Result WITHOUT `?` — a `challenge`/`install`
        // refusal must still send the framed `err` line to the peer, not
        // silently drop the connection via an early return.
        let resp: Result<String> = match verb {
            Some("challenge-v1" | "install-v1") => (|| {
                use super::installer_client::{Action, Request};
                let request = Request::parse(req)?;
                let correlation = &request.correlation;
                if correlation.recipient_generation != core.enrolled.generation {
                    return Err(Error::rejected("recipient generation correlation mismatch"));
                }
                match request.action {
                    Action::Challenge => {
                        core.challenge(&stream, &correlation.installer_generation)?;
                    }
                    Action::Install => {
                        let envelope = request
                            .envelope
                            .ok_or_else(|| Error::rejected("missing grant"))?;
                        check_transport_binding(
                            envelope,
                            now,
                            &correlation.operation,
                            &correlation.recipient_generation,
                        )?;
                        core.install(
                            &stream,
                            envelope,
                            &correlation.installer_generation,
                            ext,
                            now,
                        )?;
                    }
                }
                // Only emitted AFTER the same legacy kernel/signature/consume
                // guards. The two generations are never compared to each other.
                let ack = request.acknowledgement(core.enrolled.pid, core.enrolled.starttime);
                Ok(ack
                    .strip_prefix("ok ")
                    .expect("fixed ack prefix")
                    .strip_suffix('\n')
                    .expect("fixed ack terminator")
                    .to_string())
            })(),
            Some("challenge") => {
                // `challenge <generation>` — admit the installer, echo the
                // supervisor's recipient identity for the external enrollment.
                let gen = rest;
                core.challenge(&stream, gen).map(|r| {
                    format!(
                        "recipient pid={} starttime={} generation={}",
                        r.pid, r.starttime, r.generation
                    )
                })
            }
            Some("install") => {
                // `install <generation> <envelope>` — a malformed frame is a
                // framed `err`, not a connection drop.
                let mut p = rest.splitn(2, ' ');
                match (p.next(), p.next()) {
                    (Some(g), Some(e)) if !e.is_empty() => core
                        .install(&stream, e, g, ext, now)
                        .map(|g| format!("consumed {}", g.op())),
                    _ => Err(Error::rejected("install requires <generation> <envelope>")),
                }
            }
            _ => Err(Error::rejected("unknown grant verb")),
        };
        let out = match &resp {
            Ok(body) => format!("ok {body}\n"),
            Err(e) => format!("err {e}\n"),
        };
        // Bounded write — errors are propagated, not swallowed.
        (&stream)
            .write_all(out.as_bytes())
            .map_err(|e| Error::rejected(format!("grant response write failed: {e}")))?;
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
    /// Holds a *pre-enrolled* challenge (the external owner's durable record)
    /// and reports whether a query matches it; `install` only ever *queries*
    /// `enrolled`, never enrolls at receipt.
    struct StubConsume {
        /// The already-enrolled challenge the external owner recorded (or None).
        enrolled_record: Option<SupervisorChallenge>,
        consume: ConsumeOutcome,
        recheck_ok: bool,
        epoch: u64,
    }
    impl ExternalConsume for StubConsume {
        fn enroll_pending(&self, _c: &SupervisorChallenge) -> bool {
            // Pre-delivery enrollment — install never calls this; tests use it
            // only to model the external owner's commit.
            true
        }
        fn enrolled(&self, c: &SupervisorChallenge) -> bool {
            self.enrolled_record.as_ref() == Some(c)
        }
        fn consume(&self, _c: &SupervisorChallenge) -> ConsumeOutcome {
            self.consume
        }
        fn recheck(&self, c: &SupervisorChallenge) -> bool {
            self.recheck_ok && self.enrolled(c)
        }
        fn current_epoch(&self) -> u64 {
            self.epoch
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

    /// `install` never enrolls at receipt: it only *queries* an already-
    /// enrolled exact challenge via `enrolled`. A missing obligation, a stale
    /// enrollment, or a nonce mismatch all refuse before consume.
    #[test]
    fn install_requires_a_pre_enrolled_exact_challenge() {
        let sc = parse_supervisor_challenge(&good_claims()["challenge"]).unwrap();
        // enrolled() is true only for the exact enrolled challenge.
        let stub = StubConsume {
            enrolled_record: Some(sc.clone()),
            consume: ConsumeOutcome::Consumed,
            recheck_ok: true,
            epoch: 7,
        };
        assert!(stub.enrolled(&sc), "the enrolled challenge is honoured");
        // A different challenge (different nonce) is not enrolled -> refuses.
        let mut other = sc.clone();
        other.recipient.nonce = "123e4567-e89b-42d3-a456-426614174002".into();
        assert!(
            !stub.enrolled(&other),
            "a nonce-mismatched challenge is not enrolled"
        );
        // recheck also binds the exact tuple incl. the nonce.
        assert!(stub.recheck(&sc));
        assert!(
            !stub.recheck(&other),
            "stale/nonce-mismatched recheck refuses"
        );
        // No enrollment at all -> enrolled() false -> install refuses.
        let stub_none = StubConsume {
            enrolled_record: None,
            consume: ConsumeOutcome::Consumed,
            recheck_ok: true,
            epoch: 7,
        };
        assert!(!stub_none.enrolled(&sc), "absent obligation refuses");
        // in-memory tombstone is single-use mechanics.
        let mut tomb = TombstoneSet::default();
        assert_eq!(tomb.consume_once("op-1"), ConsumeOutcome::Consumed);
        assert_eq!(tomb.consume_once("op-1"), ConsumeOutcome::Unknown);
    }

    /// Versioned correlation must not bypass the existing UID0/pin admission.
    /// These are real named sockets; the synthetic enrollment is not authority.
    #[test]
    #[cfg(target_os = "linux")]
    fn correlated_listener_retains_kernel_admission_refusal() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("grant.sock");
        let listener = GrantListener::bind_at(&sock).unwrap();
        let claims = good_claims();
        let sc = parse_supervisor_challenge(&claims["challenge"]).unwrap();
        let operation = sc.launch.request.challenge.clone();
        let ext = StubConsume {
            enrolled_record: Some(sc),
            consume: ConsumeOutcome::Consumed,
            recheck_ok: true,
            epoch: 7,
        };
        let enrolled =
            SupervisorEnrollment::capture_test(std::process::id(), "b".repeat(32)).unwrap();
        let mut core = GrantCore::new(enrolled);
        let grant = envelope("k1", &claims, &fresh_key());
        for frame in [
            format!(
                "challenge-v1 {operation} {} {}\n",
                "a".repeat(32),
                "b".repeat(32)
            ),
            format!(
                "install-v1 {operation} {} {} {grant}\n",
                "a".repeat(32),
                "b".repeat(32)
            ),
        ] {
            let mut client = UnixStream::connect(&sock).unwrap();
            client
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            client.write_all(frame.as_bytes()).unwrap();
            let refusal = listener.serve_once(&mut core, &ext, 1050).unwrap_err();
            if unsafe { libc::getuid() } != 0 {
                assert!(refusal.to_string().contains("not root"));
            } else {
                assert!(refusal.to_string().contains("pin"));
            }
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            assert!(
                response.starts_with("err "),
                "never an acknowledged consume"
            );
        }
        assert!(production_consume_factory().is_err());
        assert!(GrantListener::listen_production().is_err());
    }

    /// The production authority + self-enrollment factories are permanently
    /// closed this batch. The `GrantListener` production refusal is Linux-only
    /// (the listener type is cfg'd out elsewhere) — gate just that assertion
    /// so the cross-platform factory checks still build/run on macOS.
    #[test]
    fn production_factories_stay_refused() {
        assert!(production_consume_factory()
            .unwrap_err()
            .to_string()
            .contains("UNKNOWN"));
        assert!(enroll_self().is_err(), "self-enrollment must stay refused");
        #[cfg(target_os = "linux")]
        assert!(
            GrantListener::listen_production().is_err(),
            "production listener must stay refused"
        );
    }

    /// Real `SO_PEERCRED` custody on a socketpair: assert on the ACTUAL
    /// kernel-reported uid of this test process rather than assuming non-root.
    /// If the suite runs as non-root the uid!=0 gate refuses; if it runs as
    /// root, admission proceeds to the generation/exe pin checks — which still
    /// refuse because the pins are unset and the presented generation/exe are
    /// wrong. Either way the gate is enforced by the real kernel uid, and a
    /// root run does NOT silently pass as if it proved the uid gate.
    #[test]
    #[cfg(target_os = "linux")]
    fn peer_admission_refuses_unpinned_or_nonroot() {
        use std::os::unix::net::UnixStream;
        let (a, _b) = UnixStream::pair().unwrap();
        let actual_uid = unsafe { libc::getuid() };
        // Pins are unset -> any peer is refused. With unset pins a wrong
        // generation/exe refuses; with a non-root uid the uid gate refuses.
        let r = admit_installer(&a, "g", None, None);
        assert!(r.is_err(), "unset pins must refuse regardless of uid");
        let r = admit_installer(&a, "g", Some([0u8; 32]), Some("g"));
        assert!(r.is_err(), "mismatched pins must refuse regardless of uid");
        if actual_uid != 0 {
            // Non-root: the uid gate is the specific reason.
            let err = r.unwrap_err().to_string();
            assert!(
                err.contains("not root") || err.contains("pin"),
                "non-root refuses at the uid or pin gate: {err}"
            );
        }
        // The challenge verb admits the same way.
        let pid = std::process::id();
        let e = SupervisorEnrollment::capture_test(pid, "b".repeat(32)).unwrap();
        let core = GrantCore::new(e);
        let (a2, _b2) = UnixStream::pair().unwrap();
        let r2 = core.challenge(&a2, "g");
        assert!(
            r2.is_err(),
            "challenge must refuse an unpinned/non-root peer"
        );
    }

    /// `peer_exe_digest` opens + measures the running binary via a held fd and
    /// enforces custody. Assert on the ACTUAL owner of the test-harness binary
    /// rather than assuming a non-root owner: if the harness binary is not
    /// root-owned the custody check refuses with 'not root-owned'; if a test
    /// environment happens to have a root-owned harness the digest is
    /// produced. Either way the measurement is exercised for real — no silent
    /// pass is counted.
    #[test]
    #[cfg(target_os = "linux")]
    fn peer_exe_digest_measures_the_running_binary() {
        let pid = std::process::id();
        match peer_exe_digest(pid) {
            Ok(d) => {
                // Harness binary happened to satisfy custody — digest produced.
                assert_eq!(d.len(), 32);
                assert_ne!(d, [0u8; 32]);
            }
            Err(e) => {
                // Typical: the test harness is owned by the invoking user.
                let m = e.to_string();
                assert!(
                    m.contains("not root-owned")
                        || m.contains("writable")
                        || m.contains("not a regular"),
                    "custody refusal reason: {m}"
                );
            }
        }
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

    /// The private listener's bounded incremental read + deadline: a missing
    /// newline / oversize line / closed stream refuses — never a success from
    /// framing alone.
    #[test]
    #[cfg(target_os = "linux")]
    fn listener_read_is_bounded() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        // The reader is the endpoint that *receives* the peer's bytes: write
        // on `peer`, read on `server` (a connected socketpair).
        // No newline + peer closes -> refuse (not a success).
        let (server, mut peer) = UnixStream::pair().unwrap();
        peer.write_all(b"challenge no-newline").unwrap();
        drop(peer); // peer closes without a newline
        let budget = std::time::Duration::from_secs(2);
        let r = GrantListener::read_request(&server, budget);
        assert!(r.is_err(), "missing newline must refuse: {r:?}");
        // Oversize line (> MAX_ENVELOPE_BYTES) -> refuse. Write from a thread
        // so the write can block on a full socket buffer without deadlocking
        // the read; the read refuses as soon as the cap is exceeded.
        let (server2, mut peer2) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let _ = peer2.write_all(&vec![b'x'; MAX_ENVELOPE_BYTES + 4096]);
        });
        let r2 = GrantListener::read_request(&server2, budget);
        assert!(r2.is_err(), "oversize line must refuse: {r2:?}");
        // A valid newline-terminated request parses.
        let (server3, mut peer3) = UnixStream::pair().unwrap();
        writeln!(peer3, "challenge gen").unwrap();
        let r3 = GrantListener::read_request(&server3, budget).unwrap();
        assert_eq!(r3, "challenge gen");
        // Slow trickle: a peer sending one byte then stalling past the WHOLE-
        // request deadline must be refused — a per-read timeout alone lets a
        // byte-every-few-seconds peer hold the server forever. Short injected
        // budget proves the absolute Instant deadline.
        let (server4, mut peer4) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let _ = peer4.write_all(b"challenge "); // no newline, then stall
            std::thread::sleep(std::time::Duration::from_millis(800));
            let _ = peer4.write_all(b"x"); // one more byte, still no newline
            std::thread::sleep(std::time::Duration::from_secs(30));
        });
        let t0 = std::time::Instant::now();
        let r4 = GrantListener::read_request(&server4, std::time::Duration::from_millis(500));
        assert!(r4.is_err(), "slow trickle must refuse: {r4:?}");
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(5),
            "the absolute deadline bounded the read, not a per-read timeout"
        );
    }

    /// The private listener serves the fixed verbs over a real bound socket:
    /// `challenge` admits the kernel installer and returns the supervisor's
    /// recipient identity (never a minted nonce); `install` from a non-root
    /// peer is refused at `SO_PEERCRED` (uid != 0). Ordinary-uid only — both
    /// verbs refuse this test's non-root peer, proving admission gates both.
    #[test]
    #[cfg(target_os = "linux")]
    fn listener_serves_fixed_verbs() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let dir = std::env::temp_dir().join(format!("sg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("grant.sock");
        let listener = GrantListener::bind_at(&sock).unwrap();
        let sc = parse_supervisor_challenge(&good_claims()["challenge"]).unwrap();
        let ext = StubConsume {
            enrolled_record: Some(sc),
            consume: ConsumeOutcome::Consumed,
            recheck_ok: true,
            epoch: 7,
        };
        // `challenge` verb — admits the kernel installer; non-root refuses.
        let mut c = UnixStream::connect(&sock).unwrap();
        writeln!(c, "challenge {}", "b".repeat(32)).unwrap();
        let e = SupervisorEnrollment::capture_test(std::process::id(), "b".repeat(32)).unwrap();
        let mut core = GrantCore::new(e);
        let resp = listener.serve_once(&mut core, &ext, 1050);
        assert!(resp.is_err(), "non-root challenge must refuse");
        // `install` verb — non-root kernel peer refuses inside install.
        let mut c2 = UnixStream::connect(&sock).unwrap();
        let env = envelope("k1", &good_claims(), &fresh_key());
        writeln!(c2, "install {} {}", "b".repeat(32), env).unwrap();
        let r2 = listener.serve_once(&mut core, &ext, 1050);
        assert!(r2.is_err(), "non-root install must refuse");
        std::fs::remove_dir_all(&dir).ok();
    }
}
