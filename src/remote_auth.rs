//! AgenticOS device credentials. This is issuer sign-in, not hosted-board RPC.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};

const MAX_BODY: u64 = 64 * 1024;
const MAX_TOKEN: usize = 4096;
const CONTRACT_NOTE: &str = "Issuer authentication only. Hosted Cadence transport, board audience binding, refresh and per-agent enrollment are not configured by this increment.";

#[derive(Serialize, Deserialize)]
struct Credential {
    issuer: String,
    org: String,
    token: String,
    expires_at: Option<u64>,
}

#[derive(Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct DeviceToken {
    access_token: String,
    token_type: String,
    expires_in: u64,
    scope: String,
    workspace_id: String,
}

fn rejected(message: &str) -> Error {
    Error::rejected(message)
}

/// Origins only: secrets are never sent to an arbitrary path or URL userinfo.
fn issuer_origin(input: &str) -> Result<String> {
    let uri: ureq::http::Uri = input
        .parse()
        .map_err(|_| rejected("Invalid issuer origin"))?;
    let authority = uri
        .authority()
        .ok_or_else(|| rejected("Issuer must be an HTTPS origin"))?;
    let secure = uri.scheme_str() == Some("https");
    // Available only inside library tests, never a CLI plaintext escape hatch.
    let test_loopback = cfg!(test)
        && uri.scheme_str() == Some("http")
        && matches!(uri.host(), Some("127.0.0.1") | Some("localhost"));
    if (!secure && !test_loopback)
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

fn validate_token(token: &str) -> Result<()> {
    if token.is_empty()
        || token.len() > MAX_TOKEN
        || !token.bytes().all(|b| (33..=126).contains(&b))
    {
        return Err(rejected(
            "Credential must be one bounded ASCII token without whitespace",
        ));
    }
    Ok(())
}

fn validate_org(org: &str) -> Result<()> {
    if org.is_empty()
        || org.len() > 200
        || !org
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err(rejected("Organization must be a workspace ID"));
    }
    Ok(())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .into()
}

fn read_response(mut response: ureq::http::Response<ureq::Body>) -> Result<(u16, Value)> {
    let status = response.status().as_u16();
    // Do not echo transport errors, issuer bodies or URLs: any can contain secrets.
    if (300..400).contains(&status) {
        return Err(rejected(
            "Issuer redirect refused; configure the exact issuer origin",
        ));
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| rejected("Unable to read issuer response"))?;
    if bytes.len() as u64 > MAX_BODY {
        return Err(rejected("Issuer response exceeds size limit"));
    }
    let value = serde_json::from_slice(&bytes)
        .map_err(|_| rejected("Issuer returned an invalid response"))?;
    Ok((status, value))
}

fn post(issuer: &str, path: &str, body: Value) -> Result<(u16, Value)> {
    let response = agent()
        .post(format!("{issuer}{path}"))
        .send_json(body)
        .map_err(|_| rejected("Issuer request failed; no credential was saved"))?;
    read_response(response)
}

fn parse_session(value: Value, org: &str) -> Result<Value> {
    let data = &value["data"];
    if value["ok"] != true
        || data["workspace"]["id"].as_str() != Some(org)
        || data["subject"]["anonymous"] != false
        || data["subject"]["id"]
            .as_str()
            .is_none_or(|id| validate_org(id).is_err())
        || data["scopes"]
            .as_array()
            .is_none_or(|scopes| !scopes.iter().any(|s| s == "read"))
    {
        return Err(rejected(
            "Issuer did not verify a named read principal for the requested organization",
        ));
    }
    // Output only stable IDs/scopes. Never echo arbitrary issuer labels or names.
    let scopes: Vec<&str> = data["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .filter(|scope| matches!(*scope, "read" | "draft" | "send"))
        .collect();
    Ok(json!({"org":org,"subject_id":data["subject"]["id"],"scopes":scopes}))
}

fn verify(issuer: &str, org: &str, token: &str) -> Result<Value> {
    validate_token(token)?;
    let response = agent()
        .get(format!("{issuer}/v1/runtime/session"))
        .header("Authorization", format!("Bearer {token}"))
        .call()
        .map_err(|_| rejected("Issuer verification failed"))?;
    let (status, value) = read_response(response)?;
    if status != 200 {
        return Err(rejected(
            "Issuer rejected the credential; check expiry, revocation and read scope",
        ));
    }
    parse_session(value, org)
}

fn unix_now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| rejected("System clock is invalid"))
}

pub fn auth_dir(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = explicit {
        if !dir.is_absolute() {
            return Err(rejected("Credential directory must be absolute"));
        }
        return Ok(dir.to_owned());
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| rejected("Set HOME, XDG_CONFIG_HOME or --auth-dir"))?;
    if !base.is_absolute() {
        return Err(rejected("Credential directory must be absolute"));
    }
    Ok(base.join("cadence/remote-auth"))
}

fn private_dir(dir: &Path, create: bool) -> Result<()> {
    if !dir.exists() && create {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(rejected(
            "Credential directory must be owned by you, mode 0700, and not a symlink",
        ));
    }
    Ok(())
}

fn load(dir: &Path) -> Result<Credential> {
    private_dir(dir, false)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("credential.json"))?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(rejected(
            "Credential file must be owned by you and mode 0600",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_BODY + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BODY {
        return Err(rejected("Credential file exceeds size limit"));
    }
    serde_json::from_slice(&bytes).map_err(|_| rejected("Credential file is invalid"))
}

fn save(dir: &Path, credential: &Credential) -> Result<()> {
    private_dir(dir, true)?;
    let _lock = credential_lock(dir)?;
    let path = dir.join("credential.json");
    if fs::symlink_metadata(&path).is_ok() {
        let previous = load(dir)?;
        if previous.issuer != credential.issuer || previous.org != credential.org {
            return Err(rejected("A different organization or issuer is stored; use another --auth-dir or logout first"));
        }
    }
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temp.write_all(
        &serde_json::to_vec(credential).map_err(|_| rejected("Cannot encode credential"))?,
    )?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|_| rejected("Unable to save credential"))?;
    Ok(())
}

// Lock validation and replacement together so parallel logins cannot overwrite
// another organization's credential. The lock file contains no secret.
fn credential_lock(dir: &Path) -> Result<fs::File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("credential.lock"))?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(rejected("Credential lock must be a private owned file"));
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(rejected("Unable to lock local credential"));
    }
    Ok(file)
}

fn read_stdin_token() -> Result<String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_TOKEN as u64 + 3)
        .read_to_end(&mut bytes)?;
    let token = String::from_utf8(bytes).map_err(|_| rejected("Credential must be ASCII"))?;
    let token = token
        .strip_suffix("\r\n")
        .or_else(|| token.strip_suffix('\n'))
        .unwrap_or(&token)
        .to_owned();
    validate_token(&token)?;
    Ok(token)
}

fn next_poll_interval(status: u16, value: &Value, interval: u64) -> Result<u64> {
    match value["error"].as_str() {
        Some("authorization_pending") if status == 400 => Ok(interval),
        Some("slow_down") if status == 400 => Ok(interval.saturating_add(5)),
        Some("access_denied") => Err(rejected("Device authorization denied")),
        Some("expired_token") => Err(rejected("Device authorization expired")),
        _ => Err(rejected("Device exchange failed; start login again")),
    }
}

fn environment_credential(
    token: String,
    issuer: Option<String>,
    org: Option<String>,
) -> Result<Credential> {
    validate_token(&token)?;
    let issuer = issuer
        .ok_or_else(|| rejected("CADENCE_TOKEN requires explicit --issuer or CADENCE_ISSUER"))?;
    let org =
        org.ok_or_else(|| rejected("CADENCE_TOKEN requires explicit --org or CADENCE_ORG"))?;
    validate_org(&org)?;
    Ok(Credential {
        issuer: issuer_origin(&issuer)?,
        org,
        token,
        expires_at: None,
    })
}

pub fn login(issuer: &str, org: &str, dir: &Path, token_stdin: bool, no_open: bool) -> Result<i32> {
    let issuer = issuer_origin(issuer)?;
    validate_org(org)?;
    let credential = if token_stdin {
        Credential {
            issuer: issuer.clone(),
            org: org.to_owned(),
            token: read_stdin_token()?,
            expires_at: None,
        }
    } else {
        let (status, value) = post(
            &issuer,
            "/v1/device/code",
            json!({"client_label":"cadence-cli","scope":"read draft"}),
        )?;
        if status != 200 {
            return Err(rejected("Device authorization unavailable at this issuer"));
        }
        let code: DeviceCode = serde_json::from_value(value)
            .map_err(|_| rejected("Invalid device authorization response"))?;
        validate_token(&code.device_code)?;
        if code.expires_in == 0
            || code.expires_in > 3600
            || code.interval == 0
            || code.interval > 60
            || code.user_code.len() != 9
            || !code
                .user_code
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(rejected("Invalid device authorization timing or code"));
        }
        // Browser URLs are not fetched with credentials, but disallow unsafe schemes/userinfo.
        let browser: ureq::http::Uri = code
            .verification_uri_complete
            .parse()
            .map_err(|_| rejected("Invalid verification URL"))?;
        if browser.scheme_str() != Some("https")
            || browser.authority().is_none_or(|a| a.as_str().contains('@'))
            || code.verification_uri.contains(['\n', '\r'])
        {
            return Err(rejected("Verification URL must use HTTPS"));
        }
        eprintln!(
            "Authorize organization {org}. Open {} and enter {}",
            code.verification_uri_complete, code.user_code
        );
        if !no_open {
            let program = if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            };
            let mut command = std::process::Command::new(program);
            command
                .arg(&code.verification_uri_complete)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            if !crate::reaper::status(&mut command).is_ok_and(|status| status.success()) {
                eprintln!("Browser unavailable; use the URL above on another device.");
            }
        }
        let deadline = Instant::now() + Duration::from_secs(code.expires_in);
        let mut interval = code.interval;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining <= Duration::from_secs(interval) {
                return Err(rejected("Device authorization expired; start login again"));
            }
            std::thread::sleep(Duration::from_secs(interval));
            let (status, value) = post(
                &issuer,
                "/v1/device/token",
                json!({"grant_type":"urn:ietf:params:oauth:grant-type:device_code","device_code":code.device_code}),
            )?;
            if status == 200 {
                let token: DeviceToken = serde_json::from_value(value)
                    .map_err(|_| rejected("Invalid credential exchange response"))?;
                validate_token(&token.access_token)?;
                if token.token_type != "Bearer"
                    || token.workspace_id != org
                    || token.expires_in == 0
                    || !token
                        .scope
                        .split_whitespace()
                        .all(|s| matches!(s, "read" | "draft"))
                {
                    return Err(rejected(
                        "Credential exchange does not match requested organization or scopes",
                    ));
                }
                break Credential {
                    issuer: issuer.clone(),
                    org: org.to_owned(),
                    token: token.access_token,
                    expires_at: Some(
                        unix_now()?
                            .checked_add(token.expires_in)
                            .ok_or_else(|| rejected("Invalid credential expiry"))?,
                    ),
                };
            }
            interval = next_poll_interval(status, &value, interval)?;
        }
    };
    let principal = verify(&issuer, org, &credential.token)?;
    save(dir, &credential)?;
    println!(
        "{}",
        json!({"authenticated":true,"issuer":issuer,"principal":principal,"storage":"permission-restricted file (0600)","expires_at":credential.expires_at,"hosted_cadence":"not_configured","note":CONTRACT_NOTE})
    );
    Ok(0)
}

pub fn status(dir: &Path, issuer: Option<&str>, org: Option<&str>) -> Result<i32> {
    let (credential, source) = if let Some(token) = std::env::var_os("CADENCE_TOKEN") {
        let token = token
            .into_string()
            .map_err(|_| rejected("CADENCE_TOKEN must be ASCII"))?;
        let issuer = issuer
            .map(str::to_owned)
            .or_else(|| std::env::var("CADENCE_ISSUER").ok());
        let org = org
            .map(str::to_owned)
            .or_else(|| std::env::var("CADENCE_ORG").ok());
        (environment_credential(token, issuer, org)?, "environment")
    } else {
        let credential = load(dir)?;
        if issuer.is_some_and(|i| issuer_origin(i).ok().as_deref() != Some(&credential.issuer))
            || org.is_some_and(|o| o != credential.org)
        {
            return Err(rejected(
                "Stored credential differs from the requested issuer or organization",
            ));
        }
        (credential, "stored")
    };
    let issuer = issuer_origin(&credential.issuer)?;
    validate_org(&credential.org)?;
    if credential
        .expires_at
        .is_some_and(|expiry| unix_now().map_or(true, |now| now >= expiry))
    {
        return Err(rejected("Stored credential expired; sign in again (refresh is not supported by this issuer contract)"));
    }
    let principal = verify(&issuer, &credential.org, &credential.token)?;
    println!(
        "{}",
        json!({"authenticated":true,"source":source,"issuer":issuer,"principal":principal,"expires_at":credential.expires_at,"hosted_cadence":"not_configured","note":CONTRACT_NOTE})
    );
    Ok(0)
}

pub fn logout(dir: &Path) -> Result<i32> {
    if dir.exists() {
        private_dir(dir, false)?;
        let _lock = credential_lock(dir)?;
        let path = dir.join("credential.json");
        if fs::symlink_metadata(&path).is_ok() {
            load(dir)?;
            fs::remove_file(path)?;
        }
    }
    println!("Local credential removed. Server credential remains active; revoke it in AgenticOS connected devices. CADENCE_TOKEN is unaffected.");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn credential(org: &str) -> Credential {
        Credential {
            issuer: "https://api.example.com".into(),
            org: org.into(),
            token: "agc_synthetic".into(),
            expires_at: None,
        }
    }

    fn server_once(
        status: u16,
        body: Value,
        redirect: Option<String>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", server.server_addr());
        let thread = std::thread::spawn(move || {
            let request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .expect("expected issuer request");
            assert_eq!(request.url(), "/v1/runtime/session");
            assert_eq!(
                request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Authorization"))
                    .unwrap()
                    .value
                    .as_str(),
                "Bearer agc_synthetic"
            );
            let mut response =
                tiny_http::Response::from_string(body.to_string()).with_status_code(status);
            if let Some(location) = redirect {
                response.add_header(tiny_http::Header::from_bytes("Location", location).unwrap());
            }
            request.respond(response).unwrap();
        });
        (origin, thread)
    }

    #[test]
    fn issuer_rejects_plaintext_credentials_and_non_origin_urls() {
        for issuer in [
            "http://example.com",
            "https://user:pass@example.com",
            "https://example.com/path",
            "https://example.com?token=x",
            "https://example.com#fragment",
        ] {
            assert!(issuer_origin(issuer).is_err(), "accepted {issuer}");
        }
        assert_eq!(
            issuer_origin("https://example.com/").unwrap(),
            "https://example.com"
        );
    }

    #[test]
    fn forged_org_and_anonymous_session_fail_closed() {
        let session = serde_json::json!({"ok":true,"data":{"workspace":{"id":"other"},"subject":{"id":"user","anonymous":false},"scopes":["read"]}});
        assert!(parse_session(session.clone(), "wanted").is_err());
        assert!(parse_session(session, "other").is_ok());
        let anonymous = serde_json::json!({"ok":true,"data":{"workspace":{"id":"wanted"},"subject":{"id":"user","anonymous":true},"scopes":["read"]}});
        assert!(parse_session(anonymous, "wanted").is_err());
    }

    #[test]
    fn secrets_cannot_inject_headers_or_expand_input() {
        for token in [
            "",
            "secret\nInjected: true",
            "token with spaces",
            "token\r",
            "é",
        ] {
            assert!(validate_token(token).is_err());
        }
        assert!(validate_token(&"a".repeat(4097)).is_err());
        assert!(validate_token("agc_synthetic-token").is_ok());
    }

    #[test]
    fn introspection_verifies_org_and_denies_revoked_tokens_without_echoing_secrets() {
        let session = json!({"ok":true,"data":{"workspace":{"id":"ws_fixture"},"subject":{"id":"user_fixture","anonymous":false},"scopes":["read","draft"]}});
        let (issuer, thread) = server_once(200, session.clone(), None);
        assert_eq!(
            verify(
                &issuer_origin(&issuer).unwrap(),
                "ws_fixture",
                "agc_synthetic"
            )
            .unwrap()["subject_id"],
            "user_fixture"
        );
        thread.join().unwrap();
        let (issuer, thread) = server_once(200, session, None);
        assert!(verify(&issuer, "ws_wrong", "agc_synthetic").is_err());
        thread.join().unwrap();
        let (issuer, thread) = server_once(401, json!({"error":"agc_synthetic"}), None);
        let error = verify(&issuer, "ws_fixture", "agc_synthetic")
            .unwrap_err()
            .to_string();
        assert!(!error.contains("agc_synthetic"));
        thread.join().unwrap();
    }

    #[test]
    fn bearer_is_never_forwarded_to_redirect_destination() {
        let destination = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let (issuer, thread) = server_once(
            302,
            json!({}),
            Some(format!("http://{}/stolen", destination.server_addr())),
        );
        assert!(verify(&issuer, "ws_fixture", "agc_synthetic").is_err());
        thread.join().unwrap();
        assert!(destination
            .recv_timeout(Duration::from_millis(50))
            .unwrap()
            .is_none());
    }

    #[test]
    fn credential_storage_is_private_and_rejects_symlinks_and_org_replacement() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("auth");
        save(&dir, &credential("ws_one")).unwrap();
        assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(dir.join("credential.json")).unwrap().mode() & 0o777,
            0o600
        );
        assert!(save(&dir, &credential("ws_two")).is_err());
        assert_eq!(load(&dir).unwrap().org, "ws_one");
        let linked = root.path().join("linked");
        std::os::unix::fs::symlink(dir.join("credential.json"), &linked).unwrap();
        fs::rename(&linked, dir.join("credential.json")).unwrap();
        assert!(load(&dir).is_err());
        assert!(auth_dir(Some(Path::new("relative"))).is_err());
    }

    #[test]
    fn concurrent_different_org_logins_cannot_replace_each_other() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("auth");
        private_dir(&dir, true).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let threads: Vec<_> = ["ws_one", "ws_two"]
            .into_iter()
            .map(|org| {
                let dir = dir.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    save(&dir, &credential(org)).is_ok()
                })
            })
            .collect();
        let successes = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|success| *success)
            .count();
        assert_eq!(successes, 1);
        assert!(["ws_one", "ws_two"].contains(&load(&dir).unwrap().org.as_str()));
    }

    #[test]
    fn exact_aos49_device_fixture_parses_without_becoming_a_board_credential() {
        let code: DeviceCode = serde_json::from_value(json!({"device_code":"agd_synthetic","user_code":"K7PM-2QNF","verification_uri":"https://app.agenticos.hk/device","verification_uri_complete":"https://app.agenticos.hk/device?code=K7PM-2QNF","expires_in":600,"interval":5})).unwrap();
        assert_eq!(code.interval, 5);
        let token: DeviceToken = serde_json::from_value(json!({"access_token":"agc_synthetic","token_type":"Bearer","expires_in":2592000,"scope":"read draft","workspace_id":"ws_fixture"})).unwrap();
        assert_eq!(token.workspace_id, "ws_fixture");
        assert!(CONTRACT_NOTE.contains("not configured"));
    }

    #[test]
    fn polling_obeys_pending_slowdown_and_terminal_errors() {
        assert_eq!(
            next_poll_interval(400, &json!({"error":"authorization_pending"}), 5).unwrap(),
            5
        );
        assert_eq!(
            next_poll_interval(400, &json!({"error":"slow_down"}), 5).unwrap(),
            10
        );
        for error in [
            "access_denied",
            "expired_token",
            "invalid_grant",
            "invalid_request",
            "unknown",
        ] {
            assert!(next_poll_interval(400, &json!({"error":error}), 5).is_err());
        }
        assert!(next_poll_interval(500, &json!({"error":"authorization_pending"}), 5).is_err());
    }

    #[test]
    fn environment_token_requires_its_own_explicit_binding_and_stays_ephemeral() {
        assert!(
            environment_credential("agc_synthetic".into(), None, Some("ws_one".into())).is_err()
        );
        assert!(environment_credential(
            "agc_synthetic".into(),
            Some("https://api.example.com".into()),
            None
        )
        .is_err());
        assert!(environment_credential(
            "".into(),
            Some("https://api.example.com".into()),
            Some("ws_one".into())
        )
        .is_err());
        let credential = environment_credential(
            "agc_synthetic".into(),
            Some("https://api.example.com".into()),
            Some("ws_one".into()),
        )
        .unwrap();
        assert_eq!(credential.org, "ws_one");
        assert!(credential.expires_at.is_none());
    }

    fn device_server(outcome: &str) -> (String, std::thread::JoinHandle<()>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", server.server_addr());
        let outcome = outcome.to_owned();
        let thread = std::thread::spawn(move || {
            let mut request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            assert_eq!(request.url(), "/v1/device/code");
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&body).unwrap(),
                json!({"client_label":"cadence-cli","scope":"read draft"})
            );
            request.respond(tiny_http::Response::from_string(json!({"device_code":"agd_fixture","user_code":"K7PM-2QNF","verification_uri":"https://app.agenticos.hk/device","verification_uri_complete":"https://app.agenticos.hk/device?code=K7PM-2QNF","expires_in":600,"interval":1}).to_string())).unwrap();
            let request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            assert_eq!(request.url(), "/v1/device/token");
            request
                .respond(
                    tiny_http::Response::from_string(
                        json!({"error":"authorization_pending"}).to_string(),
                    )
                    .with_status_code(400),
                )
                .unwrap();
            let mut request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            assert_eq!(request.url(), "/v1/device/token");
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&body).unwrap(),
                json!({"grant_type":"urn:ietf:params:oauth:grant-type:device_code","device_code":"agd_fixture"})
            );
            if outcome == "denied" {
                request
                    .respond(
                        tiny_http::Response::from_string(
                            json!({"error":"access_denied"}).to_string(),
                        )
                        .with_status_code(400),
                    )
                    .unwrap();
                return;
            }
            let org = if outcome == "wrong_org" {
                "ws_other"
            } else {
                "ws_fixture"
            };
            request.respond(tiny_http::Response::from_string(json!({"access_token":"agc_synthetic","token_type":"Bearer","expires_in":2592000,"scope":"read draft","workspace_id":org}).to_string())).unwrap();
            if outcome == "wrong_org" {
                return;
            }
            let request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            assert_eq!(request.url(), "/v1/runtime/session");
            assert_eq!(
                request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Authorization"))
                    .unwrap()
                    .value
                    .as_str(),
                "Bearer agc_synthetic"
            );
            request.respond(tiny_http::Response::from_string(json!({"ok":true,"data":{"workspace":{"id":"ws_fixture"},"subject":{"id":"user_fixture","anonymous":false},"scopes":["read","draft"]}}).to_string())).unwrap();
        });
        (origin, thread)
    }

    #[test]
    fn browser_device_flow_verifies_and_saves_only_the_approved_org() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("auth");
        let (issuer, thread) = device_server("approved");
        assert_eq!(login(&issuer, "ws_fixture", &dir, false, true).unwrap(), 0);
        thread.join().unwrap();
        let saved = load(&dir).unwrap();
        assert_eq!(saved.org, "ws_fixture");
        assert_eq!(saved.issuer, issuer);
        assert!(saved.expires_at.unwrap() > unix_now().unwrap());
    }

    #[test]
    fn denied_or_wrong_org_device_flow_never_persists_credentials() {
        for outcome in ["denied", "wrong_org"] {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("auth");
            let (issuer, thread) = device_server(outcome);
            assert!(login(&issuer, "ws_fixture", &dir, false, true).is_err());
            thread.join().unwrap();
            assert!(!dir.join("credential.json").exists());
        }
    }
}
