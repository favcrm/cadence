//! CAD-1017 / CAD-1085 r5 — the protected supervisor **grant consumer**.
//!
//! This module implements the *consumer* half of the private supervisor grant
//! protocol on top of the root owner's r5 peer proofs:
//!
//!   * **Supervisor is the server** — a private Unix listener owned by the
//!     pinned supervisor account (`cadence-supervisor`, uid 21000), separate
//!     from `cadence.sock`. The **root image-pinned installer** is the client;
//!     it may *deliver* a grant but never *ask for* one. A guest peer refuses.
//!   * **Admission is owner-measured**: the uid-21000 supervisor cannot open a
//!     root installer's `/proc/<pid>/exe` (needs `CAP_SYS_PTRACE`). It takes
//!     the actual kernel `SO_PEERCRED` pid/starttime, then asks the root owner
//!     for the signed `ChallengePeerProofV1` / `InstallerPeerProofV1` over the
//!     authenticated owner channel. The owner holds the fd, verifies root uid,
//!     stable starttime, the pinned executable digest and the enrolled
//!     generation, and signs only measured facts.
//!   * **Two non-forgeable identities**: [`ChallengePeerIdentity`] is minted
//!     only by the challenge-only proof and is accepted only by
//!     [`GrantCore::challenge`]; [`PeerIdentity`] is minted only by the
//!     prepared installer proof and is the only type [`GrantCore::install`]
//!     can use. The install consumer therefore cannot accept a correctly
//!     signed challenge-only identity.
//!   * **Two fixed actions only**: `challenge-v1` verifies the challenge-only
//!     proof, builds the exact canonical challenge from the owner-registered
//!     intent and binds it with `enroll_pending`; `install-v1` verifies the
//!     prepared proof, the root-pinned issuer signature, the exact prepared
//!     challenge, the recipient, the current owner epoch and the one-time
//!     durable consume + recheck.
//!   * **Replay/epoch**: a durable allocating intent must exist before the
//!     challenge, and a durable prepared obligation before install. The
//!     in-memory tombstone is defence-in-depth, not durable authority; a
//!     restored DB never auto-loads.
//!
//! The production factories ([`GrantCore::production_local`],
//! [`GrantListener::listen_production`]) are enabled only through this measured
//! path; a missing profile, keyring, owner channel or pin still refuses.

#![allow(dead_code)]

#[cfg(test)]
#[path = "supervisor_grant_acceptance.rs"]
mod acceptance;

// `AsRawFd` is only used by the Linux cfg'd peer/exe/listener paths; guarding
// the import keeps macOS (and other unix targets) free of an unused-import
// `-D warnings` failure while Linux still gets it.
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;

use crate::error::{Error, Result};
use crate::protected_runtime::proof;
use crate::protected_runtime::types::PreparedRuntime;
use crate::protected_runtime::{LocalProfile, RootOwnerClient};

#[cfg(target_os = "linux")]
use std::os::unix::fs::FileTypeExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;

// ────────────────── private reviewed constants (never caller-supplied) ────

/// The supervisor account's fixed uid — the server that owns the private
/// grant listener. Asserted as a constant bound (NSS resolution lives in the
/// topology layer); a socket owned by anyone else is never a grant channel.
pub(crate) const SUPERVISOR_UID: u32 = 21000;

/// The reviewed, image-pinned Ed25519 verifying keys for the supervisor grant
/// — `"<kid>:<base64url-x>"` pairs. The production grant core now builds its
/// keyring from the root-owned profile's `issuer_keys`; this compiled constant
/// remains the separate CAD-1113 enrolled route's input and is empty here, so
/// that route still refuses.
const SUPERVISOR_KEYRING: &[&[u8]] = &[];

/// Legacy test-only constant for the held `/proc/<pid>/exe` admission helper.
/// The production path never uses it: the root owner measures the installer.
const INSTALLER_EXE_DIGEST: Option<[u8; 32]> = None;

/// Legacy test-only generation pin for the same helper.
const INSTALLER_GENERATION_PIN: Option<&'static str> = None;

/// The private grant-listener path under the supervisor's runtime dir —
/// a *new* socket, separate from `cadence.sock`. Production resolution is
/// unavailable this batch (no provisioned path); [`GrantListener::bind_at`]
/// is the `#[cfg(test)]`-only injection used by ordinary-uid socket tests.
#[cfg(target_os = "linux")]
const PRODUCTION_GRANT_SOCK: &str = crate::protected_runtime::SUPERVISOR_GRANT_SOCKET;

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

/// Mint the supervisor's own enrollment from **actual process facts** when the
/// root-owned `native-oci-agent/1` profile is present: the running pid, its
/// live `/proc` starttime, a fresh per-process 32-hex generation, the pinned
/// supervisor uid and the pinned supervisor executable digest. Absent/invalid
/// profile, a different uid or a different binary refuses (legacy profiles keep
/// the previous refusal).
pub(crate) fn enroll_self() -> Result<SupervisorEnrollment> {
    let profile = crate::protected_runtime::LocalProfile::load_production()?;
    let facts = crate::protected_runtime::local_supervisor_facts(&profile)?;
    Ok(SupervisorEnrollment {
        pid: facts.pid,
        starttime: facts.starttime,
        generation: facts.generation,
    })
}

impl SupervisorEnrollment {
    /// Bind the live supervisor enrollment from actual process facts measured
    /// by the protected runtime (pinned uid + executable + live starttime).
    fn from_facts(facts: crate::protected_runtime::SupervisorFacts) -> Self {
        Self {
            pid: facts.pid,
            starttime: facts.starttime,
            generation: facts.generation,
        }
    }

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

/// The challenge-only peer identity — produced ONLY by
/// [`verify_challenge_peer_proof`] from a correctly signed owner
/// `ChallengePeerProofV1` that was bound to the actual kernel installer peer
/// and the exact allocating intent. Private fields, no construction outside
/// this module. It is deliberately a *different type* from [`PeerIdentity`] so
/// the install consumer cannot accept it even if it is correctly signed.
#[derive(Clone, Debug)]
pub(crate) struct ChallengePeerIdentity {
    allocation_id: String,
    launch_request_digest: String,
    installer_pid: u32,
    installer_starttime: u64,
    installer_exe_digest: String,
    installer_generation: String,
}

/// The prepared install peer identity — produced ONLY by
/// [`verify_installer_proof`] from a correctly signed owner
/// `InstallerPeerProofV1` bound to the exact canonical challenge digest and
/// the actual kernel installer peer. Private fields; no caller can mint one.
#[derive(Clone, Debug)]
pub(crate) struct PeerIdentity {
    installer_pid: u32,
    installer_starttime: u64,
    installer_exe_digest: String,
    installer_generation: String,
    challenge_digest: String,
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

/// Legacy, test-only held-FD admission helper: the uid-21000 supervisor cannot
/// actually dereference a root installer's `/proc/<pid>/exe`, so production
/// uses the owner-signed proofs above. Kept only to exercise kernel-uid and
/// pin refusals in unit tests; it never mints authority.
#[cfg(target_os = "linux")]
fn admit_installer(
    stream: &std::os::unix::net::UnixStream,
    generation_presented: &str,
    exe_pin: Option<[u8; 32]>,
    generation_pin: Option<&str>,
) -> Result<()> {
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
    Ok(())
}

/// Verify one signed `ChallengePeerProofV1` against the root-pinned owner key
/// and bind every field to the supervisor's own measured facts, the exact
/// allocating intent and the actual kernel installer peer. Returns the
/// challenge-only identity, usable ONLY by `GrantCore::challenge`.
#[allow(clippy::too_many_arguments)]
fn verify_challenge_peer_proof(
    profile: &LocalProfile,
    document: &serde_json::Value,
    request_nonce: &str,
    supervisor_pid: u32,
    supervisor_starttime: u64,
    supervisor_generation: &str,
    allocation_id: &str,
    launch_request_digest: &str,
    installer_pid: u32,
    installer_starttime: u64,
    owner_epoch: u64,
) -> Result<ChallengePeerIdentity> {
    let unsigned = proof::verify_proof(
        profile,
        proof::CHALLENGE_PEER_DOMAIN,
        proof::CHALLENGE_PEER_FIELDS,
        document,
    )?;
    let m = unsigned
        .as_object()
        .ok_or_else(|| Error::rejected("challenge peer proof is not an object"))?;
    if uget(m, "v")? != 1
        || sget(m, "request_nonce")? != request_nonce
        || sget(m, "workspace_id")? != profile.workspace_id()
        || uget(m, "owner_epoch")? != owner_epoch
        || sget(m, "allocation_id")? != allocation_id
        || sget(m, "launch_request_digest")? != launch_request_digest
        || uget(m, "supervisor_pid")? != u64::from(supervisor_pid)
        || uget(m, "supervisor_starttime")? != supervisor_starttime
        || sget(m, "supervisor_generation")? != supervisor_generation
        || uget(m, "installer_pid")? != u64::from(installer_pid)
        || uget(m, "installer_starttime")? != installer_starttime
        || sget(m, "installer_generation")? != profile.installer().generation.as_str()
        || sget(m, "installer_exe_digest")? != profile.installer().exe_digest.as_str()
        || sget(m, "purpose")? != "challenge_only"
        || sget(m, "key_id")? != proof::owner_key_id(profile).as_str()
    {
        return Err(Error::rejected(
            "challenge peer proof does not bind this supervisor/intent/installer",
        ));
    }
    Ok(ChallengePeerIdentity {
        allocation_id: allocation_id.to_string(),
        launch_request_digest: launch_request_digest.to_string(),
        installer_pid,
        installer_starttime,
        installer_exe_digest: profile.installer().exe_digest.clone(),
        installer_generation: profile.installer().generation.clone(),
    })
}

/// Verify one signed `InstallerPeerProofV1` against the root-pinned owner key
/// and bind every field to the exact canonical challenge, this supervisor and
/// the actual kernel installer peer.
#[allow(clippy::too_many_arguments)]
fn verify_installer_proof(
    profile: &LocalProfile,
    document: &serde_json::Value,
    request_nonce: &str,
    supervisor_pid: u32,
    supervisor_starttime: u64,
    challenge_digest: &str,
    installer_pid: u32,
    installer_starttime: u64,
    owner_epoch: u64,
) -> Result<PeerIdentity> {
    let unsigned = proof::verify_proof(
        profile,
        proof::INSTALLER_DOMAIN,
        proof::INSTALLER_FIELDS,
        document,
    )?;
    let m = unsigned
        .as_object()
        .ok_or_else(|| Error::rejected("installer peer proof is not an object"))?;
    if uget(m, "v")? != 1
        || sget(m, "request_nonce")? != request_nonce
        || sget(m, "workspace_id")? != profile.workspace_id()
        || uget(m, "owner_epoch")? != owner_epoch
        || sget(m, "challenge_digest")? != challenge_digest
        || uget(m, "supervisor_pid")? != u64::from(supervisor_pid)
        || uget(m, "supervisor_starttime")? != supervisor_starttime
        || uget(m, "installer_pid")? != u64::from(installer_pid)
        || uget(m, "installer_starttime")? != installer_starttime
        || sget(m, "installer_generation")? != profile.installer().generation.as_str()
        || sget(m, "installer_exe_digest")? != profile.installer().exe_digest.as_str()
        || sget(m, "key_id")? != proof::owner_key_id(profile).as_str()
    {
        return Err(Error::rejected(
            "installer peer proof does not bind this supervisor/challenge/installer",
        ));
    }
    Ok(PeerIdentity {
        installer_pid,
        installer_starttime,
        installer_exe_digest: profile.installer().exe_digest.clone(),
        installer_generation: profile.installer().generation.clone(),
        challenge_digest: challenge_digest.to_string(),
    })
}

/// The reviewed grant issuer keyring pinned in the root-owned profile. The
/// supervisor never invents or accepts a caller keyring.
fn profile_keyring(profile: &LocalProfile) -> Result<Vec<Vec<u8>>> {
    let mut keys = Vec::new();
    for key in profile.issuer_keys() {
        keys.push(
            format!(
                "{}:{}",
                key.kid,
                crate::protected_runtime::keys::b64(&key.x)
            )
            .into_bytes(),
        );
    }
    Ok(keys)
}

/// A fresh 256-bit request nonce for one owner measurement call.
fn fresh_nonce() -> Result<String> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|_| Error::unknown("entropy unavailable"))?;
    Ok(raw.iter().map(|b| format!("{b:02x}")).collect())
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

pub(crate) fn parse_request(v: &serde_json::Value) -> Result<LaunchRequest> {
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

/// Re-emit the canonical `SupervisorChallenge` bytes: the exact platform field
/// names, sorted object keys and no whitespace. This is the single owner-side
/// canonical form the runtime owner stores/digests and the installer signs; it
/// adds no second challenge grammar (it is derived from the same struct).
pub(crate) fn canonical_challenge_value(sc: &SupervisorChallenge) -> serde_json::Value {
    let mut identity = serde_json::json!({
        "company": sc.launch.request.identity.company,
        "instance": sc.launch.request.identity.instance,
        "backend": sc.launch.request.identity.backend,
        "tier": sc.launch.request.identity.tier,
        "generation": sc.launch.request.identity.generation,
    });
    if let Some(lane) = &sc.launch.request.identity.image_lane {
        identity["imageLane"] = serde_json::Value::String(lane.clone());
    }
    serde_json::json!({
        "launch": {
            "epoch": sc.launch.epoch,
            "request": {
                "challenge": sc.launch.request.challenge,
                "identity": identity,
                "image": sc.launch.request.image,
                "purpose": sc.launch.request.purpose,
            }
        },
        "lineage": {
            "databaseEpoch": sc.lineage.database_epoch,
            "reference": sc.lineage.reference,
        },
        "pins": {
            "helper": sc.pins.helper,
            "image": sc.pins.image,
            "node": sc.pins.node,
            "piGraph": sc.pins.pi_graph,
            "policy": sc.pins.policy,
            "source": sc.pins.source,
        },
        "recipient": {
            "generation": sc.recipient.generation,
            "nonce": sc.recipient.nonce,
            "pid": sc.recipient.pid,
            "starttime": sc.recipient.starttime,
        },
    })
}

pub(crate) fn canonical_challenge_bytes(sc: &SupervisorChallenge) -> Result<Vec<u8>> {
    serde_json::to_vec(&canonical_challenge_value(sc))
        .map_err(|_| Error::internal("canonical challenge cannot be encoded"))
}

/// Parse exact challenge bytes through the strict duplicate/depth parser and
/// the existing canonical challenge validator — no second grammar.
pub(crate) fn parse_challenge_bytes(bytes: &[u8]) -> Result<SupervisorChallenge> {
    let value = crate::protected_runtime::strict::parse(bytes)?;
    parse_supervisor_challenge(&value)
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
        let Some((ek, xb64)) = entry.rsplit_once(':') else {
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

// ──────────────────── typed grant issuance encoding ──────────────────────

/// Build the exact compact-JWS header/payload material the installer signs.
/// This is typed encoding only: the domain and claim shape are fixed and no
/// generic signer is exposed.
pub(crate) fn grant_signing_material(
    challenge: &SupervisorChallenge,
    nbf: u64,
    exp: u64,
    kid: &str,
) -> Result<(String, String, Vec<u8>)> {
    if !token_ok(kid, 64) {
        return Err(Error::rejected("grant issuer kid is malformed"));
    }
    if nbf == 0
        || exp <= nbf
        || exp > crate::protected_runtime::MAX_SAFE_INT
        || exp - nbf > MAX_GRANT_WINDOW_SECS
    {
        return Err(Error::rejected(
            "grant validity window is outside the bound",
        ));
    }
    let header_value = serde_json::json!({"alg": "EdDSA", "kid": kid});
    let payload_value = serde_json::json!({
        "challenge": canonical_challenge_value(challenge),
        "exp": exp,
        "nbf": nbf,
    });
    let header = crate::protected_runtime::keys::b64(
        &crate::protected_runtime::registry::canonical(&header_value)?,
    );
    let payload = crate::protected_runtime::keys::b64(
        &crate::protected_runtime::registry::canonical(&payload_value)?,
    );
    let mut material =
        Vec::with_capacity(GRANT_DOMAIN.len() + 1 + header.len() + 1 + payload.len());
    material.extend_from_slice(GRANT_DOMAIN.as_bytes());
    material.push(0);
    material.extend_from_slice(header.as_bytes());
    material.push(b'.');
    material.extend_from_slice(payload.as_bytes());
    Ok((header, payload, material))
}

/// Assemble the exact three-segment compact envelope. `header`/`payload` must
/// already be the bounded base64url strings from `grant_signing_material`.
pub(crate) fn encode_grant_envelope(
    header: &str,
    payload: &str,
    signature: &[u8],
) -> Result<String> {
    if signature.len() != 64 {
        return Err(Error::rejected("grant signature is not 64 bytes"));
    }
    let token = |value: &str| {
        !value.is_empty()
            && value.len() <= 16 * 1024
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    };
    if !token(header) || !token(payload) {
        return Err(Error::rejected("grant envelope segment is malformed"));
    }
    Ok(format!(
        "{header}.{payload}.{}",
        crate::protected_runtime::keys::b64(signature)
    ))
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
    /// The current owner epoch for this exact challenge — the external owner's
    /// authoritative value, never caller-supplied. Fallible: a transport or
    /// authority failure is a refusal, never an epoch value.
    fn current_epoch(&self, challenge: &SupervisorChallenge) -> Result<u64>;
}

/// The production external-consume prerequisites: a root-owned protected
/// profile with a pinned grant issuer keyring. The authenticated owner client
/// itself is constructed by [`GrantCore::production_local`]; this factory only
/// refuses a deployment whose reviewed pins are absent. Missing profile or
/// keyring still refuses.
pub(crate) fn production_consume_factory() -> Result<()> {
    let profile = LocalProfile::load_production()?;
    if profile_keyring(&profile)?.is_empty() {
        return Err(Error::rejected(
            "root profile pins no grant issuer keyring — no envelope can verify",
        ));
    }
    Ok(())
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
    nonce: String,
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
    profile: LocalProfile,
    owner: RootOwnerClient,
    keyring: Vec<Vec<u8>>,
    /// The exact allocating launch intents this supervisor prepared, keyed by
    /// the owner-generated operation id (the canonical challenge id).
    prepared: std::collections::HashMap<String, PreparedGrant>,
    /// Verified-install markers: operation id -> owned grant, published only
    /// after `install` fully verifies and the ACK write/flush succeeds. Never
    /// serialized, never reconstructed from a row/ACK/replay; bounded by the
    /// prepared intents and `profile.max_runtimes()`.
    installed: std::collections::HashMap<String, VerifiedGrant>,
}

#[derive(Clone)]
struct PreparedGrant {
    runtime_id: String,
    launch_request: LaunchRequest,
    launch_request_digest: String,
    recipient: Recipient,
    installer_pid: u32,
    installer_starttime: u64,
    owner_epoch: u64,
}

impl GrantCore {
    /// Bind the consume core to the supervisor's live enrollment, the
    /// root-owned profile and an authenticated `RootOwnerClient` (hello_v1 +
    /// kernel peer + pinned owner key). Missing profile/owner/keyring refuses;
    /// there is no caller-injectable trust.
    pub(crate) fn production_local(profile: &LocalProfile) -> Result<Self> {
        let enrolled = SupervisorEnrollment::from_facts(
            crate::protected_runtime::local_supervisor_facts(profile)?,
        );
        let owner = RootOwnerClient::connect(profile, enrolled.generation.clone())?;
        let keyring = profile_keyring(profile)?;
        if keyring.is_empty() {
            return Err(Error::rejected(
                "root profile pins no grant issuer keyring — grant delivery refuses",
            ));
        }
        Ok(Self {
            enrolled,
            tomb: TombstoneSet::default(),
            profile: profile.clone(),
            owner,
            keyring,
            prepared: std::collections::HashMap::new(),
            installed: std::collections::HashMap::new(),
        })
    }

    /// Record the exact allocating intent returned by `prepare_runtime`. The
    /// owner-launched installer pid/starttime/generation and the measured
    /// supervisor recipient are bound here; the canonical challenge is still
    /// absent (the owner binds it only at `challenge` → `enroll_pending`).
    pub(crate) fn register_prepared(&mut self, prepared: &PreparedRuntime) -> Result<()> {
        if prepared.profile_digest != self.profile.digest() {
            return Err(Error::rejected(
                "prepare reply is for a different protected profile",
            ));
        }
        if prepared.challenge.is_some() || prepared.challenge_digest.is_some() {
            return Err(Error::rejected(
                "prepare reply must not carry a canonical challenge",
            ));
        }
        if prepared.recipient_pid != self.enrolled.pid
            || prepared.recipient_starttime != self.enrolled.starttime
            || prepared.recipient_generation != self.enrolled.generation
        {
            return Err(Error::rejected(
                "prepared recipient is not this live supervisor instance",
            ));
        }
        if prepared.installer_generation != self.profile.installer().generation
            || prepared.installer_pid == 0
            || prepared.installer_starttime == 0
        {
            return Err(Error::rejected(
                "prepared installer is not the owner-enrolled pinned installer",
            ));
        }
        if prepared.owner_epoch != self.owner.owner_epoch() {
            return Err(Error::conflict_coded(
                "owner_epoch_changed",
                "prepared allocation belongs to a previous owner epoch",
            ));
        }
        let canonical = crate::protected_runtime::registry::canonical(&prepared.launch_request)?;
        if crate::protected_runtime::proof::launch_request_digest(&canonical)
            != prepared.launch_request_digest
        {
            return Err(Error::rejected(
                "prepared launch request does not match its committed digest",
            ));
        }
        let launch_request = parse_request(&prepared.launch_request)?;
        if launch_request.challenge != prepared.operation_id
            || launch_request.image != self.profile.image().reference
            || launch_request.identity.company != self.profile.workspace_id()
            || launch_request.identity.instance != prepared.runtime_id
            || launch_request.identity.generation != prepared.generation
            || launch_request.purpose != "fresh_start"
        {
            return Err(Error::rejected(
                "prepared launch request is not the owner intent for this runtime",
            ));
        }
        if self.prepared.contains_key(&prepared.operation_id) {
            return Err(Error::conflict_coded(
                "owner_intent_duplicate",
                "a launch intent is already registered for this operation",
            ));
        }
        self.prepared.insert(
            prepared.operation_id.clone(),
            PreparedGrant {
                runtime_id: prepared.runtime_id.clone(),
                launch_request,
                launch_request_digest: prepared.launch_request_digest.clone(),
                recipient: Recipient {
                    pid: prepared.recipient_pid,
                    starttime: prepared.recipient_starttime.to_string(),
                    generation: prepared.recipient_generation.clone(),
                    nonce: prepared.recipient_nonce.clone(),
                },
                installer_pid: prepared.installer_pid,
                installer_starttime: prepared.installer_starttime,
                owner_epoch: prepared.owner_epoch,
            },
        );
        Ok(())
    }

    /// Publish a verified-install marker. Only `serve_connection` calls this,
    /// after `install` returned Ok and the ACK write+flush succeeded. The
    /// operation must be a registered prepared intent; keep the first marker;
    /// never exceed the profile's runtime bound.
    fn record_installed(&mut self, operation: &str, grant: VerifiedGrant) {
        if !self.prepared.contains_key(operation)
            || self.installed.contains_key(operation)
            || self.installed.len() >= self.profile.max_runtimes() as usize
        {
            return;
        }
        self.installed.insert(operation.to_string(), grant);
    }

    /// Consume and remove the marker for exactly this operation.
    pub(crate) fn take_installed(&mut self, operation: &str) -> Option<VerifiedGrant> {
        self.installed.remove(operation)
    }

    /// Remove only this operation's marker (idempotent). Used for one
    /// runtime's failure/timeout/stop/fence; never touches another operation.
    pub(crate) fn remove_installed(&mut self, operation: &str) {
        let _ = self.installed.remove(operation);
        let _ = self.prepared.remove(operation);
    }

    /// The authenticated owner fence names a runtime, not a caller-selected
    /// operation. Remove only that exact generation's marker and intent.
    pub(crate) fn remove_runtime_installed(&mut self, runtime_id: &str, generation: u64) {
        self.installed.retain(|_, grant| {
            let identity = &grant.challenge().launch.request.identity;
            identity.instance != runtime_id || identity.generation != generation
        });
        self.prepared.retain(|_, intent| {
            intent.runtime_id != runtime_id
                || intent.launch_request.identity.generation != generation
        });
    }

    /// Drop ALL markers. Reserved for global owner-epoch change/invalidate-all
    /// or supervisor shutdown; never for one runtime's failure.
    pub(crate) fn clear_installed(&mut self) {
        self.installed.clear();
        self.prepared.clear();
    }

    fn pins(&self) -> Pins {
        Pins {
            source: self.profile.image().source.clone(),
            image: self.profile.image().reference.clone(),
            helper: self.profile.image().helper.clone(),
            node: self.profile.image().node.clone(),
            pi_graph: self.profile.image().pi_graph.clone(),
            policy: self.profile.image().policy.clone(),
        }
    }

    fn lineage(&self) -> Lineage {
        Lineage {
            reference: self.profile.lineage_reference().to_string(),
            database_epoch: self.profile.database_epoch(),
        }
    }

    /// `challenge` — measure the actual kernel installer through the root
    /// owner (`measure_challenge_installer_v1`), verify the signed
    /// challenge-only proof, build the exact canonical challenge from the
    /// registered intent, bind it at the owner (`enroll_pending`) and return
    /// the supervisor recipient. The `ChallengePeerIdentity` this produces is
    /// used nowhere else: it conveys no install/consume/start authority.
    #[cfg(target_os = "linux")]
    fn challenge(
        &self,
        stream: &std::os::unix::net::UnixStream,
        correlation: &super::installer_client::Correlation,
    ) -> Result<SupervisorRecipient> {
        if correlation.installer_generation != self.profile.installer().generation
            || correlation.recipient_generation != self.enrolled.generation
        {
            return Err(Error::rejected(
                "grant correlation is not this pinned installer/supervisor",
            ));
        }
        let prepared = self.prepared.get(&correlation.operation).ok_or_else(|| {
            Error::rejected(
                "no allocating launch intent for this operation — prepare_runtime first",
            )
        })?;
        let (uid_a, pid) = peer_credentials(stream)?;
        if uid_a != 0 {
            return Err(Error::rejected("installer peer is not root"));
        }
        let starttime = crate::peer::proc_starttime(pid)
            .ok_or_else(|| Error::rejected("installer starttime unreadable"))?;
        if prepared.installer_pid != pid || prepared.installer_starttime != starttime {
            return Err(Error::rejected(
                "installer peer is not the owner-enrolled installer for this intent",
            ));
        }
        let nonce = fresh_nonce()?;
        let document = self.owner.measure_challenge_installer(
            &prepared.runtime_id,
            &prepared.launch_request_digest,
            pid,
            starttime,
            &nonce,
        )?;
        let peer_identity = verify_challenge_peer_proof(
            &self.profile,
            &document,
            &nonce,
            self.enrolled.pid,
            self.enrolled.starttime,
            &self.enrolled.generation,
            &prepared.runtime_id,
            &prepared.launch_request_digest,
            pid,
            starttime,
            prepared.owner_epoch,
        )?;
        // Re-read the kernel peer AFTER the owner measurement: the pid must
        // still name the same live installer, never a reused one.
        let (uid_b, pid_b) = peer_credentials(stream)?;
        let starttime_b = crate::peer::proc_starttime(pid_b)
            .ok_or_else(|| Error::rejected("installer exited during measurement"))?;
        if uid_b != 0
            || pid_b != peer_identity.installer_pid
            || starttime_b != peer_identity.installer_starttime
        {
            return Err(Error::rejected(
                "installer kernel peer changed under measurement",
            ));
        }
        let challenge = SupervisorChallenge {
            launch: LaunchBinding {
                epoch: prepared.owner_epoch,
                request: prepared.launch_request.clone(),
            },
            recipient: prepared.recipient.clone(),
            pins: self.pins(),
            lineage: self.lineage(),
        };
        self.owner
            .enroll_pending(&prepared.runtime_id, &challenge)?;
        Ok(SupervisorRecipient {
            pid: self.enrolled.pid,
            starttime: self.enrolled.starttime,
            generation: self.enrolled.generation.clone(),
            nonce: prepared.recipient.nonce.clone(),
        })
    }

    /// `install` — measure the actual kernel installer through the root owner
    /// (`measure_installer_v1`), verify the signed prepared proof, verify the
    /// signed grant against the root-pinned issuer keyring, then require the
    /// exact prepared/allocated challenge and the supervisor's current epoch
    /// before one-time consume + recheck. A `ChallengePeerIdentity` cannot be
    /// passed here: install requires the distinct [`PeerIdentity`].
    #[cfg(target_os = "linux")]
    fn install(
        &mut self,
        stream: &std::os::unix::net::UnixStream,
        envelope: &str,
        correlation: &super::installer_client::Correlation,
        now: u64,
    ) -> Result<VerifiedGrant> {
        let parsed = parse_envelope(envelope, now)?;
        {
            let keyring: Vec<&[u8]> = self.keyring.iter().map(|k| k.as_slice()).collect();
            verify_signature_with(&parsed, &keyring)?;
        }
        let claims = parsed.claims;
        let sc = &claims.challenge;
        if sc.launch.request.challenge != correlation.operation
            || sc.recipient.generation != correlation.recipient_generation
            || correlation.recipient_generation != self.enrolled.generation
            || correlation.installer_generation != self.profile.installer().generation
        {
            return Err(Error::rejected("grant transport correlation mismatch"));
        }
        let prepared = self
            .prepared
            .get(&sc.launch.request.challenge)
            .cloned()
            .ok_or_else(|| Error::rejected("grant does not name a prepared launch intent"))?;
        if sc.launch.request != prepared.launch_request
            || sc.recipient != prepared.recipient
            || sc.launch.epoch != prepared.owner_epoch
            || sc.pins != self.pins()
            || sc.lineage != self.lineage()
        {
            return Err(Error::rejected(
                "grant does not bind the exact prepared launch intent",
            ));
        }
        let (uid, pid) = peer_credentials(stream)?;
        if uid != 0 {
            return Err(Error::rejected("install peer is not root"));
        }
        let starttime = crate::peer::proc_starttime(pid)
            .ok_or_else(|| Error::rejected("installer starttime unreadable"))?;
        if prepared.installer_pid != pid || prepared.installer_starttime != starttime {
            return Err(Error::rejected(
                "installer peer is not the owner-enrolled installer for this intent",
            ));
        }
        let nonce = fresh_nonce()?;
        let canonical = canonical_challenge_bytes(sc)?;
        let digest = crate::protected_runtime::store::challenge_digest(&canonical);
        let document = self
            .owner
            .measure_installer(&digest, pid, starttime, &nonce)?;
        let peer = verify_installer_proof(
            &self.profile,
            &document,
            &nonce,
            self.enrolled.pid,
            self.enrolled.starttime,
            &digest,
            pid,
            starttime,
            prepared.owner_epoch,
        )?;
        let (uid_b, pid_b) = peer_credentials(stream)?;
        let starttime_b = crate::peer::proc_starttime(pid_b)
            .ok_or_else(|| Error::rejected("installer exited during measurement"))?;
        if uid_b != 0 || pid_b != peer.installer_pid || starttime_b != peer.installer_starttime {
            return Err(Error::rejected(
                "installer kernel peer changed under measurement",
            ));
        }
        // The durable obligation must ALREADY be prepared — install never
        // enrolls at receipt.
        if !ExternalConsume::enrolled(&self.owner, sc) {
            return Err(Error::rejected(
                "no durable prepared obligation for this challenge",
            ));
        }
        // The grant's recipient must name THIS live supervisor instance.
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
        if !ExternalConsume::recheck(&self.owner, sc) {
            return Err(Error::rejected(
                "grant recipient.nonce/obligation does not match the prepared pending",
            ));
        }
        let epoch = ExternalConsume::current_epoch(&self.owner, sc)?;
        if sc.launch.epoch != epoch {
            return Err(Error::rejected(format!(
                "grant epoch {} != the supervisor's current epoch {}",
                sc.launch.epoch, epoch
            )));
        }
        let op = &sc.launch.request.challenge;
        if self.tomb.consume_once(op) != ConsumeOutcome::Consumed {
            return Err(Error::rejected(format!("grant op '{op}' already consumed")));
        }
        match ExternalConsume::consume(&self.owner, sc) {
            ConsumeOutcome::Consumed => {}
            ConsumeOutcome::Unknown => {
                return Err(Error::rejected(format!(
                    "external consume for op '{op}' returned UNKNOWN — never \
                     retried as success"
                )))
            }
        }
        if !ExternalConsume::recheck(&self.owner, sc) {
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
/// The production listener is bound by the pinned supervisor uid inside a
/// root-owned `root:supervisor_gid 0770` directory; the root installer connects
/// (root bypasses DAC), and every accepted peer is measured by the root owner
/// through the signed challenge/installer proofs. The listener path is not
/// itself authority.
#[cfg(target_os = "linux")]
pub(crate) struct GrantListener {
    listener: std::os::unix::net::UnixListener,
}

#[cfg(target_os = "linux")]
impl GrantListener {
    /// Bind the production grant listener as the pinned supervisor account.
    pub(crate) fn listen_production() -> Result<Self> {
        let profile = LocalProfile::load_production()?;
        let euid = unsafe { libc::geteuid() };
        if euid != profile.supervisor().uid {
            return Err(Error::rejected(
                "grant listener requires the pinned supervisor uid",
            ));
        }
        let path = std::path::Path::new(PRODUCTION_GRANT_SOCK);
        let parent = path
            .parent()
            .ok_or_else(|| Error::rejected("grant socket path has no parent"))?;
        let meta = std::fs::symlink_metadata(parent)
            .map_err(|_| Error::rejected("grant socket parent is missing"))?;
        if !meta.is_dir() || meta.uid() != 0 || meta.gid() != profile.supervisor().gid {
            return Err(Error::rejected(
                "grant socket parent must be a root-owned directory in the supervisor group",
            ));
        }
        let mode = meta.mode() & 0o777;
        if mode & 0o007 != 0 || mode & 0o070 == 0 || mode & 0o200 == 0 {
            return Err(Error::rejected(
                "grant socket parent must be 0770 root:supervisor_gid",
            ));
        }
        if let Ok(stale) = std::fs::symlink_metadata(path) {
            if !stale.file_type().is_socket() || stale.uid() != euid || stale.mode() & 0o077 != 0 {
                return Err(Error::rejected(
                    "grant socket path is not a stale supervisor-owned private socket",
                ));
            }
            std::fs::remove_file(path)
                .map_err(|_| Error::rejected("stale grant socket cannot be replaced"))?;
        }
        let listener = std::os::unix::net::UnixListener::bind(path)
            .map_err(|_| Error::rejected("grant socket cannot be bound"))?;
        let mut perms = std::fs::metadata(path)
            .map_err(|_| Error::rejected("grant socket metadata is unreadable"))?
            .permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms)
            .map_err(|_| Error::rejected("grant socket mode cannot be set"))?;
        Ok(Self { listener })
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

    /// Accept one production grant connection without holding the core. The
    /// caller locks the grant core only for `serve_accepted`, so `prepare`
    /// can register an allocation while the listener waits.
    #[cfg(target_os = "linux")]
    pub(crate) fn accept_production(&self) -> Result<std::os::unix::net::UnixStream> {
        let (stream, _addr) = self
            .listener
            .accept()
            .map_err(|_| Error::unknown("grant accept unavailable"))?;
        Ok(stream)
    }

    /// Serve one already-accepted connection with the caller's locked core.
    #[cfg(target_os = "linux")]
    pub(crate) fn serve_accepted(
        &self,
        stream: &std::os::unix::net::UnixStream,
        core: &mut GrantCore,
    ) -> Result<String> {
        let now = wall_clock_seconds()?;
        self.serve_connection(stream, core, now)
    }

    /// Serve the production grant listener, one bounded connection at a time.
    /// A refused connection never becomes a grant.
    pub(crate) fn serve_production(&self, core: &mut GrantCore) -> Result<()> {
        loop {
            let now = wall_clock_seconds()?;
            let (stream, _addr) = self
                .listener
                .accept()
                .map_err(|_| Error::unknown("grant accept unavailable"))?;
            let _ = self.serve_connection(&stream, core, now);
        }
    }

    /// Serve exactly one accepted connection — the qualification/smoke entry.
    pub(crate) fn serve_production_once(&self, core: &mut GrantCore) -> Result<String> {
        let now = wall_clock_seconds()?;
        let (stream, _addr) = self
            .listener
            .accept()
            .map_err(|_| Error::unknown("grant accept unavailable"))?;
        self.serve_connection(&stream, core, now)
    }

    /// Handle one bounded request: only the two fixed correlated verbs. Both
    /// proofs are produced by the authenticated root owner; a failure returns
    /// a framed `err` and never an acknowledged consume.
    #[cfg(target_os = "linux")]
    fn serve_connection(
        &self,
        stream: &std::os::unix::net::UnixStream,
        core: &mut GrantCore,
        now: u64,
    ) -> Result<String> {
        use std::io::Write;
        stream
            .set_write_timeout(Some(RESPONSE_BUDGET))
            .map_err(|e| Error::rejected(format!("grant write deadline failed: {e}")))?;
        let req = Self::read_request(stream, REQUEST_BUDGET)?;
        let request = super::installer_client::Request::parse(&req)?;
        // Compute the response as a Result WITHOUT `?` — a refusal must still
        // send the framed `err` line, not silently drop the connection.
        let mut installed: Option<VerifiedGrant> = None;
        let response: Result<String> = (|| {
            if request.correlation.recipient_generation != core.enrolled.generation {
                return Err(Error::rejected("recipient generation correlation mismatch"));
            }
            match request.action {
                super::installer_client::Action::Challenge => {
                    core.challenge(stream, &request.correlation)?;
                }
                super::installer_client::Action::Install => {
                    let envelope = request
                        .envelope
                        .ok_or_else(|| Error::rejected("missing grant"))?;
                    check_transport_binding(
                        envelope,
                        now,
                        &request.correlation.operation,
                        &request.correlation.recipient_generation,
                    )?;
                    let grant = core.install(stream, envelope, &request.correlation, now)?;
                    installed = Some(grant);
                }
            }
            // Only emitted AFTER the signed owner proofs, signature and durable
            // consume. The two generations are never compared to each other.
            let ack = request.acknowledgement(core.enrolled.pid, core.enrolled.starttime);
            Ok(ack
                .strip_prefix("ok ")
                .expect("fixed ack prefix")
                .strip_suffix('\n')
                .expect("fixed ack terminator")
                .to_string())
        })();
        let acknowledged = response.is_ok();
        let out = match &response {
            Ok(body) => format!("ok {body}\n"),
            Err(e) => format!("err {e}\n"),
        };
        let mut target = stream;
        target
            .write_all(out.as_bytes())
            .map_err(|e| Error::rejected(format!("grant response write failed: {e}")))?;
        target
            .flush()
            .map_err(|e| Error::rejected(format!("grant response flush failed: {e}")))?;
        // Publish only after the full install verification AND the ACK
        // write/flush succeeded. A failed write/consume is never a marker.
        if acknowledged {
            if let Some(grant) = installed {
                core.record_installed(&request.correlation.operation, grant);
            }
        }
        response
    }

    /// `#[cfg(test)]`-only single-connection wrapper for socket mechanics.
    #[cfg(test)]
    fn serve_once(
        &self,
        core: &mut GrantCore,
        _ext: &dyn ExternalConsume,
        now: u64,
    ) -> Result<String> {
        let (stream, _addr) = self
            .listener
            .accept()
            .map_err(|e| Error::rejected(format!("grant accept failed: {e}")))?;
        self.serve_connection(&stream, core, now)
    }
}

// ────────────────── coordinator drive + broker fence listener ─────────────

/// The broker's own generation writer fence. The owner calls this while the
/// supervisor is blocked waiting for the owner's `fence` reply, so the daemon
/// serves it on a dedicated thread. The implementation must refuse an unknown
/// runtime/generation and must not report success without taking the writer
/// fence (short current-eligibility reader).
pub(crate) trait BrokerFenceTarget: Send + Sync {
    #[cfg(target_os = "linux")]
    fn verify_owner_callback(
        &self,
        stream: &std::os::unix::net::UnixStream,
        owner_epoch: u64,
    ) -> Result<()>;
    fn fence_generation(
        &self,
        runtime_id: &str,
        owner_epoch: u64,
        generation: u64,
        reason: &str,
    ) -> Result<()>;
}

/// The supervisor-owned broker fence listener. Its dedicated parent is
/// `root:supervisor_gid 0770`, beneath the owner's `0750` run directory.
/// It accepts only the authenticated live root owner PID/starttime/epoch and
/// a request body signed with the profile-pinned owner key.
#[cfg(target_os = "linux")]
pub(crate) struct BrokerFenceListener {
    listener: std::os::unix::net::UnixListener,
    profile: LocalProfile,
}

#[cfg(target_os = "linux")]
impl BrokerFenceListener {
    pub(crate) fn listen_production(profile: &LocalProfile) -> Result<Self> {
        let euid = unsafe { libc::geteuid() };
        if euid != profile.supervisor().uid {
            return Err(Error::rejected(
                "broker fence listener requires the pinned supervisor uid",
            ));
        }
        let path = profile.broker_fence_socket();
        let parent = path
            .parent()
            .ok_or_else(|| Error::rejected("broker fence path has no parent"))?;
        let meta = std::fs::symlink_metadata(parent)
            .map_err(|_| Error::rejected("broker fence parent is missing"))?;
        if !meta.is_dir() || meta.uid() != 0 || meta.gid() != profile.supervisor().gid {
            return Err(Error::rejected(
                "broker fence parent must be a root-owned directory in the supervisor group",
            ));
        }
        let mode = meta.mode() & 0o777;
        if mode & 0o007 != 0 || mode & 0o070 == 0 || mode & 0o200 == 0 {
            return Err(Error::rejected(
                "broker fence parent must be 0770 root:supervisor_gid",
            ));
        }
        if let Ok(stale) = std::fs::symlink_metadata(&path) {
            if !stale.file_type().is_socket() || stale.uid() != euid || stale.mode() & 0o077 != 0 {
                return Err(Error::rejected(
                    "broker fence path is not a stale supervisor-owned private socket",
                ));
            }
            std::fs::remove_file(&path)
                .map_err(|_| Error::rejected("stale broker fence socket cannot be replaced"))?;
        }
        let listener = std::os::unix::net::UnixListener::bind(&path)
            .map_err(|_| Error::rejected("broker fence socket cannot be bound"))?;
        let mut perms = std::fs::metadata(&path)
            .map_err(|_| Error::rejected("broker fence metadata is unreadable"))?
            .permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms)
            .map_err(|_| Error::rejected("broker fence mode cannot be set"))?;
        Ok(Self {
            listener,
            profile: profile.clone(),
        })
    }

    /// Serve the fence listener forever, one bounded connection per thread.
    /// The target takes the real writer fence; its reply is only sent after
    /// that fence is held.
    pub(crate) fn serve(self: std::sync::Arc<Self>, target: std::sync::Arc<dyn BrokerFenceTarget>) {
        for incoming in self.listener.incoming() {
            let Ok(stream) = incoming else { continue };
            let me = std::sync::Arc::clone(&self);
            let target = std::sync::Arc::clone(&target);
            std::thread::spawn(move || {
                let _ = me.handle(&stream, target.as_ref());
            });
        }
    }

    fn handle(
        &self,
        mut stream: &std::os::unix::net::UnixStream,
        target: &dyn BrokerFenceTarget,
    ) -> Result<()> {
        use std::io::Write;
        if crate::protected_runtime::peer::credentials(stream)?.0 != 0 {
            return Err(Error::rejected("broker fence caller is not uid 0"));
        }
        let body = GrantListener::read_request(stream, REQUEST_BUDGET)?;
        let value = crate::protected_runtime::strict::parse(body.as_bytes())?;
        let m = crate::protected_runtime::strict::object(
            &value,
            &[
                "v",
                "kind",
                "runtime_id",
                "owner_epoch",
                "expected_generation",
                "reason",
                "nonce",
                "signature",
            ],
        )?;
        let response = (|| -> Result<serde_json::Value> {
            if crate::protected_runtime::strict::safe_u64(m, "v")? != 1
                || crate::protected_runtime::strict::string(m, "kind")? != "broker_fence"
            {
                return Err(Error::rejected("broker fence request is malformed"));
            }
            let runtime_id = crate::protected_runtime::strict::string(m, "runtime_id")?;
            let owner_epoch = crate::protected_runtime::strict::safe_u64(m, "owner_epoch")?;
            let generation = crate::protected_runtime::strict::safe_u64(m, "expected_generation")?;
            let reason = crate::protected_runtime::strict::string(m, "reason")?;
            let nonce = crate::protected_runtime::strict::string(m, "nonce")?;
            let signature = crate::protected_runtime::keys::decode_b64(
                crate::protected_runtime::strict::string(m, "signature")?,
            )?;
            let material = crate::protected_runtime::keys::fence_material(
                runtime_id,
                owner_epoch,
                generation,
                reason,
                nonce,
            )?;
            crate::protected_runtime::keys::verify_fence_signature(
                &self.profile,
                &material,
                &signature,
            )?;
            target.verify_owner_callback(stream, owner_epoch)?;
            target.fence_generation(runtime_id, owner_epoch, generation, reason)?;
            target.verify_owner_callback(stream, owner_epoch)?;
            let fenced_at_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| Error::unknown("fence clock unavailable"))?
                .as_millis() as u64;
            Ok(serde_json::json!({
                "v": 1,
                "runtime_id": runtime_id,
                "owner_epoch": owner_epoch,
                "generation": generation,
                "nonce": nonce,
                "fenced_at_ms": fenced_at_ms
            }))
        })();
        let payload = match response {
            Ok(data) => crate::protected_runtime::frame::ok(data),
            Err(e) => crate::protected_runtime::frame::err(
                if e.kind() == "rejected" {
                    "management_forbidden"
                } else {
                    "unavailable"
                },
                "broker fence refused",
            ),
        };
        crate::protected_runtime::frame::write_frame(stream, &payload, RESPONSE_BUDGET)?;
        let _ = stream.flush();
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) struct BrokerFenceListener;

#[cfg(not(target_os = "linux"))]
impl BrokerFenceListener {
    pub(crate) fn listen_production(_profile: &LocalProfile) -> Result<Self> {
        Err(Error::rejected("broker fence listener is Linux-only"))
    }
    pub(crate) fn serve(
        self: std::sync::Arc<Self>,
        _target: std::sync::Arc<dyn BrokerFenceTarget>,
    ) {
    }
}

fn wall_clock_seconds() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| Error::unknown("wall clock unavailable"))
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
        fn current_epoch(&self, c: &SupervisorChallenge) -> Result<u64> {
            if self.enrolled_record.as_ref() != Some(c) {
                return Err(Error::rejected("stub epoch unavailable for this challenge"));
            }
            Ok(self.epoch)
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

    fn fixture_profile() -> (crate::protected_runtime::keys::OwnerSigner, LocalProfile) {
        let key = fresh_key();
        let public: [u8; 32] = key
            .public_key()
            .as_ref()
            .try_into()
            .expect("32-byte Ed25519 public key");
        let profile = LocalProfile::test_fixture(public, 21000, 21000, "d".repeat(32));
        (
            crate::protected_runtime::keys::OwnerSigner::test_signer(key),
            profile,
        )
    }

    fn challenge_proof(
        signer: &crate::protected_runtime::keys::OwnerSigner,
        profile: &LocalProfile,
    ) -> serde_json::Value {
        let document = json!({
            "v": 1,
            "request_nonce": "a".repeat(64),
            "workspace_id": profile.workspace_id(),
            "owner_epoch": 7u64,
            "allocation_id": "123e4567-e89b-42d3-a456-426614174000",
            "launch_request_digest": format!("sha256:{}", "1".repeat(64)),
            "supervisor_pid": 21000u64,
            "supervisor_starttime": 987_654_321u64,
            "supervisor_generation": "b".repeat(32),
            "installer_pid": 4242u64,
            "installer_starttime": 11_111u64,
            "installer_exe_digest": profile.installer().exe_digest,
            "installer_generation": profile.installer().generation,
            "purpose": "challenge_only",
            "key_id": proof::owner_key_id(profile),
        });
        crate::protected_runtime::proof::test_sign_document(
            signer,
            proof::CHALLENGE_PEER_DOMAIN,
            document,
        )
        .unwrap()
    }

    fn installer_proof(
        signer: &crate::protected_runtime::keys::OwnerSigner,
        profile: &LocalProfile,
    ) -> serde_json::Value {
        let document = json!({
            "v": 1,
            "request_nonce": "c".repeat(64),
            "workspace_id": profile.workspace_id(),
            "owner_epoch": 7u64,
            "challenge_digest": format!("sha256:{}", "2".repeat(64)),
            "supervisor_pid": 21000u64,
            "supervisor_starttime": 987_654_321u64,
            "installer_pid": 4242u64,
            "installer_starttime": 11_111u64,
            "installer_exe_digest": profile.installer().exe_digest,
            "installer_generation": profile.installer().generation,
            "key_id": proof::owner_key_id(profile),
        });
        crate::protected_runtime::proof::test_sign_document(
            signer,
            proof::INSTALLER_DOMAIN,
            document,
        )
        .unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn verify_challenge(
        profile: &LocalProfile,
        document: &serde_json::Value,
        nonce: &str,
        installer_starttime: u64,
        owner_epoch: u64,
    ) -> Result<ChallengePeerIdentity> {
        verify_challenge_peer_proof(
            profile,
            document,
            nonce,
            21000,
            987_654_321,
            &"b".repeat(32),
            "123e4567-e89b-42d3-a456-426614174000",
            &format!("sha256:{}", "1".repeat(64)),
            4242,
            installer_starttime,
            owner_epoch,
        )
    }

    #[test]
    fn owner_peer_proofs_bind_exactly_and_refuse_tampering() {
        let (signer, profile) = fixture_profile();
        let challenge = challenge_proof(&signer, &profile);
        assert!(
            verify_challenge(&profile, &challenge, &"a".repeat(64), 11_111, 7).is_ok(),
            "a correctly signed, bound challenge proof verifies"
        );
        // Wrong nonce.
        assert!(verify_challenge(&profile, &challenge, &"f".repeat(64), 11_111, 7).is_err());
        // Changed installer pid/starttime (copied proof) refuses.
        assert!(verify_challenge(&profile, &challenge, &"a".repeat(64), 11_112, 7).is_err());
        // Old/changed owner epoch refuses.
        assert!(verify_challenge(&profile, &challenge, &"a".repeat(64), 11_111, 8).is_err());
        // A proof whose purpose is not challenge_only refuses.
        let mut bad_purpose = challenge.clone();
        bad_purpose["purpose"] = json!("install");
        let mut unsigned = bad_purpose.clone();
        unsigned.as_object_mut().unwrap().remove("signature");
        let resigned = crate::protected_runtime::proof::test_sign_document(
            &signer,
            proof::CHALLENGE_PEER_DOMAIN,
            unsigned,
        )
        .unwrap();
        assert!(verify_challenge(&profile, &resigned, &"a".repeat(64), 11_111, 7).is_err());
        // A flipped signature refuses.
        let mut forged = challenge.clone();
        forged["signature"] = json!("A".repeat(86));
        assert!(verify_challenge(&profile, &forged, &"a".repeat(64), 11_111, 7).is_err());
        // A different owner key cannot sign for the pinned owner.
        let (foreign_signer, _) = fixture_profile();
        let foreign = challenge_proof(&foreign_signer, &profile);
        assert!(verify_challenge(&profile, &foreign, &"a".repeat(64), 11_111, 7).is_err());

        // The prepared installer proof is a different domain/field set.
        let prepared = installer_proof(&signer, &profile);
        assert!(verify_installer_proof(
            &profile,
            &prepared,
            &"c".repeat(64),
            21000,
            987_654_321,
            &format!("sha256:{}", "2".repeat(64)),
            4242,
            11_111,
            7
        )
        .is_ok());
        // A challenge-only proof can never satisfy the install verifier, and a
        // prepared proof can never satisfy the challenge verifier.
        assert!(verify_installer_proof(
            &profile,
            &challenge,
            &"a".repeat(64),
            21000,
            987_654_321,
            &format!("sha256:{}", "1".repeat(64)),
            4242,
            11_111,
            7
        )
        .is_err());
        assert!(verify_challenge(&profile, &prepared, &"c".repeat(64), 11_111, 7).is_err());
        // A wrong canonical challenge digest refuses the prepared proof.
        assert!(verify_installer_proof(
            &profile,
            &prepared,
            &"c".repeat(64),
            21000,
            987_654_321,
            &format!("sha256:{}", "3".repeat(64)),
            4242,
            11_111,
            7
        )
        .is_err());
    }

    /// The production factories are wired to the reviewed root profile. On a
    /// host without that root-owned deployment (the ordinary test host) every
    /// one of them still refuses — a missing profile is never a readiness
    /// shortcut.
    #[test]
    fn production_factories_refuse_without_the_root_profile() {
        if LocalProfile::load_production().is_err() {
            assert!(production_consume_factory().is_err());
            assert!(enroll_self().is_err(), "self-enrollment must refuse");
            #[cfg(target_os = "linux")]
            assert!(
                GrantListener::listen_production().is_err(),
                "production listener must refuse without the profile"
            );
        }
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
}
