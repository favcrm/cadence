//! CAD-2321 independently authored refusal acceptance for the hosted CLI.
//! The product implementer must not edit or weaken this check.
//!
//! The rule (operator decision of 2026-11-15 on CAD-2294): a verified CLI
//! caller names a project-policy identity only through an exact pair of
//! principal kind and actor prefix — `operator` for a user, `user` with
//! `users/…` (keyed on the verified `sub`), `agent` with `agents/…` and
//! `service` with `service/…`. Any other pair names no caller: every
//! project read is refused as an unknown project, a listing is empty, and
//! `my_work` refuses before it loads anything.
//!
//! The fixture signs real Ed25519 envelopes, serves them a loopback JWKS and
//! drives `POST /api/cli/<verb>` through the route the public board runs.
//! Every mismatched caller carries `sub = mia`, the one member the private
//! project grants, so a guard that fell back to keying on `sub` (or on the
//! actor spelling) would admit it and this check would fail. The positive
//! control proves the same reads admit the matching pairs, so a refusal
//! here is never an unreadable fixture.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use tiny_http::{Header, Response, Server};

use super::{PublicBoard, ServeOpts};
use crate::issue::{project, write as issue_write, Pm};

const HOST: &str = "cad2321.board.localhost";
const KID: &str = "cad2321-fixture";
const ORG: &str = "cad2321-fixture";

fn fixture_server() -> Server {
    (3110..=3199)
        .find_map(|port| Server::http(("127.0.0.1", port)).ok())
        .expect("no free fixture port in 3110..3199")
}

fn port_of(server: &Server) -> u16 {
    server
        .server_addr()
        .to_ip()
        .expect("fixture bound TCP")
        .port()
}

/// The tracker's committed state: `HEAD` plus the bytes of every top-level
/// entry that is not `.git` or a gitignored cache.
type TrackerState = (String, Vec<(PathBuf, Vec<u8>)>);

fn tracker_state(pm: &Path) -> TrackerState {
    fn collect(root: &Path, at: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(at)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for entry in entries {
            let dotted = entry
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'));
            if at == root && dotted {
                continue;
            }
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
    collect(pm, pm, &mut out);
    let head = crate::issue::git(pm, &["rev-parse", "HEAD"]).unwrap();
    (head, out)
}

/// A tracker with a private project granting only `users/mia` and an open
/// project, one issue in each, behind a signed public CLI route.
struct Rig {
    _root: tempfile::TempDir,
    pm_dir: PathBuf,
    secret_issue: String,
    open_issue: String,
    key: Ed25519KeyPair,
    issuer: String,
    port: u16,
    stop: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

fn start() -> Rig {
    let root = tempfile::Builder::new()
        .prefix("cad2321-")
        .tempdir_in("/tmp")
        .unwrap();
    let pm = Pm::init(&root.path().join("pm")).unwrap();
    issue_write::project_add(&pm, "secret", "SEC", &[], &[], &[], None).unwrap();
    issue_write::project_add(&pm, "open", "OPN", &[], &[], &[], None).unwrap();
    let new = |key: &str| {
        issue_write::new_issue(
            &pm,
            &pm.dir,
            Some(key),
            &format!("{key} fixture issue"),
            None,
            None,
            &[],
            None,
            None,
            &[],
            None,
            None,
            "operator",
        )
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let secret_issue = new("secret");
    let open_issue = new("open");

    // The shipping policy writer refuses `private` while
    // `private_available()` is false, so the record is written as a real,
    // decodable `project.yaml` naming private visibility, and committed.
    let record_path = pm.dir.join("secret").join("project.yaml");
    let mut record: project::Project =
        serde_yaml::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    let mut policy = record.policy.clone().unwrap_or_default();
    policy.version = policy.version.saturating_add(1);
    policy.visibility = project::Visibility::Private;
    policy.members = vec![project::Grant::parse("users/mia").unwrap()];
    record.policy = Some(policy);
    std::fs::write(&record_path, serde_yaml::to_string(&record).unwrap()).unwrap();
    pm.commit(
        std::slice::from_ref(&record_path),
        "cad2321 private policy fixture\n\nActor: operator",
    )
    .unwrap();
    let loaded = project::load(&record_path).unwrap().policy.unwrap();
    assert_eq!(loaded.visibility, project::Visibility::Private);

    let state = root.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let key = Ed25519KeyPair::from_seed_unchecked(&[21; 32]).unwrap();
    let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());

    // The route's JWKS cache is process-wide and holds one issuer, so a
    // parallel test may force a refetch: answer every fetch until the end.
    let jwks = fixture_server();
    let issuer = format!("http://127.0.0.1:{}", port_of(&jwks));
    let jwks_stop = stop.clone();
    let jwks_thread = thread::spawn(move || {
        while !jwks_stop.load(Ordering::SeqCst) {
            if let Ok(Some(request)) = jwks.recv_timeout(Duration::from_millis(100)) {
                let valid = request.url() == "/.well-known/agenticos-board-jwks.json";
                let body = if valid {
                    json!({"keys": [{"kty": "OKP", "crv": "Ed25519", "kid": KID, "x": public_key}]})
                } else {
                    json!({"error": "not_found"})
                };
                let response = Response::from_string(body.to_string())
                    .with_status_code(if valid { 200 } else { 404 })
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                let _ = request.respond(response);
            }
        }
    });

    let board = fixture_server();
    let port = port_of(&board);
    let opts = ServeOpts {
        public: Some(PublicBoard {
            host: HOST.into(),
            issuer: issuer.clone(),
            company: ORG.into(),
            authorize_url: format!("{issuer}/authorize"),
        }),
        ..ServeOpts::default()
    };
    let pm_dir = pm.dir.clone();
    let board_stop = stop.clone();
    let route_pm = pm_dir.clone();
    let board_thread = thread::spawn(move || {
        while !board_stop.load(Ordering::SeqCst) {
            if let Ok(Some(mut request)) = board.recv_timeout(Duration::from_millis(100)) {
                let tail = request
                    .url()
                    .strip_prefix("/api/cli/")
                    .expect("only the cli route is called")
                    .to_string();
                let method = request.method().clone();
                let response =
                    super::cli_route::post(&mut request, &method, &tail, &state, &route_pm, &opts);
                let _ = request.respond(response);
            }
        }
    });

    Rig {
        _root: root,
        pm_dir,
        secret_issue,
        open_issue,
        key,
        issuer,
        port,
        stop,
        threads: vec![jwks_thread, board_thread],
    }
}

impl Rig {
    /// One call with a fresh envelope whose `sub` and `actor` are chosen
    /// independently of each other and of the principal kind.
    fn call(&self, kind: &str, actor: &str, sub: &str, verb: &str, args: Value) -> (u16, Value) {
        self.call_as(kind, actor, sub, "member", verb, args)
    }

    /// [`Rig::call`] with the role the envelope was minted with.
    fn call_as(
        &self,
        kind: &str,
        actor: &str,
        sub: &str,
        role: &str,
        verb: &str,
        args: Value,
    ) -> (u16, Value) {
        let now = crate::issue::time::now_epoch();
        let header = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "alg": "EdDSA", "typ": "agenticos-wiki-actor/1", "kid": KID
            }))
            .unwrap(),
        );
        let claims = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "iss": self.issuer,
                "aud": format!("http://{HOST}"),
                "sub": sub,
                "actor": actor,
                "principal_kind": kind,
                "role_at_issue": role,
                "organization_id": ORG,
                "scope": ["cli.read"],
                "credential_id": format!("cad2321-{sub}"),
                "iat": now,
                "exp": now + 120,
                "jti": format!("cad2321-{}", uuid::Uuid::new_v4()),
            }))
            .unwrap(),
        );
        let signed = format!("{header}.{claims}");
        let envelope = format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(self.key.sign(signed.as_bytes()).as_ref())
        );

        let body = serde_json::to_vec(&args).unwrap();
        let mut socket = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        write!(
            socket,
            "POST /api/cli/{verb} HTTP/1.1\r\nHost: {HOST}\r\nAuthorization: Bearer wikienv_{envelope}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        socket.write_all(&body).unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).unwrap();
        let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = std::str::from_utf8(&response[..split]).unwrap();
        let status = head
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        (
            status,
            serde_json::from_slice(&response[split + 4..]).unwrap(),
        )
    }
}

fn listed(body: &Value) -> Vec<String> {
    let mut ids: Vec<String> = body["issues"]
        .as_array()
        .unwrap_or_else(|| panic!("issue_ls answered no list: {body}"))
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

/// A caller whose principal kind and actor prefix disagree: refused as an
/// unknown project on both projects, an empty listing, and `my_work`
/// refused with `my_work_code`. No call changes the tracker.
fn assert_refused(rig: &Rig, kind: &str, actor: &str, my_work_code: &str) {
    let who = format!("{kind} principal with actor {actor} (sub mia)");
    let before = tracker_state(&rig.pm_dir);
    for id in [&rig.secret_issue, &rig.open_issue] {
        let (status, body) = rig.call(kind, actor, "mia", "issue_show", json!({ "id": id }));
        assert_eq!(status, 404, "{who} read {id}: {body}");
        assert_eq!(body["error"]["code"], "not_found", "{who} read {id}");
        // The project guard's answer, not the absent-issue one.
        assert_eq!(
            body["error"]["message"], "unknown project",
            "{who} read {id}"
        );
        assert_eq!(tracker_state(&rig.pm_dir), before, "{who} read {id} wrote");
    }

    let (status, body) = rig.call(kind, actor, "mia", "issue_ls", json!({}));
    assert_eq!(status, 200, "{who} issue_ls: {body}");
    assert!(listed(&body).is_empty(), "{who} listed issues: {body}");
    assert_eq!(tracker_state(&rig.pm_dir), before, "{who} issue_ls wrote");

    let (status, body) = rig.call(kind, actor, "mia", "my_work", json!({}));
    assert_eq!(status, 403, "{who} my_work: {body}");
    assert_eq!(body["error"]["code"], my_work_code, "{who} my_work: {body}");
    assert_eq!(tracker_state(&rig.pm_dir), before, "{who} my_work wrote");
}

/// The ticket's acceptance item: a `user` principal with a non-`users/`
/// actor is refused. `my_work` already refuses a non-`users/` actor as not a
/// human session, ahead of the identity check.
#[test]
fn a_user_principal_with_an_agent_actor_is_refused() {
    let rig = start();
    assert_refused(&rig, "user", "agents/x", "human_session_required");
}

/// The reverse mismatch — the issuer's fallthrough for an agent with no
/// enrolled id mints exactly this pair.
#[test]
fn an_agent_principal_with_a_user_actor_is_refused() {
    let rig = start();
    assert_refused(&rig, "agent", "users/mia", "identity_unresolved");
}

#[test]
fn a_service_principal_with_a_user_actor_is_refused() {
    let rig = start();
    assert_refused(&rig, "service", "users/mia", "identity_unresolved");
}

/// Positive control: the matching pairs reach the same reads where policy
/// grants them, so the refusals above are the guard and not the fixture.
#[test]
fn matching_kind_and_actor_pairs_are_admitted_where_policy_grants() {
    let rig = start();
    let before = tracker_state(&rig.pm_dir);

    // The granted member reads both projects and lists both issues.
    for id in [&rig.secret_issue, &rig.open_issue] {
        let (status, body) = rig.call(
            "user",
            "users/mia",
            "mia",
            "issue_show",
            json!({ "id": id }),
        );
        assert_eq!(status, 200, "granted member refused {id}: {body}");
        assert_eq!(body["issue"]["id"], json!(id), "{body}");
    }
    let (status, body) = rig.call("user", "users/mia", "mia", "issue_ls", json!({}));
    assert_eq!(status, 200, "{body}");
    let mut want = vec![rig.secret_issue.clone(), rig.open_issue.clone()];
    want.sort();
    assert_eq!(listed(&body), want, "granted member's listing");

    // An agent with its own prefix reads the open project; the private one
    // refuses every agent by policy.
    let (status, body) = rig.call(
        "agent",
        "agents/x",
        "x",
        "issue_show",
        json!({ "id": rig.open_issue }),
    );
    assert_eq!(
        status, 200,
        "matching agent refused the open project: {body}"
    );
    assert_eq!(body["issue"]["id"], json!(rig.open_issue), "{body}");
    let (status, body) = rig.call(
        "agent",
        "agents/x",
        "x",
        "issue_show",
        json!({ "id": rig.secret_issue }),
    );
    assert_eq!(status, 404, "an agent read the private project: {body}");
    assert_eq!(body["error"]["message"], "unknown project", "{body}");
    let (status, body) = rig.call("agent", "agents/x", "x", "issue_ls", json!({}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        listed(&body),
        vec![rig.open_issue.clone()],
        "matching agent listing"
    );
    assert_eq!(tracker_state(&rig.pm_dir), before, "an admitted read wrote");
}

/// A cross pair between the two reserved kinds is refused the same way.
#[test]
fn an_agent_principal_with_a_service_actor_is_refused() {
    let rig = start();
    assert_refused(&rig, "agent", "service/mia", "human_session_required");
}

/// Positive control for the remaining exact pairs: a service with its own
/// prefix reads the open project (the private one refuses every service by
/// policy), and the Owner's `operator` reads both.
#[test]
fn service_and_operator_pairs_are_admitted_where_policy_grants() {
    let rig = start();
    let before = tracker_state(&rig.pm_dir);

    let (status, body) = rig.call(
        "service",
        "service/svc",
        "svc",
        "issue_show",
        json!({ "id": rig.open_issue }),
    );
    assert_eq!(
        status, 200,
        "matching service refused the open project: {body}"
    );
    assert_eq!(body["issue"]["id"], json!(rig.open_issue), "{body}");
    let (status, body) = rig.call(
        "service",
        "service/svc",
        "svc",
        "issue_show",
        json!({ "id": rig.secret_issue }),
    );
    assert_eq!(status, 404, "a service read the private project: {body}");
    assert_eq!(body["error"]["message"], "unknown project", "{body}");
    let (status, body) = rig.call("service", "service/svc", "svc", "issue_ls", json!({}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        listed(&body),
        vec![rig.open_issue.clone()],
        "matching service listing"
    );

    for id in [&rig.secret_issue, &rig.open_issue] {
        let (status, body) = rig.call_as(
            "user",
            "operator",
            "owner",
            "owner",
            "issue_show",
            json!({ "id": id }),
        );
        assert_eq!(status, 200, "the operator was refused {id}: {body}");
        assert_eq!(body["issue"]["id"], json!(id), "{body}");
    }
    let (status, body) = rig.call_as("user", "operator", "owner", "owner", "issue_ls", json!({}));
    assert_eq!(status, 200, "{body}");
    let mut want = vec![rig.secret_issue.clone(), rig.open_issue.clone()];
    want.sort();
    assert_eq!(listed(&body), want, "the operator's listing");
    assert_eq!(tracker_state(&rig.pm_dir), before, "an admitted read wrote");
}
