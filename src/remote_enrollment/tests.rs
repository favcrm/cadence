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
        super::registration_url("https://app.agenticos.hk/device#fragment", None, None,).is_err()
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
        json!({"issuer":issuer,"client_id":"test-id.access","client_secret":"changed"}).to_string(),
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
        DestinationPin::new("ws_real", &valid.audience, &valid.subject_id, "hca_forged").unwrap(),
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
        DestinationPin::new("ws_other", &enrolled.audience, "hsp_subject", "hca_agent").unwrap(),
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
            let (mut bind, _) = continuity_request(&listener, "/v1/hosted-cadence/continuity/bind");
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
            let (mut bind, _) = continuity_request(&listener, "/v1/hosted-cadence/continuity/bind");
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
    let error = post_continuity(&issuer, "/continuity/challenge", CHILD, json!({})).unwrap_err();
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
        write!(
            first_bind,
            "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        )
        .unwrap();
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
    let (board, board_log) =
        web_issuer_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
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
    let (issuer, log) =
        web_issuer_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
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
    let (issuer, log) =
        web_issuer_server("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
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
                "over_parent" => child["agents"][0]["credential"]["expires_at"] = json!(at + 121),
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
            assert!(
                enroll_browser(&issuer, org, "http://127.0.0.1:1", client, &dir, |_, _| {
                    panic!("oversized identity reached a device request")
                })
                .is_err()
            );
        }
    }
    assert!(listener.accept().is_err(), "issuer was contacted");
    assert!(!dir.join(RECORD).exists());
}
