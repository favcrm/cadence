//! The per-runtime attestation channel and the pinned Pi 1.4.0 observer.
//!
//! The attestation socket is *separate* from the root control socket and from
//! the operations socket. Only the actual enrolled Node process — its PID and
//! live starttime measured by the root owner from the container's init — may
//! connect. On connect the owner re-measures the peer identity, requires the
//! actual kernel non-dumpable `/proc/<pid>/environ` proof (the post-exec seal),
//! and only then sends a fresh random 256-bit observation key and challenge;
//! the key never appears in argv, environment, config files or any model-facing
//! API. Every observation is HMAC-SHA256 authenticated over
//! fixed ASCII material, so a same-uid tool child, a guest-supplied PID or a
//! copied receipt cannot forge readiness.
//!
//! After readiness the channel is watched: a changed registry digest, a forged
//! frame, or the channel closing fences the runtime (registered mutation
//! invalidates the loaded receipt).

use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

use super::frame;
use super::peer;
use super::profile::LocalProfile;
use super::registry;
use super::store::OwnerStore;
use super::{MAX_FRAME_BYTES, RESPONSE_BUDGET};

/// The owner's expectation for the observer that may connect.
#[derive(Clone, Debug)]
pub(crate) struct AttestationExpectation {
    pub(crate) runtime_id: String,
    pub(crate) generation: u64,
    pub(crate) os_uid: u32,
    /// Host-namespace PID of the guest's PID 1 (the measured Node process).
    pub(crate) node_pid: u32,
    pub(crate) node_starttime: u64,
    pub(crate) key: [u8; 32],
    pub(crate) challenge_hex: String,
    /// The argv/env digests the owner measured from `/proc` at start; the
    /// observer's claim must agree with the kernel measurement.
    pub(crate) argv_digest: String,
    pub(crate) env_digest: String,
}

/// One verified registry observation.
#[derive(Debug)]
pub(crate) struct VerifiedObservation {
    /// Full dynamic payload digest (HMAC/loaded-receipt/runtime evidence).
    pub(crate) digest: String,
    /// Stable approved-registry projection digest (static profile pin).
    pub(crate) stable_digest: String,
    pub(crate) counter: u64,
    pub(crate) payload: serde_json::Value,
    /// Non-forgeable proof produced by `registry::verify_payload`; consumed by
    /// `OwnerStore::mark_ready`.
    pub(crate) verified: registry::VerifiedRegistry,
}

/// Replace the prepared placeholder with the owner's listening attestation
/// socket, owned by the enrolled runtime uid and reachable only inside the
/// guest's bind mount.
pub(crate) fn bind_attestation(
    path: &Path,
    os_uid: u32,
    os_gid: u32,
) -> Result<std::os::unix::net::UnixListener> {
    super::spec::take_placeholder(path)?;
    let listener = std::os::unix::net::UnixListener::bind(path)
        .map_err(|_| Error::internal("attestation socket cannot be bound"))?;
    super::spec::seal_socket_path(path, os_uid, os_gid)?;
    Ok(listener)
}

/// Wait until the listener has a pending connection or the deadline elapses.
fn wait_readable(fd: libc::c_int, deadline: Instant) -> Result<()> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(Error::unknown("attestation deadline elapsed"));
        }
        let remaining = deadline - now;
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc == 0 {
            return Err(Error::unknown("attestation deadline elapsed"));
        }
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(Error::unknown("attestation wait failed"));
        }
        if pfd.revents & libc::POLLIN != 0 {
            return Ok(());
        }
    }
}

/// Accept the measured Node observer, issue the fresh key/challenge, and
/// verify the first registry observation. Blocks for at most `budget`.
pub(crate) fn collect_first(
    listener: &std::os::unix::net::UnixListener,
    expectation: &AttestationExpectation,
    profile: &LocalProfile,
    budget: Duration,
) -> Result<(std::os::unix::net::UnixStream, VerifiedObservation)> {
    let deadline = Instant::now() + budget;
    listener
        .set_nonblocking(true)
        .map_err(|_| Error::internal("attestation listener cannot be nonblocking"))?;
    wait_readable(listener.as_raw_fd(), deadline)?;
    let (stream, _) = listener
        .accept()
        .map_err(|_| Error::unknown("attestation accept failed"))?;
    stream
        .set_nonblocking(false)
        .map_err(|_| Error::internal("attestation connection cannot be blocking"))?;
    let (uid, pid) = peer::credentials(&stream)?;
    if uid != expectation.os_uid {
        return Err(Error::rejected(
            "attestation peer is not the enrolled runtime uid",
        ));
    }
    if pid != expectation.node_pid {
        return Err(Error::rejected(
            "attestation peer is not the measured guest init",
        ));
    }
    let start = peer::starttime(pid)
        .ok_or_else(|| Error::rejected("attestation peer starttime unreadable"))?;
    if start != expectation.node_starttime {
        return Err(Error::rejected(
            "attestation peer is not the measured Node process",
        ));
    }
    // Root-side kernel proof that this exact process has already applied the
    // post-exec seal. It must hold BEFORE the private HMAC observation key is
    // written: payload seal booleans alone are a guest claim, not kernel proof.
    super::measure::verify_kernel_seal(pid)?;
    let issue = serde_json::json!({
        "v": 1,
        "kind": "issue",
        "runtime_generation": expectation.generation,
        "challenge": expectation.challenge_hex,
        "key": b64(&expectation.key),
        "attestation_target": super::spec::ATTEST_TARGET,
    });
    let issue = serde_json::to_vec(&issue)
        .map_err(|_| Error::internal("attestation issue frame cannot be encoded"))?;
    frame::write_frame(&stream, &issue, RESPONSE_BUDGET)?;

    // Bounded number of frames while we wait for the first valid registry.
    if (0..16).next().is_some() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let body = frame::read_frame(&stream, remaining, MAX_FRAME_BYTES)?;
        let value = super::strict::parse(&body)?;
        let m = super::strict::object(&value, &["v", "payload", "digest", "mac"])?;
        if super::strict::safe_u64(m, "v")? != 1 {
            return Err(Error::rejected("attestation frame v must be exactly 1"));
        }
        let payload = m
            .get("payload")
            .ok_or_else(|| Error::rejected("attestation payload missing"))?;
        let claimed = super::strict::string(m, "digest")?;
        super::strict::digest(claimed)?;
        let mac = decode_b64(super::strict::string(m, "mac")?)?;
        if mac.len() != 32 {
            return Err(Error::rejected("attestation MAC is not 32 bytes"));
        }
        let counter = super::strict::safe_u64(
            payload
                .as_object()
                .ok_or_else(|| Error::rejected("attestation payload is not an object"))?,
            "counter",
        )?;
        if counter == 0 {
            return Err(Error::rejected("attestation counter must be positive"));
        }
        // The MAC covers the counter and the claimed digest, both of which we
        // then compare to freshly derived values.
        let material = registry::mac_material(
            expectation.generation,
            &expectation.challenge_hex,
            counter,
            claimed,
        );
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &expectation.key);
        ring::hmac::verify(&key, &material, &mac)
            .map_err(|_| Error::rejected("attestation MAC does not verify"))?;
        // `counter` is transmitted inside the payload; remove it from the
        // canonical registry body so it is not double-counted.
        let mut registry_payload = payload.clone();
        if let Some(obj) = registry_payload.as_object_mut() {
            obj.remove("counter");
        }
        let computed =
            registry::verify_payload(&registry_payload, profile, expectation.generation)?;
        if computed.dynamic_digest() != claimed {
            return Err(Error::rejected(
                "attestation digest does not match the recomputed registry",
            ));
        }
        let digest = computed.dynamic_digest().to_string();
        let stable_digest = computed.stable_digest().to_string();
        let obj = registry_payload
            .as_object()
            .ok_or_else(|| Error::rejected("attestation payload is not an object"))?;
        if super::strict::string(obj, "argv_digest")? != expectation.argv_digest
            || super::strict::string(obj, "env_digest")? != expectation.env_digest
        {
            return Err(Error::rejected(
                "observer argv/env claim disagrees with the measured process",
            ));
        }
        return Ok((
            stream,
            VerifiedObservation {
                digest,
                stable_digest,
                counter,
                payload: registry_payload,
                verified: computed,
            },
        ));
    }
    Err(Error::unknown("no verified observation arrived"))
}

/// Keep watching the attested channel after readiness. Any changed registry
/// digest, failed MAC, or channel close fences the runtime: a mutation of the
/// active registry invalidates the loaded receipt.
pub(crate) fn watch(
    stream: std::os::unix::net::UnixStream,
    expectation: AttestationExpectation,
    approved_digest: String,
    store: Arc<OwnerStore>,
    runtime_id: String,
) {
    let mut last_counter: u64 = 0;
    loop {
        let body = match frame::read_frame(&stream, Duration::from_secs(3600), MAX_FRAME_BYTES) {
            Ok(body) => body,
            Err(_) => {
                let _ = store.fence(
                    &runtime_id,
                    store_generation(&store, &runtime_id),
                    "observer channel closed",
                );
                return;
            }
        };
        let checked = (|| -> Result<(u64, String)> {
            let value = super::strict::parse(&body)?;
            let m = super::strict::object(&value, &["v", "payload", "digest", "mac"])?;
            if super::strict::safe_u64(m, "v")? != 1 {
                return Err(Error::rejected("attestation frame v must be exactly 1"));
            }
            let payload = m
                .get("payload")
                .ok_or_else(|| Error::rejected("attestation payload missing"))?;
            let claimed = super::strict::string(m, "digest")?;
            super::strict::digest(claimed)?;
            let counter = payload
                .as_object()
                .ok_or_else(|| Error::rejected("attestation payload is not an object"))?
                .get("counter")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| Error::rejected("attestation counter missing"))?;
            let mac = decode_b64(super::strict::string(m, "mac")?)?;
            if mac.len() != 32 {
                return Err(Error::rejected("attestation MAC is not 32 bytes"));
            }
            let material = registry::mac_material(
                expectation.generation,
                &expectation.challenge_hex,
                counter,
                claimed,
            );
            ring::hmac::verify(
                &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &expectation.key),
                &material,
                &mac,
            )
            .map_err(|_| Error::rejected("attestation MAC does not verify"))?;
            Ok((counter, claimed.to_string()))
        })();
        match checked {
            Ok((counter, digest)) => {
                if counter <= last_counter || digest != approved_digest {
                    let _ = store.fence(
                        &runtime_id,
                        store_generation(&store, &runtime_id),
                        "observed registry changed or replayed",
                    );
                    return;
                }
                last_counter = counter;
            }
            Err(_) => {
                let _ = store.fence(
                    &runtime_id,
                    store_generation(&store, &runtime_id),
                    "attestation frame invalid",
                );
                return;
            }
        }
    }
}

fn store_generation(store: &OwnerStore, runtime_id: &str) -> u64 {
    store.get(runtime_id).map(|o| o.generation).unwrap_or(0)
}

fn b64(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode_b64(s: &str) -> Result<Vec<u8>> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    if s.len() > 64 {
        return Err(Error::rejected("attestation base64 value is too long"));
    }
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| Error::rejected("attestation base64 value is malformed"))
}

/// Fresh random 32 bytes from the kernel CSPRNG.
pub(crate) fn random32() -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    getrandom::fill(&mut out).map_err(|_| Error::unknown("entropy unavailable"))?;
    Ok(out)
}

pub(crate) fn hex32(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}
