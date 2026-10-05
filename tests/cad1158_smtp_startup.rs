//! CAD-1158 implementer-authored startup regression.
//!
//! This file is NOT the independently authored refusal check
//! (`cad1158_lease_off_smtp_refusal.rs`, frozen — never edited here). It
//! covers the new admitted path through the same real guard
//! (`platform::agenticos::attach`): a dedicated versioned image-owned SMTP
//! admission installs the fixed `smtp.internal` relay with the Cadence
//! lifecycle lease off, forged env URLs cannot redirect it, and the
//! installed relay drives enrollment verification and delivery through a
//! synthetic local relay with no direct SMTP socket.
//!
//! Isolation follows the existing convention in
//! `src/platform/deployments/tests.rs`: each parent test self-relaunches
//! this binary per env case under a one-shot marker var, so forged
//! environment never races the parallel suite. Children may additionally
//! scope vars to their own single-test process.

use cadence_agent::daemon::ServeOptions;
use cadence_agent::lease::Hosted;
use cadence_agent::platform::smtp_internal::{SmtpInternal, DEFAULT_BASE};
use cadence_agent::platform::{agenticos, deployments::DeploymentMetadata};

/// Deployed-style file plus the dedicated CAD-1158 SMTP admission.
const SMTP_ADMITTED: &str = r#"{"schema":1,"providers":[
    {"provider":"agenticos","origin":"http://api.internal","manifest_pin":"agenticos-manifest@1/publish_post@2"},
    {"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"},
    {"provider":"smtp","origin":"http://smtp.internal","manifest_pin":"smtp-internal-relay@2","transport":"hosted-smtp-relay@1"}
]}"#;

/// Only the SMTP admission (no media assertion).
const SMTP_ONLY: &str = r#"{"schema":1,"providers":[
    {"provider":"smtp","origin":"http://smtp.internal","manifest_pin":"smtp-internal-relay@2","transport":"hosted-smtp-relay@1"}
]}"#;

/// The real deployed image file shape (media lease, no SMTP admission).
const DEPLOYED_IMAGE: &str = r#"{"schema":1,"providers":[
    {"provider":"agenticos","origin":"http://api.internal","manifest_pin":"agenticos-manifest@1/publish_post@2"},
    {"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"}
]}"#;

const MEDIA_ONLY: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"}]}"#;
const EMPTY: &str = r#"{"schema":1,"providers":[]}"#;
const FORGED_ORIGIN_PIN: &str = r#"{"schema":1,"providers":[{"provider":"agenticos","origin":"http://127.0.0.1:3110","manifest_pin":"agenticos-manifest@1/publish_post@2"}]}"#;

const ISOLATED: &str = "CADENCE_TEST_CAD1158_STARTUP_ISOLATED";

fn relaunch(test: &str, extra_env: &[(&str, &str)]) {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", test, "--test-threads", "1", "--nocapture"])
        .env(ISOLATED, "1")
        .env_remove("CADENCE_AGENTICOS_URL")
        .env_remove("CADENCE_SMTP_INTERNAL_URL")
        .env_remove("CADENCE_HOSTED_EMAIL_FROM")
        .env_remove("CADENCE_HOSTED_EMAIL_FROM_NAME")
        .env_remove("CADENCE_AGENTICOS_EXTERNAL_URL");
    for (name, value) in extra_env {
        cmd.env(name, value);
    }
    let out = cadence_agent::reaper::output(&mut cmd).unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{test}: {text}");
    assert!(text.contains("1 passed"), "{test}: {text}");
}

fn lease_off() -> Hosted {
    Hosted {
        lease: Some("off".into()),
        ..Default::default()
    }
}

fn attach_lease_off(metadata: &str) -> ServeOptions {
    assert!(std::env::var_os(ISOLATED).is_some());
    let hosted = lease_off();
    let mut opts = ServeOptions {
        lease: Some(hosted.clone()),
        provider_deployments: Some(
            DeploymentMetadata::parse(metadata.as_bytes()).expect("fixture must parse"),
        ),
        ..Default::default()
    };
    agenticos::attach(&mut opts, &hosted).expect("attach must not error");
    opts
}

/// Lease off + dedicated SMTP admission installs the fixed relay through
/// the real guard — even with forged caller-controlled URLs in the
/// environment, which must never redirect the credential transport.
#[test]
fn cad1158_lease_off_smtp_admission_installs_fixed_relay() {
    if std::env::var_os(ISOLATED).is_none() {
        for (case, extra) in [
            (
                "forged_both",
                vec![
                    ("CADENCE_AGENTICOS_URL", "http://127.0.0.1:3110"),
                    ("CADENCE_SMTP_INTERNAL_URL", "http://127.0.0.1:3111"),
                ],
            ),
            (
                "forged_smtp_only",
                vec![("CADENCE_SMTP_INTERNAL_URL", "http://127.0.0.1:3111")],
            ),
            ("no_forged_env", vec![]),
        ] {
            let mut env: Vec<(&str, &str)> = vec![("CAD1158_CASE", case)];
            env.extend(extra);
            relaunch(
                "cad1158_lease_off_smtp_admission_installs_fixed_relay",
                &env,
            );
        }
        return;
    }
    for metadata in [SMTP_ADMITTED, SMTP_ONLY] {
        let opts = attach_lease_off(metadata);
        let relay = opts
            .smtp_internal
            .as_ref()
            .expect("dedicated SMTP admission must install the relay with lease off");
        assert_eq!(
            relay.base(),
            DEFAULT_BASE,
            "admitted relay must use the fixed bridge origin, never caller env (case {:?})",
            std::env::var("CAD1158_CASE"),
        );
        assert!(
            opts.hosted_email.is_none(),
            "the SMTP admission must not activate hosted_email"
        );
    }
}

/// A pre-registered (e.g. self-hosted explicit-URL) platform must not
/// suppress an independently admitted relay: SMTP composition runs before
/// the platform-registered early return.
#[test]
fn cad1158_lease_off_smtp_admission_survives_preregistered_platform() {
    if std::env::var_os(ISOLATED).is_none() {
        relaunch(
            "cad1158_lease_off_smtp_admission_survives_preregistered_platform",
            &[],
        );
        return;
    }
    let hosted = lease_off();
    let mut opts = ServeOptions {
        lease: Some(hosted.clone()),
        provider_deployments: Some(DeploymentMetadata::parse(SMTP_ADMITTED.as_bytes()).unwrap()),
        ..Default::default()
    };
    // Generic explicit-URL registration: retained self-hosted behavior,
    // carrying no deployment pin and no relay authority of its own.
    agenticos::register(&mut opts, "http://127.0.0.1:3110").unwrap();
    assert_eq!(
        opts.platforms[agenticos::PLATFORM].reported_manifest_version(),
        None
    );
    agenticos::attach(&mut opts, &hosted).unwrap();
    let relay = opts
        .smtp_internal
        .as_ref()
        .expect("pre-registered platform must not drop the admitted relay");
    assert_eq!(relay.base(), DEFAULT_BASE);
    assert!(opts.hosted_email.is_none());
}

/// Lease off without the dedicated admission installs nothing — forged
/// URLs, media-only, deployed-shape and forged-origin metadata alike.
#[test]
fn cad1158_lease_off_without_smtp_admission_installs_nothing() {
    if std::env::var_os(ISOLATED).is_none() {
        relaunch(
            "cad1158_lease_off_without_smtp_admission_installs_nothing",
            &[
                ("CADENCE_AGENTICOS_URL", "http://127.0.0.1:3110"),
                ("CADENCE_SMTP_INTERNAL_URL", "http://127.0.0.1:3111"),
            ],
        );
        return;
    }
    for metadata in [EMPTY, MEDIA_ONLY, DEPLOYED_IMAGE, FORGED_ORIGIN_PIN] {
        let opts = attach_lease_off(metadata);
        assert!(
            opts.smtp_internal.is_none(),
            "unadmitted composition installed the credential relay"
        );
        assert!(opts.hosted_email.is_none());
    }
}

/// SMTP-only metadata must not register hosted media (the mirrored
/// cross-authorization refusal), while still admitting the relay itself.
#[test]
fn cad1158_smtp_only_admission_grants_no_media() {
    if std::env::var_os(ISOLATED).is_none() {
        relaunch("cad1158_smtp_only_admission_grants_no_media", &[]);
        return;
    }
    let mut opts = ServeOptions {
        lease: Some(Hosted::default()),
        provider_deployments: Some(DeploymentMetadata::parse(SMTP_ONLY.as_bytes()).unwrap()),
        ..Default::default()
    };
    cadence_agent::platform::agenticos_external::attach(&mut opts).unwrap();
    assert!(
        !opts
            .platforms
            .contains_key(cadence_agent::platform::agenticos_external::PLATFORM),
        "the SMTP assertion granted hosted media authority"
    );
    agenticos::attach(&mut opts, &Hosted::default()).unwrap();
    assert!(
        opts.smtp_internal.is_some(),
        "SMTP-only admission must still install the relay"
    );
    assert!(opts.hosted_email.is_none());
}

// ---------- synthetic relay end to end (lease path install) ----------

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Seen {
    path: String,
    body: serde_json::Value,
}

fn reply(stream: &mut TcpStream, body: &serde_json::Value) {
    let text = body.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 200 X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
}

fn serve_stub(mut stream: TcpStream, log: Arc<Mutex<Vec<Seen>>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let length = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0);
    while buf.len() < head_end + length {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body: serde_json::Value =
        serde_json::from_slice(&buf[head_end..head_end + length]).unwrap_or_default();
    let path = head
        .lines()
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .unwrap_or("")
        .to_string();
    // Contract v2: strict server block on the synthetic relay too.
    let server = &body["server"];
    let shape_ok = matches!(
        (
            server["port"].as_u64().unwrap_or(0),
            server["tls_mode"].as_str().unwrap_or("")
        ),
        (465, "implicit") | (587, "starttls")
    ) && server["host"].as_str().is_some_and(|h| !h.is_empty())
        && server["password"].as_str().is_some_and(|p| !p.is_empty());
    let answer = if !shape_ok {
        serde_json::json!({"ok": false, "error": {"code": "invalid", "step": "validate", "message": "bad shape"}})
    } else {
        match path.as_str() {
            "/v1/verify" => serde_json::json!({"ok": true, "data": {"verified": true}}),
            "/v1/send" => {
                let to = body["envelope"]["to"][0]
                    .as_str()
                    .unwrap_or("operator@example.com")
                    .to_string();
                serde_json::json!({"ok": true, "data": {"accepted": [to], "rejected": [], "server_reply": "250 OK"}})
            }
            _ => {
                serde_json::json!({"ok": false, "error": {"code": "invalid", "step": "validate", "message": "unknown path"}})
            }
        }
    };
    log.lock().unwrap().push(Seen { path, body });
    reply(&mut stream, &answer);
}

/// The installed relay (composed by the real `attach`, here via the
/// retained lease path pointed at a synthetic endpoint) drives enrollment
/// verification and delivery through that relay: the stub sees
/// `/v1/verify` then `/v1/send`, the send is Accepted (never
/// Uncertain/refused-as-success), and no direct SMTP socket is dialed —
/// the stub speaks HTTP only and the mail host is synthetic.
#[test]
fn cad1158_installed_relay_verifies_and_sends_through_synthetic_stub() {
    if std::env::var_os(ISOLATED).is_none() {
        relaunch(
            "cad1158_installed_relay_verifies_and_sends_through_synthetic_stub",
            &[],
        );
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stub_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
    std::thread::spawn({
        let seen = Arc::clone(&seen);
        move || {
            for stream in listener.incoming().flatten() {
                let seen = Arc::clone(&seen);
                std::thread::spawn(move || serve_stub(stream, seen));
            }
        }
    });
    // Scoped to this single-test child only: the retained lease path
    // honours the endpoint override, the image-admitted path ignores it.
    std::env::set_var("CADENCE_SMTP_INTERNAL_URL", &stub_url);
    let root = tempfile::tempdir().unwrap();
    let lease_file = root.path().join("lease.json");
    std::fs::write(&lease_file, "{}").unwrap();
    let hosted = Hosted {
        lease: Some(format!("file:{}", lease_file.display())),
        ..Default::default()
    };
    let mut opts = ServeOptions {
        lease: Some(hosted.clone()),
        ..Default::default()
    };
    agenticos::attach(&mut opts, &hosted).expect("lease-path attach installs the relay");
    let relay = opts.smtp_internal.as_ref().expect("lease admits the relay");
    assert_eq!(relay.base(), stub_url.as_str());
    assert!(SmtpInternal::new(&stub_url).unwrap().base() == relay.base());

    let secret = b"synthetic-app-password";
    let server = cadence_agent::platform::smtp_internal::Server {
        host: "mail.example.com",
        port: 465,
        tls_mode: "implicit",
        username: "operator@example.com",
        secret,
    };
    relay
        .verify(&server)
        .expect("enrollment verification must call the relay and succeed");
    let outcome = relay
        .send_outcome(
            &server,
            "sender@example.com",
            "operator@example.com",
            b"From: sender@example.com\r\nTo: operator@example.com\r\nSubject: synthetic\r\n\r\nhello",
        )
        .expect("delivery classifies locally");
    match outcome {
        cadence_agent::platform::smtp::SmtpOutcome::Accepted { code, .. } => {
            assert_eq!(code, 250);
        }
        cadence_agent::platform::smtp::SmtpOutcome::Deferred { code, .. } => {
            panic!("synthetic relay delivery must be Accepted, got Deferred({code})")
        }
        cadence_agent::platform::smtp::SmtpOutcome::Rejected { code, .. } => {
            panic!("synthetic relay delivery must be Accepted, got Rejected({code})")
        }
        cadence_agent::platform::smtp::SmtpOutcome::NotSubmitted { .. } => {
            panic!("synthetic relay delivery must be Accepted, got NotSubmitted")
        }
        cadence_agent::platform::smtp::SmtpOutcome::Uncertain { .. } => {
            panic!("synthetic relay delivery must be Accepted, got Uncertain")
        }
        cadence_agent::platform::smtp::SmtpOutcome::PendingApproval { .. } => {
            panic!("synthetic relay delivery must be Accepted, got PendingApproval")
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if seen.lock().unwrap().len() >= 2 || Instant::now() > deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let seen = seen.lock().unwrap();
    let paths: Vec<&str> = seen.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(
        paths,
        vec!["/v1/verify", "/v1/send"],
        "both calls hit the relay"
    );
    assert_eq!(
        seen[0].body["server"]["password"].as_str(),
        Some("synthetic-app-password"),
        "the custodied credential travels only inside the relay request body"
    );
    assert_eq!(
        seen[1].body["envelope"]["to"][0].as_str(),
        Some("operator@example.com")
    );
    assert!(
        seen[1].body["message_b64"]
            .as_str()
            .is_some_and(|b| !b.is_empty()),
        "delivery carries the prepared message bytes"
    );
}
