//! R5 privileged peer bootstrap proofs (exact revision r5, lines 113–123).
//!
//! The root owner performs held-FD `/proc/<pid>/exe` measurements; the uid-21000
//! supervisor never needs CAP_SYS_PTRACE. Three narrowly typed,
//! domain-separated Ed25519 documents exist, with the exact bounded field sets:
//!
//! - `OwnerPeerProofV1`     (`cadence.runtime-owner.peer.v1`)          from `hello_v1`
//! - `ChallengePeerProofV1` (`cadence.runtime-owner.challenge-peer.v1`) from `measure_challenge_installer_v1`
//! - `InstallerPeerProofV1` (`cadence.runtime-owner.installer.v1`)      from `measure_installer_v1`
//!
//! There is no sign(bytes), caller-asserted digest or arbitrary process query.
//! Only measured server/owned-installer facts are signed. `ChallengePeerIdentity`
//! is only usable by `GrantCore::challenge`; it conveys no install/consume/
//! start/custody authority.

use crate::error::{Error, Result};

use super::keys::{b64, OwnerSigner};
use super::peer;
use super::profile::LocalProfile;
use super::registry;
use super::strict;

pub(crate) const PEER_DOMAIN: &[u8] = b"cadence.runtime-owner.peer.v1\0";
pub(crate) const CHALLENGE_PEER_DOMAIN: &[u8] = b"cadence.runtime-owner.challenge-peer.v1\0";
pub(crate) const INSTALLER_DOMAIN: &[u8] = b"cadence.runtime-owner.installer.v1\0";
pub(crate) const LAUNCH_REQUEST_DOMAIN: &[u8] = b"cadence.runtime-owner.launch-request.v1\0";

pub(crate) const PEER_FIELDS: &[&str] = &[
    "v",
    "request_nonce",
    "workspace_id",
    "owner_epoch",
    "owner_pid",
    "owner_starttime",
    "owner_exe_digest",
    "profile_digest",
    "supervisor_pid",
    "supervisor_starttime",
    "supervisor_exe_digest",
    "supervisor_generation",
    "key_id",
    "signature",
];

pub(crate) const CHALLENGE_PEER_FIELDS: &[&str] = &[
    "v",
    "request_nonce",
    "workspace_id",
    "owner_epoch",
    "allocation_id",
    "launch_request_digest",
    "supervisor_pid",
    "supervisor_starttime",
    "supervisor_generation",
    "installer_pid",
    "installer_starttime",
    "installer_exe_digest",
    "installer_generation",
    "purpose",
    "key_id",
    "signature",
];

pub(crate) const INSTALLER_FIELDS: &[&str] = &[
    "v",
    "request_nonce",
    "workspace_id",
    "owner_epoch",
    "challenge_digest",
    "supervisor_pid",
    "supervisor_starttime",
    "installer_pid",
    "installer_starttime",
    "installer_exe_digest",
    "installer_generation",
    "key_id",
    "signature",
];

/// The owner's own measured identity (held `/proc/self/exe` fd).
pub(crate) struct OwnerSelf {
    pub(crate) pid: u32,
    pub(crate) starttime: u64,
    pub(crate) exe_digest: String,
}

pub(crate) fn measure_owner_self(profile: &LocalProfile) -> Result<OwnerSelf> {
    let pid = std::process::id();
    if pid == 0 {
        return Err(Error::rejected("owner pid is unavailable"));
    }
    let starttime =
        peer::starttime(pid).ok_or_else(|| Error::unknown("owner starttime unreadable"))?;
    let exe_digest = format!("sha256:{}", peer::hex(&peer::exe_digest(pid)?));
    if exe_digest != profile.owner_exe_digest() {
        return Err(Error::rejected(
            "running owner executable is not the pinned owner binary",
        ));
    }
    Ok(OwnerSelf {
        pid,
        starttime,
        exe_digest,
    })
}

pub(crate) fn owner_key_id(profile: &LocalProfile) -> String {
    let digest = super::profile::sha256_digest(profile.owner_public_key());
    format!("owner-{}", &digest["sha256:".len()..][..16])
}

pub(crate) fn validate_nonce(nonce: &str) -> Result<()> {
    if !strict::is_lower_hex(nonce, 64) {
        return Err(Error::rejected("request_nonce must be 64 lowercase hex"));
    }
    Ok(())
}

pub(crate) fn validate_generation(generation: &str) -> Result<()> {
    if !strict::is_lower_hex(generation, 32) {
        return Err(Error::rejected("generation must be 32 lowercase hex"));
    }
    Ok(())
}

/// The exact canonical LaunchRequest the owner commits in `prepare_runtime`.
/// The supervisor parses these bounded bytes, recomputes the digest and compares
/// it with the challenge-peer proof before the first challenge handshake.
pub(crate) fn canonical_launch_request_value(
    profile: &LocalProfile,
    instance: &str,
    generation: u64,
    challenge_id: &str,
) -> serde_json::Value {
    let mut identity = serde_json::json!({
        "company": profile.workspace_id(),
        "instance": instance,
        "backend": "native",
        "tier": "basic",
        "generation": generation,
    });
    identity["imageLane"] = serde_json::Value::String("protected-local-1".to_string());
    serde_json::json!({
        "identity": identity,
        "purpose": "fresh_start",
        "challenge": challenge_id,
        "image": profile.image().reference,
    })
}

pub(crate) fn launch_request_bytes(
    profile: &LocalProfile,
    instance: &str,
    generation: u64,
    challenge_id: &str,
) -> Result<Vec<u8>> {
    registry::canonical(&canonical_launch_request_value(
        profile,
        instance,
        generation,
        challenge_id,
    ))
}

pub(crate) fn launch_request_digest(bytes: &[u8]) -> String {
    let mut material = Vec::with_capacity(LAUNCH_REQUEST_DOMAIN.len() + bytes.len());
    material.extend_from_slice(LAUNCH_REQUEST_DOMAIN);
    material.extend_from_slice(bytes);
    super::profile::sha256_digest(&material)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn sign_peer_proof(
    signer: &OwnerSigner,
    profile: &LocalProfile,
    owner_epoch: u64,
    request_nonce: &str,
    owner: &OwnerSelf,
    supervisor: &peer::Peer,
    supervisor_generation: &str,
) -> Result<serde_json::Value> {
    validate_nonce(request_nonce)?;
    validate_generation(supervisor_generation)?;
    let mut document = serde_json::json!({
        "v": 1,
        "request_nonce": request_nonce,
        "workspace_id": profile.workspace_id(),
        "owner_epoch": owner_epoch,
        "owner_pid": owner.pid,
        "owner_starttime": owner.starttime,
        "owner_exe_digest": owner.exe_digest,
        "profile_digest": profile.digest(),
        "supervisor_pid": supervisor.pid,
        "supervisor_starttime": supervisor.starttime,
        "supervisor_exe_digest": format!("sha256:{}", peer::hex(&supervisor.exe_digest)),
        "supervisor_generation": supervisor_generation,
        "key_id": owner_key_id(profile),
    });
    sign_in_place(signer, PEER_DOMAIN, &mut document)?;
    Ok(document)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn sign_challenge_peer_proof(
    signer: &OwnerSigner,
    profile: &LocalProfile,
    owner_epoch: u64,
    request_nonce: &str,
    allocation_id: &str,
    launch_request_digest: &str,
    supervisor: &peer::Peer,
    supervisor_generation: &str,
    installer_pid: u32,
    installer_starttime: u64,
    installer_generation: &str,
) -> Result<serde_json::Value> {
    validate_nonce(request_nonce)?;
    validate_generation(supervisor_generation)?;
    validate_generation(installer_generation)?;
    strict::digest(launch_request_digest)?;
    let measured = installer_measurement(profile, installer_pid, installer_starttime)?;
    let mut document = serde_json::json!({
        "v": 1,
        "request_nonce": request_nonce,
        "workspace_id": profile.workspace_id(),
        "owner_epoch": owner_epoch,
        "allocation_id": allocation_id,
        "launch_request_digest": launch_request_digest,
        "supervisor_pid": supervisor.pid,
        "supervisor_starttime": supervisor.starttime,
        "supervisor_generation": supervisor_generation,
        "installer_pid": installer_pid,
        "installer_starttime": installer_starttime,
        "installer_exe_digest": measured,
        "installer_generation": installer_generation,
        "purpose": "challenge_only",
        "key_id": owner_key_id(profile),
    });
    sign_in_place(signer, CHALLENGE_PEER_DOMAIN, &mut document)?;
    Ok(document)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn sign_installer_proof(
    signer: &OwnerSigner,
    profile: &LocalProfile,
    owner_epoch: u64,
    request_nonce: &str,
    challenge_digest: &str,
    supervisor: &peer::Peer,
    installer_pid: u32,
    installer_starttime: u64,
    installer_generation: &str,
) -> Result<serde_json::Value> {
    validate_nonce(request_nonce)?;
    validate_generation(installer_generation)?;
    strict::digest(challenge_digest)?;
    let measured = installer_measurement(profile, installer_pid, installer_starttime)?;
    let mut document = serde_json::json!({
        "v": 1,
        "request_nonce": request_nonce,
        "workspace_id": profile.workspace_id(),
        "owner_epoch": owner_epoch,
        "challenge_digest": challenge_digest,
        "supervisor_pid": supervisor.pid,
        "supervisor_starttime": supervisor.starttime,
        "installer_pid": installer_pid,
        "installer_starttime": installer_starttime,
        "installer_exe_digest": measured,
        "installer_generation": installer_generation,
        "key_id": owner_key_id(profile),
    });
    sign_in_place(signer, INSTALLER_DOMAIN, &mut document)?;
    Ok(document)
}

fn sign_in_place(
    signer: &OwnerSigner,
    domain: &[u8],
    document: &mut serde_json::Value,
) -> Result<()> {
    let canonical = registry::canonical(document)?;
    let mut material = Vec::with_capacity(domain.len() + canonical.len());
    material.extend_from_slice(domain);
    material.extend_from_slice(&canonical);
    let signature = signer.sign_domain(&material);
    document["signature"] = serde_json::Value::String(b64(&signature));
    Ok(())
}

/// Held-FD measurement of the owner-launched/enrolled installer: root uid,
/// stable live starttime, pinned executable digest. Refuses any other process.
pub(crate) fn installer_measurement(
    profile: &LocalProfile,
    pid: u32,
    expected_starttime: u64,
) -> Result<String> {
    if pid == 0 {
        return Err(Error::rejected("installer pid is invalid"));
    }
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|_| Error::rejected("installer status is unreadable"))?;
    let uid = status
        .lines()
        .find(|l| l.starts_with("Uid:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| Error::rejected("installer uid is unreadable"))?;
    if uid != 0 {
        return Err(Error::rejected("installer process is not root"));
    }
    let before =
        peer::starttime(pid).ok_or_else(|| Error::rejected("installer starttime is unreadable"))?;
    if before != expected_starttime {
        return Err(Error::rejected(
            "installer starttime does not match the kernel peer",
        ));
    }
    let measured = format!("sha256:{}", peer::hex(&peer::exe_digest(pid)?));
    if measured != profile.installer().exe_digest {
        return Err(Error::rejected(
            "installer executable is not the pinned installer",
        ));
    }
    let after = peer::starttime(pid)
        .ok_or_else(|| Error::rejected("installer exited during measurement"))?;
    if after != before {
        return Err(Error::rejected(
            "installer pid changed under measurement — pid reuse refused",
        ));
    }
    Ok(measured)
}

/// Test-only: sign one exact proof document under a fixed proof domain. Used
/// to exercise verification refusals without a live measured process. Not a
/// production signing path.
#[cfg(test)]
pub(crate) fn test_sign_document(
    signer: &OwnerSigner,
    domain: &[u8],
    mut document: serde_json::Value,
) -> Result<serde_json::Value> {
    sign_in_place(signer, domain, &mut document)?;
    Ok(document)
}

/// Verify one proof against the pinned owner public key and its exact field set.
/// Returns the unsigned document; callers must still bind every field to their
/// own kernel peer/nonce/generation/epoch/config before trusting it.
pub(crate) fn verify_proof(
    profile: &LocalProfile,
    domain: &[u8],
    fields: &[&str],
    document: &serde_json::Value,
) -> Result<serde_json::Value> {
    let m = strict::object(document, fields)?;
    let signature = m
        .get("signature")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::rejected("proof signature missing"))?;
    let signature = super::keys::decode_b64(signature)?;
    let mut unsigned = document.clone();
    if let Some(obj) = unsigned.as_object_mut() {
        obj.remove("signature");
    }
    let canonical = registry::canonical(&unsigned)?;
    let mut material = Vec::with_capacity(domain.len() + canonical.len());
    material.extend_from_slice(domain);
    material.extend_from_slice(&canonical);
    super::keys::verify_with_public_key(profile.owner_public_key(), &material, &signature)?;
    Ok(unsigned)
}
