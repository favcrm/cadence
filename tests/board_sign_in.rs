//! board_sign_in: area tests split from tests/board.rs (CAD-537).
//! Board e2e: the `cadence issue` CLI against a temp PM dir, and the
//! `cadence ui` HTTP server in-process.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

use cadence_agent::ui;
use serde_json::json;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

// ---------- CAD-526: public-name sign-in (AgenticOS assertion) ----------

/// The platform's JWKS endpoint, stubbed: answers
/// `/.well-known/agenticos-board-jwks.json` only, and counts hits so a
/// test can see a cache miss refetch.
fn jwks_stub(body: String) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let port = free_port();
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = hits.clone();
    thread::spawn(move || {
        let server = tiny_http::Server::http(format!("127.0.0.1:{port}")).unwrap();
        loop {
            match server.recv_timeout(Duration::from_millis(100)) {
                Ok(Some(req)) => {
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let resp = if req.url() == "/.well-known/agenticos-board-jwks.json" {
                        tiny_http::Response::from_string(body.clone()).with_status_code(200)
                    } else {
                        tiny_http::Response::from_string("{}").with_status_code(404)
                    };
                    let _ = req.respond(resp);
                }
                Ok(None) => {}
                Err(_) => return,
            }
        }
    });
    (port, hits)
}

/// An Ed25519 signer standing in for the platform — a fixed seed makes
/// the minted assertions deterministic; `seed` chooses whose key.
fn board_signer(seed: u8) -> ring::signature::Ed25519KeyPair {
    ring::signature::Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap()
}

/// The signer's `x` parameter, base64url — the JWKS publishes this.
fn board_pubkey(signer: &ring::signature::Ed25519KeyPair) -> String {
    use base64::Engine;
    use ring::signature::KeyPair;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signer.public_key().as_ref())
}

/// `header.payload` signed by `signer` — the compact JWS the handoff
/// page posts. `kid` defaults to `"k1"`.
fn board_assertion(signer: &ring::signature::Ed25519KeyPair, kid: &str, claims: &Value) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let h =
        b64.encode(serde_json::to_vec(&json!({"alg": "EdDSA", "typ": "JWT", "kid": kid})).unwrap());
    let p = b64.encode(serde_json::to_vec(claims).unwrap());
    let signed = format!("{h}.{p}");
    format!(
        "{signed}.{}",
        b64.encode(signer.sign(signed.as_bytes()).as_ref())
    )
}

/// Now, in the shape the platform mints it: iat a beat ago, 30 s to live.
fn board_claims(issuer: &str, aud: &str, jti: &str) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    json!({
        "iss": issuer,
        "aud": aud,
        "sub": "usr_9",
        "email": "fable@example.com",
        "name": "Fable Chen",
        "company": "co_1",
        "role": "owner",
        "iat": now - 5,
        "exp": now + 30,
        "jti": jti,
    })
}

/// A board configured for public-name sign-in: the public host is this
/// process's own port under `*.board.localhost` (the contract's local
/// shape — `aud` carries the port); the issuer is the JWKS stub.
/// Returns the port, the public `Host` value and the board's guard.
fn start_public_board(pm: &Path, state: &Path, issuer: String) -> (u16, String, BoardStop) {
    let moved = issuer.clone();
    let (port, board) = start_ui_opts(pm.to_path_buf(), state.to_path_buf(), move |opts| {
        let host = format!("acme.board.localhost:{}", opts.port);
        opts.allow_hosts.push(host.clone());
        opts.allow_origins.push(format!("http://{host}"));
        opts.public = Some(ui::PublicBoard {
            host: host.clone(),
            issuer: moved.clone(),
            company: "co_1".to_string(),
            authorize_url: format!("{moved}/v2/board/authorize"),
        });
    });
    (port, format!("acme.board.localhost:{port}"), board)
}

/// CAD-747 regression: a process in the hosted container can bypass the
/// Worker by choosing Cadence's otherwise valid local Host itself.
#[test]
fn hosted_public_only_refuses_local_host_before_board_reads() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        |opts| {
            let host = format!("acme.board.localhost:{}", opts.port);
            opts.allow_hosts.push(host.clone());
            opts.allow_origins.push(format!("http://{host}"));
            opts.public = Some(ui::PublicBoard {
                host,
                issuer: "http://api.internal".to_string(),
                company: "co_1".to_string(),
                authorize_url: "http://api.internal/v2/board/authorize".to_string(),
            });
            opts.board_public_only = true;
        },
    );
    let _board = board;
    let local = format!("127.0.0.1:{port}");
    let public = format!("acme.board.localhost:{port}");
    for path in [
        "/api/issues",
        "/api/overview",
        "/api/projects",
        "/api/setup",
    ] {
        let (status, body) = http(port, "GET", path, &local);
        assert_eq!(status, 421, "{path}: {body}");
    }
    for other_host in [
        format!("localhost:{port}"),
        "cadence.localhost".to_string(),
        format!("cadence-{port}.localhost:{port}"),
    ] {
        let (status, body) = http(port, "GET", "/api/issues", &other_host);
        assert_eq!(status, 421, "{other_host}: {body}");
    }
    for disguised_health in ["/api/health?full=1", "/api/%68ealth"] {
        assert_eq!(http(port, "GET", disguised_health, &local).0, 421);
    }
    let (status, _, body) = http_write(
        port,
        "GET",
        "/api/issues",
        &local,
        &[
            "X-Forwarded-Host: acme.board.localhost",
            "X-Cadence-Test-As: operator",
        ],
        b"",
    );
    assert_ne!(
        status, 200,
        "forged identity header escaped the host gate: {body}"
    );
    for join in (0..8).map(|_| {
        let local = local.clone();
        thread::spawn(move || http(port, "GET", "/api/issues", &local))
    }) {
        let (status, body) = join.join().unwrap();
        assert_eq!(status, 421, "parallel local read: {body}");
    }
    // A caller can hold a write body after the headers. Once released,
    // admission must still refuse it before any mutation is reached.
    let mut held = TcpStream::connect(("127.0.0.1", port)).unwrap();
    held.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    held.write_all(format!("POST /api/issues HTTP/1.0\r\nHost: {local}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n").as_bytes()).unwrap();
    thread::sleep(Duration::from_millis(50));
    held.write_all(b"{}").unwrap();
    let mut answer = String::new();
    held.read_to_string(&mut answer).unwrap();
    assert!(
        answer.starts_with("HTTP/1.1 421") || answer.starts_with("HTTP/1.0 421"),
        "{answer}"
    );

    // A detached agent-shaped process exercises the kernel TCP path,
    // rather than an in-process call to the dispatch function.
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "hosted_public_only_detached_child_probe",
            "--nocapture",
        ])
        .env("CADENCE_BOARD_PROBE_PORT", port.to_string())
        .env("CADENCE_ALIAS", "agent:untrusted");
    unsafe {
        child.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = child.output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(http(port, "GET", "/api/issues", &public).0, 401);
    let (status, health) = http(port, "GET", "/api/health", &local);
    assert_eq!(status, 200, "{health}");
    let health: Value = serde_json::from_str(&health).unwrap();
    assert_eq!(health["ok"], true);
    assert!(health["build"].is_string());
    assert_eq!(
        health.as_object().unwrap().len(),
        2,
        "local health leaked board state"
    );
}

#[test]
fn hosted_public_only_detached_child_probe() {
    let Ok(port) = std::env::var("CADENCE_BOARD_PROBE_PORT") else {
        return;
    };
    let port = port.parse::<u16>().unwrap();
    let local = format!("127.0.0.1:{port}");
    let (status, body) = http(port, "GET", "/api/issues", &local);
    assert_eq!(status, 421, "detached agent read: {body}");
}

#[test]
fn hosted_public_only_cannot_be_persisted_over_a_running_open_board() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, host, _board) =
        start_public_board(pm.path(), state.path(), "http://api.internal".to_string());
    let local = format!("127.0.0.1:{port}");
    assert_eq!(http(port, "GET", "/api/issues", &local).0, 200);

    let saved = ui::UiOpts {
        port: Some(port),
        board: Some(ui::PublicBoard {
            host: host.clone(),
            issuer: "http://api.internal".to_string(),
            company: "co_1".to_string(),
            authorize_url: "http://api.internal/v2/board/authorize".to_string(),
        }),
        ..Default::default()
    };
    let opts_path = state.path().join("ui.json");
    let before = serde_json::to_vec_pretty(&saved).unwrap();
    std::fs::write(&opts_path, &before).unwrap();
    // `read_pid` sees a live process, while the in-process board above
    // supplies the real open HTTP peer. Never call `ui stop` on this fake
    // pidfile: that would signal the test runner itself.
    std::fs::write(state.path().join("ui.pid"), std::process::id().to_string()).unwrap();
    let flags = ui::UiFlags {
        board_public_only: true,
        ..Default::default()
    };
    let result = ui::run_cli(
        state.path(),
        &ui::UiAction::Start {
            flags,
            reset: false,
        },
    );
    assert!(
        result.is_err(),
        "running board falsely reported public-only activation"
    );
    assert_eq!(std::fs::read(&opts_path).unwrap(), before);
    assert_eq!(http(port, "GET", "/api/issues", &local).0, 200);

    let reset_flags = ui::UiFlags {
        board_public_only: true,
        board_host: Some(host),
        board_issuer: Some("http://api.internal".to_string()),
        board_company: Some("co_1".to_string()),
        ..Default::default()
    };
    let result = ui::run_cli(
        state.path(),
        &ui::UiAction::Start {
            flags: reset_flags,
            reset: true,
        },
    );
    assert!(
        result.is_err(),
        "--reset falsely reported public-only activation"
    );
    assert_eq!(std::fs::read(&opts_path).unwrap(), before);

    // A prior buggy start could have written `true` while the old process
    // remained open. Persisted state alone may never certify that process.
    let stale = ui::UiOpts {
        board_public_only: true,
        ..saved
    };
    let stale_bytes = serde_json::to_vec_pretty(&stale).unwrap();
    std::fs::write(&opts_path, &stale_bytes).unwrap();
    let result = ui::run_cli(
        state.path(),
        &ui::UiAction::Start {
            flags: ui::UiFlags::default(),
            reset: false,
        },
    );
    assert!(
        result.is_err(),
        "stale true flag falsely certified an open board"
    );
    assert_eq!(std::fs::read(&opts_path).unwrap(), stale_bytes);
    assert_eq!(http(port, "GET", "/api/issues", &local).0, 200);
}

#[test]
fn hosted_public_only_cannot_change_live_public_identity_without_restart() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        |opts| {
            let host = format!("acme.board.localhost:{}", opts.port);
            opts.allow_hosts.push(host.clone());
            opts.public = Some(ui::PublicBoard {
                host,
                issuer: "http://api.internal".to_string(),
                company: "co_1".to_string(),
                authorize_url: "http://api.internal/v2/board/authorize".to_string(),
            });
            opts.board_public_only = true;
        },
    );
    let saved = ui::UiOpts {
        port: Some(port),
        board_public_only: true,
        board: Some(ui::PublicBoard {
            host: format!("acme.board.localhost:{port}"),
            issuer: "http://api.internal".to_string(),
            company: "co_1".to_string(),
            authorize_url: "http://api.internal/v2/board/authorize".to_string(),
        }),
        ..Default::default()
    };
    let opts_path = state.path().join("ui.json");
    let before = serde_json::to_vec_pretty(&saved).unwrap();
    std::fs::write(&opts_path, &before).unwrap();
    std::fs::write(state.path().join("ui.pid"), std::process::id().to_string()).unwrap();
    let result = ui::run_cli(
        state.path(),
        &ui::UiAction::Start {
            flags: ui::UiFlags {
                board_host: Some(format!("other.board.localhost:{port}")),
                ..Default::default()
            },
            reset: false,
        },
    );
    assert!(
        result.is_err(),
        "running board falsely claimed a new public identity"
    );
    assert_eq!(std::fs::read(&opts_path).unwrap(), before);
    assert_eq!(
        http(port, "GET", "/api/issues", &format!("127.0.0.1:{port}")).0,
        421
    );
}

/// The real headers a contract exchange carries (the worker's callback
/// page fetches same-origin; a form post cannot set Content-Type: json).
fn session_post(port: u16, host: &str, body: &[u8]) -> (u16, String, String) {
    http_write(
        port,
        "POST",
        "/__platform/session",
        host,
        &[
            "Content-Type: application/json",
            "Sec-Fetch-Site: same-origin",
            &format!("Origin: http://{host}"),
        ],
        body,
    )
}

fn set_cookie_line(headers: &str) -> Option<String> {
    headers
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .map(|l| l["set-cookie:".len()..].trim().to_string())
}

/// The `token` the `__Host-` cookie carries.
fn cookie_token(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_string()
}

/// CAD-526 §4–§9 end to end: a signed assertion exchanges for a
/// `__Host-aos-board-session` on the public host; that cookie alone
/// reads the board; an absent session bounces a navigation to the
/// platform authorize URL and fails an API read `401`; a replayed
/// `jti` is `assertion_replayed`; the local sign-in path never answers
/// on the public host.
#[test]
fn platform_sign_in_opens_and_gates_a_named_session() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let signer = board_signer(0x9d);
    let jwks = format!(
        r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","kid":"k1","x":"{}"}}]}}"#,
        board_pubkey(&signer)
    );
    let (jwks_port, hits) = jwks_stub(jwks);
    let issuer = format!("http://127.0.0.1:{jwks_port}");
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, host, _board) = start_public_board(pm.path(), state.path(), issuer.clone());
    let loopback = format!("127.0.0.1:{port}");

    // A navigation without a session bounces to the platform authorize
    // URL with this page as `to` — and an API read gets 401, not a
    // redirect (no SPA fetch is mistaken for a navigation).
    let (code, headers, _) =
        http_write(port, "GET", "/projects", &host, &["Accept: text/html"], b"");
    assert_eq!(code, 302);
    let loc = headers
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("location:"))
        .unwrap()
        .to_string();
    let want_prefix = format!("{issuer}/v2/board/authorize?to=");
    assert!(loc.contains(&want_prefix), "{loc}");
    assert!(loc.contains("acme.board.localhost"), "{loc}");
    let (code, body) = http(port, "GET", "/api/issues", &host);
    assert_eq!(code, 401, "{body}");
    assert!(body.contains("board_session_required"), "{body}");
    // Health stays open for probes; the loopback surface is untouched.
    assert_eq!(http(port, "GET", "/api/health", &host).0, 200);
    assert_eq!(http(port, "GET", "/api/issues", &loopback).0, 200);
    // No local sign-in on the public name — `/api/session` refuses, and
    // the platform route is not mounted on loopback.
    let (code, _, body) = http_write(
        port,
        "POST",
        "/api/session",
        &host,
        &[
            "Content-Type: application/json",
            "X-Cadence-Board: 1",
            &format!("Origin: http://{host}"),
        ],
        b"{}",
    );
    assert_eq!(code, 403, "{body}");
    let (code, _, _) = session_post(
        port,
        &loopback,
        json!({"assertion": "x"}).to_string().as_bytes(),
    );
    assert_eq!(code, 403); // operator-only route shape, not the exchange

    // The exchange: verified assertion → `{"ok":true}` plus the
    // contract cookie — `__Host-`, Secure, Lax, Path=/, no Domain,
    // 60-minute ceiling.
    let assertion = board_assertion(&signer, "k1", &board_claims(&issuer, &host, "jti-1"));
    let (code, headers, body) = session_post(
        port,
        &host,
        json!({"assertion": assertion}).to_string().as_bytes(),
    );
    assert_eq!(code, 200, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["ok"], true);
    let set = set_cookie_line(&headers).expect("no Set-Cookie");
    for want in [
        "__Host-aos-board-session=",
        "HttpOnly",
        "Secure",
        "SameSite=Lax",
        "Path=/",
        "Max-Age=3600",
    ] {
        assert!(set.contains(want), "Set-Cookie missing {want}: {set}");
    }
    assert!(!set.contains("Domain"), "{set}");
    let token = cookie_token(&set);
    let cookie = format!("Cookie: __Host-aos-board-session={token}");

    let (code, _, body) = http_write(port, "GET", "/api/meta", &host, &[&cookie], b"");
    assert_eq!(code, 200, "{body}");
    let meta: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(meta["hosted"], true);
    assert_eq!(meta["session"]["user"]["name"], "Fable Chen");
    assert_eq!(meta["session"]["user"]["email"], "fable@example.com");
    assert_eq!(meta["session"]["user"]["role"], "operator");
    assert_eq!(meta["actor"], "Fable Chen <fable@example.com> (board)");
    let (code, local) = http(port, "GET", "/api/meta", &loopback);
    assert_eq!(code, 200, "{local}");
    let local: Value = serde_json::from_str(&local).unwrap();
    assert_eq!(local["hosted"], false);
    assert_eq!(local["actor"], "operator (ui)");

    // The cookie alone is the credential: reads pass with it.
    let (code, _, body) = http_write(port, "GET", "/api/issues", &host, &[&cookie], b"");
    assert_eq!(code, 200, "{body}");

    // Replay: the same assertion mints nothing twice.
    let (code, _, body) = session_post(
        port,
        &host,
        json!({"assertion": assertion}).to_string().as_bytes(),
    );
    assert_eq!(code, 401, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["error"]["code"],
        "assertion_replayed"
    );
    // One JWKS fetch served both verifications — the cache holds.
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);

    // A fresh sign-in for the same user ends the first session —
    // named users never stack.
    let again = board_assertion(&signer, "k1", &board_claims(&issuer, &host, "jti-2"));
    let (code, headers2, _) = session_post(
        port,
        &host,
        json!({"assertion": again}).to_string().as_bytes(),
    );
    assert_eq!(code, 200);
    let token2 = cookie_token(&set_cookie_line(&headers2).unwrap());
    let (code, ..) = http_write(port, "GET", "/api/issues", &host, &[&cookie], b"");
    assert_eq!(code, 401, "the replaced session must be dead");
    let cookie2 = format!("Cookie: __Host-aos-board-session={token2}");
    let (code, ..) = http_write(port, "GET", "/api/issues", &host, &[&cookie2], b"");
    assert_eq!(code, 200);
    // Still one fetch — same kid, warm cache.
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);

    // An unknown kid forces a refetch — which still fails closed when
    // the platform publishes no such key.
    let unknown = board_assertion(&signer, "k2", &board_claims(&issuer, &host, "jti-3"));
    let (code, _, body) = session_post(
        port,
        &host,
        json!({"assertion": unknown}).to_string().as_bytes(),
    );
    assert_eq!(code, 401);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["error"]["code"],
        "assertion_invalid"
    );
    assert_eq!(
        hits.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "kid miss refetches"
    );
}

/// Every refusal of the exchange carries the contract's error code and
/// mints no cookie.
#[test]
fn platform_session_refusals() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let signer = board_signer(0x9d);
    let jwks = format!(
        r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","kid":"k1","x":"{}"}}]}}"#,
        board_pubkey(&signer)
    );
    let (jwks_port, _) = jwks_stub(jwks);
    let issuer = format!("http://127.0.0.1:{jwks_port}");
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, host, _board) = start_public_board(pm.path(), state.path(), issuer.clone());

    let refused = |assertion: String, want_code: &str, want_status: u16| {
        let (code, headers, body) = session_post(
            port,
            &host,
            json!({"assertion": assertion}).to_string().as_bytes(),
        );
        assert_eq!(code, want_status, "{want_code}: {body}");
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["error"]["code"],
            json!(want_code),
            "{body}"
        );
        assert!(
            set_cookie_line(&headers).is_none(),
            "a refusal sets no cookie"
        );
    };

    // Forged signature — signed by a key the platform never published.
    refused(
        board_assertion(
            &board_signer(0x77),
            "k1",
            &board_claims(&issuer, &host, "jti-f"),
        ),
        "assertion_invalid",
        401,
    );
    // A sibling board's assertion, honestly signed.
    refused(
        board_assertion(
            &signer,
            "k1",
            &board_claims(&issuer, "other.board.localhost:9", "jti-a"),
        ),
        "audience_mismatch",
        403,
    );
    // A foreign issuer — refused before the JWKS fetch would trust it.
    refused(
        board_assertion(
            &signer,
            "k1",
            &board_claims("http://evil.test", &host, "jti-i"),
        ),
        "issuer_mismatch",
        401,
    );
    // Expired.
    let mut c = board_claims(&issuer, &host, "jti-e");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    c["iat"] = json!(now - 90);
    c["exp"] = json!(now - 60);
    refused(board_assertion(&signer, "k1", &c), "assertion_expired", 401);
    // Another company's member.
    let mut c = board_claims(&issuer, &host, "jti-c");
    c["company"] = json!("co_2");
    refused(board_assertion(&signer, "k1", &c), "not_a_member", 403);
    // A membership role the board does not map.
    let mut c = board_claims(&issuer, &host, "jti-r");
    c["role"] = json!("admin");
    refused(board_assertion(&signer, "k1", &c), "role_unmapped", 403);

    // Request-level refusals before any crypto runs.
    let (code, _, body) = http_write(
        port,
        "POST",
        "/__platform/session",
        &host,
        &["Content-Type: text/plain"],
        b"assertion=x",
    );
    assert_eq!(code, 400);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["error"]["code"],
        "assertion_invalid"
    );
    let (code, _, body) = session_post(port, &host, b"not json");
    assert_eq!(code, 400);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["error"]["code"],
        "assertion_invalid"
    );
    let (code, _, _) = http_write(
        port,
        "POST",
        "/__platform/session",
        &host,
        &[
            "Content-Type: application/json",
            "Sec-Fetch-Site: cross-site",
        ],
        json!({"assertion": "x"}).to_string().as_bytes(),
    );
    assert_eq!(code, 403);
    let (code, _, _) = http_write(
        port,
        "POST",
        "/__platform/session",
        &host,
        &[
            "Content-Type: application/json",
            "Origin: http://evil.example",
        ],
        json!({"assertion": "x"}).to_string().as_bytes(),
    );
    assert_eq!(code, 403);

    // The reserved prefix's other routes: callback bounces to the board
    // root (the worker serves the page itself), login bounces to the
    // authorize URL, everything else is a plain 404 — any method.
    assert_eq!(http(port, "GET", "/__platform/callback", &host).0, 302);
    let (code, headers, _) = http_full(port, "GET", "/__platform/login", &host);
    assert_eq!(code, 302);
    let loc = headers
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("location:"))
        .unwrap()
        .to_string();
    assert!(loc.contains("/v2/board/authorize?to="), "{loc}");
    for (method, path) in [
        ("GET", "/__platform/session"),
        ("GET", "/__platform/nope"),
        ("POST", "/__platform/callback"),
        ("DELETE", "/__platform/session"),
    ] {
        let (code, _, _) = http_write(port, method, path, &host, &[], b"");
        assert_eq!(code, 404, "{method} {path}");
    }
}

/// The daemon fails closed: a board whose platform JWKS is unreachable
/// mints nothing — `capability_unavailable`, no cookie.
#[test]
fn platform_session_fails_closed_when_jwks_is_unreachable() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    // No stub — the issuer's port refuses connections.
    let issuer = format!("http://127.0.0.1:{}", free_port());
    let (port, host, _board) = start_public_board(pm.path(), state.path(), issuer.clone());
    let assertion = board_assertion(
        &board_signer(0x9d),
        "k1",
        &board_claims(&issuer, &host, "jti-1"),
    );
    let (code, headers, body) = session_post(
        port,
        &host,
        json!({"assertion": assertion}).to_string().as_bytes(),
    );
    assert_eq!(code, 503, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["error"]["code"],
        "capability_unavailable"
    );
    assert!(set_cookie_line(&headers).is_none());
}

/// A connection that derives an agent is refused before the assertion's
/// `jti` is spent — an agent never mints a browser session, and a real
/// sign-in still opens afterward (CAD-526; the rule a hostile agent
/// would hammer).
#[test]
fn board_session_open_refuses_an_agent_before_consuming_jti() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let signer = board_signer(0x9d);
    let jwks = format!(
        r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","kid":"k1","x":"{}"}}]}}"#,
        board_pubkey(&signer)
    );
    let (jwks_port, _) = jwks_stub(jwks);
    let issuer = format!("http://127.0.0.1:{jwks_port}");
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (_port, host, _board) = start_public_board(pm.path(), state.path(), issuer.clone());
    let assertion = board_assertion(&signer, "k1", &board_claims(&issuer, &host, "jti-agent"));

    // This process on a planted pane — the caller the gate must see.
    plant_pane(&d, "rogue", std::process::id());
    let err = d
        .rpc_opt("board_session_open", json!({"assertion": assertion}))
        .unwrap_err();
    assert!(err.to_string().contains("agent 'rogue'"), "{err}");

    // The jti was not consumed: the same assertion through an
    // unattributed (operator-shell) connection opens the session.
    let opened = d
        .operator_rpc("board_session_open", json!({"assertion": assertion}))
        .unwrap();
    assert_eq!(opened["ok"], true, "{opened}");
    assert_eq!(opened["session"]["origin"], "public");
    assert_eq!(opened["session"]["user"]["sub"], "usr_9");
    assert_eq!(opened["session"]["user"]["role"], "operator");
    // ...and the check the board runs answers it.
    let token = opened["token"].as_str().unwrap().to_string();
    let checked = d.rpc("board_session_check", json!({"token": token}));
    assert_eq!(checked["valid"], true);
    assert_eq!(checked["session"]["user"]["email"], "fable@example.com");
}

/// A `member` session reads and participates but never decides: the
/// owner-only routes refuse it; an agent-allowed write runs under its
/// own name.
#[test]
fn a_member_session_never_decides() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let signer = board_signer(0x9d);
    let jwks = format!(
        r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","kid":"k1","x":"{}"}}]}}"#,
        board_pubkey(&signer)
    );
    let (jwks_port, _) = jwks_stub(jwks);
    let issuer = format!("http://127.0.0.1:{jwks_port}");
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, host, _board) = start_public_board(pm.path(), state.path(), issuer.clone());

    let mut c = board_claims(&issuer, &host, "jti-m");
    c["role"] = json!("member");
    let assertion = board_assertion(&signer, "k1", &c);
    let (code, headers, body) = session_post(
        port,
        &host,
        json!({"assertion": assertion}).to_string().as_bytes(),
    );
    assert_eq!(code, 200, "{body}");
    let cookie = format!(
        "Cookie: __Host-aos-board-session={}",
        cookie_token(&set_cookie_line(&headers).unwrap())
    );

    // Participate: an agent-allowed write succeeds and is attributed to
    // the member's handle, not `operator`.
    let (code, _, body) = http_write(
        port,
        "GET",
        "/api/meta?operator=1&role=operator&actor=forged",
        &host,
        &[&cookie, "Tailscale-User-Login: forged@example.com"],
        b"",
    );
    assert_eq!(code, 200, "{body}");
    let meta: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(meta["session"]["user"]["role"], "member");
    assert_eq!(meta["operator"], false);
    assert_eq!(meta["actor"], "Fable Chen <fable@example.com> (board)");
    let (code, _, body) = http_write(
        port,
        "POST",
        "/api/issues/CAD-1/comments",
        &host,
        &[
            "Content-Type: application/json",
            "X-Cadence-Board: 1",
            "Sec-Fetch-Site: same-origin",
            &format!("Origin: http://{host}"),
            &cookie,
        ],
        json!({"body": "member note"}).to_string().as_bytes(),
    );
    assert_eq!(code, 200, "{body}");
    let (_, _, body) = http_write(port, "GET", "/api/issues/CAD-1", &host, &[&cookie], b"");
    assert!(body.contains("member note"), "{body}");
    // ...and it is attributed to the member's handle, not `operator` —
    // the comment file is named for and authored by `usr_9`.
    let comments_dir = pm.path().join("cadence/CAD-1/comments");
    let file = std::fs::read_dir(&comments_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().ends_with(".md"))
        .unwrap();
    let name = file.file_name().to_string_lossy().to_string();
    assert!(name.contains("usr_9"), "{name}");
    let text = std::fs::read_to_string(file.path()).unwrap();
    assert!(text.contains("author: usr_9"), "{text}");

    // Decide: the operator-only routes refuse a member — never an
    // operator proof fallback.
    let (code, _, body) = http_write(
        port,
        "DELETE",
        "/api/issues/CAD-1",
        &host,
        &[
            "Content-Type: application/json",
            "X-Cadence-Board: 1",
            "Sec-Fetch-Site: same-origin",
            &format!("Origin: http://{host}"),
            &cookie,
        ],
        b"{}",
    );
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("member_role"), "{body}");

    // CAD-606: Kick off is the owner's decision the same way — a member
    // session is refused before the handler runs.
    let (code, _, body) = http_write(
        port,
        "POST",
        "/api/issues/CAD-1/kickoff",
        &host,
        &[
            "Content-Type: application/json",
            "X-Cadence-Board: 1",
            "Sec-Fetch-Site: same-origin",
            &format!("Origin: http://{host}"),
            &cookie,
        ],
        br#"{"group":"pm","provider":"fake"}"#,
    );
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("member_role"), "{body}");

    // CAD-557: the app-approval route is the owner's decision the same
    // way — a member session is refused before the handler runs.
    let (code, _, body) = http_write(
        port,
        "POST",
        "/api/apps/cadence/studio/approve",
        &host,
        &[
            "Content-Type: application/json",
            "X-Cadence-Board: 1",
            "Sec-Fetch-Site: same-origin",
            &format!("Origin: http://{host}"),
            &cookie,
        ],
        b"{}",
    );
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("member_role"), "{body}");
}
