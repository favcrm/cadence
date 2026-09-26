//! CAD-608: a member session never drives the lane. The test seam has
//! no member assertion, so this is the public-board JWT path.
#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

use cadence_agent::ui;
use serde_json::json;
use serde_json::Value;
use std::path::Path;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn jwks_stub(body: String) -> u16 {
    let port = free_port();
    thread::spawn(move || {
        let server = tiny_http::Server::http(format!("127.0.0.1:{port}")).unwrap();
        loop {
            match server.recv_timeout(Duration::from_millis(200)) {
                Ok(Some(req)) => {
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
    port
}

fn board_signer(seed: u8) -> ring::signature::Ed25519KeyPair {
    ring::signature::Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap()
}

fn board_pubkey(signer: &ring::signature::Ed25519KeyPair) -> String {
    use base64::Engine;
    use ring::signature::KeyPair;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signer.public_key().as_ref())
}

fn board_assertion(signer: &ring::signature::Ed25519KeyPair, claims: &Value) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let h = b64
        .encode(serde_json::to_vec(&json!({"alg": "EdDSA", "typ": "JWT", "kid": "k1"})).unwrap());
    let p = b64.encode(serde_json::to_vec(claims).unwrap());
    let signed = format!("{h}.{p}");
    format!(
        "{signed}.{}",
        b64.encode(signer.sign(signed.as_bytes()).as_ref())
    )
}

fn board_claims(issuer: &str, aud: &str) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    json!({
        "iss": issuer,
        "aud": aud,
        "sub": "usr_member",
        "email": "member@example.com",
        "name": "Member",
        "company": "co_1",
        "role": "member",
        "iat": now - 5,
        "exp": now + 30,
        "jti": "jti-lane-member",
    })
}

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

/// Every lane write refuses a member, including when the issue is absent
/// — admit runs before the handler.
#[test]
fn a_member_cannot_drive_the_lane() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let signer = board_signer(0x61);
    let jwks = format!(
        r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","kid":"k1","x":"{}"}}]}}"#,
        board_pubkey(&signer)
    );
    let jwks_port = jwks_stub(jwks);
    let issuer = format!("http://127.0.0.1:{jwks_port}");
    let _daemon = UiDaemon::start_on(state.path().to_path_buf());
    let (port, host, _board) = start_public_board(pm.path(), state.path(), issuer.clone());
    let assertion = board_assertion(&signer, &board_claims(&issuer, &host));
    let (code, headers, body) = http_write(
        port,
        "POST",
        "/__platform/session",
        &host,
        &[
            "Content-Type: application/json",
            "Sec-Fetch-Site: same-origin",
            &format!("Origin: http://{host}"),
        ],
        json!({"assertion": assertion}).to_string().as_bytes(),
    );
    assert_eq!(code, 200, "{body}");
    let cookie_line = headers
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .unwrap();
    let token = cookie_line
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1;
    let cookie = format!("Cookie: __Host-aos-board-session={token}");
    for (path, payload) in [
        ("/api/issues/CAD-1/lane/ask", r#"{"text":"status"}"#),
        ("/api/issues/CAD-1/lane/instruct", r#"{"text":"do it"}"#),
        ("/api/issues/CAD-1/lane/interrupt", "{}"),
        ("/api/issues/CAD-1/lane/stop", "{}"),
        (
            "/api/issues/CAD-1/lane/unfence",
            r#"{"status":"interrupted"}"#,
        ),
        ("/api/issues/CAD-1/lane/reassign", r#"{"provider":"fake"}"#),
        (
            "/api/issues/NO-SUCH/lane/reassign",
            r#"{"provider":"fake"}"#,
        ),
    ] {
        let (code, _, body) = http_write(
            port,
            "POST",
            path,
            &host,
            &[
                "Content-Type: application/json",
                "X-Cadence-Board: 1",
                "Sec-Fetch-Site: same-origin",
                &format!("Origin: http://{host}"),
                &cookie,
            ],
            payload.as_bytes(),
        );
        assert_eq!(code, 403, "{path}: {body}");
        assert!(body.contains("member_role"), "{path}: {body}");
    }
}
