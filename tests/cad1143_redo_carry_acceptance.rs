//! CAD-1143 Redo carry acceptance — written by the independent
//! acceptance author (cc13-pi-acc793), not the carry implementer, who may
//! not edit or weaken this file. From the ticket (CAD-1143/CAD-1138): a
//! trusted Redo tap preserves exact same-scope reviewed content and never
//! turns another installation's bytes into target-run material.
//!
//! Real guard under test (no mirror predicate): the daemon carry scope
//! check — `Shared::start_app_run`'s pre-scope refusal plus the
//! authoritative re-proof in `Store::app_carry_material_in`
//! ("carry source is outside this installation and context") —
//! exercised through the real `app_run_start` RPC and through the
//! board's relay `POST /api/app-runs/start` with an operator session
//! minted the real way. One valid same-scope carry control proves the
//! refusal is not vacuous (no missing team/workflow/quote/receipt), and
//! every refusal asserts zero target run rows.
//!
//! UNEXECUTED: code-only turn per kickoff — no test/build run yet; the
//! full isolated gate must still judge this file.
#![cfg(feature = "test-seam")]

use cadence_agent::store::Store;
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SUBJECT: &str = "Renewal";
const FACTS: &str = "Plan renews 1 July. Price stays HK$88/month.";
const CAPTION: &str = "# Carry source\nReviewed copy.";

type DaemonHandle = (
    Arc<AtomicBool>,
    std::thread::JoinHandle<cadence_agent::Result<()>>,
);

struct Fx {
    root: tempfile::TempDir,
    clock: Arc<AtomicI64>,
    daemon: Option<DaemonHandle>,
    store: Store,
}

impl Fx {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("c1143carryacc")
            .tempdir()
            .unwrap();
        cadence_agent::issue::Pm::init(&root.path().join("pm")).unwrap();
        let clock = Arc::new(AtomicI64::new(1_800_000_000));
        std::fs::create_dir_all(root.path().join("s")).unwrap();
        let store = Store::open(&root.path().join("s").join("cadence.sqlite3")).unwrap();
        Self {
            root,
            clock,
            daemon: None,
            store,
        }
    }
    fn dir(&self) -> std::path::PathBuf {
        self.root.path().join("s")
    }
    fn pm(&self) -> std::path::PathBuf {
        self.root.path().join("pm")
    }
    fn start(&mut self) {
        let (dir, stop) = (self.dir(), Arc::new(AtomicBool::new(false)));
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", self.pm().to_str().unwrap());
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
        while client::rpc_timeout(&self.dir(), "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&self.dir()).is_none()
        {
            assert!(
                !handle.is_finished() && std::time::Instant::now() < deadline,
                "daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        self.daemon = Some((stop, handle));
    }
    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(who, || client::rpc(&self.dir(), method, params))
    }
    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }
    /// Two workspace installs of the same carry workflow shape under
    /// different app names, each with its installation team. Returns
    /// `(install_id, bundle_digest)`.
    fn install(&self, tag: &str) -> (String, String) {
        let store = &self.store;
        for (alias, role) in [("lead", "pm"), ("writer", "worker"), ("reviewer", "worker")] {
            if store.agent_opt(alias).unwrap().is_none() {
                store
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
                    .unwrap();
                store
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
                    .unwrap();
            }
        }
        let source = self.root.path().join(format!("app-src-{tag}"));
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(
            source.join("app.md"),
            format!(
                "---\napp: carry-{tag}\ntitle: Carry {tag}\nversion: '0.1.0'\n\
                 summary: Redo-carry fixture.\nneeds:\n  connections: []\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send\n---\n\n# Carry {tag}\n"
            ),
        )
        .unwrap();
        std::fs::write(source.join("workflows/brief.md"), BRIEF_WORKFLOW).unwrap();
        std::fs::write(source.join("workflows/redo.md"), REDO_WORKFLOW).unwrap();
        let installed = self.op(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        );
        let install = installed["install_id"].as_str().unwrap().to_string();
        self.op(
            "app_install_team_set",
            json!({"install_id": install, "owner_pm": "lead",
                "roles": {"writer": "writer", "reviewer": "reviewer"},
                "expected_revision": 0}),
        );
        (install, installed["digest"].as_str().unwrap().to_string())
    }
    /// One completed, approved source run on the non-carry brief workflow
    /// through the real `app_run_start` path, its two worker turns
    /// completed through the store's own turn path — the same mark_running
    /// + finish the daemon's actor performs.
    fn complete_source_run(&self, install: &str, bundle: &str, tag: &str) -> String {
        let store = &self.store;
        let started = self.op(
            "app_run_start",
            json!({"install_id": install, "workflow": "brief",
                "request_id": format!("carry-src-{tag}"), "expected_quotes": {},
                "inputs": {"subject": SUBJECT, "source": FACTS}}),
        );
        let run_id = started["id"].as_str().unwrap().to_string();
        let finish_step = |run: &Value, step: usize, reply: Value| {
            let message = run["steps"][step]["message_id"]
                .as_str()
                .unwrap()
                .to_string();
            let token = cadence_agent::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
            store.mark_running(&message, &token).unwrap();
            let msg = store.message(&message).unwrap().unwrap();
            let reply = json!({"turn_id": msg.turn_id, "text": reply.to_string()});
            store.finish(&msg, "completed", &reply, None).unwrap();
        };
        let run = store.app_run_dispatch(&run_id, bundle).unwrap();
        finish_step(
            &run,
            0,
            json!({"schema":1,"kind":"produce_text","run_id":run_id,"step_id":"s1","revision":1,
                "outcome":"succeeded","artifacts":[{"media_type":"text/markdown","text":CAPTION}]}),
        );
        let run = store.app_run_dispatch(&run_id, bundle).unwrap();
        let digest = cadence_agent::store::app_runs::artifact_digest(CAPTION.as_bytes());
        finish_step(
            &run,
            1,
            json!({"schema":1,"kind":"review_text","run_id":run_id,"step_id":"s2","revision":1,
                "producer_step_id":"s1","producer_revision":1,"artifact_sha256":digest,
                "decision":"approve","rationale":"Checked the exact artifact."}),
        );
        let shown = self.op("app_run_show", json!({"run_id": run_id}));
        assert_eq!(
            shown["state"], "succeeded",
            "source run never completed: {shown}"
        );
        assert_eq!(
            shown["snapshot"]["inputs"]["source"],
            json!(FACTS),
            "source run froze no facts: {shown}"
        );
        run_id
    }
    fn runs_of(&self, install: &str) -> usize {
        self.op("app_run_list", json!({"install_id": install}))["runs"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0)
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        if let Some((stop, handle)) = self.daemon.take() {
            stop.store(true, SeqCst);
            let _ = handle.join();
        }
    }
}

/// Source runs are created on this non-carry workflow: it declares no
/// `carries:` half, so it starts carry-free and freezes the facts the
/// redo target later retains.
const BRIEF_WORKFLOW: &str = r#"---
title: "Carry brief"
goal: "One reviewed brief with frozen facts"
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

/// The redo target: declares `carries: [text]` both ways with the daemon
/// (a carry needs this half declared, and this workflow never starts
/// without a carry source), with the daemon-seeded `carry_caption` input
/// carrying the retained caption into the render.
const REDO_WORKFLOW: &str = r#"---
title: "Redo brief"
goal: "Reissue the reviewed caption exactly"
carries: [text]
inputs:
  writer: { ask: "writer" }
  reviewer: { ask: "reviewer" }
  subject: { ask: "subject" }
  source: { ask: "facts" }
  carry_caption: { ask: "retained caption" }
---

## Write
agent: {{writer}}
size: S
action: local.text.produce

Reissue {{subject}} preserving exactly: {{carry_caption}}

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

/// CAD-1143: a carry naming another installation's run is refused by the
/// real daemon scope guard — over RPC and over the board's HTTP relay —
/// with zero target run rows. Control: the same-scope carry succeeds and
/// preserves the exact frozen facts.
#[test]
fn foreign_install_carry_is_refused_and_same_scope_carry_preserves_content() {
    let mut fx = Fx::new();
    fx.start();
    let (install_a, bundle_a) = fx.install("a");
    let (install_b, bundle_b) = fx.install("b");
    let source_a = fx.complete_source_run(&install_a, &bundle_a, "a");
    let source_b = fx.complete_source_run(&install_b, &bundle_b, "b");
    // Control first: the same-scope carry is served and keeps the exact
    // frozen facts — so the refusals below cannot pass vacuously.
    let carried = fx
        .rpc(
            Asserted::Operator,
            "app_run_start",
            json!({"install_id": install_a, "workflow": "redo",
                "request_id": "carry-ctl-1", "expected_quotes": {}, "inputs": {},
                "carry": {"from_run_id": source_a, "retain": "text"}}),
        )
        .expect("control: same-scope text carry must be served");
    assert_eq!(
        carried["snapshot"]["carry"]["from_run_id"],
        json!(source_a),
        "control carry froze no linkage: {carried}"
    );
    assert_eq!(
        carried["snapshot"]["inputs"]["source"],
        json!(FACTS),
        "control carry lost the exact frozen facts: {carried}"
    );
    let after_control = fx.runs_of(&install_a);
    assert_eq!(after_control, 2, "control carry wrote no target run");
    // The foreign-install carry is refused by the real guard over RPC.
    let err = fx
        .rpc(
            Asserted::Operator,
            "app_run_start",
            json!({"install_id": install_a, "workflow": "redo",
                "request_id": "carry-frg-rpc-1", "expected_quotes": {}, "inputs": {},
                "carry": {"from_run_id": source_b, "retain": "text"}}),
        )
        .map(|_| ())
        .unwrap_err();
    assert!(
        format!("{err}").contains("outside this installation and context"),
        "foreign carry refused by the wrong guard: {err}"
    );
    assert_eq!(
        fx.runs_of(&install_a),
        after_control,
        "a refused foreign carry wrote a target run row"
    );
    // The same refusal through the board's HTTP relay, operator session
    // minted the real way — the relay is at least as strict as the RPC.
    let board = Board::start(&fx.dir(), &fx.pm());
    let (status, body) = board.call(
        "operator",
        "POST",
        "/api/app-runs/start",
        json!({"install_id": install_a, "workflow": "redo",
            "request_id": "carry-frg-http-1", "expected_quotes": {}, "inputs": {},
            "carry": {"from_run_id": source_b, "retain": "text"}}),
    );
    assert!(
        status != 200 && (400..600).contains(&status),
        "foreign carry reached the provider path over HTTP: {status} {body}"
    );
    assert!(
        body.to_string()
            .contains("outside this installation and context"),
        "HTTP refused by the wrong guard: {status} {body}"
    );
    assert_eq!(
        fx.runs_of(&install_a),
        after_control,
        "a refused HTTP foreign carry wrote a target run row"
    );
}

// --------------------------------------------------------------------
// The board's HTTP surface: seam daemon fixture + operator session
// minted through `operator_link_mint` + `POST /api/session`, mirroring
// the in-crate app_runs_rpc HTTP harness.
// --------------------------------------------------------------------

struct Board {
    agent: ureq::Agent,
    base: String,
    host: String,
    token: String,
    cookie: String,
    key: String,
    _stop: BoardStop,
}

struct BoardStop(Arc<AtomicBool>, Mutex<Vec<std::thread::JoinHandle<()>>>);

impl Board {
    fn start(state: &std::path::Path, pm: &std::path::Path) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let mut port = 3110 + (std::process::id() % 80) as u16;
        let server = loop {
            let (startup, ready) = std::sync::mpsc::channel();
            let opts = cadence_agent::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(stop.clone()),
                startup: Some(startup),
                test_seam: true,
                ..Default::default()
            };
            let (state, pm) = (state.to_path_buf(), pm.to_path_buf());
            let thread =
                std::thread::spawn(move || drop(cadence_agent::ui::serve(&state, &pm, &opts)));
            match ready.recv_timeout(Duration::from_secs(30)).unwrap() {
                Ok(()) => break thread,
                Err(_) if port < 3199 => {
                    port += 1;
                    thread.join().unwrap();
                }
                Err(kind) => panic!("board could not bind: {kind:?}"),
            }
        };
        let token = Seam::token_at(state).unwrap();
        let host = format!("cadence-{port}.localhost:{port}");
        cadence_agent::operator_auth::ensure_secret(state).unwrap();
        let secret = cadence_agent::operator_auth::read_secret(state).unwrap();
        let nonce = scoped(Asserted::Operator, || {
            client::rpc(
                state,
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
        })
        .unwrap()["nonce"]
            .clone();
        let config = ureq::Agent::config_builder().http_status_as_error(false);
        let agent: ureq::Agent = config.build().into();
        let base = format!("http://127.0.0.1:{port}");
        let session = agent
            .post(format!("{base}/api/session"))
            .header("Host", &host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{host}"))
            .header(AS_HEADER, "operator")
            .header(TOKEN_HEADER, &token)
            .header("Content-Type", "application/json")
            .send(json!({"nonce": nonce}).to_string())
            .unwrap();
        let set = session.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .to_string();
        let cookie = set[..set.find(';').unwrap()].to_owned();
        let key: Value = session.into_body().read_json().unwrap();
        Board {
            agent,
            base,
            host,
            token,
            cookie,
            key: key["session_key"].as_str().unwrap().to_string(),
            _stop: BoardStop(stop, Mutex::new(vec![server])),
        }
    }
    fn call(&self, who: &str, method: &str, path: &str, body: Value) -> (u16, Value) {
        let url = format!("{}{}", self.base, path);
        let mut req = self
            .agent
            .post(&url)
            .header("Host", &self.host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{}", self.host))
            .header(AS_HEADER, who)
            .header(TOKEN_HEADER, &self.token)
            .header("Content-Type", "application/json");
        if who == "operator" {
            req = req
                .header("Cookie", &self.cookie)
                .header("X-Cadence-Session", &self.key);
        }
        let _ = method;
        let response = req.send(body.to_string()).unwrap();
        let status = response.status().as_u16();
        let mut response = response;
        (
            status,
            response.body_mut().read_json().unwrap_or(Value::Null),
        )
    }
}

impl Drop for BoardStop {
    fn drop(&mut self) {
        self.0.store(true, SeqCst);
        if let Ok(guard) = self.1.get_mut() {
            for handle in guard.drain(..) {
                let _ = handle.join();
            }
        }
    }
}
