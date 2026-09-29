//! board_sign_in: area tests split from tests/board.rs (CAD-537).
//! Board e2e: the `cadence issue` CLI against a temp PM dir, and the
//! `cadence ui` HTTP server in-process.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

#[cfg(feature = "test-seam")]
use cadence_agent::device_login::DeviceConfig;
#[cfg(feature = "test-seam")]
use cadence_agent::store::Store;
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

// ---------- CAD-777: device-grant sign-in ----------
//
// The whole area is test-build only: the issuer stub serves plain-http
// loopback, which `DeviceConfig` accepts only under `test-seam` (the
// feature the suite runs with) — under a production feature shape the
// tests below are absent, not silently passing.
#[cfg(feature = "test-seam")]
mod device {
    use super::*;

    /// A scripted AgenticOS device issuer: device code, token poll and
    /// session check. `mode` is `approve`, `deny` or `wrongorg`; the first
    /// token poll is always pending so the board proves it waits.
    struct DeviceStub {
        mode: std::sync::Mutex<String>,
        polls: std::sync::atomic::AtomicUsize,
        /// `/v1/runtime/session` hits — the issuer-side verify. A refused
        /// caller must never reach it.
        verifies: std::sync::atomic::AtomicUsize,
    }

    fn device_stub(mode: &str) -> (String, std::sync::Arc<DeviceStub>) {
        let port = free_port();
        let stub = std::sync::Arc::new(DeviceStub {
            mode: std::sync::Mutex::new(mode.to_string()),
            polls: std::sync::atomic::AtomicUsize::new(0),
            verifies: std::sync::atomic::AtomicUsize::new(0),
        });
        let serve = stub.clone();
        thread::spawn(move || {
            let server = tiny_http::Server::http(format!("127.0.0.1:{port}")).unwrap();
            loop {
                let Ok(Some(mut req)) = server.recv_timeout(Duration::from_millis(100)) else {
                    continue;
                };
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let bearer = req
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Authorization"))
                    .map(|h| h.value.as_str().to_string())
                    .unwrap_or_default();
                let (status, doc) = match (req.method(), req.url()) {
                    (tiny_http::Method::Post, "/v1/device/code") => {
                        if serve.mode.lock().unwrap().as_str() == "slow_code" {
                            // A slow issuer: the code answer takes ~3 s.
                            thread::sleep(Duration::from_secs(3));
                        }
                        (
                            200,
                            json!({
                                "device_code": "agd_t",
                                "user_code": "ABCD-1234",
                                "verification_uri": format!("http://127.0.0.1:{port}/approve"),
                                "verification_uri_complete": format!("http://127.0.0.1:{port}/approve?code=ABCD-1234"),
                                "expires_in": 600,
                                "interval": 1
                            }),
                        )
                    }
                    (tiny_http::Method::Post, "/v1/device/token") => {
                        // The real issuer's gate: the grant type is
                        // required, exactly like the CLI posts it.
                        if !body.contains("urn:ietf:params:oauth:grant-type:device_code") {
                            (400, json!({"error": "unsupported_grant_type"}))
                        } else if !body.contains("agd_t") {
                            (400, json!({"error": "invalid_grant"}))
                        } else if serve
                            .polls
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                            == 0
                        {
                            (400, json!({"error": "authorization_pending"}))
                        } else {
                            match serve.mode.lock().unwrap().as_str() {
                                "deny" => (400, json!({"error": "access_denied"})),
                                "wrongorg" => (
                                    200,
                                    json!({
                                        "access_token": "agc_t",
                                        "token_type": "bearer",
                                        "expires_in": 999,
                                        "scope": "read draft",
                                        "workspace_id": "ws_other"
                                    }),
                                ),
                                _ => (
                                    200,
                                    json!({
                                        "access_token": "agc_t",
                                        "token_type": "bearer",
                                        "expires_in": 999,
                                        "scope": "read draft",
                                        "workspace_id": "ws_company"
                                    }),
                                ),
                            }
                        }
                    }
                    (tiny_http::Method::Get, "/v1/runtime/session") => {
                        serve
                            .verifies
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if bearer == "Bearer agc_t" {
                            (
                                200,
                                json!({
                                    "ok": true,
                                    "data": {
                                        "workspace": {"id": "ws_company"},
                                        "subject": {"anonymous": false, "id": "op_9"},
                                        "scopes": ["read", "draft"]
                                    }
                                }),
                            )
                        } else {
                            (401, json!({"ok": false, "error": "unauthorized"}))
                        }
                    }
                    _ => (404, json!({"ok": false})),
                };
                let _ = req.respond(
                    tiny_http::Response::from_string(doc.to_string()).with_status_code(status),
                );
            }
        });
        (format!("http://127.0.0.1:{port}"), stub)
    }

    /// A board with device login armed for `ws_company` against the stub,
    /// allowlisting the stub's approved subject `op_9`.
    fn start_device_board(pm: &Path, state: &Path, issuer: String) -> (u16, BoardStop) {
        start_device_board_for(pm, state, issuer, vec!["op_9".to_string()])
    }

    /// `start_device_board` with a caller-chosen subject allowlist.
    fn start_device_board_for(
        pm: &Path,
        state: &Path,
        issuer: String,
        subjects: Vec<String>,
    ) -> (u16, BoardStop) {
        start_ui_opts(pm.to_path_buf(), state.to_path_buf(), move |opts| {
            opts.device_login = Some(ui::DeviceLogin::with_issuer(
                DeviceConfig::new(&issuer, "ws_company").unwrap(),
                subjects.clone(),
            ));
        })
    }

    /// Device sessions on disk (`<state>/operator/sessions.json`) — the
    /// mint count the gate tests assert on.
    fn device_session_count(state: &Path) -> usize {
        std::fs::read(state.join("operator").join("sessions.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v["sessions"].as_array().map(|s| s.len()))
            .unwrap_or(0)
    }

    /// POST to a device route on the board's own name with write guards.
    fn device_post(port: u16, host: &str, path: &str, body: &str) -> (u16, String, String) {
        op::raw(
            port,
            &op::request(
                "POST",
                path,
                host,
                Some(&format!("http://{host}")),
                None,
                body,
            ),
        )
    }

    /// The `status` of a poll answer body.
    fn status_of(body: &str) -> String {
        serde_json::from_str::<Value>(body).unwrap()["status"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Unconfigured boards answer the device routes like unknown shapes.
    #[test]
    fn device_routes_are_dead_without_configuration() {
        let pm = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        seed(pm.path(), state.path());
        let _d = UiDaemon::start_on(state.path().to_path_buf());
        let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
        let host = op::board_host(port);
        for path in ["/api/session/device/code", "/api/session/device/poll"] {
            let (code, _, body) = device_post(port, &host, path, "{}");
            assert_eq!(code, 404, "{path}: {body}");
        }
        let (code, _, _) = device_post(port, &host, "/api/session/device/code", "{}");
        assert_eq!(code, 404);
    }

    /// The full loop: code, one pending poll, approval, session cookie +
    /// key — and the cookie opens a live operator session. Issuer secrets
    /// never appear in any board response.
    #[test]
    fn device_grant_opens_a_remote_session() {
        let pm = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        seed(pm.path(), state.path());
        let _d = UiDaemon::start_on(state.path().to_path_buf());
        let (issuer, stub) = device_stub("approve");
        let (port, _board) = start_device_board(pm.path(), state.path(), issuer);
        let host = op::board_host(port);

        let (code, _, body) = device_post(port, &host, "/api/session/device/code", "{}");
        assert_eq!(code, 200, "{body}");
        let doc: Value = serde_json::from_str(&body).unwrap();
        let pending = doc["pending_id"].as_str().unwrap().to_string();
        assert_eq!(pending.len(), 64, "{body}");
        assert_eq!(doc["user_code"], json!("ABCD-1234"));
        assert!(!body.contains("agd_t"), "device code leaked: {body}");

        let (code, _, body) = device_post(
            port,
            &host,
            "/api/session/device/poll",
            &format!(r#"{{"pending_id":"{pending}"}}"#),
        );
        assert_eq!(code, 200, "{body}");
        assert_eq!(status_of(&body), "pending", "{body}");

        let (code, head, body) = device_post(
            port,
            &host,
            "/api/session/device/poll",
            &format!(r#"{{"pending_id":"{pending}"}}"#),
        );
        assert_eq!(code, 200, "{body}");
        assert!(!body.contains("agc_t"), "credential leaked: {body}");
        let key = serde_json::from_str::<Value>(&body).unwrap()["session_key"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(key.len(), 64, "{body}");
        let set = op::set_cookie(&head).unwrap();
        assert!(
            set.starts_with(&format!("cadence_operator_{port}=")),
            "{set}"
        );
        let cookie = set.split(';').next().unwrap().to_string();

        // A loopback device session is the OPERATOR's session: meta's
        // actor is what a write records (`operator (ui)`), not the
        // issuer's subject id — `held_of` attributes every non-public
        // session the same (review r4).
        let (code, _, body) = http_write(
            port,
            "GET",
            "/api/meta",
            &host,
            &[
                &format!("Cookie: {cookie}"),
                &format!("X-Cadence-Session: {key}"),
            ],
            b"",
        );
        assert_eq!(code, 200, "{body}");
        let meta: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(meta["signed_in"], json!(true), "{body}");
        assert_eq!(meta["actor"], json!("operator (ui)"), "{body}");

        // The daemon is the single live verifier: one approved grant =
        // exactly ONE `/v1/runtime/session` hit (review r5 — the board
        // must not double-verify after the code is consumed).
        assert_eq!(
            stub.verifies.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the grant was verified more than once"
        );

        // The session is live: ending it answers 204.
        let (code, _, _) = op::raw(
            port,
            &op::request(
                "POST",
                "/api/session/logout",
                &host,
                Some(&format!("http://{host}")),
                Some(&cookie),
                "{}",
            ),
        );
        assert_eq!(code, 204);

        // The grant is spent: polling again finds nothing.
        let (code, _, body) = device_post(
            port,
            &host,
            "/api/session/device/poll",
            &format!(r#"{{"pending_id":"{pending}"}}"#),
        );
        assert_eq!(code, 200, "{body}");
        assert_eq!(status_of(&body), "expired", "{body}");
    }

    /// Denial, wrong-org approval and unknown pendings all settle without
    /// a session; malformed bodies and methods refuse loudly.
    #[test]
    fn device_grant_terminal_states_settle_without_a_session() {
        let pm = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        seed(pm.path(), state.path());
        let _d = UiDaemon::start_on(state.path().to_path_buf());
        let (issuer, _stub) = device_stub("deny");
        let (port, _board) = start_device_board(pm.path(), state.path(), issuer);
        let host = op::board_host(port);
        let pending = |port: u16| -> String {
            let (code, _, body) = device_post(port, &host, "/api/session/device/code", "{}");
            assert_eq!(code, 200, "{body}");
            serde_json::from_str::<Value>(&body).unwrap()["pending_id"]
                .as_str()
                .unwrap()
                .to_string()
        };

        // Denied on second poll (first is pending), then expired.
        let id = pending(port);
        let (code, _, body) = device_post(
            port,
            &host,
            "/api/session/device/poll",
            &format!(r#"{{"pending_id":"{id}"}}"#),
        );
        assert_eq!(code, 200, "{body}");
        assert_eq!(status_of(&body), "pending", "{body}");
        let (code, head, body) = device_post(
            port,
            &host,
            "/api/session/device/poll",
            &format!(r#"{{"pending_id":"{id}"}}"#),
        );
        assert_eq!(code, 200, "{body}");
        assert_eq!(status_of(&body), "denied", "{body}");
        assert!(!head.to_ascii_lowercase().contains("set-cookie"), "{head}");
        let (code, _, body) = device_post(
            port,
            &host,
            "/api/session/device/poll",
            &format!(r#"{{"pending_id":"{id}"}}"#),
        );
        assert_eq!(code, 200, "{body}");
        assert_eq!(status_of(&body), "expired", "{body}");

        // Unknown and malformed pendings.
        let (code, _, body) = device_post(
            port,
            &host,
            "/api/session/device/poll",
            &format!(r#"{{"pending_id":"{}"}}"#, "0".repeat(64)),
        );
        assert_eq!(code, 200, "{body}");
        assert_eq!(status_of(&body), "expired", "{body}");
        let (code, _, _) = device_post(port, &host, "/api/session/device/poll", "{}");
        assert_eq!(code, 400);
        let (code, _, _) = device_post(port, &host, "/api/session/device/poll", "not json");
        assert_eq!(code, 400);
        // Like `/api/session` itself, the device exchange has no GET shape.
        let (code, _, _) = op::raw(
            port,
            &op::request(
                "GET",
                "/api/session/device/code",
                &host,
                Some(&format!("http://{host}")),
                None,
                "",
            ),
        );
        assert_eq!(code, 404);
    }

    /// A poll from a pane child is refused and spends nothing: the pending
    /// grant survives for the operator's own poll, which still approves.
    #[test]
    fn device_poll_from_a_pane_is_refused_without_side_effects() {
        let pm = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        seed(pm.path(), state.path());
        let d = UiDaemon::start_on(state.path().to_path_buf());
        let (issuer, _stub) = device_stub("approve");
        let (port, _board) = start_device_board(pm.path(), state.path(), issuer);
        let host = op::board_host(port);
        let (code, _, body) = device_post(port, &host, "/api/session/device/code", "{}");
        assert_eq!(code, 200, "{body}");
        let pending = serde_json::from_str::<Value>(&body).unwrap()["pending_id"]
            .as_str()
            .unwrap()
            .to_string();
        // Past pending: the stub answers pending once, then approves.
        let (code, _, body) = device_post(
            port,
            &host,
            "/api/session/device/poll",
            &format!(r#"{{"pending_id":"{pending}"}}"#),
        );
        assert_eq!(status_of(&body), "pending", "{code} {body}");

        // The same poll from inside a pane: refused as an agent caller.
        let req = op::request(
            "POST",
            "/api/session/device/poll",
            &host,
            Some(&format!("http://{host}")),
            None,
            &format!(r#"{{"pending_id":"{pending}"}}"#),
        );
        let mut pane = std::process::Command::new("bash")
            .args(["-c", r#"read -r _; bash -c "$CLIENT"; true"#])
            .env(
                "CLIENT",
                format!(
                "exec 3<>/dev/tcp/127.0.0.1/{port}; printf '%s' \"$REQ\" >&3; timeout 10 cat <&3"
            ),
            )
            .env("REQ", req)
            .env_remove("CADENCE_ALIAS")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        plant_pane(&d, "w-device", pane.id());
        use std::io::Write as _;
        pane.stdin.take().unwrap().write_all(b"go\n").unwrap();
        let mut out = String::new();
        use std::io::Read as _;
        pane.stdout
            .take()
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        assert!(pane.wait().unwrap().success());
        assert!(out.contains("403"), "{out}");
        assert!(out.contains("session_from_agent"), "{out}");

        // The pending survived: the operator's poll still approves.
        let (code, head, body) = device_post(
            port,
            &host,
            "/api/session/device/poll",
            &format!(r#"{{"pending_id":"{pending}"}}"#),
        );
        assert_eq!(code, 200, "{body}");
        assert!(
            serde_json::from_str::<Value>(&body).unwrap()["session_key"]
                .as_str()
                .is_some(),
            "{body}"
        );
        assert!(head.to_ascii_lowercase().contains("set-cookie"), "{head}");
    }

    /// Disabling device login while the board runs is refused — the live
    /// routes, the pin file and the saved options cannot drift apart.
    /// A restart without the pair clears the pin and the routes go 404.
    #[test]
    fn device_login_change_refused_while_running_cleared_on_restart() {
        let pm = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        seed(pm.path(), state.path());
        let _d = UiDaemon::start_on(state.path().to_path_buf());
        let (issuer, _stub) = device_stub("approve");
        let (port, board) = start_device_board(pm.path(), state.path(), issuer.clone());
        let host = op::board_host(port);
        let pin = state.path().join("operator").join("device-login.json");
        assert!(pin.is_file(), "serve writes the pin");
        let (code, _, _) = device_post(port, &host, "/api/session/device/code", "{}");
        assert_eq!(code, 200);

        // A reset while running is refused; nothing changes. The fixture
        // board never persists ui.json, so record what a real `ui start`
        // would have saved (pair + port).
        std::fs::write(
            state.path().join("ui.json"),
            serde_json::to_vec_pretty(&ui::UiOpts {
                port: Some(port),
                device_login: Some(ui::DeviceLoginOpts {
                    issuer: issuer.clone(),
                    org: "ws_company".to_string(),
                    subjects: vec!["op_9".to_string()],
                }),
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::write(state.path().join("ui.pid"), std::process::id().to_string()).unwrap();
        let reset = ui::UiFlags::default();
        let err = ui::run_cli(
            state.path(),
            &ui::UiAction::Start {
                flags: reset,
                reset: true,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("device login configuration cannot change"),
            "{err}"
        );
        assert!(pin.is_file(), "refused reset keeps the pin");
        // A subjects-only change is refused the same way — the running
        // comparison covers the allowlist (DeviceLoginOpts !=).
        let err = ui::run_cli(
            state.path(),
            &ui::UiAction::Start {
                flags: ui::UiFlags {
                    device_login_issuer: Some(issuer.clone()),
                    device_login_org: Some("ws_company".to_string()),
                    device_login_subject: vec!["op_1".to_string()],
                    ..Default::default()
                },
                reset: false,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("device login configuration cannot change"),
            "subjects-only change while running: {err}"
        );
        let (code, _, _) = device_post(port, &host, "/api/session/device/code", "{}");
        assert_eq!(code, 200, "live routes untouched by the refused reset");
        std::fs::remove_file(state.path().join("ui.pid")).unwrap();
        drop(board);

        // Restarted without the pair: pin gone, routes dead.
        let (port2, _board2) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
        assert!(!pin.exists(), "restart without the pair clears the pin");
        let host2 = op::board_host(port2);
        let (code, _, _) = device_post(port2, &host2, "/api/session/device/code", "{}");
        assert_eq!(code, 404);
    }

    /// An approval for another workspace settles with no session.
    #[test]
    fn device_grant_for_another_workspace_settles_without_a_session() {
        let pm = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        seed(pm.path(), state.path());
        let _d = UiDaemon::start_on(state.path().to_path_buf());
        let (issuer, _stub) = device_stub("wrongorg");
        let (port, _board) = start_device_board(pm.path(), state.path(), issuer);
        let host = op::board_host(port);
        let (code, _, body) = device_post(port, &host, "/api/session/device/code", "{}");
        assert_eq!(code, 200, "{body}");
        let pending = serde_json::from_str::<Value>(&body).unwrap()["pending_id"]
            .as_str()
            .unwrap()
            .to_string();
        // First poll is pending; the wrong-org approval then settles expired.
        let (code, _, _) = device_post(
            port,
            &host,
            "/api/session/device/poll",
            &format!(r#"{{"pending_id":"{pending}"}}"#),
        );
        assert_eq!(code, 200);
        let (code, head, body) = device_post(
            port,
            &host,
            "/api/session/device/poll",
            &format!(r#"{{"pending_id":"{pending}"}}"#),
        );
        assert_eq!(code, 200, "{body}");
        assert_eq!(status_of(&body), "expired", "{body}");
        assert!(!head.to_ascii_lowercase().contains("set-cookie"), "{head}");
    }

    /// The reviewer's exploit, closed: the daemon RPC mints nothing for a
    /// forged bearer, and nothing at all without its pinned trust root.
    /// The only key that opens a session is an issuer-minted grant the
    /// live issuer verifies — verified here against the stub.
    #[test]
    fn device_daemon_rpc_needs_an_issuer_verified_bearer_and_a_pin() {
        use cadence_agent::device_login::{write_pin, DevicePin};
        // No pin file anywhere: fail closed before any issuer contact.
        let lonely = TempDir::new().unwrap();
        let _alone = UiDaemon::start_on(lonely.path().to_path_buf());
        let err = _alone
            .rpc_opt(
                "operator_session_open_device",
                json!({"token": "agc_t", "origin": "loopback"}),
            )
            .unwrap_err();
        assert_eq!(
            err.code(),
            Some("capability_unavailable"),
            "unpinned daemon minted or misreported: {err}"
        );

        // Pinned daemon, forged bearer: the stub answers 401, no session.
        let state = TempDir::new().unwrap();
        let _d = UiDaemon::start_on(state.path().to_path_buf());
        let (issuer, _stub) = device_stub("approve");
        write_pin(
            state.path(),
            &DevicePin {
                issuer: issuer.clone(),
                org: "ws_company".to_string(),
                subjects: vec!["op_9".to_string()],
            },
        )
        .unwrap();
        let err = _d
            .rpc_opt(
                "operator_session_open_device",
                json!({"token": "agc_forged", "origin": "loopback"}),
            )
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("rejected") || err.contains("Issuer rejected"),
            "forged bearer minted or misreported: {err}"
        );
        // Positive control: the stub-verified grant mints (the test
        // process is no agent). Proves the gate is bearer possession +
        // live issuer verification, not field assertion.
        let opened = _d
            .rpc_opt(
                "operator_session_open_device",
                json!({"token": "agc_t", "origin": "loopback"}),
            )
            .unwrap();
        assert_eq!(opened["session"]["origin"], json!("loopback"));
        assert!(opened["token"].as_str().is_some_and(|t| t.len() == 64));
    }

    // --- adversarial gate proofs (review of #541) ---
    //
    // Each test below names the guard it pins; each was run against the
    // tree with that guard removed and failed — the failure logs are in
    // the PR thread.

    /// The daemon's events of `kind` on its own stream, read-only.
    fn device_daemon_events(state: &Path, kind: &str) -> Vec<Value> {
        let conn = rusqlite::Connection::open_with_flags(
            state.join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut stmt = conn
            .prepare("SELECT payload FROM events WHERE alias=?1 AND kind=?2 ORDER BY seq")
            .unwrap();
        let rows = stmt
            .query_map(rusqlite::params![Store::DAEMON_STREAM, kind], |r| {
                r.get::<_, String>(0)
            })
            .unwrap();
        rows.map(|r| serde_json::from_str(&r.unwrap()).unwrap())
            .collect()
    }

    /// A daemon on `state` with the device pin written for `subjects`.
    fn device_daemon(state: &Path, issuer: &str, subjects: &[&str]) -> UiDaemon {
        use cadence_agent::device_login::{write_pin, DevicePin};
        let d = UiDaemon::start_on(state.to_path_buf());
        write_pin(
            state,
            &DevicePin {
                issuer: issuer.to_string(),
                org: "ws_company".to_string(),
                subjects: subjects.iter().map(|s| s.to_string()).collect(),
            },
        )
        .unwrap();
        d
    }

    /// Run `probe` as a child of a pane planted for `alias` under `d`;
    /// `detached` wraps it in `setsid` (new session, same /proc ancestry —
    /// a double fork that reparents away is a systemic ancestry limit this
    /// test does not claim to cover). `envs` feed the probe. Returns the
    /// JSON the probe landed at its `CADENCE_PROBE_OUT`.
    fn pane_probe(d: &UiDaemon, alias: &str, detached: bool, envs: &[(&str, String)]) -> Value {
        let exe = std::env::current_exe().unwrap();
        let mut probe = format!(
            "{} --exact device::device_gate_probe --nocapture",
            exe.display()
        );
        if detached {
            // `setsid` (no --fork) detaches the child's session while
            // keeping its parent — the ancestry the gate walks is intact.
            probe = format!("setsid {probe}");
        }
        let mut pane = std::process::Command::new("bash")
            .args(["-c", r#"read -r _; bash -c "$CLIENT"; true"#])
            .env("CLIENT", probe)
            .env_remove("CADENCE_ALIAS")
            .envs(envs.iter().map(|(k, v)| (k.to_string(), v.clone())))
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        plant_pane(d, alias, pane.id());
        pane.stdin.take().unwrap().write_all(b"go\n").unwrap();
        let out = envs
            .iter()
            .find(|(k, _)| *k == "CADENCE_PROBE_OUT")
            .map(|(_, v)| v.clone())
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !Path::new(&out).exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "probe never answered at {out}"
            );
            thread::sleep(Duration::from_millis(25));
        }
        let answer: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let _ = pane.wait();
        answer
    }

    /// The gate-probe half of this binary: a no-op in the suite, a live
    /// caller only when a pane child re-execs it with `CADENCE_PROBE_*`.
    /// `rpc` calls `operator_session_open_device` on `CADENCE_PROBE_STATE`'s
    /// daemon; `poll` posts the device poll to `CADENCE_PROBE_PORT` and
    /// lands `{status, headers, body}` at `CADENCE_PROBE_OUT`.
    #[test]
    fn device_gate_probe() {
        let Ok(kind) = std::env::var("CADENCE_PROBE_KIND") else {
            return;
        };
        let out = std::env::var("CADENCE_PROBE_OUT").unwrap();
        let answer = match kind.as_str() {
            "rpc" => {
                let state = std::env::var("CADENCE_PROBE_STATE").unwrap();
                match cadence_agent::client::rpc(
                    Path::new(&state),
                    "operator_session_open_device",
                    json!({"token": "agc_t", "origin": "loopback"}),
                ) {
                    Ok(v) => json!({"ok": true, "text": v.to_string()}),
                    Err(e) => json!({"ok": false, "text": e.to_string()}),
                }
            }
            "poll" => {
                let port: u16 = std::env::var("CADENCE_PROBE_PORT")
                    .unwrap()
                    .parse()
                    .unwrap();
                let host = std::env::var("CADENCE_PROBE_HOST").unwrap();
                let pending = std::env::var("CADENCE_PROBE_PENDING").unwrap();
                let (status, headers, body) = device_post(
                    port,
                    &host,
                    "/api/session/device/poll",
                    &format!(r#"{{"pending_id":"{pending}"}}"#),
                );
                json!({"status": status, "headers": headers, "body": body})
            }
            other => json!({"ok": false, "text": format!("unknown probe kind {other}")}),
        };
        std::fs::write(format!("{out}.tmp"), answer.to_string()).unwrap();
        std::fs::rename(format!("{out}.tmp"), &out).unwrap();
    }

    /// The RPC's `slot_identity` gate: an agent pane's child presenting a
    /// stub-valid grant is refused before any issuer contact — no
    /// `/v1/runtime/session` hit, no session row.
    #[test]
    fn device_rpc_from_a_pane_is_refused_before_issuer_contact() {
        let state = TempDir::new().unwrap();
        let (issuer, stub) = device_stub("approve");
        let d = device_daemon(state.path(), &issuer, &["op_9"]);
        let out_file = state.path().join("probe-out.json");
        let answer = pane_probe(
            &d,
            "w-device",
            false,
            &[
                ("CADENCE_PROBE_KIND", "rpc".to_string()),
                ("CADENCE_PROBE_STATE", state.path().display().to_string()),
                ("CADENCE_PROBE_OUT", out_file.display().to_string()),
            ],
        );
        assert_eq!(answer["ok"], json!(false), "{answer}");
        assert!(
            answer["text"].as_str().unwrap().contains("agent"),
            "{answer}"
        );
        assert_eq!(
            stub.verifies.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "refused pane caller reached the issuer"
        );
        assert_eq!(device_session_count(state.path()), 0);
    }

    /// Same gate, from a `setsid` child of the pane: a new session is no
    /// escape — the /proc ancestry still names the pane.
    #[test]
    fn device_rpc_from_a_detached_child_is_refused() {
        let state = TempDir::new().unwrap();
        let (issuer, stub) = device_stub("approve");
        let d = device_daemon(state.path(), &issuer, &["op_9"]);
        let out_file = state.path().join("probe-out.json");
        let answer = pane_probe(
            &d,
            "w-device",
            true,
            &[
                ("CADENCE_PROBE_KIND", "rpc".to_string()),
                ("CADENCE_PROBE_STATE", state.path().display().to_string()),
                ("CADENCE_PROBE_OUT", out_file.display().to_string()),
            ],
        );
        assert_eq!(answer["ok"], json!(false), "{answer}");
        assert!(
            answer["text"].as_str().unwrap().contains("agent"),
            "{answer}"
        );
        assert_eq!(
            stub.verifies.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "refused detached caller reached the issuer"
        );
        assert_eq!(device_session_count(state.path()), 0);
    }

    /// The HTTP twin: a detached pane child polling spends nothing — the
    /// pending grant survives for the operator's own poll.
    #[test]
    fn device_poll_from_a_detached_child_is_refused_and_spares_the_grant() {
        let pm = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        seed(pm.path(), state.path());
        let d = UiDaemon::start_on(state.path().to_path_buf());
        let (issuer, _stub) = device_stub("approve");
        let (port, _board) = start_device_board(pm.path(), state.path(), issuer);
        let host = op::board_host(port);
        let (code, _, body) = device_post(port, &host, "/api/session/device/code", "{}");
        assert_eq!(code, 200, "{body}");
        let pending = serde_json::from_str::<Value>(&body).unwrap()["pending_id"]
            .as_str()
            .unwrap()
            .to_string();

        let out_file = state.path().join("probe-out.json");
        let answer = pane_probe(
            &d,
            "w-device",
            true,
            &[
                ("CADENCE_PROBE_KIND", "poll".to_string()),
                ("CADENCE_PROBE_PORT", port.to_string()),
                ("CADENCE_PROBE_HOST", host.clone()),
                ("CADENCE_PROBE_PENDING", pending.clone()),
                ("CADENCE_PROBE_OUT", out_file.display().to_string()),
            ],
        );
        assert_eq!(answer["status"], json!(403), "{answer}");
        assert!(
            answer["body"]
                .as_str()
                .unwrap()
                .contains("session_from_agent"),
            "{answer}"
        );
        assert!(
            !answer["headers"]
                .as_str()
                .unwrap()
                .to_ascii_lowercase()
                .contains("set-cookie"),
            "{answer}"
        );

        // The pending survived: the operator's own poll still approves.
        let mut minted = None;
        for _ in 0..3 {
            let (code, head, body) = device_post(
                port,
                &host,
                "/api/session/device/poll",
                &format!(r#"{{"pending_id":"{pending}"}}"#),
            );
            assert_eq!(code, 200, "{body}");
            if serde_json::from_str::<Value>(&body).unwrap()["session_key"]
                .as_str()
                .is_some()
            {
                assert!(head.to_ascii_lowercase().contains("set-cookie"), "{head}");
                minted = Some(());
                break;
            }
        }
        assert!(minted.is_some(), "operator poll never minted");
    }

    /// The pending `remove` under the lock is what makes one approved id
    /// mint at most once: 8 racing polls see exactly one session.
    #[test]
    fn device_poll_mints_exactly_once_under_concurrent_polls() {
        let pm = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        seed(pm.path(), state.path());
        let _d = UiDaemon::start_on(state.path().to_path_buf());
        let (issuer, _stub) = device_stub("approve");
        let (port, _board) = start_device_board(pm.path(), state.path(), issuer);
        let host = op::board_host(port);
        let (code, _, body) = device_post(port, &host, "/api/session/device/code", "{}");
        assert_eq!(code, 200, "{body}");
        let pending = serde_json::from_str::<Value>(&body).unwrap()["pending_id"]
            .as_str()
            .unwrap()
            .to_string();
        // Past `pending`: the next exchange approves.
        let (code, _, body) = device_post(
            port,
            &host,
            "/api/session/device/poll",
            &format!(r#"{{"pending_id":"{pending}"}}"#),
        );
        assert_eq!(status_of(&body), "pending", "{code} {body}");

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut joins = Vec::new();
        for _ in 0..8 {
            let barrier = barrier.clone();
            let host = host.clone();
            let pending = pending.clone();
            joins.push(thread::spawn(move || {
                barrier.wait();
                device_post(
                    port,
                    &host,
                    "/api/session/device/poll",
                    &format!(r#"{{"pending_id":"{pending}"}}"#),
                )
            }));
        }
        let mut minted = 0;
        let mut expired = 0;
        for join in joins {
            let (code, head, body) = join.join().unwrap();
            assert_eq!(code, 200, "{body}");
            if serde_json::from_str::<Value>(&body).unwrap()["session_key"]
                .as_str()
                .is_some()
            {
                minted += 1;
                assert!(head.to_ascii_lowercase().contains("set-cookie"), "{head}");
            } else {
                assert_eq!(status_of(&body), "expired", "{body}");
                assert!(!head.to_ascii_lowercase().contains("set-cookie"), "{head}");
                expired += 1;
            }
        }
        assert_eq!((minted, expired), (1, 7), "one pending id mints once");
        assert_eq!(device_session_count(state.path()), 1);
    }

    /// Forged identity fields buy nothing: the minted session's user is
    /// the issuer-verified subject, never a request field.
    #[test]
    fn device_rpc_mints_the_issuers_subject_not_forged_fields() {
        let state = TempDir::new().unwrap();
        let (issuer, _stub) = device_stub("approve");
        let d = device_daemon(state.path(), &issuer, &["op_9"]);
        let opened = d
            .rpc_opt(
                "operator_session_open_device",
                json!({
                    "token": "agc_t",
                    "origin": "loopback",
                    "sub": "attacker_1",
                    "subject": "attacker_1",
                    "role": "owner",
                    "org": "ws_other",
                    "issuer": "https://attacker.example"
                }),
            )
            .unwrap();
        assert_eq!(opened["session"]["user"]["sub"], json!("op_9"));
        assert_eq!(opened["session"]["user"]["role"], json!("operator"));
    }

    /// The allowlist is the gate: a verified subject off it is refused at
    /// the RPC (`device_subject_not_allowed`, no session, a loud event)
    /// and at the HTTP poll (403, no cookie).
    #[test]
    fn device_subject_off_the_allowlist_is_refused() {
        let pm = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        seed(pm.path(), state.path());
        let d = UiDaemon::start_on(state.path().to_path_buf());
        let (issuer, _stub) = device_stub("approve");
        // The pin the board writes at serve carries `op_1` — the stub's
        // approved subject `op_9` is verified but not allowed.
        let (port, _board) = start_device_board_for(
            pm.path(),
            state.path(),
            issuer.clone(),
            vec!["op_1".to_string()],
        );
        let host = op::board_host(port);

        let err = d
            .rpc_opt(
                "operator_session_open_device",
                json!({"token": "agc_t", "origin": "loopback"}),
            )
            .unwrap_err();
        assert_eq!(
            err.code(),
            Some("device_subject_not_allowed"),
            "allowlist refusal misreported: {err}"
        );
        assert!(err.to_string().contains("op_9"), "{err}");
        let refused = device_daemon_events(state.path(), "operator_device_session_refused");
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(refused[0]["subject"], json!("op_9"));
        assert_eq!(device_session_count(state.path()), 0);

        // The HTTP path maps the same refusal to 403 — no cookie.
        let (code, _, body) = device_post(port, &host, "/api/session/device/code", "{}");
        assert_eq!(code, 200, "{body}");
        let pending = serde_json::from_str::<Value>(&body).unwrap()["pending_id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut refused_http = None;
        for _ in 0..3 {
            let (code, head, body) = device_post(
                port,
                &host,
                "/api/session/device/poll",
                &format!(r#"{{"pending_id":"{pending}"}}"#),
            );
            if code == 403 {
                refused_http = Some((head, body));
                break;
            }
            assert_eq!(code, 200, "{body}");
        }
        let (head, body) = refused_http.expect("poll never refused");
        assert!(body.contains("device_subject_not_allowed"), "{body}");
        assert!(body.contains("op_9"), "{body}");
        assert!(!head.to_ascii_lowercase().contains("set-cookie"), "{head}");
        assert_eq!(device_session_count(state.path()), 0);
    }

    /// `/api/session/device/code` is exempt from the board-wide write
    /// lock: a slow issuer (3 s on the stub's code answer) must not stall
    /// an unrelated write. `/api/session/logout` posts through WRITE_LOCK,
    /// so it is the probe — it must answer well before the issuer does.
    #[test]
    fn device_code_does_not_stall_unrelated_writes() {
        let pm = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        seed(pm.path(), state.path());
        let _d = UiDaemon::start_on(state.path().to_path_buf());
        let (issuer, _stub) = device_stub("slow_code");
        let (port, _board) = start_device_board(pm.path(), state.path(), issuer);
        let host = op::board_host(port);

        let code_thread = {
            let host = host.clone();
            thread::spawn(move || device_post(port, &host, "/api/session/device/code", "{}"))
        };
        // Let the /code request reach the issuer and start its sleep.
        thread::sleep(Duration::from_millis(300));
        let began = std::time::Instant::now();
        let (code, _, _body) = device_post(port, &host, "/api/session/logout", "{}");
        let elapsed = began.elapsed();
        assert!(
            elapsed < Duration::from_millis(1500),
            "unrelated write stalled behind the issuer call: {elapsed:?}"
        );
        // The logout answer is a refusal or a no-op — either way it did
        // not wait for the issuer.
        assert!(code == 204 || code / 100 == 4, "{code}");
        let (code, _, body) = code_thread.join().unwrap();
        assert_eq!(code, 200, "{body}");
    }

    /// `/api/meta` advertises device sign-in on a configured board's
    /// loopback surface only — false when unconfigured.
    #[test]
    fn device_meta_advertises_sign_in_only_when_configured() {
        let pm = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        seed(pm.path(), state.path());
        let _d = UiDaemon::start_on(state.path().to_path_buf());
        let (issuer, _stub) = device_stub("approve");
        let (port, _board) = start_device_board(pm.path(), state.path(), issuer);
        let host = op::board_host(port);
        let (code, body) = http(port, "GET", "/api/meta", &host);
        assert_eq!(code, 200, "{body}");
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["device_login"],
            json!(true),
            "{body}"
        );

        let pm2 = TempDir::new().unwrap();
        let state2 = TempDir::new().unwrap();
        seed(pm2.path(), state2.path());
        let _d2 = UiDaemon::start_on(state2.path().to_path_buf());
        let (port2, _board2) = start_ui(pm2.path().to_path_buf(), state2.path().to_path_buf());
        let host2 = op::board_host(port2);
        let (code, body) = http(port2, "GET", "/api/meta", &host2);
        assert_eq!(code, 200, "{body}");
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["device_login"],
            json!(false),
            "{body}"
        );
    }

    /// A second foreground `ui run` against the same state dir must not
    /// touch the daemon's device trust pin while a board is live — the
    /// serving board holds `device-login.lock` for its lifetime, so a
    /// run on ANY free port can neither re-point nor clear the pin, and
    /// a run that loses the bind touches nothing (reviews r3/r4).
    #[test]
    fn device_pin_survives_a_second_ui_run_while_a_board_is_live() {
        let pm = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        seed(pm.path(), state.path());
        let _d = UiDaemon::start_on(state.path().to_path_buf());
        let (issuer, _stub) = device_stub("approve");
        let pin = state.path().join("operator").join("device-login.json");

        // Board one: a real `ui run` on a thread — no ui.pid anywhere,
        // exactly the foreground case the pid-file guard could not see.
        let port1 = free_port();
        let first_state = state.path().to_path_buf();
        let first_flags = ui::UiFlags {
            port: Some(port1),
            device_login_issuer: Some(issuer.clone()),
            device_login_org: Some("ws_company".into()),
            device_login_subject: vec!["op_9".into()],
            ..Default::default()
        };
        thread::spawn(move || {
            let _ = ui::run_cli(&first_state, &ui::UiAction::Run { flags: first_flags });
        });
        let host = op::board_host(port1);
        // Bound port = serving: probe the socket, never a request that
        // could refuse mid-start.
        for _ in 0..100 {
            if TcpStream::connect(("127.0.0.1", port1)).is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(http(port1, "GET", "/api/health", &host).0, 200);
        let before = std::fs::read(&pin).unwrap();

        // A run's answer: Err on a refusal; a run that starts SERVING
        // never answers — detected as a timeout, which is failure either
        // way for the refuse cases.
        let run = |flags: ui::UiFlags| -> bool {
            let state = state.path().to_path_buf();
            let (tx, rx) = std::sync::mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(ui::run_cli(&state, &ui::UiAction::Run { flags }));
            });
            matches!(rx.recv_timeout(Duration::from_secs(10)), Ok(Err(_)))
        };

        // (i) re-pointing flags on a FREE port: refused by the held lock.
        let errored = run(ui::UiFlags {
            port: Some(free_port()),
            device_login_issuer: Some("http://127.0.0.1:1".into()),
            device_login_org: Some("ws_company".into()),
            device_login_subject: vec!["op_evil".into()],
            ..Default::default()
        });
        assert!(
            errored,
            "a second `ui run` with device login must refuse while a board owns the pin"
        );
        assert_eq!(
            std::fs::read(&pin).unwrap(),
            before,
            "re-pointing run rewrote the pin"
        );

        // The live board still mints: its poll flow is untouched.
        let (code, _, body) = device_post(port1, &host, "/api/session/device/code", "{}");
        assert_eq!(code, 200, "{body}");
        let pending_id = serde_json::from_str::<Value>(&body).unwrap()["pending_id"]
            .as_str()
            .unwrap()
            .to_string();
        let (code, _, body) = device_post(
            port1,
            &host,
            "/api/session/device/poll",
            &format!(r#"{{"pending_id": "{pending_id}"}}"#),
        );
        assert_eq!(code, 200, "{body}");
        // pending then approve — second poll mints.
        let mut minted = serde_json::from_str::<Value>(&body).unwrap()["session_key"].is_string();
        if !minted {
            let (code, _, body) = device_post(
                port1,
                &host,
                "/api/session/device/poll",
                &format!(r#"{{"pending_id": "{pending_id}"}}"#),
            );
            assert_eq!(code, 200, "{body}");
            minted = serde_json::from_str::<Value>(&body).unwrap()["session_key"].is_string();
        }
        assert!(minted, "the first board's poll must still mint");

        // (ii) no device flags: a second board on another state dir-free
        // port is allowed to serve — it just may not clear the pin.
        let port2 = free_port();
        let second_state = state.path().to_path_buf();
        let second = thread::spawn(move || {
            let _ = ui::run_cli(
                &second_state,
                &ui::UiAction::Run {
                    flags: ui::UiFlags {
                        port: Some(port2),
                        ..Default::default()
                    },
                },
            );
        });
        let host2 = op::board_host(port2);
        let mut up = false;
        for _ in 0..100 {
            if TcpStream::connect(("127.0.0.1", port2)).is_ok() {
                up = true;
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(up, "an unconfigured second board may serve");
        assert_eq!(
            std::fs::read(&pin).unwrap(),
            before,
            "an unconfigured board cleared the pin"
        );
        // Its device routes stay dead while it holds no config.
        let (code, _, body) = device_post(port2, &host2, "/api/session/device/code", "{}");
        assert_eq!(code, 404, "{body}");
        drop(second); // a serve thread outlives the test, like every fixture board

        // (iii) the bind-loss case: a re-pointing run on board one's OWN
        // port fails the bind and touches nothing.
        let errored = run(ui::UiFlags {
            port: Some(port1),
            device_login_issuer: Some("http://127.0.0.1:1".into()),
            device_login_org: Some("ws_company".into()),
            device_login_subject: vec!["op_evil".into()],
            ..Default::default()
        });
        assert!(errored, "a run that loses the bind must fail");
        assert_eq!(
            std::fs::read(&pin).unwrap(),
            before,
            "a lost bind rewrote the pin"
        );
    }
}
