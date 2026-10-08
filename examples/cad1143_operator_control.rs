//! CAD-1143 destinations-read and prepared-intent operator control
//! (external, independently authored by cc13-pi-acc793 — never the
//! implementer).
//!
//! A COMPLETE, ISOLATED fixture the operator runs explicitly — NOT an
//! `#[ignore]`d cargo test and never run in-band by the suite. It boots a
//! seam-armed daemon, exercises the destinations RPC/HTTP routes, then
//! prepares a real approved run and submits forged attach attempts. Fixture
//! setup uses an explicit seam; every guarded RPC call is unscoped, so
//! `proven_operator` must derive genuine caller ancestry from `SO_PEERCRED`
//! + `/proc`. This is distinct from the policy-seam test.
//!
//! Source only in this turn: this control has not been built or run.
//!
//! WHY SEPARATE: a managed test process is a tool of an enrolled/pane
//! endpoint — its own connections are only a literal NON-operator caller
//! (the negative). The positive needs a caller whose ancestry is clean of
//! every pane/endpoint, which only an actual operator shell provides; a
//! test cannot spawn that without detaching, and `proven_operator` refuses
//! detached callers anyway. So this binary is run BY the operator, and it
//! refuses (nonzero) when it cannot see a genuine operator-origin caller:
//! no `CADENCE_ALIAS`, no `CADENCE_RUNNER_ID`, no setsid/detach.
//!
//!   cargo run --features test-seam --example cad1143_operator_control
//!
//! Asserts: one validated destination row and one upstream GET per read;
//! the real operator prepare returns a stable prepared identity; an allowed
//! attach selector reaches the fail-closed verifier boundary, while forged
//! and replayed caller grant fields refuse with zero queued/authorized sends. The
//! prepared fixture intentionally writes a reviewed source run; the separate
//! destination install remains read-only.
#![cfg(feature = "test-seam")]

use cadence_agent::platform::agenticos_external::media_import::MediaResolver;
use cadence_agent::platform::agenticos_external::publish_sender::DeviceCredential;
use cadence_agent::store::{Store, PLATFORM_STREAM};
use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

const DEST: &str = "275491372109884";
const GRANT: &str = "dpq_destinations_fixture_grant";
const CAPTION: &str = "# Operator acceptance brief\nReviewed copy.";

/// The isolated fixture — identical shape to the acceptance test's `Fx`:
/// a seam-armed daemon on a temp dir + the real `MediaResolver` pointed
/// at a counting stub serving the strict version-1 envelope. `reads` is
/// the upstream-GET counter that proves the operator call reached it
/// exactly once (the refused cases prove 0).
struct Fx {
    root: tempfile::TempDir,
    clock: Arc<AtomicI64>,
    daemon: Option<(
        Arc<AtomicBool>,
        std::thread::JoinHandle<cadence_agent::Result<()>>,
    )>,
    store: Store,
    reads: Arc<AtomicU64>,
}

impl Fx {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("c1143opctl")
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
            reads: Arc::new(AtomicU64::new(0)),
        }
    }
    fn dir(&self) -> PathBuf {
        self.root.path().join("s")
    }
    fn pm(&self) -> PathBuf {
        self.root.path().join("pm")
    }
    fn start(&mut self) {
        let (dir, stop) = (self.dir(), Arc::new(AtomicBool::new(false)));
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", self.pm().to_str().unwrap());
        let clock = Arc::clone(&self.clock);
        // Counting upstream stub: strict canonical envelope, one row,
        // every GET bumps `reads`.
        let stub = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = stub.server_addr().to_string();
        let reads = Arc::clone(&self.reads);
        std::thread::spawn(move || {
            let body = json!({"ok": true, "data": {"version": "1", "destinations": [{
                "connectionId": "connA_harbour", "toolkit": "facebook",
                "displayName": "Harbour", "destinationId": DEST,
                "status": "active", "available": true, "publishable": true,
            }]}})
            .to_string();
            for request in stub.incoming_requests() {
                reads.fetch_add(1, SeqCst);
                let _ = request.respond(tiny_http::Response::from_string(body.clone()));
            }
        });
        let resolver = MediaResolver::new(
            &format!("http://{addr}"),
            DeviceCredential::new("read-cred".into()),
        )
        .unwrap();
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
            social_media_resolver: Some(Arc::new(resolver)),
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
        {
            assert!(
                !handle.is_finished() && std::time::Instant::now() < deadline,
                "daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        self.daemon = Some((stop, handle));
    }
    /// Business setup under one seam scope — install/bind/publish_set —
    /// dropped before the guarded call. Seam is for fixture bring-up only.
    fn setup(&self) -> String {
        scoped(Asserted::Operator, || self.install_scoped())
    }
    fn setup_rpc(&self, method: &str, params: Value) -> Value {
        client::rpc(&self.dir(), method, params).unwrap_or_else(|e| panic!("setup {method}: {e}"))
    }
    /// Build a second install with an actually completed/reviewed source run.
    /// The setup calls use the fixture seam; the guarded positive calls in
    /// `main` deliberately do not.
    fn setup_prepared_intent(&self) -> (String, String) {
        scoped(Asserted::Operator, || {
            self.install_prepared_fixture_scoped()
        })
    }
    fn install_prepared_fixture_scoped(&self) -> (String, String) {
        self.register_run_agents();
        let source = self.root.path().join("app-src-intent-control");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(
            source.join("app.md"),
            "---\napp: intent-op-control\ntitle: Intent operator control\nversion: '0.1.0'\n\\
             summary: Prepared-intent operator control.\nneeds:\n  connections: []\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send\n---\n\n# Intent operator control\n",
        )
        .unwrap();
        std::fs::write(source.join("workflows/brief.md"), WORKFLOW).unwrap();
        let installed = self.setup_rpc(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        );
        let install = installed["install_id"].as_str().unwrap().to_string();
        let bundle = installed["digest"].as_str().unwrap().to_string();
        self.setup_rpc(
            "app_install_team_set",
            json!({"install_id": install, "owner_pm": "lead",
                "roles": {"writer": "writer", "reviewer": "reviewer"},
                "expected_revision": 0}),
        );
        let bound = self.setup_rpc(
            "app_binding_create",
            json!({"install_id": install, "slot": "publication",
                "connection_id": self.local_connection(), "request_id": "bind-intent-control"}),
        );
        let binding = &bound["binding"];
        self.setup_rpc(
            "app_binding_publish_set",
            json!({"install_id": install, "binding_id": binding["id"],
                "expected_revision": binding["revision"],
                "destination_id": DEST, "destination_label": "Harbour",
                "toolkit": "facebook", "timezone": "Asia/Hong_Kong",
                "grant_id": GRANT}),
        );
        let run_id = self.complete_approved_run(&install, &bundle, "intent-op-run-01");
        (install, run_id)
    }
    fn register_run_agents(&self) {
        for (alias, role) in [("lead", "pm"), ("writer", "worker"), ("reviewer", "worker")] {
            if self.store.agent_opt(alias).unwrap().is_none() {
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
                    .unwrap();
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
                    .unwrap();
            }
        }
    }
    fn complete_approved_run(&self, install: &str, bundle: &str, request_id: &str) -> String {
        let started = self.setup_rpc(
            "app_run_start",
            json!({"install_id": install, "workflow": "brief", "request_id": request_id,
                "expected_quotes": {},
                "inputs": {"subject": "Operator control", "source": "Fixture facts."}}),
        );
        let run_id = started["id"].as_str().unwrap().to_string();
        let finish_step = |run: &Value, step: usize, reply: Value| {
            let message = run["steps"][step]["message_id"]
                .as_str()
                .unwrap()
                .to_string();
            let token = cadence_agent::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
            self.store.mark_running(&message, &token).unwrap();
            let msg = self.store.message(&message).unwrap().unwrap();
            let reply = json!({"turn_id": msg.turn_id, "text": reply.to_string()});
            self.store.finish(&msg, "completed", &reply, None).unwrap();
        };
        let run = self.store.app_run_dispatch(&run_id, bundle).unwrap();
        finish_step(
            &run,
            0,
            json!({"schema":1,"kind":"produce_text","run_id":run_id,"step_id":"s1","revision":1,
                "outcome":"succeeded","artifacts":[{"media_type":"text/markdown","text":CAPTION}]}),
        );
        let run = self.store.app_run_dispatch(&run_id, bundle).unwrap();
        let artifact_digest = cadence_agent::store::app_runs::artifact_digest(CAPTION.as_bytes());
        finish_step(
            &run,
            1,
            json!({"schema":1,"kind":"review_text","run_id":run_id,"step_id":"s2","revision":1,
                "producer_step_id":"s1","producer_revision":1,"artifact_sha256":artifact_digest,
                "decision":"approve","rationale":"Reviewed the exact artifact."}),
        );
        let shown = self.setup_rpc("app_run_show", json!({"run_id": run_id}));
        if shown["state"] != json!("succeeded") {
            eprintln!("prepared-intent fixture run did not succeed: {shown}");
            std::process::exit(2);
        }
        run_id
    }
    fn install_scoped(&self) -> String {
        let source = self.root.path().join("app-src-dest");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(
            source.join("app.md"),
            "---\napp: dest-accept\ntitle: Dest accept\nversion: '0.1.0'\n\
             summary: Destinations-read fixture.\nneeds:\n  connections: []\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send\n---\n\n# Dest accept\n",
        )
        .unwrap();
        std::fs::write(source.join("workflows/brief.md"), WORKFLOW).unwrap();
        let installed = self.setup_rpc(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        );
        let install = installed["install_id"].as_str().unwrap().to_string();
        let bound = self.setup_rpc(
            "app_binding_create",
            json!({"install_id": install, "slot": "publication",
                "connection_id": self.local_connection(), "request_id": "bind-dest"}),
        );
        let binding = &bound["binding"];
        self.setup_rpc(
            "app_binding_publish_set",
            json!({"install_id": install, "binding_id": binding["id"],
                "expected_revision": binding["revision"],
                "destination_id": DEST, "destination_label": "Harbour",
                "toolkit": "facebook", "timezone": "Asia/Hong_Kong",
                "grant_id": GRANT}),
        );
        install
    }
    fn local_connection(&self) -> String {
        let rows = self.setup_rpc("connection_list", json!({}))["connections"].clone();
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["provider"] == "local" && row["account"] == "local")
            .expect("local builtin connection")["id"]
            .as_str()
            .unwrap()
            .into()
    }
    /// "No write" proof — store-direct, never an operator RPC.
    fn intent_and_run_count(&self, install: &str) -> usize {
        let runs = self
            .store
            .app_run_list_filtered(Some(install), None)
            .unwrap()["runs"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0);
        let intents = self.store.social_publish_list(Some(install), None).unwrap()["intents"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0);
        runs + intents
    }
    fn queued_intent_count(&self, install: &str) -> usize {
        self.store.social_publish_list(Some(install), None).unwrap()["intents"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0)
    }
    fn authorized_event_count(&self) -> usize {
        self.store
            .events_tail(PLATFORM_STREAM, 200)
            .unwrap_or_default()
            .iter()
            .filter(|event| {
                event.kind == cadence_agent::store::social_publish::SOCIAL_PUBLISH_AUTHORIZED_EVENT
            })
            .count()
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

const WORKFLOW: &str = r#"---
title: "Dest brief"
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

/// Refuse (nonzero) unless this process is a genuine operator-origin
/// caller — a shell outside every pane/managed endpoint carrying no
/// managed-caller env. The daemon's `proven_operator` is the final
/// authority (it also rejects detached/setsid callers); this preflight
/// refuses the obvious managed marker so a pane run is never mistaken
/// for the operator control.
fn refuse_unless_operator_origin() {
    for var in ["CADENCE_ALIAS", "CADENCE_RUNNER_ID"] {
        if std::env::var_os(var).is_some() {
            eprintln!(
                "refusing: {var} is set — this is a managed/runner caller, not \
                 an operator-origin shell. Run from an attached operator shell \
                 outside every pane and managed endpoint."
            );
            std::process::exit(2);
        }
    }
}

#[cfg(feature = "test-seam")]
fn main() {
    refuse_unless_operator_origin();
    let mut fx = Fx::new();
    fx.start();
    let install = fx.setup();
    let before = fx.reads.load(SeqCst);

    // The REAL operator call: this process is operator-origin (checked
    // above + proven_operator for real on the daemon). Unscoped — no
    // seam assertion on the frame.
    let reply = client::rpc(
        &fx.dir(),
        "app_publish_destinations_list",
        json!({"install_id": install}),
    )
    .unwrap_or_else(|e| {
        eprintln!("operator destinations read refused: {e}");
        std::process::exit(2);
    });

    // Exactly one row, the validated 7-field shape.
    let rows = reply["destinations"].as_array().unwrap_or_else(|| {
        eprintln!("expected destinations array: {reply}");
        std::process::exit(2);
    });
    if rows.len() != 1 {
        eprintln!(
            "expected exactly 1 destination row, got {}: {reply}",
            rows.len()
        );
        std::process::exit(2);
    }
    let row = &rows[0];
    for field in [
        "connectionId",
        "toolkit",
        "displayName",
        "destinationId",
        "status",
        "available",
        "publishable",
    ] {
        if !row.get(field).is_some_and(|v| !v.is_null()) {
            eprintln!("row missing field {field}: {row}");
            std::process::exit(2);
        }
    }
    if row["destinationId"] != json!(DEST) {
        eprintln!("destinationId mismatch: {row}");
        std::process::exit(2);
    }
    // Exactly one upstream GET — the operator call reached the resolver
    // once (refused cases prove 0; this is the positive counterpart).
    if fx.reads.load(SeqCst) != before + 1 {
        eprintln!(
            "expected exactly one upstream read (before={before}, now={})",
            fx.reads.load(SeqCst)
        );
        std::process::exit(2);
    }
    // Read-only: no install-scoped runs/intents written.
    if fx.intent_and_run_count(&install) != 0 {
        eprintln!("operator read wrote install-scoped state");
        std::process::exit(2);
    }

    // -----------------------------------------------------------------
    // REAL OPERATOR-ORIGIN PREPARE + FORGED ATTACH NEGATIVE.
    // Fixture setup above is seam-scoped, but these guarded calls are
    // deliberately unscoped. The daemon's `proven_operator` must establish
    // this process's genuine operator ancestry; no seam assertion can pass
    // them. This is separate from the deterministic policy-seam acceptance.
    // -----------------------------------------------------------------
    let (publish_install, run_id) = fx.setup_prepared_intent();
    let queued0 = fx.queued_intent_count(&publish_install);
    let authorized0 = fx.authorized_event_count();
    if queued0 != 0 || authorized0 != 0 {
        eprintln!("prepared-intent fixture was not send-free before the control");
        std::process::exit(2);
    }
    let request = json!({"request_id": "intent-op-prepare-01", "run_id": run_id, "mode": "now"});
    let prepared = client::rpc(&fx.dir(), "app_publish_intent_prepare", request.clone())
        .unwrap_or_else(|e| {
            eprintln!("operator prepare refused: {e}");
            std::process::exit(2);
        });
    let prepared_id = prepared["prepared"]["prepared_id"]
        .as_str()
        .unwrap_or_else(|| {
            eprintln!("operator prepare returned no prepared_id: {prepared}");
            std::process::exit(2);
        })
        .to_string();
    if prepared["prepared"]["state"] != json!("prepared") {
        eprintln!("operator prepare did not return prepared state: {prepared}");
        std::process::exit(2);
    }
    let retry = client::rpc(&fx.dir(), "app_publish_intent_prepare", request).unwrap_or_else(|e| {
        eprintln!("operator prepare retry refused: {e}");
        std::process::exit(2);
    });
    if retry["prepared"]["prepared_id"] != json!(prepared_id)
        || retry["prepared"]["state"] != json!("prepared")
    {
        eprintln!("same-owner prepare retry changed identity/state: {retry}");
        std::process::exit(2);
    }
    if fx.queued_intent_count(&publish_install) != queued0 {
        eprintln!("prepare wrote a queued send intent");
        std::process::exit(2);
    }

    let allowed_attach = client::rpc(
        &fx.dir(),
        "app_publish_intent_attach",
        json!({"prepared_id": prepared_id, "install_id": publish_install}),
    );
    match allowed_attach {
        Err(error)
            if format!("{error:?}")
                .contains("capability_unavailable: publish intent authorization") => {}
        Err(error) => {
            eprintln!(
                "allowed operator attach missed the verifier fail-closed boundary: {error:?}"
            );
            std::process::exit(2);
        }
        Ok(reply) => {
            eprintln!("unwired attach unexpectedly succeeded: {reply}");
            std::process::exit(2);
        }
    }

    let forged = client::rpc(
        &fx.dir(),
        "app_publish_intent_attach",
        json!({"prepared_id": prepared_id, "install_id": publish_install,
            "grant_id": "dpq_forged_wellformed"}),
    );
    match forged {
        Err(error) if format!("{error:?}").contains("unsupported fields") => {}
        Err(error) => {
            eprintln!("forged grant was refused for the wrong reason: {error:?}");
            std::process::exit(2);
        }
        Ok(reply) => {
            eprintln!("caller-supplied grant unexpectedly attached: {reply}");
            std::process::exit(2);
        }
    }
    let replayed_forged = client::rpc(
        &fx.dir(),
        "app_publish_intent_attach",
        json!({"prepared_id": prepared_id, "install_id": publish_install,
            "grant_id": "dpq_forged_wellformed"}),
    );
    match replayed_forged {
        Err(error) if format!("{error:?}").contains("unsupported fields") => {}
        Err(error) => {
            eprintln!("replayed forged grant was refused for the wrong reason: {error:?}");
            std::process::exit(2);
        }
        Ok(reply) => {
            eprintln!("replayed caller-supplied grant unexpectedly attached: {reply}");
            std::process::exit(2);
        }
    }
    if fx.queued_intent_count(&publish_install) != queued0
        || fx.authorized_event_count() != authorized0
    {
        eprintln!("forged attach changed queued/authorized send state");
        std::process::exit(2);
    }

    // -----------------------------------------------------------------
    // LITERAL HTTP POSITIVE (real board session, no seam headers).
    //
    // Mint + open an operator session through the real link-exchange
    // (`operator_link_mint` needs the operator secret, same as `ui
    // login`; `operator_session_open` refuses an agent peer — this
    // process is operator-origin), then GET the board's read route with
    // the operator cookie + `X-Cadence-Session`. The board's
    // `admit_operator_read`→`board_caller`→`decide` sees a Held::Operator
    // session and admits it — the real operator proof over HTTP, not a
    // seam assertion. Asserts 200 + the one validated row + the same
    // upstream-read delta.
    // -----------------------------------------------------------------
    operator_session_http_positive(&fx, &install, before);

    println!("OK: operator destinations RPC+HTTP read passed; operator-origin prepare returned a stable prepared intent; forged/replayed attach refused with zero queued/authorized sends.");
    std::process::exit(0);
}

/// The literal HTTP positive: mint→open an operator session (real
/// link exchange, operator secret), then GET the read route with the
/// cookie + session key — `admit_operator_read` admits a Held::Operator
/// session on the real TCP-peer path (no seam headers). Returns the
/// status + body; the caller asserts 200 + the row + one more upstream
/// read.
fn operator_session_http_positive(fx: &Fx, install: &str, before: u64) {
    // 1) Operator secret (created by the daemon's own login plumbing on
    //    the isolated fixture's state dir; `ui login`'s exact read path).
    let secret = cadence_agent::operator_auth::read_secret(&fx.dir()).unwrap_or_else(|e| {
        eprintln!("read operator secret: {e}");
        std::process::exit(2);
    });
    // 2) Mint the single-use link nonce (operator-with-secret RPC — the
    //    same call `ui login` makes; this caller is operator-origin so
    //    `operator_with_secret`→`proven_operator` passes for real).
    let nonce = client::rpc(
        &fx.dir(),
        "operator_link_mint",
        json!({"secret": secret, "origin": "loopback"}),
    )
    .unwrap_or_else(|e| {
        eprintln!("operator_link_mint refused: {e}");
        std::process::exit(2);
    })["nonce"]
        .as_str()
        .unwrap_or_else(|| {
            eprintln!("no nonce in link mint");
            std::process::exit(2);
        })
        .to_string();
    // 3) Exchange the nonce for a session token + key
    //    (operator_session_open refuses an agent peer — real check).
    let opened = client::rpc(
        &fx.dir(),
        "operator_session_open",
        json!({"nonce": nonce, "origin": "loopback", "user_agent": "opctl"}),
    )
    .unwrap_or_else(|e| {
        eprintln!("operator_session_open refused: {e}");
        std::process::exit(2);
    });
    let token = opened["token"].as_str().unwrap().to_string();
    let key = opened["key"].as_str().unwrap().to_string();

    // 4) GET the read route with the operator cookie + session key over
    //    a raw TCP request to a seam-armed board. NO seam headers — the
    //    operator session is the real credential.
    let path = format!("/api/app-installations/{install}/publish-destinations");
    let (status, body) = http_get_with_session(&fx.dir(), &path, &token, &key);
    if status != 200 {
        eprintln!("operator HTTP destinations read returned {status}: {body}");
        std::process::exit(2);
    }
    let data: Value = serde_json::from_str(body.split("\r\n\r\n").nth(1).unwrap_or("{}"))
        .unwrap_or_else(|_| {
            eprintln!("operator HTTP body not JSON: {body}");
            std::process::exit(2);
        });
    if data["destinations"].as_array().map(Vec::len) != Some(1) {
        eprintln!("operator HTTP read returned wrong set: {data}");
        std::process::exit(2);
    }
    if fx.reads.load(SeqCst) != before + 2 {
        eprintln!(
            "expected RPC+HTTP to make exactly 2 upstream reads (now={})",
            fx.reads.load(SeqCst)
        );
        std::process::exit(2);
    }
}

/// One GET to the board with an operator session — cookie + the
/// `X-Cadence-Session` key (never a seam header). Real TCP, real session,
/// real peer attribution.
fn http_get_with_session(dir: &Path, path: &str, cookie: &str, key: &str) -> (u16, String) {
    let free = |p: &u16| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok();
    let port = (3110..3200).find(free).unwrap();
    let (startup, ready) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let opts = cadence_agent::ui::ServeOpts {
        host: "127.0.0.1".into(),
        port,
        stop: Some(stop.clone()),
        startup: Some(startup),
        test_seam: true,
        ..Default::default()
    };
    let (dir_owned, pm) = (dir.to_path_buf(), dir.join("pm"));
    let board = std::thread::spawn(move || drop(cadence_agent::ui::serve(&dir_owned, &pm, &opts)));
    ready
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let host = format!("cadence-{port}.localhost:{port}");
    // Cookie name matches `ui/operator.rs cookie_name` for Loopback:
    // `cadence_operator_{port}` (the `__Host-` prefix is only used on
    // https origins — tailnet and public). The session key rides in
    // `X-Cadence-Session`, the same header the SPA sends on writes.
    let req = format!(
        "GET {path} HTTP/1.0\r\nHost: {host}\r\nX-Cadence-Board: 1\r\n\
         Sec-Fetch-Site: same-origin\r\nOrigin: http://{host}\r\n\
         Cookie: cadence_operator_{port}={cookie}\r\nX-Cadence-Session: {key}\r\n\r\n"
    );
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    std::io::Write::write_all(&mut s, req.as_bytes()).unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut s, &mut text).unwrap();
    stop.store(true, SeqCst);
    let _ = board.join();
    (
        text.split_whitespace().nth(1).unwrap().parse().unwrap(),
        text,
    )
}

#[cfg(not(feature = "test-seam"))]
fn main() {
    eprintln!(
        "build with `--features test-seam`: cargo run --features test-seam \
         --example cad1143_operator_control"
    );
    std::process::exit(2);
}
