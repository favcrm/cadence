//! Personal PM reads through the real signed CLI HTTP guard and tracker.
use super::*;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

#[test]
fn cad1500_owner_and_member_pm_work_is_self_scoped_and_changes_after_reassignment() {
    let root = tempfile::tempdir().unwrap();
    let pm = Pm::init(&root.path().join("pm")).unwrap();
    issue_write::project_add(&pm, "fixture", "PMT", &[], &[], &[], None).unwrap();
    let state = root.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let key = Ed25519KeyPair::from_seed_unchecked(&[15; 32]).unwrap();
    let public = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let jwks = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let issuer = format!("http://{}", jwks.server_addr());
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    let opts = ServeOpts {
        public: Some(crate::ui::PublicBoard {
            host: "cad1500.board.localhost".into(),
            issuer: issuer.clone(),
            company: "cad1500".into(),
            authorize_url: format!("{issuer}/authorize"),
        }),
        ..ServeOpts::default()
    };
    let stop = Arc::new(AtomicBool::new(false));
    let jwks_stop = stop.clone();
    let jwks_thread = std::thread::spawn(move || {
        while !jwks_stop.load(Ordering::SeqCst) {
            if let Some(request) = jwks.recv_timeout(Duration::from_millis(100)).unwrap() {
                request
                    .respond(Response::from_string(
                        json!({"keys": [{
                            "kty":"OKP", "crv":"Ed25519", "kid":"cad1500", "x":public
                        }]})
                        .to_string(),
                    ))
                    .unwrap();
            }
        }
    });
    let route_stop = stop.clone();
    let pm_dir = pm.dir.clone();
    let route_thread = std::thread::spawn(move || {
        while !route_stop.load(Ordering::SeqCst) {
            if let Some(mut request) = server.recv_timeout(Duration::from_millis(100)).unwrap() {
                let verb = request.url().strip_prefix("/api/cli/").unwrap().to_string();
                let method = request.method().clone();
                let response = post(&mut request, &method, &verb, &state, &pm_dir, &opts);
                request.respond(response).unwrap();
            }
        }
    });
    // Join on unwind too: a failed assertion must not leave fixture services running.
    struct Cleanup(Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
            for thread in self.1.drain(..) {
                let _ = thread.join();
            }
        }
    }
    let _cleanup = Cleanup(stop, vec![route_thread, jwks_thread]);
    let mut serial = 0;
    let mut call = |actor: &str, scope: &str, verb: &str, args: Value| {
        serial += 1;
        let now = crate::issue::time::now_epoch();
        // Match the existing Worker mint: Owners use operator, Members users/sub.
        let (principal_actor, role) = if actor == "owner-human" {
            ("operator".to_string(), "owner")
        } else {
            (format!("users/{actor}"), "member")
        };
        let header = URL_SAFE_NO_PAD.encode(
            json!({"alg":"EdDSA", "typ":"agenticos-wiki-actor/1", "kid":"cad1500"}).to_string(),
        );
        let claims = URL_SAFE_NO_PAD.encode(json!({
            "iss":issuer, "aud":"http://cad1500.board.localhost", "sub":actor,
            "actor":principal_actor, "principal_kind":"user", "role_at_issue":role,
            "organization_id":"cad1500", "scope":[scope], "credential_id":format!("fixture-{actor}"),
            "iat":now, "exp":now+120, "jti":format!("cad1500-{serial}")
        }).to_string());
        let signed = format!("{header}.{claims}");
        let envelope = format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).as_ref())
        );
        let body = args.to_string();
        let mut socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        write!(socket, "POST /api/cli/{verb} HTTP/1.1\r\nHost: cad1500.board.localhost\r\nAuthorization: Bearer wikienv_{envelope}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut bytes = Vec::new();
        socket.read_to_end(&mut bytes).unwrap();
        let split = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let header = std::str::from_utf8(&bytes[..split]).unwrap();
        let status: u16 = header
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes[split + 4..]).unwrap();
        (status, body)
    };
    let (status, created) = call(
        "alpha",
        "cli.write",
        "issue_new",
        json!({
            "project":"fixture", "title":"Review requirements", "owner":"alpha", "body":"Acceptance needs a document, not a repository."
        }),
    );
    assert_eq!(status, 201, "{created}");
    let id = created["issue"]["id"].as_str().unwrap();
    let (status, comment) = call(
        "alpha",
        "cli.write",
        "issue_comment",
        json!({"id":id,"body":"Progress: requirements reviewed."}),
    );
    assert_eq!(status, 200, "{comment}");
    let (status, work) = call("alpha", "cli.read", "my_work", json!({}));
    assert_eq!(status, 200, "{work}");
    assert_eq!(work["owner"], "alpha");
    assert_eq!(work["issues"][0]["id"], id);
    assert_eq!(work["issues"][0]["title"], "Review requirements");
    assert_eq!(work["issues"][0]["counts"]["comments"], 1);
    let (status, other) = call("beta", "cli.read", "my_work", json!({}));
    assert_eq!(status, 200, "{other}");
    assert_eq!(other["issues"], json!([]));
    assert_eq!(
        call("beta", "cli.read", "my_work", json!({"owner":"alpha"})).0,
        400
    );
    assert_eq!(call("alpha", "cli.write", "my_work", json!({})).0, 403);
    let rev = work["issues"][0]["rev"].as_str().unwrap();
    let (status, edited) = call(
        "alpha",
        "cli.write",
        "issue_set",
        json!({
            "ids":[id],"set":["owner=beta"],"if_rev":rev
        }),
    );
    assert_eq!(status, 200, "{edited}");
    let (_, departed) = call("alpha", "cli.read", "my_work", json!({}));
    assert_eq!(departed["issues"], json!([]));
    let (status, arrived) = call("beta", "cli.read", "my_work", json!({}));
    assert_eq!(status, 200, "{arrived}");
    assert_eq!(arrived["issues"][0]["id"], id);
    let (status, owner_created) = call(
        "owner-human",
        "cli.write",
        "issue_new",
        json!({"project":"fixture", "title":"Owner's personal requirement", "owner":"owner-human"}),
    );
    assert_eq!(status, 201, "{owner_created}");
    let (status, owner_work) = call("owner-human", "cli.read", "my_work", json!({}));
    assert_eq!(status, 200, "{owner_work}");
    assert_eq!(owner_work["owner"], "owner-human");
    assert_eq!(owner_work["issues"].as_array().unwrap().len(), 1);
    assert_eq!(owner_work["issues"][0]["id"], owner_created["issue"]["id"]);
}
