//! Signed HTTP coverage for `POST /api/cli/project_policy`.
//!
//! A loopback JWKS and a real Ed25519 envelope go through `post`, the same
//! verifier the public board uses. Actor tails stay inside the grammar
//! `verify` already requires (`[A-Za-z0-9_-]{1,200}`). This fixture does not
//! stand in for the manager's daemon plus public-board smoke.

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
    (3310..=3399)
        .find_map(|port| Server::http(("127.0.0.1", port)).ok())
        .expect("no free isolated fixture port in 3310..3399")
}

fn assertion(
    key: &Ed25519KeyPair,
    issuer: &str,
    actor: &str,
    principal_kind: &str,
    role: &str,
    jti: &str,
) -> String {
    let now = crate::issue::time::now_epoch();
    let header = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "alg": "EdDSA", "typ": "agenticos-wiki-actor/1", "kid": "aos274-fixture"
        }))
        .unwrap(),
    );
    let sub = match actor {
        "operator" => "owner",
        _ => actor.rsplit('/').next().unwrap_or(actor),
    };
    let claims = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "iss": issuer,
            "aud": "http://aos274.board.localhost",
            "sub": sub,
            "actor": actor,
            "principal_kind": principal_kind,
            "role_at_issue": role,
            "organization_id": "aos274-fixture",
            "scope": ["cli.read", "cli.write"],
            "credential_id": format!("fixture-{sub}"),
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
    write!(
        socket,
        "POST /api/cli/project_policy HTTP/1.1\r\nHost: aos274.board.localhost\r\nAuthorization: Bearer wikienv_{envelope}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
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
            .map(|entry| entry.unwrap().path())
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
fn signed_project_policy_admits_a_human_owner_and_refuses_the_rest() {
    let root = tempfile::Builder::new()
        .prefix("aos274-")
        .tempdir_in("/tmp")
        .unwrap();
    let pm = Pm::init(&root.path().join("pm")).unwrap();
    issue_write::project_add(&pm, "cadence", "CAD", &[], &[], &[], None).unwrap();
    let project_dir = pm.dir.join("cadence");
    let initial_version = crate::issue::project::load(&project_dir.join("project.yaml"))
        .unwrap()
        .policy
        .unwrap()
        .version;
    let state = root.path().join("state");
    std::fs::create_dir_all(&state).unwrap();

    let key = Ed25519KeyPair::from_seed_unchecked(&[174; 32]).unwrap();
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
                "kty": "OKP", "crv": "Ed25519", "kid": "aos274-fixture", "x": public_key
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
            host: "aos274.board.localhost".into(),
            issuer: issuer.clone(),
            company: "aos274-fixture".into(),
            authorize_url: format!("{issuer}/authorize"),
        }),
        ..ServeOpts::default()
    };
    let pm_dir = pm.dir.clone();
    let route_thread = thread::spawn(move || {
        for _ in 0..6 {
            let mut request = server
                .recv_timeout(Duration::from_secs(20))
                .unwrap()
                .expect("route not called");
            assert_eq!(request.url(), "/api/cli/project_policy");
            let method = request.method().clone();
            let response = post(
                &mut request,
                &method,
                "project_policy",
                &state,
                &pm_dir,
                &opts,
            );
            request.respond(response).unwrap();
        }
    });

    let owner = assertion(
        &key,
        &issuer,
        "users/owner",
        "user",
        "owner",
        "aos274-owner",
    );
    let member = assertion(
        &key,
        &issuer,
        "users/member",
        "user",
        "member",
        "aos274-member",
    );
    let owner_agent = assertion(
        &key,
        &issuer,
        "agents/lead",
        "agent",
        "owner",
        "aos274-agent",
    );

    let before = tree_bytes(&project_dir);
    let grant = json!({
        "project": "cadence",
        "visibility": "workspace-visible",
        "grants": ["users/mia", "operator"],
    });
    let (status, body) = call(port, &member, &grant);
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["code"], "not_permitted");
    assert_eq!(tree_bytes(&project_dir), before);

    let (status, body) = call(port, &owner_agent, &grant);
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["code"], "not_permitted");
    assert_eq!(tree_bytes(&project_dir), before);

    let owner_private = assertion(
        &key,
        &issuer,
        "users/owner",
        "user",
        "owner",
        "aos274-private",
    );
    let (status, body) = call(
        port,
        &owner_private,
        &json!({"project": "cadence", "visibility": "private", "grants": []}),
    );
    assert_eq!(status, 400, "{body}");
    assert_eq!(tree_bytes(&project_dir), before);

    let owner_bad = assertion(
        &key,
        &issuer,
        "users/owner",
        "user",
        "owner",
        "aos274-bad-key",
    );
    let (status, body) = call(
        port,
        &owner_bad,
        &json!({"project": "../aos274-escape", "visibility": "workspace-visible", "grants": ["users/mia"]}),
    );
    assert_eq!(status, 400, "{body}");
    assert!(!root
        .path()
        .join("pm")
        .join("..")
        .join("aos274-escape")
        .join("project.yaml")
        .exists());
    assert_eq!(tree_bytes(&project_dir), before);

    let (status, body) = call(port, &owner, &grant);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["project"], "cadence");
    assert_eq!(body["committed"], true);
    let first_version = body["policy"]["version"].as_u64().unwrap();
    assert_eq!(first_version, initial_version + 1);
    assert_eq!(body["policy"]["visibility"], "workspace_visible");
    assert_eq!(body["policy"]["members"], json!(["users/mia", "operator"]));
    assert!(body.get("issue").is_none());
    assert!(body.get("card").is_none());

    let owner_replace = assertion(&key, &issuer, "operator", "user", "owner", "aos274-replace");
    let (status, body) = call(
        port,
        &owner_replace,
        &json!({"project": "cadence", "visibility": "workspace-visible", "grants": ["operator"]}),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["policy"]["version"], first_version + 1);
    assert_eq!(
        body["policy"]["members"],
        json!(["operator"]),
        "grants replace the recorded list"
    );
    let journal = crate::issue::git(&pm.dir, &["log", "-1", "--format=%B"]).unwrap();
    assert!(
        journal.lines().any(|line| line == "Actor: users/owner"),
        "the policy journal did not identify the authenticated Owner: {journal}"
    );

    jwks_thread.join().unwrap();
    route_thread.join().unwrap();
}
