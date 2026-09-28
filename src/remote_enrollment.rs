//! Issuer-verified hosted enrollment. A selected outbox destination is never authority.
//!
//! One private directory owns one service credential and one short-lived child.
//! Re-enrollment replaces the record atomically; expiry requires explicit renewal.
//! Server-side revocation is enforced by the hosted gateway at receipt, because
//! AOS-75 exposes no local revocation introspection endpoint.
use crate::remote_result_outbox::DestinationPin;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const VERSION: &str = "hosted-cadence-auth.v1";
const MAX_RESPONSE: u64 = 64 * 1024;
const MAX_SERVICE_TOKEN: usize = 128;
const RECORD: &str = "enrollment.json";
const TRUSTED_ISSUER: &str = "trusted-issuer";

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
    service_token: String,
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
            || !token(&self.service_token, "hcs_")
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
    record.valid(now()?)?;
    record.with_pin(pin, send)?
}
pub fn current(dir: &Path) -> Result<EnrollmentInfo> {
    private_dir(dir, false)?;
    let _guard = lock(dir, false)?;
    let record = read_locked(dir)?;
    record.valid(now()?)?;
    Ok(record.info())
}
/// Explicit renewal uses the previously stored service credential. The issuer
/// re-verifies its current owner, organization, audience, scope and revocation.
pub fn renew(dir: &Path) -> Result<EnrollmentInfo> {
    private_dir(dir, false)?;
    let prior = {
        let _guard = lock(dir, false)?;
        read_locked(dir)?
    };
    enroll(
        &prior.issuer,
        &prior.organization_id,
        &prior.audience,
        &prior.client_agent_id,
        &prior.service_token,
        dir,
    )
}
pub fn remove(dir: &Path) -> Result<()> {
    private_dir(dir, false)?;
    let _guard = lock(dir, true)?;
    let path = dir.join(RECORD);
    let _ = read_locked(dir)?;
    fs::remove_file(path)?;
    Ok(())
}
fn save(dir: &Path, record: &Enrollment) -> Result<()> {
    private_dir(dir, true)?;
    let _guard = lock(dir, true)?;
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
    require_trusted_issuer(dir, &issuer)?;
    let requested = ["bridge.enroll", "results.submit"];
    let exchange = post(
        &issuer,
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
    let enrollment = post(
        &issuer,
        "/v1/hosted-cadence/service/enroll",
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
        issuer,
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
        service_token: service_token.into(),
    };
    if record.organization_id != org
        || record.audience != audience
        || record.subject_id != field(principal, "subject_id")?
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
        return Err(reject("Issuer child did not match service grant"));
    }
    record.valid(at)?;
    require_trusted_issuer(dir, &record.issuer)?;
    save(dir, &record)?;
    Ok(record.info())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;

    const SERVICE: &str = "hcs_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    const BRIDGE: &str = "hct_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
    const CHILD: &str = "hct_CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";
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
            service_token: SERVICE.into(),
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
    fn private_record_detects_tamper_symlink_world_readability_and_survives_restart() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("enroll");
        let original = record(u64::MAX);
        save(&dir, &original).unwrap();
        assert_eq!(current(&dir).unwrap().agent_id(), original.agent_id);
        assert!(with_current(&dir, &pin(&original.audience), |token| Ok(token == CHILD)).unwrap());
        let file = dir.join(RECORD);
        let mut value: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        value["record"]["audience"] = json!("https://attacker.board.example.test");
        fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(current(&dir).is_err());
        save(&root.path().join("fresh"), &original).unwrap();
        let fresh = root.path().join("fresh");
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

    fn respond(socket: &mut std::net::TcpStream, body: &Value) {
        let bytes = serde_json::to_vec(body).unwrap();
        write!(socket, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", bytes.len()).unwrap();
        socket.write_all(&bytes).unwrap();
    }
    fn request(listener: &TcpListener, path: &str, bearer: &str) -> std::net::TcpStream {
        let (mut socket, _) = listener.accept().unwrap();
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
        assert_eq!(body["audience"], "http://127.0.0.1:1");
        socket
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
}
