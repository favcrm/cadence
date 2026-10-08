//! Independently authored by terminal-c9b4c-pmqa from CAD-1179 acceptance.
//! Exercise the real signed HTTP mutation guard and under-lock revision check.
//! The implementation writer must not edit or weaken this acceptance check.

use super::*;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use tiny_http::Server;

fn loopback_server() -> Server {
    (3110..=3199)
        .find_map(|port| Server::http(("127.0.0.1", port)).ok())
        .expect("no free isolated fixture port in 3110..3199")
}

fn assertion(key: &Ed25519KeyPair, issuer: &str, actor: &str, scope: &str, jti: &str) -> String {
    let now = crate::issue::time::now_epoch();
    let header = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "alg": "EdDSA", "typ": "agenticos-wiki-actor/1", "kid": "cad1179-fixture"
        }))
        .unwrap(),
    );
    let claims = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "iss": issuer,
            "aud": "http://cad1179.board.localhost",
            "sub": actor,
            "actor": format!("users/{actor}"),
            "principal_kind": "user",
            "role_at_issue": "member",
            "organization_id": "cad1179-fixture",
            "scope": [scope],
            "credential_id": format!("fixture-{actor}"),
            "iat": now,
            "exp": now + 120,
            "jti": jti
        }))
        .unwrap(),
    );
    let signed = format!("{header}.{claims}");
    format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).as_ref())
    )
}

fn call(port: u16, envelope: &str, args: &Value) -> (u16, Value) {
    let body = serde_json::to_vec(args).unwrap();
    let mut socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    write!(socket,
        "POST /api/cli/issue_set HTTP/1.1\r\nHost: cad1179.board.localhost\r\nAuthorization: Bearer wikienv_{envelope}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()).unwrap();
    socket.write_all(&body).unwrap();
    let mut response = Vec::new();
    socket.read_to_end(&mut response).unwrap();
    let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let header = std::str::from_utf8(&response[..split]).unwrap();
    let status = header
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let value = serde_json::from_slice(&response[split + 4..]).unwrap();
    (status, value)
}

fn tree_bytes(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn collect(root: &Path, at: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(at)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for entry in entries {
            if entry.is_dir() {
                collect(root, &entry, out);
            } else {
                out.push((
                    entry.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(&entry).unwrap(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    collect(root, root, &mut out);
    out
}

#[test]
fn cad1179_signed_ticket_writes_refuse_read_scope_stale_revision_and_replay_without_mutation() {
    let root = tempfile::Builder::new()
        .prefix("c1179-")
        .tempdir_in("/tmp")
        .unwrap();
    let pm = Pm::init(&root.path().join("pm")).unwrap();
    issue_write::project_add(&pm, "fixture", "TKT", &[], &[], &[], None).unwrap();
    let created = issue_write::new_issue(
        &pm,
        &pm.dir,
        Some("fixture"),
        "original",
        None,
        None,
        &[],
        None,
        None,
        &[],
        None,
        None,
        "fixture-author",
    )
    .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let issue_dir = board::find_issue(&pm.dir, &id).unwrap().dir;
    let original_rev = issue_write::issue_rev(&issue_dir).unwrap();
    let project_dir = pm.dir.join("fixture");
    let state = root.path().join("state");
    std::fs::create_dir_all(&state).unwrap();

    // A synthetic issuer key, used only for this isolated route fixture.
    let key = Ed25519KeyPair::from_seed_unchecked(&[117; 32]).unwrap();
    let jwks = loopback_server();
    let issuer = format!("http://{}", jwks.server_addr());
    let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let jwks_thread = thread::spawn(move || {
        let request = jwks
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
            .expect("JWKS not fetched");
        assert_eq!(request.url(), "/.well-known/agenticos-board-jwks.json");
        let response = Response::from_string(
            json!({"keys": [{
                "kty": "OKP", "crv": "Ed25519", "kid": "cad1179-fixture", "x": public_key
            }]})
            .to_string(),
        )
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
        request.respond(response).unwrap();
    });

    let server = loopback_server();
    let port = server.server_addr().to_ip().unwrap().port();
    let opts = ServeOpts {
        public: Some(crate::ui::PublicBoard {
            host: "cad1179.board.localhost".into(),
            issuer: issuer.clone(),
            company: "cad1179-fixture".into(),
            company_slug: None,
            authorize_url: format!("{issuer}/authorize"),
        }),
        ..ServeOpts::default()
    };
    let pm_dir = pm.dir.clone();
    let route_thread = thread::spawn(move || {
        for _ in 0..6 {
            let mut request = server
                .recv_timeout(Duration::from_secs(15))
                .unwrap()
                .expect("route not called");
            assert_eq!(request.url(), "/api/cli/issue_set");
            let method = request.method().clone();
            let response = post(&mut request, &method, "issue_set", &state, &pm_dir, &opts);
            request.respond(response).unwrap();
        }
    });

    let readonly = assertion(&key, &issuer, "reader", "cli.read", "c1179-read");
    let write_b = assertion(&key, &issuer, "writer-b", "cli.write", "c1179-write-b");
    let write_a = assertion(&key, &issuer, "writer-a", "cli.write", "c1179-write-a");
    let args_b = json!({"ids": [&id], "set": ["title=writer-b"], "if_rev": original_rev});

    let before = tree_bytes(&project_dir);
    let (status, body) = call(port, &readonly, &args_b);
    assert_eq!(status, 403, "read scope entered the mutation path: {body}");
    assert_eq!(body["error"]["code"], "insufficient_scope");
    assert_eq!(
        tree_bytes(&project_dir),
        before,
        "read-scoped call changed tracker bytes"
    );

    let (status, body) = call(port, &write_b, &args_b);
    assert_eq!(
        status, 200,
        "current-revision write was not confirmed: {body}"
    );
    let current_rev = issue_write::issue_rev(&issue_dir).unwrap();
    assert_ne!(current_rev, original_rev);
    assert!(std::fs::read_to_string(issue_dir.join("issue.md"))
        .unwrap()
        .contains("writer-b"));

    let before = tree_bytes(&project_dir);
    let forced = assertion(&key, &issuer, "writer-a", "cli.write", "c1179-force");
    let forced_args = json!({
        "ids": [&id], "set": ["status=done"], "if_rev": current_rev,
        "force": "caller-selected evidence override"
    });
    let (status, body) = call(port, &forced, &forced_args);
    assert_eq!(status, 400, "remote force override was not refused: {body}");
    assert_eq!(body["error"]["code"], "invalid_request");
    assert_eq!(
        tree_bytes(&project_dir),
        before,
        "force override changed tracker bytes"
    );

    let missing_rev = assertion(&key, &issuer, "writer-a", "cli.write", "c1179-missing-rev");
    let (status, body) = call(
        port,
        &missing_rev,
        &json!({
            "ids": [&id], "set": ["title=unconditional-writer-a"]
        }),
    );
    assert_eq!(
        status, 400,
        "missing revision entered mutation path: {body}"
    );
    assert_eq!(body["error"]["code"], "invalid_request");
    assert_eq!(
        tree_bytes(&project_dir),
        before,
        "missing revision changed tracker bytes"
    );

    let args_a = json!({"ids": [&id], "set": ["title=stale-writer-a"], "if_rev": original_rev});
    let (status, body) = call(port, &write_a, &args_a);
    assert_eq!(
        status, 409,
        "stale collaborator overwrote a newer ticket: {body}"
    );
    assert_eq!(body["current_rev"], current_rev);
    assert_eq!(
        tree_bytes(&project_dir),
        before,
        "stale revision changed tracker bytes"
    );

    let (status, body) = call(port, &write_b, &args_b);
    assert_eq!(status, 401, "a spent write assertion was accepted: {body}");
    assert_eq!(body["error"]["code"], "assertion_replayed");
    assert_eq!(
        tree_bytes(&project_dir),
        before,
        "replayed write changed tracker bytes"
    );

    route_thread.join().unwrap();
    jwks_thread.join().unwrap();
}
