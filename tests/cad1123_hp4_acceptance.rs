//! CAD-1123 HP4 acceptance check — written by the reviewer
//! (cc13-pi-acc793), not the implementer. A real in-process daemon and
//! board over the `--features test-seam` caller-identity harness; the
//! provider door is a counting `PublishSender` over the fake ledger and
//! the local→AOS `connectionId` map a stub HTTP resolver. Each test
//! names the ticket outcome it proves, from the plan (CAD-1123 HP4) —
//! not from the code: one tap is one provider call; only the operator
//! publishes, schedules or binds; the atomic reschedule never leaves a
//! scheduleless window; and the destination always derives from the
//! binding's `config.publish`, never the request.
#![cfg(feature = "test-seam")]

use cadence_agent::platform::agenticos_external::media_import::MediaResolver;
use cadence_agent::platform::agenticos_external::publish::{self as door, FakeProviderBehavior};
use cadence_agent::platform::agenticos_external::publish::{
    LedgerOutcome, Preflight, PublishSender, Refusal,
};
use cadence_agent::platform::agenticos_external::publish_sender::DeviceCredential;
use cadence_agent::store::app_runs::{LocalRunRequest, LocalWorkflow};
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon, store::Store};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const CONN: &str = "connA_harbour_fb";
const DEST: &str = "275491372109884";
const GRANT: &str = "dpq_hp4_synthetic_grant";
const NOW: i64 = 1_800_000_000;

type DaemonHandle = (
    Arc<AtomicBool>,
    std::thread::JoinHandle<cadence_agent::Result<()>>,
);

/// The provider door: a counting `PublishSender` over the fake ledger.
/// `execs` is the count that proves exactly-once; the staging
/// rendezvous lets two presses meet inside preflight before either
/// claims.
struct Door {
    ledger: door::FakePublishLedger,
    grant: Mutex<door::SendGrant>,
    execs: AtomicU64,
    meet: Option<(Mutex<u32>, Condvar)>,
}

fn destination() -> door::Destination {
    door::Destination {
        connection_id: CONN.into(),
        toolkit: door::Toolkit::Facebook,
        display_name: "Harbour".into(),
        destination_id: DEST.into(),
        status_active: true,
        available: true,
    }
}

impl PublishSender for Door {
    fn preflight(&self, b: &door::SendBinding) -> Preflight {
        if let Some((count, cv)) = &self.meet {
            *count.lock().unwrap() += 1;
            cv.notify_all();
            let met =
                cv.wait_timeout_while(count.lock().unwrap(), Duration::from_secs(20), |n| *n < 2);
            assert_eq!(*met.unwrap().0, 2, "the presses never overlapped");
        }
        let grant = self.grant.lock().unwrap();
        match self.ledger.preflight(b, &destination(), &grant, "ws", NOW) {
            Ok(_) => Preflight::Approved,
            Err(refusal) => Preflight::Refused(refusal),
        }
    }
    fn execute(&self, b: &door::SendBinding) -> Result<LedgerOutcome, Refusal> {
        self.execs.fetch_add(1, SeqCst);
        let mut grant = self.grant.lock().unwrap();
        self.ledger.execute(
            b,
            &destination(),
            &mut grant,
            "ws",
            NOW,
            FakeProviderBehavior::Post,
        )
    }
    fn status(&self, key: &str) -> Result<LedgerOutcome, Refusal> {
        self.ledger.status(key)
    }
}

/// One seam-armed daemon on a temp dir, a stub resolver, a counting
/// door and a PM dir the workspace install writes into.
struct Fx {
    root: tempfile::TempDir,
    door: Arc<Door>,
    clock: Arc<AtomicI64>,
    daemon: Option<DaemonHandle>,
    resolver_addr: Arc<Mutex<Option<String>>>,
    /// The one store handle the test opens: `Store::open` clears every
    /// agent's runtime generation (crash-path reset), so a second open
    /// after `set_identity` would fence the tokens the run turns need.
    store: Store,
}

impl Fx {
    fn new(meet: bool) -> Self {
        let grant = door::SendGrant {
            id: GRANT.into(),
            workspace_id: "ws".into(),
            connection_id: CONN.into(),
            destination_id: DEST.into(),
            toolkit: door::Toolkit::Facebook,
            caption_digest: caption(),
            image_digest: None,
            cadence_approval_id: "appr-hp4".into(),
            max_uses: 10,
            remaining_uses: 10,
            revoked: false,
            not_before_epoch: 0,
            expires_at_epoch: i64::MAX,
        };
        let door = Arc::new(Door {
            ledger: door::FakePublishLedger::enabled(),
            grant: Mutex::new(grant),
            execs: AtomicU64::new(0),
            meet: meet.then(Default::default),
        });
        let root = tempfile::Builder::new()
            .prefix("c1123hp4acc")
            .tempdir()
            .unwrap();
        cadence_agent::issue::Pm::init(&root.path().join("pm")).unwrap();
        let clock = Arc::new(AtomicI64::new(NOW));
        std::fs::create_dir_all(root.path().join("s")).unwrap();
        let store = Store::open(&root.path().join("s").join("cadence.sqlite3")).unwrap();
        Self {
            root,
            door,
            clock,
            daemon: None,
            resolver_addr: Arc::new(Mutex::new(None)),
            store,
        }
    }
    fn dir(&self) -> std::path::PathBuf {
        self.root.path().join("s")
    }
    fn pm(&self) -> std::path::PathBuf {
        self.root.path().join("pm")
    }
    /// Serve the one publishable destination row forever; records the
    /// bound address for the resolver the daemon is configured with.
    fn arm_resolver(&self) {
        let stub = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = stub.server_addr().to_string();
        let body = json!({"ok": true, "data": [{
            "connectionId": CONN, "toolkit": "facebook",
            "displayName": "Harbour", "destinationId": DEST,
            "status": "active", "available": true, "publishable": true,
        }]})
        .to_string();
        std::thread::spawn(move || {
            for request in stub.incoming_requests() {
                let _ = request.respond(tiny_http::Response::from_string(body.clone()));
            }
        });
        *self.resolver_addr.lock().unwrap() = Some(addr);
    }
    fn start(&mut self) {
        let (dir, stop) = (self.dir(), Arc::new(AtomicBool::new(false)));
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", self.pm().to_str().unwrap());
        let clock = Arc::clone(&self.clock);
        let resolver = MediaResolver::new(
            &format!(
                "http://{}",
                self.resolver_addr.lock().unwrap().as_deref().unwrap()
            ),
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
            social_publish_sender: Some(self.door.clone()),
            social_media_resolver: Some(Arc::new(resolver)),
            ..Default::default()
        };
        // The `local` platform carries `text.publish` — the publication
        // slot's provider-neutral mapping.
        cadence_agent::platform::local::register_at(
            &dir,
            &mut opts,
            dir.join("outbox"),
            "http://127.0.0.1:3010".into(),
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
    fn stop(&mut self) {
        if let Some((stop, handle)) = self.daemon.take() {
            stop.store(true, SeqCst);
            let _ = handle.join();
        }
    }
    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(who, || client::rpc(&self.dir(), method, params))
    }
    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }
    /// The workspace install carrying a `publication` slot bound to
    /// `local`, with the operator's publish settings recorded on the
    /// binding — the chain the RPCs themselves perform: install → bind →
    /// publish_set. Returns `(install_id, binding_id, bundle_digest)`.
    fn install(&self) -> (String, String, String) {
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
                let identity = cadence_agent::adapter::Identity {
                    thread_id: "t".into(),
                    session_id: "s".into(),
                    model: None,
                    effort: None,
                    pid: std::process::id(),
                    endpoint: None,
                    generation: Some("g1".into()),
                    attach: None,
                };
                store.set_identity(alias, &identity).unwrap();
            }
        }
        let source = self.root.path().join("app-src");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(
            source.join("app.md"),
            "---\napp: hp4-accept\ntitle: HP4 accept\nversion: '0.1.0'\n\
             summary: Publication-slot fixture.\nneeds:\n  connections: []\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send\n---\n\n# HP4 accept\n",
        )
        .unwrap();
        std::fs::write(source.join("workflows/post.md"), WORKFLOW).unwrap();
        let installed = self.op(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        );
        let install = installed["install_id"].as_str().unwrap().to_string();
        let bound = self.op(
            "app_binding_create",
            json!({"install_id": install, "slot": "publication",
                "connection_id": self.local_connection(), "request_id": "bind-hp4"}),
        );
        let binding = &bound["binding"];
        // The operator records the publish settings once — destination,
        // toolkit, timezone and grant derive only from this binding.
        let set = self.op(
            "app_binding_publish_set",
            json!({"install_id": install, "binding_id": binding["id"],
                "expected_revision": binding["revision"],
                "destination_id": DEST, "destination_label": "Harbour",
                "toolkit": "facebook", "timezone": "Asia/Hong_Kong",
                "grant_id": GRANT}),
        );
        assert!(set["binding"]["config"]["publish"].is_object(), "{set}");
        (
            install,
            binding["id"].as_str().unwrap().to_string(),
            installed["digest"].as_str().unwrap().to_string(),
        )
    }
    fn local_connection(&self) -> String {
        let rows = self.op("connection_list", json!({}))["connections"].clone();
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["provider"] == "local" && row["account"] == "local")
            .expect("local builtin connection")["id"]
            .as_str()
            .unwrap()
            .into()
    }
    /// One reviewed run on the publication slot, completed by its two
    /// assigned workers through the store's own turn path
    /// (`app_run_dispatch` → `mark_running` → `finish`). `bundle` is the
    /// install's real digest; the live `app_artifact` effect the freeze
    /// requires is staged by `social_publish_start` itself.
    fn approved_run(&self, install: &str, bundle: &str, tag: &str) -> String {
        let store = &self.store;
        let inputs = BTreeMap::from([
            ("writer".to_string(), "writer".to_string()),
            ("reviewer".to_string(), "reviewer".to_string()),
        ]);
        let workflow = LocalWorkflow::parse(WORKFLOW, &inputs).unwrap();
        let binding = store
            .app_binding_for_slot(install, None, "publication", bundle)
            .unwrap();
        let run = store
            .app_run_create_with_publication(
                LocalRunRequest {
                    install_id: install,
                    bundle_digest: bundle,
                    workflow: &workflow,
                    inputs: &inputs,
                    request_id: &format!("run-{tag}"),
                    owner_pm: "lead",
                    project_link: None,
                },
                None,
                binding.as_ref(),
            )
            .unwrap();
        let run_id = run["id"].as_str().unwrap().to_string();
        store
            .app_run_decide(
                &run_id,
                run["snapshot_digest"].as_str(),
                false,
                Some(bundle),
            )
            .unwrap();
        // One constant caption so the door's send grant — bound to its
        // digest — authorizes every run's frozen caption.
        let body = "# Post hp4\nReviewed copy.";
        // One dispatched step claimed and completed by its assignee —
        // the same mark_running + finish the daemon's actor performs.
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
                "outcome":"succeeded","artifacts":[{"media_type":"text/markdown","text":body}]}),
        );
        let run = store.app_run_dispatch(&run_id, bundle).unwrap();
        let digest = cadence_agent::store::app_runs::artifact_digest(body.as_bytes());
        finish_step(
            &run,
            1,
            json!({"schema":1,"kind":"review_text","run_id":run_id,"step_id":"s2","revision":1,
                "producer_step_id":"s1","producer_revision":1,"artifact_sha256":digest,
                "decision":"approve","rationale":"Checked the exact artifact."}),
        );
        run_id
    }
    /// A queued intent for `run_id` frozen through the real
    /// `social_publish_start` — the operator's tap, `schedule` mode so
    /// no claim runs.
    fn freeze(&self, run_id: &str, request: &str, due: i64) -> Value {
        self.op(
            "social_publish_start",
            json!({"request_id": request, "run_id": run_id, "mode": "schedule",
                "due_epoch": due}),
        )["intent"]
            .clone()
    }
    fn intents(&self, install: &str) -> Vec<Value> {
        self.op("social_publish_list", json!({"install_id": install}))["intents"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
    fn state(&self, intent_id: &str) -> String {
        self.op("social_publish_show", json!({"intent_id": intent_id}))["intent"]["state"]
            .as_str()
            .unwrap()
            .into()
    }
    fn execs(&self) -> u64 {
        self.door.execs.load(SeqCst)
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop();
    }
}

fn caption() -> String {
    door::caption_digest_of("# Post hp4\nReviewed copy.")
}

/// The workflow one publishable run carries: a text producer and an
/// independent reviewer on the `publication` slot.
const WORKFLOW: &str = r#"---
title: "HP4 post"
goal: "One reviewed post"
publication_slot: publication
inputs:
  writer: { ask: "writer" }
  reviewer: { ask: "reviewer" }
---

## Write
agent: {{writer}}
size: S
action: local.text.produce

Write one post.

### Acceptance
- [ ] post exists

## Review
agent: {{reviewer}}
size: S
depends_on: 1
action: local.text.review

Review the artifact.

### Acceptance
- [ ] reviewed
"#;

fn send_now_params(id: &str, install: &str) -> Value {
    json!({"intent_id": id, "install_id": install, "context_id": null})
}

fn reschedule_params(id: &str, install: &str, expected: i64, due: i64) -> Value {
    json!({"intent_id": id, "install_id": install, "context_id": null,
        "expected_due_epoch": expected, "due_epoch": due})
}

// --------------------------------------------------------------------
// Acceptance items, from the ticket and plan — not from the code.
// --------------------------------------------------------------------

/// HP4: one tap posts once — two `social_publish_start` calls with the
/// same request id make one intent and one provider call; a `send_now`
/// racing the frozen intent still makes exactly one send.
#[test]
fn double_start_and_racing_send_now_make_one_provider_call() {
    let mut fx = Fx::new(true);
    fx.arm_resolver();
    fx.start();
    let (install, _, bundle) = fx.install();
    let run = fx.approved_run(&install, &bundle, "once");
    // Two taps at once on the same request id: one intent, one send.
    let taps: Vec<_> = (0..2)
        .map(|_| {
            let dir = fx.dir();
            let run = run.clone();
            std::thread::spawn(move || {
                scoped(Asserted::Operator, || {
                    client::rpc(
                        &dir,
                        "social_publish_start",
                        json!({"request_id":"hp4-once","run_id":run,"mode":"now"}),
                    )
                })
            })
        })
        .collect();
    let replies: Vec<_> = taps.into_iter().map(|t| t.join().unwrap()).collect();
    let intents = fx.intents(&install);
    assert_eq!(intents.len(), 1, "two intents froze: {intents:?}");
    let id = intents[0]["intent_id"].as_str().unwrap().to_string();
    // A `send_now` racing the just-frozen intent still claims once — it
    // may land while the first tap's send is in flight or after it
    // posted; either way it makes no second provider call.
    let _press = fx.rpc(
        Asserted::Operator,
        "social_publish_send_now",
        send_now_params(&id, &install),
    );
    assert_eq!(
        fx.execs(),
        1,
        "the provider was called more than once: {replies:?}"
    );
    assert_eq!(fx.state(&id), "posted", "{replies:?}");
}

/// HP4: an agent caller and an unproven caller are refused on
/// `social_publish_start`, `social_publish_reschedule` and
/// `app_binding_publish_set` over RPC and over the board's HTTP route —
/// and nothing is written: no intent row and no provider call.
#[test]
fn non_operator_is_refused_over_rpc_and_http_and_writes_nothing() {
    let mut fx = Fx::new(false);
    fx.arm_resolver();
    fx.start();
    let (install, binding, bundle) = fx.install();
    let run = fx.approved_run(&install, &bundle, "gate");
    let intent = fx.freeze(&run, "hp4-gate", NOW + 3600);
    let id = intent["intent_id"].as_str().unwrap().to_string();
    let before_execs = fx.execs();
    let before_intents = fx.intents(&install).len();
    for who in [Asserted::Agent("cc13-pw".into()), Asserted::Unproven] {
        assert!(
            fx.rpc(
                who.clone(),
                "social_publish_start",
                json!({"request_id":"hp4-forge","run_id":run,"mode":"now"}),
            )
            .is_err(),
            "{who:?} reached social_publish_start"
        );
        assert!(
            fx.rpc(
                who.clone(),
                "social_publish_reschedule",
                reschedule_params(&id, &install, NOW + 3600, NOW + 7200),
            )
            .is_err(),
            "{who:?} reached social_publish_reschedule"
        );
        assert!(
            fx.rpc(
                who.clone(),
                "app_binding_publish_set",
                json!({"install_id": install, "binding_id": binding,
                    "expected_revision": 2,
                    "destination_id": "forged", "destination_label": "x",
                    "toolkit": "facebook", "timezone": "UTC", "grant_id": "dpq_forged12345"}),
            )
            .is_err(),
            "{who:?} reached app_binding_publish_set"
        );
    }
    // The same agent assertion refused over the board's HTTP route.
    for (path, body) in [
        (
            format!("/api/social-publishes/{id}/reschedule"),
            reschedule_params(&id, &install, NOW + 3600, NOW + 7200).to_string(),
        ),
        (
            "/api/social-publish-starts".to_string(),
            json!({"request_id":"hp4-forge2","run_id":run,"mode":"now"}).to_string(),
        ),
    ] {
        let (status, reply) = http_post(&fx.dir(), "agent:cc13-pw", &path, &body);
        assert_eq!(status, 403, "agent reached {path} over HTTP: {reply}");
    }
    // Nothing was written: no new intent row, no provider call, and the
    // queued intent still waits.
    assert_eq!(fx.execs(), before_execs, "a refused caller sent a post");
    assert_eq!(
        fx.intents(&install).len(),
        before_intents,
        "a refused caller froze an intent"
    );
    assert_eq!(fx.state(&id), "queued");
    // The operator still publishes the same row.
    let sent = fx.rpc(
        Asserted::Operator,
        "social_publish_send_now",
        send_now_params(&id, &install),
    );
    assert!(sent.is_ok(), "{sent:?}");
    assert_eq!(fx.execs(), before_execs + 1);
    assert_eq!(fx.state(&id), "posted");
}

/// HP4: a stale `expected_due_epoch` is refused; reschedule on a
/// processing, posted or cancelled intent is refused; a replayed
/// approval id is refused; at no point does the intent lack a schedule.
#[test]
fn reschedule_is_atomic_and_refuses_stale_replay_and_terminal_states() {
    let mut fx = Fx::new(false);
    fx.arm_resolver();
    fx.start();
    let (install, _, bundle) = fx.install();
    let run = fx.approved_run(&install, &bundle, "resched");
    let intent = fx.freeze(&run, "hp4-resched", NOW + 600);
    let id = intent["intent_id"].as_str().unwrap().to_string();
    let move_to = |expected: i64, due: i64| {
        fx.rpc(
            Asserted::Operator,
            "social_publish_reschedule",
            reschedule_params(&id, &install, expected, due),
        )
    };
    // A stale expected time is refused and changes nothing — and the
    // intent never lacks a schedule.
    assert!(move_to(NOW + 599, NOW + 3600).is_err(), "stale expected");
    let shown = fx.op("social_publish_show", json!({"intent_id": id}));
    assert_eq!(
        shown["intent"]["due_epoch"],
        json!(NOW + 600),
        "the intent lost its schedule: {shown}"
    );
    assert!(move_to(NOW + 600, NOW - 1).is_err(), "past due");
    // The atomic move lands: same intent, new due, a new approval.
    let moved = move_to(NOW + 600, NOW + 3600).unwrap();
    assert_eq!(moved["intent"]["state"], "queued");
    assert_eq!(moved["intent"]["due_epoch"], json!(NOW + 3600));
    assert_ne!(
        moved["intent"]["frozen"]["approval_id"], intent["frozen"]["approval_id"],
        "reschedule did not mint a new approval"
    );
    // The old expected time can never move it again.
    assert!(move_to(NOW + 600, NOW + 7200).is_err(), "old expected");
    // A replayed approval id (the original freeze's) cannot move it.
    let replayed = fx.rpc(
        Asserted::Operator,
        "social_publish_reschedule",
        json!({"intent_id": id, "install_id": install, "context_id": null,
            "expected_due_epoch": NOW + 3600, "due_epoch": NOW + 7200,
            "approval_id": intent["frozen"]["approval_id"]}),
    );
    assert!(replayed.is_err(), "a replayed approval moved the intent");
    // The posted row is terminal.
    let sent = fx.rpc(
        Asserted::Operator,
        "social_publish_send_now",
        send_now_params(&id, &install),
    );
    assert!(sent.is_ok(), "{sent:?}");
    assert_eq!(fx.state(&id), "posted");
    assert!(move_to(NOW + 3600, NOW + 7200).is_err(), "posted row moved");
    // A cancelled row refuses too.
    let run2 = fx.approved_run(&install, &bundle, "resched2");
    let intent2 = fx.freeze(&run2, "hp4-resched2", NOW + 600);
    let id2 = intent2["intent_id"].as_str().unwrap().to_string();
    fx.op(
        "social_publish_cancel",
        json!({"intent_id": id2, "install_id": install, "context_id": null}),
    );
    assert!(
        fx.rpc(
            Asserted::Operator,
            "social_publish_reschedule",
            reschedule_params(&id2, &install, NOW + 600, NOW + 3600),
        )
        .is_err(),
        "a cancelled row moved"
    );
    assert_eq!(fx.state(&id2), "cancelled");
}

/// HP4: the destination always derives from the binding's
/// `config.publish` — extra body fields (`destination`, `grant`,
/// `scope`, `approval`, `price`) are refused before any freeze, and the
/// request can never name where or how the post goes.
#[test]
fn forged_destination_grant_scope_approval_and_price_are_refused() {
    let mut fx = Fx::new(false);
    fx.arm_resolver();
    fx.start();
    let (install, _, bundle) = fx.install();
    let run = fx.approved_run(&install, &bundle, "forge");
    let before = fx.intents(&install).len();
    for forged in [
        json!({"destination_id": "forged-dest"}),
        json!({"grant_id": "dpq_forged_grant_01"}),
        json!({"scope": {"install_id": "other"}}),
        json!({"approval_id": "apv-00000000000000000000000000000000"}),
        json!({"price": "0.06"}),
        json!({"toolkit": "instagram"}),
        json!({"timezone": "UTC"}),
        json!({"connection_id": "connX"}),
        json!({"install_id": "other-install"}),
        json!({"context_id": "other-ctx"}),
    ] {
        let mut params = json!({"request_id":"hp4-forge","run_id":run,"mode":"now"});
        for (key, value) in forged.as_object().unwrap() {
            params[key] = value.clone();
        }
        let reply = fx.rpc(Asserted::Operator, "social_publish_start", params.clone());
        assert!(reply.is_err(), "a forged field was accepted: {params}");
        assert_eq!(
            fx.intents(&install).len(),
            before,
            "a forged start froze a row: {params}"
        );
    }
    // A request id reused for another run is a forged reuse, never a resume.
    let intent = fx.freeze(&run, "hp4-reuse", NOW + 3600);
    let run2 = fx.approved_run(&install, &bundle, "forge2");
    let reused = fx.rpc(
        Asserted::Operator,
        "social_publish_start",
        json!({"request_id":"hp4-reuse","run_id":run2,"mode":"now"}),
    );
    assert!(reused.is_err(), "a request id reused across runs");
    assert_eq!(fx.execs(), 0, "a forged start reached the provider");
    assert_eq!(fx.state(intent["intent_id"].as_str().unwrap()), "queued");
}

// --------------------------------------------------------------------
// The board's HTTP surface.
// --------------------------------------------------------------------

/// A minimal HTTP POST against a board on `dir` over an ephemeral port,
/// asserting `who` through the seam headers. Returns (status, body).
fn http_post(dir: &std::path::Path, who: &str, path: &str, body: &str) -> (u16, String) {
    let free = |p: &u16| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok();
    let port = (3110..3200).find(free).unwrap();
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
    let (dir_owned, pm) = (dir.to_path_buf(), dir.join("pm"));
    let board = std::thread::spawn(move || cadence_agent::ui::serve(&dir_owned, &pm, &opts));
    rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    let token = Seam::token_at(dir).unwrap();
    let host = format!("cadence-{port}.localhost:{port}");
    let req = format!(
        "POST {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         X-Cadence-Board: 1\r\nSec-Fetch-Site: same-origin\r\nOrigin: http://{host}\r\n\
         {AS_HEADER}: {who}\r\n{TOKEN_HEADER}: {token}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
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
