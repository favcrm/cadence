//! CAD-1120 ticket results at the daemon boundary: an in-process daemon,
//! the real CRM bundle installed and approved through `Shared::dispatch`,
//! and fake workers that the real idle timer stops. The workers mint a
//! fresh endpoint generation on every open, as managed Pi does.
//!
//! - R1: a team the idle timer stopped can be planned and dispatched,
//!   and dispatch wakes the worker with no manual resume.
//! - R2: a worker an operator stopped is refused at create, and also at
//!   dispatch when the operator stops it after create.
//! - R3: agent callers and an unproven (detached) caller cannot plan,
//!   approve or dispatch, and forged request fields are refused.
//!
//! Each refusal is asserted by its reason, and each test holds a positive
//! control so it cannot pass by refusing everything.

use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

use super::*;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};

const REFUSED_TEAM: &str = "local team needs an enabled registered managed local worker";

pub(super) fn pid() -> u32 {
    std::process::id()
}

/// A daemon with an owner PM `lead`, two fake workers in its group, the
/// CRM bundle installed and approved, and an idle timer on a clock the
/// test moves.
pub(super) struct Fx {
    _dir: tempfile::TempDir,
    pub(super) shared: Arc<Shared>,
    pub(super) install: String,
    clock: Arc<AtomicU64>,
}

impl Fx {
    pub(super) fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("c1120").tempdir().unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let clock = Arc::new(AtomicU64::new(epoch_secs() as u64));
        let read = Arc::clone(&clock);
        let opts = ServeOptions {
            auto_stop: Some(AutoStopSetting::idle_after(3600)),
            auto_stop_clock: Some(Arc::new(move || read.load(Ordering::SeqCst) as f64)),
            ..ServeOptions::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd = dir.path().to_str().unwrap().to_string();
        for (alias, role, params) in [
            ("lead", "pm", None),
            (
                "writer",
                "worker",
                Some(r#"{"upstream":"lead","fake_generation":true}"#),
            ),
            (
                "reviewer",
                "worker",
                Some(r#"{"upstream":"lead","fake_generation":true}"#),
            ),
        ] {
            shared
                .store
                .register_agent(&NewAgent {
                    alias,
                    provider: "fake",
                    endpoint_kind: "fake",
                    role,
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        let source = format!("{}/workspace-apps/crm", env!("CARGO_MANIFEST_DIR"));
        let mut fx = Self {
            _dir: dir,
            shared,
            install: String::new(),
            clock,
        };
        let out = fx
            .operator("app_workspace_install", json!({"source": source}))
            .unwrap();
        let approve = json!({"install_id": out["install_id"], "digest": out["digest"]});
        fx.operator("app_local_install_approve", approve).unwrap();
        fx.install = out["install_id"].as_str().unwrap().to_string();
        fx
    }

    pub(super) fn call(&self, who: Asserted, method: &str, params: Value) -> Result<Value> {
        scoped(who, || self.shared.dispatch(method, &params, pid()))
    }

    pub(super) fn operator(&self, method: &str, params: Value) -> Result<Value> {
        self.call(Asserted::Operator, method, params)
    }

    pub(super) fn agent(&self, alias: &str) -> crate::store::Agent {
        self.shared.store.agent(alias).unwrap()
    }

    pub(super) fn owned(&self, alias: &str) -> bool {
        self.shared.lifecycle.lock().unwrap().owned(alias)
    }

    pub(super) fn wait(&self, what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Open both workers through the daemon and wait until each is idle
    /// with its native session recorded.
    pub(super) fn start_team(&self) {
        for alias in ["writer", "reviewer"] {
            self.shared.launch_actor(alias).unwrap();
        }
        self.wait("team idle", || {
            ["writer", "reviewer"].iter().all(|a| {
                let agent = self.agent(a);
                agent.state == "idle" && agent.session_id.is_some()
            })
        });
    }

    /// Two hours pass and the real idle timer stops both workers.
    pub(super) fn idle_out(&self) {
        self.clock.fetch_add(2 * 3600, Ordering::SeqCst);
        self.shared.auto_stop_tick();
        self.wait("auto-stop", || {
            ["writer", "reviewer"]
                .iter()
                .all(|a| self.agent(a).state == "stopped" && !self.owned(a))
        });
        for alias in ["writer", "reviewer"] {
            let agent = self.agent(alias);
            assert!(!agent.enabled, "the idle timer leaves {alias} disabled");
            let marker = self.marker(alias);
            assert_eq!(marker, AUTO_STOP_EVENT, "{alias}");
        }
    }

    pub(super) fn marker(&self, alias: &str) -> String {
        self.shared
            .store
            .last_event_of(alias, AUTO_STOP_MARKER_KINDS)
            .unwrap()
            .map(|e| e.kind)
            .unwrap_or_default()
    }

    pub(super) fn create_params(&self, request: &str) -> Value {
        json!({
            "install_id": self.install,
            "workflow": "email-brief",
            "request_id": request,
            "owner_pm": "lead",
            "inputs": {
                "subject": "Renewal",
                "audience": "Customers due a renewal",
                "facts": "Plan renews 1 July. Price stays HK$88/month.",
                "writer": "writer",
                "reviewer": "reviewer",
            },
        })
    }

    pub(super) fn create(&self, request: &str) -> Result<Value> {
        self.operator("app_run_create", self.create_params(request))
    }

    pub(super) fn approve(&self, run: &Value) {
        let params = json!({"run_id": run["id"], "digest": run["snapshot_digest"]});
        self.operator("app_run_approve", params).unwrap();
    }

    /// The writer step's kickoff message, once one is dispatched.
    pub(super) fn kickoff(&self, run: &Value) -> Option<String> {
        let shown = self
            .shared
            .store
            .app_run_show(run["id"].as_str().unwrap())
            .unwrap();
        shown["steps"][0]["message_id"].as_str().map(str::to_string)
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        for alias in ["writer", "reviewer"] {
            if self.owned(alias) {
                let _ = self.shared.rpc_stop(&json!({"alias": alias}));
            }
        }
    }
}

pub(super) fn refusal(result: Result<Value>) -> String {
    result.expect_err("expected a refusal").to_string()
}

/// R1. Guards: the planner's and dispatch's auto-stop eligibility
/// (`Store::agent_auto_parked_in`), the generation-free run binding
/// (`Store::app_binding_identity`) and the wake in the dispatch RPC.
#[test]
fn cad1120_auto_stopped_team_plans_dispatches_and_wakes_without_resume() {
    let fx = Fx::new();
    fx.start_team();
    let before = fx.agent("writer").generation;
    assert!(before.is_some(), "the fixture mints a generation per open");
    fx.idle_out();

    let run = fx.create("r1").expect("an auto-stopped team is eligible");
    fx.approve(&run);
    let dispatched = fx
        .operator("app_run_dispatch", json!({"run_id": run["id"]}))
        .expect("dispatch of an auto-stopped team");
    assert_eq!(dispatched["state"], "running", "{dispatched}");

    // The dispatch woke the writer: no `agent_resume`, no operator.
    let kickoff = fx.kickoff(&run).expect("writer kickoff queued");
    fx.wait("kickoff delivered to the woken writer", || {
        fx.shared
            .store
            .message(&kickoff)
            .unwrap()
            .is_some_and(|m| m.turn_id.is_some())
    });
    let writer = fx.agent("writer");
    assert!(writer.enabled, "woken writer is enabled again");
    assert_ne!(writer.generation, before, "a new endpoint generation");
    let resumed = fx
        .shared
        .store
        .last_event_of("writer", &[AUTO_RESUME_EVENT])
        .unwrap()
        .expect("the wake is the CAD-413 auto-resume");
    assert_eq!(resumed.payload["message"], kickoff.as_str(), "{resumed:?}");
    let message = fx.shared.store.message(&kickoff).unwrap().unwrap();
    assert_ne!(message.state, "rejected", "{message:?}");
    // The reviewer's step has not been dispatched: it stays parked.
    assert_eq!(fx.agent("reviewer").state, "stopped");
}

/// R2. Guard: eligibility needs the idle timer's own stop to be the
/// newest stop record; an operator stop, before create or after it,
/// refuses and never wakes the worker.
#[test]
fn cad1120_operator_stopped_worker_is_refused_at_create_and_dispatch() {
    let fx = Fx::new();
    fx.start_team();

    // A live worker the operator stops.
    fx.operator("agent_stop", json!({"alias": "writer"}))
        .unwrap();
    assert!(refusal(fx.create("op-live")).contains(REFUSED_TEAM));

    // Auto-stopped, then stopped again by the operator: the operator's
    // stop is the newer record and wins.
    fx.operator("agent_resume", json!({"alias": "writer"}))
        .unwrap();
    fx.wait("writer back", || fx.agent("writer").state == "idle");
    fx.idle_out();
    fx.operator("agent_stop", json!({"alias": "writer"}))
        .unwrap();
    assert!(refusal(fx.create("op-after-auto")).contains(REFUSED_TEAM));

    // Control: an unregistered worker is still refused, a parked one is
    // not.
    let mut ghost = fx.create_params("ghost");
    ghost["inputs"]["writer"] = json!("ghost");
    assert!(fx.operator("app_run_create", ghost).is_err());
    fx.operator("agent_resume", json!({"alias": "writer"}))
        .unwrap();
    fx.wait("writer back", || fx.agent("writer").state == "idle");
    fx.idle_out();
    let run = fx.create("parked").expect("control: parked team plans");
    fx.approve(&run);

    // The operator stops the writer after create: dispatch refuses and
    // the writer stays stopped, with no kickoff queued.
    fx.operator("agent_stop", json!({"alias": "writer"}))
        .unwrap();
    let error = refusal(fx.operator("app_run_dispatch", json!({"run_id": run["id"]})));
    assert!(
        error.contains("registered app assignment changed"),
        "{error}"
    );
    std::thread::sleep(Duration::from_millis(200));
    let writer = fx.agent("writer");
    assert_eq!(writer.state, "stopped");
    assert!(!writer.enabled && !fx.owned("writer"));
    assert_eq!(fx.kickoff(&run), None, "no kickoff for a refused dispatch");
}

/// R3. Guards: `operator_connection` on every app run verb and the
/// request field allowlist. Neither an agent nor a detached child (an
/// unproven caller) can plan for, approve for or wake a parked team,
/// and no request field can mark a worker as auto-stopped.
#[test]
fn cad1120_agents_and_forged_fields_cannot_reach_the_wake_path() {
    let fx = Fx::new();
    fx.start_team();
    fx.idle_out();
    let callers = [
        Asserted::Agent("writer".into()),
        Asserted::Agent("lead".into()),
        Asserted::Unproven,
    ];
    for who in &callers {
        let error = refusal(fx.call(who.clone(), "app_run_create", fx.create_params("agent")));
        assert!(
            error.contains("operator") || error.contains("unproven"),
            "{who:?}: {error}"
        );
    }

    // Forged fields that claim a stop reason or ask for a wake.
    fx.operator("agent_resume", json!({"alias": "writer"}))
        .unwrap();
    fx.wait("writer back", || fx.agent("writer").state == "idle");
    fx.operator("agent_stop", json!({"alias": "writer"}))
        .unwrap();
    for (field, value) in [
        ("auto_stopped", json!(true)),
        ("wake", json!(true)),
        ("stop_reason", json!(AUTO_STOP_EVENT)),
        (
            "assignments",
            json!({"1": {"alias": "writer", "auto_stopped": true}}),
        ),
    ] {
        let mut params = fx.create_params(&format!("forged-{}", field.replace('_', "-")));
        params[field] = value;
        let error = refusal(fx.operator("app_run_create", params));
        assert!(error.contains("unsupported fields"), "{field}: {error}");
    }
    assert!(refusal(fx.create("forged-control")).contains(REFUSED_TEAM));
    assert_eq!(fx.marker("writer"), "stop_requested");

    // A parked team the operator planned and approved: agent callers
    // still cannot dispatch it, so nothing wakes.
    fx.operator("agent_resume", json!({"alias": "writer"}))
        .unwrap();
    fx.wait("writer back", || fx.agent("writer").state == "idle");
    fx.idle_out();
    let run = fx.create("operator-run").unwrap();
    for who in &callers {
        let approve = json!({"run_id": run["id"], "digest": run["snapshot_digest"]});
        let error = refusal(fx.call(who.clone(), "app_run_approve", approve));
        assert!(
            error.contains("operator") || error.contains("unproven"),
            "{who:?}: {error}"
        );
    }
    fx.approve(&run);
    for who in &callers {
        let dispatch = json!({"run_id": run["id"]});
        let error = refusal(fx.call(who.clone(), "app_run_dispatch", dispatch));
        assert!(
            error.contains("operator") || error.contains("unproven"),
            "{who:?}: {error}"
        );
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(fx.agent("writer").state, "stopped");
    assert!(!fx.owned("writer"));
    assert_eq!(fx.marker("writer"), AUTO_STOP_EVENT);
    // Control: the operator's dispatch wakes it.
    fx.operator("app_run_dispatch", json!({"run_id": run["id"]}))
        .unwrap();
    fx.wait("writer woken", || fx.owned("writer"));
}
