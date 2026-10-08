//! CAD-1143 owner-intent read verifier acceptance — independently authored
//! by cc13-pi-acc793, separate from the host implementation and the existing
//! prepared-intent/attach acceptance files.
//!
//! Exercises the actual `social_owner_intent_read` daemon dispatch and the
//! public-host `GET /api/social-owner-intent/{id}` route. The fixture creates a
//! real reviewed app run and PREPARED row, serves a local JWKS document, signs
//! the AOS v1 assertion with its matching Ed25519 key, and starts the real UI
//! peer. The owner-intent GET is sent without a cookie or session header; the
//! public-host divert must reach the signed-assertion verifier before the
//! ordinary public-session gate. Its valid fixture uses the closed v1 claim
//! shape with the typed `COMPANY` workspace id and no instance/generation
//! claims; AOS's own before/after epoch check is outside this host acceptance.
//!
//! Refusals cover mismatched issuer, audience/host, workspace, purpose, intent
//! id, and digest, plus a stale app identity, staged-effect state/material,
//! and imported media-key connection/digest. The exact public Host is required.
//! The replay control reads
//! the hashed JTI ledger, restarts the daemon, then replays the same valid JWS
//! through the actual HTTP route. Expected app-data side effects remain fixed:
//! one PREPARED row, zero queued intents, and no new authorization event. The
//! row snapshot is a read-only SQLite side-effect oracle, not a public
//! prepared-state RPC (none exists). The test deliberately corrupts/restores
//! local prepared/effect fixtures to exercise stale-provenance refusals; during
//! each verifier call, the only expected durable write is sha256(jti) in
//! operator-auth state.
//!
//! Source-only turn: this acceptance has not been built or run.
//!
//! Attach authority is separate: `tests/cad1143_intent_attach_acceptance.rs`
//! pins `social.queue-validation.v1` and proves a `social.intent.read.v1`
//! assertion cannot authorize attach. This test deliberately remains scoped
//! to the independent owner-intent READ RPC and public-host GET boundary.
#![cfg(feature = "test-seam")]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cadence_agent::platform::agenticos_external::media_import::MediaResolver;
use cadence_agent::platform::agenticos_external::publish_sender::DeviceCredential;
use cadence_agent::store::{Store, PLATFORM_STREAM};
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon};
use ring::signature::{Ed25519KeyPair, KeyPair};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use uuid::Uuid;

const NOW: i64 = 1_800_000_000;
const COMPANY: &str = "workspace_cad1143_owner_read";
const KID: &str = "cad1143-owner-read-acceptance";
const PURPOSE: &str = "social.intent.read.v1";
const DESTINATION_ID: &str = "dest-01";
const CAPTION: &str = "# Owner read acceptance\nReviewed copy.";

/// A bounded local HTTP peer used only for the two source-backed fixture
/// edges: JWKS publication and the AOS destinations lookup required by
/// prepare. The daemon still uses its real HTTP clients and verifier.
struct HttpStub {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl HttpStub {
    fn start(path: &'static str, body: String) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local HTTP fixture");
        listener
            .set_nonblocking(true)
            .expect("nonblocking local HTTP fixture");
        let addr = listener.local_addr().expect("fixture address");
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let thread_stop = Arc::clone(&stop);
        let thread_requests = Arc::clone(&requests);
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut request = Vec::new();
                        let mut chunk = [0u8; 1024];
                        while request.len() < 16 * 1024 {
                            match stream.read(&mut chunk) {
                                Ok(0) => break,
                                Ok(read) => {
                                    request.extend_from_slice(&chunk[..read]);
                                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                        break;
                                    }
                                }
                                Err(error)
                                    if matches!(
                                        error.kind(),
                                        std::io::ErrorKind::WouldBlock
                                            | std::io::ErrorKind::TimedOut
                                    ) =>
                                {
                                    break;
                                }
                                Err(_) => break,
                            }
                        }
                        if !request.is_empty() {
                            thread_requests.fetch_add(1, SeqCst);
                        }
                        let request_line = String::from_utf8_lossy(&request);
                        let target = request_line
                            .lines()
                            .next()
                            .and_then(|line| line.split_ascii_whitespace().nth(1))
                            .unwrap_or("");
                        let matches = target.split('?').next() == Some(path);
                        let (status, reason, response_body) = if matches {
                            (200, "OK", body.as_str())
                        } else {
                            (404, "Not Found", "{}")
                        };
                        let response = format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                            response_body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            stop,
            requests,
            thread: Some(thread),
        }
    }

    fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn requests(&self) -> usize {
        self.requests.load(SeqCst)
    }
}

impl Drop for HttpStub {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Signer {
    key: Ed25519KeyPair,
    jwks: String,
}

impl Signer {
    fn new() -> Self {
        let seed = [0x42u8; 32];
        let key = Ed25519KeyPair::from_seed_unchecked(&seed).expect("fixed test Ed25519 key");
        let jwks = json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "kid": KID,
                "x": URL_SAFE_NO_PAD.encode(key.public_key().as_ref()),
                "use": "sig",
                "alg": "EdDSA"
            }]
        })
        .to_string();
        Self { key, jwks }
    }

    fn compact(&self, claims: &Value) -> String {
        let header = json!({"alg":"EdDSA", "typ":"JWT", "kid":KID});
        let encoded_header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
        let encoded_claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
        let signing_input = format!("{encoded_header}.{encoded_claims}");
        let signature = self.key.sign(signing_input.as_bytes());
        format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.as_ref())
        )
    }
}

struct Fx {
    root: tempfile::TempDir,
    clock: Arc<AtomicI64>,
    store: Store,
    media_stub: HttpStub,
    resolver: Arc<MediaResolver>,
    daemon: Option<(Arc<AtomicBool>, JoinHandle<cadence_agent::Result<()>>)>,
}

impl Fx {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("c1143owner")
            .tempdir()
            .expect("isolated owner-read root");
        cadence_agent::issue::Pm::init(&root.path().join("pm")).expect("fixture PM");
        std::fs::create_dir_all(root.path().join("state")).expect("fixture state dir");
        let store = Store::open(&root.path().join("state/cadence.sqlite3")).expect("fixture store");
        let media_body = json!({
            "ok": true,
            "data": {
                "version": "1",
                "destinations": [{
                    "connectionId": "connA_harbour",
                    "toolkit": "facebook",
                    "displayName": "Harbour",
                    "destinationId": DESTINATION_ID,
                    "status": "active",
                    "available": true,
                    "publishable": true
                }]
            }
        })
        .to_string();
        let media_stub = HttpStub::start("/v1/runtime/connectors/destinations", media_body);
        let resolver = Arc::new(
            MediaResolver::new(
                &media_stub.origin(),
                DeviceCredential::new("acceptance-read-credential".into()),
            )
            .expect("fixture destinations resolver"),
        );
        Self {
            root,
            clock: Arc::new(AtomicI64::new(NOW)),
            store,
            media_stub,
            resolver,
            daemon: None,
        }
    }

    fn state_dir(&self) -> PathBuf {
        self.root.path().join("state")
    }

    fn pm_dir(&self) -> PathBuf {
        self.root.path().join("pm")
    }

    fn start_daemon(&mut self) {
        assert!(self.daemon.is_none(), "fixture daemon already running");
        let dir = self.state_dir();
        let stop = Arc::new(AtomicBool::new(false));
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", self.pm_dir().to_str().unwrap());
        let clock = Arc::clone(&self.clock);
        let mut opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(Arc::clone(&stop)),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            operator_clock: Some(Arc::new(move || clock.load(SeqCst))),
            social_media_resolver: Some(Arc::clone(&self.resolver)),
            ..Default::default()
        };
        cadence_agent::platform::local::register_at(
            &dir,
            &mut opts,
            dir.join("outbox"),
            "http://127.0.0.1:3119".into(),
        );
        let handle = std::thread::spawn(move || daemon::serve_with(&dir, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(
            &self.state_dir(),
            "health",
            json!({}),
            Duration::from_secs(2),
        )
        .is_err()
            || Seam::token_at(&self.state_dir()).is_none()
        {
            assert!(
                !handle.is_finished() && std::time::Instant::now() < deadline,
                "fixture daemon failed to start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        self.daemon = Some((stop, handle));
    }

    fn stop_daemon(&mut self) {
        let Some((stop, handle)) = self.daemon.take() else {
            return;
        };
        stop.store(true, SeqCst);
        handle
            .join()
            .expect("fixture daemon thread panicked")
            .expect("fixture daemon shutdown");
    }

    fn restart_daemon(&mut self) {
        self.stop_daemon();
        self.start_daemon();
    }

    fn setup_rpc(&self, method: &str, params: Value) -> Value {
        scoped(Asserted::Operator, || {
            client::rpc(&self.state_dir(), method, params)
        })
        .unwrap_or_else(|error| panic!("fixture setup {method} refused: {error}"))
    }

    fn setup_prepared_intent(&self) -> (String, String, Value) {
        self.register_run_agents();
        let source = self.root.path().join("app-source");
        std::fs::create_dir_all(source.join("workflows")).expect("workflow dir");
        std::fs::write(
            source.join("app.md"),
            "---\napp: owner-read-accept\ntitle: Owner read accept\nversion: '0.1.0'\n\
             summary: Owner intent read acceptance fixture.\nneeds:\n  connections: []\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send\n---\n\n# Owner read accept\n",
        )
        .expect("fixture manifest");
        std::fs::write(source.join("workflows/brief.md"), WORKFLOW).expect("fixture workflow");
        let installed = self.setup_rpc(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        );
        let install = installed["install_id"].as_str().unwrap().to_owned();
        let bundle = installed["digest"].as_str().unwrap().to_owned();
        self.setup_rpc(
            "app_install_team_set",
            json!({
                "install_id": install,
                "owner_pm": "lead",
                "roles": {"writer": "writer", "reviewer": "reviewer"},
                "expected_revision": 0
            }),
        );
        let connection_id = self.local_connection();
        let created = self.setup_rpc(
            "app_binding_create",
            json!({
                "install_id": install,
                "slot": "publication",
                "connection_id": connection_id,
                "request_id": "bind-owner-read"
            }),
        );
        let binding = &created["binding"];
        self.setup_rpc(
            "app_binding_publish_set",
            json!({
                "install_id": install,
                "binding_id": binding["id"],
                "expected_revision": binding["revision"],
                "destination_id": DESTINATION_ID,
                "destination_label": "Harbour",
                "toolkit": "facebook",
                "timezone": "Asia/Hong_Kong",
                "grant_id": "dpq_owner_read_fixture_grant"
            }),
        );
        let run_id = self.complete_approved_run(&install, &bundle, "owner-read-run-01");
        let prepared = self.setup_rpc(
            "app_publish_intent_prepare",
            json!({"request_id":"owner-read-prepare-01", "run_id":run_id, "mode":"now"}),
        );
        assert_eq!(prepared["prepared"]["state"], "prepared", "{prepared}");
        let prepared_id = prepared["prepared"]["prepared_id"]
            .as_str()
            .expect("prepared identity")
            .to_owned();
        let owner_descriptor = prepared["prepared"]["owner_intent"].clone();
        assert_eq!(owner_descriptor["intent_id"], json!(prepared_id));
        (install, prepared_id, owner_descriptor)
    }

    fn register_run_agents(&self) {
        for (alias, role) in [("lead", "pm"), ("writer", "worker"), ("reviewer", "worker")] {
            if self.store.agent_opt(alias).expect("agent lookup").is_none() {
                self.store
                    .register_agent(&cadence_agent::store::NewAgent {
                        alias,
                        provider: "claude",
                        endpoint_kind: "managed",
                        role,
                        cwd: "/tmp",
                        sandbox: "read-only",
                        instructions: None,
                        params: Some("{\"upstream\":\"lead\"}"),
                        team_role: None,
                        model_policy: None,
                    })
                    .expect("fixture agent");
                self.store
                    .set_identity(
                        alias,
                        &cadence_agent::adapter::Identity {
                            thread_id: "t".into(),
                            session_id: "s".into(),
                            model: None,
                            effort: None,
                            pid: std::process::id(),
                            endpoint: None,
                            generation: Some("g1".into()),
                            attach: None,
                        },
                    )
                    .expect("fixture agent identity");
            }
        }
    }

    fn complete_approved_run(&self, install: &str, bundle: &str, request_id: &str) -> String {
        let started = self.setup_rpc(
            "app_run_start",
            json!({
                "install_id": install,
                "workflow": "brief",
                "request_id": request_id,
                "expected_quotes": {},
                "inputs": {"subject":"Board update", "source":"Reviewed fixture facts."}
            }),
        );
        let run_id = started["id"].as_str().expect("run id").to_owned();
        let finish_step = |run: &Value, step: usize, reply: Value| {
            let message_id = run["steps"][step]["message_id"]
                .as_str()
                .expect("step message")
                .to_owned();
            let token = cadence_agent::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
            self.store
                .mark_running(&message_id, &token)
                .expect("mark fixture turn");
            let message = self
                .store
                .message(&message_id)
                .expect("message read")
                .expect("message exists");
            let reply = json!({"turn_id":message.turn_id, "text":reply.to_string()});
            self.store
                .finish(&message, "completed", &reply, None)
                .expect("finish fixture turn");
        };
        let run = self
            .store
            .app_run_dispatch(&run_id, bundle)
            .expect("dispatch writer turn");
        finish_step(
            &run,
            0,
            json!({
                "schema":1,"kind":"produce_text","run_id":run_id,"step_id":"s1","revision":1,
                "outcome":"succeeded","artifacts":[{"media_type":"text/markdown","text":CAPTION}]
            }),
        );
        let run = self
            .store
            .app_run_dispatch(&run_id, bundle)
            .expect("dispatch reviewer turn");
        let artifact_digest = cadence_agent::store::app_runs::artifact_digest(CAPTION.as_bytes());
        finish_step(
            &run,
            1,
            json!({
                "schema":1,"kind":"review_text","run_id":run_id,"step_id":"s2","revision":1,
                "producer_step_id":"s1","producer_revision":1,"artifact_sha256":artifact_digest,
                "decision":"approve","rationale":"Reviewed the exact fixture artifact."
            }),
        );
        let shown = self.setup_rpc("app_run_show", json!({"run_id":run_id}));
        assert_eq!(shown["state"], "succeeded", "approved fixture run: {shown}");
        run_id
    }

    fn local_connection(&self) -> String {
        self.setup_rpc("connection_list", json!({}))["connections"]
            .as_array()
            .expect("connections")
            .iter()
            .find(|row| row["provider"] == "local" && row["account"] == "local")
            .expect("built-in local connection")["id"]
            .as_str()
            .expect("local connection id")
            .to_owned()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop_daemon();
    }
}

struct Board {
    port: u16,
    host: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<cadence_agent::Result<()>>>,
}

impl Board {
    fn start(state_dir: &Path, pm_dir: &Path, issuer: &str) -> Self {
        let port = (3110..3200)
            .find(|port| TcpListener::bind(("127.0.0.1", *port)).is_ok())
            .expect("free test board port in 3110-3199");
        let host = format!("owner-intent.board.localhost:{port}");
        let public = cadence_agent::ui::PublicBoard {
            host: host.clone(),
            issuer: issuer.to_owned(),
            company: COMPANY.to_owned(),
            company_slug: None,
            authorize_url: format!("{issuer}/v2/board/authorize"),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (startup_tx, startup_rx) = std::sync::mpsc::channel();
        let opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            allow_hosts: vec![host.clone()],
            board_public_only: true,
            public: Some(public),
            read_only: true,
            stop: Some(Arc::clone(&stop)),
            startup: Some(startup_tx),
            test_seam: true,
            ..Default::default()
        };
        let state = state_dir.to_path_buf();
        let pm = pm_dir.to_path_buf();
        let thread = std::thread::spawn(move || cadence_agent::ui::serve(&state, &pm, &opts));
        startup_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("board startup notification")
            .expect("board listener bind");
        Self {
            port,
            host,
            stop,
            thread: Some(thread),
        }
    }

    fn stop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .expect("fixture board thread panicked")
                .expect("fixture board shutdown");
        }
    }
}

impl Drop for Board {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct HttpResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

fn http_get_owner_read(
    board: &Board,
    host: &str,
    intent_id: &str,
    assertion: &str,
) -> HttpResponse {
    let mut stream = TcpStream::connect(("127.0.0.1", board.port)).expect("connect fixture board");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("board read timeout");
    let request = format!(
        "GET /api/social-owner-intent/{intent_id} HTTP/1.1\r\nHost: {host}\r\nAccept: application/json\r\nAuthorization: Bearer {assertion}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .expect("send owner-intent GET");
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .expect("read owner-intent response");
    let response = String::from_utf8(raw).expect("HTTP response is UTF-8");
    let (header_text, body) = response
        .split_once("\r\n\r\n")
        .expect("HTTP response headers");
    let mut lines = header_text.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_ascii_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .expect("HTTP status");
    let headers: BTreeMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .fold(BTreeMap::new(), |mut headers, (name, value)| {
            headers
                .entry(name)
                .and_modify(|existing| {
                    existing.push_str(", ");
                    existing.push_str(&value);
                })
                .or_insert(value);
            headers
        });
    HttpResponse {
        status,
        headers,
        body: body.to_owned(),
    }
}

fn base_claims(host: &str, issuer: &str, intent_id: &str, digest: &str, jti: &str) -> Value {
    json!({
        "iss": issuer,
        "aud": host,
        "sub": "aos-owner-acceptance-user",
        "purpose": PURPOSE,
        "workspace": COMPANY,
        "intent_id": intent_id,
        "expected_intent_digest": digest,
        "iat": NOW,
        "exp": NOW + 15,
        "jti": jti
    })
}

fn fresh_jti() -> String {
    Uuid::new_v4().to_string()
}

fn owner_rpc(fx: &Fx, intent_id: &str, assertion: &str) -> cadence_agent::Result<Value> {
    client::rpc(
        &fx.state_dir(),
        "social_owner_intent_read",
        json!({"intent_id":intent_id, "assertion":assertion}),
    )
}

fn refusal(fx: &Fx, intent_id: &str, assertion: &str, expected: &str) {
    let error =
        owner_rpc(fx, intent_id, assertion).expect_err("owner assertion unexpectedly accepted");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains(expected),
        "expected refusal containing {expected:?}, got {rendered}"
    );
}

fn jti_hashes(state_dir: &Path) -> HashSet<String> {
    let path = cadence_agent::operator_auth::dir(state_dir).join("sessions.json");
    let Ok(bytes) = std::fs::read(path) else {
        return HashSet::new();
    };
    let saved: Value = serde_json::from_slice(&bytes).expect("operator auth ledger JSON");
    saved
        .get("jtis")
        .and_then(Value::as_object)
        .map(|jtis| jtis.keys().cloned().collect())
        .unwrap_or_default()
}

#[derive(Debug, PartialEq, Eq)]
struct PublishEffects {
    prepared: Vec<(String, String, String, Option<String>)>,
    staged_effects: Vec<(String, String)>,
    queued: usize,
    prepared_events: usize,
    authorized_events: usize,
}

fn publish_effects(fx: &Fx, install: &str) -> PublishEffects {
    let conn = Connection::open_with_flags(
        fx.state_dir().join("cadence.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("read-only prepared-row connection");
    let mut statement = conn
        .prepare(
            "SELECT prepared_id, state, descriptor_digest, grant_id FROM social_publish_prepared WHERE install_id = ?1 ORDER BY prepared_id",
        )
        .expect("prepared row query");
    let prepared = statement
        .query_map([install], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("prepared row iterator")
        .map(|row| row.expect("prepared row"))
        .collect();
    let mut effect_statement = conn
        .prepare(
            "SELECT p.effect_id, e.state FROM social_publish_prepared p JOIN platform_effects e ON e.effect_id = p.effect_id WHERE p.install_id = ?1 ORDER BY p.effect_id",
        )
        .expect("prepared staged-effect query");
    let staged_effects = effect_statement
        .query_map([install], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("prepared staged-effect iterator")
        .map(|row| row.expect("prepared staged-effect row"))
        .collect();
    let queued = fx
        .store
        .social_publish_list(Some(install), None)
        .expect("publish queue read")["intents"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(usize::MAX);
    let events = fx
        .store
        .events_tail(PLATFORM_STREAM, 500)
        .expect("platform event read");
    let prepared_events = events
        .iter()
        .filter(|event| {
            event.kind == cadence_agent::store::social_publish::SOCIAL_PUBLISH_PREPARED_EVENT
        })
        .count();
    let authorized_events = events
        .iter()
        .filter(|event| {
            event.kind == cadence_agent::store::social_publish::SOCIAL_PUBLISH_AUTHORIZED_EVENT
        })
        .count();
    PublishEffects {
        prepared,
        staged_effects,
        queued,
        prepared_events,
        authorized_events,
    }
}

fn set_staged_effect_state(fx: &Fx, effect_id: &str, state: &str) {
    let conn = Connection::open(fx.state_dir().join("cadence.sqlite3"))
        .expect("open prepared staged-effect database");
    let changed = conn
        .execute(
            "UPDATE platform_effects SET state = ?1 WHERE effect_id = ?2",
            rusqlite::params![state, effect_id],
        )
        .expect("change prepared staged-effect state for refusal case");
    assert_eq!(changed, 1, "fixture staged effect exists");
}

fn persist_prepared_descriptor(
    fx: &Fx,
    prepared_id: &str,
    mut descriptor: Value,
    media_key: Option<&str>,
) -> String {
    let intent_digest = independent_intent_digest(&descriptor);
    descriptor["intent_digest"] = json!(intent_digest);
    let descriptor_digest = cadence_agent::store::app_runs::material_digest(&descriptor);
    let image_digest = descriptor
        .get("image_digest")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let conn = Connection::open(fx.state_dir().join("cadence.sqlite3"))
        .expect("open prepared-intent database");
    let changed = conn
        .execute(
            "UPDATE social_publish_prepared SET descriptor = ?1, descriptor_digest = ?2, image_digest = ?3, media_key = ?4 WHERE prepared_id = ?5",
            rusqlite::params![descriptor.to_string(), descriptor_digest, image_digest, media_key, prepared_id],
        )
        .expect("persist prepared descriptor refusal fixture");
    assert_eq!(changed, 1, "fixture prepared intent exists");
    intent_digest
}

fn independent_intent_digest(descriptor: &Value) -> String {
    let object = descriptor.as_object().expect("owner descriptor object");
    let fields: BTreeMap<String, Value> = object
        .iter()
        .filter(|(key, _)| key.as_str() != "intent_digest")
        .map(|(key, value)| (key.as_str().to_owned(), value.clone()))
        .collect();
    format!("{:x}", Sha256::digest(serde_json::to_vec(&fields).unwrap()))
}

const WORKFLOW: &str = r#"---
title: "Owner read brief"
goal: "One reviewed brief"
inputs:
  writer: { ask: "writer" }
  reviewer: { ask: "reviewer" }
  subject: { ask: "subject" }
  source: { ask: "facts" }
---

## Write
agent: {{writer}}
size: S
action: local.text.produce

Write one brief about {{subject}} grounded only in {{source}}.

### Acceptance
- [ ] brief exists

## Review
agent: {{reviewer}}
size: S
depends_on: 1
action: local.text.review

Review the artifact.

### Acceptance
- [ ] reviewed
"#;

#[test]
fn signed_owner_read_checks_exact_scope_and_durable_one_use_jti_without_publish_effects() {
    let signer = Signer::new();
    let issuer = HttpStub::start(
        "/.well-known/agenticos-board-jwks.json",
        signer.jwks.clone(),
    );
    let mut fx = Fx::new();
    fx.start_daemon();
    let (install, intent_id, expected_descriptor) = fx.setup_prepared_intent();
    let independently_computed_digest = independent_intent_digest(&expected_descriptor);
    assert_eq!(
        expected_descriptor["intent_digest"].as_str(),
        Some(independently_computed_digest.as_str()),
        "descriptor digest follows the closed sorted-field contract"
    );
    let expected_digest = expected_descriptor["intent_digest"]
        .as_str()
        .expect("intent digest")
        .to_owned();
    let before = publish_effects(&fx, &install);
    assert_eq!(before.prepared.len(), 1);
    assert_eq!(before.prepared[0].0, intent_id);
    assert_eq!(before.prepared[0].1, "prepared");
    assert_eq!(before.prepared[0].3, None, "PREPARED has no attached grant");
    assert_eq!(expected_descriptor["app_id"], json!("owner-read-accept"));
    assert_eq!(before.staged_effects.len(), 1);
    assert_eq!(
        before.staged_effects[0].0.as_str(),
        expected_descriptor["effect_id"]
            .as_str()
            .expect("staged effect identity")
    );
    assert_eq!(before.staged_effects[0].1, "waiting");
    assert_eq!(before.queued, 0);
    assert_eq!(before.prepared_events, 1);
    assert_eq!(before.authorized_events, 0);

    let mut board = Board::start(&fx.state_dir(), &fx.pm_dir(), &issuer.origin());
    assert_eq!(
        board.host,
        format!("owner-intent.board.localhost:{}", board.port)
    );
    assert!(jti_hashes(&fx.state_dir()).is_empty());

    // Correctly signed but host-bound claim mismatches fail before JTI
    // consumption. Claims use the actual Config values for the base control.
    let jti_aud = fresh_jti();
    let mut wrong_aud = base_claims(
        &board.host,
        &issuer.origin(),
        &intent_id,
        &expected_digest,
        &jti_aud,
    );
    wrong_aud["aud"] = json!(format!("other.board.localhost:{}", board.port));
    let wrong_aud_jws = signer.compact(&wrong_aud);
    refusal(
        &fx,
        &intent_id,
        &wrong_aud_jws,
        "issuer, audience, workspace, purpose, or intent scope mismatch",
    );
    let wrong_aud_http = http_get_owner_read(&board, &board.host, &intent_id, &wrong_aud_jws);
    assert_eq!(
        wrong_aud_http.status, 403,
        "wrong JWS audience over real UI route"
    );
    assert!(!jti_hashes(&fx.state_dir()).contains(&cadence_agent::operator_auth::digest(&jti_aud)));
    assert_eq!(publish_effects(&fx, &install), before);

    let jti_workspace = fresh_jti();
    let mut wrong_workspace = base_claims(
        &board.host,
        &issuer.origin(),
        &intent_id,
        &expected_digest,
        &jti_workspace,
    );
    wrong_workspace["workspace"] = json!("workspace_other");
    refusal(
        &fx,
        &intent_id,
        &signer.compact(&wrong_workspace),
        "issuer, audience, workspace, purpose, or intent scope mismatch",
    );
    assert!(!jti_hashes(&fx.state_dir())
        .contains(&cadence_agent::operator_auth::digest(&jti_workspace)));
    assert_eq!(publish_effects(&fx, &install), before);

    let jti_purpose = fresh_jti();
    let mut wrong_purpose = base_claims(
        &board.host,
        &issuer.origin(),
        &intent_id,
        &expected_digest,
        &jti_purpose,
    );
    wrong_purpose["purpose"] = json!("social.publish.v1");
    refusal(
        &fx,
        &intent_id,
        &signer.compact(&wrong_purpose),
        "issuer, audience, workspace, purpose, or intent scope mismatch",
    );
    assert!(
        !jti_hashes(&fx.state_dir()).contains(&cadence_agent::operator_auth::digest(&jti_purpose))
    );
    assert_eq!(publish_effects(&fx, &install), before);

    let jti_issuer = fresh_jti();
    let mut wrong_issuer = base_claims(
        &board.host,
        &issuer.origin(),
        &intent_id,
        &expected_digest,
        &jti_issuer,
    );
    wrong_issuer["iss"] = json!("http://untrusted-issuer.invalid");
    refusal(
        &fx,
        &intent_id,
        &signer.compact(&wrong_issuer),
        "issuer, audience, workspace, purpose, or intent scope mismatch",
    );
    assert!(
        !jti_hashes(&fx.state_dir()).contains(&cadence_agent::operator_auth::digest(&jti_issuer))
    );
    assert_eq!(publish_effects(&fx, &install), before);

    let jti_intent = fresh_jti();
    let mut wrong_intent = base_claims(
        &board.host,
        &issuer.origin(),
        &intent_id,
        &expected_digest,
        &jti_intent,
    );
    wrong_intent["intent_id"] = json!("sprep-other-intent");
    refusal(
        &fx,
        &intent_id,
        &signer.compact(&wrong_intent),
        "issuer, audience, workspace, purpose, or intent scope mismatch",
    );
    assert!(
        !jti_hashes(&fx.state_dir()).contains(&cadence_agent::operator_auth::digest(&jti_intent))
    );
    assert_eq!(publish_effects(&fx, &install), before);

    // A correctly scoped, signed assertion with the wrong expected digest is
    // refused by the real descriptor comparison. The verified assertion's
    // JTI is durably burned even though no publish state changes.
    let wrong_digest_value = if expected_digest == "f".repeat(64) {
        "e".repeat(64)
    } else {
        "f".repeat(64)
    };
    let jti_bad_digest = fresh_jti();
    let wrong_digest_claims = base_claims(
        &board.host,
        &issuer.origin(),
        &intent_id,
        &wrong_digest_value,
        &jti_bad_digest,
    );
    let wrong_digest_jws = signer.compact(&wrong_digest_claims);
    refusal(
        &fx,
        &intent_id,
        &wrong_digest_jws,
        "social intent descriptor digest mismatch",
    );
    assert!(jti_hashes(&fx.state_dir())
        .contains(&cadence_agent::operator_auth::digest(&jti_bad_digest)));
    let jti_bad_digest_http = fresh_jti();
    let wrong_digest_http_jws = signer.compact(&base_claims(
        &board.host,
        &issuer.origin(),
        &intent_id,
        &wrong_digest_value,
        &jti_bad_digest_http,
    ));
    let wrong_digest_http =
        http_get_owner_read(&board, &board.host, &intent_id, &wrong_digest_http_jws);
    assert_eq!(
        wrong_digest_http.status, 403,
        "wrong descriptor digest over real UI route"
    );
    assert!(jti_hashes(&fx.state_dir())
        .contains(&cadence_agent::operator_auth::digest(&jti_bad_digest_http)));
    assert_eq!(publish_effects(&fx, &install), before);

    // The configured public host is an exact surface selector. A request to
    // the allowed loopback Host is refused by hosted-only mode before daemon
    // verification; its valid assertion is still unspent.
    let valid_jti = fresh_jti();
    let valid_claims = base_claims(
        &board.host,
        &issuer.origin(),
        &intent_id,
        &expected_digest,
        &valid_jti,
    );
    let valid_jws = signer.compact(&valid_claims);
    let wrong_host_http = http_get_owner_read(
        &board,
        &format!("127.0.0.1:{}", board.port),
        &intent_id,
        &valid_jws,
    );
    assert_eq!(
        wrong_host_http.status, 421,
        "non-public Host must not reach the signed route"
    );
    assert!(
        !jti_hashes(&fx.state_dir()).contains(&cadence_agent::operator_auth::digest(&valid_jti))
    );
    assert_eq!(publish_effects(&fx, &install), before);

    let mut reproof_jtis = Vec::new();
    let effect_id = expected_descriptor["effect_id"]
        .as_str()
        .expect("staged effect identity");

    // The actual owner GET must not disclose a prepared descriptor once its
    // staged effect has left the waiting state.
    set_staged_effect_state(&fx, effect_id, "decided");
    let decided_effects = publish_effects(&fx, &install);
    assert!(decided_effects
        .staged_effects
        .iter()
        .any(|(id, state)| id == effect_id && state == "decided"));
    let decided_jti = fresh_jti();
    let decided_jws = signer.compact(&base_claims(
        &board.host,
        &issuer.origin(),
        &intent_id,
        &expected_digest,
        &decided_jti,
    ));
    let decided_http = http_get_owner_read(&board, &board.host, &intent_id, &decided_jws);
    assert_eq!(
        decided_http.status, 403,
        "terminal staged effect must refuse owner read"
    );
    assert_eq!(publish_effects(&fx, &install), decided_effects);
    assert!(
        jti_hashes(&fx.state_dir()).contains(&cadence_agent::operator_auth::digest(&decided_jti))
    );
    reproof_jtis.push(decided_jti);
    set_staged_effect_state(&fx, effect_id, "waiting");
    assert_eq!(publish_effects(&fx, &install), before);

    // A self-consistent prepared descriptor cannot override the staged
    // effect's receipt for the current reviewed run material.
    let mut changed_material = expected_descriptor.clone();
    let changed_snapshot_digest =
        if changed_material["run_snapshot_digest"] == json!("a".repeat(64)) {
            "b".repeat(64)
        } else {
            "a".repeat(64)
        };
    changed_material["run_snapshot_digest"] = json!(changed_snapshot_digest);
    let changed_material_digest =
        persist_prepared_descriptor(&fx, &intent_id, changed_material, None);
    let changed_material_effects = publish_effects(&fx, &install);
    let material_jti = fresh_jti();
    let material_jws = signer.compact(&base_claims(
        &board.host,
        &issuer.origin(),
        &intent_id,
        &changed_material_digest,
        &material_jti,
    ));
    let material_http = http_get_owner_read(&board, &board.host, &intent_id, &material_jws);
    assert_eq!(
        material_http.status, 403,
        "staged effect material mismatch must refuse owner read"
    );
    assert_eq!(publish_effects(&fx, &install), changed_material_effects);
    assert!(
        jti_hashes(&fx.state_dir()).contains(&cadence_agent::operator_auth::digest(&material_jti))
    );
    reproof_jtis.push(material_jti);
    assert_eq!(
        persist_prepared_descriptor(&fx, &intent_id, expected_descriptor.clone(), None),
        expected_digest
    );
    assert_eq!(publish_effects(&fx, &install), before);

    // The manifest in the currently completed bundle, not a caller-selected
    // descriptor field, owns the app identity returned by the read.
    let mut changed_app = expected_descriptor.clone();
    changed_app["app_id"] = json!("different-current-app");
    let changed_app_digest = persist_prepared_descriptor(&fx, &intent_id, changed_app, None);
    let changed_app_effects = publish_effects(&fx, &install);
    let app_jti = fresh_jti();
    let app_jws = signer.compact(&base_claims(
        &board.host,
        &issuer.origin(),
        &intent_id,
        &changed_app_digest,
        &app_jti,
    ));
    let app_http = http_get_owner_read(&board, &board.host, &intent_id, &app_jws);
    assert_eq!(
        app_http.status, 403,
        "descriptor app_id differing from current bundle must refuse"
    );
    assert_eq!(publish_effects(&fx, &install), changed_app_effects);
    assert!(jti_hashes(&fx.state_dir()).contains(&cadence_agent::operator_auth::digest(&app_jti)));
    reproof_jtis.push(app_jti);
    assert_eq!(
        persist_prepared_descriptor(&fx, &intent_id, expected_descriptor.clone(), None),
        expected_digest
    );
    assert_eq!(publish_effects(&fx, &install), before);

    // Structurally plausible persisted media keys are still bound to the
    // current AOS connection and exact image digest at read time.
    let image_digest = "a".repeat(64);
    let aos_connection = expected_descriptor["connection_id"]
        .as_str()
        .expect("descriptor AOS connection");
    let foreign_connection = if aos_connection == "foreign_connection" {
        "other_connection"
    } else {
        "foreign_connection"
    };
    let foreign_key = format!(
        "dp1.test_workspace.{foreign_connection}.{}",
        &image_digest[..32]
    );
    let mut foreign_media_descriptor = expected_descriptor.clone();
    foreign_media_descriptor["image_digest"] = json!(image_digest);
    let foreign_media_digest = persist_prepared_descriptor(
        &fx,
        &intent_id,
        foreign_media_descriptor,
        Some(&foreign_key),
    );
    let foreign_media_effects = publish_effects(&fx, &install);
    let foreign_media_jti = fresh_jti();
    let foreign_media_jws = signer.compact(&base_claims(
        &board.host,
        &issuer.origin(),
        &intent_id,
        &foreign_media_digest,
        &foreign_media_jti,
    ));
    let foreign_media_http =
        http_get_owner_read(&board, &board.host, &intent_id, &foreign_media_jws);
    assert_eq!(
        foreign_media_http.status, 403,
        "media key for another connection must refuse"
    );
    assert_eq!(publish_effects(&fx, &install), foreign_media_effects);
    assert!(jti_hashes(&fx.state_dir())
        .contains(&cadence_agent::operator_auth::digest(&foreign_media_jti)));
    reproof_jtis.push(foreign_media_jti);
    assert_eq!(
        persist_prepared_descriptor(&fx, &intent_id, expected_descriptor.clone(), None),
        expected_digest
    );
    assert_eq!(publish_effects(&fx, &install), before);

    let mismatched_digest_key = format!("dp1.test_workspace.{aos_connection}.{}", "b".repeat(32));
    let mut mismatched_digest_descriptor = expected_descriptor.clone();
    mismatched_digest_descriptor["image_digest"] = json!(image_digest);
    let mismatched_media_digest = persist_prepared_descriptor(
        &fx,
        &intent_id,
        mismatched_digest_descriptor,
        Some(&mismatched_digest_key),
    );
    let mismatched_media_effects = publish_effects(&fx, &install);
    let mismatched_media_jti = fresh_jti();
    let mismatched_media_jws = signer.compact(&base_claims(
        &board.host,
        &issuer.origin(),
        &intent_id,
        &mismatched_media_digest,
        &mismatched_media_jti,
    ));
    let mismatched_media_http =
        http_get_owner_read(&board, &board.host, &intent_id, &mismatched_media_jws);
    assert_eq!(
        mismatched_media_http.status, 403,
        "media key for another digest must refuse"
    );
    assert_eq!(publish_effects(&fx, &install), mismatched_media_effects);
    assert!(jti_hashes(&fx.state_dir())
        .contains(&cadence_agent::operator_auth::digest(&mismatched_media_jti)));
    reproof_jtis.push(mismatched_media_jti);
    assert_eq!(
        persist_prepared_descriptor(&fx, &intent_id, expected_descriptor.clone(), None),
        expected_digest
    );
    assert_eq!(publish_effects(&fx, &install), before);

    // No cookie or X-Cadence-Session is sent. The actual public-host route
    // diverts before the ordinary public-session gate, verifies the JWS with
    // the configured issuer's fetched key, and returns the bare descriptor.
    let valid_http = http_get_owner_read(&board, &board.host, &intent_id, &valid_jws);
    assert_eq!(
        valid_http.status, 200,
        "valid signed fixture read: {}",
        valid_http.body
    );
    assert!(valid_http
        .headers
        .get("cache-control")
        .is_some_and(|value| value.contains("no-store")));
    assert!(valid_http
        .headers
        .get("vary")
        .is_some_and(|value| value.to_ascii_lowercase().contains("authorization")));
    assert_eq!(
        valid_http
            .headers
            .get("x-content-type-options")
            .map(String::as_str),
        Some("nosniff")
    );
    let returned: Value = serde_json::from_str(&valid_http.body).expect("bare descriptor JSON");
    assert_eq!(returned, expected_descriptor);
    assert!(
        issuer.requests() >= 1,
        "daemon must fetch the configured issuer JWKS"
    );
    let valid_hash = cadence_agent::operator_auth::digest(&valid_jti);
    let durable_hashes = jti_hashes(&fx.state_dir());
    assert!(durable_hashes.contains(&valid_hash));
    assert!(durable_hashes.contains(&cadence_agent::operator_auth::digest(&jti_bad_digest)));
    assert!(durable_hashes.contains(&cadence_agent::operator_auth::digest(&jti_bad_digest_http)));
    for jti in &reproof_jtis {
        assert!(durable_hashes.contains(&cadence_agent::operator_auth::digest(jti)));
    }
    assert_eq!(
        durable_hashes.len(),
        3 + reproof_jtis.len(),
        "only correctly signed assertions burn JTI entries"
    );
    let ledger_path = cadence_agent::operator_auth::dir(&fx.state_dir()).join("sessions.json");
    let ledger_text = std::fs::read_to_string(&ledger_path).expect("durable JTI ledger");
    let ledger: Value = serde_json::from_str(&ledger_text).expect("operator ledger JSON");
    assert_eq!(
        ledger["sessions"],
        json!([]),
        "owner reads must not create board sessions"
    );
    assert!(
        !ledger_text.contains(&valid_jti),
        "raw JTI must not be persisted"
    );
    assert!(
        !ledger_text.contains(&jti_bad_digest),
        "raw refused-read JTI must not be persisted"
    );
    assert!(
        !ledger_text.contains(&jti_bad_digest_http),
        "raw HTTP-refusal JTI must not be persisted"
    );
    for jti in &reproof_jtis {
        assert!(
            !ledger_text.contains(jti),
            "raw re-proof refusal JTI must not be persisted"
        );
    }
    assert_eq!(publish_effects(&fx, &install), before);

    // This exact signed fixture assertion has now passed the actual READ route.
    // The explicit test-seam identity reaches attach only to exercise its
    // closed parameter guard; this does not claim process-ancestry/operator
    // proof or a live cross-repository AOS exchange.
    let read_purpose_attach = scoped(Asserted::Operator, || {
        client::rpc(
            &fx.state_dir(),
            "app_publish_intent_attach",
            json!({
                "prepared_id": intent_id,
                "install_id": install,
                "assertion": valid_jws.clone(),
                "purpose": PURPOSE,
            }),
        )
    })
    .expect_err("a READ-purpose assertion must not authorize attach");
    let read_purpose_attach_error = format!("{read_purpose_attach:?}");
    assert!(
        read_purpose_attach_error.contains("unsupported fields"),
        "attach must refuse READ-purpose fields before authorization: {read_purpose_attach_error}"
    );
    assert_eq!(
        jti_hashes(&fx.state_dir()),
        durable_hashes,
        "attach refusal must not consume or add a READ-purpose JTI"
    );
    assert_eq!(publish_effects(&fx, &install), before);

    // A daemon restart reloads the hashed JTI ledger. The identical signed
    // assertion remains refused through the actual public HTTP route.
    let hashes_before_restart = jti_hashes(&fx.state_dir());
    fx.restart_daemon();
    let replay_http = http_get_owner_read(&board, &board.host, &intent_id, &valid_jws);
    assert_eq!(
        replay_http.status, 403,
        "durably replayed assertion must be refused"
    );
    refusal(
        &fx,
        &intent_id,
        &valid_jws,
        "social intent assertion jti was already used",
    );
    assert_eq!(
        jti_hashes(&fx.state_dir()),
        hashes_before_restart,
        "replay adds no JTI record"
    );
    assert!(
        issuer.requests() >= 2,
        "restarted daemon must independently fetch JWKS"
    );
    assert_eq!(publish_effects(&fx, &install), before);
    assert_eq!(
        fx.media_stub.requests(),
        1,
        "owner reads must not repeat destination discovery"
    );

    board.stop();
    fx.stop_daemon();
}
