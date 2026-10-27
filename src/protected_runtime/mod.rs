//! CAD-1085 `native-oci-agent/1` — the root-owned protected runtime authority.
//!
//! This module is the *owner/control* half of the protected native profile: a
//! finite, root-owned service (`cadence-runtime-owner`, uid 0) that owns the
//! OCI isolation, the durable owner epochs/obligations/watermark, the actual
//! kernel measurements and the per-runtime attestation/operations plumbing.
//! It holds **no user credential master key**; it never decrypts, serves or
//! signs user credentials, and it exposes no generic sign/eval/key API.
//!
//! What is real here:
//!
//! - [`profile`] — the root-owned trust profile (`/var/lib/cadence/runtime-owner/
//!   profile.json`). It is strict JSON with private fields, must live in a
//!   root-owned non-writable path, describes *expected* measurements, and has
//!   no `qualified` flag. Environment variables never contribute a trust root.
//! - [`store`] — the private authority SQLite (`owner_meta`,
//!   `runtime_obligations`, append-only `obligation_transitions`,
//!   `owner_checkpoints`, `restore_log`, `peer_enrollments`) with checked
//!   monotonic increments and CAS phase transitions. Restoring a snapshot does
//!   not restore admission.
//! - [`peer`] — actual `SO_PEERCRED` + held `/proc/<pid>/exe` digest + live
//!   starttime + enrolled generation measurement. UID 0 alone is not a pinned
//!   installer.
//! - [`docker`] — a deterministic owned Docker Engine API client. Runtime
//!   objects carry exact labels and are reconciled by name/nonce/profile/image
//!   only; no arbitrary image, mount, command or Docker flag is accepted.
//! - [`measure`] — post-start measurement of the actual guest: namespaces,
//!   uid/gid, capabilities, `NoNewPrivs`, seccomp, core limit, post-exec
//!   non-dumpability, argv/env, exe digest, image digest and cgroup.
//! - [`registry`] + [`attestation`] — the pinned Pi 1.4.0 observer's
//!   HMAC-authenticated `cadence.runtime.registry/1` channel, bound to the
//!   enrolled Node PID/starttime measured by the root owner, and compared with
//!   the immutable installed material.
//! - [`ops`] — the single per-runtime operations socket. Its listener is
//!   constructed for the enrolled runtime identity and derives a
//!   [`caller::RuntimeCaller`] from the kernel peer UID; it never calls the
//!   daemon's `operator_connection` with a supervisor PID.
//! - [`qualified`] — `QualifiedRuntime`/`CurrentEligibility`: server-created,
//!   non-serializable, non-caller-constructible readiness with a short read
//!   guard for final admission, and the external [`checkpoint`] watermark.
//!
//! What refuses, truthfully:
//!
//! - every non-`native-oci-agent/1` profile keeps its existing refusal; there
//!   is no caller boolean, env switch or test seam that makes a runtime
//!   "protected" without the root profile, a positively consumed obligation and
//!   the live measurements above;
//! - a host that cannot expose a required fact (no root, no `/proc`, no Docker
//!   engine, non-Linux) refuses readiness instead of assuming it;
//! - the owner never provisions, deploys, installs pins, restarts a daemon,
//!   reads production credentials or touches provider/model traffic.

#![allow(dead_code)]

use crate::error::{Error, Result};

pub(crate) mod attestation;
pub(crate) mod client;
pub(crate) mod console;
pub(crate) mod docker;
pub(crate) mod frame;
pub(crate) mod installer;
pub(crate) mod keys;
pub(crate) mod measure;
pub(crate) mod measure_mounts;
pub(crate) mod ops;
pub(crate) mod peer;
pub(crate) mod profile;
pub(crate) mod proof;
pub(crate) mod qualified;
pub(crate) mod registry;
pub(crate) mod service;
pub(crate) mod spec;
pub(crate) mod store;
pub(crate) mod strict;
pub(crate) mod types;

pub(crate) use client::{OwnerEvidence, RootOwnerClient};
pub(crate) use ops::RuntimeCaller;
pub(crate) use profile::LocalProfile;
pub(crate) use qualified::{CurrentEligibility, RuntimeStatus};
pub(crate) use store::{CheckpointAck, CheckpointV1};

/// The root-owned deployment class this module implements. Any other class
/// (legacy, hosted, test) refuses before it can observe protected state.
pub(crate) const PROFILE_CLASS: &str = "native-oci-agent/1";

/// Root-owned configuration root. Compile-time constant; no environment
/// variable or request field can redirect it.
pub(crate) const OWNER_ROOT: &str = "/var/lib/cadence/runtime-owner";

/// The root-owned trust profile path.
pub(crate) const PROFILE_PATH: &str = "/var/lib/cadence/runtime-owner/profile.json";

/// The root-owned runtime state directory (authority SQLite + private key).
pub(crate) const STATE_DIR: &str = "/var/lib/cadence/runtime-owner/state";

/// The root-owned runtime path for control/attestation sockets.
pub(crate) const RUN_DIR: &str = "/run/cadence-runtime-owner";

/// The root control socket. `0700` dir, `0600` socket, uid 0.
pub(crate) const CONTROL_SOCKET: &str = "/run/cadence-runtime-owner/control.sock";

/// The supervisor-owned grant listener socket. Fixed, private, under the
/// supervisor account's root-controlled `0770` parent; never a caller path.
pub(crate) const SUPERVISOR_GRANT_SOCKET: &str = "/run/cadence-supervisor/grant.sock";

/// One private owner frame is one strict newline-terminated JSON object.
pub(crate) const MAX_FRAME_BYTES: usize = 32 * 1024;

/// One guest operations request may be up to 1 MiB (r3 F3), and one complete
/// dispatch receipt up to 1 MiB + 8 KiB. Root control frames stay 32 KiB.
pub(crate) const MAX_OPERATION_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_RECEIPT_BYTES: usize = 1024 * 1024 + 8 * 1024;

/// Whole-request deadline (monotonic, absolute) for one owner frame.
pub(crate) const REQUEST_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// Whole-response write deadline.
pub(crate) const RESPONSE_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

/// The largest `/proc/<pid>/exe` this profile will hash.
pub(crate) const MAX_EXE_BYTES: u64 = 128 * 1024 * 1024;

/// JS-safe integer ceiling used by the canonical supervisor challenge.
pub(crate) const MAX_SAFE_INT: u64 = 9_007_199_254_740_991;

pub(crate) fn rejected(message: impl Into<String>) -> Error {
    Error::rejected(message)
}

pub(crate) fn unknown(message: impl Into<String>) -> Error {
    Error::unknown(message)
}

pub(crate) fn internal(message: impl Into<String>) -> Error {
    Error::internal(message)
}

/// True when a root-owned protected profile exists and is structurally valid.
///
/// This is the daemon-side launch gate the root manager uses to refuse every
/// non-native launch path: it performs the same metadata validation as the
/// owner (root-owned, non-writable file and parents) and never consults the
/// environment. An unreadable/invalid profile is *inactive*; a profile that
/// exists but lies outside the root-owned tree is also inactive, so the
/// protected mode is never silently enabled by an attacker-controlled path.
pub fn protected_profile_active() -> bool {
    LocalProfile::load_production().is_ok()
        // A process running as the reserved supervisor uid is protected-only
        // even if the profile file is missing: deleting the profile must not
        // downgrade the deployment to legacy in-process execution.
        || unsafe { libc::geteuid() } == crate::daemon::supervisor_grant::SUPERVISOR_UID
}

/// The live supervisor facts a local grant recipient binds: the running pid,
/// its `/proc` starttime and a fresh per-process 32-hex generation. Produced
/// only from actual kernel/profile facts — never from a caller literal or a
/// JSON/serde value. A different euid or executable refuses.
pub(crate) struct SupervisorFacts {
    pub(crate) pid: u32,
    pub(crate) starttime: u64,
    pub(crate) generation: String,
}

pub(crate) fn local_supervisor_facts(profile: &LocalProfile) -> Result<SupervisorFacts> {
    let euid = unsafe { libc::geteuid() };
    if euid != profile.supervisor().uid {
        return Err(rejected(
            "protected supervisor facts require the pinned supervisor uid",
        ));
    }
    let pid = std::process::id();
    let starttime =
        peer::starttime(pid).ok_or_else(|| unknown("supervisor starttime unreadable"))?;
    let measured = format!("sha256:{}", peer::hex(&peer::exe_digest(pid)?));
    if measured != profile.supervisor().exe_digest {
        return Err(rejected(
            "running executable is not the pinned supervisor binary",
        ));
    }
    let generation = supervisor_generation()?;
    Ok(SupervisorFacts {
        pid,
        starttime,
        generation,
    })
}

/// The supervisor's per-process generation. It is minted once and reused for
/// the life of the process: an existing same PID/starttime cannot change its
/// generation, so every owner exchange and grant recipient binds the same
/// value. A different process gets a different generation.
fn supervisor_generation() -> Result<String> {
    static GENERATION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let mut guard = GENERATION
        .lock()
        .map_err(|_| unknown("supervisor generation mutex is poisoned"))?;
    if let Some(value) = guard.as_ref() {
        return Ok(value.clone());
    }
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|_| unknown("entropy unavailable"))?;
    let value: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    *guard = Some(value.clone());
    Ok(value)
}

/// The only argv the owner ever passes to its pinned installer. The installer
/// reads its bounded capsule from stdin and has no caller-selectable path,
/// command, environment or socket.
pub(crate) const INSTALLER_LAUNCH_ARG: &str = "--owner-launched-install";

/// The `cadence-runtime-owner` binary entry. Exposed through the library so
/// the root-owned bin target is a two-line wrapper (the reviewable owner code
/// stays in this module tree). Exactly one fixed argv selects the installer
/// mode; every other shape stays the service CLI.
pub fn run_owner_cli() -> i32 {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if args.len() == 2 && args[1].as_os_str() == std::ffi::OsStr::new(INSTALLER_LAUNCH_ARG) {
        return installer::run();
    }
    service::run_cli()
}
