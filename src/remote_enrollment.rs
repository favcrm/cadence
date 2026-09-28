//! Issuer-verified hosted enrollment. A selected outbox destination is never authority.
//!
//! One private directory owns one short-lived child. A service enrollment can
//! explicitly renew; a browser enrollment requires fresh owner consent.
//! Server-side revocation is enforced by the hosted gateway at receipt, because
//! AOS-75 exposes no local revocation introspection endpoint.
use crate::remote_result_outbox::DestinationPin;
use crate::{Error, Result};
use base64::Engine;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VERSION: &str = "hosted-cadence-auth.v1";
const MAX_RESPONSE: u64 = 64 * 1024;
const MAX_SERVICE_TOKEN: usize = 128;
const RECORD: &str = "enrollment.json";
const TRUSTED_ISSUER: &str = "trusted-issuer";
const DEVICE_VERSION: &str = "hosted-cadence-device.v1";
const CONTINUITY_VERSION: &str = "hosted-cadence-continuity.v1";
const CONTINUITY_RECORD: &str = "continuity.json";

#[derive(Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EnrollmentSource {
    #[default]
    Service,
    Browser,
}
#[derive(Clone, Copy)]
enum ChildSource<'a> {
    Service(&'a str),
    Browser,
}
fn is_service_source(source: &EnrollmentSource) -> bool {
    *source == EnrollmentSource::Service
}

fn reject(message: &str) -> Error {
    Error::rejected(message)
}
fn now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| reject("Invalid system clock"))?
        .as_secs())
}
fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn origin(value: &str, allow_test_http: bool) -> Result<String> {
    let uri: ureq::http::Uri = value.parse().map_err(|_| reject("Invalid origin"))?;
    let authority = uri
        .authority()
        .ok_or_else(|| reject("Origin must have a host"))?;
    let https = uri.scheme_str() == Some("https");
    let loopback = allow_test_http
        && uri.scheme_str() == Some("http")
        && matches!(uri.host(), Some("127.0.0.1") | Some("localhost"));
    if (!https && !loopback)
        || authority.as_str().contains('@')
        || value != format!("{}://{authority}", uri.scheme_str().unwrap_or(""))
        || uri.host().is_none_or(|h| h.is_empty() || h.contains('*'))
        || value.contains(['#', '?'])
    {
        return Err(reject("Expected an exact HTTPS origin"));
    }
    Ok(value.to_owned())
}
fn token(value: &str, prefix: &str) -> bool {
    value.len() <= MAX_SERVICE_TOKEN
        && value.strip_prefix(prefix).is_some_and(|suffix| {
            suffix.len() == 43
                && suffix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        })
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Enrollment {
    version: String,
    issuer: String,
    organization_id: String,
    audience: String,
    subject_id: String,
    bridge_id: String,
    agent_id: String,
    client_agent_id: String,
    credential_id: String,
    role: String,
    capabilities: Vec<String>,
    expires_at: u64,
    child_token: String,
    #[serde(default, skip_serializing_if = "is_service_source")]
    source: EnrollmentSource,
    #[serde(default)]
    service_token: Option<String>,
}
impl std::fmt::Debug for Enrollment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Enrollment")
            .field("issuer", &self.issuer)
            .field("organization_id", &self.organization_id)
            .field("audience", &self.audience)
            .field("subject_id", &self.subject_id)
            .field("agent_id", &self.agent_id)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug)]
pub struct EnrollmentInfo {
    organization_id: String,
    audience: String,
    subject_id: String,
    agent_id: String,
    expires_at: u64,
}
impl EnrollmentInfo {
    pub fn organization_id(&self) -> &str {
        &self.organization_id
    }
    pub fn audience(&self) -> &str {
        &self.audience
    }
    pub fn subject_id(&self) -> &str {
        &self.subject_id
    }
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }
}
impl Enrollment {
    fn info(&self) -> EnrollmentInfo {
        EnrollmentInfo {
            organization_id: self.organization_id.clone(),
            audience: self.audience.clone(),
            subject_id: self.subject_id.clone(),
            agent_id: self.agent_id.clone(),
            expires_at: self.expires_at,
        }
    }

    fn valid(&self, at: u64) -> Result<()> {
        if self.version != VERSION
            || origin(&self.issuer, cfg!(test)).is_err()
            || origin(&self.audience, cfg!(test)).is_err()
            || ![
                &self.organization_id,
                &self.subject_id,
                &self.bridge_id,
                &self.agent_id,
                &self.client_agent_id,
                &self.credential_id,
            ]
            .into_iter()
            .all(|value| id(value))
            || self.role != "implementer"
            || self.capabilities.len() != 1
            || self.capabilities[0] != "results.submit"
            || self.agent_id == self.subject_id
            || self.agent_id == self.bridge_id
            || self.credential_id == self.bridge_id
            || !token(&self.child_token, "hct_")
            || match self.source {
                EnrollmentSource::Service => self
                    .service_token
                    .as_deref()
                    .is_none_or(|credential| !token(credential, "hcs_")),
                EnrollmentSource::Browser => self.service_token.is_some(),
            }
            || self.expires_at <= at
        {
            return Err(reject("Hosted enrollment invalid or expired; re-enroll"));
        }
        Ok(())
    }
    /// The callback runs only when issuer-bound identity exactly matches the immutable pin.
    /// A caller-controlled org, board or agent string cannot substitute for this check.
    fn with_pin<T>(&self, pin: &DestinationPin, send: impl FnOnce(&str) -> T) -> Result<T> {
        self.valid(now()?)?;
        if pin.organization_id() != self.organization_id
            || pin.audience() != self.audience
            || pin.subject_id() != self.subject_id
            || pin.agent_id() != self.agent_id
        {
            return Err(reject(
                "Hosted result destination differs from issuer enrollment",
            ));
        }
        Ok(send(&self.child_token))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ContinuityState {
    Pending,
    Bound,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContinuityRecord {
    version: String,
    issuer: String,
    organization_id: String,
    audience: String,
    subject_id: String,
    bridge_id: String,
    agent_id: String,
    credential_id: String,
    seed: String,
    state: ContinuityState,
    lineage_id: Option<String>,
    key_id: Option<String>,
    generation: Option<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealedContinuity {
    record: ContinuityRecord,
    checksum: String,
}

#[derive(Clone, Debug)]
pub struct ContinuityInfo {
    lineage_id: String,
    organization_id: String,
    audience: String,
    subject_id: String,
    agent_id: String,
    generation: u64,
}
impl ContinuityInfo {
    pub fn lineage_id(&self) -> &str {
        &self.lineage_id
    }
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }
    pub fn organization_id(&self) -> &str {
        &self.organization_id
    }
    pub fn audience(&self) -> &str {
        &self.audience
    }
    pub fn subject_id(&self) -> &str {
        &self.subject_id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl ContinuityRecord {
    fn for_child(enrollment: &Enrollment, seed: String) -> Self {
        Self {
            version: CONTINUITY_VERSION.into(),
            issuer: enrollment.issuer.clone(),
            organization_id: enrollment.organization_id.clone(),
            audience: enrollment.audience.clone(),
            subject_id: enrollment.subject_id.clone(),
            bridge_id: enrollment.bridge_id.clone(),
            agent_id: enrollment.agent_id.clone(),
            credential_id: enrollment.credential_id.clone(),
            seed,
            state: ContinuityState::Pending,
            lineage_id: None,
            key_id: None,
            generation: None,
        }
    }

    fn valid_for(&self, enrollment: &Enrollment) -> Result<Ed25519KeyPair> {
        if self.version != CONTINUITY_VERSION
            || enrollment.source != EnrollmentSource::Browser
            || self.issuer != enrollment.issuer
            || self.organization_id != enrollment.organization_id
            || self.audience != enrollment.audience
            || self.subject_id != enrollment.subject_id
            || self.bridge_id != enrollment.bridge_id
            || self.agent_id != enrollment.agent_id
            || self.credential_id != enrollment.credential_id
            || !continuity_b64(&self.seed)
            || match self.state {
                ContinuityState::Pending => {
                    self.lineage_id.is_some() || self.key_id.is_some() || self.generation.is_some()
                }
                ContinuityState::Bound => {
                    self.lineage_id
                        .as_deref()
                        .is_none_or(|value| !continuity_lineage_id(value))
                        || self
                            .key_id
                            .as_deref()
                            .is_none_or(|value| !continuity_minted(value, "key_"))
                        || self.generation != Some(1)
                }
            }
        {
            return Err(reject(
                "Hosted continuity record does not match browser enrollment",
            ));
        }
        let seed = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&self.seed)
            .map_err(|_| reject("Invalid hosted continuity key"))?;
        Ed25519KeyPair::from_seed_unchecked(&seed)
            .map_err(|_| reject("Invalid hosted continuity key"))
    }

    fn info(&self) -> Result<ContinuityInfo> {
        if self.state != ContinuityState::Bound {
            return Err(reject("Hosted continuity bind is pending or uncertain"));
        }
        Ok(ContinuityInfo {
            lineage_id: self
                .lineage_id
                .clone()
                .ok_or_else(|| reject("Invalid lineage"))?,
            organization_id: self.organization_id.clone(),
            audience: self.audience.clone(),
            subject_id: self.subject_id.clone(),
            agent_id: self.agent_id.clone(),
            generation: self
                .generation
                .ok_or_else(|| reject("Invalid generation"))?,
        })
    }
}

fn continuity_b64(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}
fn continuity_minted(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(continuity_b64)
}
fn continuity_lineage_id(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("hcl_") else {
        return false;
    };
    let bytes = suffix.as_bytes();
    bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && bytes[14] == b'4'
        && b"89ab".contains(&bytes[19])
        && bytes.iter().enumerate().all(|(index, byte)| {
            matches!(index, 8 | 13 | 18 | 23)
                || byte.is_ascii_digit()
                || (b'a'..=b'f').contains(byte)
        })
}
fn continuity_checksum(record: &ContinuityRecord) -> Result<String> {
    let bytes =
        serde_json::to_vec(record).map_err(|_| reject("Invalid hosted continuity record"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn read_continuity_locked(dir: &Path) -> Result<ContinuityRecord> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(CONTINUITY_RECORD))?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(reject(
            "Hosted continuity record must be a private owned file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RESPONSE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err(reject("Hosted continuity record too large"));
    }
    let sealed: SealedContinuity =
        serde_json::from_slice(&bytes).map_err(|_| reject("Invalid hosted continuity record"))?;
    if sealed.checksum != continuity_checksum(&sealed.record)? {
        return Err(reject("Hosted continuity record checksum mismatch"));
    }
    Ok(sealed.record)
}
fn save_continuity_locked(dir: &Path, record: &ContinuityRecord) -> Result<()> {
    let path = dir.join(CONTINUITY_RECORD);
    match fs::symlink_metadata(&path) {
        Ok(meta)
            if !meta.is_file()
                || meta.uid() != unsafe { libc::geteuid() }
                || meta.mode() & 0o077 != 0 =>
        {
            return Err(reject("Hosted continuity path is not a private owned file"));
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    let sealed = SealedContinuity {
        record: record.clone(),
        checksum: continuity_checksum(record)?,
    };
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temp.write_all(&serde_json::to_vec(&sealed).map_err(|_| reject("Invalid continuity"))?)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|_| reject("Unable to save hosted continuity"))?;
    File::open(dir)?.sync_all()?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContinuityChallenge {
    version: String,
    challenge_id: String,
    operation_id: String,
    nonce: String,
    registry_epoch: String,
    issued_at_ms: u64,
    expires_at_ms: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContinuityBindReply {
    version: String,
    lineage_id: String,
    key_id: String,
    generation: u64,
    delivery: String,
    organization_id: String,
    audience: String,
    subject_id: String,
    bridge_id: String,
    agent_id: String,
    credential_kind: String,
}

/// Bind a browser child while its short-lived bearer and original owner consent
/// are still live. A private pending seed is durable before either HTTP request;
/// a lost bind response never manufactures a bound lineage.
pub fn bind_browser(dir: &Path) -> Result<ContinuityInfo> {
    private_dir(dir, false)?;
    let _guard = lock(dir, true)?;
    let enrollment = read_locked(dir)?;
    enrollment.valid(now()?)?;
    require_trusted_issuer(dir, &enrollment.issuer)?;
    if enrollment.source != EnrollmentSource::Browser {
        return Err(reject(
            "Only browser enrollment supports browser continuity",
        ));
    }
    let mut continuity = match fs::symlink_metadata(dir.join(CONTINUITY_RECORD)) {
        Ok(_) => {
            let stored = read_continuity_locked(dir)?;
            stored.valid_for(&enrollment)?;
            if stored.state == ContinuityState::Bound {
                return stored.info();
            }
            stored
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut bytes = [0_u8; 32];
            getrandom::fill(&mut bytes).map_err(|_| reject("Unable to create continuity key"))?;
            let seed = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
            let pending = ContinuityRecord::for_child(&enrollment, seed);
            save_continuity_locked(dir, &pending)?;
            pending
        }
        Err(error) => return Err(error.into()),
    };
    let key = continuity.valid_for(&enrollment)?;
    let public = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let challenge: ContinuityChallenge = serde_json::from_value(post(
        &enrollment.issuer,
        "/v1/hosted-cadence/continuity/challenge",
        &enrollment.child_token,
        json!({"version":CONTINUITY_VERSION}),
    )?)
    .map_err(|_| reject("Invalid hosted continuity challenge"))?;
    let at = now()?
        .checked_mul(1000)
        .ok_or_else(|| reject("Invalid system clock"))?;
    if challenge.version != CONTINUITY_VERSION
        || !continuity_minted(&challenge.challenge_id, "ch_")
        || !continuity_minted(&challenge.operation_id, "op_")
        || !continuity_b64(&challenge.nonce)
        || !continuity_b64(&challenge.registry_epoch)
        || challenge.issued_at_ms > at.saturating_add(2_000)
        || challenge.expires_at_ms <= at
        || challenge.expires_at_ms <= challenge.issued_at_ms
        || challenge.expires_at_ms - challenge.issued_at_ms > 30_000
    {
        return Err(reject("Invalid or expired hosted continuity challenge"));
    }
    let proof = format!(
        "{{\"version\":\"{CONTINUITY_VERSION}\",\"action\":\"bind_key\",\"challengeId\":\"{}\",\"nonce\":\"{}\",\"registryEpoch\":\"{}\",\"publicKey\":\"{public}\"}}",
        challenge.challenge_id, challenge.nonce, challenge.registry_epoch,
    );
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(key.sign(proof.as_bytes()).as_ref());
    let reply: ContinuityBindReply = serde_json::from_value(
        post(
            &enrollment.issuer,
            "/v1/hosted-cadence/continuity/bind",
            &enrollment.child_token,
            json!({"version":CONTINUITY_VERSION,"challengeId":challenge.challenge_id,
            "publicKey":public,"signature":signature}),
        )
        .map_err(|_| reject("Hosted continuity bind uncertain; retry with the same pending key"))?,
    )
    .map_err(|_| reject("Invalid hosted continuity bind response"))?;
    if reply.version != CONTINUITY_VERSION
        || !continuity_lineage_id(&reply.lineage_id)
        || !continuity_minted(&reply.key_id, "key_")
        || reply.generation != 1
        || (reply.delivery != "bound" && reply.delivery != "metadata")
        || reply.organization_id != enrollment.organization_id
        || reply.audience != enrollment.audience
        || reply.subject_id != enrollment.subject_id
        || reply.bridge_id != enrollment.bridge_id
        || reply.agent_id != enrollment.agent_id
        || reply.credential_kind != "child"
    {
        return Err(reject("Issuer continuity bind did not match browser child"));
    }
    require_trusted_issuer(dir, &enrollment.issuer)?;
    enrollment.valid(now()?)?;
    continuity.state = ContinuityState::Bound;
    continuity.lineage_id = Some(reply.lineage_id);
    continuity.key_id = Some(reply.key_id);
    continuity.generation = Some(reply.generation);
    save_continuity_locked(dir, &continuity).map_err(|_| {
        reject("Hosted continuity bind may be committed remotely; local save uncertain")
    })?;
    continuity.info()
}

/// Metadata only; the local key never leaves the protected record.
pub fn bound_browser(dir: &Path) -> Result<ContinuityInfo> {
    private_dir(dir, false)?;
    let _guard = lock(dir, false)?;
    let enrollment = read_locked(dir)?;
    enrollment.valid(0)?;
    require_trusted_issuer(dir, &enrollment.issuer)?;
    let continuity = read_continuity_locked(dir)?;
    continuity.valid_for(&enrollment)?;
    continuity.info()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sealed {
    record: Enrollment,
    checksum: String,
}
fn checksum(record: &Enrollment) -> Result<String> {
    let bytes = serde_json::to_vec(record).map_err(|_| reject("Invalid hosted enrollment"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn private_dir(dir: &Path, create: bool) -> Result<()> {
    if !dir.is_absolute() {
        return Err(reject("Enrollment directory must be absolute"));
    }
    if create && !dir.exists() {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(reject("Enrollment directory must be owned and mode 0700"));
    }
    Ok(())
}
/// The operator must establish this independent trust pin before a service
/// credential is ever transmitted. Enrollment never creates it.
fn require_trusted_issuer(dir: &Path, issuer: &str) -> Result<()> {
    private_dir(dir, false)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(TRUSTED_ISSUER))
        .map_err(|_| reject("Trusted issuer pin is missing"))?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(reject("Trusted issuer pin must be a private owned file"));
    }
    let mut bytes = Vec::new();
    file.take(2049).read_to_end(&mut bytes)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| reject("Invalid trusted issuer pin"))?;
    let pinned = text.strip_suffix('\n').unwrap_or(text);
    if pinned.len() > 2048 || origin(pinned, cfg!(test))?.as_str() != issuer {
        return Err(reject("Issuer differs from operator trust pin"));
    }
    Ok(())
}
fn lock(dir: &Path, exclusive: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("enrollment.lock"))?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(reject("Enrollment lock must be a private owned file"));
    }
    let kind = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    if unsafe { libc::flock(file.as_raw_fd(), kind) } != 0 {
        return Err(reject("Unable to lock enrollment"));
    }
    Ok(file)
}
fn read_locked(dir: &Path) -> Result<Enrollment> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(RECORD))?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(reject("Enrollment record must be a private owned file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RESPONSE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err(reject("Enrollment record too large"));
    }
    let sealed: Sealed =
        serde_json::from_slice(&bytes).map_err(|_| reject("Invalid enrollment record"))?;
    if checksum(&sealed.record)? != sealed.checksum {
        return Err(reject("Enrollment record checksum mismatch"));
    }
    Ok(sealed.record)
}
pub fn with_current<T>(
    dir: &Path,
    pin: &DestinationPin,
    send: impl FnOnce(&str) -> Result<T>,
) -> Result<T> {
    private_dir(dir, false)?;
    let _guard = lock(dir, false)?;
    let record = read_locked(dir)?;
    require_trusted_issuer(dir, &record.issuer)?;
    record.valid(now()?)?;
    record.with_pin(pin, send)?
}
pub fn current(dir: &Path) -> Result<EnrollmentInfo> {
    private_dir(dir, false)?;
    let _guard = lock(dir, false)?;
    let record = read_locked(dir)?;
    require_trusted_issuer(dir, &record.issuer)?;
    record.valid(now()?)?;
    Ok(record.info())
}
/// Explicit renewal uses the previously stored service credential. The issuer
/// re-verifies its current owner, organization, audience, scope and revocation.
pub fn renew(dir: &Path) -> Result<EnrollmentInfo> {
    private_dir(dir, false)?;
    let _guard = lock(dir, true)?;
    let prior = read_locked(dir)?;
    if prior.source == EnrollmentSource::Browser {
        return Err(reject(
            "Browser enrollment cannot renew; run remote enrollment browser again",
        ));
    }
    let service_token = prior
        .service_token
        .as_deref()
        .ok_or_else(|| reject("Missing service credential"))?;
    enroll_locked(
        &prior.issuer,
        &prior.organization_id,
        &prior.audience,
        &prior.client_agent_id,
        service_token,
        dir,
    )
}
pub fn remove(dir: &Path) -> Result<()> {
    private_dir(dir, false)?;
    let _guard = lock(dir, true)?;
    let path = dir.join(RECORD);
    let _ = read_locked(dir)?;
    let continuity = dir.join(CONTINUITY_RECORD);
    match fs::symlink_metadata(&continuity) {
        Ok(meta) => {
            if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
                return Err(reject("Hosted continuity path is not a private owned file"));
            }
            fs::remove_file(continuity)?;
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    fs::remove_file(path)?;
    File::open(dir)?.sync_all()?;
    Ok(())
}
fn save_locked(dir: &Path, record: &Enrollment) -> Result<()> {
    if dir.join(RECORD).exists() {
        let prior = read_locked(dir)?;
        if prior.issuer != record.issuer
            || prior.organization_id != record.organization_id
            || prior.audience != record.audience
            || prior.client_agent_id != record.client_agent_id
        {
            return Err(reject(
                "Enrollment destination changed; remove the old record explicitly",
            ));
        }
    }
    let sealed = Sealed {
        record: record.clone(),
        checksum: checksum(record)?,
    };
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temp.write_all(&serde_json::to_vec(&sealed).map_err(|_| reject("Invalid enrollment"))?)?;
    temp.as_file().sync_all()?;
    temp.persist(dir.join(RECORD))
        .map_err(|_| reject("Unable to save enrollment"))?;
    File::open(dir)?.sync_all()?;
    Ok(())
}
#[cfg(test)]
fn save(dir: &Path, record: &Enrollment) -> Result<()> {
    private_dir(dir, true)?;
    let _guard = lock(dir, true)?;
    save_locked(dir, record)
}

fn post(issuer: &str, path: &str, bearer: &str, body: Value) -> Result<Value> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .build();
    let response = ureq::Agent::new_with_config(config)
        .post(format!("{issuer}{path}"))
        .header("Authorization", format!("Bearer {bearer}"))
        .send_json(body)
        .map_err(|_| reject("Issuer enrollment request failed"))?;
    if response.status() != 200 {
        return Err(reject("Issuer refused hosted enrollment"));
    }
    let mut body = response.into_body();
    let mut bytes = Vec::new();
    body.as_reader()
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err(reject("Issuer response too large"));
    }
    serde_json::from_slice(&bytes).map_err(|_| reject("Invalid issuer response"))
}

fn post_public(issuer: &str, path: &str, body: Value) -> Result<(u16, Value)> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .build();
    let response = ureq::Agent::new_with_config(config)
        .post(format!("{issuer}{path}"))
        .send_json(body)
        .map_err(|_| reject("Hosted browser request failed"))?;
    let status = response.status().as_u16();
    if (300..400).contains(&status) {
        return Err(reject("Hosted issuer redirect refused"));
    }
    let mut bytes = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err(reject("Hosted issuer response too large"));
    }
    let value =
        serde_json::from_slice(&bytes).map_err(|_| reject("Invalid hosted browser response"))?;
    Ok((status, value))
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| reject("Incomplete issuer enrollment"))
}
fn timestamp(value: &Value, key: &str) -> Result<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| reject("Incomplete issuer enrollment"))
}

fn device_code(value: &Value) -> Result<(&str, &str, &str, u64)> {
    let device = field(value, "device_code")?;
    let user = field(value, "user_code")?;
    let verification = field(value, "verification_uri")?;
    let complete = field(value, "verification_uri_complete")?;
    let code_chars = user.bytes().enumerate().all(|(i, b)| {
        if i == 4 {
            b == b'-'
        } else {
            b.is_ascii_uppercase() || (b'2'..=b'9').contains(&b)
        }
    });
    let verification_uri: ureq::http::Uri = verification
        .parse()
        .map_err(|_| reject("Invalid hosted browser URL"))?;
    if field(value, "version")? != DEVICE_VERSION
        || !token(device, "hcd_")
        || user.len() != 9
        || !code_chars
        || verification_uri.scheme_str() != Some("https")
        || verification_uri
            .authority()
            .is_none_or(|a| a.as_str().contains('@'))
        || verification_uri.host().is_none_or(str::is_empty)
        || verification.contains(['\n', '\r', '#', '?'])
        || complete != format!("{verification}?code={user}")
        || timestamp(value, "expires_in")? == 0
        || timestamp(value, "expires_in")? > 600
        || timestamp(value, "interval")? < 5
        || timestamp(value, "interval")? > 30
    {
        return Err(reject("Invalid hosted device grant"));
    }
    Ok((device, user, complete, timestamp(value, "interval")?))
}

fn browser_grant(value: &Value, org: &str, audience: &str) -> Result<()> {
    let at = now()?;
    let principal = &value["principal"];
    let credential = &value["credential"];
    let caps = value["capabilities"]
        .as_array()
        .ok_or_else(|| reject("Invalid hosted browser capabilities"))?;
    if field(value, "version")? != VERSION
        || field(value, "organization_id")? != org
        || field(value, "audience")? != audience
        || field(principal, "kind")? != "user"
        || field(principal, "current_role")? != "owner"
        || !id(field(principal, "subject_id")?)
        || caps.len() != 2
        || !caps.contains(&json!("bridge.enroll"))
        || !caps.contains(&json!("results.submit"))
        || field(credential, "token_type")? != "Bearer"
        || field(credential, "renewal")? != "reexchange"
        || !token(field(credential, "access_token")?, "hct_")
        || !id(field(credential, "credential_id")?)
        || timestamp(credential, "issued_at")? > at
        || timestamp(credential, "expires_at")? <= at
        || timestamp(credential, "expires_at")? - timestamp(credential, "issued_at")? > 300
    {
        return Err(reject("Issuer grant did not match hosted browser consent"));
    }
    Ok(())
}

/// Complete one AOS-76 owner consent. The shared lock orders an in-flight
/// exchange and child save before remove; no device code or verifier is stored.
pub fn enroll_browser(
    issuer: &str,
    org: &str,
    audience: &str,
    client_agent: &str,
    dir: &Path,
    show_code: impl FnOnce(&str, &str) -> Result<()>,
) -> Result<EnrollmentInfo> {
    let issuer = origin(issuer, cfg!(test))?;
    let audience = origin(audience, cfg!(test))?;
    if !id(org) || !id(client_agent) {
        return Err(reject("Invalid hosted browser enrollment request"));
    }
    private_dir(dir, false)?;
    let _guard = lock(dir, true)?;
    require_trusted_issuer(dir, &issuer)?;
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).map_err(|_| reject("Unable to create PKCE verifier"))?;
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let (status, code) = post_public(
        &issuer,
        "/v1/hosted-cadence/device/code",
        json!({"version":DEVICE_VERSION,"organization_id":org,"audience":audience,
            "client_label":"Cadence local team","requested_capabilities":["bridge.enroll","results.submit"],
            "code_challenge":challenge}),
    )?;
    if status != 200 {
        return Err(reject("Hosted issuer refused device authorization"));
    }
    let (device, user, verification, mut interval) = device_code(&code)?;
    show_code(verification, user)?;
    let deadline = Instant::now() + Duration::from_secs(timestamp(&code, "expires_in")?);
    let grant = loop {
        if Instant::now() >= deadline {
            return Err(reject("Hosted browser authorization expired; start again"));
        }
        std::thread::sleep(Duration::from_secs(interval));
        if Instant::now() >= deadline {
            return Err(reject("Hosted browser authorization expired; start again"));
        }
        require_trusted_issuer(dir, &issuer)?;
        let (status, response) = post_public(
            &issuer,
            "/v1/hosted-cadence/device/token",
            json!({"device_code":device,"code_verifier":verifier}),
        )?;
        if Instant::now() >= deadline {
            return Err(reject("Hosted browser authorization expired; start again"));
        }
        if status == 200 {
            break response;
        }
        match (status, response["error"].as_str()) {
            (400, Some("authorization_pending")) => {}
            (400, Some("slow_down")) => interval = interval.saturating_add(5).min(30),
            (400, Some("access_denied")) => return Err(reject("Hosted browser consent denied")),
            (400, Some("expired_token")) => return Err(reject("Hosted device code expired")),
            _ => return Err(reject("Hosted browser grant refused; start again")),
        }
    };
    browser_grant(&grant, org, &audience)?;
    require_trusted_issuer(dir, &issuer)?;
    enroll_child_locked(
        &issuer,
        org,
        &audience,
        client_agent,
        dir,
        &grant,
        ChildSource::Browser,
    )
}

/// Bootstrap only against an explicitly trusted issuer origin. The `hcs_` service
/// token is read by the CLI from protected stdin, never from an argv or outbox row.
pub fn enroll(
    issuer: &str,
    org: &str,
    audience: &str,
    client_agent: &str,
    service_token: &str,
    dir: &Path,
) -> Result<EnrollmentInfo> {
    let issuer = origin(issuer, cfg!(test))?;
    let audience = origin(audience, cfg!(test))?;
    if !id(org) || !id(client_agent) || !token(service_token, "hcs_") {
        return Err(reject("Invalid hosted enrollment request"));
    }
    private_dir(dir, false)?;
    let _guard = lock(dir, true)?;
    enroll_locked(&issuer, org, &audience, client_agent, service_token, dir)
}

fn enroll_locked(
    issuer: &str,
    org: &str,
    audience: &str,
    client_agent: &str,
    service_token: &str,
    dir: &Path,
) -> Result<EnrollmentInfo> {
    require_trusted_issuer(dir, issuer)?;
    let requested = ["bridge.enroll", "results.submit"];
    let exchange = post(
        issuer,
        "/v1/hosted-cadence/service/exchange",
        service_token,
        json!({"version":VERSION,"organization_id":org,"audience":audience,
            "requested_capabilities":requested}),
    )?;
    let at = now()?;
    let principal = &exchange["principal"];
    let bridge = &exchange["credential"];
    if field(&exchange, "version")? != VERSION
        || field(&exchange, "organization_id")? != org
        || field(&exchange, "audience")? != audience
        || field(principal, "kind")? != "service"
        || field(principal, "current_role")? != "member"
        || !id(field(principal, "subject_id")?)
        || !id(field(principal, "provisioned_by")?)
        || exchange["capabilities"] != json!(requested)
        || field(bridge, "token_type")? != "Bearer"
        || field(bridge, "renewal")? != "reexchange"
        || !token(field(bridge, "access_token")?, "hct_")
        || !id(field(bridge, "credential_id")?)
        || timestamp(bridge, "issued_at")? > at
        || timestamp(bridge, "expires_at")? <= at
        || timestamp(bridge, "expires_at")? - timestamp(bridge, "issued_at")? > 300
    {
        return Err(reject("Issuer grant did not match service credential"));
    }
    enroll_child_locked(
        issuer,
        org,
        audience,
        client_agent,
        dir,
        &exchange,
        ChildSource::Service(service_token),
    )
}

fn enroll_child_locked(
    issuer: &str,
    org: &str,
    audience: &str,
    client_agent: &str,
    dir: &Path,
    grant: &Value,
    source: ChildSource<'_>,
) -> Result<EnrollmentInfo> {
    let bridge = &grant["credential"];
    let path = match source {
        ChildSource::Service(_) => "/v1/hosted-cadence/service/enroll",
        ChildSource::Browser => "/v1/hosted-cadence/enroll",
    };
    let enrollment = post(
        issuer,
        path,
        field(bridge, "access_token")?,
        json!({"version":VERSION,"organization_id":org,"audience":audience,
            "client_label":"Cadence local team", "agents":[{
                "client_agent_id":client_agent,"role":"implementer",
                "requested_capabilities":["results.submit"]}]}),
    )?;
    let enrolled_at = now()?;
    let agents = enrollment["agents"]
        .as_array()
        .ok_or_else(|| reject("Invalid issuer enrollment"))?;
    if agents.len() != 1 {
        return Err(reject("Invalid issuer enrollment"));
    }
    let child = &agents[0];
    let child_credential = &child["credential"];
    let record = Enrollment {
        version: field(&enrollment, "version")?.into(),
        issuer: issuer.to_owned(),
        organization_id: field(&enrollment, "organization_id")?.into(),
        audience: field(&enrollment, "audience")?.into(),
        subject_id: field(child, "principal_subject_id")?.into(),
        bridge_id: field(&enrollment, "bridge_id")?.into(),
        agent_id: field(child, "agent_id")?.into(),
        client_agent_id: field(child, "client_agent_id")?.into(),
        credential_id: field(child_credential, "credential_id")?.into(),
        role: field(child, "role")?.into(),
        capabilities: serde_json::from_value(child["capabilities"].clone())
            .map_err(|_| reject("Invalid child capabilities"))?,
        expires_at: timestamp(child_credential, "expires_at")?,
        child_token: field(child_credential, "access_token")?.into(),
        source: match source {
            ChildSource::Service(_) => EnrollmentSource::Service,
            ChildSource::Browser => EnrollmentSource::Browser,
        },
        service_token: match source {
            ChildSource::Service(credential) => Some(credential.to_owned()),
            ChildSource::Browser => None,
        },
    };
    if record.organization_id != org
        || record.audience != audience
        || record.subject_id != field(&grant["principal"], "subject_id")?
        || record.client_agent_id != client_agent
        || field(child, "organization_id")? != org
        || field(child, "audience")? != audience
        || field(child, "bridge_id")? != record.bridge_id
        || field(child_credential, "token_type")? != "Bearer"
        || field(child_credential, "renewal")? != "reexchange"
        || timestamp(child_credential, "issued_at")? > enrolled_at
        || record.expires_at <= enrolled_at
        || record.expires_at - timestamp(child_credential, "issued_at")? > 300
        || record.expires_at > timestamp(bridge, "expires_at")?
        || record.credential_id == field(bridge, "credential_id")?
        || record.child_token == field(bridge, "access_token")?
    {
        return Err(reject("Issuer child did not match bridge grant"));
    }
    record.valid(now()?)?;
    require_trusted_issuer(dir, &record.issuer)?;
    save_locked(dir, &record)?;
    Ok(record.info())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_result_outbox::{deliver_with, ResultCommand, ResultOutbox};
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;

    const SERVICE: &str = "hcs_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const BRIDGE: &str = "hct_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
    const CHILD: &str = "hct_CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";
    const LINEAGE: &str = "hcl_00000000-0000-4000-8000-000000000001";
    fn record(expires: u64) -> Enrollment {
        Enrollment {
            version: VERSION.into(),
            issuer: "https://issuer.example.test".into(),
            organization_id: "ws_real".into(),
            audience: "https://real.board.example.test".into(),
            subject_id: "hsp_subject".into(),
            bridge_id: "hcb_bridge".into(),
            agent_id: "hca_agent".into(),
            client_agent_id: "worker".into(),
            credential_id: "hcc_credential".into(),
            role: "implementer".into(),
            capabilities: vec!["results.submit".into()],
            expires_at: expires,
            child_token: CHILD.into(),
            source: EnrollmentSource::Service,
            service_token: Some(SERVICE.into()),
        }
    }
    fn trust(dir: &Path, issuer: &str) {
        fs::create_dir(dir).unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(dir.join(TRUSTED_ISSUER), format!("{issuer}\n")).unwrap();
        fs::set_permissions(dir.join(TRUSTED_ISSUER), fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn pin(board: &str) -> DestinationPin {
        DestinationPin::new("ws_real", board, "hsp_subject", "hca_agent").unwrap()
    }

    #[test]
    fn issuer_binding_refuses_forged_org_board_subject_agent_and_expiry_before_callback() {
        let valid = record(u64::MAX);
        let mut calls = 0;
        for forged in [
            DestinationPin::new(
                "ws_other",
                &valid.audience,
                &valid.subject_id,
                &valid.agent_id,
            )
            .unwrap(),
            pin("https://attacker.board.example.test"),
            DestinationPin::new("ws_real", &valid.audience, "hsp_forged", &valid.agent_id).unwrap(),
            DestinationPin::new("ws_real", &valid.audience, &valid.subject_id, "hca_forged")
                .unwrap(),
        ] {
            assert!(valid.with_pin(&forged, |_| calls += 1).is_err());
        }
        assert!(record(1)
            .with_pin(&pin(&valid.audience), |_| calls += 1)
            .is_err());
        assert_eq!(calls, 0);
        valid
            .with_pin(&pin(&valid.audience), |_| calls += 1)
            .unwrap();
        assert_eq!(calls, 1);
    }

    #[test]
    fn enrolled_sender_uses_original_pin_and_never_posts_to_attacker_board() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("enroll");
        let enrolled = record(u64::MAX);
        trust(&dir, &enrolled.issuer);
        save(&dir, &enrolled).unwrap();
        let outbox = ResultOutbox::open(&root.path().join("outbox")).unwrap();
        let command = ResultCommand::parse_json(
            &json!({"version":"hosted-cadence-result.v1","commandId":"command-1",
                "kind":"agent_result","assignmentId":"assignment-1","taskId":"task-1",
                "taskRevision":1,"turnId":"turn-1","reportedHeadSha":"a".repeat(40),
                "text":"private result"})
            .to_string(),
        )
        .unwrap();
        let genuine = pin(&enrolled.audience);
        outbox.enqueue(&genuine, &command).unwrap();
        let row = outbox.get("command-1").unwrap();
        let original = row.receipt().destination();
        let queued = with_current(&dir, original, |child| {
            deliver_with(
                &outbox,
                "command-1",
                original.organization_id(),
                original.audience(),
                child,
                |url, bearer, body| {
                    assert_eq!(
                        url,
                        "https://real.board.example.test/__platform/hosted-cadence/ws_real/results"
                    );
                    assert_eq!(bearer, CHILD);
                    assert_eq!(body, command.canonical_json());
                    Ok((
                        202,
                        json!({"ok":true,"receipt":{"commandId":"command-1",
                        "state":"queued","acceptedAt":100,"expiresAt":200,
                        "digest":command.digest()}})
                        .to_string()
                        .into_bytes(),
                    ))
                },
            )
        })
        .unwrap();
        assert_eq!(queued.state(), "remote_queued");

        let mut calls = 0;
        for forged in [
            pin("https://attacker.board.example.test"),
            DestinationPin::new("ws_other", &enrolled.audience, "hsp_subject", "hca_agent")
                .unwrap(),
            DestinationPin::new("ws_real", &enrolled.audience, "hsp_subject", "hca_other").unwrap(),
        ] {
            assert!(with_current(&dir, &forged, |_| {
                calls += 1;
                Ok(())
            })
            .is_err());
        }
        assert_eq!(calls, 0);
    }

    #[test]
    fn private_record_detects_tamper_symlink_world_readability_and_survives_restart() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("enroll");
        let original = record(u64::MAX);
        trust(&dir, &original.issuer);
        save(&dir, &original).unwrap();
        assert!(!fs::read_to_string(dir.join(RECORD))
            .unwrap()
            .contains("\"source\""));
        assert_eq!(current(&dir).unwrap().agent_id(), original.agent_id);
        assert!(with_current(&dir, &pin(&original.audience), |token| Ok(token == CHILD)).unwrap());
        let file = dir.join(RECORD);
        let mut value: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        value["record"]["audience"] = json!("https://attacker.board.example.test");
        fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(current(&dir).is_err());
        let mut forged = original.clone();
        forged.issuer = "https://attacker.example.test".into();
        let sealed = Sealed {
            checksum: checksum(&forged).unwrap(),
            record: forged,
        };
        fs::write(&file, serde_json::to_vec(&sealed).unwrap()).unwrap();
        let mut calls = 0;
        assert!(current(&dir).is_err());
        assert!(with_current(&dir, &pin(&original.audience), |_| {
            calls += 1;
            Ok(())
        })
        .is_err());
        assert_eq!(calls, 0);
        let fresh = root.path().join("fresh");
        trust(&fresh, &original.issuer);
        save(&fresh, &original).unwrap();
        fs::set_permissions(fresh.join(RECORD), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(current(&fresh).is_err());
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&fresh, &link).unwrap();
        assert!(current(&link).is_err());
    }

    #[test]
    fn renewal_cannot_replace_child_while_send_holds_enrollment_lock() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("enroll");
        let original = record(u64::MAX);
        trust(&dir, &original.issuer);
        save(&dir, &original).unwrap();
        let expected = pin(&original.audience);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let send_dir = dir.clone();
        let sending = thread::spawn(move || {
            with_current(&send_dir, &expected, |child| {
                assert_eq!(child, CHILD);
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .unwrap();
        });
        entered_rx.recv().unwrap();
        let (saved_tx, saved_rx) = std::sync::mpsc::channel();
        let replace_dir = dir.clone();
        let replacing = thread::spawn(move || {
            let mut replacement = original;
            replacement.child_token = format!("hct_{}", "D".repeat(43));
            save(&replace_dir, &replacement).unwrap();
            saved_tx.send(()).unwrap();
        });
        assert!(saved_rx.recv_timeout(Duration::from_millis(40)).is_err());
        release_tx.send(()).unwrap();
        sending.join().unwrap();
        replacing.join().unwrap();
        saved_rx.recv().unwrap();
    }

    #[test]
    fn removal_waits_for_inflight_send_and_blocks_every_later_send() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("enroll");
        let original = record(u64::MAX);
        trust(&dir, &original.issuer);
        save(&dir, &original).unwrap();
        let expected = pin(&original.audience);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let send_dir = dir.clone();
        let sending = thread::spawn(move || {
            with_current(&send_dir, &expected, |child| {
                assert_eq!(child, CHILD);
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .unwrap();
        });
        entered_rx.recv().unwrap();
        let (removed_tx, removed_rx) = std::sync::mpsc::channel();
        let remove_dir = dir.clone();
        let removing = thread::spawn(move || {
            remove(&remove_dir).unwrap();
            removed_tx.send(()).unwrap();
        });
        assert!(removed_rx.recv_timeout(Duration::from_millis(40)).is_err());
        release_tx.send(()).unwrap();
        sending.join().unwrap();
        removing.join().unwrap();
        removed_rx.recv().unwrap();
        assert!(
            with_current(&dir, &pin(&original.audience), |_| -> Result<()> {
                panic!("send callback ran after removal")
            })
            .is_err()
        );
    }

    fn respond(socket: &mut std::net::TcpStream, body: &Value) {
        let bytes = serde_json::to_vec(body).unwrap();
        write!(socket, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", bytes.len()).unwrap();
        socket.write_all(&bytes).unwrap();
    }
    fn respond_error(socket: &mut std::net::TcpStream, error: &str) {
        let bytes = json!({"error":error}).to_string();
        write!(socket, "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", bytes.len()).unwrap();
        socket.write_all(bytes.as_bytes()).unwrap();
    }
    fn browser_code(expires_in: u64) -> Value {
        json!({"version":DEVICE_VERSION,
            "device_code":format!("hcd_{}", "A".repeat(43)),"user_code":"K7PM-2QNF",
            "verification_uri":"https://app.agenticos.test/device/hosted-cadence",
            "verification_uri_complete":"https://app.agenticos.test/device/hosted-cadence?code=K7PM-2QNF",
            "expires_in":expires_in,"interval":5})
    }
    fn request(listener: &TcpListener, path: &str, bearer: &str) -> std::net::TcpStream {
        request_with_audience(listener, path, bearer, "http://127.0.0.1:1")
    }
    fn request_with_audience(
        listener: &TcpListener,
        path: &str,
        bearer: &str,
        expected_audience: &str,
    ) -> std::net::TcpStream {
        let (socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), format!("POST {path} HTTP/1.1"));
        let mut authorization = String::new();
        let mut length = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            let (key, value) = line.split_once(':').unwrap();
            if key.eq_ignore_ascii_case("authorization") {
                authorization = value.trim().into();
            }
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
        assert_eq!(authorization, format!("Bearer {bearer}"));
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["organization_id"], "ws_real");
        assert_eq!(body["audience"], expected_audience);
        socket
    }
    fn public_request(listener: &TcpListener, path: &str) -> (std::net::TcpStream, Value) {
        let (socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), format!("POST {path} HTTP/1.1"));
        let mut length = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            let (key, value) = line.split_once(':').unwrap();
            assert!(!key.eq_ignore_ascii_case("authorization"));
            assert!(!key.eq_ignore_ascii_case("cookie"));
            assert!(!key.eq_ignore_ascii_case("origin"));
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        (socket, serde_json::from_slice(&body).unwrap())
    }

    fn continuity_request(listener: &TcpListener, path: &str) -> (std::net::TcpStream, Value) {
        let (socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), format!("POST {path} HTTP/1.1"));
        let mut length = 0;
        let mut bearer = String::new();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            let (key, value) = line.split_once(':').unwrap();
            if key.eq_ignore_ascii_case("authorization") {
                bearer = value.trim().into();
            }
            assert!(!key.eq_ignore_ascii_case("cookie"));
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
        assert_eq!(bearer, format!("Bearer {CHILD}"));
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        (socket, serde_json::from_slice(&body).unwrap())
    }

    #[test]
    fn browser_continuity_bind_proves_key_and_retains_exact_private_lineage() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let at = now().unwrap();
        let server = thread::spawn(move || {
            let (mut challenge, request) =
                continuity_request(&listener, "/v1/hosted-cadence/continuity/challenge");
            assert_eq!(request, json!({"version":"hosted-cadence-continuity.v1"}));
            respond(
                &mut challenge,
                &json!({
                    "version":"hosted-cadence-continuity.v1",
                    "challengeId":format!("ch_{}", "A".repeat(43)),
                    "operationId":format!("op_{}", "B".repeat(43)),
                    "nonce":"C".repeat(43), "registryEpoch":"D".repeat(43),
                    "issuedAtMs":at*1000,"expiresAtMs":at*1000+30_000
                }),
            );
            let (mut bind, request) =
                continuity_request(&listener, "/v1/hosted-cadence/continuity/bind");
            assert_eq!(request["version"], "hosted-cadence-continuity.v1");
            assert_eq!(request["challengeId"], format!("ch_{}", "A".repeat(43)));
            let public = request["publicKey"].as_str().unwrap();
            let signature = request["signature"].as_str().unwrap();
            let proof = format!(
                "{{\"version\":\"hosted-cadence-continuity.v1\",\"action\":\"bind_key\",\"challengeId\":\"ch_{}\",\"nonce\":\"{}\",\"registryEpoch\":\"{}\",\"publicKey\":\"{}\"}}",
                "A".repeat(43), "C".repeat(43), "D".repeat(43), public
            );
            ring::signature::UnparsedPublicKey::new(
                &ring::signature::ED25519,
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(public)
                    .unwrap(),
            )
            .verify(
                proof.as_bytes(),
                &base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(signature)
                    .unwrap(),
            )
            .unwrap();
            respond(
                &mut bind,
                &json!({
                    "version":"hosted-cadence-continuity.v1", "delivery":"bound",
                    "lineageId":LINEAGE, "keyId":format!("key_{}", "E".repeat(43)),
                    "generation":1, "organizationId":"ws_real",
                    "audience":"https://real.board.example.test", "subjectId":"hsp_subject",
                    "bridgeId":"hcb_bridge", "agentId":"hca_agent", "credentialKind":"child"
                }),
            );
        });
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("enroll");
        trust(&dir, &issuer);
        let mut child = record(at + 120);
        child.issuer = issuer;
        child.source = EnrollmentSource::Browser;
        child.service_token = None;
        save(&dir, &child).unwrap();
        let bound = bind_browser(&dir).unwrap();
        server.join().unwrap();
        assert_eq!(bound.lineage_id(), LINEAGE);
        assert_eq!(bound.agent_id(), child.agent_id);
        assert_eq!(bound.generation(), 1);
        assert_eq!(bound_browser(&dir).unwrap().lineage_id(), LINEAGE);
        let file = dir.join("continuity.json");
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(!fs::read_to_string(file).unwrap().contains(CHILD));
        let original = fs::read(dir.join(CONTINUITY_RECORD)).unwrap();
        let mut tampered: Value = serde_json::from_slice(&original).unwrap();
        tampered["record"]["agent_id"] = json!("hca_attacker");
        fs::write(dir.join(CONTINUITY_RECORD), tampered.to_string()).unwrap();
        assert!(bound_browser(&dir).is_err());
        fs::write(dir.join(CONTINUITY_RECORD), &original).unwrap();
        fs::set_permissions(
            dir.join(CONTINUITY_RECORD),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(bound_browser(&dir).is_err());
        fs::set_permissions(
            dir.join(CONTINUITY_RECORD),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        remove(&dir).unwrap();
        assert!(!dir.join(CONTINUITY_RECORD).exists());
        assert!(bound_browser(&dir).is_err());
    }

    #[test]
    fn browser_continuity_bind_rejects_forged_identity_and_lineage_metadata() {
        for (field, wrong) in [
            ("organizationId", json!("ws_other")),
            ("audience", json!("https://attacker.board.example.test")),
            ("subjectId", json!("hsp_other")),
            ("bridgeId", json!("hcb_other")),
            ("agentId", json!("hca_other")),
            ("credentialKind", json!("parent")),
            ("generation", json!(2)),
            ("keyId", json!("key_invalid")),
            (
                "lineageId",
                json!("hca_00000000-0000-4000-8000-000000000001"),
            ),
            (
                "lineageId",
                json!("hcl_00000000-0000-1000-8000-000000000001"),
            ),
            ("sessionId", json!("private_session")),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let issuer = format!("http://{}", listener.local_addr().unwrap());
            let at = now().unwrap();
            let server = thread::spawn(move || {
                let (mut challenge, _) =
                    continuity_request(&listener, "/v1/hosted-cadence/continuity/challenge");
                respond(
                    &mut challenge,
                    &json!({
                        "version":"hosted-cadence-continuity.v1",
                        "challengeId":format!("ch_{}", "A".repeat(43)),
                        "operationId":format!("op_{}", "B".repeat(43)),
                        "nonce":"C".repeat(43), "registryEpoch":"D".repeat(43),
                        "issuedAtMs":at*1000,"expiresAtMs":at*1000+30_000
                    }),
                );
                let (mut bind, _) =
                    continuity_request(&listener, "/v1/hosted-cadence/continuity/bind");
                let mut reply = json!({
                    "version":"hosted-cadence-continuity.v1", "delivery":"bound",
                    "lineageId":LINEAGE, "keyId":format!("key_{}", "E".repeat(43)),
                    "generation":1, "organizationId":"ws_real",
                    "audience":"https://real.board.example.test", "subjectId":"hsp_subject",
                    "bridgeId":"hcb_bridge", "agentId":"hca_agent", "credentialKind":"child"
                });
                reply[field] = wrong;
                respond(&mut bind, &reply);
            });
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("enroll");
            trust(&dir, &issuer);
            let mut child = record(at + 120);
            child.issuer = issuer;
            child.source = EnrollmentSource::Browser;
            child.service_token = None;
            save(&dir, &child).unwrap();
            assert!(
                bind_browser(&dir).is_err(),
                "forged field {field} was accepted"
            );
            server.join().unwrap();
            assert!(bound_browser(&dir).is_err());
        }
    }

    #[test]
    fn browser_continuity_bind_refuses_expired_challenge_before_proof() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let at = now().unwrap();
        let server = thread::spawn(move || {
            let (mut challenge, _) =
                continuity_request(&listener, "/v1/hosted-cadence/continuity/challenge");
            respond(
                &mut challenge,
                &json!({
                    "version":"hosted-cadence-continuity.v1",
                    "challengeId":format!("ch_{}", "A".repeat(43)),
                    "operationId":format!("op_{}", "B".repeat(43)),
                    "nonce":"C".repeat(43), "registryEpoch":"D".repeat(43),
                    "issuedAtMs":at*1000-31_000,"expiresAtMs":at*1000-1_000
                }),
            );
            listener.set_nonblocking(true).unwrap();
            std::thread::sleep(Duration::from_millis(50));
            assert!(
                listener.accept().is_err(),
                "expired challenge caused bind request"
            );
        });
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("enroll");
        trust(&dir, &issuer);
        let mut child = record(at + 120);
        child.issuer = issuer;
        child.source = EnrollmentSource::Browser;
        child.service_token = None;
        save(&dir, &child).unwrap();
        let error = bind_browser(&dir).unwrap_err();
        assert!(format!("{error}").contains("expired"));
        server.join().unwrap();
        assert!(bound_browser(&dir).is_err());
    }

    #[test]
    fn browser_continuity_bind_never_claims_success_after_local_save_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let at = now().unwrap();
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("enroll");
        trust(&dir, &issuer);
        let mut child = record(at + 120);
        child.issuer = issuer;
        child.source = EnrollmentSource::Browser;
        child.service_token = None;
        save(&dir, &child).unwrap();
        let changed_dir = dir.clone();
        let server = thread::spawn(move || {
            let (mut challenge, _) =
                continuity_request(&listener, "/v1/hosted-cadence/continuity/challenge");
            respond(
                &mut challenge,
                &json!({
                    "version":"hosted-cadence-continuity.v1",
                    "challengeId":format!("ch_{}", "A".repeat(43)),
                    "operationId":format!("op_{}", "B".repeat(43)),
                    "nonce":"C".repeat(43), "registryEpoch":"D".repeat(43),
                    "issuedAtMs":at*1000,"expiresAtMs":at*1000+30_000
                }),
            );
            let (mut bind, _) = continuity_request(&listener, "/v1/hosted-cadence/continuity/bind");
            fs::remove_file(changed_dir.join(CONTINUITY_RECORD)).unwrap();
            fs::create_dir(changed_dir.join(CONTINUITY_RECORD)).unwrap();
            respond(
                &mut bind,
                &json!({
                    "version":"hosted-cadence-continuity.v1", "delivery":"bound",
                    "lineageId":LINEAGE, "keyId":format!("key_{}", "E".repeat(43)),
                    "generation":1, "organizationId":"ws_real",
                    "audience":"https://real.board.example.test", "subjectId":"hsp_subject",
                    "bridgeId":"hcb_bridge", "agentId":"hca_agent", "credentialKind":"child"
                }),
            );
        });
        let error = bind_browser(&dir).unwrap_err();
        assert!(format!("{error}").contains("uncertain"), "{error}");
        server.join().unwrap();
        assert!(bound_browser(&dir).is_err());
    }

    #[test]
    fn browser_continuity_bind_metadata_recovers_only_the_same_pending_key() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let at = now().unwrap();
        let server = thread::spawn(move || {
            let (mut first_challenge, _) =
                continuity_request(&listener, "/v1/hosted-cadence/continuity/challenge");
            respond(
                &mut first_challenge,
                &json!({
                    "version":"hosted-cadence-continuity.v1",
                    "challengeId":format!("ch_{}", "A".repeat(43)),
                    "operationId":format!("op_{}", "B".repeat(43)),
                    "nonce":"C".repeat(43), "registryEpoch":"D".repeat(43),
                    "issuedAtMs":at*1000,"expiresAtMs":at*1000+30_000
                }),
            );
            let (mut first_bind, first_body) =
                continuity_request(&listener, "/v1/hosted-cadence/continuity/bind");
            let first_public = first_body["publicKey"].as_str().unwrap().to_owned();
            write!(first_bind, "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").unwrap();
            let (mut second_challenge, _) =
                continuity_request(&listener, "/v1/hosted-cadence/continuity/challenge");
            respond(
                &mut second_challenge,
                &json!({
                    "version":"hosted-cadence-continuity.v1",
                    "challengeId":format!("ch_{}", "F".repeat(43)),
                    "operationId":format!("op_{}", "G".repeat(43)),
                    "nonce":"H".repeat(43), "registryEpoch":"I".repeat(43),
                    "issuedAtMs":at*1000,"expiresAtMs":at*1000+30_000
                }),
            );
            let (mut second_bind, second_body) =
                continuity_request(&listener, "/v1/hosted-cadence/continuity/bind");
            assert_eq!(second_body["publicKey"], first_public);
            assert_ne!(second_body["signature"], first_body["signature"]);
            let proof = format!(
                "{{\"version\":\"hosted-cadence-continuity.v1\",\"action\":\"bind_key\",\"challengeId\":\"ch_{}\",\"nonce\":\"{}\",\"registryEpoch\":\"{}\",\"publicKey\":\"{}\"}}",
                "F".repeat(43), "H".repeat(43), "I".repeat(43), first_public
            );
            ring::signature::UnparsedPublicKey::new(
                &ring::signature::ED25519,
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(&first_public)
                    .unwrap(),
            )
            .verify(
                proof.as_bytes(),
                &base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(second_body["signature"].as_str().unwrap())
                    .unwrap(),
            )
            .unwrap();
            respond(
                &mut second_bind,
                &json!({
                    "version":"hosted-cadence-continuity.v1", "delivery":"metadata",
                    "lineageId":LINEAGE, "keyId":format!("key_{}", "E".repeat(43)),
                    "generation":1, "organizationId":"ws_real",
                    "audience":"https://real.board.example.test", "subjectId":"hsp_subject",
                    "bridgeId":"hcb_bridge", "agentId":"hca_agent", "credentialKind":"child"
                }),
            );
        });
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("enroll");
        trust(&dir, &issuer);
        let mut child = record(at + 120);
        child.issuer = issuer;
        child.source = EnrollmentSource::Browser;
        child.service_token = None;
        save(&dir, &child).unwrap();
        let error = bind_browser(&dir).unwrap_err();
        assert!(format!("{error}").contains("uncertain"), "{error}");
        assert!(
            bound_browser(&dir).is_err(),
            "pending key claimed a lineage"
        );
        assert_eq!(bind_browser(&dir).unwrap().lineage_id(), LINEAGE);
        server.join().unwrap();
        assert_eq!(bound_browser(&dir).unwrap().generation(), 1);
    }

    #[test]
    fn browser_device_grant_rejects_forged_binding_and_scopes() {
        let at = now().unwrap();
        let valid = json!({"version":VERSION,"organization_id":"ws_real",
            "audience":"https://real.board.example.test",
            "principal":{"kind":"user","subject_id":"user_1","current_role":"owner"},
            "capabilities":["bridge.enroll","results.submit"],
            "credential":{"credential_id":"hcp_credential","access_token":BRIDGE,
                "token_type":"Bearer","issued_at":at,"expires_at":at+120,"renewal":"reexchange"}});
        assert!(browser_grant(&valid, "ws_real", "https://real.board.example.test").is_ok());
        for (path, wrong) in [
            ("organization_id", json!("ws_other")),
            ("audience", json!("https://attacker.board.example.test")),
            ("capabilities", json!(["bridge.enroll"])),
            ("capabilities", json!(["bridge.enroll", "reviews.submit"])),
        ] {
            let mut forged = valid.clone();
            forged[path] = wrong;
            assert!(browser_grant(&forged, "ws_real", "https://real.board.example.test").is_err());
        }
        for (path, wrong) in [
            ("kind", "service"),
            ("current_role", "member"),
            ("subject_id", "invalid subject"),
        ] {
            let mut forged = valid.clone();
            forged["principal"][path] = json!(wrong);
            assert!(browser_grant(&forged, "ws_real", "https://real.board.example.test").is_err());
        }
        let mut stale = valid;
        stale["credential"]["expires_at"] = json!(at - 1);
        assert!(browser_grant(&stale, "ws_real", "https://real.board.example.test").is_err());
    }

    #[test]
    fn hosted_browser_device_code_refuses_redirected_consent_url() {
        let code = json!({"version":DEVICE_VERSION,"device_code":format!("hcd_{}", "A".repeat(43)),
            "user_code":"K7PM-2QNF","verification_uri":"https://app.agenticos.test/device/hosted-cadence",
            "verification_uri_complete":"https://app.agenticos.test/device/hosted-cadence?code=K7PM-2QNF",
            "expires_in":600,"interval":5});
        assert!(device_code(&code).is_ok());
        let mut changed = code.clone();
        changed["verification_uri_complete"] = json!("https://attacker.test/?code=K7PM-2QNF");
        assert!(device_code(&changed).is_err());
        changed = code;
        changed["verification_uri"] = json!("http://app.agenticos.test/device/hosted-cadence");
        assert!(device_code(&changed).is_err());
    }

    #[test]
    fn browser_refuses_foreign_issuer_before_device_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        trust(&dir, "https://trusted.agenticos.test");
        assert!(enroll_browser(
            &issuer,
            "ws_real",
            "http://127.0.0.1:1",
            "worker",
            &dir,
            |_, _| panic!("foreign issuer returned a browser code"),
        )
        .is_err());
        assert!(listener.accept().is_err());
    }

    #[test]
    fn hosted_public_token_request_refuses_redirect_without_forwarding_verifier() {
        let issuer_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let attacker = TcpListener::bind("127.0.0.1:0").unwrap();
        attacker.set_nonblocking(true).unwrap();
        let issuer = format!("http://{}", issuer_listener.local_addr().unwrap());
        let location = format!("http://{}/stolen", attacker.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut socket, value) =
                public_request(&issuer_listener, "/v1/hosted-cadence/device/token");
            assert!(value["code_verifier"].is_string());
            write!(socket, "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").unwrap();
        });
        assert!(post_public(
            &issuer,
            "/v1/hosted-cadence/device/token",
            json!({"device_code":format!("hcd_{}", "A".repeat(43)),
                "code_verifier":"V".repeat(43)}),
        )
        .is_err());
        server.join().unwrap();
        assert!(attacker.accept().is_err());
    }

    #[test]
    fn browser_public_request_ignores_ambient_proxy() {
        if std::env::var_os("CAD729_PROXY_PROOF_CHILD").is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .arg("browser_public_request_ignores_ambient_proxy")
                .env("CAD729_PROXY_PROOF_CHILD", "1")
                .env("HTTP_PROXY", "http://127.0.0.1:1")
                .env("http_proxy", "http://127.0.0.1:1")
                .env("ALL_PROXY", "http://127.0.0.1:1")
                .env("all_proxy", "http://127.0.0.1:1")
                .env("NO_PROXY", "")
                .env("no_proxy", "");
            let output = crate::reaper::output(&mut child).unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut socket, body) = public_request(&listener, "/v1/hosted-cadence/device/code");
            assert_eq!(body["organization_id"], "ws_real");
            respond(&mut socket, &browser_code(60));
        });
        let (status, _) = post_public(
            &issuer,
            "/v1/hosted-cadence/device/code",
            json!({"organization_id":"ws_real"}),
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(status, 200);
    }

    #[test]
    fn browser_poll_pending_slow_down_obeys_deadline_without_child() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut code, _) = public_request(&listener, "/v1/hosted-cadence/device/code");
            respond(&mut code, &browser_code(16));
            for error in ["authorization_pending", "slow_down"] {
                let (mut token, request) =
                    public_request(&listener, "/v1/hosted-cadence/device/token");
                assert_eq!(request["device_code"], format!("hcd_{}", "A".repeat(43)));
                assert_eq!(request["code_verifier"].as_str().unwrap().len(), 43);
                respond_error(&mut token, error);
            }
            listener.set_nonblocking(true).unwrap();
            listener
        });
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        trust(&dir, &issuer);
        let started = Instant::now();
        let err = enroll_browser(
            &issuer,
            "ws_real",
            "https://real.board.example.test",
            "worker",
            &dir,
            |_, _| Ok(()),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("expired"));
        assert!(started.elapsed() >= Duration::from_secs(15));
        let listener = server.join().unwrap();
        assert!(
            listener.accept().is_err(),
            "deadline allowed another token or child request"
        );
        assert!(!dir.join(RECORD).exists());
    }

    #[test]
    fn browser_poll_terminal_denial_and_expiry_never_create_child() {
        for error in ["access_denied", "expired_token"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let issuer = format!("http://{}", listener.local_addr().unwrap());
            let server = thread::spawn(move || {
                let (mut code, _) = public_request(&listener, "/v1/hosted-cadence/device/code");
                respond(&mut code, &browser_code(60));
                let (mut token, _) = public_request(&listener, "/v1/hosted-cadence/device/token");
                respond_error(&mut token, error);
                listener.set_nonblocking(true).unwrap();
                listener
            });
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("e");
            trust(&dir, &issuer);
            let err = enroll_browser(
                &issuer,
                "ws_real",
                "https://real.board.example.test",
                "worker",
                &dir,
                |_, _| Ok(()),
            )
            .unwrap_err();
            assert!(format!("{err}").contains(if error == "access_denied" {
                "denied"
            } else {
                "expired"
            }));
            let listener = server.join().unwrap();
            assert!(
                listener.accept().is_err(),
                "terminal response allowed child request"
            );
            assert!(!dir.join(RECORD).exists());
        }
    }

    #[test]
    fn browser_long_poll_orders_concurrent_remove_before_any_later_send() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut code, _) = public_request(&listener, "/v1/hosted-cadence/device/code");
            respond(&mut code, &browser_code(60));
            let (mut token, _) = public_request(&listener, "/v1/hosted-cadence/device/token");
            respond_error(&mut token, "access_denied");
            listener.set_nonblocking(true).unwrap();
            listener
        });
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        trust(&dir, &issuer);
        let mut existing = record(u64::MAX);
        existing.issuer = issuer.clone();
        save(&dir, &existing).unwrap();
        let (shown_tx, shown_rx) = std::sync::mpsc::channel();
        let (continue_tx, continue_rx) = std::sync::mpsc::channel();
        let browser_dir = dir.clone();
        let browser = thread::spawn(move || {
            enroll_browser(
                &issuer,
                "ws_real",
                "https://real.board.example.test",
                "worker",
                &browser_dir,
                |_, _| {
                    shown_tx.send(()).unwrap();
                    continue_rx.recv().unwrap();
                    Ok(())
                },
            )
        });
        shown_rx.recv().unwrap();
        let (remove_started_tx, remove_started_rx) = std::sync::mpsc::channel();
        let (removed_tx, removed_rx) = std::sync::mpsc::channel();
        let remove_dir = dir.clone();
        let removing = thread::spawn(move || {
            remove_started_tx.send(()).unwrap();
            let result = remove(&remove_dir);
            removed_tx.send(result).unwrap();
        });
        remove_started_rx.recv().unwrap();
        assert!(removed_rx.recv_timeout(Duration::from_millis(40)).is_err());
        continue_tx.send(()).unwrap();
        assert!(format!("{}", browser.join().unwrap().unwrap_err()).contains("denied"));
        removing.join().unwrap();
        removed_rx.recv().unwrap().unwrap();
        let listener = server.join().unwrap();
        assert!(
            listener.accept().is_err(),
            "denied browser made a child request"
        );
        assert!(!dir.join(RECORD).exists());
        assert!(
            with_current(&dir, &pin(&existing.audience), |_| -> Result<()> {
                panic!("bearer callback ran after concurrent removal")
            })
            .is_err()
        );
    }

    #[test]
    fn browser_fixture_binds_one_child_without_storing_bridge_or_verifier() {
        if let Some(path) = std::env::var_os("CAD729_RESTART_PROOF_DIR") {
            let dir = Path::new(&path);
            let info = current(dir).unwrap();
            assert_eq!(info.subject_id(), "user_1");
            assert!(with_current(
                dir,
                &DestinationPin::new(
                    "ws_real",
                    "https://real.board.example.test",
                    "user_1",
                    "hca_agent"
                )
                .unwrap(),
                |secret| Ok(secret == CHILD)
            )
            .unwrap());
            return;
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let audience = "https://real.board.example.test";
        let at = now().unwrap();
        let server = thread::spawn(move || {
            let (mut code_socket, code_request) =
                public_request(&listener, "/v1/hosted-cadence/device/code");
            assert_eq!(code_request["version"], DEVICE_VERSION);
            assert_eq!(code_request["organization_id"], "ws_real");
            assert_eq!(code_request["audience"], audience);
            assert_eq!(
                code_request["requested_capabilities"],
                json!(["bridge.enroll", "results.submit"])
            );
            let challenge = code_request["code_challenge"].as_str().unwrap().to_owned();
            respond(
                &mut code_socket,
                &json!({"version":DEVICE_VERSION,
                "device_code":format!("hcd_{}", "A".repeat(43)),"user_code":"K7PM-2QNF",
                "verification_uri":"https://app.agenticos.test/device/hosted-cadence",
                "verification_uri_complete":"https://app.agenticos.test/device/hosted-cadence?code=K7PM-2QNF",
                "expires_in":600,"interval":5}),
            );
            let (mut token_socket, token_request) =
                public_request(&listener, "/v1/hosted-cadence/device/token");
            assert_eq!(
                token_request["device_code"],
                format!("hcd_{}", "A".repeat(43))
            );
            let verifier = token_request["code_verifier"].as_str().unwrap();
            assert_eq!(verifier.len(), 43);
            assert_eq!(
                challenge,
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(Sha256::digest(verifier.as_bytes()))
            );
            respond(
                &mut token_socket,
                &json!({"version":VERSION,"organization_id":"ws_real",
                "audience":audience,"principal":{"kind":"user","subject_id":"user_1","current_role":"owner"},
                "capabilities":["bridge.enroll","results.submit"],
                "credential":{"credential_id":"hcp_credential","access_token":BRIDGE,
                    "token_type":"Bearer","issued_at":at,"expires_at":at+120,"renewal":"reexchange"}}),
            );
            let mut child =
                request_with_audience(&listener, "/v1/hosted-cadence/enroll", BRIDGE, audience);
            respond(
                &mut child,
                &json!({"version":VERSION,"organization_id":"ws_real",
                "audience":audience,"bridge_id":"hcb_bridge","agents":[{
                "agent_id":"hca_agent","bridge_id":"hcb_bridge","client_agent_id":"worker",
                "principal_subject_id":"user_1","organization_id":"ws_real","audience":audience,
                "role":"implementer","capabilities":["results.submit"],
                "credential":{"credential_id":"hcc_credential","access_token":CHILD,
                    "token_type":"Bearer","issued_at":at,"expires_at":at+120,"renewal":"reexchange"}}]}),
            );
        });
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        trust(&dir, &issuer);
        let info = enroll_browser(&issuer, "ws_real", audience, "worker", &dir, |url, code| {
            assert_eq!(
                url,
                "https://app.agenticos.test/device/hosted-cadence?code=K7PM-2QNF"
            );
            assert_eq!(code, "K7PM-2QNF");
            Ok(())
        })
        .unwrap();
        server.join().unwrap();
        assert_eq!(info.subject_id(), "user_1");
        assert!(with_current(
            &dir,
            &DestinationPin::new("ws_real", audience, "user_1", "hca_agent").unwrap(),
            |secret| Ok(secret == CHILD)
        )
        .unwrap());
        assert!(!fs::read_to_string(dir.join(RECORD))
            .unwrap()
            .contains(BRIDGE));
        assert!(renew(&dir).is_err());
        assert_eq!(current(&dir).unwrap().agent_id(), "hca_agent");
        let mut restart = std::process::Command::new(std::env::current_exe().unwrap());
        restart
            .arg("browser_fixture_binds_one_child_without_storing_bridge_or_verifier")
            .env("CAD729_RESTART_PROOF_DIR", &dir);
        let output = crate::reaper::output(&mut restart).unwrap();
        assert!(
            output.status.success(),
            "fresh process could not use saved browser child: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
    #[test]
    fn local_issuer_fixture_binds_service_exchange_to_child_and_never_prints_secret() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let audience = "http://127.0.0.1:1";
        let at = now().unwrap();
        let server = thread::spawn(move || {
            let mut first = request(&listener, "/v1/hosted-cadence/service/exchange", SERVICE);
            respond(
                &mut first,
                &json!({"version":VERSION,"organization_id":"ws_real",
                "audience":audience,"principal":{"kind":"service","subject_id":"hsp_subject",
                "current_role":"member","provisioned_by":"owner_1"},
                "capabilities":["bridge.enroll","results.submit"],
                "credential":{"credential_id":"hcb_credential","access_token":BRIDGE,
                    "token_type":"Bearer","issued_at":at,"expires_at":at+120,"renewal":"reexchange"}}),
            );
            let mut second = request(&listener, "/v1/hosted-cadence/service/enroll", BRIDGE);
            respond(
                &mut second,
                &json!({"version":VERSION,"organization_id":"ws_real",
                "audience":audience,"bridge_id":"hcb_bridge","agents":[{
                "agent_id":"hca_agent","bridge_id":"hcb_bridge","client_agent_id":"worker",
                "principal_subject_id":"hsp_subject","organization_id":"ws_real",
                "audience":audience,"role":"implementer","capabilities":["results.submit"],
                "credential":{"credential_id":"hcc_credential","access_token":CHILD,
                    "token_type":"Bearer","issued_at":at,"expires_at":at+120,"renewal":"reexchange"}}]}),
            );
        });
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        trust(&dir, &issuer);
        let saved = enroll(&issuer, "ws_real", audience, "worker", SERVICE, &dir).unwrap();
        server.join().unwrap();
        assert_eq!(saved.audience(), audience);
        assert_eq!(current(&dir).unwrap().subject_id(), "hsp_subject");
        assert!(!format!("{saved:?}").contains(CHILD));
    }

    #[test]
    fn issuer_mismatched_audience_cannot_create_a_child_or_local_record() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let at = now().unwrap();
        let server = thread::spawn(move || {
            let mut first = request(&listener, "/v1/hosted-cadence/service/exchange", SERVICE);
            respond(
                &mut first,
                &json!({"version":VERSION,"organization_id":"ws_real",
                "audience":"https://attacker.board.example.test",
                "principal":{"kind":"service","subject_id":"hsp_subject",
                    "current_role":"member","provisioned_by":"owner_1"},
                "capabilities":["bridge.enroll","results.submit"],
                "credential":{"credential_id":"hcb_credential","access_token":BRIDGE,
                    "token_type":"Bearer","issued_at":at,"expires_at":at+120,
                    "renewal":"reexchange"}}),
            );
        });
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        trust(&dir, &issuer);
        assert!(enroll(
            &issuer,
            "ws_real",
            "http://127.0.0.1:1",
            "worker",
            SERVICE,
            &dir
        )
        .is_err());
        server.join().unwrap();
        assert!(!dir.join(RECORD).exists());
    }

    #[test]
    fn issuer_refuses_wrong_org_role_scope_and_expired_service_grant() {
        for mutation in ["org", "role", "scope", "expired"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let issuer = format!("http://{}", listener.local_addr().unwrap());
            let at = now().unwrap();
            let server = thread::spawn(move || {
                let mut first = request(&listener, "/v1/hosted-cadence/service/exchange", SERVICE);
                let mut grant = json!({"version":VERSION,"organization_id":"ws_real",
                    "audience":"http://127.0.0.1:1",
                    "principal":{"kind":"service","subject_id":"hsp_subject",
                        "current_role":"member","provisioned_by":"owner_1"},
                    "capabilities":["bridge.enroll","results.submit"],
                    "credential":{"credential_id":"hcb_credential","access_token":BRIDGE,
                        "token_type":"Bearer","issued_at":at,"expires_at":at+120,
                        "renewal":"reexchange"}});
                match mutation {
                    "org" => grant["organization_id"] = json!("ws_other"),
                    "role" => grant["principal"]["current_role"] = json!("owner"),
                    "scope" => grant["capabilities"] = json!(["assignments.read"]),
                    "expired" => grant["credential"]["expires_at"] = json!(at),
                    _ => unreachable!(),
                }
                respond(&mut first, &grant);
            });
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("e");
            trust(&dir, &issuer);
            assert!(enroll(
                &issuer,
                "ws_real",
                "http://127.0.0.1:1",
                "worker",
                SERVICE,
                &dir
            )
            .is_err());
            server.join().unwrap();
            assert!(!dir.join(RECORD).exists(), "saved invalid {mutation} grant");
        }
    }

    #[test]
    fn issuer_refuses_wrong_agent_role_capability_or_expiry() {
        for mutation in ["agent", "role", "scope", "expired", "over_parent"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let issuer = format!("http://{}", listener.local_addr().unwrap());
            let at = now().unwrap();
            let server = thread::spawn(move || {
                let mut first = request(&listener, "/v1/hosted-cadence/service/exchange", SERVICE);
                respond(
                    &mut first,
                    &json!({"version":VERSION,"organization_id":"ws_real",
                    "audience":"http://127.0.0.1:1",
                    "principal":{"kind":"service","subject_id":"hsp_subject",
                        "current_role":"member","provisioned_by":"owner_1"},
                    "capabilities":["bridge.enroll","results.submit"],
                    "credential":{"credential_id":"hcb_credential","access_token":BRIDGE,
                        "token_type":"Bearer","issued_at":at,"expires_at":at+120,
                        "renewal":"reexchange"}}),
                );
                let mut second = request(&listener, "/v1/hosted-cadence/service/enroll", BRIDGE);
                let mut child = json!({"version":VERSION,"organization_id":"ws_real",
                    "audience":"http://127.0.0.1:1","bridge_id":"hcb_bridge","agents":[{
                    "agent_id":"hca_agent","bridge_id":"hcb_bridge","client_agent_id":"worker",
                    "principal_subject_id":"hsp_subject","organization_id":"ws_real",
                    "audience":"http://127.0.0.1:1","role":"implementer",
                    "capabilities":["results.submit"],
                    "credential":{"credential_id":"hcc_credential","access_token":CHILD,
                        "token_type":"Bearer","issued_at":at,"expires_at":at+120,
                        "renewal":"reexchange"}}]});
                match mutation {
                    "agent" => child["agents"][0]["client_agent_id"] = json!("other"),
                    "role" => child["agents"][0]["role"] = json!("reviewer"),
                    "scope" => child["agents"][0]["capabilities"] = json!(["reviews.submit"]),
                    "expired" => child["agents"][0]["credential"]["expires_at"] = json!(at),
                    "over_parent" => {
                        child["agents"][0]["credential"]["expires_at"] = json!(at + 121)
                    }
                    _ => unreachable!(),
                }
                respond(&mut second, &child);
            });
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("e");
            trust(&dir, &issuer);
            assert!(enroll(
                &issuer,
                "ws_real",
                "http://127.0.0.1:1",
                "worker",
                SERVICE,
                &dir
            )
            .is_err());
            server.join().unwrap();
            assert!(!dir.join(RECORD).exists(), "saved invalid {mutation} child");
        }
    }

    #[test]
    fn issuer_redirect_never_forwards_service_bearer_to_another_origin() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let trap = TcpListener::bind("127.0.0.1:0").unwrap();
        let trap_url = format!("http://{}", trap.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut first = request(&listener, "/v1/hosted-cadence/service/exchange", SERVICE);
            write!(first, "HTTP/1.1 302 Found\r\nlocation: {trap_url}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").unwrap();
        });
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        trust(&dir, &issuer);
        assert!(enroll(
            &issuer,
            "ws_real",
            "http://127.0.0.1:1",
            "worker",
            SERVICE,
            &dir
        )
        .is_err());
        server.join().unwrap();
        trap.set_nonblocking(true).unwrap();
        assert!(
            trap.accept().is_err(),
            "issuer redirect forwarded the service bearer"
        );
        assert!(!dir.join(RECORD).exists());
    }

    #[test]
    fn revoked_service_exchange_does_not_extend_existing_child() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut first = request(&listener, "/v1/hosted-cadence/service/exchange", SERVICE);
            write!(
                first,
                "HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            )
            .unwrap();
        });
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        trust(&dir, &issuer);
        let mut old = record(now().unwrap() + 60);
        old.issuer = issuer;
        old.audience = "http://127.0.0.1:1".into();
        save(&dir, &old).unwrap();
        let bytes = fs::read(dir.join(RECORD)).unwrap();
        assert!(renew(&dir).is_err());
        server.join().unwrap();
        assert_eq!(fs::read(dir.join(RECORD)).unwrap(), bytes);
    }

    #[test]
    fn unpinned_or_foreign_issuer_fails_before_any_service_token_transport() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        assert!(enroll(
            "https://attacker.example.test",
            "ws_real",
            "https://real.board.example.test",
            "worker",
            SERVICE,
            &dir
        )
        .is_err());
        trust(&dir, "https://api.agenticos.example.test");
        assert!(enroll(
            "https://attacker.example.test",
            "ws_real",
            "https://real.board.example.test",
            "worker",
            SERVICE,
            &dir
        )
        .is_err());
        assert!(!dir.join(RECORD).exists());
    }

    #[test]
    fn remove_orders_after_inflight_renew_and_cannot_be_undone_by_it() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let audience = "http://127.0.0.1:1";
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("enroll");
        trust(&dir, &issuer);
        let mut old = record(u64::MAX);
        old.issuer = issuer.clone();
        old.audience = audience.into();
        save(&dir, &old).unwrap();
        let (seen_tx, seen_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let at = now().unwrap();
        let server = thread::spawn(move || {
            let mut first = request(&listener, "/v1/hosted-cadence/service/exchange", SERVICE);
            seen_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            respond(
                &mut first,
                &json!({"version":VERSION,"organization_id":"ws_real",
                "audience":audience,"principal":{"kind":"service","subject_id":"hsp_subject",
                "current_role":"member","provisioned_by":"owner_1"},
                "capabilities":["bridge.enroll","results.submit"],
                "credential":{"credential_id":"hcb_new","access_token":BRIDGE,
                    "token_type":"Bearer","issued_at":at,"expires_at":at+120,
                    "renewal":"reexchange"}}),
            );
            let mut second = request(&listener, "/v1/hosted-cadence/service/enroll", BRIDGE);
            respond(
                &mut second,
                &json!({"version":VERSION,"organization_id":"ws_real",
                "audience":audience,"bridge_id":"hcb_new_bridge","agents":[{
                "agent_id":"hca_new","bridge_id":"hcb_new_bridge","client_agent_id":"worker",
                "principal_subject_id":"hsp_subject","organization_id":"ws_real",
                "audience":audience,"role":"implementer","capabilities":["results.submit"],
                "credential":{"credential_id":"hcc_new","access_token":CHILD,
                    "token_type":"Bearer","issued_at":at,"expires_at":at+120,
                    "renewal":"reexchange"}}]}),
            );
        });
        let renew_dir = dir.clone();
        let renewing = thread::spawn(move || renew(&renew_dir).unwrap());
        seen_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let (removed_tx, removed_rx) = std::sync::mpsc::channel();
        let remove_dir = dir.clone();
        let removing = thread::spawn(move || {
            remove(&remove_dir).unwrap();
            removed_tx.send(()).unwrap();
        });
        let removed_while_renewing = removed_rx.recv_timeout(Duration::from_millis(50)).is_ok();
        release_tx.send(()).unwrap();
        renewing.join().unwrap();
        removing.join().unwrap();
        server.join().unwrap();
        assert!(
            !removed_while_renewing,
            "remove returned while renew was in flight"
        );
        assert!(
            !dir.join(RECORD).exists(),
            "renew recreated removed credential"
        );
    }
}
