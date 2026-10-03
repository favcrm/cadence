//! CAD-1019 slice 3: `POST /api/cli/<verb>` — the container end of the
//! remote CLI, adversarial tests (contract `docs/design/remote-cli.md`,
//! AgenticOS AOS-128's `checkCliCall` + `hostedCadenceCliActorClaimsSchema`).
//!
//! Every guard gets its failure here, each red without the guard: a
//! forged signature, another workspace's envelope, a sibling board's
//! `aud`, an expired envelope, a `cli.read`-only envelope calling a
//! write verb, an unlisted and an operator-only verb, a replay after
//! `exp`, a cookie-only request, an agent-shaped bearer — plus the
//! happy read and write paths. The JWKS and keypairs are local stubs;
//! no live host is touched.
//!
//! The "proves" check for each refusal row is `—` on the response
//! body: a refused call writes nothing.

#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

use cadence_agent::ui;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

// ---------- cli actor envelope fixtures (the AOS-128 wire shape) ----------

/// The platform's JWKS endpoint, stubbed: answers
/// `/.well-known/agenticos-board-jwks.json` only.
fn jwks_stub(body: String) -> u16 {
    let port = free_port();
    thread::spawn(move || {
        let server = tiny_http::Server::http(format!("127.0.0.1:{port}")).unwrap();
        loop {
            match server.recv_timeout(Duration::from_millis(100)) {
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

fn signer(seed: u8) -> ring::signature::Ed25519KeyPair {
    ring::signature::Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap()
}

fn pubkey(signer: &ring::signature::Ed25519KeyPair) -> String {
    use base64::Engine;
    use ring::signature::KeyPair;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signer.public_key().as_ref())
}

/// Sign `header.payload` into the compact JWS the issuer mints — the
/// cli family shares the wiki mint's `agenticos-wiki-actor/1` typ.
fn envelope(signer: &ring::signature::Ed25519KeyPair, kid: &str, claims: &Value) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let h = b64.encode(
        serde_json::to_vec(&json!({"alg": "EdDSA", "typ": "agenticos-wiki-actor/1", "kid": kid}))
            .unwrap(),
    );
    let p = b64.encode(serde_json::to_vec(claims).unwrap());
    let signed = format!("{h}.{p}");
    format!(
        "{signed}.{}",
        b64.encode(signer.sign(signed.as_bytes()).as_ref())
    )
}

/// `hostedCadenceCliActorClaimsSchema` — the closed claim set. `aud` is
/// the public ORIGIN (`http://<host>` for a `*.localhost` dev board,
/// `https` on cadencecloud); `actor` is the issuer-derived handle.
fn cli_claims(issuer: &str, host: &str, scope: &[&str]) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    // Every minted envelope carries its own jti — the mint makes each
    // id distinct (uuid-ish), and the route consumes it on first use,
    // so the fixture can never lean on a shared one.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    json!({
        "iss": issuer,
        "aud": format!("http://{host}"),
        "sub": "usr_9",
        "actor": "users/usr_9",
        "principal_kind": "user",
        "role_at_issue": "owner",
        "organization_id": "co_1",
        "scope": scope,
        "credential_id": "hct_cli1",
        "iat": now - 5,
        "exp": now + 120,
        "jti": format!("jti-{now}-{seq}"),
    })
}

/// A board configured for the public name, like `start_public_board`.
fn start_cli_board(pm: &Path, state: &Path, issuer: String) -> (u16, String, BoardStop) {
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

/// `POST /api/cli/<verb>` with the envelope bearer — the exact forward
/// the worker issues (`content-type: application/json`, `authorization:
/// Bearer wikienv_<env>`, the arguments object as body). It sends NO
/// `X-Cadence-Board`, no `Origin`, no cookie — the route's own guards
/// are the whole boundary.
fn cli_call(
    port: u16,
    host: &str,
    verb: &str,
    bearer: Option<&str>,
    args: &Value,
) -> (u16, String) {
    let mut headers = vec!["Content-Type: application/json".to_string()];
    if let Some(b) = bearer {
        headers.push(format!("Authorization: Bearer {b}"));
    }
    let (code, _, body) = http_write(
        port,
        "POST",
        &format!("/api/cli/{verb}"),
        host,
        &headers.iter().map(String::as_str).collect::<Vec<_>>(),
        &args.to_string().into_bytes(),
    );
    (code, body)
}

fn cli_envelope(signer: &ring::signature::Ed25519KeyPair, claims: &Value) -> String {
    format!("wikienv_{}", envelope(signer, "k1", claims))
}

// ---------- the adversarial table ----------

/// The happy path first: a verified envelope runs a read and a write.
#[test]
fn cli_read_and_write_happy_paths() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let key = signer(0x9d);
    let jwks = format!(
        r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","kid":"k1","x":"{}"}}]}}"#,
        pubkey(&key)
    );
    let jwks_port = jwks_stub(jwks);
    let issuer = format!("http://127.0.0.1:{jwks_port}");
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, host, _board) = start_cli_board(pm.path(), state.path(), issuer.clone());

    // cli.read: issue_ls lists the seeded tracker. Each call carries
    // its own envelope — the contract's envelope is single-use, so the
    // worker mints per call and the fixture does the same.
    let env = cli_envelope(&key, &cli_claims(&issuer, &host, &["cli.read"]));
    let (code, body) = cli_call(port, &host, "issue_ls", Some(&env), &json!({}));
    assert_eq!(code, 200, "{body}");
    let out: Value = serde_json::from_str(&body).unwrap();
    assert!(out["issues"].as_array().unwrap().len() >= 3, "{out}");

    // issue_show / issue_history read the seeded issue — each on a
    // freshly minted envelope (single-use).
    let env = cli_envelope(&key, &cli_claims(&issuer, &host, &["cli.read"]));
    let (code, body) = cli_call(
        port,
        &host,
        "issue_show",
        Some(&env),
        &json!({"id": "CAD-1"}),
    );
    assert_eq!(code, 200, "{body}");
    let env = cli_envelope(&key, &cli_claims(&issuer, &host, &["cli.read"]));
    let (code, body) = cli_call(
        port,
        &host,
        "issue_history",
        Some(&env),
        &json!({"id": "CAD-1", "limit": 5}),
    );
    assert_eq!(code, 200, "{body}");

    // cli.write: a comment lands under the envelope's derived handle —
    // never "operator".
    let envw = cli_envelope(&key, &cli_claims(&issuer, &host, &["cli.write"]));
    let (code, body) = cli_call(
        port,
        &host,
        "issue_comment",
        Some(&envw),
        &json!({"id": "CAD-1", "body": "remote note"}),
    );
    assert_eq!(code, 200, "{body}");
    let out: Value = serde_json::from_str(&body).unwrap();
    let author = out["issue"]["comments"]
        .as_array()
        .and_then(|c| c.last())
        .map(|c| c["author"].as_str().unwrap_or_default());
    assert_eq!(author, Some("usr_9"), "{out}");
    // The write is real: the tracker carries the comment file.
    let dir = pm.path().join("cadence/CAD-1/comments");
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        names.iter().any(|n| n.contains("usr_9")),
        "no comment authored by the cli actor: {names:?}"
    );
    // And the git trailer names the derived actor, not "operator".
    let log = git(pm.path(), &["log", "-1", "--format=%B"]).1;
    assert!(log.contains("usr_9"), "{log}");
    assert!(!log.contains("Actor: operator"), "{log}");
}

/// Every credential failure closes the route before a byte of work.
#[test]
fn cli_refusals_fail_closed() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let key = signer(0x9d);
    let jwks = format!(
        r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","kid":"k1","x":"{}"}}]}}"#,
        pubkey(&key)
    );
    let jwks_port = jwks_stub(jwks);
    let issuer = format!("http://127.0.0.1:{jwks_port}");
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, host, _board) = start_cli_board(pm.path(), state.path(), issuer.clone());
    let env = cli_envelope(
        &key,
        &cli_claims(&issuer, &host, &["cli.read", "cli.write"]),
    );
    let before = std::fs::read_dir(pm.path().join("cadence/CAD-1/comments"))
        .map(|d| d.count())
        .unwrap_or(0);

    // No bearer at all.
    let (code, body) = cli_call(port, &host, "issue_ls", None, &json!({}));
    assert_eq!(code, 401, "{body}");
    assert!(body.contains("unauthorized"), "{body}");
    // A bearer that is not the envelope prefix.
    let (code, body) = cli_call(port, &host, "issue_ls", Some("hct_forged"), &json!({}));
    assert_eq!(code, 401, "{body}");
    // A valid prefix with a forged signature (a key the JWKS does not
    // publish signed this envelope).
    let forged = cli_envelope(&signer(0x5e), &cli_claims(&issuer, &host, &["cli.read"]));
    let (code, body) = cli_call(port, &host, "issue_ls", Some(&forged), &json!({}));
    assert_eq!(code, 401, "{body}");
    assert!(
        body.contains("assertion_invalid") || body.contains("signature_invalid"),
        "{body}"
    );
    // Another workspace's envelope.
    let mut foreign = cli_claims(&issuer, &host, &["cli.read"]);
    foreign["organization_id"] = json!("co_2");
    let env2 = cli_envelope(&key, &foreign);
    let (code, body) = cli_call(port, &host, "issue_ls", Some(&env2), &json!({}));
    assert_eq!(code, 401, "{body}");
    assert!(body.contains("not_a_member"), "{body}");
    // A sibling board's `aud`.
    let mut wrong_aud = cli_claims(&issuer, &host, &["cli.read"]);
    wrong_aud["aud"] = json!("http://other.board.localhost:9999");
    let env3 = cli_envelope(&key, &wrong_aud);
    let (code, body) = cli_call(port, &host, "issue_ls", Some(&env3), &json!({}));
    assert_eq!(code, 401, "{body}");
    assert!(body.contains("audience_mismatch"), "{body}");
    // An expired envelope (exp in the past, iat earlier still).
    let mut expired = cli_claims(&issuer, &host, &["cli.read"]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    expired["iat"] = json!(now - 400);
    expired["exp"] = json!(now - 200);
    let env4 = cli_envelope(&key, &expired);
    let (code, body) = cli_call(port, &host, "issue_ls", Some(&env4), &json!({}));
    assert_eq!(code, 401, "{body}");
    assert!(body.contains("assertion_expired"), "{body}");
    // A read-scoped envelope calling a write verb.
    let read_only = cli_envelope(&key, &cli_claims(&issuer, &host, &["cli.read"]));
    let (code, body) = cli_call(
        port,
        &host,
        "issue_comment",
        Some(&read_only),
        &json!({"id": "CAD-1", "body": "x"}),
    );
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("insufficient_scope"), "{body}");
    // An unlisted verb.
    let (code, body) = cli_call(port, &host, "issue_nuke", Some(&env), &json!({}));
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("cli_verb_refused"), "{body}");
    // An operator-only verb is never reachable — same refusal, before
    // the credential is even checked.
    let (code, body) = cli_call(port, &host, "shutdown", Some(&env), &json!({}));
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("cli_verb_refused"), "{body}");
    // A cookie-only request carries no bearer.
    let cookie = "Cookie: __Host-aos-board-session=fake";
    let (code, _, body) = http_write(
        port,
        "POST",
        "/api/cli/issue_ls",
        &host,
        &["Content-Type: application/json", cookie],
        b"{}",
    );
    assert_eq!(code, 401, "{body}");
    // An identity field in the arguments refuses whole — the same
    // forbidden-field rule the worker applies.
    let (code, body) = cli_call(
        port,
        &host,
        "issue_ls",
        Some(&env),
        &json!({"actor": "operator"}),
    );
    assert_eq!(code, 400, "{body}");
    // The body must be a JSON object.
    let (code, _, body) = http_write(
        port,
        "POST",
        "/api/cli/issue_ls",
        &host,
        &[
            "Content-Type: application/json",
            &format!("Authorization: Bearer {env}"),
        ],
        b"[1,2,3]",
    );
    assert_eq!(code, 400, "{body}");
    // Nothing was written through any of it.
    let after = std::fs::read_dir(pm.path().join("cadence/CAD-1/comments"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(before, after, "a refused cli call left a write behind");
}

/// The route exists only on this board's public host — a loopback
/// request carrying a perfectly valid envelope is still the write
/// path's OperatorOnly refusal, not the cli surface.
#[test]
fn cli_route_never_answers_off_the_public_host() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let key = signer(0x9d);
    let jwks = format!(
        r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","kid":"k1","x":"{}"}}]}}"#,
        pubkey(&key)
    );
    let jwks_port = jwks_stub(jwks);
    let issuer = format!("http://127.0.0.1:{jwks_port}");
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, host, _board) = start_cli_board(pm.path(), state.path(), issuer.clone());
    let loopback = format!("127.0.0.1:{port}");

    let env = cli_envelope(&key, &cli_claims(&issuer, &host, &["cli.read"]));
    // On the public host the same envelope reads.
    let (code, body) = cli_call(port, &host, "issue_ls", Some(&env), &json!({}));
    assert_eq!(code, 200, "{body}");
    // On loopback the path is just another unlisted write — refused by
    // the session gate, no bearer consulted.
    let (code, _, body) = http_write(
        port,
        "POST",
        "/api/cli/issue_ls",
        &loopback,
        &[
            "Content-Type: application/json",
            &format!("Authorization: Bearer {env}"),
        ],
        b"{}",
    );
    assert!(code == 403 || code == 401, "{code} {body}");
}

/// A forged or tampered envelope part is refused before dispatch.
#[test]
fn cli_envelope_tampering_and_replay() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let key = signer(0x9d);
    let jwks = format!(
        r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","kid":"k1","x":"{}"}}]}}"#,
        pubkey(&key)
    );
    let jwks_port = jwks_stub(jwks);
    let issuer = format!("http://127.0.0.1:{jwks_port}");
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, host, _board) = start_cli_board(pm.path(), state.path(), issuer.clone());

    // A well-formed envelope minted for a DIFFERENT workspace id under
    // the same issuer — cross-workspace replay.
    let mut other_org = cli_claims(&issuer, &host, &["cli.read"]);
    other_org["organization_id"] = json!("co_999");
    other_org["sub"] = json!("usr_foreign");
    let env = cli_envelope(&key, &other_org);
    let (code, body) = cli_call(port, &host, "issue_ls", Some(&env), &json!({}));
    assert_eq!(code, 401, "{body}");

    // A valid envelope whose signature block is swapped for another's.
    let good = envelope(&key, "k1", &cli_claims(&issuer, &host, &["cli.read"]));
    let foreign = envelope(
        &signer(0x77),
        "k1",
        &cli_claims(&issuer, &host, &["cli.read"]),
    );
    let mut parts = good.split('.').collect::<Vec<_>>();
    parts[2] = foreign.split('.').nth(2).unwrap();
    let swapped = format!("wikienv_{}", parts.join("."));
    let (code, body) = cli_call(port, &host, "issue_ls", Some(&swapped), &json!({}));
    assert_eq!(code, 401, "{body}");

    // A claim set with an extra field — the closed schema refuses what
    // it does not name (a second mint entry point stays contract-free).
    let mut extra = cli_claims(&issuer, &host, &["cli.read"]);
    extra["client_id"] = json!("x");
    let env = cli_envelope(&key, &extra);
    let (code, body) = cli_call(port, &host, "issue_ls", Some(&env), &json!({}));
    assert_eq!(code, 401, "{body}");

    // The `operator` actor under a non-owner principal — the mint's own
    // invariant, re-verified here.
    let mut cheat = cli_claims(&issuer, &host, &["cli.read", "cli.write"]);
    cheat["actor"] = json!("operator");
    cheat["principal_kind"] = json!("user");
    cheat["role_at_issue"] = json!("member");
    let env = cli_envelope(&key, &cheat);
    let (code, body) = cli_call(port, &host, "issue_ls", Some(&env), &json!({}));
    assert_eq!(code, 401, "{body}");

    // Replay after exp: mint with a life so short it is already spent.
    let mut stale = cli_claims(&issuer, &host, &["cli.read"]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    // `exp` in the past fails closed (the worker refuses `exp <= now`
    // with no trailing skew — the container check is no looser).
    stale["iat"] = json!(now - 290);
    stale["exp"] = json!(now - 1);
    let env = cli_envelope(&key, &stale);
    let (code, body) = cli_call(port, &host, "issue_ls", Some(&env), &json!({}));
    assert_eq!(code, 401, "{body}");
    assert!(body.contains("assertion_expired"), "{body}");
}

/// The contract calls the envelope single-use — and a captured bearer
/// can be POSTed straight to this route, skipping the worker's
/// live-bearer rebind, so the container is itself the single-use
/// authority (spec review, PR #744): the second in-window use of one
/// `jti` refuses — a read AND a write — with no side effect, and the
/// refusal survives a board restart inside the ≤300 s window. Expired
/// entries prune.
#[test]
fn a_cli_envelope_is_single_use_and_the_set_survives_a_restart() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let key = signer(0x9d);
    let jwks = format!(
        r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","kid":"k1","x":"{}"}}]}}"#,
        pubkey(&key)
    );
    let jwks_port = jwks_stub(jwks);
    let issuer = format!("http://127.0.0.1:{jwks_port}");
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, host, board) = start_cli_board(pm.path(), state.path(), issuer.clone());

    // A read consumes its jti: the same envelope replayed refuses with
    // assertion_replayed — nothing is read twice either.
    let env = cli_envelope(&key, &cli_claims(&issuer, &host, &["cli.read"]));
    let (code, body) = cli_call(port, &host, "issue_ls", Some(&env), &json!({}));
    assert_eq!(code, 200, "{body}");
    let (code, body) = cli_call(port, &host, "issue_ls", Some(&env), &json!({}));
    assert_eq!(code, 401, "{body}");
    assert!(body.contains("assertion_replayed"), "{body}");

    // A write the same way — and its refusal writes nothing.
    let envw = cli_envelope(&key, &cli_claims(&issuer, &host, &["cli.write"]));
    let (code, body) = cli_call(
        port,
        &host,
        "issue_comment",
        Some(&envw),
        &json!({"id": "CAD-1", "body": "once"}),
    );
    assert_eq!(code, 200, "{body}");
    let (code, body) = cli_call(
        port,
        &host,
        "issue_comment",
        Some(&envw),
        &json!({"id": "CAD-1", "body": "replay"}),
    );
    assert_eq!(code, 401, "{body}");
    assert!(body.contains("assertion_replayed"), "{body}");
    let (_code, body) = {
        let env = cli_envelope(&key, &cli_claims(&issuer, &host, &["cli.read"]));
        cli_call(
            port,
            &host,
            "issue_show",
            Some(&env),
            &json!({"id": "CAD-1"}),
        )
    };
    let out: Value = serde_json::from_str(&body).unwrap();
    let comments = out["issue"]["comments"].as_array().unwrap().len();
    assert_eq!(comments, 1, "the replayed write wrote anyway: {out}");

    // A different jti on an otherwise identical envelope passes — the
    // set is per-id, not per-principal.
    let env = cli_envelope(&key, &cli_claims(&issuer, &host, &["cli.read"]));
    let (code, body) = cli_call(port, &host, "issue_ls", Some(&env), &json!({}));
    assert_eq!(code, 200, "{body}");

    // And a forged replay can never burn the real jti: the consume
    // runs only after the signature and every pin verify.
    let mut claims = cli_claims(&issuer, &host, &["cli.read"]);
    claims["jti"] = json!("jti-victim");
    let stolen = cli_envelope(&signer(0x5e), &claims);
    let (code, _) = cli_call(port, &host, "issue_ls", Some(&stolen), &json!({}));
    assert_eq!(code, 401);
    let real = cli_envelope(&key, &claims);
    let (code, body) = cli_call(port, &host, "issue_ls", Some(&real), &json!({}));
    assert_eq!(code, 200, "forged replay burned the real jti: {body}");

    // A restart inside the window still refuses: the seen-set is
    // persisted under state_dir, not kept in memory.
    drop(board);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "the board did not stop within 10s"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let (port2, host2, _board2) = start_cli_board(pm.path(), state.path(), issuer.clone());
    // jti changes with the port only in `aud` — mint one envelope for
    // each listener; the SAME jti, replays across the restart.
    let mut claims = cli_claims(&issuer, &host2, &["cli.read"]);
    claims["jti"] = json!("jti-restart");
    let env2 = cli_envelope(&key, &claims);
    let (code, body) = cli_call(port2, &host2, "issue_ls", Some(&env2), &json!({}));
    assert_eq!(code, 200, "{body}");
    drop(_board2);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port2)).is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "the restarted board did not stop within 10s"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let (port3, host3, _board3) = start_cli_board(pm.path(), state.path(), issuer.clone());
    let mut claims = cli_claims(&issuer, &host3, &["cli.read"]);
    claims["jti"] = json!("jti-restart");
    let env3 = cli_envelope(&key, &claims);
    let (code, body) = cli_call(port3, &host3, "issue_ls", Some(&env3), &json!({}));
    assert_eq!(code, 401, "{body}");
    assert!(body.contains("assertion_replayed"), "{body}");

    // The persisted set prunes: mint an envelope already expired, let
    // its refuse leave the file, then re-mint the same jti live — the
    // expired row is gone so the fresh envelope still passes (the file
    // can never wedge a jti beyond its exp).
    let mut dead = cli_claims(&issuer, &host3, &["cli.read"]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    dead["jti"] = json!("jti-prune");
    dead["iat"] = json!(now - 200);
    dead["exp"] = json!(now - 100);
    let envd = cli_envelope(&key, &dead);
    let (code, _) = cli_call(port3, &host3, "issue_ls", Some(&envd), &json!({}));
    assert_eq!(code, 401);
    let set = state.path().join("cli-jtis.json");
    let text = std::fs::read_to_string(&set).unwrap_or_default();
    // Expired rows are pruned on consume — `jti-prune` was refused at
    // verify (exp<=now) before ever touching the set, so the file must
    // carry no entry that outlives its exp.
    let file: Value = serde_json::from_str(&text).unwrap_or(json!({"jtis":{}}));
    if let Some(map) = file["jtis"].as_object() {
        for (h, exp) in map {
            assert!(
                exp.as_i64().unwrap_or(i64::MAX) >= now,
                "an expired entry lingered in {set:?}: {h}"
            );
        }
    }
}
