//! CAD-1159 owned root constructor. Public bootstrap material cannot elect its
//! own verifier. Image-authority pins are independent immutable build inputs.
//! Child construction is private and happens ONLY after this authentication.
mod capture;
mod channel;
mod child;
mod children;
mod context;
mod custody;
mod dispatcher;
mod helper;
mod helper_trust;
mod layout;
mod lifecycle;
pub(crate) mod private_wire;
mod runtime;
pub(crate) mod runtime_child;
mod runtime_kernel;
mod wire;
pub use helper_trust::{helper_image_trust, HelperImageTrust};
pub(crate) use runtime_kernel::OwnedDaemon;

use super::{fixed_arguments, refused, Deadline, Result};
use crate::daemon::supervisor_grant as grant;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
pub(crate) use children::recipient_entry;
pub(crate) use context::{
    acknowledge_children, binding_json, custody_recheck, enrollment, grant_keys, image,
    install_once, installer, owner_request, proof, receipt_keys, require_self, Proof,
};
pub(crate) use helper::{HelperPhase, HelperStdio, OwnedHelper};
pub(crate) use lifecycle::{
    pi_acquire, pi_consume, pi_current, pi_expires_at_ms, pi_public_keys, runtime_current,
    runtime_proof, RuntimeProof,
};
use serde::{Deserialize, Serialize};
use std::os::fd::AsRawFd;
pub(crate) use wire::{Kind, OwnerResponse};

/// PUBLIC purpose-elected keys only; never a signer or caller-shaped factory.
pub(crate) type PiKeyRecord = (String, String, u64, [u8; 32]);
const DOMAIN: &[u8] = b"cadence.native-image-qualification.v1\0";
const MAX_SAFE: u64 = 9007199254740991;
const MAX_KEY_VERSION: u64 = 2147483647;

/// Root election supplies these PUBLIC pins, never a private signing key.
/// No caller, environment variable, guest file, diagnostic or manifest key can
/// append to this set. Empty means unconfigured, not successful qualification.
struct ImageAuthorityKey {
    kid: &'static str,
    version: u64,
    public_key: [u8; 32],
}
const IMAGE_AUTHORITY_KEYS: &[ImageAuthorityKey] = &[];

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Configure {
    version: u8,
    #[serde(rename = "type")]
    kind: String,
    operation: String,
    barrier_nonce: String,
    expires_at_ms: u64,
    launch: LaunchWire,
    image_attestation: String,
    lineage: LineageWire,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct IdentityWire {
    company: String,
    instance: String,
    backend: String,
    tier: String,
    generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_lane: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RequestWire {
    identity: IdentityWire,
    purpose: String,
    challenge: String,
    image: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LaunchWire {
    request: RequestWire,
    epoch: u64,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct LineageWire {
    pub(crate) reference: String,
    pub(crate) database_epoch: u64,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Header {
    alg: String,
    issuer: String,
    key_version: u64,
    kid: String,
    #[serde(rename = "type")]
    kind: String,
    version: u8,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Artifacts {
    constructor: String,
    client: String,
    carrier: String,
    observer: String,
    supervisor: String,
    helper: String,
    node: String,
    pi_graph: String,
    policy: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct PublicTrust {
    issuer: String,
    kid: String,
    key_version: u64,
    public_key: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Manifest {
    version: u8,
    image: String,
    source: String,
    artifacts: Artifacts,
    not_before_ms: u64,
    expires_at_ms: u64,
    receipt_trust: Vec<PublicTrust>,
    grant_trust: Vec<PublicTrust>,
    /// Distinct purpose election. An earlier receipt/grant ring does not
    /// authorize a runtime lifetime. Absent means runtime remains unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_trust: Option<Vec<PublicTrust>>,
    /// Independent Pi purpose election; no receipt/grant/runtime fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pi_trust: Option<Vec<PublicTrust>>,
}

/// Non-exported, non-Clone production token. Only authenticated bootstrap can
/// create one. It is not a process construction proof or a launch capability.
pub(crate) struct QualifiedBootstrap {
    manifest: Manifest,
    image_attestation: String,
    launch: grant::LaunchBinding,
    lineage: grant::Lineage,
    operation: String,
    barrier_nonce: String,
    expires_at_ms: u64,
    authenticated_at_ms: u64,
}
fn hex(s: &str, count: usize) -> bool {
    s.len() == count
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && s.bytes().any(|b| b != b'0')
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}
fn decode(segment: &str, max: usize) -> Result<Vec<u8>> {
    if segment.is_empty()
        || segment.len() > max * 4 / 3 + 4
        || !segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        return Err(refused());
    }
    let bytes = URL_SAFE_NO_PAD.decode(segment).map_err(|_| refused())?;
    if bytes.len() > max || URL_SAFE_NO_PAD.encode(&bytes) != segment {
        return Err(refused());
    }
    Ok(bytes)
}
fn canonical<T: for<'a> Deserialize<'a> + Serialize>(bytes: &[u8]) -> Result<T> {
    let typed: T = serde_json::from_slice(bytes).map_err(|_| refused())?;
    // serde structs reject duplicate/unknown fields; Value's sorted BTreeMap
    // encoding then demands exact recursively canonical signature bytes.
    let value = serde_json::to_value(&typed).map_err(|_| refused())?;
    if serde_json::to_vec(&value).map_err(|_| refused())? != bytes {
        return Err(refused());
    }
    Ok(typed)
}
fn trust(keys: &[PublicTrust]) -> Result<()> {
    if keys.is_empty() || keys.len() > 8 {
        return Err(refused());
    }
    let mut identities = std::collections::BTreeSet::new();
    for key in keys {
        if key.issuer != "agenticos-native-owner"
            || !identifier(&key.kid)
            || key.key_version == 0
            || key.key_version > MAX_KEY_VERSION
            || !identities.insert((&key.kid, key.key_version))
        {
            return Err(refused());
        }
        let public = decode(&key.public_key, 32)?;
        if public.len() != 32 || public.iter().all(|b| *b == 0) {
            return Err(refused());
        }
    }
    Ok(())
}

/// SAME production guard used before any fork/seal/exec. The clock is a public
/// observation, not an authority source. No diagnostic/PID/embedded-root field
/// exists. Syntax-valid self-signed qualification still fails immutable lookup.
pub(super) fn authenticate_bootstrap(bytes: &[u8], now_ms: u64) -> Result<QualifiedBootstrap> {
    if bytes.is_empty()
        || bytes.len() >= 65536
        || now_ms > MAX_SAFE
        || bytes.iter().any(|b| !b.is_ascii() || b"\n\r\0".contains(b))
    {
        return Err(refused());
    }
    let input: Configure = serde_json::from_slice(bytes).map_err(|_| refused())?;
    if input.version != 1
        || input.kind != "configure"
        || !uuid(&input.operation)
        || !uuid(&input.barrier_nonce)
        || input.expires_at_ms > MAX_SAFE
        || input.expires_at_ms <= now_ms
        || input.expires_at_ms - now_ms > 300000
        || input.image_attestation.len() > 32768
    {
        return Err(refused());
    }
    let launch = grant::parse_launch(&serde_json::to_value(&input.launch).map_err(|_| refused())?)?;
    let lineage =
        grant::parse_lineage(&serde_json::to_value(&input.lineage).map_err(|_| refused())?)?;
    if launch.request.challenge != input.operation {
        return Err(refused());
    }
    let manifest = authenticate_image_attestation(
        &input.image_attestation,
        now_ms,
        Some(&launch.request.image),
        input.expires_at_ms,
    )?;
    Ok(QualifiedBootstrap {
        manifest,
        image_attestation: input.image_attestation,
        launch,
        lineage,
        operation: input.operation,
        barrier_nonce: input.barrier_nonce,
        expires_at_ms: input.expires_at_ms,
        authenticated_at_ms: now_ms,
    })
}

fn authenticate_image_attestation(
    attestation: &str,
    now_ms: u64,
    expected_image: Option<&str>,
    expires_bound: u64,
) -> Result<Manifest> {
    if attestation.len() > 32768 || now_ms > MAX_SAFE {
        return Err(refused());
    }
    let parts: Vec<_> = attestation.split('.').collect();
    if parts.len() != 3 {
        return Err(refused());
    }
    let header: Header = canonical(&decode(parts[0], 256)?)?;
    let manifest: Manifest = canonical(&decode(parts[1], 20000)?)?;
    let signature = decode(parts[2], 64)?;
    if signature.len() != 64
        || header.alg != "Ed25519"
        || header.issuer != "agenticos-native-image-owner"
        || header.kind != "native-image-qualification"
        || header.version != 1
        || !identifier(&header.kid)
        || header.key_version == 0
        || header.key_version > MAX_KEY_VERSION
        || manifest.version != 1
        || expected_image.is_some_and(|image| manifest.image != image)
        || manifest.image.is_empty()
        || manifest.image.len() > 256
        || manifest.expires_at_ms <= now_ms
        || !hex(&manifest.source, 40)
        || manifest.not_before_ms > now_ms
        || manifest.not_before_ms >= manifest.expires_at_ms
        || manifest.expires_at_ms > MAX_SAFE
        || expires_bound > manifest.expires_at_ms
    {
        return Err(refused());
    }
    for digest in [
        &manifest.artifacts.constructor,
        &manifest.artifacts.client,
        &manifest.artifacts.carrier,
        &manifest.artifacts.observer,
        &manifest.artifacts.supervisor,
        &manifest.artifacts.helper,
        &manifest.artifacts.node,
        &manifest.artifacts.pi_graph,
        &manifest.artifacts.policy,
    ] {
        if !hex(digest, 64) {
            return Err(refused());
        }
    }
    trust(&manifest.receipt_trust)?;
    trust(&manifest.grant_trust)?;
    if let Some(keys) = &manifest.runtime_trust {
        trust(keys)?;
    }
    if let Some(keys) = &manifest.pi_trust {
        trust(keys)?;
    }
    let key = IMAGE_AUTHORITY_KEYS
        .iter()
        .find(|k| k.kid == header.kid && k.version == header.key_version)
        .ok_or_else(|| {
            crate::Error::rejected("constructor image authority unconfigured or untrusted")
        })?;
    let mut signed = DOMAIN.to_vec();
    signed.extend_from_slice(parts[0].as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(parts[1].as_bytes());
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key.public_key)
        .verify(&signed, &signature)
        .map_err(|_| refused())?;
    Ok(manifest)
}

/// Fixed production root entry. Missing independently elected image roots
/// refuse before root effects; positive state is constructed, never asserted.
pub(super) fn entry() -> Result<()> {
    fixed_arguments()?;
    let deadline = Deadline::new();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for fd in [stdin.as_raw_fd(), stdout.as_raw_fd(), 2] {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut stat) } != 0
            || stat.st_mode & libc::S_IFMT != libc::S_IFIFO
        {
            return Err(refused());
        }
    }
    let mut wire = channel::Duplex::new(stdin.as_raw_fd(), stdout.as_raw_fd(), deadline)?;
    let frame = wire.receive()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| refused())?
        .as_millis();
    let qualified = authenticate_bootstrap(&frame.0, u64::try_from(now).map_err(|_| refused())?)?;
    runtime::run(qualified, &frame.0, wire, deadline)
}

pub(crate) fn client_entry() -> Result<()> {
    children::client_entry()
}

pub(crate) type ReceiptKeyRecord = (String, String, u64, [u8; 32]);
impl QualifiedBootstrap {
    pub(crate) fn receipt_records(&self) -> Result<Vec<ReceiptKeyRecord>> {
        self.manifest
            .receipt_trust
            .iter()
            .map(|k| {
                Ok((
                    k.issuer.clone(),
                    k.kid.clone(),
                    k.key_version,
                    decode(&k.public_key, 32)?
                        .try_into()
                        .map_err(|_| refused())?,
                ))
            })
            .collect()
    }
    fn grant_public_keys(&self) -> Result<Vec<[u8; 32]>> {
        self.manifest
            .grant_trust
            .iter()
            .map(|k| decode(&k.public_key, 32)?.try_into().map_err(|_| refused()))
            .collect()
    }
}

fn pin(value: &str) -> Result<[u8; 32]> {
    if !hex(value, 64) {
        return Err(refused());
    }
    let mut out = [0; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).map_err(|_| refused())?;
    }
    Ok(out)
}

#[cfg(test)]
mod guard;
