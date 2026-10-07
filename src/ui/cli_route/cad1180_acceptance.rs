//! Independently authored by qa-fallback from the CAD-1180 durable
//! hosted ticket-writes acceptance (durable-ticket-spec.md). Exercises
//! the real signed HTTP write path end to end — real `post()`
//! dispatch, a real JWKS issuer on loopback, independently enrolled
//! synthetic principals — never the operator identity.
//!
//! The implementation writer must not edit or weaken this acceptance
//! check.
//!
//! Arms, in one deterministic sequence:
//!
//!  A. **Capability declared-but-unusable refuses before mutation.**
//!     `Hosted::validate()` is the pre-write gate the route must run
//!     (fail-closed `capability_unavailable`) when `opts.durability`
//!     is `Some` but its required binding/store is unusable — per the
//!     parent's design election, a durable-mode write with an unusable
//!     capability refuses BEFORE the write; the tracker tree must be
//!     byte-identical afterwards.
//!  B. **Store-effect failure after a known apply is truthful.** A
//!     signed `issue_set` with a stub `Store` answering `Err` (folded
//!     to `Unconfirmed`) must answer a non-success status carrying
//!     `applied:true` + `durability:"unconfirmed"` — never a durable
//!     success — and must not be auto-replayed: exactly one `persist`
//!     call reaches the seam, and the same spent assertion replayed
//!     answers 401, never a second mutation.
//!  C. **No capability configured is the unchanged local path.** The
//!     same verb with `opts.durability = None` answers the CAD-1179
//!     success shape with no `applied`/`durability`/`receipt` keys.
//!
//! Expected RED against the frozen `cli_route` baseline: the route
//! never reads `opts.durability` (the attach point and the pre-write
//! `Hosted::validate()` gate are the owner-pending integration), so
//! arm A answers the legacy 200 success and mutates the tracker —
//! failing on `assert_eq!(status, 503 …)` and the byte-identical-tree
//! assertion. After the owner wires attach + gate, arm A must refuse
//! before mutation, arm B must surface `applied:true` +
//! `durability:"unconfirmed"`, and arm C stays the unchanged shape.

use super::*;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tiny_http::Server;

use crate::issue::durability::{Binding, Deadline, Hosted, Receipt, Store, StoreOutcome};

/// `Store` stub: records every `persist` call and answers the armed
/// outcome. A stub answering `Confirmed` proves only that this fake
/// said so — never real generation/upload/restore semantics.
struct StubStore {
    calls: AtomicUsize,
}

impl Store for StubStore {
    fn persist(
        &self,
        _receipt: &Receipt,
        _artifact: &[u8],
        _deadline: &Deadline,
    ) -> crate::error::Result<StoreOutcome> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(crate::error::Error::internal(
            "stubbed store effect: persist refused",
        ))
    }
}

fn loopback_server() -> Server {
    (3110..=3199)
        .find_map(|port| Server::http(("127.0.0.1", port)).ok())
        .expect("no free isolated fixture port in 3110..3199")
}

fn assertion(key: &Ed25519KeyPair, issuer: &str, actor: &str, scope: &str, jti: &str) -> String {
    let now = crate::issue::time::now_epoch();
    let header = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "alg": "EdDSA", "typ": "agenticos-wiki-actor/1", "kid": "cad1180-fixture"
        }))
        .unwrap(),
    );
    let claims = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "iss": issuer,
            "aud": "http://cad1180.board.localhost",
            "sub": actor,
            "actor": format!("users/{actor}"),
            "principal_kind": "user",
            "role_at_issue": "member",
            "organization_id": "cad1180-fixture",
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
        "POST /api/cli/issue_set HTTP/1.1\r\nHost: cad1180.board.localhost\r\nAuthorization: Bearer wikienv_{envelope}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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

/// Serve `count` `POST /api/cli/issue_set` calls through the real
/// `post()` dispatch with the given `ServeOpts`.
fn serve_issue_set(
    server: Server,
    state: PathBuf,
    pm_dir: PathBuf,
    opts: ServeOpts,
    count: usize,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for _ in 0..count {
            let mut request = server
                .recv_timeout(Duration::from_secs(20))
                .unwrap()
                .expect("route not called");
            assert_eq!(request.url(), "/api/cli/issue_set");
            let method = request.method().clone();
            let response = post(&mut request, &method, "issue_set", &state, &pm_dir, &opts);
            request.respond(response).unwrap();
        }
    })
}

/// A fully bound `Hosted` on a failing stub store — capability
/// present, store reachable-but-refusing.
fn hosted_failing_store(stub: &Arc<StubStore>) -> Hosted {
    Hosted {
        binding: Binding {
            company: "cad1180-fixture".into(),
            instance: "cad1180-instance".into(),
            generation: 1,
        },
        store: stub.clone(),
        persist_budget: Duration::from_secs(5),
    }
}

#[test]
fn cad1180_hosted_write_refuses_capability_gap_reports_store_failure_and_keeps_local_unchanged() {
    let root = tempfile::Builder::new()
        .prefix("c1180-")
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

    // A synthetic issuer key, used only for this isolated route
    // fixture — never the operator identity.
    let key = Ed25519KeyPair::from_seed_unchecked(&[118; 32]).unwrap();
    let jwks = loopback_server();
    let issuer = format!("http://{}", jwks.server_addr());
    let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let jwks_thread = thread::spawn(move || {
        // The process-wide JWKS cache is keyed on the issuer; serve as
        // many fetches as the route makes (cache hits serve none).
        while let Ok(Some(request)) = jwks.recv_timeout(Duration::from_secs(20)) {
            assert_eq!(request.url(), "/.well-known/agenticos-board-jwks.json");
            let response = Response::from_string(
                json!({"keys": [{
                    "kty": "OKP", "crv": "Ed25519",
                    "kid": "cad1180-fixture", "x": public_key
                }]})
                .to_string(),
            )
            .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
            request.respond(response).unwrap();
        }
    });

    let public = crate::ui::PublicBoard {
        host: "cad1180.board.localhost".into(),
        issuer: issuer.clone(),
        company: "cad1180-fixture".into(),
        authorize_url: format!("{issuer}/authorize"),
    };

    // ---------------- Arm A: a hosted-capable `Pm` with an unusable
    // capability must refuse before mutating. The route-level gate is
    // `opts.durability` + `Hosted::validate()`; an invalid binding is
    // the "declared but unbound" case the parent elected to fail
    // closed. `Hosted` cannot express `store:None`, so the refusal
    // proof exercises the real `validate()` contract the route calls,
    // then drives the same signed write through `post()` with the
    // invalid capability attached — a route that gates must refuse it
    // before any tracker byte moves. ----------------
    let mut invalid = hosted_failing_store(&Arc::new(StubStore {
        calls: AtomicUsize::new(0),
    }));
    invalid.binding.instance = String::new();
    assert!(
        invalid.validate().is_err(),
        "an unbound capability must not validate"
    );

    let server_a = loopback_server();
    let port_a = server_a.server_addr().to_ip().unwrap().port();
    let route_a = serve_issue_set(
        server_a,
        state.clone(),
        pm.dir.clone(),
        ServeOpts {
            public: Some(public.clone()),
            durability: Some(invalid),
            ..ServeOpts::default()
        },
        1,
    );

    let writer_a = assertion(
        &key,
        &issuer,
        "hosted-writer",
        "cli.write",
        "c1180-hosted-a",
    );
    let args = json!({"ids": [&id], "set": ["title=hosted-writer"], "if_rev": original_rev});
    let before = tree_bytes(&project_dir);
    let (status, body) = call(port_a, &writer_a, &args);
    assert_eq!(
        status, 503,
        "a hosted write with an unusable capability must be refused \
         pre-write as capability_unavailable, not written: {body}"
    );
    assert_eq!(
        body["error"]["code"], "capability_unavailable",
        "the refusal must name the missing capability: {body}"
    );
    assert_eq!(
        tree_bytes(&project_dir),
        before,
        "capability-unusable write mutated the tracker before refusing"
    );
    assert_eq!(
        issue_write::issue_rev(&issue_dir).unwrap(),
        original_rev,
        "capability-unusable write bumped the ticket revision"
    );
    route_a.join().unwrap();

    // ------------- Arm B: store effect fails after a known apply —
    // truthful `applied:true` + `unconfirmed`, non-success, no replay.
    let stub = Arc::new(StubStore {
        calls: AtomicUsize::new(0),
    });
    let server_b = loopback_server();
    let port_b = server_b.server_addr().to_ip().unwrap().port();
    let route_b = serve_issue_set(
        server_b,
        state.clone(),
        pm.dir.clone(),
        ServeOpts {
            public: Some(public.clone()),
            durability: Some(hosted_failing_store(&stub)),
            ..ServeOpts::default()
        },
        1,
    );

    let writer_b = assertion(
        &key,
        &issuer,
        "hosted-writer",
        "cli.write",
        "c1180-hosted-b",
    );
    let (status, body) = call(port_b, &writer_b, &args);
    assert!(
        status >= 400 && status != 401 && status != 403,
        "a store-failed applied write must not answer a durable success \
         or a credential refusal: {status} {body}"
    );
    assert_eq!(
        body["applied"], true,
        "the applied write must be reported as applied: {body}"
    );
    assert_eq!(
        body["durability"], "unconfirmed",
        "a failed store effect must report unconfirmed, never durable: {body}"
    );
    assert!(
        body["receipt"]["commit"].is_string(),
        "an applied write must carry its originating receipt: {body}"
    );
    route_b.join().unwrap();
    assert_eq!(
        stub.calls.load(Ordering::SeqCst),
        1,
        "the client/relay must not replay a store-failed write"
    );
    // The commit landed (applied is real): the ticket carries it, and
    // a replay of the same spent assertion is refused — the failure
    // surface never silently re-mutates.
    let confirmed_rev = issue_write::issue_rev(&issue_dir).unwrap();
    assert_ne!(confirmed_rev, original_rev);
    let replay_args = json!({"ids": [&id], "set": ["title=replayed"], "if_rev": confirmed_rev});
    let server_b2 = loopback_server();
    let port_b2 = server_b2.server_addr().to_ip().unwrap().port();
    let route_b2 = serve_issue_set(
        server_b2,
        state.clone(),
        pm.dir.clone(),
        ServeOpts {
            public: Some(public.clone()),
            durability: Some(hosted_failing_store(&stub)),
            ..ServeOpts::default()
        },
        1,
    );
    let (status, body) = call(port_b2, &writer_b, &replay_args);
    assert_eq!(status, 401, "a spent write assertion was accepted: {body}");
    assert_eq!(body["error"]["code"], "assertion_replayed");
    route_b2.join().unwrap();
    assert_eq!(
        stub.calls.load(Ordering::SeqCst),
        1,
        "a refused replay must not reach the store seam"
    );

    // ------------- Arm C: no capability configured — the local path
    // is byte-for-byte the CAD-1179 shape. -------------
    let server_c = loopback_server();
    let port_c = server_c.server_addr().to_ip().unwrap().port();
    let route_c = serve_issue_set(
        server_c,
        state.clone(),
        pm.dir.clone(),
        ServeOpts {
            public: Some(public.clone()),
            durability: None,
            ..ServeOpts::default()
        },
        1,
    );
    let writer_c = assertion(
        &key,
        &issuer,
        "hosted-writer",
        "cli.write",
        "c1180-hosted-c",
    );
    let local_args = json!({
        "ids": [&id], "set": ["title=local-writer"], "if_rev": confirmed_rev
    });
    let (status, body) = call(port_c, &writer_c, &local_args);
    assert_eq!(status, 200, "an unconfigured write must succeed: {body}");
    assert!(
        body.get("applied").is_none()
            && body.get("durability").is_none()
            && body.get("receipt").is_none(),
        "a local write must not gain durability keys: {body}"
    );
    route_c.join().unwrap();

    jwks_thread.join().unwrap();
}
