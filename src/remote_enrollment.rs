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

/// CAD-1853: the explicit cloud-assistant device grant. `bridge.enroll` is
/// the narrow extra authority that lets this consent enroll one managed
/// local peer; the cloud pair is what the human login itself may spend. The
/// enrolled child requests only the pair.
pub(crate) const CLOUD_AGENT_LOGIN_SCOPES: [&str; 3] =
    ["bridge.enroll", "cloud_agent.read", "cloud_agent.write"];

/// The issuer may narrow the consent: a grant is the CLI pair (always) plus
/// any unique subset of the other requested capabilities. It never names an
/// unrequested or duplicate capability, and it never drops a CLI scope.
/// Requested web scopes and requested cloud-agent scopes are kept whole:
/// asking for them and receiving only some of them is not a usable login.
fn consented_caps(caps: &[Value], requested: &[&str]) -> bool {
    let mut seen = BTreeSet::new();
    caps.iter().all(|c| {
        c.as_str()
            .is_some_and(|c| requested.contains(&c) && seen.insert(c))
    }) && ["cli.read", "cli.write"].iter().all(|c| seen.contains(c))
        && requested
            .iter()
            .filter(|c| c.starts_with("web."))
            .all(|c| seen.contains(c))
        && CLOUD_AGENT_LOGIN_SCOPES
            .iter()
            .filter(|c| requested.contains(c))
            .all(|c| seen.contains(c))
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
    login_portal_scoped(issuer, hint, dir, show_code, false, false, false, false)
}

/// The portal login. `web` adds the Publish Web scopes (`cadence login
/// --web`), `settings` adds the Settings scopes (`cadence login
/// --settings`), and `cloud_agent` adds the cloud-agent scopes (`cadence
/// login --cloud-agent`), and `audit_approve` adds `audit.approve`
/// (`cadence login --audit-approve`, CAD-1946). The flags combine. None of
/// them is a second login.
#[allow(clippy::too_many_arguments)]
pub fn login_portal_scoped(
    issuer: &str,
    hint: Option<&str>,
    dir: &Path,
    show_code: impl FnOnce(&str, &str) -> Result<()>,
    web: bool,
    settings: bool,
    cloud_agent: bool,
    audit_approve: bool,
) -> Result<LoginGrant> {
    let issuer = origin(issuer, cfg!(test))?;
    login_device_flow_with(
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
        cloud_agent,
        audit_approve,
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
        false,
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
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn login_device_flow(
    issuer: &str,
    org: Option<&str>,
    hint: Option<&str>,
    dir: &Path,
    show_code: impl FnOnce(&str, &str) -> Result<()>,
    runtime: &mut dyn LoginRuntime,
    scope: LoginScope,
    cloud_agent: bool,
) -> Result<LoginGrant> {
    login_device_flow_with(
        issuer,
        org,
        hint,
        dir,
        show_code,
        runtime,
        scope,
        cloud_agent,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn login_device_flow_with(
    issuer: &str,
    org: Option<&str>,
    hint: Option<&str>,
    dir: &Path,
    show_code: impl FnOnce(&str, &str) -> Result<()>,
    runtime: &mut dyn LoginRuntime,
    scope: LoginScope,
    cloud_agent: bool,
    audit_approve: bool,
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
    let mut requested = scope.requested();
    if cloud_agent {
        requested.extend_from_slice(&CLOUD_AGENT_LOGIN_SCOPES);
    }
    if audit_approve {
        requested.push("audit.approve");
    }
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
mod tests;

#[cfg(test)]
#[path = "remote_enrollment_cli_lock_acceptance.rs"]
mod cli_lock_acceptance;

#[cfg(test)]
#[path = "remote_enrollment_tls_acceptance.rs"]
mod tls_acceptance;

#[cfg(test)]
mod portable_migration_gate_tests;
