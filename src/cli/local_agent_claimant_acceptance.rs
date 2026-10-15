//! Claimant pin at the saved-login boundary.
//!
//! A private credential file is swapped from member A to member B after the
//! kickoff already names A. Context reads and a cached handoff both go through
//! the real credential loader. The board is a loopback listener; a refusal
//! must accept nothing.

use super::*;
use cadence_agent::local_assignment::reporting::store_kickoff_binding;
use std::io::{BufRead, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::time::Duration;

const SUBJECT_A: &str = "user_member_a";
const SUBJECT_B: &str = "user_member_b";
const ISSUER: &str = "https://issuer.example.test";
const ORG: &str = "acme";
const ORG_ID: &str = "ws_cad1460claim";
const TOKEN_A: &str = "hct_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const TOKEN_A_RENEWED: &str = "hct_RRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRRR";
const TOKEN_B: &str = "hct_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

fn isolated() -> bool {
    if std::env::var_os("CADENCE_CLAIMANT_TEST_CHILD").is_some() {
        return false;
    }
    let root = tempfile::tempdir_in("/tmp").expect("isolated root");
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).expect("root mode");
    let current = std::thread::current();
    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            current.name().expect("test name"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("CADENCE_CLAIMANT_TEST_CHILD", "1")
        .env("HOME", root.path())
        .env("TMPDIR", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_STATE_HOME", root.path().join("state"))
        .env("CADENCE_STATE_DIR", root.path().join("daemon"))
        .env("CADENCE_PM_DIR", root.path().join("tracker"))
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_ORG")
        .output()
        .expect("isolated claimant test");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

fn with_auth<T>(dir: &Path, body: impl FnOnce() -> T) -> T {
    let prior = std::env::var_os("XDG_CONFIG_HOME");
    std::env::set_var("XDG_CONFIG_HOME", dir);
    let value = body();
    match prior {
        Some(prior) => std::env::set_var("XDG_CONFIG_HOME", prior),
        None => std::env::remove_var("XDG_CONFIG_HOME"),
    }
    value
}

fn write_private(path: &Path, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .expect("credential");
    file.write_all(bytes).expect("credential bytes");
}

fn credential(endpoint: &str, subject: &str, token: &str, generation: u64) -> String {
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    json!({
        "version": "hosted-cadence-auth.v1",
        "organization_id": ORG_ID,
        "organization_slug": ORG,
        "audience": endpoint,
        "issuer": ISSUER,
        "subject_id": subject,
        "capabilities": ["cli.read", "cli.write", "wiki.read"],
        "access_token": token,
        "expires_at": at + 3600,
        "cli_session": {
            "session_id": "hcs_12345678-1234-4234-8234-123456789abc",
            "refresh_token": "hcr_CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC",
            "generation": generation,
            "device_label": "cadence CLI",
            "idle_expires_at": at + 10_000,
            "absolute_expires_at": at + 20_000
        }
    })
    .to_string()
}

fn install_login(config: &Path, endpoint: &str, subject: &str, token: &str, generation: u64) {
    let auth = config.join("cadence/remote-auth");
    std::fs::create_dir_all(&auth).expect("auth dir");
    std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o700)).expect("auth mode");
    write_private(
        &auth.join(format!("cli-{ORG}.json")),
        credential(endpoint, subject, token, generation).as_bytes(),
    );
}

fn target(endpoint: &str) -> RemoteTarget {
    RemoteTarget {
        org: ORG.into(),
        endpoint: endpoint.into(),
        org_id: ORG_ID.into(),
        identity: None,
    }
}

fn kickoff_fixture<'a>(subject: Option<&'a str>, issuer: Option<&'a str>) -> Kickoff<'a> {
    Kickoff {
        alias: "worker",
        issue_id: "CAD1460",
        assignment_id: "hla_00000000000000000000000000000001",
        worktree: "/tmp/cad1460-work",
        message_id: "0123456789abcdef0123456789abcdef",
        claim_id: "hlc_00000000000000000000000000000001",
        generation: 1,
        organization_id: ORG_ID,
        audience: "https://board.test",
        subject_id: subject,
        issuer,
    }
}

fn quiet_listener() -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    listener.set_nonblocking(true).expect("nonblocking");
    listener
}

fn http_response(socket: &mut std::net::TcpStream, body: &Value) {
    let bytes = serde_json::to_vec(body).expect("body");
    write!(
        socket,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        bytes.len()
    )
    .expect("headers");
    socket.write_all(&bytes).expect("payload");
}

/// Authorize plus call for issue show, issue history, and wiki enumerate.
fn context_board(expected_bearer: &'static str) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("board");
    let endpoint = format!("http://{}", listener.local_addr().expect("addr"));
    let handle = std::thread::spawn(move || {
        let mut bearers = Vec::new();
        for _ in 0..6 {
            let (mut socket, _) = listener.accept().expect("board accept");
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("timeout");
            let mut reader = std::io::BufReader::new(socket.try_clone().expect("clone"));
            let mut line = String::new();
            reader.read_line(&mut line).expect("request");
            let path = line.split(' ').nth(1).unwrap_or("").to_string();
            let mut bearer = String::new();
            let mut length = 0usize;
            loop {
                line.clear();
                reader.read_line(&mut line).expect("header");
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                let Some((key, value)) = line.split_once(':') else {
                    continue;
                };
                if key.eq_ignore_ascii_case("authorization") {
                    bearer = value.trim().trim_start_matches("Bearer ").to_string();
                }
                if key.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut raw = vec![0; length];
            reader.read_exact(&mut raw).expect("request body");
            bearers.push(bearer);
            let version = if path.contains("/wiki/") {
                "hosted-cadence-wiki.v1"
            } else {
                "hosted-cadence-cli.v1"
            };
            let body = if path.ends_with("/authorize") {
                json!({"version": version, "envelope": "h.e.1"})
            } else if path.contains("/wiki/") {
                json!({"path": "", "entries": []})
            } else {
                json!({"id": "CAD1460", "title": "claimed by A"})
            };
            assert_eq!(bearers.last().map(String::as_str), Some(expected_bearer));
            http_response(&mut socket, &body);
        }
        bearers
    });
    (endpoint, handle)
}

#[test]
fn swapped_login_supplies_no_context_and_renewed_claimant_still_can() {
    if isolated() {
        return;
    }
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let listener = quiet_listener();
    let endpoint = format!("http://{}", listener.local_addr().expect("addr"));
    install_login(config.path(), &endpoint, SUBJECT_A, TOKEN_A, 1);
    install_login(config.path(), &endpoint, SUBJECT_B, TOKEN_B, 1);
    let kickoff = kickoff_fixture(Some(SUBJECT_A), Some(ISSUER));
    let refused = with_auth(config.path(), || {
        let started = std::time::Instant::now();
        let result = deliver_kickoff(
            &target(&endpoint),
            Some(state.path().to_path_buf()),
            &kickoff,
            3,
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "refusal waited on the local actor"
        );
        result
    });
    assert!(refused.is_err(), "member B delivered the kickoff");
    assert!(
        listener.accept().unwrap_err().kind() == std::io::ErrorKind::WouldBlock,
        "member B received a context request"
    );
    assert!(!state.path().join("hosted-assignments").exists());
    assert!(!state.path().join("cadence.sock").exists());

    let (board, server) = context_board(TOKEN_A_RENEWED);
    install_login(config.path(), &board, SUBJECT_A, TOKEN_A_RENEWED, 2);
    let kickoff = kickoff_fixture(Some(SUBJECT_A), Some(ISSUER));
    let context = with_auth(config.path(), || {
        require_context(
            &claimant_target(&target(&board), &kickoff).expect("pin"),
            &kickoff,
            5,
        )
    })
    .expect("renewed member A context");
    let bearers = server.join().expect("board");
    assert_eq!(bearers.len(), 6);
    assert!(bearers.iter().all(|bearer| bearer == TOKEN_A_RENEWED));
    assert_eq!(context["issue"]["title"], json!("claimed by A"));
    assert_eq!(context["organization_id"], json!(ORG_ID));
    assert!(context.get("context_error").is_none());
}

#[test]
fn cached_handoff_refuses_a_replacement_login_before_the_actor() {
    if isolated() {
        return;
    }
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let listener = quiet_listener();
    let endpoint = format!("http://{}", listener.local_addr().expect("addr"));
    install_login(config.path(), &endpoint, SUBJECT_B, TOKEN_B, 1);
    let kickoff = kickoff_fixture(Some(SUBJECT_A), Some(ISSUER));
    store_kickoff_binding(state.path(), ORG, &endpoint, &kickoff, "already bound").expect("cache");
    let refused = with_auth(config.path(), || {
        let started = std::time::Instant::now();
        let result = deliver_kickoff(
            &target(&endpoint),
            Some(state.path().to_path_buf()),
            &kickoff,
            3,
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        result
    });
    assert!(refused.is_err(), "cached binding was handed to member B");
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(!state.path().join("cadence.sock").exists());
}

#[test]
fn a_missing_or_partial_claimant_pin_never_uses_the_saved_login() {
    if isolated() {
        return;
    }
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let listener = quiet_listener();
    let endpoint = format!("http://{}", listener.local_addr().expect("addr"));
    install_login(config.path(), &endpoint, SUBJECT_A, TOKEN_A, 1);
    for (subject, issuer) in [
        (None, None),
        (Some(SUBJECT_A), None),
        (None, Some(ISSUER)),
        (Some(""), Some(ISSUER)),
        (Some(SUBJECT_A), Some("")),
    ] {
        let kickoff = kickoff_fixture(subject, issuer);
        let refused = with_auth(config.path(), || {
            deliver_kickoff(
                &target(&endpoint),
                Some(state.path().to_path_buf()),
                &kickoff,
                3,
            )
        });
        assert!(refused.is_err(), "unpinned kickoff used the saved login");
    }
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(!state.path().join("cadence.sock").exists());
}
