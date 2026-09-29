//! Host-custodied SMTP submission for the CRM email MVP (CAD-785).
//!
//! One bounded sender connection per CRM installation/context: the
//! operator enrolls typed transport material (host, port, TLS mode,
//! verified sender identity) plus the secret through the
//! provider-neutral `connection_create` path with enrollment shape
//! `"smtp"`. Every byte — transport fields, sender identity and
//! secret — lives in host custody; only the connection ID, the
//! credential (authorization) revision and non-secret projections
//! ever leave the daemon. No App SQLite, browser frame, workflow
//! text, log, manifest or agent prompt receives a credential.
//!
//! Submission is authenticated encrypted mail submission only:
//! port 465 with implicit TLS, or port 587 with mandatory STARTTLS,
//! both with full certificate verification against the platform
//! roots (plus an operator-configured isolated-test CA the fixture
//! daemon pins — production never sets it). Plaintext submission,
//! a missing STARTTLS advertisement, an unverifiable certificate,
//! an unauthenticated session and any downgrade are refused before
//! a single message byte is written. The client never authenticates
//! before the TLS handshake completes, on either port.
//!
//! The test send is a distinct operator-only effect: one operator
//! address, the exact CAD-782 frozen HTML/text bytes, a multipart
//! `alternative` body, and a receipt that records SMTP
//! acceptance/refusal only — never delivery, never reads. There is
//! no bulk path in this ticket.

use super::connections::{CapabilityDescriptor, CapabilitySemantics, ProviderDescriptor};
use crate::contract_fixture::ToolTable;
use crate::error::{Error, Result};
use crate::store::app_records::email_shape_valid;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

/// The reviewed provider name — hyphen-only platform grammar.
pub const PLATFORM: &str = "smtp";
/// Enrollment shape this provider accepts. The only non-`token`
/// shape the provider-neutral grammar admits, by exact allowlist.
pub const ENROLLMENT_SHAPE: &str = "smtp";
/// Manifest pin reviewed with this adapter's tool table.
pub const MANIFEST_PIN: &str = "smtp-connections/1";
/// Composition receipt for binding checks.
pub const REGISTRATION: &str = "smtp-connections/1";
/// Capability/tool/scope vocabulary for the email-send slot.
pub const CAPABILITY_EMAIL_SEND: &str = "email.send";
pub const CAPABILITY_VERSION: u32 = 1;
pub const TOOL_EMAIL_SEND: &str = "email.send";
pub const SCOPE_EMAIL_SEND: &str = "email:send";

/// Submission ports. Exactly these two exist: 465 speaks TLS
/// immediately, 587 upgrades with a mandatory STARTTLS. Port 25
/// (opportunistic/plaintext relay) and every other port are refused.
pub const PORT_IMPLICIT_TLS: u16 = 465;
pub const PORT_STARTTLS: u16 = 587;
/// TLS modes, paired with their port: `implicit` on 465,
/// `starttls` on 587. Cross-pairing is refused — it is either a
/// misconfiguration or a downgrade probe.
pub const TLS_IMPLICIT: &str = "implicit";
pub const TLS_STARTTLS: &str = "starttls";

const TABLE_JSON: &str = r#"{
    "platform": "smtp",
    "manifest_version": "smtp-connections/1",
    "tools": [
        {
            "tool": "email.send",
            "effect": "send",
            "scopes": ["email:send"],
            "label": "Submit one operator test message through the enrolled SMTP sender"
        }
    ]
}"#;

/// Field bounds for operator-typed enrollment material.
pub const HOST_BYTES: usize = 253;
pub const USERNAME_BYTES: usize = 320;
pub const SECRET_BYTES: usize = 1024;
pub const SENDER_NAME_BYTES: usize = 80;

/// Connection timeout for the submission socket.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Read/write timeout once connected.
const IO_TIMEOUT: Duration = Duration::from_secs(15);
/// Bound on one SMTP reply line and on the screened reply excerpt a
/// receipt may carry.
const REPLY_LINE_CAP: usize = 4096;
const RECEIPT_TEXT_CAP: usize = 512;
/// Bound on the assembled message (headers + both MIME parts).
const MESSAGE_BYTES_CAP: usize = 1024 * 1024;

/// Typed enrollment material: everything the operator supplies for
/// shape `"smtp"`, validated before any custody write. The secret
/// travels inside custody bytes only.
#[derive(Clone, Debug)]
pub struct SmtpEnrollment {
    pub host: String,
    pub port: u16,
    pub tls_mode: String,
    pub username: String,
    pub secret: Vec<u8>,
    pub sender: String,
    pub sender_name: String,
}

/// Non-secret projection of enrolled material. Safe for operator
/// metadata, binding digests and receipts; the secret is never a
/// field here by construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmtpProjection {
    pub host: String,
    pub port: u16,
    pub tls_mode: String,
    pub username: String,
    pub sender: String,
    pub sender_name: String,
}

impl SmtpProjection {
    pub fn to_json(&self) -> Value {
        json!({
            "host": self.host,
            "port": self.port,
            "tls_mode": self.tls_mode,
            "username": self.username,
            "sender": self.sender,
            "sender_name": self.sender_name,
        })
    }
}

/// Custody bytes are canonical JSON under schema 1. The store keeps
/// only a fingerprint; this codec is the only reader. The secret is
/// a field here — these bytes enter custody and never leave it.
pub fn custody_bytes(enrollment: &SmtpEnrollment) -> Result<Vec<u8>> {
    let secret = std::str::from_utf8(&enrollment.secret)
        .map_err(|_| Error::rejected("SMTP secret exceeds its supported shape or bounds"))?;
    let bytes = serde_json::to_vec(&json!({
        "schema": 1,
        "provider": PLATFORM,
        "host": enrollment.host,
        "port": enrollment.port,
        "tls_mode": enrollment.tls_mode,
        "username": enrollment.username,
        "secret": secret,
        "sender": enrollment.sender,
        "sender_name": enrollment.sender_name,
    }))
    .map_err(|e| Error::internal(e.to_string()))?;
    Ok(bytes)
}

/// Parse custody bytes back into the envelope (secret included) plus
/// its public projection. Anything that is not exactly the canonical
/// schema-1 document refuses — a foreign or hand-edited custody blob
/// never becomes send authority.
pub fn custody_decode(bytes: &[u8]) -> Result<(SmtpEnvelope, SmtpProjection)> {
    let doc: Value =
        serde_json::from_slice(bytes).map_err(|_| Error::rejected("SMTP custody is corrupt"))?;
    let fields = doc
        .as_object()
        .ok_or_else(|| Error::rejected("SMTP custody is corrupt"))?;
    // Exactly the canonical schema-1 document: schema, provider,
    // host, port, tls_mode, username, secret, sender, sender_name.
    if fields.len() != 9 || doc["schema"] != json!(1) || doc["provider"] != json!(PLATFORM) {
        return Err(Error::rejected("SMTP custody is corrupt"));
    }
    let host = doc["host"]
        .as_str()
        .ok_or_else(|| Error::rejected("SMTP custody is corrupt"))?;
    let port = doc["port"]
        .as_u64()
        .ok_or_else(|| Error::rejected("SMTP custody is corrupt"))?;
    let port: u16 = u16::try_from(port).map_err(|_| Error::rejected("SMTP custody is corrupt"))?;
    let tls_mode = doc["tls_mode"]
        .as_str()
        .ok_or_else(|| Error::rejected("SMTP custody is corrupt"))?;
    let username = doc["username"]
        .as_str()
        .ok_or_else(|| Error::rejected("SMTP custody is corrupt"))?;
    let secret = doc["secret"]
        .as_str()
        .ok_or_else(|| Error::rejected("SMTP custody is corrupt"))?;
    let sender = doc["sender"]
        .as_str()
        .ok_or_else(|| Error::rejected("SMTP custody is corrupt"))?;
    let sender_name = doc["sender_name"]
        .as_str()
        .ok_or_else(|| Error::rejected("SMTP custody is corrupt"))?;
    let host = validate_host(host)?;
    validate_port_tls(&host, port, tls_mode)?;
    validate_username(username)?;
    validate_secret(secret)?;
    validate_sender(sender)?;
    validate_sender_name(sender_name)?;
    Ok((
        SmtpEnvelope {
            host: host.to_string(),
            port,
            tls_mode: tls_mode.to_string(),
            username: username.to_string(),
            secret: secret.as_bytes().to_vec(),
            sender: sender.to_string(),
            sender_name: sender_name.to_string(),
        },
        SmtpProjection {
            host: host.to_string(),
            port,
            tls_mode: tls_mode.to_string(),
            username: username.to_string(),
            sender: sender.to_string(),
            sender_name: sender_name.to_string(),
        },
    ))
}

/// The live send envelope: validated transport + secret, resolved
/// from custody inside the operator-gated send path only.
pub struct SmtpEnvelope {
    pub host: String,
    pub port: u16,
    pub tls_mode: String,
    pub username: String,
    secret: Vec<u8>,
    pub sender: String,
    pub sender_name: String,
}

impl SmtpEnvelope {
    /// Custody bytes for leak-screening send outputs. The callers
    /// that hold an envelope already held the custody bytes it was
    /// decoded from — this spreads nothing new.
    pub fn secret(&self) -> &[u8] {
        &self.secret
    }
}

/// A DNS host: dotted names, or exactly `localhost` for the isolated
/// synthetic test rig. IP literals are refused — certificate
/// verification pins DNS names, and submission to a bare address is
/// never the CRM sender. A trailing dot is normalized away.
pub fn validate_host(host: &str) -> Result<String> {
    if host.is_empty() || host.len() > HOST_BYTES {
        return Err(Error::rejected(
            "SMTP host exceeds its supported shape or bounds",
        ));
    }
    if host.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return Err(Error::rejected(
            "SMTP host exceeds its supported shape or bounds",
        ));
    }
    let normalized = host.strip_suffix('.').unwrap_or(host);
    if normalized.is_empty() || normalized.len() > HOST_BYTES {
        return Err(Error::rejected(
            "SMTP host exceeds its supported shape or bounds",
        ));
    }
    if normalized == "localhost" {
        return Ok(normalized.to_string());
    }
    // No IP literals: a parsed address is never a DNS submission host.
    if normalized.parse::<std::net::IpAddr>().is_ok() {
        return Err(Error::rejected(
            "SMTP host must be a DNS name, not an IP literal",
        ));
    }
    if !normalized.contains('.') {
        return Err(Error::rejected(
            "SMTP host must be a fully qualified DNS name",
        ));
    }
    for label in normalized.split('.') {
        if label.is_empty()
            || label.len() > 63
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || label.starts_with('-')
            || label.ends_with('-')
        {
            return Err(Error::rejected(
                "SMTP host exceeds its supported shape or bounds",
            ));
        }
    }
    Ok(normalized.to_ascii_lowercase())
}

/// Exactly (465, implicit) or (587, starttls) on public hosts.
/// Anything else — port 25, a crossed pair — is refused as
/// plaintext or downgrade surface, never negotiated. The
/// isolated-test host `localhost` may use any port (the rig binds
/// ephemeral loopback ports), but the TLS mode is still mandatory
/// and still decides the wire behavior: `implicit` handshakes TLS
/// first, `starttls` upgrades from a mandatory STARTTLS. There is
/// no plaintext mode on any host or port.
pub fn validate_port_tls(host: &str, port: u16, tls_mode: &str) -> Result<()> {
    if tls_mode != TLS_IMPLICIT && tls_mode != TLS_STARTTLS {
        return Err(Error::rejected(
            "SMTP submission requires implicit TLS or mandatory STARTTLS",
        ));
    }
    if host == "localhost" {
        if port == 0 {
            return Err(Error::rejected(
                "SMTP submission requires port 465 with implicit TLS or port 587 with mandatory STARTTLS",
            ));
        }
        return Ok(());
    }
    match (port, tls_mode) {
        (PORT_IMPLICIT_TLS, TLS_IMPLICIT) | (PORT_STARTTLS, TLS_STARTTLS) => Ok(()),
        _ => Err(Error::rejected(
            "SMTP submission requires port 465 with implicit TLS or port 587 with mandatory STARTTLS",
        )),
    }
}

fn validate_username(username: &str) -> Result<()> {
    if username.is_empty()
        || username.len() > USERNAME_BYTES
        || username
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
        || username.contains(['<', '>', '"', '\'', '`', '\\'])
    {
        return Err(Error::rejected(
            "SMTP username exceeds its supported shape or bounds",
        ));
    }
    Ok(())
}

fn validate_secret(secret: &str) -> Result<()> {
    if secret.is_empty() || secret.len() > SECRET_BYTES || secret.chars().any(char::is_whitespace) {
        return Err(Error::rejected(
            "SMTP secret exceeds its supported shape or bounds",
        ));
    }
    Ok(())
}

/// The verified sender identity: strict address shape, ASCII only
/// (SMTP headers are 7-bit before encoding), never a placeholder.
pub fn validate_sender(sender: &str) -> Result<()> {
    if !email_shape_valid(sender)
        || !sender.is_ascii()
        || sender.contains(['<', '>', '(', ')', '[', ']', '\\', '"', '\'', ';', ',', '`'])
        || sender.to_lowercase().contains(".invalid")
    {
        return Err(Error::rejected(
            "SMTP sender exceeds its supported shape or bounds",
        ));
    }
    Ok(())
}

fn validate_sender_name(name: &str) -> Result<()> {
    if name.len() > SENDER_NAME_BYTES
        || (!name.is_empty() && name.trim() != name)
        || name.chars().any(char::is_control)
        || name.contains(['<', '>', '\r', '\n', '"', '`', '\\'])
        || !name.is_ascii()
    {
        return Err(Error::rejected(
            "SMTP sender name exceeds its supported shape or bounds",
        ));
    }
    Ok(())
}

fn required_field<'a>(params: &'a Value, name: &str) -> Result<&'a str> {
    params
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected(format!("SMTP enrollment is missing '{name}'")))
}

fn optional_field(params: &Value, name: &str) -> Result<Option<String>> {
    match params.get(name) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(Error::rejected(format!(
            "SMTP field '{name}' must be a string"
        ))),
    }
}

/// Parse and validate the typed shape-`"smtp"` enrollment grammar.
/// `token`/`class` never participate — the secret arrives as
/// `secret`, transport as typed fields, and an opaque-token paste is
/// refused by shape, not reinterpreted.
pub fn parse_enrollment(params: &Value) -> Result<SmtpEnrollment> {
    if params.get("token").is_some() || params.get("class").is_some() {
        return Err(Error::rejected(
            "SMTP enrollment carries typed host, port, TLS mode, sender and secret fields — no token",
        ));
    }
    let host = validate_host(required_field(params, "host")?)?;
    let port = params
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .ok_or_else(|| Error::rejected("SMTP port must be 1-65535"))?;
    let tls_mode = required_field(params, "tls_mode")?;
    if tls_mode != TLS_IMPLICIT && tls_mode != TLS_STARTTLS {
        return Err(Error::rejected(
            "SMTP TLS mode must be implicit or starttls",
        ));
    }
    validate_port_tls(&host, port, tls_mode)?;
    let username = required_field(params, "username")?;
    validate_username(username)?;
    let secret = required_field(params, "secret")?;
    validate_secret(secret)?;
    let sender = required_field(params, "sender")?;
    validate_sender(sender)?;
    let sender_name = match optional_field(params, "sender_name")? {
        None => String::new(),
        Some(name) => {
            validate_sender_name(&name)?;
            name
        }
    };
    Ok(SmtpEnrollment {
        host,
        port,
        tls_mode: tls_mode.to_string(),
        username: username.to_string(),
        secret: secret.as_bytes().to_vec(),
        sender: sender.to_string(),
        sender_name,
    })
}

/// Overlay a rotate's partial re-spec onto the live custody
/// projection: `secret` is always fresh; transport/sender fields
/// re-validate when present and inherit otherwise. The result is a
/// full enrollment ready for custody — rotation never leaves half
/// a transport behind.
pub fn overlay_rotate(current: &SmtpProjection, params: &Value) -> Result<SmtpEnrollment> {
    if params.get("token").is_some() || params.get("class").is_some() {
        return Err(Error::rejected(
            "SMTP rotation carries the fresh secret — no token",
        ));
    }
    let secret = required_field(params, "secret")?;
    validate_secret(secret)?;
    let host = match optional_field(params, "host")? {
        None => current.host.clone(),
        Some(raw) => validate_host(&raw)?,
    };
    let port = match params.get("port") {
        None => current.port,
        Some(Value::Number(_)) => params
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok())
            .ok_or_else(|| Error::rejected("SMTP port must be 1-65535"))?,
        Some(_) => return Err(Error::rejected("SMTP port must be 1-65535")),
    };
    let tls_mode = match optional_field(params, "tls_mode")? {
        None => current.tls_mode.clone(),
        Some(mode) => {
            if mode != TLS_IMPLICIT && mode != TLS_STARTTLS {
                return Err(Error::rejected(
                    "SMTP TLS mode must be implicit or starttls",
                ));
            }
            mode
        }
    };
    validate_port_tls(&host, port, &tls_mode)?;
    let username = match optional_field(params, "username")? {
        None => current.username.clone(),
        Some(raw) => {
            validate_username(&raw)?;
            raw
        }
    };
    let sender = match optional_field(params, "sender")? {
        None => current.sender.clone(),
        Some(raw) => {
            validate_sender(&raw)?;
            raw
        }
    };
    let sender_name = match optional_field(params, "sender_name")? {
        None => current.sender_name.clone(),
        Some(raw) => {
            validate_sender_name(&raw)?;
            raw
        }
    };
    Ok(SmtpEnrollment {
        host,
        port,
        tls_mode,
        username,
        secret: secret.as_bytes().to_vec(),
        sender,
        sender_name,
    })
}

/// The adapter: reviewed tool table, descriptor with the `smtp`
/// enrollment shape, and the `email.send` capability for the CRM
/// slot. Registration is composition metadata, never liveness.
pub struct SmtpAdapter {
    table: ToolTable,
}

impl SmtpAdapter {
    pub fn new() -> Result<Self> {
        Ok(Self {
            table: ToolTable::from_json(
                &serde_json::from_str(TABLE_JSON)
                    .map_err(|e| Error::internal(format!("smtp tool table is corrupt: {e}")))?,
            )
            .map_err(|e| Error::internal(format!("smtp tool table is invalid: {e}")))?,
        })
    }
}

impl Default for SmtpAdapter {
    fn default() -> Self {
        Self::new().expect("smtp tool table parses")
    }
}

/// Register the `smtp` provider on `opts`. Unconditional like
/// `local`: the adapter holds no credential and opens no socket —
/// enrollment is the operator's explicit act, and a daemon without
/// one fails every SMTP send closed.
pub fn attach(opts: &mut crate::daemon::ServeOptions) {
    opts.platforms
        .insert(PLATFORM.to_string(), Arc::new(SmtpAdapter::default()));
}

impl crate::platform::PlatformAdapter for SmtpAdapter {
    fn table(&self) -> &ToolTable {
        &self.table
    }

    fn reported_manifest_version(&self) -> Option<String> {
        self.table.manifest_version.clone()
    }

    fn connection_descriptor(&self) -> Option<ProviderDescriptor> {
        Some(ProviderDescriptor {
            schema: 1,
            provider: PLATFORM.to_string(),
            revision: MANIFEST_PIN.to_string(),
            enrollment_shapes: vec![ENROLLMENT_SHAPE.to_string()],
            builtin_accounts: vec![],
            capabilities: vec![CapabilityDescriptor {
                id: CAPABILITY_EMAIL_SEND.to_string(),
                version: CAPABILITY_VERSION,
                tools: vec![TOOL_EMAIL_SEND.to_string()],
                scopes: vec![SCOPE_EMAIL_SEND.to_string()],
                effect: "send".to_string(),
                semantics: CapabilitySemantics::EmailSend,
            }],
            action_mappings: vec![],
        })
    }

    fn connection_registration(&self) -> Option<String> {
        Some(REGISTRATION.to_string())
    }

    fn preview(&self, account: &str, tool: &str, input: &Value) -> String {
        format!("{tool} through smtp/{account}: {input}")
    }

    fn execute(
        &self,
        _credential: &[u8],
        _tool: &str,
        _input: &Value,
        _idempotency_key: &str,
        _expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        Err("smtp test sends run through the CRM sender path, not the generic effect gate".into())
    }

    fn read_back(&self, _tool: &str, _input: &Value) -> crate::contract_fixture::Verified {
        crate::contract_fixture::Verified::Unknown
    }

    fn source_hash(&self, _agent: &str, _source: &str) -> Option<String> {
        None
    }
}

// ---------- the send path ----------

/// One operator test message: verified sender, one operator
/// recipient, the frozen CAD-782 subject/HTML/text bytes.
pub struct SmtpMessage {
    pub to: String,
    pub subject: String,
    pub html: String,
    pub text: String,
    pub unsubscribe_url: String,
    /// CAD-786: the durable per-recipient key a campaign delivery
    /// stamps as `Message-ID: <key@cadence.invalid>`; `None` mints an
    /// ephemeral id on the envelope host (test sends).
    pub idempotency_key: Option<String>,
}

/// What the receipt records: SMTP acceptance or refusal — never
/// delivery, never reads. `message` is a bounded server excerpt
/// screened against the secret before it lands anywhere.
pub struct SmtpReceipt {
    pub accepted: bool,
    pub code: u16,
    pub message: String,
}

/// Minimal PEM parser for exactly one use: loading the
/// operator-configured isolated-test CA into the trust store.
/// Production never sets it and never needs this code path.
fn parse_pem_certificates(pem: &[u8]) -> Result<Vec<Vec<u8>>> {
    const BEGIN: &[u8] = b"-----BEGIN CERTIFICATE-----";
    const END: &[u8] = b"-----END CERTIFICATE-----";
    let text =
        std::str::from_utf8(pem).map_err(|_| Error::rejected("SMTP test CA is not PEM text"))?;
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(begin) = rest.find("-----BEGIN CERTIFICATE-----") {
        let body = &rest[begin + BEGIN.len()..];
        let Some(end) = body.find("-----END CERTIFICATE-----") else {
            return Err(Error::rejected("SMTP test CA PEM is truncated"));
        };
        let b64: String = body[..end]
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        if b64.is_empty() {
            return Err(Error::rejected("SMTP test CA PEM holds no certificate"));
        }
        let der =
            base64_decode(&b64).ok_or_else(|| Error::rejected("SMTP test CA PEM is not base64"))?;
        if der.is_empty() {
            return Err(Error::rejected("SMTP test CA PEM holds no certificate"));
        }
        out.push(der);
        rest = &body[end + END.len()..];
    }
    if out.is_empty() {
        return Err(Error::rejected("SMTP test CA holds no certificate"));
    }
    Ok(out)
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD.decode(input).ok()
}

fn base64_encode(input: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD.encode(input)
}

fn tls_config(extra_ca_pem: Option<&[u8]>) -> Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(pem) = extra_ca_pem {
        for der in parse_pem_certificates(pem)? {
            roots
                .add(rustls::pki_types::CertificateDer::from(der))
                .map_err(|_| Error::rejected("SMTP test CA certificate is not a valid CA"))?;
        }
    }
    Ok(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

/// RFC 2047 for the Subject: plain ASCII rides raw; anything else is
/// one base64 encoded-word. Deterministic, bounded, 7-bit safe.
fn encode_subject(subject: &str) -> Result<String> {
    if subject.is_empty() || subject.len() > 1000 {
        return Err(Error::rejected("SMTP subject exceeds its supported bounds"));
    }
    if subject.is_ascii() && !subject.chars().any(char::is_control) {
        return Ok(subject.to_string());
    }
    if !subject.is_ascii() && subject.chars().any(char::is_control) {
        return Err(Error::rejected("SMTP subject exceeds its supported bounds"));
    }
    if subject.chars().any(|ch| ch == '\r' || ch == '\n') {
        return Err(Error::rejected("SMTP subject exceeds its supported bounds"));
    }
    Ok(format!("=?utf-8?b?{}?=", base64_encode(subject.as_bytes())))
}

fn header_address(name: &str, address: &str) -> Result<String> {
    if !email_shape_valid(address) || !address.is_ascii() {
        return Err(Error::rejected("SMTP address exceeds its supported shape"));
    }
    if name.is_empty() {
        return Ok(format!("<{address}>"));
    }
    validate_sender_name(name)?;
    Ok(format!("\"{name}\" <{address}>"))
}

/// Deterministic multipart/alternative assembly from the frozen
/// render bytes. The boundary is derived from the content digest so
/// the same revision always assembles the same body.
pub fn assemble_message(
    envelope: &SmtpEnvelope,
    message: &SmtpMessage,
    content_digest: &str,
) -> Result<String> {
    validate_sender(&message.to).map_err(|_| Error::rejected("SMTP recipient is invalid"))?;
    let boundary = format!(
        "cadence-{}",
        &crate::platform::connections::registration_digest(content_digest)[7..39]
    );
    let date = crate::issue::time::now_epoch();
    let message_id = match &message.idempotency_key {
        Some(key) => {
            // The durable key is a sha256 digest; its `sha256:` tag
            // stays out of the header's local part.
            let key = key.strip_prefix("sha256:").unwrap_or(key);
            if key.len() > 128
                || !key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(Error::rejected("SMTP idempotency key exceeds its bounds"));
            }
            format!("<{key}@cadence.invalid>")
        }
        None => format!("<{}@{}>", uuid::Uuid::new_v4().simple(), envelope.host),
    };
    let mut out = String::new();
    out.push_str(&format!(
        "From: {}\r\n",
        header_address(&envelope.sender_name, &envelope.sender)?
    ));
    out.push_str(&format!("To: <{}>\r\n", message.to));
    out.push_str(&format!(
        "Subject: {}\r\n",
        encode_subject(&message.subject)?
    ));
    out.push_str(&format!("Date: {date}\r\n"));
    out.push_str(&format!("Message-ID: {message_id}\r\n"));
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str(&format!(
        "Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n"
    ));
    if !message.unsubscribe_url.is_empty() {
        // https anywhere, or loopback http for the isolated rigs —
        // the same shape the daemon's origin check enforces.
        let loopback = message
            .unsubscribe_url
            .strip_prefix("http://")
            .is_some_and(|rest| {
                matches!(
                    rest.split(['/', '?', '#'])
                        .next()
                        .unwrap_or_default()
                        .split(':')
                        .next()
                        .unwrap_or_default(),
                    "localhost" | "127.0.0.1" | "[::1]"
                )
            });
        if message.unsubscribe_url.len() > 2000
            || !(message.unsubscribe_url.starts_with("https://") || loopback)
        {
            return Err(Error::rejected(
                "SMTP unsubscribe URL exceeds its supported shape",
            ));
        }
        out.push_str(&format!(
            "List-Unsubscribe: <{}>\r\n",
            message.unsubscribe_url
        ));
        out.push_str("List-Unsubscribe-Post: List-Unsubscribe=One-Click\r\n");
    }
    out.push_str("\r\n");
    out.push_str("This is a multi-part message in MIME format.\r\n");
    for (part, body) in [("plain", &message.text), ("html", &message.html)] {
        if body.len() > MESSAGE_BYTES_CAP {
            return Err(Error::rejected("SMTP body exceeds its supported bounds"));
        }
        let subtype = if part == "plain" {
            "text/plain"
        } else {
            "text/html"
        };
        out.push_str(&format!("--{boundary}\r\n"));
        out.push_str(&format!("Content-Type: {subtype}; charset=utf-8\r\n"));
        out.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
        // Bodies come from the host renderer; normalize lone LF just
        // in case, then enforce CRLF-only, 7/8-bit-clean lines.
        let normalized = body
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\n', "\r\n");
        out.push_str(&normalized);
        out.push_str("\r\n");
    }
    out.push_str(&format!("--{boundary}--\r\n"));
    if out.len() > 2 * MESSAGE_BYTES_CAP {
        return Err(Error::rejected("SMTP body exceeds its supported bounds"));
    }
    Ok(out)
}

struct SmtpLine {
    code: u16,
    text: String,
}

/// The session wire: plaintext only before STARTTLS, the verified
/// TLS tunnel ever after. `StreamOwned` owns both halves of the
/// connection, so the upgrade is a move, never an alias — there is
/// no handle left that could still speak plaintext after the
/// handshake.
enum Wire {
    Plain(BufReader<TcpStream>),
    Tls(Box<BufReader<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>>),
}

struct SmtpSession {
    wire: Wire,
}

impl SmtpSession {
    fn dial(host: &str, port: u16) -> Result<TcpStream> {
        let addresses: Vec<_> = (host, port)
            .to_socket_addrs()
            .map_err(|_| Error::rejected("SMTP host does not resolve"))?
            .collect();
        if addresses.is_empty() {
            return Err(Error::rejected("SMTP host does not resolve"));
        }
        // Refuse to open submission sockets at non-loopback
        // addresses when the enrolled host is the isolated-test
        // name: `localhost` must resolve to loopback, never to a
        // LAN or public address a test CA would then vouch for.
        if host == "localhost" && !addresses.iter().all(|addr| addr.ip().is_loopback()) {
            return Err(Error::rejected(
                "SMTP isolated-test host must resolve to loopback",
            ));
        }
        let mut last = Error::rejected("SMTP host is unreachable");
        for addr in addresses {
            match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(IO_TIMEOUT))
                        .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
                        .map_err(|_| Error::rejected("SMTP transport failed"))?;
                    return Ok(stream);
                }
                Err(_) => {
                    last = Error::rejected("SMTP host is unreachable");
                }
            }
        }
        Err(last)
    }

    fn tls_connection(
        host: &str,
        tls: &Arc<rustls::ClientConfig>,
    ) -> Result<rustls::ClientConnection> {
        let name = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|_| Error::rejected("SMTP host is not a valid TLS server name"))?;
        rustls::ClientConnection::new(tls.clone(), name)
            .map_err(|_| Error::rejected("SMTP TLS negotiation failed"))
    }

    fn reader(&mut self) -> &mut dyn BufRead {
        match &mut self.wire {
            Wire::Plain(reader) => reader,
            Wire::Tls(reader) => reader,
        }
    }

    fn writer(&mut self) -> &mut dyn Write {
        match &mut self.wire {
            Wire::Plain(reader) => reader.get_mut(),
            Wire::Tls(reader) => reader.get_mut(),
        }
    }

    fn plain(stream: TcpStream) -> Self {
        Self {
            wire: Wire::Plain(BufReader::new(stream)),
        }
    }

    fn tls(stream: TcpStream, host: &str, tls: &Arc<rustls::ClientConfig>) -> Result<Self> {
        let connection = Self::tls_connection(host, tls)?;
        Ok(Self {
            // The handshake — with full certificate verification —
            // completes on first I/O through this handle; a failed
            // verify errors before any SMTP byte is exchanged.
            wire: Wire::Tls(Box::new(BufReader::new(rustls::StreamOwned::new(
                connection, stream,
            )))),
        })
    }

    /// Move a plaintext session into the verified tunnel. The old
    /// wire is consumed: after this returns, only TLS exists — no
    /// handle is left that could still speak plaintext.
    fn upgrade_tls(self, host: &str, tls: &Arc<rustls::ClientConfig>) -> Result<Self> {
        match self.wire {
            Wire::Plain(reader) => Self::tls(reader.into_inner(), host, tls),
            Wire::Tls(_) => Err(Error::internal("SMTP session is already encrypted")),
        }
    }

    fn read_reply(&mut self) -> Result<SmtpLine> {
        let mut text = String::new();
        let mut code = 0u16;
        loop {
            let mut line = String::new();
            let read = self
                .reader()
                .read_line(&mut line)
                .map_err(|_| Error::rejected("SMTP reply is unreadable"))?;
            if read == 0 {
                return Err(Error::rejected("SMTP server closed the connection"));
            }
            if line.len() > REPLY_LINE_CAP {
                return Err(Error::rejected("SMTP reply exceeds its bounds"));
            }
            if line.len() < 4 {
                return Err(Error::rejected("SMTP reply is malformed"));
            }
            let parsed: u16 = line[..3]
                .parse()
                .map_err(|_| Error::rejected("SMTP reply is malformed"))?;
            if code == 0 {
                code = parsed;
            } else if parsed != code {
                return Err(Error::rejected("SMTP reply is malformed"));
            }
            // Strip the `250-`/`250 ` framing; continuation text is
            // joined with `\n` so capability lines parse uniformly.
            let payload = line[3..]
                .trim_end_matches(['\r', '\n'])
                .trim_start_matches(['-', ' ']);
            text.push_str(payload);
            match line.chars().nth(3) {
                Some(' ') => return Ok(SmtpLine { code, text }),
                Some('-') => {
                    text.push('\n');
                    continue;
                }
                _ => return Err(Error::rejected("SMTP reply is malformed")),
            }
        }
    }

    /// One command + reply. The failure carries its shape for CAD-786
    /// outcome classification: an answered refusal keeps its code,
    /// a silent wire keeps the same refusal text CAD-785 produced.
    fn command(&mut self, text: &str, expect: &[u16]) -> std::result::Result<SmtpLine, Fail> {
        if text.contains(['\r', '\n']) {
            return Err(Fail::Io("SMTP command carries a line break".to_string()));
        }
        self.writer()
            .write_all(format!("{text}\r\n").as_bytes())
            .map_err(|_| Fail::Io("SMTP transport failed".to_string()))?;
        self.writer()
            .flush()
            .map_err(|_| Fail::Io("SMTP transport failed".to_string()))?;
        let reply = self.read_reply().map_err(|e| Fail::Io(e.to_string()))?;
        if !expect.contains(&reply.code) {
            return Err(Fail::Refused(
                reply.code,
                format!("SMTP server refused the command with code {}", reply.code),
            ));
        }
        Ok(reply)
    }
}

/// How one submission ended, classified for the delivery ledger
/// (CAD-786). The test-send path maps it back to its old
/// accepted/refused shape; the campaign worker translates it into
/// durable row states. `message`/`code` carry the server's answer —
/// bounded and secret-screened before they land anywhere.
pub enum SmtpOutcome {
    /// 250 after end-of-data: the server took the message.
    Accepted { code: u16, message: String },
    /// 4xx at any stage — not accepted, safe to retry later.
    Deferred { code: u16, message: String },
    /// 5xx — permanent.
    Rejected { code: u16, message: String },
    /// The exchange failed before the end-of-data `.` was written —
    /// the server provably never saw a complete message; retry is safe.
    NotSubmitted { message: String },
    /// The wire broke after the `.` terminator was written but
    /// before a reply was read — acceptance is unknowable, so the
    /// row is NEVER retried; the operator resolves it.
    Uncertain { message: String },
}

/// Where a dialog step failed.
enum Fail {
    /// The server answered with a code outside the expected set;
    /// the text is the same refusal CAD-785 produced.
    Refused(u16, String),
    /// The exchange broke before a reply was read — connect, TLS,
    /// IO or a malformed/absent reply.
    Io(String),
}

fn refused_outcome(code: u16, message: String) -> SmtpOutcome {
    if (400..500).contains(&code) {
        SmtpOutcome::Deferred { code, message }
    } else {
        SmtpOutcome::Rejected { code, message }
    }
}

fn fail_outcome(fail: Fail, data_written: bool) -> SmtpOutcome {
    match fail {
        Fail::Refused(code, message) => refused_outcome(code, message),
        Fail::Io(message) => {
            if data_written {
                SmtpOutcome::Uncertain { message }
            } else {
                SmtpOutcome::NotSubmitted { message }
            }
        }
    }
}

/// Screen any server-supplied text against the secret before it
/// becomes a receipt, a row value, an error, or a log line. A hit
/// withholds the text, never the verdict.
fn screened(text: &str, secret: &[u8]) -> String {
    if carries_secret(text, secret) {
        return "SMTP submission refused".to_string();
    }
    let mut excerpt: String = text.chars().take(RECEIPT_TEXT_CAP).collect();
    if excerpt.len() != text.len() {
        excerpt.push('…');
    }
    excerpt
}

/// Submit one message, returning the classified outcome (CAD-786).
/// `Err` is only for local grammar failures — enrollment validation,
/// message assembly — never a wire result. Every `Ok` variant's
/// text is already secret-screened.
pub fn send_outcome(
    envelope: &SmtpEnvelope,
    message: &SmtpMessage,
    content_digest: &str,
    extra_ca_pem: Option<&[u8]>,
) -> Result<SmtpOutcome> {
    validate_port_tls(&envelope.host, envelope.port, &envelope.tls_mode)?;
    if envelope.secret.iter().any(|b| *b == b'\r' || *b == b'\n') {
        return Err(Error::rejected(
            "SMTP secret exceeds its supported shape or bounds",
        ));
    }
    let secret_text = std::str::from_utf8(&envelope.secret)
        .map_err(|_| Error::rejected("SMTP secret exceeds its supported shape or bounds"))?;
    let body = assemble_message(envelope, message, content_digest)?;
    let tls = tls_config(extra_ca_pem)?;
    let outcome = send_inner(envelope, message, &body, &tls, secret_text);
    Ok(match outcome {
        Ok(receipt) => SmtpOutcome::Accepted {
            code: receipt.code,
            message: screened(&receipt.message, &envelope.secret),
        },
        Err(SmtpOutcome::Accepted { code, message }) => SmtpOutcome::Accepted {
            code,
            message: screened(&message, &envelope.secret),
        },
        Err(SmtpOutcome::Deferred { code, message }) => SmtpOutcome::Deferred {
            code,
            message: screened(&message, &envelope.secret),
        },
        Err(SmtpOutcome::Rejected { code, message }) => SmtpOutcome::Rejected {
            code,
            message: screened(&message, &envelope.secret),
        },
        Err(SmtpOutcome::NotSubmitted { message }) => SmtpOutcome::NotSubmitted {
            message: screened(&message, &envelope.secret),
        },
        Err(SmtpOutcome::Uncertain { message }) => SmtpOutcome::Uncertain {
            message: screened(&message, &envelope.secret),
        },
    })
}

/// Submit one message (CAD-785 test-send shape kept verbatim):
/// `Ok` is an SMTP-accepted receipt; every other outcome is the
/// same `rejected` refusal text the old pipeline produced, screened
/// against the secret exactly as before.
pub fn send(
    envelope: &SmtpEnvelope,
    message: &SmtpMessage,
    content_digest: &str,
    extra_ca_pem: Option<&[u8]>,
) -> Result<SmtpReceipt> {
    match send_outcome(envelope, message, content_digest, extra_ca_pem)? {
        SmtpOutcome::Accepted { code, message } => Ok(SmtpReceipt {
            accepted: true,
            code,
            message,
        }),
        SmtpOutcome::Deferred { message, .. }
        | SmtpOutcome::Rejected { message, .. }
        | SmtpOutcome::NotSubmitted { message }
        | SmtpOutcome::Uncertain { message } => Err(Error::rejected(message)),
    }
}

fn carries_secret(text: &str, secret: &[u8]) -> bool {
    let secret = String::from_utf8_lossy(secret);
    if secret.is_empty() {
        return false;
    }
    if text.contains(secret.as_ref()) {
        return true;
    }
    let chars: Vec<_> = secret.chars().collect();
    chars
        .windows(8)
        .any(|fragment| text.contains(&fragment.iter().collect::<String>()))
}

fn send_inner(
    envelope: &SmtpEnvelope,
    message: &SmtpMessage,
    body: &str,
    tls: &Arc<rustls::ClientConfig>,
    secret: &str,
) -> std::result::Result<SmtpReceipt, SmtpOutcome> {
    if envelope.tls_mode == TLS_IMPLICIT {
        send_implicit_tls(envelope, message, body, tls, secret)
    } else {
        send_starttls(envelope, message, body, tls, secret)
    }
}

/// Port 465: TLS first, then the SMTP dialog inside the verified
/// tunnel. The greeting is read only after the handshake — a server
/// that talks plaintext here is a hard refusal, not a fallback.
fn send_implicit_tls(
    envelope: &SmtpEnvelope,
    message: &SmtpMessage,
    body: &str,
    tls: &Arc<rustls::ClientConfig>,
    secret: &str,
) -> std::result::Result<SmtpReceipt, SmtpOutcome> {
    // TLS first: the handshake (with certificate verification)
    // completes on first I/O, before the SMTP greeting is read —
    // a server that talks plaintext here errors, never downgrades.
    let stream = SmtpSession::dial(&envelope.host, envelope.port).map_err(|e| {
        SmtpOutcome::NotSubmitted {
            message: e.to_string(),
        }
    })?;
    let mut session =
        SmtpSession::tls(stream, &envelope.host, tls).map_err(|_| SmtpOutcome::NotSubmitted {
            message: "SMTP TLS negotiation failed".to_string(),
        })?;
    // The first read completes the handshake with certificate
    // verification: a rogue certificate or a plaintext speaker
    // fails here as a TLS refusal, never as a downgrade.
    let greeting = session
        .read_reply()
        .map_err(|_| SmtpOutcome::NotSubmitted {
            message: "SMTP TLS negotiation failed".to_string(),
        })?;
    if greeting.code != 220 {
        return Err(refused_outcome(
            greeting.code,
            "SMTP server greeting refused".to_string(),
        ));
    }
    dialog_authenticated(&mut session, envelope, message, body, secret)
}

/// Port 587: plaintext greeting, EHLO, then a MANDATORY STARTTLS —
/// a server that does not advertise it refuses before AUTH. AUTH
/// and the message run only inside the verified tunnel.
fn send_starttls(
    envelope: &SmtpEnvelope,
    message: &SmtpMessage,
    body: &str,
    tls: &Arc<rustls::ClientConfig>,
    secret: &str,
) -> std::result::Result<SmtpReceipt, SmtpOutcome> {
    let stream = SmtpSession::dial(&envelope.host, envelope.port).map_err(|e| {
        SmtpOutcome::NotSubmitted {
            message: e.to_string(),
        }
    })?;
    let mut session = SmtpSession::plain(stream);
    let greeting = session
        .read_reply()
        .map_err(|e| SmtpOutcome::NotSubmitted {
            message: e.to_string(),
        })?;
    if greeting.code != 220 {
        return Err(refused_outcome(
            greeting.code,
            "SMTP server greeting refused".to_string(),
        ));
    }
    let ehlo = session
        .command(&format!("EHLO {EHLO_NAME}"), &[250])
        .map_err(|f| fail_outcome(f, false))?;
    if !advertises(&ehlo.text, "STARTTLS") {
        return Err(SmtpOutcome::NotSubmitted {
            message: "SMTP server does not offer STARTTLS — plaintext submission refused"
                .to_string(),
        });
    }
    session
        .command("STARTTLS", &[220])
        .map_err(|f| fail_outcome(f, false))?;
    // RFC 3207: the plaintext session is gone here; everything
    // below — including the second EHLO — runs inside the tunnel.
    // The post-upgrade EHLO completes the handshake first: a rogue
    // certificate fails here as a TLS refusal, before AUTH.
    let mut session =
        session
            .upgrade_tls(&envelope.host, tls)
            .map_err(|_| SmtpOutcome::NotSubmitted {
                message: "SMTP TLS negotiation failed".to_string(),
            })?;
    session
        .command(&format!("EHLO {EHLO_NAME}"), &[250])
        .map_err(|f| match f {
            Fail::Io(_) => SmtpOutcome::NotSubmitted {
                message: "SMTP TLS negotiation failed".to_string(),
            },
            Fail::Refused(code, message) => refused_outcome(code, message),
        })?;
    dialog_authenticated(&mut session, envelope, message, body, secret)
}

/// The EHLO client name is fixed operator-neutral text — never the
/// credential, never customer data.
const EHLO_NAME: &str = "cadence-smtp";

/// Post-handshake dialog shared by both ports: EHLO, mandatory
/// AUTH, one message, QUIT — all inside the verified tunnel. AUTH
/// LOGIN is attempted first (widest server support), then PLAIN on
/// a 5xx refusal. No credential ever crosses before this point. The
/// moment the `.` terminator is written the outcome can no longer
/// be `NotSubmitted` — a break after it is `Uncertain`.
fn dialog_authenticated(
    session: &mut SmtpSession,
    envelope: &SmtpEnvelope,
    message: &SmtpMessage,
    body: &str,
    secret: &str,
) -> std::result::Result<SmtpReceipt, SmtpOutcome> {
    let ehlo = session
        .command(&format!("EHLO {EHLO_NAME}"), &[250])
        .map_err(|f| fail_outcome(f, false))?;
    let mechanisms = auth_mechanisms(&ehlo.text);
    if mechanisms.is_empty() {
        return Err(SmtpOutcome::NotSubmitted {
            message: "SMTP server offers no authentication — unauthenticated submission refused"
                .to_string(),
        });
    }
    let mut authenticated = false;
    if mechanisms.iter().any(|name| name == "LOGIN") {
        authenticated = try_auth_login(session, &envelope.username, secret)
            .map_err(|f| fail_outcome(f, false))?;
    }
    if !authenticated && mechanisms.iter().any(|name| name == "PLAIN") {
        authenticated = try_auth_plain(session, &envelope.username, secret)
            .map_err(|f| fail_outcome(f, false))?;
    }
    if !authenticated {
        return Err(SmtpOutcome::NotSubmitted {
            message: "SMTP server offers no supported authentication — submission refused"
                .to_string(),
        });
    }
    session
        .command(&format!("MAIL FROM:<{}>", envelope.sender), &[250])
        .map_err(|f| fail_outcome(f, false))?;
    session
        .command(&format!("RCPT TO:<{}>", message.to), &[250, 251])
        .map_err(|f| fail_outcome(f, false))?;
    session
        .command("DATA", &[354])
        .map_err(|f| fail_outcome(f, false))?;
    // Dot-stuff per RFC 5321 §4.5.2, then the terminator. The
    // assembled body is CRLF throughout.
    let mut data = String::with_capacity(body.len() + 16);
    for line in body.split("\r\n") {
        if line.starts_with('.') {
            data.push('.');
        }
        data.push_str(line);
        data.push_str("\r\n");
    }
    data.push_str(".\r\n");
    session
        .writer()
        .write_all(data.as_bytes())
        .map_err(|_| SmtpOutcome::NotSubmitted {
            message: "SMTP transport failed".to_string(),
        })?;
    session
        .writer()
        .flush()
        .map_err(|_| SmtpOutcome::NotSubmitted {
            message: "SMTP transport failed".to_string(),
        })?;
    // The `.` terminator is on the wire: from here the server may
    // have accepted. Any further transport failure is `Uncertain`.
    let accepted = session.read_reply().map_err(|e| SmtpOutcome::Uncertain {
        message: e.to_string(),
    })?;
    if accepted.code != 250 {
        return Err(refused_outcome(
            accepted.code,
            format!(
                "SMTP server refused the message with code {}",
                accepted.code
            ),
        ));
    }
    // QUIT is courtesy after acceptance: a failure here never
    // un-accepts the message, so it is best-effort.
    let _ = session.command("QUIT", &[221]);
    Ok(SmtpReceipt {
        accepted: true,
        code: accepted.code,
        message: accepted.text,
    })
}

fn try_auth_login(
    session: &mut SmtpSession,
    username: &str,
    secret: &str,
) -> std::result::Result<bool, Fail> {
    let challenge = session.command("AUTH LOGIN", &[234, 235, 334, 500, 502, 504, 535])?;
    if challenge.code == 235 {
        return Ok(true);
    }
    if challenge.code != 334 {
        return Ok(false);
    }
    let user = session.command(&base64_encode(username.as_bytes()), &[234, 235, 334])?;
    if user.code == 235 {
        return Ok(true);
    }
    if user.code != 334 {
        return Err(Fail::Refused(
            user.code,
            "SMTP authentication refused".to_string(),
        ));
    }
    let pass = session.command(&base64_encode(secret.as_bytes()), &[235, 535])?;
    match pass.code {
        235 => Ok(true),
        535 => Err(Fail::Refused(
            535,
            "SMTP authentication refused".to_string(),
        )),
        code => Err(Fail::Refused(
            code,
            "SMTP authentication refused".to_string(),
        )),
    }
}

fn try_auth_plain(
    session: &mut SmtpSession,
    username: &str,
    secret: &str,
) -> std::result::Result<bool, Fail> {
    let mut combined = Vec::with_capacity(username.len() + secret.len() + 2);
    combined.push(0u8);
    combined.extend_from_slice(username.as_bytes());
    combined.push(0u8);
    combined.extend_from_slice(secret.as_bytes());
    let reply = session.command(
        &format!("AUTH PLAIN {}", base64_encode(&combined)),
        &[235, 535, 500, 502, 504],
    )?;
    match reply.code {
        235 => Ok(true),
        535 => Err(Fail::Refused(
            535,
            "SMTP authentication refused".to_string(),
        )),
        _ => Ok(false),
    }
}

/// First whitespace-delimited token per EHLO line, uppercased.
fn ehlo_capabilities(ehlo: &str) -> Vec<String> {
    ehlo.lines()
        .filter_map(|line| {
            line.split_whitespace()
                .next()
                .map(|token| token.to_ascii_uppercase())
        })
        .collect()
}

fn advertises(ehlo: &str, keyword: &str) -> bool {
    ehlo_capabilities(ehlo)
        .iter()
        .any(|capability| capability == keyword)
}

/// Mechanisms named by an `AUTH` EHLO line, uppercased.
fn auth_mechanisms(ehlo: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in ehlo.lines() {
        let mut tokens = line.split_whitespace();
        if tokens
            .next()
            .is_some_and(|head| head.eq_ignore_ascii_case("AUTH"))
        {
            out.extend(tokens.map(|name| name.to_ascii_uppercase()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_grammar_accepts_only_the_two_encrypted_submissions() {
        let base = json!({
            "host": "mail.example.com",
            "username": "sender",
            "secret": "s3cret-value",
            "sender": "news@example.com",
        });
        for (port, mode) in [(465u64, "implicit"), (587u64, "starttls")] {
            let mut params = base.clone();
            params["port"] = json!(port);
            params["tls_mode"] = json!(mode);
            parse_enrollment(&params).unwrap();
        }
        for (port, mode) in [
            (25u64, "starttls"),
            (587u64, "implicit"),
            (465u64, "starttls"),
            (2525u64, "starttls"),
            (443u64, "implicit"),
        ] {
            let mut params = base.clone();
            params["port"] = json!(port);
            params["tls_mode"] = json!(mode);
            assert!(parse_enrollment(&params).is_err(), "{port}/{mode}");
        }
        // The isolated-test host may use ephemeral loopback ports,
        // but the TLS mode stays mandatory with no plaintext option.
        for (port, mode) in [
            (465u64, "implicit"),
            (587u64, "starttls"),
            (40211u64, "implicit"),
            (51997u64, "starttls"),
        ] {
            let mut params = base.clone();
            params["host"] = json!("localhost");
            params["port"] = json!(port);
            params["tls_mode"] = json!(mode);
            parse_enrollment(&params).unwrap();
        }
        for mode in ["plain", "opportunistic", "none"] {
            let mut params = base.clone();
            params["host"] = json!("localhost");
            params["port"] = json!(40211u64);
            params["tls_mode"] = json!(mode);
            assert!(parse_enrollment(&params).is_err(), "localhost/{mode}");
        }
    }

    #[test]
    fn enrollment_refuses_plaintext_surface_and_placeholders() {
        let good = || {
            json!({
                "host": "mail.example.com", "port": 587, "tls_mode": "starttls",
                "username": "sender", "secret": "s3cret-value",
                "sender": "news@example.com",
            })
        };
        // Opaque token material never participates.
        let mut params = good();
        params["token"] = json!("opaque");
        assert!(parse_enrollment(&params).is_err());
        // IP literals are not DNS submission hosts.
        for host in ["127.0.0.1", "::1", "10.0.0.9"] {
            let mut params = good();
            params["host"] = json!(host);
            assert!(parse_enrollment(&params).is_err(), "{host}");
        }
        // Preview placeholders are never senders.
        for sender in ["noreply@cadence.invalid", "x@y", "not-an-address", "a@b"] {
            let mut params = good();
            params["sender"] = json!(sender);
            assert!(parse_enrollment(&params).is_err(), "{sender}");
        }
        // `localhost` is the isolated-test allowance.
        let mut params = good();
        params["host"] = json!("localhost");
        params["port"] = json!(465);
        params["tls_mode"] = json!("implicit");
        parse_enrollment(&params).unwrap();
    }

    #[test]
    fn sender_name_refuses_quoted_string_breakers() {
        // `header_address` interpolates the display name into a
        // quoted string with no escaping, so `"` and `\` would both
        // break out or splice an escape into the header.
        assert!(validate_sender_name("CRM News").is_ok());
        assert!(validate_sender_name("").is_ok());
        for name in ["News \"dept\"", "News \\ sales", "a\nb", "héllo"] {
            assert!(validate_sender_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn custody_round_trips_and_rejects_foreign_documents() {
        let enrollment = SmtpEnrollment {
            host: "mail.example.com".into(),
            port: 587,
            tls_mode: "starttls".into(),
            username: "sender".into(),
            secret: b"s3cret".to_vec(),
            sender: "news@example.com".into(),
            sender_name: "News".into(),
        };
        let bytes = custody_bytes(&enrollment).unwrap();
        let (envelope, projection) = custody_decode(&bytes).unwrap();
        assert_eq!(envelope.host, "mail.example.com");
        assert_eq!(projection.sender, "news@example.com");
        assert!(!projection.to_json().to_string().contains("s3cret"));
        assert!(!String::from_utf8_lossy(&bytes).contains("token"));
        let mut foreign: Value = serde_json::from_slice(&bytes).unwrap();
        foreign["provider"] = json!("other");
        assert!(custody_decode(&serde_json::to_vec(&foreign).unwrap()).is_err());
        assert!(custody_decode(b"not json").is_err());
    }

    #[test]
    fn descriptor_validates_against_the_reviewed_table() {
        use crate::platform::PlatformAdapter;
        let adapter = SmtpAdapter::default();
        let descriptor = adapter.connection_descriptor().unwrap();
        descriptor.validate(adapter.table()).unwrap();
        assert_eq!(descriptor.enrollment_shapes, vec!["smtp"]);
        // Foreign shapes — `password`, `oauth` — refuse even though
        // the sibling `token` shape stays allowed by the grammar.
        for shape in ["password", "oauth", "api_key"] {
            let mut bad = descriptor.clone();
            bad.enrollment_shapes = vec![shape.into()];
            assert!(bad.validate(adapter.table()).is_err(), "{shape}");
        }
    }
}
