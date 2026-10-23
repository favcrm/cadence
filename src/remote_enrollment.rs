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
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VERSION: &str = "hosted-cadence-auth.v1";
const MAX_RESPONSE: u64 = 64 * 1024;
/// A submissions page is the one management response that can legitimately
/// exceed the default bound: the endpoint admits up to 100 entries with 16 KiB
/// field bodies, so the read allowance mirrors the published page limits plus
/// envelope overhead. Every other route keeps [`MAX_RESPONSE`].
const MAX_SUBMISSIONS_RESPONSE: u64 = 4 * 1024 * 1024;
const MAX_SERVICE_TOKEN: usize = 128;
const RECORD: &str = "enrollment.json";
const TRUSTED_ISSUER: &str = "trusted-issuer";
const ACCESS_INGRESS: &str = "access-issuer.json";
const DEVICE_VERSION: &str = "hosted-cadence-device.v1";
const CONTINUITY_VERSION: &str = "hosted-cadence-continuity.v1";
const CONTINUITY_RECORD: &str = "continuity.json";
pub const HOSTED_LOGIN_ISSUER: &str = "https://api-v2.agenticos.hk";

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
    Browser(Option<&'a AccessIngress>),
}
fn is_service_source(source: &EnrollmentSource) -> bool {
    *source == EnrollmentSource::Service
}

fn reject(message: &str) -> Error {
    Error::rejected(message)
}
/// Single-quote a value for a shell command the CLI only ever *prints*
/// (the trusted-issuer remedy). `'` becomes `'\''` so a crafted
/// `--auth-dir` or issuer cannot break out of the quotes.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| reject("Invalid system clock"))?
        .as_secs())
}
// Shared AgenticOS hosted id contract: ASCII letters/digits/'_'/'-', 1..=200
// (packages/contracts/src/hosted-ids.ts). The old 128 cap rejected issued ids.
const MAX_ID: usize = 200;
fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID
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
    if record.state == ContinuityState::Bound {
        // The pending key is already durable. Sync the new temp entry before
        // publication, then make rename the last fallible step. A crash can
        // recover either Pending (retry the same key) or a complete Bound file.
        File::open(dir)?.sync_all()?;
    }
    temp.persist(path)
        .map_err(|_| reject("Unable to save hosted continuity"))?;
    if record.state == ContinuityState::Pending {
        // No network request is allowed until the recovery key itself is durable.
        File::open(dir)?.sync_all()?;
    }
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
    let challenge: ContinuityChallenge = serde_json::from_value(post_continuity(
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
    let reply: ContinuityBindReply = serde_json::from_value(post_continuity(
        &enrollment.issuer,
        "/v1/hosted-cadence/continuity/bind",
        &enrollment.child_token,
        json!({"version":CONTINUITY_VERSION,"challengeId":challenge.challenge_id,
        "publicKey":public,"signature":signature}),
    )?)
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
        reject("Hosted continuity bind may be committed remotely; local save uncertain; retry with `cadence remote enrollment bind-continuity --enrollment-dir <same-enrollment-dir>`")
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
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(TRUSTED_ISSUER))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // The pin is the operator's independent trust decision — the CLI
            // never creates it. Name the exact file and the exact commands,
            // every interpolated value shell-quoted so a crafted --auth-dir
            // cannot break out of the printed command.
            let file = dir.join(TRUSTED_ISSUER);
            let file = file.display().to_string();
            return Err(Error::rejected(format!(
                "Trusted issuer pin is missing. Create it yourself, then retry: \
                 printf '%s\\n' {} > {} && chmod 600 {}",
                shell_quote(issuer),
                shell_quote(&file),
                shell_quote(&file)
            )));
        }
        // Anything else (a symlink the O_NOFOLLOW refused, EACCES, a dangling
        // path component) is a refusal, not "missing": never print a write
        // command against a path that may resolve somewhere else.
        Err(error) => {
            return Err(Error::rejected(format!(
                "Trusted issuer pin at '{}' cannot be opened ({error}) — it must \
                 be a regular file you own, mode 0600, not a symlink",
                dir.join(TRUSTED_ISSUER).display()
            )));
        }
    };
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

fn require_login_issuer(dir: &Path, issuer: &str) -> Result<()> {
    if issuer == HOSTED_LOGIN_ISSUER {
        private_dir(dir, true)?;
        match fs::symlink_metadata(dir.join(TRUSTED_ISSUER)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            // A present or inaccessible pin must be validated; only genuine
            // absence permits the compiled hosted issuer default.
            _ => require_trusted_issuer(dir, issuer)?,
        }
        return Ok(());
    }
    require_trusted_issuer(dir, issuer)
}

/// Optional ingress credential. This is a Cloudflare Access pass for the
/// pinned issuer only, never an AgenticOS identity or a board credential.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccessIngress {
    issuer: String,
    client_id: String,
    client_secret: String,
}

fn access_header(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && value.bytes().all(|b| b.is_ascii_graphic())
}

fn read_access_ingress(dir: &Path, issuer: &str) -> Result<Option<AccessIngress>> {
    private_dir(dir, false)?;
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(ACCESS_INGRESS))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(reject("Hosted Access ingress configuration cannot be read")),
    };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(reject(
            "Hosted Access ingress configuration must be a private owned file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(2049).read_to_end(&mut bytes)?;
    if bytes.len() > 2048 {
        return Err(reject("Hosted Access ingress configuration is too large"));
    }
    let access: AccessIngress = serde_json::from_slice(&bytes)
        .map_err(|_| reject("Invalid hosted Access ingress configuration"))?;
    if origin(&access.issuer, cfg!(test))?.as_str() != issuer
        || !access_header(&access.client_id)
        || !access_header(&access.client_secret)
    {
        return Err(reject(
            "Hosted Access ingress configuration differs from pinned issuer",
        ));
    }
    Ok(Some(access))
}

fn confirm_access_ingress(
    dir: &Path,
    issuer: &str,
    expected: &Option<AccessIngress>,
) -> Result<()> {
    require_trusted_issuer(dir, issuer)?;
    confirm_access_ingress_config(dir, issuer, expected)
}

fn confirm_login_access_ingress(
    dir: &Path,
    issuer: &str,
    expected: &Option<AccessIngress>,
) -> Result<()> {
    require_login_issuer(dir, issuer)?;
    confirm_access_ingress_config(dir, issuer, expected)
}

/// Session lifecycle calls use the same exact issuer pin and Access ingress
/// checks as device login, before and after every outbound request.
pub fn validate_cli_session_pin(dir: &Path, issuer: &str) -> Result<()> {
    let issuer = origin(issuer, false)?;
    private_dir(dir, false)?;
    let access = read_access_ingress(dir, &issuer)?;
    confirm_login_access_ingress(dir, &issuer, &access)
}

pub fn cli_session_post(
    dir: &Path,
    issuer: &str,
    path: &str,
    bearer: &str,
    body: Value,
    deadline: Instant,
) -> Result<(u16, Value)> {
    let answer = cli_session_post_detailed(dir, issuer, path, bearer, body, deadline)?;
    Ok((answer.status, answer.body))
}

/// The issuer's correlation headers, read only when they are exactly the values
/// the AOS renewal route emits (`x-cadence-hop: issuer` and a UUID id). They are
/// diagnostic only: they never authorize anything and never change a status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IssuerCorrelation {
    pub(crate) request_id: String,
}

/// Test-only: run the production header validator on values a fake issuer would
/// send. The production send and the tests share this one validator.
#[cfg(test)]
pub(crate) fn correlation_from_header_values(
    hop: Option<&str>,
    request_id: Option<&str>,
) -> Option<IssuerCorrelation> {
    let mut headers = ureq::http::HeaderMap::new();
    if let Some(hop) = hop {
        headers.insert("x-cadence-hop", hop.parse().ok()?);
    }
    if let Some(id) = request_id {
        headers.insert("x-cadence-request-id", id.parse().ok()?);
    }
    issuer_correlation(&headers)
}

/// One renewal or revoke answer: the status, the parsed body, and the safe
/// correlation when the issuer sent a valid one. `correlation` is `None` for an
/// old issuer, for a malformed header, or for a success answer that carries none.
pub(crate) struct IssuerAnswer {
    pub(crate) status: u16,
    pub(crate) body: Value,
    pub(crate) correlation: Option<IssuerCorrelation>,
}

/// Read the AOS issuer's correlation headers. Only `x-cadence-hop: issuer` and a
/// bounded lowercase-or-uppercase UUID id are accepted; any other value is
/// dropped, so no arbitrary header text reaches a message or a log.
fn issuer_correlation(headers: &ureq::http::HeaderMap) -> Option<IssuerCorrelation> {
    let hop = headers.get("x-cadence-hop")?.to_str().ok()?;
    if hop != "issuer" {
        return None;
    }
    let id = headers.get("x-cadence-request-id")?.to_str().ok()?;
    let shaped = id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        });
    shaped.then(|| IssuerCorrelation {
        request_id: id.to_owned(),
    })
}

/// The production renewal or revoke POST, returning the status, body and the
/// validated issuer correlation. Only renewal reads the correlation; the legacy
/// `cli_session_post` discards it.
pub(crate) fn cli_session_post_detailed(
    dir: &Path,
    issuer: &str,
    path: &str,
    bearer: &str,
    body: Value,
    deadline: Instant,
) -> Result<IssuerAnswer> {
    cli_session_post_with(
        dir,
        issuer,
        path,
        bearer,
        body,
        deadline,
        send_issuer_request,
    )
}

/// The production wire call: strict TLS, no redirects, no proxy, bounded
/// timeout. It is the only transport a real renewal can use.
fn send_issuer_request(
    url: &str,
    bearer: &str,
    access: Option<&AccessIngress>,
    body: &Value,
    timeout: Duration,
) -> Result<(u16, Vec<u8>, Option<IssuerCorrelation>)> {
    let builder = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None);
    let mut request = crate::issuer_transport::agent(builder)
        .post(url)
        .header("Authorization", &format!("Bearer {bearer}"));
    if let Some(access) = access {
        request = request
            .header("CF-Access-Client-Id", &access.client_id)
            .header("CF-Access-Client-Secret", &access.client_secret);
    }
    let response = request
        .send_json(body)
        .map_err(|_| reject("Hosted CLI session request failed"))?;
    let status = response.status().as_u16();
    // Read the correlation headers before the body is consumed.
    let correlation = issuer_correlation(response.headers());
    let mut bytes = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut bytes)?;
    Ok((status, bytes, correlation))
}

/// `cli_session_post` with the wire call injected. Every check before the send
/// and every response guard after it are the production code; only the bytes
/// on the wire come from `send`. Production passes `send_issuer_request`.
/// Tests pass a closure, which is never reachable from config or environment.
fn cli_session_post_with(
    dir: &Path,
    issuer: &str,
    path: &str,
    bearer: &str,
    body: Value,
    deadline: Instant,
    send: impl FnOnce(
        &str,
        &str,
        Option<&AccessIngress>,
        &Value,
        Duration,
    ) -> Result<(u16, Vec<u8>, Option<IssuerCorrelation>)>,
) -> Result<IssuerAnswer> {
    if !matches!(
        path,
        "/v1/hosted-cadence/cli-session/renew" | "/v1/hosted-cadence/cli-session/revoke"
    ) {
        return Err(reject("Hosted CLI session path refused"));
    }
    let issuer = origin(issuer, false)?;
    private_dir(dir, false)?;
    require_login_issuer(dir, &issuer)?;
    let access = read_access_ingress(dir, &issuer)?;
    confirm_login_access_ingress(dir, &issuer, &access)?;
    let request_deadline = deadline.min(Instant::now() + Duration::from_secs(20));
    if Instant::now() >= request_deadline {
        return Err(Error::busy_coded(
            "timeout",
            "remote command deadline elapsed before session request",
        ));
    }
    let timeout = request_deadline.saturating_duration_since(Instant::now());
    let url = format!("{issuer}{path}");
    let (status, bytes, correlation) = send(&url, bearer, access.as_ref(), &body, timeout)?;
    if (300..400).contains(&status) {
        return Err(reject("Hosted issuer redirect refused"));
    }
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err(reject("Hosted issuer response too large"));
    }
    let value = serde_json::from_slice(&bytes)
        .map_err(|_| reject("Invalid hosted CLI session response"))?;
    confirm_login_access_ingress(dir, &issuer, &access)?;
    Ok(IssuerAnswer {
        status,
        body: value,
        correlation,
    })
}

#[cfg(test)]
pub(crate) fn cli_session_post_for_test(
    dir: &Path,
    issuer: &str,
    path: &str,
    bearer: &str,
    body: Value,
    deadline: Instant,
    send: impl FnOnce(
        &str,
        &str,
        Option<&AccessIngress>,
        &Value,
        Duration,
    ) -> Result<(u16, Vec<u8>, Option<IssuerCorrelation>)>,
) -> Result<IssuerAnswer> {
    cli_session_post_with(dir, issuer, path, bearer, body, deadline, send)
}

/// The account image door uses the saved canonical board audience, never the
/// issuer's media origin. Trust remains the normal login trust; no media pin
/// or ingress credential is added (issuer-only Access secrets stay there).
pub(crate) fn validate_account_image_destination(
    dir: &Path,
    issuer: &str,
    audience: &str,
) -> Result<()> {
    origin(audience, cfg!(test))?;
    validate_cli_session_pin(dir, issuer)
}

pub(crate) fn account_image_send(
    dir: &Path,
    issuer: &str,
    audience: &str,
    bearer: &str,
    operation: crate::remote_cli::AccountImageOperation,
    deadline: Instant,
) -> Result<crate::remote_cli::AccountImageResponse> {
    use crate::remote_cli::{AccountImageOperation as Op, AccountImageResponse};
    const JSON_CAP: u64 = 256 * 1024;
    const ARTIFACT_CAP: u64 = 10 * 1024 * 1024;
    let segment = |value: &str| {
        (5..=84).contains(&value.len())
            && value.starts_with("med_")
            && value[4..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    };
    let (path, submit, artifact) = match operation {
        Op::Quote => ("/__platform/media/price/image".to_owned(), None, false),
        Op::QuoteReference => (
            "/__platform/media/price/image?variant=image-to-image".to_owned(),
            None,
            false,
        ),
        Op::Submit {
            input,
            idempotency_key,
        } => {
            let object = input
                .as_object()
                .ok_or_else(|| reject("account image input must be an object"))?;
            let reference_edit = object.contains_key("references");
            let reference_ok = !reference_edit
                || (object["references"].as_array().is_some_and(|refs| {
                    refs.len() == 1
                        && refs[0].as_str().is_some_and(|reference| {
                            reference.split_once('.').is_some_and(|(job, digest)| {
                                segment(job)
                                    && digest.len() == 64
                                    && digest
                                        .bytes()
                                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                            })
                        })
                }));
            if object.len() != if reference_edit { 5 } else { 4 }
                || !["model", "prompt", "aspectRatio", "max_charge_minor"]
                    .iter()
                    .all(|key| object.contains_key(*key))
                || !reference_ok
                || input["model"] != "openai/gpt-image-2.5"
                || input["prompt"].as_str().is_none_or(|p| p.trim().is_empty())
                || input["aspectRatio"] != "1:1"
                || input["max_charge_minor"]
                    .as_u64()
                    .is_none_or(|n| !(1..=1_000_000_000).contains(&n))
                || !(8..=128).contains(&idempotency_key.len())
                || !idempotency_key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            {
                return Err(reject(
                    "account image submit is outside the reviewed image contract",
                ));
            }
            let bytes =
                serde_json::to_vec(&input).map_err(|_| reject("invalid account image input"))?;
            if bytes.len() as u64 > JSON_CAP {
                return Err(reject("account image input exceeds 256 KiB"));
            }
            (
                "/__platform/media/image".to_owned(),
                Some((bytes, idempotency_key)),
                false,
            )
        }
        Op::Job { job_id } if segment(&job_id) => {
            (format!("/__platform/media/jobs/{job_id}"), None, false)
        }
        Op::Artifact { reference } => {
            let valid = reference.split_once('.').is_some_and(|(job, digest)| {
                segment(job)
                    && digest.len() == 64
                    && digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            });
            if !valid {
                return Err(reject("account image artifact reference refused"));
            }
            (
                format!("/__platform/media/artifacts/{reference}"),
                None,
                true,
            )
        }
        _ => return Err(reject("account image job reference refused")),
    };
    validate_account_image_destination(dir, issuer, audience)?;
    let end = deadline
        .min(Instant::now() + std::time::Duration::from_secs(if artifact { 60 } else { 20 }));
    if Instant::now() >= end {
        return Err(Error::busy_coded(
            "timeout",
            "account image operation deadline elapsed",
        ));
    }
    let agent = issuer_agent(
        ureq::Agent::config_builder()
            .timeout_global(Some(end.saturating_duration_since(Instant::now())))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None),
    );
    let url = format!("{audience}{path}");
    let authorization = format!("Bearer {bearer}");
    let mut response = match submit {
        Some((bytes, key)) => agent
            .post(&url)
            .header("Authorization", &authorization)
            .header("Content-Type", "application/json")
            .header("Idempotency-Key", &key)
            .send(bytes.as_slice()),
        None => agent
            .get(&url)
            .header("Authorization", &authorization)
            .call(),
    }
    .map_err(|_| reject("account image operation could not be confirmed"))?;
    let status = response.status().as_u16();
    if (300..400).contains(&status) {
        return Err(reject("account image redirect refused"));
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() <= 128)
        .map(str::to_owned);
    let cap = if artifact && status == 200 {
        ARTIFACT_CAP
    } else {
        JSON_CAP
    };
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(cap + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(Error::invalid(
            "account_image_response_too_large",
            "account image response exceeds its operation bound",
        ));
    }
    validate_account_image_destination(dir, issuer, audience)?;
    if artifact && status == 200 {
        Ok(AccountImageResponse {
            status,
            body: None,
            content_type,
            bytes,
        })
    } else {
        let body = serde_json::from_slice(&bytes)
            .map_err(|_| reject("account image response was not JSON"))?;
        Ok(AccountImageResponse {
            status,
            body: Some(body),
            content_type: None,
            bytes: Vec::new(),
        })
    }
}

/// CAD-1429: one Publish Web management request to the verified PUBLIC
/// issuer, never to the board audience. Board hosts are answered by the board
/// catch-all before any `/v1` route, so `/v1/web-sites` must go to the issuer
/// origin the saved credential was minted by. The path namespace is exact, the
/// issuer is the one the caller verified (never a request argument), the
/// trusted-issuer pin and Access ingress are checked as for every issuer call,
/// and redirects are refused. Returns `(status, parsed JSON body)`.
///
/// One Publish Web management request: the verb, the namespace path, the
/// bearer, the contract headers and an optional body.
pub struct WebRequest<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub bearer: &'a str,
    pub headers: &'a [(&'a str, String)],
    pub body: Option<&'a [u8]>,
}

pub fn web_issuer_request(
    dir: &Path,
    issuer: &str,
    request: WebRequest<'_>,
    deadline: Instant,
) -> Result<(u16, Option<Value>)> {
    let WebRequest {
        method,
        path,
        bearer,
        headers,
        body,
    } = request;
    if !web_management_path(path) {
        return Err(reject("Publish Web path refused"));
    }
    let issuer = origin(issuer, cfg!(test))?;
    private_dir(dir, false)?;
    require_login_issuer(dir, &issuer)?;
    let access = read_access_ingress(dir, &issuer)?;
    confirm_login_access_ingress(dir, &issuer, &access)?;
    let request_deadline = web_request_deadline(method, path, Instant::now(), deadline);
    if Instant::now() >= request_deadline {
        return Err(Error::busy_coded(
            "timeout",
            "remote command deadline elapsed before the web request",
        ));
    }
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(
            request_deadline.saturating_duration_since(Instant::now()),
        ))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None);
    let agent = issuer_agent(config);
    let url = format!("{issuer}{path}");
    let auth = format!("Bearer {bearer}");
    let response = match (method, body) {
        ("GET", _) => {
            let mut request = agent.get(&url).header("Authorization", &auth);
            if let Some(access) = access.as_ref() {
                request = request
                    .header("CF-Access-Client-Id", &access.client_id)
                    .header("CF-Access-Client-Secret", &access.client_secret);
            }
            for (name, value) in headers {
                request = request.header(*name, value.as_str());
            }
            request.call()
        }
        (verb @ ("POST" | "PUT"), bytes) => {
            let builder = if verb == "PUT" {
                agent.put(&url)
            } else {
                agent.post(&url)
            };
            let mut request = builder.header("Authorization", &auth);
            if let Some(access) = access.as_ref() {
                request = request
                    .header("CF-Access-Client-Id", &access.client_id)
                    .header("CF-Access-Client-Secret", &access.client_secret);
            }
            for (name, value) in headers {
                request = request.header(*name, value.as_str());
            }
            match bytes {
                Some(bytes) => request.send(bytes),
                None => request.send_empty(),
            }
        }
        _ => return Err(reject("Publish Web method refused")),
    }
    .map_err(|_| reject("Hosted Publish Web request failed"))?;
    let status = response.status().as_u16();
    if (300..400).contains(&status) {
        return Err(reject("Hosted issuer redirect refused"));
    }
    let mut bytes = Vec::new();
    let limit = web_response_limit(path);
    response
        .into_body()
        .as_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(reject("Hosted issuer response too large"));
    }
    confirm_login_access_ingress(dir, &issuer, &access)?;
    Ok((status, serde_json::from_slice(&bytes).ok()))
}

/// The only paths a Publish Web management request may name. The namespace is
/// `/v1/web-sites/`. Encoded separators are refused everywhere except one shape:
/// the upload-file route `/v1/web-sites/uploads/upl_<id>/files/<segment>`, whose
/// single segment must decode, once and canonically, to a safe artifact path
/// (for example `assets/app-abc123.js` as `assets%2Fapp-abc123.js`). Every other
/// route refuses `%2F` and `%5C`, traversal, a query or fragment, and any
/// double-encoded or malformed escape.
/// The one construction point for issuer agents. Production uses the caller's
/// finished config; tests may additionally route through the installed issuer
/// fixtures, with the canonical transport selecting their precedence.
fn issuer_agent(builder: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>) -> ureq::Agent {
    crate::issuer_transport::agent(builder)
}

fn web_management_path(path: &str) -> bool {
    if path.len() > 2048
        || path.contains('#')
        || path.contains('\\')
        || !path.bytes().all(|b| (0x21..0x7f).contains(&b))
    {
        return false;
    }
    let (base, query) = match path.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (path, None),
    };
    if !base.starts_with("/v1/web-sites/") {
        return false;
    }
    if let Some(query) = query {
        // The submissions read is the one queried route. Its query is rebuilt
        // from a fixed key order and the published grammars, so it can never
        // carry an extra parameter, a fragment or an encoded separator.
        return web_submissions_route(base) && web_submissions_query(query);
    }
    if let Some(rest) = base.strip_prefix("/v1/web-sites/uploads/") {
        if let Some((upload, segment)) = rest.split_once("/files/") {
            if is_upload_id(upload) {
                // Only this route may name an encoded artifact path, and only if
                // the segment is the canonical encoding of a safe artifact path.
                return !segment.contains('/')
                    && crate::web_publish::decode_artifact_segment(segment).is_ok();
            }
        }
    }
    // Every other route: no percent-encoded separators, traversal or double encoding.
    let lower = base.to_ascii_lowercase();
    !base.contains("..") && !lower.contains("%2f") && !lower.contains("%5c") && !lower.contains('%')
}

/// `/v1/web-sites/sites/<site_id>/submissions`, the only route allowed a query.
fn web_submissions_route(base: &str) -> bool {
    let Some(rest) = base.strip_prefix("/v1/web-sites/sites/") else {
        return false;
    };
    let Some((site, tail)) = rest.split_once('/') else {
        return false;
    };
    let Some(suffix) = site.strip_prefix("site_") else {
        return false;
    };
    tail == "submissions"
        && (8..=80).contains(&suffix.len())
        && suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The read bound for one management path. A submissions page is the only
/// response allowed to exceed the default; every other route keeps it.
fn web_response_limit(path: &str) -> u64 {
    let base = path.split_once('?').map_or(path, |(base, _)| base);
    if web_submissions_route(base) {
        MAX_SUBMISSIONS_RESPONSE
    } else {
        MAX_RESPONSE
    }
}

/// `limit=<1..100>` then optionally `form=` and/or `cursor=`, in that order.
fn web_submissions_query(query: &str) -> bool {
    let mut parts = query.split('&');
    let Some(limit) = parts.next().and_then(|part| part.strip_prefix("limit=")) else {
        return false;
    };
    let limit_ok = (1..=3).contains(&limit.len())
        && limit.bytes().all(|b| b.is_ascii_digit())
        && (1..=100).contains(&limit.parse::<u16>().unwrap_or(0));
    if !limit_ok {
        return false;
    }
    let rest: Vec<&str> = parts.collect();
    match rest.as_slice() {
        [] => true,
        [part] if part.starts_with("form=") => part
            .strip_prefix("form=")
            .is_some_and(crate::web_sites::is_form_key),
        [part] if part.starts_with("cursor=") => part
            .strip_prefix("cursor=")
            .is_some_and(crate::web_sites::is_form_cursor),
        [form, cursor] => {
            form.strip_prefix("form=")
                .is_some_and(crate::web_sites::is_form_key)
                && cursor
                    .strip_prefix("cursor=")
                    .is_some_and(crate::web_sites::is_form_cursor)
        }
        _ => false,
    }
}

/// A file upload gets the caller's bounded deadline; JSON and other routes
/// retain the normal 20-second request cap.
fn web_request_deadline(
    method: &str,
    path: &str,
    now: Instant,
    caller_deadline: Instant,
) -> Instant {
    if method == "PUT" && web_upload_file_path(path) {
        caller_deadline
    } else {
        caller_deadline.min(now + Duration::from_secs(20))
    }
}

fn web_upload_file_path(path: &str) -> bool {
    path.strip_prefix("/v1/web-sites/uploads/")
        .and_then(|rest| rest.split_once("/files/"))
        .is_some_and(|(upload, segment)| {
            is_upload_id(upload)
                && !segment.contains('/')
                && crate::web_publish::decode_artifact_segment(segment).is_ok()
        })
}

/// CAD-1593: one portable-migration request to the verified issuer origin —
/// the same origin discipline as a session lifecycle call (`origin`,
/// `private_dir`, `require_login_issuer`, Access ingress rechecked before and
/// after), never to the board audience and never a caller-chosen URL. The
/// namespace is exactly `/v1/hosted-cadence/portable-migrations`; every other
/// path refuses before a request exists. No retries live here: the caller
/// decides, and a mutation whose answer was lost stays outcome-unknown for
/// reconciliation rather than being re-sent.
pub enum MigrationBody<'a> {
    /// No body (GET `/:operationId`).
    None,
    /// A JSON object body (preflight, begin, commit, rollback).
    Json(&'a Value),
    /// Exact manifest bytes (POST `/:operationId/manifest`), already bounded
    /// by the caller at 4 MiB.
    Bytes(&'a [u8]),
    /// A bounded streaming body: a whole pack (PUT `…/input-packs/:index`) or
    /// one part (PUT `…/input-packs/:index/parts/:part`). The reader must
    /// produce exactly that many bytes; nothing is buffered whole and the
    /// caller's `Read` verifies each member hash at its end.
    Stream(&'a mut dyn Read),
    /// No body, POST only (POST `/:operationId/input-packs/:index/finalize`).
    Empty,
}

/// Per-request timeout class. The commit call bounds to the server's 600 s
/// applier deadline plus a small transport margin; ordinary JSON and uploads
/// keep short, bounded caps. These are transport timeouts only — they are not
/// the `--wake-timeout` budget, and no request is re-sent on expiry.
#[derive(Clone, Copy)]
pub enum MigrationTimeout {
    /// Small JSON calls: preflight, begin, status, rollback.
    Json,
    /// POST `/:operationId/manifest` (bounded 4 MiB upload).
    Manifest,
    /// PUT `/:operationId/input-packs/:index` (bounded streaming upload).
    Pack,
    /// POST `/:operationId/commit` — covers the hosted 600 s apply window.
    Commit,
}

impl MigrationTimeout {
    pub(crate) fn cap(self) -> Duration {
        match self {
            Self::Json => Duration::from_secs(30),
            Self::Manifest => Duration::from_secs(60),
            Self::Pack => Duration::from_secs(300),
            Self::Commit => Duration::from_secs(620),
        }
    }

    /// Bounded answer size. A pack receipt repeats the sealed member tuple
    /// list — a full-inventory pack can legitimately fill megabytes — so pack
    /// answers share the 4 MiB manifest bound; every other answer stays at
    /// the ordinary small-response cap.
    pub(crate) fn response_cap(self) -> u64 {
        match self {
            Self::Pack => 4 * 1024 * 1024,
            _ => MAX_RESPONSE,
        }
    }
}

/// The request the native portable-migration flow may send. `workspace` is
/// the registered workspace id (`X-Cadence-Workspace`), `bearer` the renewed
pub struct MigrationRequest<'a, 'b> {
    pub method: &'a str,
    pub path: &'a str,
    pub workspace: &'a str,
    pub bearer: &'a str,
    pub timeout: MigrationTimeout,
    /// The body borrows the caller's buffer independently — a streamed pack
    /// reader must not force the header borrows to outlive it.
    pub body: MigrationBody<'b>,
}

/// Send one portable-migration request. Returns `(status, body)` with `body`
/// `None` when the answer is not JSON. Redirects, oversized answers and every
/// path outside the namespace are refused; the raw failure body is never
/// surfaced to the caller beyond this function's parsed envelope.
pub(crate) fn migration_issuer_request(
    dir: &Path,
    issuer: &str,
    request: MigrationRequest<'_, '_>,
    deadline: Instant,
) -> Result<(u16, Option<Value>)> {
    let MigrationRequest {
        method,
        path,
        workspace,
        bearer,
        timeout,
        body,
    } = request;
    if !portable_migration_path(path) {
        return Err(reject("Hosted portable-migration path refused"));
    }
    if !matches!(method, "GET" | "POST" | "PUT") || !id(workspace) {
        return Err(reject("Hosted portable-migration request refused"));
    }
    if !matches!(
        (method, &body),
        ("GET", MigrationBody::None)
            | (
                "POST",
                MigrationBody::Json(_) | MigrationBody::Bytes(_) | MigrationBody::Empty
            )
            | ("PUT", MigrationBody::Stream(_))
    ) {
        return Err(reject("Hosted portable-migration request shape refused"));
    }
    // The finalize shape is bound to its path: an empty POST only at
    // `…/finalize`, and `…/finalize` only as an empty POST.
    if !portable_migration_shape_admitted(path, method, &body) {
        return Err(reject("Hosted portable-migration request shape refused"));
    }
    private_dir(dir, false)?;
    require_login_issuer(dir, issuer)?;
    let access = read_access_ingress(dir, issuer)?;
    confirm_login_access_ingress(dir, issuer, &access)?;
    let request_deadline = deadline.min(Instant::now() + timeout.cap());
    if Instant::now() >= request_deadline {
        return Err(Error::busy_coded(
            "timeout",
            "remote command deadline elapsed before the migration request",
        ));
    }
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(
            request_deadline.saturating_duration_since(Instant::now()),
        ))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None);
    let agent = issuer_agent(config);
    let url = format!("{issuer}{path}");
    let auth = format!("Bearer {bearer}");
    let response = match (method, body) {
        ("GET", MigrationBody::None) => {
            migration_headers(agent.get(&url), &auth, workspace, access.as_ref()).call()
        }
        ("POST", MigrationBody::Json(value)) => {
            migration_headers(agent.post(&url), &auth, workspace, access.as_ref()).send_json(value)
        }
        ("POST", MigrationBody::Bytes(bytes)) => {
            migration_headers(agent.post(&url), &auth, workspace, access.as_ref())
                .content_type("application/octet-stream")
                .send(bytes)
        }
        ("POST", MigrationBody::Empty) => {
            migration_headers(agent.post(&url), &auth, workspace, access.as_ref()).send_empty()
        }
        ("PUT", MigrationBody::Stream(reader)) => {
            migration_headers(agent.put(&url), &auth, workspace, access.as_ref())
                .content_type("application/octet-stream")
                .send(ureq::SendBody::from_reader(reader))
        }
        // Unreachable after the shape check above; refuse, never guess.
        _ => return Err(reject("Hosted portable-migration request shape refused")),
    }
    // A send failure after headers were built is outcome-unknown — the bytes
    // may have landed. The caller decides; nothing here re-sends.
    .map_err(|_| Error::unknown("Hosted portable-migration request did not complete"))?;
    let status = response.status().as_u16();
    if (300..400).contains(&status) {
        return Err(reject("Hosted issuer redirect refused"));
    }
    let response_cap = timeout.response_cap();
    let mut bytes = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(response_cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            Error::unknown("Hosted migration response was interrupted; reconcile before retrying")
        })?;
    if bytes.len() as u64 > response_cap {
        return Err(Error::unknown(
            "Hosted migration response exceeded its bound; reconcile before retrying",
        ));
    }
    confirm_login_access_ingress(dir, issuer, &access).map_err(|_| {
        Error::unknown(
            "Hosted issuer authorization pin changed after the request; reconcile before retrying",
        )
    })?;
    Ok((status, serde_json::from_slice(&bytes).ok()))
}

/// The headers every portable-migration request carries: the `hct_` bearer,
/// the registered workspace pin and the pinned issuer's Access ingress pair
/// when one is configured. `S` stays generic — GET builds `WithoutBody`.
fn migration_headers<S>(
    request: ureq::RequestBuilder<S>,
    auth: &str,
    workspace: &str,
    access: Option<&AccessIngress>,
) -> ureq::RequestBuilder<S> {
    let request = request
        .header("Authorization", auth)
        .header("X-Cadence-Workspace", workspace);
    match access {
        Some(access) => request
            .header("CF-Access-Client-Id", &access.client_id)
            .header("CF-Access-Client-Secret", &access.client_secret),
        None => request,
    }
}

/// The request-shape decision for the migration family: an empty POST is
/// admitted only at a `…/finalize` path, and a `…/finalize` path is admitted
/// only as an empty POST. Pure, so the binding is testable without an issuer.
fn portable_migration_shape_admitted(path: &str, method: &str, body: &MigrationBody<'_>) -> bool {
    let empty_post = method == "POST" && matches!(body, MigrationBody::Empty);
    empty_post == path.ends_with("/finalize") && (!path.ends_with("/finalize") || empty_post)
}

/// The only paths a portable-migration request may name. Includes read-only
/// `/by-request/<requestId>` reconciliation after a lost begin response.
/// Per-operation paths admit manifest, input-packs, inputs (read-only), commit,
/// rollback and status only. Query, fragment, encoded separators and traversal refuse.
fn portable_migration_path(path: &str) -> bool {
    const BASE: &str = "/v1/hosted-cadence/portable-migrations";
    if !path.starts_with(BASE)
        || path.len() > 512
        || path.contains('?')
        || path.contains('#')
        || path.contains('\\')
        || path.contains('%')
        || path.contains("..")
        || !path.bytes().all(|b| (0x21..0x7f).contains(&b))
    {
        return false;
    }
    match &path[BASE.len()..] {
        "" | "/preflight" => true,
        rest => {
            let Some(tail) = rest.strip_prefix('/') else {
                return false;
            };
            if let Some((operation, suffix)) = tail.split_once('/') {
                if operation == "by-request" {
                    return id(suffix);
                }
                if !id(operation) {
                    return false;
                }
                match suffix {
                    "manifest" | "commit" | "rollback" | "inputs" => true,
                    _ => suffix
                        .strip_prefix("input-packs/")
                        .is_some_and(valid_pack_suffix),
                }
            } else {
                id(tail)
            }
        }
    }
}

/// `input-packs/` element after the prefix: `<index>`, `<index>/parts/<part>`
/// or `<index>/finalize`.
fn valid_pack_suffix(value: &str) -> bool {
    match value.split_once('/') {
        None => valid_pack_index(value),
        Some((index, "finalize")) => valid_pack_index(index),
        Some((index, tail)) => {
            valid_pack_index(index) && tail.strip_prefix("parts/").is_some_and(valid_part_number)
        }
    }
}

/// `^[1-9][0-9]*$` — a 1-based transport part number.
fn valid_part_number(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|b| b.is_ascii_digit())
        && !value.starts_with('0')
}

/// `^(0|[1-9][0-9]*)$` — the pack index the server-minted plan assigns.
fn valid_pack_index(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|b| b.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

/// `upl_` plus the contract's id body: 8 to 80 of `[A-Za-z0-9_-]`.
fn is_upload_id(value: &str) -> bool {
    value.strip_prefix("upl_").is_some_and(|rest| {
        (8..=80).contains(&rest.len())
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

fn confirm_access_ingress_config(
    dir: &Path,
    issuer: &str,
    expected: &Option<AccessIngress>,
) -> Result<()> {
    if &read_access_ingress(dir, issuer)? != expected {
        return Err(reject(
            "Hosted Access ingress configuration changed during enrollment",
        ));
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
    post_with_access(issuer, path, bearer, body, None)
}

fn post_with_access(
    issuer: &str,
    path: &str,
    bearer: &str,
    body: Value,
    access: Option<&AccessIngress>,
) -> Result<Value> {
    if access.is_some() && path != "/v1/hosted-cadence/enroll" {
        return Err(reject("Hosted Access ingress path refused"));
    }
    if access.is_some() {
        origin(issuer, cfg!(test))?;
    }
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .build();
    let mut request = ureq::Agent::new_with_config(config)
        .post(format!("{issuer}{path}"))
        .header("Authorization", format!("Bearer {bearer}"));
    if let Some(access) = access {
        if access.issuer != issuer {
            return Err(reject("Hosted Access ingress issuer changed"));
        }
        request = request
            .header("CF-Access-Client-Id", &access.client_id)
            .header("CF-Access-Client-Secret", &access.client_secret);
    }
    let response = request
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

#[derive(Clone, Copy)]
enum ContinuityRefusal {
    FreshConsent,
    Parked,
    Authorization,
    Uncertain,
    Invalid,
}

impl ContinuityRefusal {
    fn error(self) -> Error {
        reject(match self {
            Self::FreshConsent => {
                "Hosted continuity requires fresh consent in a new private enrollment directory"
            }
            Self::Parked => "Hosted continuity is parked while original session status is uncertain",
            Self::Authorization => "Hosted continuity authorization or issuer configuration refused",
            Self::Uncertain => "Hosted continuity request uncertain; retry the same pending key with `cadence remote enrollment bind-continuity --enrollment-dir <same-enrollment-dir>`",
            Self::Invalid => "Invalid hosted continuity issuer response",
        })
    }
}

fn generic_authorization_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
        && !matches!(
            code,
            "lineage_terminal" | "parked" | "authority_unavailable" | "unauthorized" | "forbidden"
        )
}

fn continuity_refusal(status: u16, bytes: &[u8]) -> ContinuityRefusal {
    // A gateway may lose the issuer's body after a bind commits. An empty 503
    // has no authority to claim, but must retain the exact-key retry path.
    if status == 503 && bytes.is_empty() {
        return ContinuityRefusal::Uncertain;
    }
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return ContinuityRefusal::Invalid;
    };
    let Some(fields) = value.as_object() else {
        return ContinuityRefusal::Invalid;
    };
    if fields.get("ok") != Some(&json!(false)) {
        return ContinuityRefusal::Invalid;
    }
    let Some(code) = fields.get("code").and_then(Value::as_str) else {
        return ContinuityRefusal::Invalid;
    };
    let reason = match fields.get("reason") {
        Some(reason) => match reason.as_str() {
            Some(reason) => Some(reason),
            None => return ContinuityRefusal::Invalid,
        },
        None => None,
    };
    if fields.len() != 2 + usize::from(reason.is_some()) {
        return ContinuityRefusal::Invalid;
    }
    match (status, code, reason) {
        (401, "lineage_terminal", None) | (409, "parked", Some("session_expired")) => {
            ContinuityRefusal::FreshConsent
        }
        (409, "parked", Some("session_uncertain")) => ContinuityRefusal::Parked,
        (401, "unauthorized", None) | (403, "forbidden", None) => ContinuityRefusal::Authorization,
        (503, "authority_unavailable", None) => ContinuityRefusal::Uncertain,
        (401 | 403, code, None) if generic_authorization_code(code) => {
            ContinuityRefusal::Authorization
        }
        _ => ContinuityRefusal::Invalid,
    }
}

/// Only continuity has structured issuer refusals. Other hosted enrollment
/// calls retain their existing error behavior and never inspect these codes.
fn post_continuity(issuer: &str, path: &str, bearer: &str, body: Value) -> Result<Value> {
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
        .map_err(|_| ContinuityRefusal::Uncertain.error())?;
    let status = response.status().as_u16();
    if (300..400).contains(&status) {
        return Err(reject("Hosted continuity issuer redirect refused"));
    }
    if status != 200 && !matches!(status, 401 | 403 | 409 | 503) {
        return Err(ContinuityRefusal::Invalid.error());
    }
    let mut bytes = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ContinuityRefusal::Uncertain.error())?;
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err(ContinuityRefusal::Invalid.error());
    }
    if status != 200 {
        return Err(continuity_refusal(status, &bytes).error());
    }
    serde_json::from_slice(&bytes).map_err(|_| ContinuityRefusal::Invalid.error())
}

#[cfg(test)]
fn post_public(issuer: &str, path: &str, body: Value) -> Result<(u16, Value)> {
    post_public_with_access(issuer, path, body, None)
}

fn post_public_with_access(
    issuer: &str,
    path: &str,
    body: Value,
    access: Option<&AccessIngress>,
) -> Result<(u16, Value)> {
    if access.is_some()
        && !matches!(
            path,
            "/v1/hosted-cadence/device/code" | "/v1/hosted-cadence/device/token"
        )
    {
        return Err(reject("Hosted Access ingress path refused"));
    }
    if access.is_some() {
        origin(issuer, cfg!(test))?;
    }
    let builder = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None);
    let mut request = issuer_agent(builder).post(format!("{issuer}{path}"));
    if let Some(access) = access {
        if access.issuer != issuer {
            return Err(reject("Hosted Access ingress issuer changed"));
        }
        request = request
            .header("CF-Access-Client-Id", &access.client_id)
            .header("CF-Access-Client-Secret", &access.client_secret);
    }
    let response = request
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
fn exact_fields(value: &Value, required: &[&str], optional: &[&str]) -> bool {
    value.as_object().is_some_and(|fields| {
        required.iter().all(|key| fields.contains_key(*key))
            && fields
                .keys()
                .all(|key| required.contains(&key.as_str()) || optional.contains(&key.as_str()))
    })
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
    enroll_browser_with(
        issuer,
        org,
        audience,
        client_agent,
        dir,
        show_code,
        &mut RealLoginRuntime,
    )
}

/// `enroll_browser` on an injected runtime — private, production only
/// ever passes [`RealLoginRuntime`]; tests drive a scripted one so the
/// poll loop's waits run on virtual time (CAD-1341).
fn enroll_browser_with(
    issuer: &str,
    org: &str,
    audience: &str,
    client_agent: &str,
    dir: &Path,
    show_code: impl FnOnce(&str, &str) -> Result<()>,
    runtime: &mut dyn LoginRuntime,
) -> Result<EnrollmentInfo> {
    let issuer = origin(issuer, cfg!(test))?;
    let audience = origin(audience, cfg!(test))?;
    if !id(org) || !id(client_agent) {
        return Err(reject("Invalid hosted browser enrollment request"));
    }
    private_dir(dir, false)?;
    let _guard = lock(dir, true)?;
    require_trusted_issuer(dir, &issuer)?;
    let access = read_access_ingress(dir, &issuer)?;
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).map_err(|_| reject("Unable to create PKCE verifier"))?;
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    confirm_access_ingress(dir, &issuer, &access)?;
    let (status, code) = runtime.post_public(
        &issuer,
        "/v1/hosted-cadence/device/code",
        json!({"version":DEVICE_VERSION,"organization_id":org,"audience":audience,
            "client_label":"Cadence local team","requested_capabilities":["bridge.enroll","results.submit"],
            "code_challenge":challenge}),
        access.as_ref(),
    )?;
    if status != 200 {
        return Err(reject("Hosted issuer refused device authorization"));
    }
    let (device, user, verification, mut interval) = device_code(&code)?;
    show_code(verification, user)?;
    let deadline = runtime.now() + Duration::from_secs(timestamp(&code, "expires_in")?);
    let grant = loop {
        if runtime.now() >= deadline {
            return Err(reject("Hosted browser authorization expired; start again"));
        }
        runtime.wait(Duration::from_secs(interval));
        if runtime.now() >= deadline {
            return Err(reject("Hosted browser authorization expired; start again"));
        }
        confirm_access_ingress(dir, &issuer, &access)?;
        let (status, response) = runtime.post_public(
            &issuer,
            "/v1/hosted-cadence/device/token",
            json!({"device_code":device,"code_verifier":verifier}),
            access.as_ref(),
        )?;
        if runtime.now() >= deadline {
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
    confirm_access_ingress(dir, &issuer, &access)?;
    enroll_child_locked(
        &issuer,
        org,
        &audience,
        client_agent,
        dir,
        &grant,
        ChildSource::Browser(access.as_ref()),
    )
}

/// A verified login: the operator named the slug, and the issuer's grant
/// echoed exactly the workspace and audience that were requested.
pub struct LoginGrant {
    pub organization_id: String,
    pub slug: String,
    pub endpoint: String,
    pub expires_at: u64,
    issuer: String,
    subject_id: String,
    capabilities: Vec<String>,
    cli_session: Option<CliSession>,
    token: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliSession {
    pub session_id: String,
    pub refresh_token: String,
    pub generation: u64,
    pub device_label: String,
    pub idle_expires_at: u64,
    pub absolute_expires_at: u64,
}

impl CliSession {
    fn from_value(value: &Value, at: u64) -> Result<Self> {
        let session = &value["cli_session"];
        let result = Self {
            session_id: field(session, "session_id")?.to_owned(),
            refresh_token: field(session, "refresh_token")?.to_owned(),
            generation: timestamp(session, "generation")?,
            device_label: field(session, "device_label")?.to_owned(),
            idle_expires_at: timestamp(session, "idle_expires_at")?,
            absolute_expires_at: timestamp(session, "absolute_expires_at")?,
        };
        let fields = session
            .as_object()
            .ok_or_else(|| reject("Invalid CLI session metadata"))?;
        if fields.len() != 6
            || !result.session_id.strip_prefix("hcs_").is_some_and(|s| {
                s.len() == 36 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
            || !token(&result.refresh_token, "hcr_")
            || result.generation != 1
            || result.device_label.is_empty()
            || result.device_label.len() > 120
            || result.idle_expires_at <= at
            || result.absolute_expires_at <= at
            || result.idle_expires_at > result.absolute_expires_at
        {
            return Err(reject("Invalid CLI session metadata"));
        }
        Ok(result)
    }
}

const CLOUD_SUFFIX: &str = ".cadencecloud.app";

/// A DNS label: lowercase letters, digits and inner hyphens, 1..=63.
fn slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The issuer may narrow the consent: a grant is the CLI pair (always) plus
/// any unique subset of the other requested capabilities. It never names an
/// unrequested or duplicate capability, and it never drops a CLI scope.
fn consented_caps(caps: &[Value], requested: &[&str]) -> bool {
    let mut seen = BTreeSet::new();
    caps.iter().all(|c| {
        c.as_str()
            .is_some_and(|c| requested.contains(&c) && seen.insert(c))
    }) && ["cli.read", "cli.write"].iter().all(|c| seen.contains(c))
        // A web scope must be granted exactly when it was requested.
        && requested.iter().filter(|c| c.starts_with("web.")).all(|c| seen.contains(c))
}

/// Human workspace roles the hosted login exchange returns. Bridge
/// enrollment stays owner-only; service identity stays `kind: service`.
pub(crate) fn human_workspace_role(role: &str) -> bool {
    matches!(role, "owner" | "member")
}

/// Accept an owner or member grant for the requested workspace and audience
/// (`https://<slug>.cadencecloud.app`), with capabilities that
/// [`consented_caps`] allows for `requested`. The role does not add scopes.
/// The issuer binds slug to workspace itself; the response carries no slug field.
fn login_grant(
    value: &Value,
    org: Option<&str>,
    hint: Option<&str>,
    issuer: &str,
    requested: &[&str],
) -> Result<LoginGrant> {
    let at = now()?;
    let (principal, credential) = (&value["principal"], &value["credential"]);
    let organization_id = field(value, "organization_id")?;
    let slug_value = if org.is_some() {
        hint.ok_or_else(|| reject("Legacy login needs --slug"))?
    } else {
        field(value, "organization_slug")?
    };
    let endpoint = format!("https://{slug_value}{CLOUD_SUFFIX}");
    let caps = value["capabilities"]
        .as_array()
        .ok_or_else(|| reject("Invalid hosted login capabilities"))?;
    let exact_caps = consented_caps(caps, requested);
    if !exact_fields(
        value,
        &[
            "version",
            "organization_id",
            "audience",
            "principal",
            "capabilities",
            "credential",
            "cli_session",
        ],
        &["organization_slug"],
    ) || !exact_fields(principal, &["kind", "current_role", "subject_id"], &[])
        || !exact_fields(
            credential,
            &[
                "token_type",
                "access_token",
                "credential_id",
                "issued_at",
                "expires_at",
                "renewal",
            ],
            &[],
        )
        || field(value, "version")? != VERSION
        || org.is_some_and(|expected| organization_id != expected)
        || hint.is_some_and(|expected| slug_value != expected)
        || !id(organization_id)
        || !slug(slug_value)
        || field(value, "audience")? != endpoint
        || field(principal, "kind")? != "user"
        || !human_workspace_role(field(principal, "current_role")?)
        || !id(field(principal, "subject_id")?)
        || !exact_caps
        || field(credential, "token_type")? != "Bearer"
        || field(credential, "renewal")? != "reexchange"
        || !token(field(credential, "access_token")?, "hct_")
        || !id(field(credential, "credential_id")?)
        || timestamp(credential, "issued_at")? > at
        || timestamp(credential, "expires_at")? <= at
    {
        return Err(reject("Issuer grant did not match hosted login consent"));
    }
    let issued_at = timestamp(credential, "issued_at")?;
    let expires_at = timestamp(credential, "expires_at")?;
    if expires_at - issued_at > 300 {
        return Err(reject(
            "Issuer access credential lifetime exceeded five minutes",
        ));
    }
    let cli_session = Some(CliSession::from_value(value, at)?);
    let fields = value
        .as_object()
        .ok_or_else(|| reject("Issuer grant response shape invalid"))?;
    let allowed = [
        "version",
        "organization_id",
        "audience",
        "organization_slug",
        "principal",
        "capabilities",
        "credential",
        "cli_session",
    ];
    if fields.keys().any(|key| !allowed.contains(&key.as_str()))
        || [
            "version",
            "organization_id",
            "audience",
            "principal",
            "capabilities",
            "credential",
        ]
        .iter()
        .any(|key| !fields.contains_key(*key))
        || fields.contains_key("organization_slug") && fields["organization_slug"] != slug_value
    {
        return Err(reject("Issuer grant response shape invalid"));
    }
    Ok(LoginGrant {
        organization_id: organization_id.to_string(),
        slug: slug_value.to_string(),
        endpoint,
        expires_at,
        issuer: issuer.to_owned(),
        subject_id: field(principal, "subject_id")?.to_owned(),
        capabilities: serde_json::from_value(value["capabilities"].clone())
            .map_err(|_| reject("Invalid hosted login capabilities"))?,
        cli_session,
        token: field(credential, "access_token")?.to_string(),
    })
}

/// Test controller access to the strict human login parser.
#[cfg(all(test, feature = "test-seam"))]
pub(crate) fn login_grant_for_test(value: &Value, issuer: &str) -> Result<LoginGrant> {
    login_grant(
        value,
        None,
        Some("lc"),
        issuer,
        &["cli.read", "cli.write", "wiki.read", "wiki.write"],
    )
}

impl LoginGrant {
    /// Write the `hct_` to `cli-<slug>.json` at 0600 (temp file, then
    /// rename), separate from `orgs.json`. Never printed.
    pub fn save_credential(&self, dir: &Path) -> Result<()> {
        private_dir(dir, false)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let _guard = crate::remote_cli::private_lock(dir, &self.slug, deadline)?;
        let destination = dir.join(format!("cli-{}.json", self.slug));
        match fs::symlink_metadata(&destination) {
            Ok(meta)
                if meta.is_file()
                    && meta.uid() == unsafe { libc::geteuid() }
                    && meta.mode() & 0o077 == 0 => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(reject(
                    "CLI credential destination must be a private owned regular file",
                ))
            }
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random)
            .map_err(|_| reject("Unable to create CLI credential temporary path"))?;
        let tmp = dir.join(format!(
            ".cli-{}.tmp-{}",
            self.slug,
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random)
        ));
        let body = serde_json::to_vec(&json!({"version": VERSION,
            "organization_id": self.organization_id, "organization_slug": self.slug,
            "audience": self.endpoint, "issuer": self.issuer, "subject_id": self.subject_id,
            "capabilities": self.capabilities, "access_token": self.token,
            "expires_at": self.expires_at, "cli_session": self.cli_session}))
        .map_err(|e| Error::internal(e.to_string()))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)
            .map_err(|_| reject("CLI credential temporary path refused"))?;
        // A mode-0300 auth directory is valid for saving (write + execute)
        // but cannot be opened for read. Keep the handle best-effort: once
        // the rename publishes the credential, a directory fsync failure
        // must not make the caller roll the registry back and pair the old
        // binding with the new credential.
        let dir_handle = File::open(dir).ok();
        let result = (|| {
            file.write_all(&body)?;
            file.sync_all()?;
            fs::rename(&tmp, destination)?;
            if let Some(dir) = &dir_handle {
                let _ = dir.sync_all();
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }
}

/// What the login device poll needs from its environment: a monotonic
/// clock, a wait and the public request/response transport
/// (CAD-1124). Production always uses [`RealLoginRuntime`]; tests drive
/// [`login_browser_with`] with a scripted in-memory one. The seam is
/// private and `pub fn login_browser` constructs the real runtime itself
/// — no caller, environment or remote input can ever select another.
trait LoginRuntime {
    fn now(&mut self) -> Instant;
    fn wait(&mut self, interval: Duration);
    fn post_public(
        &mut self,
        issuer: &str,
        path: &str,
        body: Value,
        access: Option<&AccessIngress>,
    ) -> Result<(u16, Value)>;
}

/// The production login runtime: the real monotonic clock, a real sleep
/// and the real [`post_public_with_access`] transport, unchanged.
struct RealLoginRuntime;

impl LoginRuntime for RealLoginRuntime {
    fn now(&mut self) -> Instant {
        Instant::now()
    }
    fn wait(&mut self, interval: Duration) {
        std::thread::sleep(interval);
    }
    fn post_public(
        &mut self,
        issuer: &str,
        path: &str,
        body: Value,
        access: Option<&AccessIngress>,
    ) -> Result<(u16, Value)> {
        post_public_with_access(issuer, path, body, access)
    }
}

/// `cadence login` (CAD-1019 slice 1b): the hosted-cadence device grant
/// (PKCE, `hcd_` device code, `hct_` bridge) against the operator-pinned
/// issuer. The operator names the workspace and slug; the request carries
/// audience `https://<slug>.cadencecloud.app`, and `login_grant` accepts
/// only a grant that echoes both. Nothing is persisted here: the caller
/// records the org, then saves. On a fresh HOME the login creates the
/// credential directory (0700) itself, then stops at the operator's
/// trusted-issuer pin — the pin is never created automatically.
pub fn login_browser(
    issuer: &str,
    org: &str,
    slug_value: &str,
    dir: &Path,
    show_code: impl FnOnce(&str, &str) -> Result<()>,
) -> Result<LoginGrant> {
    login_browser_with(
        issuer,
        org,
        slug_value,
        dir,
        show_code,
        &mut RealLoginRuntime,
    )
}

/// Ordinary portal and org-flag logins request CLI and wiki capabilities.
/// `--web` deliberately requests CLI and Publish Web capabilities instead,
/// without adding wiki consent; `--settings` requests the CLI pair plus the
/// Settings scopes (`settings.read`, `settings.write` — CAD-1586 remote
/// `settings agents`/`master status`/`master resume` need them). The two
/// flags combine. The issuer may narrow the requested set; accepting a
/// member role never widens it.
#[derive(Clone, Copy)]
enum LoginScope {
    Wiki,
    /// CAD-1429: `cadence login --web` asks for the Publish Web scopes on top
    /// of the CLI pair, and only on this explicit request.
    Web,
    /// CAD-1586: `cadence login --settings` asks for the Settings scopes on
    /// top of the CLI pair — remote settings verbs refuse without them.
    Settings,
    /// Both opt-ins on one consent: `--web --settings`.
    WebAndSettings,
}

impl LoginScope {
    fn requested(self) -> Vec<&'static str> {
        match self {
            LoginScope::Wiki => vec!["cli.read", "cli.write", "wiki.read", "wiki.write"],
            LoginScope::Web => vec!["cli.read", "cli.write", "web.read", "web.prepare"],
            LoginScope::Settings => {
                vec!["cli.read", "cli.write", "settings.read", "settings.write"]
            }
            LoginScope::WebAndSettings => vec![
                "cli.read",
                "cli.write",
                "web.read",
                "web.prepare",
                "settings.read",
                "settings.write",
            ],
        }
    }
}

pub fn login_portal(
    issuer: &str,
    hint: Option<&str>,
    dir: &Path,
    show_code: impl FnOnce(&str, &str) -> Result<()>,
) -> Result<LoginGrant> {
    login_portal_scoped(issuer, hint, dir, show_code, false, false)
}

/// The portal login; `web` adds the Publish Web scopes (`cadence login
/// --web`), `settings` adds the Settings scopes (`cadence login
/// --settings`), and nothing else changes.
pub fn login_portal_scoped(
    issuer: &str,
    hint: Option<&str>,
    dir: &Path,
    show_code: impl FnOnce(&str, &str) -> Result<()>,
    web: bool,
    settings: bool,
) -> Result<LoginGrant> {
    let issuer = origin(issuer, cfg!(test))?;
    login_device_flow(
        &issuer,
        None,
        hint,
        dir,
        show_code,
        &mut RealLoginRuntime,
        match (web, settings) {
            (true, true) => LoginScope::WebAndSettings,
            (true, false) => LoginScope::Web,
            (false, true) => LoginScope::Settings,
            (false, false) => LoginScope::Wiki,
        },
    )
}

/// Start a hosted login device grant from the CLI registration entry point.
/// The browser first completes workspace registration at `/register`, then
/// continues to the normal hosted consent page. No credential is issued by
/// the registration page itself; the existing device token exchange remains
/// the only bearer-issuing step.
pub fn register_portal(
    issuer: &str,
    name: Option<&str>,
    workspace_slug: Option<&str>,
    dir: &Path,
    show_code: impl FnOnce(&str, &str) -> Result<()>,
) -> Result<LoginGrant> {
    register_portal_with(
        issuer,
        name,
        workspace_slug,
        dir,
        show_code,
        &mut RealLoginRuntime,
    )
}

fn register_portal_with(
    issuer: &str,
    name: Option<&str>,
    workspace_slug: Option<&str>,
    dir: &Path,
    show_code: impl FnOnce(&str, &str) -> Result<()>,
    runtime: &mut dyn LoginRuntime,
) -> Result<LoginGrant> {
    let issuer = origin(issuer, cfg!(test))?;
    if let Some(name) = name {
        if name.is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
            return Err(reject("Invalid workspace name"));
        }
    }
    if let Some(slug_value) = workspace_slug {
        if !slug(slug_value) {
            return Err(reject("Invalid workspace slug"));
        }
    }
    login_device_flow(
        &issuer,
        None,
        None,
        dir,
        |verification, code| {
            let registration = registration_url(verification, name, workspace_slug)?;
            show_code(&registration, code)
        },
        runtime,
        LoginScope::Wiki,
    )
}

/// Build the `/register` handoff from the issuer's device consent URL. The
/// `after` value is the consent URL's path and query; validation keeps the
/// browser on the issuer's HTTPS origin.
fn registration_url(
    verification: &str,
    name: Option<&str>,
    workspace_slug: Option<&str>,
) -> Result<String> {
    let uri: ureq::http::Uri = verification
        .parse()
        .map_err(|_| reject("Invalid registration handoff URL"))?;
    let authority = uri
        .authority()
        .ok_or_else(|| reject("Registration handoff URL must be absolute"))?;
    if uri.scheme_str() != Some("https")
        || authority.as_str().contains('@')
        || uri.path().is_empty()
        || uri.path() == "/"
        || verification.contains('#')
    {
        return Err(reject("Registration handoff URL must be HTTPS"));
    }
    let after = uri
        .path_and_query()
        .map_or(uri.path(), |value| value.as_str());
    let mut query = vec![format!("after={}", query_encode(after))];
    if let Some(name) = name {
        query.push(format!("name={}", query_encode(name)));
    }
    if let Some(slug) = workspace_slug {
        query.push(format!("slug={}", query_encode(slug)));
    }
    Ok(format!(
        "https://{}/register?{}",
        authority,
        query.join("&")
    ))
}

fn query_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

fn login_browser_with(
    issuer: &str,
    org: &str,
    slug_value: &str,
    dir: &Path,
    show_code: impl FnOnce(&str, &str) -> Result<()>,
    runtime: &mut dyn LoginRuntime,
) -> Result<LoginGrant> {
    let issuer = origin(issuer, cfg!(test))?;
    if !id(org) || !slug(slug_value) {
        return Err(reject("Invalid hosted login request"));
    }
    login_device_flow(
        &issuer,
        Some(org),
        Some(slug_value),
        dir,
        show_code,
        runtime,
        LoginScope::Wiki,
    )
}

fn login_device_flow(
    issuer: &str,
    org: Option<&str>,
    hint: Option<&str>,
    dir: &Path,
    show_code: impl FnOnce(&str, &str) -> Result<()>,
    runtime: &mut dyn LoginRuntime,
    scope: LoginScope,
) -> Result<LoginGrant> {
    if org.is_some_and(|value| !id(value)) || hint.is_some_and(|value| !slug(value)) {
        return Err(reject("Invalid hosted login request"));
    }
    let issuer = issuer.to_owned();
    // Create the credential dir (0700, owner check kept) before checking the
    // optional issuer pin; a fresh HOME may use the compiled hosted default.
    private_dir(dir, true)?;
    let _guard = lock(dir, true)?;
    require_login_issuer(dir, &issuer)?;
    let access = read_access_ingress(dir, &issuer)?;
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).map_err(|_| reject("Unable to create PKCE verifier"))?;
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    confirm_login_access_ingress(dir, &issuer, &access)?;
    let requested = scope.requested();
    let mut request = json!({"version":DEVICE_VERSION,
        "client_label":"cadence-cli","requested_capabilities":requested,
        "code_challenge":challenge,"cli_session":"renewable"});
    if let (Some(org), Some(hint)) = (org, hint) {
        request["organization_id"] = json!(org);
        request["audience"] = json!(format!("https://{hint}{CLOUD_SUFFIX}"));
    } else if org.is_none() {
        if let Some(hint) = hint {
            request["organization_slug"] = json!(hint);
        }
    }
    let (status, code) = runtime.post_public(
        &issuer,
        "/v1/hosted-cadence/device/code",
        request,
        access.as_ref(),
    )?;
    if status != 200 {
        return Err(reject("Hosted issuer refused device authorization"));
    }
    let (device, user, verification, mut interval) = device_code(&code)?;
    show_code(verification, user)?;
    let deadline = runtime.now() + Duration::from_secs(timestamp(&code, "expires_in")?);
    let expired = || reject("Hosted browser authorization expired; start again");
    let grant = loop {
        if runtime.now() >= deadline {
            return Err(expired());
        }
        runtime.wait(Duration::from_secs(interval));
        if runtime.now() >= deadline {
            return Err(expired());
        }
        confirm_login_access_ingress(dir, &issuer, &access)?;
        let (status, response) = runtime.post_public(
            &issuer,
            "/v1/hosted-cadence/device/token",
            json!({"device_code":device,"code_verifier":verifier}),
            access.as_ref(),
        )?;
        if runtime.now() >= deadline {
            return Err(expired());
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
    let grant = login_grant(&grant, org, hint, &issuer, &requested)?;
    confirm_login_access_ingress(dir, &issuer, &access)?;
    Ok(grant)
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
        ChildSource::Browser(_) => "/v1/hosted-cadence/enroll",
    };
    let access = match source {
        ChildSource::Service(_) => None,
        ChildSource::Browser(access) => access,
    };
    let enrollment = post_with_access(
        issuer,
        path,
        field(bridge, "access_token")?,
        json!({"version":VERSION,"organization_id":org,"audience":audience,
            "client_label":"Cadence local team", "agents":[{
                "client_agent_id":client_agent,"role":"implementer",
            "requested_capabilities":["results.submit"]}]}),
        access,
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
            ChildSource::Browser(_) => EnrollmentSource::Browser,
        },
        service_token: match source {
            ChildSource::Service(credential) => Some(credential.to_owned()),
            ChildSource::Browser(_) => None,
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
    #[test]
    fn submissions_responses_get_the_larger_read_bound() {
        assert_eq!(
            super::web_response_limit("/v1/web-wites/sites/by-slug/acc-site"),
            super::MAX_RESPONSE
        );
        assert_eq!(
            super::web_response_limit("/v1/web-sites/sites/site_abcd1234/submissions?limit=50"),
            super::MAX_SUBMISSIONS_RESPONSE
        );
        assert_eq!(
            super::web_response_limit("/v1/web-wites/sites/site_abcd1234/submissions?limit=50"),
            super::MAX_RESPONSE
        );
    }

    /// The production header validator: exact `issuer` hop, bounded UUID id,
    /// header names matched case-insensitively. Anything else yields no id.
    #[test]
    fn issuer_correlation_accepts_only_the_exact_issuer_hop_and_a_uuid() {
        let id = "0b2e1f4a-7c3d-4e5f-8a9b-1c2d3e4f5a6b";
        let good = super::correlation_from_header_values(Some("issuer"), Some(id)).unwrap();
        assert_eq!(good.request_id, id);
        assert!(super::correlation_from_header_values(Some("Issuer"), Some(id)).is_none());
        assert!(super::correlation_from_header_values(Some("issuer "), Some(id)).is_none());
        assert!(super::correlation_from_header_values(Some("board"), Some(id)).is_none());
        assert!(super::correlation_from_header_values(Some("issuer"), Some("0b2e1f4a")).is_none());
        assert!(super::correlation_from_header_values(
            Some("issuer"),
            Some("0b2e1f4a_7c3d-4e5f-8a9b-1c2d3e4f5a6b")
        )
        .is_none());
        assert!(super::correlation_from_header_values(
            Some("issuer"),
            Some("0b2e1f4a-7c3d-4e5f-8a9b-1c2d3e4f5a6g")
        )
        .is_none());
    }

    #[test]
    fn registration_handoff_keeps_the_consent_path_and_encodes_intent() {
        let url = super::registration_url(
            "https://app.agenticos.hk/device/hosted-cadence?code=K7PM-2QNF",
            Some("Acme Labs"),
            Some("acme-labs"),
        )
        .unwrap();
        assert_eq!(
            url,
            "https://app.agenticos.hk/register?after=%2Fdevice%2Fhosted-cadence%3Fcode%3DK7PM-2QNF&name=Acme%20Labs&slug=acme-labs"
        );
        assert_eq!(
            super::registration_url(
                "https://app.agenticos.hk/device/hosted-cadence?code=K7PM-2QNF",
                None,
                None,
            )
            .unwrap(),
            "https://app.agenticos.hk/register?after=%2Fdevice%2Fhosted-cadence%3Fcode%3DK7PM-2QNF"
        );
    }

    #[test]
    fn registration_handoff_refuses_non_https_and_fragments() {
        assert!(super::registration_url(
            "http://app.agenticos.hk/device/hosted-cadence?code=K7PM-2QNF",
            None,
            None,
        )
        .is_err());
        assert!(
            super::registration_url("https://app.agenticos.hk/device#fragment", None, None,)
                .is_err()
        );
        assert!(super::registration_url("https://app.agenticos.hk/", None, None).is_err());
    }

    #[test]
    fn register_portal_refuses_unsafe_intent_before_network() {
        let dir = std::path::Path::new("/nonexistent");
        let error = match super::register_portal(
            "https://api.example.com",
            Some("bad\nname"),
            None,
            dir,
            |_, _| Ok(()),
        ) {
            Ok(_) => panic!("unsafe workspace name was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), "Invalid workspace name");
        let error = match super::register_portal(
            "https://api.example.com",
            Some("Acme"),
            Some("Not_A_Slug"),
            dir,
            |_, _| Ok(()),
        ) {
            Ok(_) => panic!("unsafe workspace slug was accepted"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), "Invalid workspace slug");
    }

    use super::*;
    use crate::remote_result_outbox::{deliver_with, ResultCommand, ResultOutbox};
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread;

    const SERVICE: &str = "hcs_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const BRIDGE: &str = "hct_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
    const CHILD: &str = "hct_CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";
    const LINEAGE: &str = "hcl_00000000-0000-4000-8000-000000000001";
    /// Shared log of `(method, path, Authorization)` a loopback issuer records.
    type WebIssuerLog = std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>;
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
    fn access_file(dir: &Path, issuer: &str) {
        fs::write(
            dir.join(ACCESS_INGRESS),
            json!({"issuer":issuer,"client_id":"test-id.access","client_secret":"test-secret"})
                .to_string(),
        )
        .unwrap();
        fs::set_permissions(dir.join(ACCESS_INGRESS), fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn web_request_deadline_uses_caller_budget_only_for_valid_upload_file_puts() {
        let now = Instant::now();
        let caller = now + Duration::from_secs(60);
        let file_path = "/v1/web-sites/uploads/upl_12345678/files/assets%2Fjs%2Fapp.js";
        assert_eq!(web_request_deadline("PUT", file_path, now, caller), caller);
        assert_eq!(
            web_request_deadline(
                "POST",
                "/v1/web-sites/uploads/upl_12345678/files/assets%2Fapp.js",
                now,
                caller
            ),
            now + Duration::from_secs(20)
        );
        assert_eq!(
            web_request_deadline(
                "PUT",
                "/v1/web-sites/uploads/upl_12345678/finalize",
                now,
                caller
            ),
            now + Duration::from_secs(20)
        );
        assert_eq!(
            web_request_deadline("PUT", file_path, now, now + Duration::from_secs(5)),
            now + Duration::from_secs(5)
        );
        for invalid in [
            "/v1/web-sites/uploads/upl_short/files/assets%2Fapp.js",
            "/v1/web-sites/uploads/upl_12345678/files/assets/js/app.js",
        ] {
            assert_eq!(
                web_request_deadline("PUT", invalid, now, caller),
                now + Duration::from_secs(20)
            );
        }
    }

    #[test]
    fn access_ingress_file_is_opt_in_private_and_exactly_pinned() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        let issuer = "https://issuer.example.test";
        trust(&dir, issuer);
        assert!(read_access_ingress(&dir, issuer).unwrap().is_none());
        access_file(&dir, issuer);
        let pinned = read_access_ingress(&dir, issuer).unwrap();
        assert!(pinned.is_some());
        assert!(confirm_access_ingress(&dir, issuer, &pinned).is_ok());
        fs::set_permissions(dir.join(ACCESS_INGRESS), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_access_ingress(&dir, issuer).is_err());
        fs::set_permissions(dir.join(ACCESS_INGRESS), fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(dir.join(ACCESS_INGRESS),
            json!({"issuer":"https://other.example.test","client_id":"test-id.access","client_secret":"test-secret"}).to_string()).unwrap();
        assert!(read_access_ingress(&dir, issuer).is_err());
        access_file(&dir, issuer);
        fs::write(
            dir.join(ACCESS_INGRESS),
            json!({"issuer":issuer,"client_id":"test-id.access","client_secret":"changed"})
                .to_string(),
        )
        .unwrap();
        assert!(confirm_access_ingress(&dir, issuer, &pinned).is_err());
        fs::write(
            dir.join(ACCESS_INGRESS),
            json!({"issuer":issuer,"client_id":"test-id.access","client_secret":"bad\nsecret"})
                .to_string(),
        )
        .unwrap();
        assert!(read_access_ingress(&dir, issuer).is_err());
        fs::remove_file(dir.join(ACCESS_INGRESS)).unwrap();
        std::os::unix::fs::symlink("trusted-issuer", dir.join(ACCESS_INGRESS)).unwrap();
        assert!(read_access_ingress(&dir, issuer).is_err());
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
        access_file(&dir, &enrolled.issuer);
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
        request_with_audience_access(listener, path, bearer, expected_audience, false)
    }
    fn request_with_audience_access(
        listener: &TcpListener,
        path: &str,
        bearer: &str,
        expected_audience: &str,
        expect_access: bool,
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
        let mut access_id = None;
        let mut access_secret = None;
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
            if key.eq_ignore_ascii_case("cf-access-client-id") {
                access_id = Some(value.trim().to_owned());
            }
            if key.eq_ignore_ascii_case("cf-access-client-secret") {
                access_secret = Some(value.trim().to_owned());
            }
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
        assert_eq!(authorization, format!("Bearer {bearer}"));
        assert_eq!(
            access_id.as_deref(),
            expect_access.then_some("test-id.access")
        );
        assert_eq!(
            access_secret.as_deref(),
            expect_access.then_some("test-secret")
        );
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["organization_id"], "ws_real");
        assert_eq!(body["audience"], expected_audience);
        socket
    }
    fn public_request(listener: &TcpListener, path: &str) -> (std::net::TcpStream, Value) {
        public_request_access(listener, path, false)
    }
    fn public_request_access(
        listener: &TcpListener,
        path: &str,
        expect_access: bool,
    ) -> (std::net::TcpStream, Value) {
        let (socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), format!("POST {path} HTTP/1.1"));
        let mut length = 0;
        let mut access_id = None;
        let mut access_secret = None;
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
            if key.eq_ignore_ascii_case("cf-access-client-id") {
                access_id = Some(value.trim().to_owned());
            }
            if key.eq_ignore_ascii_case("cf-access-client-secret") {
                access_secret = Some(value.trim().to_owned());
            }
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            }
        }
        assert_eq!(
            access_id.as_deref(),
            expect_access.then_some("test-id.access")
        );
        assert_eq!(
            access_secret.as_deref(),
            expect_access.then_some("test-secret")
        );
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
    fn browser_continuity_bind_classifies_issuer_refusals_without_losing_pending_key() {
        for (status, code, reason, expected) in [
            ("401 Unauthorized", "lineage_terminal", "", "fresh consent"),
            ("409 Conflict", "parked", "session_expired", "fresh consent"),
            ("409 Conflict", "parked", "session_uncertain", "parked"),
            ("401 Unauthorized", "unauthorized", "", "authorization"),
            ("403 Forbidden", "forbidden", "", "authorization"),
            (
                "401 Unauthorized",
                "other_issuer_refusal",
                "",
                "authorization",
            ),
            ("403 Forbidden", "other_issuer_refusal", "", "authorization"),
            (
                "503 Service Unavailable",
                "authority_unavailable",
                "",
                "uncertain",
            ),
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
                let body = if reason.is_empty() {
                    json!({"ok":false,"code":code})
                } else {
                    json!({"ok":false,"code":code,"reason":reason})
                }
                .to_string();
                write!(
                    bind,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
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
            assert!(
                format!("{error}").contains(expected),
                "{status} {code}/{reason}: {error}"
            );
            server.join().unwrap();
            assert!(matches!(
                read_continuity_locked(&dir).unwrap().state,
                ContinuityState::Pending
            ));
            assert!(bound_browser(&dir).is_err());
        }
    }

    #[test]
    fn continuity_refusal_rejects_mismatched_or_forged_error_envelopes() {
        for (status, body) in [
            (401, json!({"ok":true,"code":"lineage_terminal"})),
            (
                401,
                json!({"ok":false,"code":"parked","reason":"session_expired"}),
            ),
            (409, json!({"ok":false,"code":"lineage_terminal"})),
            (403, json!({"ok":false,"code":"lineage_terminal"})),
            (401, json!({"ok":false,"code":"authority_unavailable"})),
            (401, json!({"ok":false,"code":"other refusal"})),
            (
                403,
                json!({"ok":false,"code":"other_issuer_refusal","reason":"session_expired"}),
            ),
            (409, json!({"ok":false,"code":"parked","reason":"other"})),
            (409, json!({"ok":false,"code":"parked","reason":null})),
            (
                503,
                json!({"ok":false,"code":"authority_unavailable","extra":true}),
            ),
            (503, json!({"ok":false,"code":"lineage_terminal"})),
        ] {
            assert!(matches!(
                continuity_refusal(status, &serde_json::to_vec(&body).unwrap()),
                ContinuityRefusal::Invalid
            ));
        }
        assert!(matches!(
            continuity_refusal(503, b"<html>upstream error</html>"),
            ContinuityRefusal::Invalid
        ));
        assert!(matches!(
            continuity_refusal(503, b""),
            ContinuityRefusal::Uncertain
        ));
    }

    #[test]
    fn continuity_post_refuses_redirect_without_forwarding_child_bearer() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let attacker = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let target = format!("http://{}/stolen", attacker.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut request, _) = continuity_request(&listener, "/continuity/challenge");
            write!(
                request,
                "HTTP/1.1 302 Found\r\nlocation: {target}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            )
            .unwrap();
        });
        let error =
            post_continuity(&issuer, "/continuity/challenge", CHILD, json!({})).unwrap_err();
        assert!(format!("{error}").contains("redirect"));
        server.join().unwrap();
        attacker.set_nonblocking(true).unwrap();
        assert!(
            attacker.accept().is_err(),
            "child bearer followed a redirect"
        );
    }

    #[test]
    fn continuity_post_bounds_errors_and_parks_lost_replies_as_uncertain() {
        let oversized = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", oversized.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut request, _) = continuity_request(&oversized, "/continuity/bind");
            let body = "X".repeat(MAX_RESPONSE as usize + 1);
            write!(
                request,
                "HTTP/1.1 409 Conflict\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            request.write_all(body.as_bytes()).unwrap();
        });
        let error = post_continuity(&issuer, "/continuity/bind", CHILD, json!({})).unwrap_err();
        assert!(format!("{error}").contains("Invalid"));
        server.join().unwrap();

        let lost = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", lost.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (request, _) = continuity_request(&lost, "/continuity/bind");
            drop(request);
        });
        let error = post_continuity(&issuer, "/continuity/bind", CHILD, json!({})).unwrap_err();
        assert!(format!("{error}").contains("uncertain"));
        server.join().unwrap();
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
            // Fail the private-file check before the Bound rename. The durable
            // Pending seed remains available for an exact-key retry.
            fs::set_permissions(
                changed_dir.join(CONTINUITY_RECORD),
                fs::Permissions::from_mode(0o644),
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
        let error = bind_browser(&dir).unwrap_err();
        assert!(format!("{error}").contains("uncertain"), "{error}");
        server.join().unwrap();
        assert!(bound_browser(&dir).is_err());
        fs::set_permissions(
            dir.join(CONTINUITY_RECORD),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(matches!(
            read_continuity_locked(&dir).unwrap().state,
            ContinuityState::Pending
        ));
        assert!(bound_browser(&dir).is_err());
    }

    #[test]
    fn browser_continuity_bind_metadata_recovers_only_the_same_pending_key() {
        if let Some(path) = std::env::var_os("CAD740_BIND_RETRY_DIR") {
            let dir = Path::new(&path);
            assert_eq!(bind_browser(dir).unwrap().lineage_id(), LINEAGE);
            assert_eq!(bound_browser(dir).unwrap().generation(), 1);
            return;
        }
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
        let mut restart = std::process::Command::new(std::env::current_exe().unwrap());
        restart
            .arg("browser_continuity_bind_metadata_recovers_only_the_same_pending_key")
            .env("CAD740_BIND_RETRY_DIR", &dir);
        let output = crate::reaper::output(&mut restart).unwrap();
        assert!(
            output.status.success(),
            "fresh process could not recover pending bind: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
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
    fn access_ingress_rejects_foreign_host_path_and_redirect_without_forwarding() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let attacker = TcpListener::bind("127.0.0.1:0").unwrap();
        attacker.set_nonblocking(true).unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let access = AccessIngress {
            issuer: issuer.clone(),
            client_id: "test-id.access".into(),
            client_secret: "test-secret".into(),
        };
        assert!(post_public_with_access(
            "https://other.example.test",
            "/v1/hosted-cadence/device/code",
            json!({}),
            Some(&access),
        )
        .is_err());
        assert!(post_public_with_access(
            &issuer,
            "/v1/hosted-cadence/service/exchange",
            json!({}),
            Some(&access),
        )
        .is_err());
        assert!(post_with_access(
            &issuer,
            "/v1/hosted-cadence/service/enroll",
            BRIDGE,
            json!({}),
            Some(&access),
        )
        .is_err());
        let location = format!("http://{}/stolen", attacker.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut socket, _) =
                public_request_access(&listener, "/v1/hosted-cadence/device/code", true);
            write!(socket, "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").unwrap();
        });
        let error = post_public_with_access(
            &issuer,
            "/v1/hosted-cadence/device/code",
            json!({}),
            Some(&access),
        )
        .unwrap_err();
        assert!(!format!("{error}").contains("test-secret"));
        server.join().unwrap();
        assert!(attacker.accept().is_err());
    }

    #[test]
    fn access_ingress_challenge_cannot_create_or_save_a_child() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut socket, _) =
                public_request_access(&listener, "/v1/hosted-cadence/device/code", true);
            write!(socket, "HTTP/1.1 403 Forbidden\r\ncontent-type: text/html\r\ncontent-length: 7\r\nconnection: close\r\n\r\ndenied!").unwrap();
            listener.set_nonblocking(true).unwrap();
            listener
        });
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        trust(&dir, &issuer);
        access_file(&dir, &issuer);
        let error = enroll_browser(
            &issuer,
            "ws_real",
            "https://real.board.example.test",
            "worker",
            &dir,
            |_, _| panic!("Access challenge returned a device code"),
        )
        .unwrap_err();
        assert!(!format!("{error}").contains("test-secret"));
        assert!(!dir.join(RECORD).exists());
        assert!(server.join().unwrap().accept().is_err());
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
            let (mut socket, body) =
                public_request_access(&listener, "/v1/hosted-cadence/device/code", true);
            assert_eq!(body["organization_id"], "ws_real");
            respond(&mut socket, &browser_code(60));
        });
        let (status, _) = post_public(
            &issuer,
            "/v1/hosted-cadence/device/code",
            json!({"organization_id":"ws_real"}),
        )
        .unwrap();
        assert_eq!(status, 200);
        let access = AccessIngress {
            issuer: issuer.clone(),
            client_id: "test-id.access".into(),
            client_secret: "test-secret".into(),
        };
        let (status, _) = post_public_with_access(
            &issuer,
            "/v1/hosted-cadence/device/code",
            json!({"organization_id":"ws_real"}),
            Some(&access),
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(status, 200);
    }

    /// Virtual clock over the real transport (CAD-1387): `now` is a
    /// monotonic baseline plus recorded virtual seconds, `wait` records
    /// the interval and advances virtual time by it, and `post_public`
    /// delegates to the exact production [`post_public_with_access`]
    /// ingress — real loopback HTTP, PKCE, grant and access checks all
    /// still run; only the incidental first-poll wait leaves wall time.
    struct VirtualWaitRealTransportRuntime {
        baseline: Instant,
        virtual_secs: u64,
        waits: Vec<u64>,
    }

    impl VirtualWaitRealTransportRuntime {
        fn new() -> Self {
            Self {
                baseline: Instant::now(),
                virtual_secs: 0,
                waits: Vec::new(),
            }
        }
    }

    impl LoginRuntime for VirtualWaitRealTransportRuntime {
        fn now(&mut self) -> Instant {
            self.baseline + Duration::from_secs(self.virtual_secs)
        }
        fn wait(&mut self, interval: Duration) {
            self.virtual_secs += interval.as_secs();
            self.waits.push(interval.as_secs());
        }
        fn post_public(
            &mut self,
            issuer: &str,
            path: &str,
            body: Value,
            access: Option<&AccessIngress>,
        ) -> Result<(u16, Value)> {
            post_public_with_access(issuer, path, body, access)
        }
    }

    /// Scripted enrollment issuer on the same LoginRuntime seam the
    /// login tests use (CAD-1341): virtual monotonic clock records
    /// every wait, `post_public` replays the token-error script, so
    /// poll backoff/deadline behavior is exact and wall-free.
    struct ScriptedEnroll {
        issuer: String,
        token_errors: Vec<&'static str>,
        /// Fixed baseline captured at construction; `now` is
        /// `baseline + virtual_secs` — wall time never advances it.
        baseline: Instant,
        virtual_secs: u64,
        /// Every wait interval observed, in order.
        waits: Vec<u64>,
        /// Requests received, in order (path only).
        requests: Vec<String>,
        finished: bool,
    }

    impl ScriptedEnroll {
        fn new(issuer: &str, token_errors: Vec<&'static str>) -> Self {
            Self {
                issuer: issuer.to_string(),
                token_errors,
                baseline: Instant::now(),
                virtual_secs: 0,
                waits: Vec::new(),
                requests: Vec::new(),
                finished: false,
            }
        }
    }

    impl LoginRuntime for ScriptedEnroll {
        fn now(&mut self) -> Instant {
            self.baseline + Duration::from_secs(self.virtual_secs)
        }
        fn wait(&mut self, interval: Duration) {
            self.virtual_secs += interval.as_secs();
            self.waits.push(interval.as_secs());
        }
        fn post_public(
            &mut self,
            issuer: &str,
            path: &str,
            body: Value,
            access: Option<&AccessIngress>,
        ) -> Result<(u16, Value)> {
            assert_eq!(issuer, self.issuer);
            assert!(access.is_none());
            assert!(!self.finished, "request after terminal response");
            self.requests.push(path.to_string());
            if path.ends_with("/device/code") {
                assert_eq!(path, "/v1/hosted-cadence/device/code");
                assert_eq!(body["version"], DEVICE_VERSION);
                assert_eq!(body["organization_id"], "ws_real");
                assert_eq!(body["audience"], "https://real.board.example.test");
                assert_eq!(
                    body["requested_capabilities"],
                    json!(["bridge.enroll", "results.submit"])
                );
                assert_eq!(body["code_challenge"].as_str().unwrap().len(), 43);
                return Ok((200, browser_code(16)));
            }
            assert_eq!(path, "/v1/hosted-cadence/device/token");
            assert_eq!(body["device_code"], format!("hcd_{}", "A".repeat(43)));
            assert_eq!(body["code_verifier"].as_str().unwrap().len(), 43);
            let error = self.token_errors.remove(0);
            self.finished = matches!(error, "access_denied" | "expired_token");
            Ok((400, json!({"error": error})))
        }
    }

    /// Drive `enroll_browser_with` against a bound loopback listener
    /// as `issuer`: the scripted runtime answers every poll (it never
    /// connects), while any request that escapes the seam — the
    /// post-grant `enroll_child_locked` uses the real
    /// `post_with_access` transport — lands on the real listener
    /// instead of the open network.
    fn run_enroll(
        listener: &TcpListener,
        token_errors: Vec<&'static str>,
    ) -> (
        Result<EnrollmentInfo>,
        PathBuf,
        tempfile::TempDir,
        ScriptedEnroll,
    ) {
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        trust(&dir, &issuer);
        let mut script = ScriptedEnroll::new(&issuer, token_errors);
        let out = enroll_browser_with(
            &issuer,
            "ws_real",
            "https://real.board.example.test",
            "worker",
            &dir,
            |_, _| Ok(()),
            &mut script,
        );
        (out, dir, root, script)
    }

    /// Nonblocking proof that nothing escaped the scripted seam to the
    /// real issuer listener — the original tests' terminal/deadline
    /// "no further request" evidence, kept on the real socket.
    fn assert_no_request(listener: &TcpListener) {
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "a request escaped the scripted runtime to the real issuer"
        );
    }

    /// Same scenario as the retired wall-clock version: expires_in 16,
    /// pending then slow_down. Pending keeps the 5s interval,
    /// slow_down backs it off +5 → 10; the third wait lands on the
    /// deadline so no third token request is made — and the real
    /// loopback issuer sees nothing at all.
    #[test]
    fn browser_poll_pending_slow_down_obeys_deadline_without_child() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let (out, dir, _root, script) =
            run_enroll(&listener, vec!["authorization_pending", "slow_down"]);
        let err = out.unwrap_err();
        assert!(format!("{err}").contains("expired"), "{err}");
        assert_eq!(script.waits, [5, 5, 10], "{:?}", script.waits);
        assert_eq!(script.virtual_secs, 20);
        assert_eq!(
            script.requests,
            [
                "/v1/hosted-cadence/device/code",
                "/v1/hosted-cadence/device/token",
                "/v1/hosted-cadence/device/token",
            ],
            "deadline allowed another token request"
        );
        assert_no_request(&listener);
        assert!(!dir.join(RECORD).exists());
    }

    /// Denial and expiry are terminal on the first token response —
    /// the poll stops, the error class survives, no request on the
    /// real issuer, no record.
    #[test]
    fn browser_poll_terminal_denial_and_expiry_never_create_child() {
        for error in ["access_denied", "expired_token"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let (out, dir, _root, script) = run_enroll(&listener, vec![error]);
            let err = out.unwrap_err();
            assert!(format!("{err}").contains(if error == "access_denied" {
                "denied"
            } else {
                "expired"
            }));
            assert_eq!(script.waits, [5]);
            assert_eq!(
                script.requests,
                [
                    "/v1/hosted-cadence/device/code",
                    "/v1/hosted-cadence/device/token"
                ],
                "terminal response allowed further polling"
            );
            assert_no_request(&listener);
            assert!(!dir.join(RECORD).exists());
        }
    }

    // --- CAD-1019 slice 1b: `cadence login` device grant ---

    const LOGIN_ENDPOINT: &str = "https://acme.cadencecloud.app";

    /// The AgenticOS device/code request contract (`requestShape` in
    /// apps/api/src/hosted-cadence/device.ts): these keys plus explicit
    /// renewable-session opt-in, an exact HTTPS audience, and PKCE.
    fn real_code_request_ok(body: &Value) -> bool {
        let keys = [
            "version",
            "organization_id",
            "audience",
            "client_label",
            "requested_capabilities",
            "code_challenge",
            "cli_session",
        ];
        body.as_object().is_some_and(|o| {
            o.len() == keys.len()
                && keys.iter().all(|k| o.contains_key(*k))
                && body["cli_session"] == "renewable"
                && body["version"] == DEVICE_VERSION
                && body["audience"]
                    .as_str()
                    .is_some_and(|a| origin(a, false).is_ok())
                && body["client_label"]
                    .as_str()
                    .is_some_and(|l| (1..=120).contains(&l.len()))
                && body["code_challenge"]
                    .as_str()
                    .is_some_and(|c| c.len() == 43)
        })
    }

    /// The real token response (`hostedCadenceExchangeResponseFor`): exactly
    /// these fields and no `organization_slug`.
    fn login_grant_body() -> Value {
        let at = now().unwrap();
        json!({"version":VERSION,"organization_id":"ws_real","audience":LOGIN_ENDPOINT,
            "principal":{"kind":"user","subject_id":"user_1","current_role":"owner"},
            "capabilities":["cli.read","cli.write"],
            "credential":{"credential_id":"hcp_credential","access_token":BRIDGE,
                "token_type":"Bearer","issued_at":at,"expires_at":at+120,"renewal":"reexchange"},
            "cli_session":{"session_id":"hcs_00000000-0000-4000-8000-000000000001",
                "refresh_token":format!("hcr_{}", "R".repeat(43)),"generation":1,
                "device_label":"cadence-cli","idle_expires_at":at+2592000,"absolute_expires_at":at+7776000}})
    }

    /// A scripted in-memory login issuer (CAD-1124): the same request
    /// contract the loopback fixture enforced — device/code first, then
    /// one device/token poll per `token_errors`, then the grant — plus a
    /// virtual monotonic clock `wait` advances and records, so pending
    /// and slow_down scheduling stays observable. No TCP and no wall
    /// sleeps; every trust/PKCE/access/grant check in the real
    /// implementation still runs.
    struct ScriptedLogin {
        grant: Option<Value>,
        token_errors: Vec<&'static str>,
        finished: bool,
        /// Fixed baseline captured at construction; `now` is
        /// `baseline + virtual_secs` — wall time never advances the
        /// virtual clock.
        baseline: Instant,
        /// Virtual seconds elapsed since the runtime was created.
        virtual_secs: u64,
        /// Every `wait` interval observed, in order.
        waits: Vec<u64>,
        /// Requests received, in order — checked for exact script
        /// consumption at the end of `run_login`.
        requests: Vec<String>,
    }

    impl ScriptedLogin {
        fn new(grant: Value, token_errors: Vec<&'static str>) -> Self {
            let terminal = token_errors
                .iter()
                .position(|error| matches!(*error, "access_denied" | "expired_token"));
            if let Some(index) = terminal {
                assert_eq!(
                    index + 1,
                    token_errors.len(),
                    "response after terminal error"
                );
            }
            Self {
                grant: terminal.is_none().then_some(grant),
                token_errors,
                finished: false,
                baseline: Instant::now(),
                virtual_secs: 0,
                waits: Vec::new(),
                requests: Vec::new(),
            }
        }
    }

    impl LoginRuntime for ScriptedLogin {
        fn now(&mut self) -> Instant {
            self.baseline + Duration::from_secs(self.virtual_secs)
        }
        fn wait(&mut self, interval: Duration) {
            self.virtual_secs += interval.as_secs();
            self.waits.push(interval.as_secs());
        }
        fn post_public(
            &mut self,
            issuer: &str,
            path: &str,
            body: Value,
            access: Option<&AccessIngress>,
        ) -> Result<(u16, Value)> {
            assert_eq!(issuer, "https://issuer.scripted.test");
            assert!(access.is_none(), "login script wrote no ingress");
            assert!(!self.finished, "request after terminal login response");
            self.requests.push(path.to_string());
            if self.requests.len() == 1 {
                assert_eq!(path, "/v1/hosted-cadence/device/code");
                if !real_code_request_ok(&body)
                    || body["organization_id"] != "ws_real"
                    || body["audience"] != LOGIN_ENDPOINT
                    || body["requested_capabilities"]
                        != json!(["cli.read", "cli.write", "wiki.read", "wiki.write"])
                {
                    return Ok((400, json!({"error":"invalid_request"})));
                }
                return Ok((200, browser_code(60)));
            }
            assert_eq!(
                path, "/v1/hosted-cadence/device/token",
                "unexpected request order"
            );
            assert_eq!(body["device_code"], format!("hcd_{}", "A".repeat(43)));
            assert_eq!(body["code_verifier"].as_str().unwrap().len(), 43);
            if let Some(error) = self.token_errors.first().copied() {
                self.token_errors.remove(0);
                self.finished = matches!(error, "access_denied" | "expired_token");
                return Ok((400, json!({"error":error})));
            }
            let grant = self.grant.take().expect("unexpected token request");
            self.finished = true;
            Ok((200, grant))
        }
    }

    /// Drive `login_browser` for slug `acme` through the scripted
    /// runtime: the issuer answers any `token_errors` in order, then
    /// `grant`. Returns the outcome plus the recorded waits and request
    /// order so pending/slow_down backoff stays observable.
    fn run_login(
        grant: Value,
        token_errors: Vec<&'static str>,
    ) -> (
        Result<LoginGrant>,
        PathBuf,
        tempfile::TempDir,
        ScriptedLogin,
    ) {
        let issuer = "https://issuer.scripted.test";
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        trust(&dir, issuer);
        let mut script = ScriptedLogin::new(grant, token_errors);
        let expected_requests = 1 + script.token_errors.len() + usize::from(script.grant.is_some());
        let out = login_browser_with(issuer, "ws_real", "acme", &dir, |_, _| Ok(()), &mut script);
        assert!(
            script.finished,
            "login never consumed its terminal response"
        );
        assert!(script.grant.is_none(), "login never consumed its grant");
        assert!(script.token_errors.is_empty(), "unconsumed login errors");
        assert_eq!(
            script.requests.len(),
            expected_requests,
            "unexpected poll count"
        );
        (out, dir, root, script)
    }

    fn assert_nothing_stored(dir: &Path) {
        assert!(!dir.join("cli-acme.json").exists());
        assert!(!dir.join(RECORD).exists());
    }

    /// Independent AOS-198 acceptance runtime for the unbound portal
    /// contract, with and without an organization slug hint. It rejects any
    /// other device/code request shape and returns the authoritative grant.
    struct PortalAcceptanceRuntime {
        issuer: String,
        grant: Option<Value>,
        requests: Vec<(String, Value)>,
        baseline: Instant,
        elapsed: u64,
        /// The device-code `requested_capabilities` this run must send.
        requested: Vec<&'static str>,
    }

    impl LoginRuntime for PortalAcceptanceRuntime {
        fn now(&mut self) -> Instant {
            self.baseline + Duration::from_secs(self.elapsed)
        }
        fn wait(&mut self, interval: Duration) {
            self.elapsed += interval.as_secs();
        }
        fn post_public(
            &mut self,
            issuer: &str,
            path: &str,
            body: Value,
            access: Option<&AccessIngress>,
        ) -> Result<(u16, Value)> {
            assert_eq!(issuer, self.issuer);
            assert!(access.is_none());
            self.requests.push((path.to_string(), body.clone()));
            if path.ends_with("/device/code") {
                assert_eq!(path, "/v1/hosted-cadence/device/code");
                let keys = [
                    "version",
                    "client_label",
                    "requested_capabilities",
                    "code_challenge",
                    "cli_session",
                ];
                let has_slug_hint = body.get("organization_slug").is_some();
                let request = body.as_object().unwrap();
                assert_eq!(request.len(), keys.len() + usize::from(has_slug_hint));
                assert_eq!(body["cli_session"], "renewable");
                assert!(keys.iter().all(|key| request.contains_key(*key)));
                assert_eq!(body["version"], DEVICE_VERSION);
                assert_eq!(body["client_label"], "cadence-cli");
                assert_eq!(body["requested_capabilities"], json!(self.requested));
                assert!(body.get("organization_id").is_none());
                assert!(body.get("audience").is_none());
                if has_slug_hint {
                    assert_eq!(body["organization_slug"], "acme");
                }
                return Ok((200, browser_code(60)));
            }
            assert_eq!(path, "/v1/hosted-cadence/device/token");
            assert!(body["device_code"].is_string());
            assert!(body["code_verifier"].is_string());
            Ok((200, self.grant.take().expect("one portal grant")))
        }
    }

    fn run_wiki_portal_acceptance(
        grant: Value,
        dir: &Path,
    ) -> (Result<LoginGrant>, PortalAcceptanceRuntime) {
        let mut runtime = PortalAcceptanceRuntime {
            issuer: HOSTED_LOGIN_ISSUER.to_string(),
            grant: Some(grant),
            requests: Vec::new(),
            baseline: Instant::now(),
            elapsed: 0,
            requested: vec!["cli.read", "cli.write", "wiki.read", "wiki.write"],
        };
        let out = login_device_flow(
            HOSTED_LOGIN_ISSUER,
            None,
            Some("acme"),
            dir,
            |_, _| Ok(()),
            &mut runtime,
            LoginScope::Wiki,
        );
        (out, runtime)
    }

    fn run_portal_acceptance(
        hint: Option<&str>,
        grant: Value,
        dir: &Path,
    ) -> (Result<LoginGrant>, PortalAcceptanceRuntime) {
        let mut runtime = PortalAcceptanceRuntime {
            issuer: HOSTED_LOGIN_ISSUER.to_string(),
            grant: Some(grant),
            requests: Vec::new(),
            baseline: Instant::now(),
            elapsed: 0,
            requested: vec!["cli.read", "cli.write", "wiki.read", "wiki.write"],
        };
        let out = login_device_flow(
            HOSTED_LOGIN_ISSUER,
            None,
            hint,
            dir,
            |_, _| Ok(()),
            &mut runtime,
            LoginScope::Wiki,
        );
        (out, runtime)
    }

    #[test]
    fn hosted_portal_acceptance_binds_only_the_issuer_selected_workspace() {
        // A fresh HOME has no manually installed issuer pin. Bare login sends
        // an unbound request; the issuer's returned workspace identity is the
        // only source for the resulting endpoint and slug.
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("fresh-home/auth");
        let mut grant = login_grant_body();
        grant["organization_slug"] = json!("acme");
        let (out, runtime) = run_portal_acceptance(None, grant, &dir);
        let grant = out.unwrap();
        assert_eq!(
            (
                grant.organization_id.as_str(),
                grant.slug.as_str(),
                grant.endpoint.as_str()
            ),
            ("ws_real", "acme", LOGIN_ENDPOINT)
        );
        assert_eq!(runtime.requests.len(), 2);
        assert!(!dir.join(TRUSTED_ISSUER).exists());
        assert_nothing_stored(&dir);

        // A positional slug is an unbound selector hint, never a locally
        // pinned organization id or audience.
        let mut hinted_grant = login_grant_body();
        hinted_grant["organization_slug"] = json!("acme");
        let (out, runtime) = run_portal_acceptance(Some("acme"), hinted_grant, &dir);
        assert_eq!(out.unwrap().slug, "acme");
        assert_eq!(runtime.requests.len(), 2);
        assert!(runtime.requests[0].1.get("organization_id").is_none());
        assert!(runtime.requests[0].1.get("audience").is_none());
        assert_eq!(runtime.requests[0].1["organization_slug"], "acme");

        // A positional slug is a selector hint only: a different server
        // selection, even with otherwise valid identity/audience, is refused.
        let mut mismatched_hint = login_grant_body();
        mismatched_hint["organization_slug"] = json!("other");
        mismatched_hint["audience"] = json!("https://other.cadencecloud.app");
        let (out, runtime) = run_portal_acceptance(Some("acme"), mismatched_hint, &dir);
        assert!(out.is_err());
        assert_eq!(runtime.requests.len(), 2);
        assert_nothing_stored(&dir);

        // A grant's audience must be canonical for its server-returned slug;
        // port/path/lookalike variants are not registrations.
        for audience in [
            "https://acme.cadencecloud.app:444",
            "https://acme.cadencecloud.app/",
            "https://acme.cadencecloud.app.evil.test",
        ] {
            let mut wrong_audience = login_grant_body();
            wrong_audience["organization_slug"] = json!("acme");
            wrong_audience["audience"] = json!(audience);
            let (out, _) = run_portal_acceptance(None, wrong_audience, &dir);
            assert!(out.is_err(), "{audience}");
            assert_nothing_stored(&dir);
        }
    }

    #[test]
    fn register_portal_flows_through_the_hosted_device_grant() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("fresh-home/auth");
        let mut grant = login_grant_body();
        grant["organization_slug"] = json!("acme");
        let mut runtime = PortalAcceptanceRuntime {
            issuer: HOSTED_LOGIN_ISSUER.to_string(),
            grant: Some(grant),
            requests: Vec::new(),
            baseline: Instant::now(),
            elapsed: 0,
            requested: vec!["cli.read", "cli.write", "wiki.read", "wiki.write"],
        };
        let mut shown = None;
        let out = register_portal_with(
            HOSTED_LOGIN_ISSUER,
            Some("Acme Labs"),
            Some("acme"),
            &dir,
            |url, code| {
                shown = Some((url.to_owned(), code.to_owned()));
                Ok(())
            },
            &mut runtime,
        );
        let grant = out.unwrap();
        assert_eq!(grant.slug, "acme");
        assert_eq!(grant.endpoint, LOGIN_ENDPOINT);
        let (url, code) = shown.unwrap();
        assert_eq!(
            url,
            "https://app.agenticos.test/register?after=%2Fdevice%2Fhosted-cadence%3Fcode%3DK7PM-2QNF&name=Acme%20Labs&slug=acme"
        );
        assert_eq!(code, "K7PM-2QNF");
        assert!(runtime.requests[0].1.get("organization_slug").is_none());
        assert_eq!(runtime.requests.len(), 2);
        assert_nothing_stored(&dir);
    }

    #[test]
    fn hosted_portal_requires_exact_requested_capabilities_before_storage() {
        // The issuer may return the two requested capabilities in either
        // order, but an additional CLI capability is outside the consent.
        let root = tempfile::tempdir().unwrap();
        let mut reordered = login_grant_body();
        reordered["organization_slug"] = json!("acme");
        reordered["capabilities"] = json!(["cli.write", "cli.read"]);
        let dir = root.path().join("reordered/auth");
        let (out, runtime) = run_portal_acceptance(None, reordered, &dir);
        assert!(
            out.is_ok(),
            "reordering requested capabilities was rejected"
        );
        assert_eq!(runtime.requests.len(), 2);
        assert_nothing_stored(&dir);

        for (case, capabilities) in [
            ("extra-admin", json!(["cli.read", "cli.write", "cli.admin"])),
            ("missing-write", json!(["cli.read"])),
            (
                "duplicate-read",
                json!(["cli.read", "cli.read", "cli.write"]),
            ),
        ] {
            let mut grant = login_grant_body();
            grant["organization_slug"] = json!("acme");
            grant["capabilities"] = capabilities;
            let dir = root.path().join(format!("{case}/auth"));
            let (out, runtime) = run_portal_acceptance(None, grant, &dir);
            assert!(out.is_err(), "issuer capability set {case} was accepted");
            assert_eq!(runtime.requests.len(), 2);
            assert_nothing_stored(&dir);
        }
    }

    #[test]
    fn hosted_portal_accepts_a_narrowed_wiki_consent_and_refuses_escalation() {
        // CAD-1401 blocker 2: an issuer may grant a subset of the requested
        // scopes (the CLI pair plus wiki.read for a portal login). The
        // grant must keep both CLI scopes, may only name requested scopes,
        // and never repeats one.
        let root = tempfile::tempdir().unwrap();
        let mut narrowed = login_grant_body();
        narrowed["organization_slug"] = json!("acme");
        narrowed["capabilities"] = json!(["cli.read", "cli.write", "wiki.read"]);
        let dir = root.path().join("narrowed/auth");
        let (out, runtime) = run_wiki_portal_acceptance(narrowed, &dir);
        let grant = out.expect("a narrowed wiki consent was refused");
        assert_eq!(runtime.requests.len(), 2);
        assert!(grant.capabilities.iter().any(|c| c == "wiki.read"));
        assert!(!grant.capabilities.iter().any(|c| c == "wiki.write"));

        for (case, capabilities) in [
            ("missing-cli-write", json!(["cli.read", "wiki.read"])),
            (
                "unrequested-admin",
                json!(["cli.read", "cli.write", "cli.admin"]),
            ),
            (
                "duplicate-wiki",
                json!(["cli.read", "cli.write", "wiki.read", "wiki.read"]),
            ),
        ] {
            let mut grant = login_grant_body();
            grant["organization_slug"] = json!("acme");
            grant["capabilities"] = capabilities;
            let dir = root.path().join(format!("{case}/auth"));
            let (out, runtime) = run_wiki_portal_acceptance(grant, &dir);
            assert!(out.is_err(), "wiki consent case {case} was accepted");
            assert_eq!(runtime.requests.len(), 2);
            assert_nothing_stored(&dir);
        }
    }

    /// CAD-1429: `--web` consent. The request names exactly the CLI pair plus
    /// the Publish Web pair; a grant that drops a requested web scope, adds an
    /// unrequested one, or repeats one is refused and stores nothing.
    /// A loopback issuer that records every request and answers with `reply`.
    /// Returns its origin and the shared log of `(method, path, Authorization)`.
    fn web_issuer_server(reply: &'static str) -> (String, WebIssuerLog) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&log);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut parts = line.split(' ');
                let method = parts.next().unwrap_or_default().to_string();
                let path = parts.next().unwrap_or_default().to_string();
                let mut auth = String::new();
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" || header.is_empty() {
                        break;
                    }
                    if let Some(v) = header.to_ascii_lowercase().strip_prefix("authorization:") {
                        auth = v.trim().to_string();
                    }
                }
                seen.lock().unwrap().push((method, path, auth));
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        (origin, log)
    }

    /// CAD-1429: a board audience and a public issuer are different origins.
    /// Web management must reach the saved issuer and never the board audience,
    /// whose host answers `/v1` with the container catch-all.
    #[test]
    fn web_management_goes_to_the_saved_issuer_never_the_board_audience() {
        let (issuer, issuer_log) = web_issuer_server(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}",
        );
        let (board, board_log) = web_issuer_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        );
        assert_ne!(issuer, board, "the fixture must use distinct origins");
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("web/auth");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(dir.join(TRUSTED_ISSUER), format!("{issuer}\n")).unwrap();
        fs::set_permissions(dir.join(TRUSTED_ISSUER), fs::Permissions::from_mode(0o600)).unwrap();
        // The board audience is only ever a stored field, never a web destination.
        let _ = &board;
        let (status, body) = web_issuer_request(
            &dir,
            &issuer,
            WebRequest {
                method: "GET",
                path: "/v1/web-sites/sites/by-slug/my-site",
                bearer: "hct_BEARERTOKENFORTESTONLYxxxxxxxxxxxxxxxxxxxxxxx",
                headers: &[],
                body: None,
            },
            Instant::now() + Duration::from_secs(10),
        )
        .expect("web request to the issuer failed");
        assert_eq!(status, 200);
        assert!(body.is_some());
        let issuer_seen = issuer_log.lock().unwrap().clone();
        assert_eq!(
            issuer_seen.len(),
            1,
            "exactly one request reached the issuer"
        );
        assert_eq!(issuer_seen[0].1, "/v1/web-sites/sites/by-slug/my-site");
        // The header name is case-insensitive and the scheme is too; the token is exact.
        assert!(
            issuer_seen[0]
                .2
                .to_ascii_lowercase()
                .starts_with("bearer hct_"),
            "auth header was {}",
            issuer_seen[0].2
        );
        assert!(
            board_log.lock().unwrap().is_empty(),
            "a board/container audience received a web request"
        );
    }

    /// Only the exact `/v1/web-sites` namespace is sent: no other `/v1` route,
    /// no traversal, no query, no encoded separator. Each is refused before any
    /// request reaches the issuer.
    #[test]
    fn web_paths_are_confined_to_the_web_namespace() {
        let (issuer, log) = web_issuer_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        );
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("ns/auth");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(dir.join(TRUSTED_ISSUER), format!("{issuer}\n")).unwrap();
        fs::set_permissions(dir.join(TRUSTED_ISSUER), fs::Permissions::from_mode(0o600)).unwrap();
        for path in [
            "/v1/hosted-cadence/cli-session/revoke",
            "/v1/web-sites/../hosted-cadence/x",
            "/v1/web-sites/a?x=1",
            "/v1/web-sites/a%2Fb",
            "/v1/web-sites/a%5Cb",
            "/api/cli/status",
        ] {
            let out = web_issuer_request(
                &dir,
                &issuer,
                WebRequest {
                    method: "GET",
                    path,
                    bearer: "hct_BEARERTOKENFORTESTONLYxxxxxxxxxxxxxxxxxxxxxxx",
                    headers: &[],
                    body: None,
                },
                Instant::now() + Duration::from_secs(10),
            );
            assert!(out.is_err(), "path {path} was accepted");
        }
        assert!(
            log.lock().unwrap().is_empty(),
            "a refused path reached the issuer"
        );
    }

    /// CAD-1429: the upload-file route is the one place an encoded `/` is valid.
    /// Built React/Vite assets are nested (`assets/app-abc123.js`, `assets/css/
    /// site.css`), so their canonical `:encodedPath` must pass the transport
    /// guard, and nothing else that looks like it may.
    #[test]
    fn upload_file_route_admits_canonical_nested_artifact_paths_only() {
        let upload = "/v1/web-sites/uploads/upl_22222222/files/";
        // Positive: the canonical encodings the client sends for built assets.
        for ok in [
            "index.html",
            "assets%2Fapp-abc123.js",
            "assets%2Fcss%2Fsite.css",
        ] {
            assert!(
                web_management_path(&format!("{upload}{ok}")),
                "{ok} refused"
            );
        }
        // Negative: encoded traversal, double encoding, malformed or non-canonical
        // escapes, raw separators, backslash, query and fragment.
        for bad in [
            "..%2Fsecret.js",
            "assets%2F..%2Fsecret.js",
            "assets%252Fapp.js",
            "assets%2fapp.js",
            "assets%2",
            "assets%ZZapp.js",
            "assets/app.js",
            "assets%5Capp.js",
            "app.js%3Fx%3D1",
            "app.js?x=1",
            "app.js#frag",
            ".env",
            "assets%2F.env",
            "node_modules%2Fx.js",
            "app.js.map",
        ] {
            assert!(
                !web_management_path(&format!("{upload}{bad}")),
                "{bad} accepted"
            );
        }
        // Outside the upload-file shape, any encoded separator stays refused.
        for bad in [
            "/v1/web-sites/sites/by-slug/a%2Fb",
            "/v1/web-sites/sites/site_11111111/releases%2F..",
            "/v1/web-sites/uploads/upl_22222222/finalize%2F",
            "/v1/web-sites/uploads/upl_bad/files/assets%2Fapp.js",
        ] {
            assert!(!web_management_path(bad), "{bad} accepted");
        }
    }

    /// CAD-1659: reading form submissions is the one route that carries a
    /// query. Only the published keys, in a fixed order, with the published
    /// grammars pass; everything else keeps the no-query rule.
    #[test]
    fn submissions_route_admits_only_the_published_query() {
        let route = "/v1/web-sites/sites/site_0bcef7ac757243108b1e65ca18ca3517/submissions";
        for ok in [
            "?limit=50",
            "?limit=1",
            "?limit=100&form=contact",
            "?limit=10&form=contact&cursor=1730000000000:wsub_abcdefgh",
            "?limit=10&cursor=1730000000000:wsub_abcdefgh",
        ] {
            assert!(web_management_path(&format!("{route}{ok}")), "{ok} refused");
        }
        for bad in [
            "?limit=0",
            "?limit=101",
            "?limit=",
            "?limit=10&limit=20",
            "?limit=10&form=Contact",
            "?limit=10&form=",
            "?limit=10&form=a%2Fb",
            "?limit=10&form=contact&extra=1",
            "?limit=10&cursor=abc",
            "?limit=10&cursor=1730000000000:wsub_abc",
            "?limit=10&cursor=1730000000000:wsub_abcdefgh&limit=2",
            "?limit=10#frag",
            "?form=contact",
        ] {
            assert!(
                !web_management_path(&format!("{route}{bad}")),
                "{bad} accepted"
            );
        }
        // Only the submissions route may carry a query at all.
        for bad in [
            "/v1/web-sites/sites/by-slug/a?limit=10",
            "/v1/web-sites/sites/site_x/submissions?limit=10",
            "/v1/web-sites/sites/site_aaaaaaaa/releases?limit=10",
            "/v1/web-sites/sites/site_aaaa%2Fbbbb/submissions?limit=10",
        ] {
            assert!(!web_management_path(bad), "{bad} accepted");
        }
    }

    /// A redirect from the issuer is a refusal, never followed.
    #[test]
    fn web_issuer_redirects_are_refused() {
        let (issuer, _log) = web_issuer_server(
            "HTTP/1.1 302 Found\r\nLocation: https://evil.example.test/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("redir/auth");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(dir.join(TRUSTED_ISSUER), format!("{issuer}\n")).unwrap();
        fs::set_permissions(dir.join(TRUSTED_ISSUER), fs::Permissions::from_mode(0o600)).unwrap();
        let err = web_issuer_request(
            &dir,
            &issuer,
            WebRequest {
                method: "GET",
                path: "/v1/web-sites/sites/by-slug/my-site",
                bearer: "hct_BEARERTOKENFORTESTONLYxxxxxxxxxxxxxxxxxxxxxxx",
                headers: &[],
                body: None,
            },
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap_err();
        assert!(err.to_string().contains("redirect refused"), "{err}");
    }

    /// No trusted-issuer pin: the bearer is never sent.
    #[test]
    fn web_issuer_without_a_pin_sends_no_bearer() {
        let (issuer, log) = web_issuer_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        );
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("nopin/auth");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let out = web_issuer_request(
            &dir,
            &issuer,
            WebRequest {
                method: "GET",
                path: "/v1/web-sites/sites/by-slug/my-site",
                bearer: "hct_BEARERTOKENFORTESTONLYxxxxxxxxxxxxxxxxxxxxxxx",
                headers: &[],
                body: None,
            },
            Instant::now() + Duration::from_secs(10),
        );
        assert!(out.is_err());
        assert!(
            log.lock().unwrap().is_empty(),
            "a bearer reached an unpinned issuer"
        );
    }

    #[test]
    fn hosted_web_consent_requests_only_web_scopes_it_was_asked_for() {
        let root = tempfile::tempdir().unwrap();
        let mut runtime = PortalAcceptanceRuntime {
            issuer: HOSTED_LOGIN_ISSUER.to_string(),
            grant: None,
            requests: Vec::new(),
            baseline: Instant::now(),
            elapsed: 0,
            requested: vec!["cli.read", "cli.write", "web.read", "web.prepare"],
        };
        let mut grant = login_grant_body();
        grant["organization_slug"] = json!("acme");
        grant["capabilities"] = json!(["cli.read", "cli.write", "web.read", "web.prepare"]);
        runtime.grant = Some(grant.clone());
        let dir = root.path().join("web-ok/auth");
        let out = login_device_flow(
            HOSTED_LOGIN_ISSUER,
            None,
            Some("acme"),
            &dir,
            |_, _| Ok(()),
            &mut runtime,
            LoginScope::Web,
        );
        let granted = out.expect("a full web consent was refused");
        assert!(granted.capabilities.iter().any(|c| c == "web.prepare"));
        assert_eq!(
            runtime.requests[0].1["requested_capabilities"],
            json!(["cli.read", "cli.write", "web.read", "web.prepare"])
        );

        for (case, capabilities) in [
            (
                "dropped-web-prepare",
                json!(["cli.read", "cli.write", "web.read"]),
            ),
            (
                "unrequested-wiki",
                json!([
                    "cli.read",
                    "cli.write",
                    "web.read",
                    "web.prepare",
                    "wiki.write"
                ]),
            ),
            (
                "duplicate-web-read",
                json!([
                    "cli.read",
                    "cli.write",
                    "web.read",
                    "web.read",
                    "web.prepare"
                ]),
            ),
        ] {
            let mut bad = grant.clone();
            bad["capabilities"] = capabilities;
            let mut runtime = PortalAcceptanceRuntime {
                issuer: HOSTED_LOGIN_ISSUER.to_string(),
                grant: Some(bad),
                requests: Vec::new(),
                baseline: Instant::now(),
                elapsed: 0,
                requested: vec!["cli.read", "cli.write", "web.read", "web.prepare"],
            };
            let dir = root.path().join(format!("{case}/auth"));
            let out = login_device_flow(
                HOSTED_LOGIN_ISSUER,
                None,
                Some("acme"),
                &dir,
                |_, _| Ok(()),
                &mut runtime,
                LoginScope::Web,
            );
            assert!(out.is_err(), "web consent case {case} was accepted");
            assert_nothing_stored(&dir);
        }
    }

    /// A plain portal login (no `--web`) never asks for web scopes.
    #[test]
    fn plain_portal_login_never_requests_web_scopes() {
        let root = tempfile::tempdir().unwrap();
        let mut grant = login_grant_body();
        grant["organization_slug"] = json!("acme");
        let dir = root.path().join("plain/auth");
        let (out, runtime) = run_wiki_portal_acceptance(grant, &dir);
        out.expect("plain portal login was refused");
        let requested = runtime.requests[0].1["requested_capabilities"].clone();
        assert!(!requested.to_string().contains("web."));
    }

    #[test]
    fn hosted_portal_rejects_conflicting_exact_issuer_pin_before_request() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        trust(&dir, "https://api-v2.agenticos.hk:444");
        let mut runtime = PortalAcceptanceRuntime {
            issuer: HOSTED_LOGIN_ISSUER.to_string(),
            grant: Some(login_grant_body()),
            requests: Vec::new(),
            baseline: Instant::now(),
            elapsed: 0,
            requested: vec!["cli.read", "cli.write", "wiki.read", "wiki.write"],
        };
        let out = login_device_flow(
            HOSTED_LOGIN_ISSUER,
            None,
            None,
            &dir,
            |_, _| Ok(()),
            &mut runtime,
            LoginScope::Wiki,
        );
        assert!(out.is_err());
        assert!(runtime.requests.is_empty(), "pin conflict reached issuer");
        assert_nothing_stored(&dir);
    }

    #[test]
    fn login_sends_the_slug_audience_and_returns_the_verified_org() {
        let (out, dir, _root, script) = run_login(login_grant_body(), vec![]);
        assert_eq!(
            script.requests,
            [
                "/v1/hosted-cadence/device/code",
                "/v1/hosted-cadence/device/token"
            ]
        );
        // interval 5 from the device grant — one scheduled wait, no sleep.
        assert_eq!(script.waits, [5]);
        let grant = out.unwrap();
        assert_eq!(
            (
                grant.organization_id.as_str(),
                grant.slug.as_str(),
                grant.endpoint.as_str()
            ),
            ("ws_real", "acme", LOGIN_ENDPOINT)
        );
        // Verification persists nothing; the caller records, then saves.
        assert_nothing_stored(&dir);
        grant.save_credential(&dir).unwrap();
        let cred = dir.join("cli-acme.json");
        assert_eq!(cred.metadata().unwrap().mode() & 0o777, 0o600);
        let body: Value = serde_json::from_slice(&fs::read(&cred).unwrap()).unwrap();
        assert_eq!(body["access_token"], BRIDGE);
        assert_eq!(body["audience"], LOGIN_ENDPOINT);
        assert!(!dir.join(RECORD).exists());
    }

    #[test]
    fn credential_save_succeeds_when_the_directory_cannot_be_opened_for_read() {
        // A write+execute-only auth directory accepts the rename; the old
        // post-rename `File::open(dir)?` failed after publishing the new
        // credential, which made login roll the registry back.
        let (out, dir, _root, _script) = run_login(login_grant_body(), vec![]);
        let grant = out.unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o300)).unwrap();
        grant.save_credential(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let body: Value =
            serde_json::from_slice(&fs::read(dir.join("cli-acme.json")).unwrap()).unwrap();
        assert_eq!(body["organization_id"], "ws_real");
    }

    #[test]
    fn login_denied_expired_and_slow_down() {
        let (out, dir, _r, script) = run_login(login_grant_body(), vec!["access_denied"]);
        assert!(out.err().unwrap().to_string().contains("denied"));
        assert_nothing_stored(&dir);
        assert_eq!(script.requests.len(), 2, "denied login kept polling");
        let (out, dir, _r, script) = run_login(login_grant_body(), vec!["expired_token"]);
        assert!(out.err().unwrap().to_string().contains("expired"));
        assert_nothing_stored(&dir);
        assert_eq!(script.requests.len(), 2, "expired login kept polling");
        // slow_down is not terminal: back off 5->10, then the grant lands.
        let (out, _dir, _r, script) = run_login(login_grant_body(), vec!["slow_down"]);
        assert_eq!(out.unwrap().slug, "acme");
        assert_eq!(script.waits, [5, 10], "slow_down backoff not applied");
        assert_eq!(script.requests.len(), 3);
    }

    #[test]
    fn login_rejects_a_grant_for_a_different_workspace() {
        // `--org` asked for ws_real; the issuer answers for another one.
        let mut grant = login_grant_body();
        grant["organization_id"] = json!("ws_other");
        let (out, dir, _r, _s) = run_login(grant, vec![]);
        assert!(out.is_err());
        assert_nothing_stored(&dir);
    }

    #[test]
    fn login_rejects_a_grant_for_a_different_audience() {
        // The requested audience must come back byte for byte.
        for audience in [
            "https://other.cadencecloud.app",
            "https://acme.cadencecloud.app.evil.test",
            "https://acme.cadencecloud.app/",
            "http://acme.cadencecloud.app",
        ] {
            let mut grant = login_grant_body();
            grant["audience"] = json!(audience);
            let (out, dir, _r, _s) = run_login(grant, vec![]);
            assert!(out.is_err(), "{audience}");
            assert_nothing_stored(&dir);
        }
    }

    #[test]
    fn login_refuses_a_slug_that_is_not_one_dns_label_before_any_request() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        for bad in [
            "evil.example",
            "acme/x",
            "Acme",
            "-acme",
            "acme-",
            "",
            "a_b",
            "acme:443",
        ] {
            // No listener exists: refusal must precede any network call.
            let out = login_browser("http://127.0.0.1:1", "ws_real", bad, &dir, |_, _| Ok(()));
            assert!(
                out.err()
                    .unwrap()
                    .to_string()
                    .contains("Invalid hosted login request"),
                "{bad}"
            );
        }
    }

    // --- CAD-1125: first-run pin refusals (review acceptance checks) ---

    #[test]
    fn missing_pin_refusal_names_the_shell_quoted_file_and_commands() {
        // Fresh auth dir: login creates it, then the NotFound refusal
        // carries the printf/chmod remedy against the exact pin path.
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        let err = login_browser("http://127.0.0.1:1", "ws_real", "acme", &dir, |_, _| Ok(()))
            .err()
            .unwrap()
            .to_string();
        let file = dir.join(TRUSTED_ISSUER).display().to_string();
        let quoted = shell_quote(&file);
        assert!(err.contains("Trusted issuer pin is missing"), "{err}");
        assert!(
            err.contains(&format!("printf '%s\\n' 'http://127.0.0.1:1' > {quoted}")),
            "{err}"
        );
        assert!(err.contains(&format!("chmod 600 {quoted}")), "{err}");
        // The CLI created the private dir but must never create the pin.
        assert!(dir.is_dir());
        assert_eq!(dir.metadata().unwrap().mode() & 0o777, 0o700);
        assert!(!dir.join(TRUSTED_ISSUER).exists());
    }

    #[test]
    fn symlinked_pin_refuses_without_a_write_command() {
        // ELOOP from O_NOFOLLOW is not "missing": the refusal must not print
        // `printf > <pin>` — that command would clobber the symlink target.
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let victim = root.path().join("victim");
        fs::write(&victim, "keep me").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join(TRUSTED_ISSUER)).unwrap();
        let err = login_browser("http://127.0.0.1:1", "ws_real", "acme", &dir, |_, _| Ok(()))
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("cannot be opened"), "{err}");
        assert!(!err.contains("printf"), "{err}");
        assert!(!err.contains("chmod"), "{err}");
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep me");
    }

    #[test]
    fn auth_dir_with_a_quote_cannot_break_the_printed_command() {
        // A crafted --auth-dir containing ' must not escape the single
        // quotes in the printed remedy.
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("it's");
        let err = login_browser("http://127.0.0.1:1", "ws_real", "acme", &dir, |_, _| Ok(()))
            .err()
            .unwrap()
            .to_string();
        let file = dir.join(TRUSTED_ISSUER).display().to_string();
        // The raw, unescaped path (with a bare ') never appears.
        assert!(!err.contains(&format!(">{file}")), "{err}");
        assert!(!err.contains(&format!("> '{file}'")), "{err}");
        // The shell-quoted form does.
        assert!(err.contains(&format!("> {}", shell_quote(&file))), "{err}");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn login_rejects_agent_capabilities_and_unknown_role() {
        let mut agent = login_grant_body();
        agent["capabilities"] = json!(["bridge.enroll", "results.submit"]);
        let mut unknown = login_grant_body();
        unknown["principal"]["current_role"] = json!("admin");
        for grant in [agent, unknown] {
            let (out, dir, _r, _s) = run_login(grant, vec![]);
            assert!(out.is_err());
            assert_nothing_stored(&dir);
        }
    }

    #[test]
    fn login_accepts_member_without_adding_scopes() {
        let mut member = login_grant_body();
        member["principal"]["current_role"] = json!("member");
        let (out, dir, _r, _s) = run_login(member, vec![]);
        let grant = out.expect("member is a human workspace role");
        assert_eq!(
            grant.capabilities,
            vec!["cli.read".to_string(), "cli.write".to_string(),]
        );
        grant.save_credential(&dir).unwrap();
        let body: Value =
            serde_json::from_slice(&fs::read(dir.join("cli-acme.json")).unwrap()).unwrap();
        assert_eq!(body["capabilities"], json!(["cli.read", "cli.write"]));
        assert_eq!(body["subject_id"], "user_1");
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
            let mut runtime = VirtualWaitRealTransportRuntime::new();
            let out = enroll_browser_with(
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
                &mut runtime,
            );
            (out, runtime.waits)
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
        let (browser_out, browser_waits) = browser.join().unwrap();
        assert_eq!(browser_waits, [5]);
        assert!(format!("{}", browser_out.unwrap_err()).contains("denied"));
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
                public_request_access(&listener, "/v1/hosted-cadence/device/code", true);
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
                public_request_access(&listener, "/v1/hosted-cadence/device/token", true);
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
            let mut child = request_with_audience_access(
                &listener,
                "/v1/hosted-cadence/enroll",
                BRIDGE,
                audience,
                true,
            );
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
        access_file(&dir, &issuer);
        let mut runtime = VirtualWaitRealTransportRuntime::new();
        let info = enroll_browser_with(
            &issuer,
            "ws_real",
            audience,
            "worker",
            &dir,
            |url, code| {
                assert_eq!(
                    url,
                    "https://app.agenticos.test/device/hosted-cadence?code=K7PM-2QNF"
                );
                assert_eq!(code, "K7PM-2QNF");
                Ok(())
            },
            &mut runtime,
        )
        .unwrap();
        assert_eq!(runtime.waits, [5]);
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
        assert!(!fs::read_to_string(dir.join(RECORD))
            .unwrap()
            .contains("test-secret"));
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

    #[test]
    fn hosted_identity_fields_accept_the_200_character_issuer_contract() {
        // The AgenticOS shared contract allows 1..=200 ASCII letters/digits/'_'
        // /'-'; the previous 128 cap rejected issuer identities it must honor.
        fn long_id(len: usize, tag: &str) -> String {
            format!("{tag}-{}", "x".repeat(len - tag.len() - 1))
        }
        for len in [128usize, 129, 200] {
            let mut enrolled = record(u64::MAX);
            // Distinct values preserve the agent != subject/bridge invariants.
            enrolled.organization_id = long_id(len, "org");
            enrolled.subject_id = long_id(len, "sub");
            enrolled.bridge_id = long_id(len, "brg");
            enrolled.agent_id = long_id(len, "agt");
            enrolled.client_agent_id = long_id(len, "cln");
            enrolled.credential_id = long_id(len, "crd");
            enrolled.valid(now().unwrap()).unwrap();
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("e");
            trust(&dir, &enrolled.issuer);
            save(&dir, &enrolled).unwrap();
            let reopened = current(&dir).unwrap();
            assert_eq!(reopened.organization_id(), enrolled.organization_id);
            assert_eq!(reopened.subject_id(), enrolled.subject_id);
            let pinned = DestinationPin::new(
                &enrolled.organization_id,
                &enrolled.audience,
                &enrolled.subject_id,
                &enrolled.agent_id,
            )
            .unwrap();
            let mut calls = 0;
            let bearer_matched = with_current(&dir, &pinned, |token| {
                calls += 1;
                Ok(token == CHILD)
            })
            .unwrap();
            assert_eq!(calls, 1);
            assert!(bearer_matched, "issuer-bound callback saw a wrong bearer");
        }
    }

    #[test]
    fn stored_enrollment_refuses_oversized_or_punctuated_identity_fields() {
        for field in [
            "organization_id",
            "subject_id",
            "bridge_id",
            "agent_id",
            "client_agent_id",
            "credential_id",
        ] {
            for bad in [
                "i".repeat(201),
                String::new(),
                "has space".to_owned(),
                "has.dot".to_owned(),
                "has/slash".to_owned(),
                "non-ascii-é".to_owned(),
            ] {
                let mut forged = record(u64::MAX);
                let slot = match field {
                    "organization_id" => &mut forged.organization_id,
                    "subject_id" => &mut forged.subject_id,
                    "bridge_id" => &mut forged.bridge_id,
                    "agent_id" => &mut forged.agent_id,
                    "client_agent_id" => &mut forged.client_agent_id,
                    "credential_id" => &mut forged.credential_id,
                    _ => unreachable!(),
                };
                *slot = bad.clone();
                // The signed record is intact and private; validation alone refuses.
                assert!(
                    forged.valid(now().unwrap()).is_err(),
                    "{field} accepted {bad:?}"
                );
            }
        }
    }

    #[test]
    fn oversized_identity_fails_before_any_issuer_request_or_child_save() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("e");
        trust(&dir, &issuer);
        for bad in ["o".repeat(201), "has.dot".to_owned()] {
            for (org, client) in [(bad.as_str(), "worker"), ("ws_real", bad.as_str())] {
                assert!(enroll(&issuer, org, "http://127.0.0.1:1", client, SERVICE, &dir).is_err());
                assert!(enroll_browser(
                    &issuer,
                    org,
                    "http://127.0.0.1:1",
                    client,
                    &dir,
                    |_, _| { panic!("oversized identity reached a device request") }
                )
                .is_err());
            }
        }
        assert!(listener.accept().is_err(), "issuer was contacted");
        assert!(!dir.join(RECORD).exists());
    }
}

#[cfg(test)]
#[path = "remote_enrollment_cli_lock_acceptance.rs"]
mod cli_lock_acceptance;

#[cfg(test)]
#[path = "remote_enrollment_tls_acceptance.rs"]
mod tls_acceptance;

#[cfg(test)]
mod portable_migration_gate_tests {
    use super::*;

    const BASE: &str = "/v1/hosted-cadence/portable-migrations/op-1/input-packs";

    #[test]
    fn pack_suffix_admits_only_the_three_shapes() {
        for ok in [
            "/v1/hosted-cadence/portable-migrations/op-1/input-packs/0",
            "/v1/hosted-cadence/portable-migrations/op-1/input-packs/12",
            "/v1/hosted-cadence/portable-migrations/op-1/input-packs/0/parts/1",
            "/v1/hosted-cadence/portable-migrations/op-1/input-packs/12/parts/3",
            "/v1/hosted-cadence/portable-migrations/op-1/input-packs/0/finalize",
        ] {
            assert!(portable_migration_path(ok), "should admit {ok}");
        }
        for tail in [
            "0/parts/0",
            "0/parts/01",
            "00",
            "0/parts/1/extra",
            "0/finalize/extra",
            "0/parts",
            "0/parts/1/2",
            "-1",
            "0x1",
            "/parts/1",
            "0/partsx/1",
            "0/parts/",
            "0/finalize/",
            "01",
        ] {
            let path = format!("{BASE}/{tail}");
            assert!(!portable_migration_path(&path), "should refuse {tail}");
        }
        assert!(!portable_migration_path(
            "/v1/hosted-cadence/portable-migrations/op-1/input-packs"
        ));
        assert!(!portable_migration_path(
            "/v1/hosted-cadence/portable-migrations/op-1/other/0"
        ));
    }

    #[test]
    fn empty_post_is_admitted_only_at_finalize() {
        let finalize = format!("{BASE}/0/finalize");
        let pack = format!("{BASE}/0");
        assert!(portable_migration_shape_admitted(
            &finalize,
            "POST",
            &MigrationBody::Empty
        ));
        // Empty POST refused at a non-finalize path.
        assert!(!portable_migration_shape_admitted(
            &pack,
            "POST",
            &MigrationBody::Empty
        ));
        // A finalize path refused with any body other than an empty POST.
        let body = json!({"x": 1});
        assert!(!portable_migration_shape_admitted(
            &finalize,
            "POST",
            &MigrationBody::Json(&body)
        ));
        assert!(!portable_migration_shape_admitted(
            &finalize,
            "POST",
            &MigrationBody::Bytes(b"x")
        ));
        let mut stream: &[u8] = b"x";
        assert!(!portable_migration_shape_admitted(
            &finalize,
            "PUT",
            &MigrationBody::Stream(&mut stream)
        ));
    }
}
