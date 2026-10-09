//! CAD-1290 acceptance check — written by the Spec/security reviewer
//! (opus-rev-spec-898), not the implementer. The implementer may not edit or
//! weaken it. A real in-process daemon and a real board over the
//! `--features test-seam` caller-identity harness; the company's destination
//! list is a stub lease door whose rows the test changes between calls.
//!
//! The rule it proves, from the ticket: only the operator chooses where the
//! install publishes, and only to one of the company's live, active
//! Instagram accounts.
//! (a) An agent caller, and a board session that is not the operator, cannot
//!     bind the publication slot through `app_binding_use_destination` or its
//!     board relay `POST /api/app-installations/<id>/publishing/use` — neither
//!     to create the binding nor to repoint an existing one.
//! (b) A destination that is not in the company's live active list (another
//!     company's id, a forged id, a connection that went away, a paused one)
//!     is refused even for the operator, over RPC and HTTP, and no binding is
//!     written or changed.
#![cfg(feature = "test-seam")]

use cadence_agent::platform::agenticos_external::media_import::MediaResolver;
use cadence_agent::platform::agenticos_external::publish_sender::DeviceCredential;
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon, operator_auth};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// This company's live Instagram accounts.
const MINE: &str = "17841400008460056";
const MINE_TOO: &str = "17841400008460057";
/// Live now, disconnected before it is chosen.
const GOING: &str = "17841400008460058";
/// Listed but not available (paused): publishable, so only the live-list
/// rule refuses it.
const PAUSED: &str = "17841400008460059";
/// Another company's account: its door never lists it here.
const OTHER_COMPANY: &str = "17841499999999999";

/// A board session: (cookie, `X-Cadence-Session` key).
type Session = (String, String);

fn row(id: &str, name: &str, available: bool) -> Value {
    json!({"connectionId": format!("c-{id}"), "toolkit": "instagram", "displayName": name,
        "destinationId": id, "status": "active", "available": available, "publishable": true})
}

struct Fx {
    root: tempfile::TempDir,
    rows: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    board: Option<(u16, Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
}

impl Fx {
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("c1290acc")
            .tempdir()
            .unwrap();
        cadence_agent::issue::Pm::init(&root.path().join("pm")).unwrap();
        let dir = root.path().join("s");
        std::fs::create_dir_all(&dir).unwrap();
        let rows = Arc::new(Mutex::new(vec![
            row(MINE, "@harbour", true),
            row(MINE_TOO, "@harbour.two", true),
            row(GOING, "@going", true),
            row(PAUSED, "@paused", false),
        ]));
        let stub = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = stub.server_addr().to_string();
        let served = Arc::clone(&rows);
        std::thread::spawn(move || {
            for request in stub.incoming_requests() {
                let body = json!({"ok": true, "data": *served.lock().unwrap()}).to_string();
                let _ = request.respond(tiny_http::Response::from_string(body));
            }
        });
        let resolver = MediaResolver::new(
            &format!("http://{addr}"),
            DeviceCredential::new("read-cred".into()),
        )
        .unwrap();
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", root.path().join("pm").to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
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
            social_media_resolver: Some(Arc::new(resolver)),
            ..Default::default()
        };
        cadence_agent::platform::local::register_at(
            &dir,
            &mut opts,
            dir.join("outbox"),
            "http://127.0.0.1:3010".into(),
        );
        let run_dir = dir.clone();
        let handle = std::thread::spawn(move || daemon::serve_with(&run_dir, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&dir, "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&dir).is_none()
        {
            assert!(
                !handle.is_finished() && std::time::Instant::now() < deadline,
                "daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut fx = Self {
            root,
            rows,
            stop,
            daemon: Some(handle),
            board: None,
        };
        fx.start_board();
        fx
    }
    fn dir(&self) -> std::path::PathBuf {
        self.root.path().join("s")
    }
    fn start_board(&mut self) {
        let free = |p: &u16| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok();
        let port = (3110..3200)
            .find(free)
            .expect("a free board port in 3110-3199");
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            startup: Some(tx),
            test_seam: true,
            ..Default::default()
        };
        let (dir, pm) = (self.dir(), self.root.path().join("pm"));
        let board = std::thread::spawn(move || {
            let _ = cadence_agent::ui::serve(&dir, &pm, &opts);
        });
        rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        self.board = Some((port, stop, board));
    }
    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let dir = self.dir();
        scoped(who, || client::rpc(&dir, method, params))
    }
    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }
    /// `POST <path>` on the board as `who` (seam headers), optionally with
    /// a board session (cookie, key). (status, reply, set-cookie).
    fn http_post(
        &self,
        who: &Asserted,
        path: &str,
        body: &Value,
        session: Option<&Session>,
    ) -> (u16, String, Option<String>) {
        let port = self.board.as_ref().unwrap().0;
        let host = format!("cadence-{port}.localhost:{port}");
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let mut request = agent
            .post(format!("http://127.0.0.1:{port}{path}"))
            .header("Host", &host)
            .header("Origin", format!("http://{host}"))
            .header("X-Cadence-Board", "1")
            .header("Content-Type", "application/json")
            .header(AS_HEADER, who.as_str())
            .header(TOKEN_HEADER, Seam::token_at(&self.dir()).unwrap());
        if let Some((cookie, key)) = session {
            request = request
                .header("Cookie", cookie)
                .header("X-Cadence-Session", key);
        }
        let mut response = request.send(body.to_string()).unwrap();
        let status = response.status().as_u16();
        let cookie = response
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap().to_string());
        (
            status,
            response.body_mut().read_to_string().unwrap(),
            cookie,
        )
    }
    /// The operator's real board session: a link minted from the operator
    /// secret, exchanged at `/api/session` for the cookie and session key.
    fn operator_session(&self) -> Session {
        operator_auth::ensure_secret(&self.dir()).unwrap();
        let secret = operator_auth::read_secret(&self.dir()).unwrap();
        let nonce = self.op(
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )["nonce"]
            .clone();
        let (status, reply, cookie) = self.http_post(
            &Asserted::Operator,
            "/api/session",
            &json!({"nonce": nonce}),
            None,
        );
        assert_eq!(status, 200, "operator session exchange failed: {reply}");
        let key = serde_json::from_str::<Value>(&reply).unwrap()["session_key"]
            .as_str()
            .unwrap()
            .to_string();
        (cookie.unwrap(), key)
    }
    fn install(&self) -> String {
        let source = self.root.path().join("app-src");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(
            source.join("app.md"),
            "---\napp: c1290acc\ntitle: C1290 acceptance\nversion: '0.1.0'\n\
             summary: Publication-slot fixture.\nneeds:\n  connections: []\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send\n---\n\n# C1290 acceptance\n",
        )
        .unwrap();
        std::fs::write(
            source.join("workflows/post.md"),
            "---\ntitle: \"Post\"\ngoal: \"One post\"\npublication_slot: publication\ninputs:\n  writer: { ask: \"writer\" }\n---\n\n## Write\nagent: {{writer}}\nsize: S\naction: local.text.produce\n\nWrite one post.\n\n### Acceptance\n- [ ] post exists\n",
        )
        .unwrap();
        self.op(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        )["install_id"]
            .as_str()
            .unwrap()
            .into()
    }
    /// What the store holds for the install: (binding id, revision,
    /// destination) of every binding, as the operator reads it.
    fn bindings(&self, install: &str) -> Vec<(String, i64, Value)> {
        self.op("app_binding_list", json!({"install_id": install}))["bindings"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|b| {
                (
                    b["id"].as_str().unwrap_or_default().to_string(),
                    b["revision"].as_i64().unwrap_or_default(),
                    b["config"]["publish"]["destination_id"].clone(),
                )
            })
            .collect()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        if let Some((_, stop, board)) = self.board.take() {
            stop.store(true, SeqCst);
            let _ = board.join();
        }
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.daemon.take() {
            let _ = handle.join();
        }
    }
}

fn use_body(destination: &str, revision: Option<i64>, request: &str) -> Value {
    let mut body = json!({"destination_id": destination});
    match revision {
        Some(rev) => body["expected_revision"] = json!(rev),
        None => body["request_id"] = json!(request),
    }
    body
}

fn rpc_params(install: &str, destination: &str, revision: Option<i64>, request: &str) -> Value {
    let mut params = use_body(destination, revision, request);
    params["install_id"] = json!(install);
    params
}

#[test]
fn only_the_operator_binds_and_only_to_a_live_company_destination() {
    let fx = Fx::start();
    let install = fx.install();
    let path = format!("/api/app-installations/{install}/publishing/use");
    let callers = [Asserted::Agent("cc13-pw".into()), Asserted::Unproven];
    // The operator's real board session. A non-operator caller is tried both
    // without a session (the board's own session check) and riding a fresh
    // operator session (a session ridden by an agent is revoked as stolen, so
    // each try mints its own).
    let refused_everywhere = |who: &Asserted, rpc: Value, body: Value, what: &str| {
        let result = fx.rpc(who.clone(), "app_binding_use_destination", rpc);
        let refusal = result.expect_err(&format!("{who:?} {what} over RPC"));
        assert!(
            refusal.to_string().contains("operator"),
            "{who:?} {what}: refused for another reason: {refusal}"
        );
        for carried in [None, Some(fx.operator_session())] {
            let (status, reply, _) = fx.http_post(who, &path, &body, carried.as_ref());
            assert_eq!(
                status,
                403,
                "{who:?} {what} over the board relay (session: {}): {reply}",
                carried.is_some()
            );
        }
    };

    // (a) Create: neither an agent nor a non-operator board session binds.
    for who in &callers {
        refused_everywhere(
            who,
            rpc_params(&install, MINE, None, "acc-a-rpc"),
            use_body(MINE, None, "acc-a-http"),
            "bound the slot",
        );
    }
    assert!(
        fx.bindings(&install).is_empty(),
        "a refused caller wrote a binding"
    );

    // (b) Create: a destination outside the live active list is refused even
    // for the operator, over RPC and HTTP, and nothing is written. GOING was
    // live, then disconnected before it is chosen.
    fx.rows
        .lock()
        .unwrap()
        .retain(|r| r["destinationId"] != GOING);
    let outside = [OTHER_COMPANY, "forged-destination", GOING, PAUSED];
    for (n, destination) in outside.into_iter().enumerate() {
        let rpc = fx.rpc(
            Asserted::Operator,
            "app_binding_use_destination",
            rpc_params(&install, destination, None, &format!("acc-b-rpc-{n}")),
        );
        let refusal = rpc.expect_err(&format!("operator bound {destination}"));
        assert!(
            refusal.to_string().contains("grant_binding_mismatch"),
            "operator bound {destination}: refused for another reason: {refusal}"
        );
        let (status, reply, _) = fx.http_post(
            &Asserted::Operator,
            &path,
            &use_body(destination, None, &format!("acc-b-http-{n}")),
            Some(&fx.operator_session()),
        );
        assert_eq!(
            status, 409,
            "operator bound {destination} over HTTP: {reply}"
        );
    }
    assert!(
        fx.bindings(&install).is_empty(),
        "a refused destination wrote a binding"
    );

    // Not vacuous: the relay reaches the daemon, and the operator's session
    // binds a live account of the company.
    let (status, reply, _) = fx.http_post(
        &Asserted::Operator,
        &path,
        &use_body(MINE, None, "acc-ok"),
        Some(&fx.operator_session()),
    );
    assert_eq!(
        status, 200,
        "the operator could not bind over HTTP: {reply}"
    );
    let bound = fx.bindings(&install);
    assert_eq!(bound.len(), 1, "{bound:?}");
    let (id, revision, destination) = bound[0].clone();
    assert_eq!(destination, json!(MINE));

    // (a) Replace: with the live revision in hand, an agent or a non-operator
    // board session still cannot repoint the binding.
    for who in &callers {
        refused_everywhere(
            who,
            rpc_params(&install, MINE_TOO, Some(revision), "unused"),
            use_body(MINE_TOO, Some(revision), "unused"),
            "repointed the binding",
        );
    }
    // (b) Replace: the operator cannot repoint to a destination outside the
    // live active list either.
    for destination in outside {
        let rpc = fx.rpc(
            Asserted::Operator,
            "app_binding_use_destination",
            rpc_params(&install, destination, Some(revision), "unused"),
        );
        let refusal = rpc.expect_err(&format!("operator repointed to {destination}"));
        assert!(
            refusal.to_string().contains("grant_binding_mismatch"),
            "operator repointed to {destination}: refused for another reason: {refusal}"
        );
        let (status, reply, _) = fx.http_post(
            &Asserted::Operator,
            &path,
            &use_body(destination, Some(revision), "unused"),
            Some(&fx.operator_session()),
        );
        assert_eq!(
            status, 409,
            "operator repointed to {destination} over HTTP: {reply}"
        );
    }
    assert_eq!(
        fx.bindings(&install),
        vec![(id, revision, json!(MINE))],
        "a refused call changed the binding"
    );
}
