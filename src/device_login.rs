//! Board sign-in through the AgenticOS device grant (CAD-777).
//!
//! The board backend acts like `cadence login`: it requests a device
//! code from the issuer, shows the user code + verification link, polls
//! for the credential, verifies it through `/v1/runtime/session`, and
//! only then mints its own board session (via
//! [`crate::operator_auth::Auth::open_device`]). The `agc_` credential
//! is verified, never stored as a board session and never returned to
//! the browser.
//!
//! The issuer surface is the AOS-49 device grant: flat OAuth JSON on
//! `/v1/device/code` and `/v1/device/token` (RFC 8628 shape), bearer
//! check on `/v1/runtime/session`. No AgenticOS browser session, cookie
//! or OTP value ever crosses this module — the human types their email
//! OTP on the issuer's approval page, which lives outside Cadence.
//!
//! Every refusal is a fixed string. Issuer bodies, URLs, codes and
//! tokens are never echoed: any of them can carry secrets.

use std::time::Duration;

use serde_json::{json, Value};

use crate::error::{Error, Result};

/// Remote (device-grant) board sessions live shorter than on-host ones:
/// 12 h idle, 24 h absolute. See `open_device`.
pub const REMOTE_IDLE_SECS: i64 = 12 * 3600;
pub const REMOTE_ABSOLUTE_SECS: i64 = 24 * 3600;

const BODY_CAP: u64 = 64 * 1024;
const TOKEN_CAP: usize = 4096;
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
/// The login audience this module requests: `read draft`, like the CLI.
/// Provider audiences (`provider.read`/`provider.draft`) are a separate
/// grant and are never requested here.
const LOGIN_SCOPE: &str = "read draft";

fn rejected(message: &str) -> Error {
    Error::rejected(message)
}

/// An issuer origin plus the exact workspace the sign-in is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceConfig {
    issuer: String,
    org: String,
}

impl DeviceConfig {
    /// `issuer` is an HTTPS origin without path, userinfo, query or
    /// fragment; `org` is a workspace ID. Both rules mirror
    /// `remote_auth` so the CLI and the board accept the same values.
    pub fn new(issuer: &str, org: &str) -> Result<Self> {
        Ok(Self {
            issuer: issuer_origin(issuer)?,
            org: validate_org(org)?,
        })
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    pub fn org(&self) -> &str {
        &self.org
    }
}

fn issuer_origin(input: &str) -> Result<String> {
    let uri: ureq::http::Uri = input
        .parse()
        .map_err(|_| rejected("Invalid issuer origin"))?;
    let authority = uri
        .authority()
        .ok_or_else(|| rejected("Issuer must be an HTTPS origin"))?;
    let secure = uri.scheme_str() == Some("https");
    // Loopback issuers (local fixtures, staging on this host) are
    // allowed over plain HTTP in every build: the issuer is
    // operator-configured (flag/env/persisted), never caller input, so
    // nothing the network says chooses it, and bodies stay on-host.
    // This mirrors the board-identity issuer, which already accepts
    // local http:// origins (CAD-526).
    let loopback = uri.scheme_str() == Some("http")
        && matches!(uri.host(), Some("127.0.0.1") | Some("localhost"));
    if (!secure && !loopback)
        || authority.as_str().contains('@')
        || input.contains('#')
        || uri.path_and_query().is_some_and(|p| p.as_str() != "/")
    {
        return Err(rejected(
            "Issuer must be an HTTPS origin without credentials, path, query or fragment",
        ));
    }
    Ok(format!(
        "{}://{}",
        uri.scheme_str().unwrap_or("https"),
        authority
    ))
}

fn validate_org(org: &str) -> Result<String> {
    if org.is_empty()
        || org.len() > 200
        || !org
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(rejected("Organization must be a workspace ID"));
    }
    Ok(org.to_string())
}

fn validate_token(token: &str) -> Result<()> {
    if token.is_empty()
        || token.len() > TOKEN_CAP
        || !token.bytes().all(|b| (33..=126).contains(&b))
    {
        return Err(rejected(
            "Credential must be one bounded ASCII token without whitespace",
        ));
    }
    Ok(())
}

/// Subjects the daemon accepts from a verified device grant: the same
/// workspace-ID grammar the issuer uses for companies and principals.
pub fn validate_subject(raw: &str) -> Result<()> {
    if raw.is_empty()
        || raw.len() > 200
        || !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(rejected("Subject must be a workspace-style ID"));
    }
    Ok(())
}

/// What the board shows the human. `device_code` is intentionally
/// absent: the board keeps it server-side in its pending map and the
/// browser polls with the pending id instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeDisplay {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// The issuer's answer to one poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Poll {
    /// Still waiting — poll again after the interval.
    Pending,
    /// Polling too fast — back off.
    SlowDown,
    /// The human denied the grant. Terminal: start over.
    Denied,
    /// The code expired or was already exchanged. Terminal: start over.
    Gone,
    /// Approved. The credential still needs `verify_session` before it
    /// authorizes anything.
    Approved {
        token: String,
        workspace_id: String,
        scopes: Vec<String>,
    },
}

/// A credential the issuer verified for exactly the requested org.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    pub subject_id: String,
    pub org: String,
}

/// The HTTP this module needs. The live impl speaks ureq like
/// `remote_auth` (no redirects, bounded bodies); tests script a fake.
/// Object-safe and shareable: the board keeps one behind `Arc`.
pub trait IssuerTransport: Send + Sync {
    fn post(&self, url: &str, body: Value) -> Result<(u16, Value)>;
    fn get(&self, url: &str, bearer: &str) -> Result<(u16, Value)>;
}

pub struct UreqTransport {
    http: ureq::Agent,
}

impl UreqTransport {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(HTTP_TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        Self {
            http: ureq::Agent::new_with_config(config),
        }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

fn read_response(mut response: ureq::http::Response<ureq::Body>) -> Result<(u16, Value)> {
    use std::io::Read;
    let status = response.status().as_u16();
    if (300..400).contains(&status) {
        return Err(rejected(
            "Issuer redirect refused; configure the exact issuer origin",
        ));
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(BODY_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| rejected("Unable to read issuer response"))?;
    if bytes.len() as u64 > BODY_CAP {
        return Err(rejected("Issuer response exceeds size limit"));
    }
    let value = serde_json::from_slice(&bytes)
        .map_err(|_| rejected("Issuer returned an invalid response"))?;
    Ok((status, value))
}

impl IssuerTransport for UreqTransport {
    fn post(&self, url: &str, body: Value) -> Result<(u16, Value)> {
        let response = self
            .http
            .post(url)
            .send_json(body)
            .map_err(|_| rejected("Issuer request failed"))?;
        read_response(response)
    }

    fn get(&self, url: &str, bearer: &str) -> Result<(u16, Value)> {
        validate_token(bearer)?;
        let response = self
            .http
            .get(url)
            .header("Authorization", format!("Bearer {bearer}"))
            .call()
            .map_err(|_| rejected("Issuer verification failed"))?;
        read_response(response)
    }
}

fn check_verification(display: &Value) -> Result<CodeDisplay> {
    let user_code = display
        .get("user_code")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| (33..=126).contains(&b)))
        .ok_or_else(|| rejected("Issuer returned an invalid device code"))?;
    let verification_uri = display
        .get("verification_uri")
        .and_then(Value::as_str)
        .filter(|s| valid_https_url(s))
        .ok_or_else(|| rejected("Issuer returned an invalid verification URL"))?;
    let complete = display
        .get("verification_uri_complete")
        .and_then(Value::as_str)
        .filter(|s| valid_https_url(s))
        .ok_or_else(|| rejected("Issuer returned an invalid verification URL"))?;
    let expires_in = display
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|n| (30..=3600).contains(n))
        .ok_or_else(|| rejected("Issuer returned an invalid device code"))?;
    let interval = display
        .get("interval")
        .and_then(Value::as_u64)
        .filter(|n| (1..=60).contains(n))
        .unwrap_or(5);
    Ok(CodeDisplay {
        user_code: user_code.to_string(),
        verification_uri: verification_uri.to_string(),
        verification_uri_complete: complete.to_string(),
        expires_in,
        interval,
    })
}

fn valid_https_url(raw: &str) -> bool {
    if raw.contains(['\n', '\r', ' ']) || raw.len() > 2048 {
        return false;
    }
    let Ok(uri): std::result::Result<ureq::http::Uri, _> = raw.parse() else {
        return false;
    };
    let secure = uri.scheme_str() == Some("https");
    // Same loopback allowance as `issuer_origin`: operator-configured
    // fixtures only, never caller input.
    let loopback = uri.scheme_str() == Some("http")
        && matches!(uri.host(), Some("127.0.0.1") | Some("localhost"));
    if !secure && !loopback {
        return false;
    }
    let Some(authority) = uri.authority() else {
        return false;
    };
    !authority.as_str().contains('@')
}

/// The raw device code the board keeps server-side. Validated like any
/// credential: one bounded ASCII token, never logged or shown.
pub fn check_device_code(raw: &Value) -> Result<String> {
    let code = raw
        .get("device_code")
        .and_then(Value::as_str)
        .ok_or_else(|| rejected("Issuer returned an invalid device code"))?;
    validate_token(code)?;
    Ok(code.to_string())
}

/// Request a device code for the login audience. Returns what the
/// human needs plus the server-side code the pending map keeps.
pub fn request_code(
    transport: &dyn IssuerTransport,
    config: &DeviceConfig,
) -> Result<(CodeDisplay, String)> {
    let (status, value) = transport.post(
        &format!("{}/v1/device/code", config.issuer),
        json!({"scope": LOGIN_SCOPE}),
    )?;
    if status != 200 {
        return Err(rejected("Issuer refused the device request"));
    }
    let display = check_verification(&value)?;
    let code = check_device_code(&value)?;
    Ok((display, code))
}

/// One poll of the device grant.
pub fn poll_token(
    transport: &dyn IssuerTransport,
    config: &DeviceConfig,
    device_code: &str,
) -> Result<Poll> {
    validate_token(device_code)?;
    let (status, value) = transport.post(
        &format!("{}/v1/device/token", config.issuer),
        json!({"device_code": device_code}),
    )?;
    if status == 200 {
        let token = value
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| rejected("Issuer returned an invalid credential"))?;
        validate_token(token)?;
        let workspace_id = value
            .get("workspace_id")
            .and_then(Value::as_str)
            .ok_or_else(|| rejected("Issuer returned an invalid credential"))?;
        if validate_org(workspace_id).is_err() || workspace_id != config.org {
            return Err(rejected(
                "Issuer approved a different organization — start over",
            ));
        }
        let scopes = value
            .get("scope")
            .and_then(Value::as_str)
            .map(|s| s.split_whitespace().map(str::to_string).collect::<Vec<_>>())
            .unwrap_or_default();
        if !scopes.iter().any(|s| s == "read") {
            return Err(rejected("Issuer granted no read scope — start over"));
        }
        return Ok(Poll::Approved {
            token: token.to_string(),
            workspace_id: workspace_id.to_string(),
            scopes,
        });
    }
    // Flat OAuth errors (device.ts speaks `{error}`, not the envelope).
    match value.get("error").and_then(Value::as_str) {
        Some("authorization_pending") => Ok(Poll::Pending),
        Some("slow_down") => Ok(Poll::SlowDown),
        Some("access_denied") => Ok(Poll::Denied),
        Some("expired_token") | Some("invalid_grant") => Ok(Poll::Gone),
        _ => Err(rejected("Issuer returned an invalid token response")),
    }
}

/// Verify an approved credential against `/v1/runtime/session`. Same
/// rules as the CLI (`remote_auth`): named non-anonymous principal,
/// exact org, `read` among a small allowlisted scope set.
pub fn verify_session(
    transport: &dyn IssuerTransport,
    config: &DeviceConfig,
    token: &str,
) -> Result<Verified> {
    validate_token(token)?;
    let (status, value) = transport.get(&format!("{}/v1/runtime/session", config.issuer), token)?;
    if status != 200 {
        return Err(rejected(
            "Issuer rejected the credential; check expiry, revocation and read scope",
        ));
    }
    parse_session(&value, &config.org)
}

fn parse_session(value: &Value, org: &str) -> Result<Verified> {
    let data = &value["data"];
    let subject_id = data["subject"]["id"].as_str().ok_or_else(|| {
        rejected("Issuer did not verify a named read principal for the requested organization")
    })?;
    if value["ok"] != true
        || data["workspace"]["id"].as_str() != Some(org)
        || data["subject"]["anonymous"] != false
        || validate_org(subject_id).is_err()
        || data["scopes"].as_array().is_none_or(|scopes| {
            scopes.is_empty()
                || scopes.len() > 3
                || !scopes.iter().any(|s| s == "read")
                || scopes
                    .iter()
                    .any(|scope| !matches!(scope.as_str(), Some("read" | "draft" | "send")))
                || scopes
                    .iter()
                    .enumerate()
                    .any(|(i, scope)| scopes[..i].contains(scope))
        })
    {
        return Err(rejected(
            "Issuer did not verify a named read principal for the requested organization",
        ));
    }
    Ok(Verified {
        subject_id: subject_id.to_string(),
        org: org.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Scripted fake issuer: each call pops the next response.
    /// Anything unscripted (or a wrong URL/body shape) is a test bug,
    /// reported as an unexpected-call refusal.
    struct Fake {
        calls: Mutex<Vec<(String, Value, Option<String>)>>,
        answers: Mutex<VecDeque<(u16, Value)>>,
    }

    impl Fake {
        fn new(answers: Vec<(u16, Value)>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                answers: Mutex::new(answers.into()),
            }
        }

        fn called(&self) -> Vec<(String, Value, Option<String>)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl IssuerTransport for Fake {
        fn post(&self, url: &str, body: Value) -> Result<(u16, Value)> {
            self.calls
                .lock()
                .unwrap()
                .push((url.to_string(), body, None));
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| rejected("test fake issuer ran out of answers"))
        }

        fn get(&self, url: &str, bearer: &str) -> Result<(u16, Value)> {
            validate_token(bearer)?;
            self.calls.lock().unwrap().push((
                url.to_string(),
                Value::Null,
                Some(bearer.to_string()),
            ));
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| rejected("test fake issuer ran out of answers"))
        }
    }

    fn config() -> DeviceConfig {
        DeviceConfig::new("http://127.0.0.1:9", "ws_company").unwrap()
    }

    fn code_answer() -> Value {
        json!({
            "device_code": "agd_testcode",
            "user_code": "ABCD-1234",
            "verification_uri": "http://127.0.0.1:9/verify",
            "verification_uri_complete": "http://127.0.0.1:9/verify?code=ABCD-1234",
            "expires_in": 600,
            "interval": 5
        })
    }

    fn session_answer() -> Value {
        json!({
            "ok": true,
            "data": {
                "workspace": {"id": "ws_company"},
                "subject": {"anonymous": false, "id": "op_1"},
                "scopes": ["read", "draft"]
            }
        })
    }

    #[test]
    fn config_rejects_bad_issuer_and_org() {
        // Plain HTTP to a non-loopback host is refused in every build;
        // loopback fixtures are the documented operator exception.
        assert!(DeviceConfig::new("http://example.com", "ws_company").is_err());
        assert!(DeviceConfig::new("https://issuer.example/x", "ws_company").is_err());
        assert!(DeviceConfig::new("https://issuer.example", "not a workspace!").is_err());
        assert!(DeviceConfig::new("https://issuer.example", "").is_err());
        assert!(DeviceConfig::new("http://127.0.0.1:9", "ws_company").is_ok());
    }

    #[test]
    fn request_code_posts_login_scope_and_splits_display_from_secret() {
        let fake = Fake::new(vec![(200, code_answer())]);
        let (display, code) = request_code(&fake, &config()).unwrap();
        assert_eq!(display.user_code, "ABCD-1234");
        assert_eq!(display.expires_in, 600);
        assert_eq!(code, "agd_testcode");
        let calls = fake.called();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].0.ends_with("/v1/device/code"));
        assert_eq!(calls[0].1["scope"], json!("read draft"));
    }

    #[test]
    fn request_code_refuses_malformed_and_error_status() {
        for answer in [
            (400, json!({"error": "invalid_request"})),
            (200, json!({"user_code": "ABCD-1234"})),
            (
                200,
                json!({
                    "device_code": "agd_x", "user_code": "ABCD-1234",
                    "verification_uri": "http://evil.example/verify",
                    "verification_uri_complete": "http://evil.example/verify?code=x",
                    "expires_in": 600, "interval": 5
                }),
            ),
            (
                200,
                json!({
                    "device_code": "has whitespace", "user_code": "ABCD-1234",
                    "verification_uri": "http://127.0.0.1:9/verify",
                    "verification_uri_complete": "http://127.0.0.1:9/verify?code=x",
                    "expires_in": 600, "interval": 5
                }),
            ),
        ] {
            let fake = Fake::new(vec![answer]);
            assert!(request_code(&fake, &config()).is_err());
        }
    }

    #[test]
    fn poll_maps_every_terminal_state() {
        let pending = Fake::new(vec![(400, json!({"error": "authorization_pending"}))]);
        assert_eq!(
            poll_token(&pending, &config(), "agd_x").unwrap(),
            Poll::Pending
        );
        let slow = Fake::new(vec![(400, json!({"error": "slow_down"}))]);
        assert_eq!(
            poll_token(&slow, &config(), "agd_x").unwrap(),
            Poll::SlowDown
        );
        let denied = Fake::new(vec![(400, json!({"error": "access_denied"}))]);
        assert_eq!(
            poll_token(&denied, &config(), "agd_x").unwrap(),
            Poll::Denied
        );
        for error in ["expired_token", "invalid_grant"] {
            let gone = Fake::new(vec![(400, json!({"error": error}))]);
            assert_eq!(poll_token(&gone, &config(), "agd_x").unwrap(), Poll::Gone);
        }
        let weird = Fake::new(vec![(500, json!({"ok": false}))]);
        assert!(poll_token(&weird, &config(), "agd_x").is_err());
    }

    #[test]
    fn poll_approved_binds_exact_org_and_read_scope() {
        let approved = Fake::new(vec![(
            200,
            json!({
                "access_token": "agc_testtoken",
                "token_type": "bearer",
                "expires_in": 999,
                "scope": "read draft",
                "workspace_id": "ws_company"
            }),
        )]);
        let Poll::Approved {
            token,
            workspace_id,
            ..
        } = poll_token(&approved, &config(), "agd_x").unwrap()
        else {
            panic!("expected approval");
        };
        assert_eq!(token, "agc_testtoken");
        assert_eq!(workspace_id, "ws_company");

        // Different org on approval: fail, do not proceed.
        let other = Fake::new(vec![(
            200,
            json!({
                "access_token": "agc_testtoken",
                "scope": "read",
                "workspace_id": "ws_other"
            }),
        )]);
        assert!(poll_token(&other, &config(), "agd_x").is_err());

        // No read scope on approval: fail.
        let noscope = Fake::new(vec![(
            200,
            json!({
                "access_token": "agc_testtoken",
                "scope": "draft",
                "workspace_id": "ws_company"
            }),
        )]);
        assert!(poll_token(&noscope, &config(), "agd_x").is_err());
    }

    #[test]
    fn verify_session_enforces_named_principal_exact_org_and_scope_shape() {
        let fake = Fake::new(vec![(200, session_answer())]);
        let verified = verify_session(&fake, &config(), "agc_testtoken").unwrap();
        assert_eq!(verified.subject_id, "op_1");
        // Bearer travels as the Authorization header, never the body.
        let calls = fake.called();
        assert!(calls[0].0.ends_with("/v1/runtime/session"));
        assert_eq!(calls[0].2.as_deref(), Some("agc_testtoken"));

        // Non-200, anonymous, wrong org, missing read, bad scope, dupes.
        let bad = vec![
            (401, json!({"ok": false})),
            (
                200,
                json!({"ok": true, "data": {
                "workspace": {"id": "ws_company"},
                "subject": {"anonymous": true, "id": "op_1"},
                "scopes": ["read"]}}),
            ),
            (
                200,
                json!({"ok": true, "data": {
                "workspace": {"id": "ws_other"},
                "subject": {"anonymous": false, "id": "op_1"},
                "scopes": ["read"]}}),
            ),
            (
                200,
                json!({"ok": true, "data": {
                "workspace": {"id": "ws_company"},
                "subject": {"anonymous": false, "id": "op_1"},
                "scopes": ["draft"]}}),
            ),
            (
                200,
                json!({"ok": true, "data": {
                "workspace": {"id": "ws_company"},
                "subject": {"anonymous": false, "id": "op_1"},
                "scopes": ["read", "admin"]}}),
            ),
            (
                200,
                json!({"ok": true, "data": {
                "workspace": {"id": "ws_company"},
                "subject": {"anonymous": false, "id": "op_1"},
                "scopes": ["read", "read"]}}),
            ),
        ];
        for answer in bad {
            let fake = Fake::new(vec![answer]);
            assert!(verify_session(&fake, &config(), "agc_testtoken").is_err());
        }
    }

    #[test]
    fn refusals_carry_no_issuer_content() {
        let fake = Fake::new(vec![(200, json!({"surprise": "SECRET-BODY-X1"}))]);
        let err = request_code(&fake, &config()).unwrap_err().to_string();
        assert!(!err.contains("SECRET-BODY-X1"));
    }
}
