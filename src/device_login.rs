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

use serde::{Deserialize, Serialize};
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

/// The daemon-side trust pin for device sign-in (CAD-777, fix of the
/// reviewer finding on #541): the operator-configured issuer + exact
/// workspace + the subject allowlist, written by `ui run`/`ui start`
/// resolve and read by the daemon at mint time. The daemon verifies
/// the presented `agc_` against THIS pin — never against
/// caller-supplied issuer/org — and mints only when the verified
/// subject is on `subjects`, so a socket caller can neither choose
/// the trust root nor mint for a principal the operator did not name.
/// Lives at `<state>/operator/device-login.json`, `0600` in the
/// `0700` operator directory, under the same hygiene as the secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevicePin {
    pub issuer: String,
    pub org: String,
    /// The verified issuer subjects allowed a board session — one or
    /// a few named operators. No wildcard: an empty list fails the
    /// pin's validation, i.e. the flow stays off.
    pub subjects: Vec<String>,
    /// The board process that wrote this pin while holding the pin
    /// lock. The daemon accepts the pin only while that pid is alive
    /// AND the lock is still held (review r9) — a stale file behind a
    /// dead or replaced board mints nothing. A pin written without
    /// this field fails closed on read.
    pub board_pid: u32,
}

const PIN_FILE: &str = "device-login.json";

/// Advisory lock the serving board holds for its lifetime, taken
/// before the pin is written or cleared — a second `ui run` on ANY
/// port cannot rewrite or clear the pin under a live board. Lives
/// in the `0700` operator dir next to the pin; the daemon also reads
/// its held-ness as part of pin liveness ([`pin_is_live`]).
pub const DEVICE_PIN_LOCK: &str = "device-login.lock";

/// The pin is mint authority only while the board that wrote it is
/// alive and still holds [`DEVICE_PIN_LOCK`]: (a) `kill(board_pid, 0)`
/// must succeed, and (b) a shared non-blocking flock on the lock file
/// must fail with EWOULDBLOCK — someone holds the exclusive lock. A
/// missing file, a free lock, or a dead pid all fail closed with
/// `capability_unavailable`; the daemon calls this before any issuer
/// contact (review r9).
pub fn pin_is_live(state_dir: &std::path::Path, pin: &DevicePin) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    let gone = || {
        Error::invalid(
            "capability_unavailable",
            "device login is not live: the board that pinned it is gone",
        )
    };
    let alive = unsafe { libc::kill(pin.board_pid as i32, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    if !alive {
        return Err(gone());
    }
    let lock_path = crate::operator_auth::dir(state_dir).join(DEVICE_PIN_LOCK);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&lock_path)
        .map_err(|_| gone())?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
        // Nobody holds it — release ours and fail closed.
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
        return Err(gone());
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return Ok(());
    }
    Err(Error::internal(format!("device pin liveness: {error}")))
}

/// Record the pin. Overwrites atomically (tmp + rename); a partial
/// write never replaces a good one.
pub fn write_pin(state_dir: &std::path::Path, pin: &DevicePin) -> Result<()> {
    // Validate before persisting: a bad triple fails the board at
    // boot, never at first sign-in.
    pin.check()?;
    crate::operator_auth::write_private(
        state_dir,
        PIN_FILE,
        &serde_json::to_vec_pretty(pin).map_err(|e| Error::internal(e.to_string()))?,
    )
}

/// Remove the pin (operator disabled device login): mint fails closed
/// afterwards. Missing file is fine.
pub fn clear_pin(state_dir: &std::path::Path) -> Result<()> {
    let path = crate::operator_auth::dir(state_dir).join(PIN_FILE);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::internal(format!("{}: {e}", path.display()))),
    }
}

/// The configured pin — `Err` (`capability_unavailable`) when device
/// login is not provisioned or the file fails strict modes.
pub fn read_pin(state_dir: &std::path::Path) -> Result<DevicePin> {
    let bytes = crate::operator_auth::read_private(state_dir, PIN_FILE)?;
    let pin: DevicePin = serde_json::from_slice(&bytes).map_err(|e| {
        Error::invalid(
            "capability_unavailable",
            format!("device login pin is invalid: {e}"),
        )
    })?;
    // Re-validate on read: a hand-edited file cannot widen the grant.
    pin.check()?;
    Ok(pin)
}

impl DevicePin {
    /// The pin's own consistency: a valid issuer + workspace AND a
    /// well-formed non-empty allowlist. Used by both the write and the
    /// read path so a hand-edited file fails closed the same way.
    fn check(&self) -> Result<()> {
        DeviceConfig::new(&self.issuer, &self.org)?;
        validate_subjects(&self.subjects)
    }
}

/// At most this many named operators may sign in remotely.
const MAX_SUBJECTS: usize = 16;

/// The operator-configured subject allowlist: 1–16 ids, each under
/// the same charset rule `validate_org` applies (the verified subject
/// id already passes it in `parse_session`), no duplicates. An empty
/// or malformed list is an operator error — the allowlist is the gate,
/// never a wildcard.
pub fn validate_subjects(subjects: &[String]) -> Result<()> {
    if subjects.is_empty() {
        return Err(rejected(
            "Device login needs at least one allowlisted subject (--device-login-subject)",
        ));
    }
    if subjects.len() > MAX_SUBJECTS {
        return Err(rejected(
            &format!("Device login allows at most {MAX_SUBJECTS} subjects — name the few operators who sign in remotely"),
        ));
    }
    for (i, subject) in subjects.iter().enumerate() {
        validate_org(subject).map_err(|_| {
            rejected(
                "Device login subjects must be workspace-style ids (letters, digits, '_' or '-')",
            )
        })?;
        if subjects[..i].contains(subject) {
            return Err(rejected("Device login subjects must not repeat"));
        }
    }
    Ok(())
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
    // Loopback issuers over plain HTTP exist only for test fixtures:
    // the integration stub serves `127.0.0.1` under the `test-seam`
    // feature the suite runs with, and this module's unit tests
    // under `cfg(test)`. A production build refuses a plaintext
    // issuer, exactly like `remote_auth`'s `cfg!(test)` gate —
    // device credentials must never ride the wire unencrypted.
    let loopback = cfg!(any(test, feature = "test-seam"))
        && uri.scheme_str() == Some("http")
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
    // fixtures only (test / test-seam), never caller input and never a
    // production approval link (review r9).
    let loopback = cfg!(any(test, feature = "test-seam"))
        && uri.scheme_str() == Some("http")
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
        // Same body the CLI exchange posts (remote_auth): the RFC 8628
        // grant type is required — the real issuer answers
        // `unsupported_grant_type` without it.
        json!({
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            "device_code": device_code,
        }),
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
        // The posted body carries the RFC 8628 grant type — the issuer
        // refuses `unsupported_grant_type` without it.
        let calls = pending.called();
        assert_eq!(
            calls[0].1["grant_type"],
            json!("urn:ietf:params:oauth:grant-type:device_code")
        );
        assert_eq!(calls[0].1["device_code"], json!("agd_x"));
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

    fn pin_dir() -> tempfile::TempDir {
        tempfile::TempDir::new().unwrap()
    }

    /// The pin round-trips through the operator directory with secret
    /// hygiene (0700 dir, 0600 file); absent or malformed pins fail
    /// closed, and an invalid pair never persists.
    #[test]
    fn device_pin_round_trips_under_strict_modes() {
        use std::os::unix::fs::MetadataExt;
        let dir = pin_dir();
        assert!(read_pin(dir.path()).is_err());
        let pin = DevicePin {
            issuer: "https://issuer.example".to_string(),
            org: "ws_company".to_string(),
            subjects: vec!["op_1".to_string()],
            board_pid: std::process::id(),
        };
        write_pin(dir.path(), &pin).unwrap();
        assert_eq!(read_pin(dir.path()).unwrap(), pin);
        let md = std::fs::symlink_metadata(dir.path().join("operator")).unwrap();
        assert_eq!(md.mode() & 0o777, 0o700);
        let md = std::fs::symlink_metadata(dir.path().join("operator").join("device-login.json"))
            .unwrap();
        assert!(md.is_file());
        assert_eq!(md.mode() & 0o777, 0o600);
        // Malformed content and invalid pairs fail closed.
        std::fs::write(
            dir.path().join("operator").join("device-login.json"),
            b"{not json",
        )
        .unwrap();
        assert!(read_pin(dir.path()).is_err());
        // A pin written before the allowlist existed fails closed too.
        std::fs::write(
            dir.path().join("operator").join("device-login.json"),
            br#"{"issuer":"https://issuer.example","org":"ws_company"}"#,
        )
        .unwrap();
        assert!(read_pin(dir.path()).is_err());
        assert!(write_pin(
            dir.path(),
            &DevicePin {
                issuer: "http://evil.example".to_string(),
                org: "ws_company".to_string(),
                subjects: vec!["op_1".to_string()],
                board_pid: std::process::id(),
            }
        )
        .is_err());
        // Clearing removes mint authority; clearing twice is fine.
        clear_pin(dir.path()).unwrap();
        clear_pin(dir.path()).unwrap();
        assert!(read_pin(dir.path()).is_err());
    }

    /// The pin is mint authority only while its writer is alive AND
    /// holds the lock: no lock file, a free lock file, or a dead pid
    /// all refuse `capability_unavailable` (review r9).
    #[test]
    fn pin_is_live_requires_a_live_lock_holder() {
        use std::os::unix::fs::DirBuilderExt;
        use std::os::unix::io::AsRawFd;
        let dir = pin_dir();
        let lock_path = dir.path().join("operator").join(DEVICE_PIN_LOCK);
        let pin = DevicePin {
            issuer: "https://issuer.example".to_string(),
            org: "ws_company".to_string(),
            subjects: vec!["op_1".to_string()],
            board_pid: std::process::id(),
        };
        // No operator dir at all → refuse.
        let err = pin_is_live(dir.path(), &pin).unwrap_err();
        assert_eq!(err.code(), Some("capability_unavailable"), "{err}");
        // A lock file nobody holds → refuse.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(dir.path().join("operator"))
            .unwrap();
        std::fs::write(&lock_path, b"").unwrap();
        assert!(pin_is_live(dir.path(), &pin).is_err());
        // Take the exclusive lock ourselves: a live holder.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert!(pin_is_live(dir.path(), &pin).is_ok());
        // The lock is held but the writing pid is gone → refuse.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        child.wait().unwrap();
        let dead = DevicePin {
            board_pid: child.id(),
            ..pin
        };
        assert!(pin_is_live(dir.path(), &dead).is_err());
    }

    /// The subject allowlist is a list of 1–16 workspace-style ids,
    /// never empty, never duplicated — the gate, not a wildcard.
    #[test]
    fn device_subjects_validate_all_or_nothing() {
        assert!(validate_subjects(&["op_1".to_string()]).is_ok());
        assert!(validate_subjects(&[]).is_err());
        assert!(validate_subjects(&vec!["op_x".to_string(); 17]).is_err());
        for bad in ["", "op 1", "op@1", "op.1"]
            .into_iter()
            .map(str::to_string)
            .chain(std::iter::once("x".repeat(201)))
        {
            assert!(
                validate_subjects(std::slice::from_ref(&bad)).is_err(),
                "{bad}"
            );
        }
        assert!(validate_subjects(&["op_1".to_string(), "op_1".to_string()]).is_err());
        // A hand-edited pin with a bad allowlist fails closed on read.
        let dir = pin_dir();
        let pin = DevicePin {
            issuer: "https://issuer.example".to_string(),
            org: "ws_company".to_string(),
            subjects: vec!["op_1".to_string()],
            board_pid: std::process::id(),
        };
        write_pin(dir.path(), &pin).unwrap();
        let file = dir.path().join("operator").join("device-login.json");
        std::fs::write(
            &file,
            br#"{"issuer":"https://issuer.example","org":"ws_company","subjects":[]}"#,
        )
        .unwrap();
        assert!(read_pin(dir.path()).is_err());
    }
}
