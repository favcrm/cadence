//! CAD-1142 independent acceptance — written from the ticket's required
//! security behavior, not as an oracle for the candidate's output.
//!
//! Required boundary (Demo symptom: an Instagram `source` run failed at
//! s1 in ~16s with no delivered message and no shown reason):
//!
//! - A transient provider quote/transport outage is NOT silently
//!   reclassified as authority loss: a `running` run must survive
//!   monitor ticks that cannot reach the quote door, and a
//!   queued-but-approved run must not be invalidated by a dead probe.
//! - Dispatch/admission still re-proves the binding revision, the
//!   capability allowance and the content at the real call, so a
//!   genuinely stale binding, a revocation or a wrong-scope change
//!   STILL refuses and invalidates — carrying the actual plain-words
//!   reason onto the failed step.
//! - An auth denial or a corrupt price schema from the provider is an
//!   unknown refusal, never a permissive skip; the spend ceiling is
//!   still enforced at the call, not merely at quote time.
//! - The operator rule holds: agent and unproven callers, and a forged
//!   field, are refused on every app-lifecycle route that runs the
//!   binding/authority checks — with nothing written.
//!
//! Every check drives a real in-process seam daemon (the 1s monitor tick
//! that re-runs `advance_app_runs`, exactly like Demo) plus a probe
//! provider whose quote door can be toggled to fail the way the hosted
//! AgenticOS quote read did. Temp HOME/XDG/TMP/PM/state are explicit; no
//! live provider or real agent launch.
#![cfg(feature = "test-seam")]

use cadence_agent::contract_fixture::{ToolTable, Verified};
use cadence_agent::platform::connections::{
    BoundActionMapping, CapabilityDescriptor, CapabilitySemantics, ProviderDescriptor,
};
use cadence_agent::platform::{AppCapabilityQuote, PlatformAdapter};
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon, store::Store};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PROVIDER: &str = "probe1142";
const TOOL: &str = "read_probe_listing";

/// The five quote postures this provider can be told to serve. `Up`
/// returns the reviewed quote; `Down` models Demo's unreachable door;
/// `Denied` is an auth/permission refusal; `Corrupt` is a well-formed
/// transport carrying a price schema that fails `valid()` — the
/// "unknown auth-denial / corrupt response" the boundary must not skip.
/// `Priced` is a healthy door reporting a *different* live price than
/// the frozen one — the real `Rejected` drift path (authority intact,
/// price moved), which still invalidates.
#[derive(Clone, Copy)]
enum Posture {
    Up,
    Down,
    Denied,
    Corrupt,
    Priced,
}

struct Probe {
    posture: Mutex<Posture>,
    table: Mutex<&'static ToolTable>,
}

impl Probe {
    fn new() -> Arc<Self> {
        let table = ToolTable::from_json(&json!({"platform": PROVIDER,
            "manifest_version": "probe-tools@1",
            "tools": [{"tool": TOOL, "effect": "read", "scopes": ["provider.read"]}]}))
        .unwrap();
        Arc::new(Self {
            posture: Mutex::new(Posture::Up),
            table: Mutex::new(Box::leak(Box::new(table))),
        })
    }
    fn set_posture(&self, posture: Posture) {
        *self.posture.lock().unwrap() = posture;
    }
    /// The exact quote a healthy door serves for this binding.
    fn reviewed_quote() -> AppCapabilityQuote {
        AppCapabilityQuote {
            schema: 1,
            currency: "USD".into(),
            unit_price_micros: 2000,
            units: 1,
            total_price_micros: 2000,
            price_revision: "probe-price/1".into(),
        }
    }
}

impl PlatformAdapter for Probe {
    fn table(&self) -> &ToolTable {
        *self.table.lock().unwrap()
    }
    fn connection_descriptor(&self) -> Option<ProviderDescriptor> {
        Some(ProviderDescriptor {
            schema: 1,
            provider: PROVIDER.into(),
            revision: "probe-connections/1".into(),
            enrollment_shapes: vec!["token".into()],
            builtin_accounts: vec!["hosted".into()],
            capabilities: vec![CapabilityDescriptor {
                id: "probe.read".into(),
                version: 1,
                tools: vec![TOOL.into()],
                scopes: vec!["provider.read".into()],
                effect: "read".into(),
                semantics: CapabilitySemantics::MetadataRead,
            }],
            action_mappings: vec![BoundActionMapping {
                capability: "probe.read".into(),
                version: 1,
                action: "list_items".into(),
                resource_kind: "connection_account".into(),
                tool: TOOL.into(),
                scopes: vec!["provider.read".into()],
                effect: "read".into(),
                semantics: CapabilitySemantics::MetadataRead,
                input_contract: "probe.query@1".into(),
                output_contract: "probe.receipt@1".into(),
            }],
        })
    }
    fn connection_registration(&self) -> Option<String> {
        Some("probe1142:probe-connections/1:probe-tools@1".into())
    }
    fn app_credentialless_account(&self, account: &str) -> bool {
        account == "hosted"
    }
    fn quote_app_capability(
        &self,
        _credential: &[u8],
        binding: &Value,
    ) -> Result<AppCapabilityQuote, String> {
        match *self.posture.lock().unwrap() {
            // Demo's AgenticOS quote door: unreachable → refusal.
            Posture::Down => {
                Err("bound capability price discovery refused: door_unreachable".into())
            }
            // An auth/permission denial is not a price; it must surface
            // as a refusal the run cannot mistake for authority loss or
            // silently skip.
            Posture::Denied => {
                Err("bound capability price discovery refused: auth_denied".into())
            }
            // A corrupt schema: units out of range → `valid()` is false.
            Posture::Corrupt => Ok(AppCapabilityQuote {
                schema: 1,
                currency: "USD".into(),
                unit_price_micros: 2000,
                units: 0,
                total_price_micros: 0,
                price_revision: String::new(),
            }),
            // A live quote that moved since approval: a well-formed,
            // valid quote whose total/revision differ from the frozen
            // one. The run's drift comparison must reject — a genuine
            // `Rejected`, not a probe miss — so the run invalidates.
            Posture::Priced => Ok(AppCapabilityQuote {
                schema: 1,
                currency: "USD".into(),
                unit_price_micros: 9000,
                units: 1,
                total_price_micros: 9000,
                price_revision: "probe-price/2".into(),
            }),
            Posture::Up => {
                if binding["config"]["mapping"]["tool"] != TOOL {
                    return Err("not_allowlisted".into());
                }
                Ok(Self::reviewed_quote())
            }
        }
    }
    fn reported_manifest_version(&self) -> Option<String> {
        Some("probe-tools@1".into())
    }
    fn preview(&self, _: &str, _: &str, _: &Value) -> String {
        String::new()
    }
    fn execute(
        &self,
        _: &[u8],
        _: &str,
        _: &Value,
        _: &str,
        _: Option<&str>,
    ) -> Result<Value, String> {
        Err("probe executes nothing".into())
    }
    fn read_back(&self, _: &str, _: &Value) -> Verified {
        Verified::Unknown
    }
    fn source_hash(&self, _: &str, _: &str) -> Option<String> {
        None
    }
}

const MANIFEST: &str = r#"---
app: probe-reader
title: Probe Reader
version: '1.0.0'
summary: Read one bounded listing through a bound source.
needs:
  connections: []
  capabilities:
    source:
      schema: 1
      capability: probe.read
      version: 1
      action: list_items
      resource_kind: connection_account
      effect: read
---

# Probe Reader

Reads one bounded listing through the bound `source` capability.
"#;

const WORKFLOW: &str = r#"---
title: "Read listing: {{handle}}"
goal: "Retain one bounded provider receipt"
label: Read listing
capability_slots: [source]
inputs:
  handle: { ask: "Listing handle", example: "probe" }
  writer: { ask: "Registered reader in the owner PM group" }
---

Read the selected listing through the bound `source` capability.

## Read listing: {{handle}}
agent: {{writer}}
size: S
action: local.text.produce

Call `source` once and report the receipt identity.

### Acceptance
- [ ] the receipt identity is reported
"#;

struct Fx {
    root: tempfile::TempDir,
    provider: Arc<Probe>,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        for thread in self.threads.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

impl Fx {
    fn start() -> Self {
        let root = tempfile::Builder::new().prefix("c1142acc").tempdir().unwrap();
        let provider = Probe::new();
        let mut fx = Self {
            root,
            provider,
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
        };
        cadence_agent::issue::Pm::init(&fx.pm()).unwrap();
        fx.package();
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", fx.pm().to_str().unwrap());
        let mut opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(fx.stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        opts.platforms.insert(
            PROVIDER.into(),
            fx.provider.clone() as Arc<dyn PlatformAdapter>,
        );
        let state = fx.state();
        fx.threads.push(std::thread::spawn(move || {
            daemon::serve_with(&state, opts).unwrap()
        }));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&fx.state(), "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&fx.state()).is_none()
        {
            assert!(std::time::Instant::now() < deadline, "daemon never started");
            std::thread::sleep(Duration::from_millis(50));
        }
        fx.team();
        fx
    }
    fn state(&self) -> std::path::PathBuf {
        self.root.path().join("s")
    }
    fn pm(&self) -> std::path::PathBuf {
        self.root.path().join("pm")
    }
    fn source(&self) -> std::path::PathBuf {
        self.root.path().join("app")
    }
    fn package(&self) {
        std::fs::create_dir_all(self.source().join("workflows")).unwrap();
        std::fs::write(self.source().join("app.md"), MANIFEST).unwrap();
        std::fs::write(self.source().join("workflows/read.md"), WORKFLOW).unwrap();
    }
    /// The owner PM and the assigned reader — a managed local worker in
    /// the owner's group, exactly the assignment `app_run_create` proves.
    fn team(&self) {
        let store = Store::open(&self.state().join("cadence.sqlite3")).unwrap();
        let cwd = self.root.path().to_str().unwrap();
        for (alias, role) in [("lead", "pm"), ("writer", "worker")] {
            store
                .register_agent(&cadence_agent::store::NewAgent {
                    alias,
                    provider: "claude",
                    endpoint_kind: "managed",
                    role,
                    cwd,
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
    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let state = self.state();
        scoped(who, || client::rpc(&state, method, params))
    }
    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }
    /// `op`, preserving the error text for refusal checks.
    fn op_err(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        self.rpc(Asserted::Operator, method, params)
    }
    fn show(&self, id: &str) -> Value {
        self.op("app_run_show", json!({"run_id": id}))
    }
    fn run_list(&self, install: &str) -> Value {
        self.op("app_run_list", json!({"install_id": install}))
    }
    fn wait_state(&self, id: &str, want: &str, what: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let shown = self.show(id);
            if shown["state"] == want {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{what}: still {} — {shown}",
                shown["state"].as_str().unwrap_or("?")
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    /// Install → bind → create → approve → first dispatch, returning
    /// `(install_id, binding_id, run_id)`. The same operator RPC chain the
    /// Demo host ran before s1 dispatched.
    fn live_run(&self, tag: &str) -> (String, String, String) {
        let installed = self.op("app_workspace_install", json!({"source": self.source()}));
        let install = installed["install_id"].as_str().unwrap().to_string();
        let rows = self.op("connection_list", json!({}))["connections"].clone();
        let connection = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["provider"] == PROVIDER && row["account"] == "hosted")
            .expect("probe hosted connection")["id"]
            .as_str()
            .unwrap()
            .to_string();
        let bound = self.op(
            "app_binding_create",
            json!({"install_id": install, "slot": "source",
                "connection_id": connection, "request_id": format!("bind-{tag}")}),
        )["binding"]
            .clone();
        let run = self
            .rpc(
                Asserted::Operator,
                "app_run_create",
                json!({"install_id": install, "workflow": "read",
                    "inputs": {"handle": "probe", "writer": "writer"},
                    "request_id": format!("run-{tag}"), "owner_pm": "lead"}),
            )
            .unwrap();
        let id = run["id"].as_str().unwrap().to_string();
        self.op(
            "app_run_approve",
            json!({"run_id": id, "digest": run["snapshot_digest"]}),
        );
        (
            install,
            bound["id"].as_str().unwrap().to_string(),
            id,
        )
    }
    /// Dispatch the first step, asserting the kickoff was queued.
    fn dispatch_first(&self, run_id: &str) -> Value {
        let dispatched = self.op("app_run_dispatch", json!({"run_id": run_id}));
        assert_eq!(dispatched["state"], "running", "{dispatched}");
        assert_eq!(dispatched["steps"][0]["state"], "dispatched", "{dispatched}");
        assert!(dispatched["steps"][0]["message_id"].is_string());
        dispatched
    }
}

/// A step's persisted failure reason — the board/screen read.
fn step_reason(shown: &Value, idx: usize) -> String {
    shown["steps"][idx]["reason"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

// --------------------------------------------------------------------
// The boundary CAD-1142 draws, from the ticket.
// --------------------------------------------------------------------

/// A transient quote outage on the monitor's re-validation ticks must not
/// reclassify as authority loss: a running run stays `running` through a
/// dead door, then still invalidates — with the real reason — once the
/// binding is genuinely revoked. The step never loses its message_id, and
/// no second run replaces the killed one.
#[test]
fn transient_outage_survives_then_stale_binding_invalidates_with_reason() {
    let fx = Fx::start();
    let (install, binding_id, run_id) = fx.live_run("survive");
    fx.dispatch_first(&run_id);

    // The provider door goes down. Successive monitor ticks re-quote;
    // the run must stay running, the step dispatched, the message
    // queued — not failed before the worker's first turn.
    fx.provider.set_posture(Posture::Down);
    std::thread::sleep(Duration::from_secs(4));
    let shown = fx.show(&run_id);
    assert_eq!(shown["state"], "running", "{shown}");
    assert_eq!(shown["steps"][0]["state"], "dispatched", "{shown}");
    assert!(shown["steps"][0]["message_id"].is_string(), "{shown}");
    // And no phantom invalidation wrote a fresh run or a reason.
    assert_eq!(
        fx.run_list(&install)["runs"].as_array().unwrap().len(),
        1,
        "a transient outage spawned a replacement run"
    );

    // The door recovers; the binding is then genuinely revoked — the
    // next tick must invalidate, and the failed step must carry the
    // real authority-loss reason, not a generic one.
    fx.provider.set_posture(Posture::Up);
    fx.op(
        "app_binding_revoke",
        json!({"install_id": install, "binding_id": binding_id,
            "expected_revision": 1}),
    );
    fx.wait_state(&run_id, "failed", "stale binding invalidates");
    let shown = fx.show(&run_id);
    assert_eq!(shown["steps"][0]["state"], "failed", "{shown}");
    let reason = step_reason(&shown, 0);
    assert!(!reason.trim().is_empty(), "failed step carries no reason: {shown}");
    assert!(
        reason.contains("binding"),
        "reason names the stale authority, got: {reason}"
    );
}

/// An approved run whose first kickoff has not yet been dispatched is
/// also not authority-loss prey to a transient quote outage: the
/// dispatch-time re-proof (which *does* re-quote, like Demo's first
/// tick) must treat a dead door as a probe miss and still dispatch the
/// step, not reject the run. Once the door recovers the run proceeds.
#[test]
fn outage_at_first_dispatch_still_dispatches() {
    let fx = Fx::start();
    let (install, _binding_id, run_id) = fx.live_run("queued");
    // Approved, first kickoff pending, door down: the dispatch path's
    // own binding/quote re-proof must treat the dead door as a probe
    // miss — dispatch the step anyway, not fail the run.
    fx.provider.set_posture(Posture::Down);
    let dispatched = fx.op("app_run_dispatch", json!({"run_id": run_id}));
    assert_eq!(
        dispatched["state"], "running",
        "a transient outage refused the dispatch: {dispatched}"
    );
    assert_eq!(dispatched["steps"][0]["state"], "dispatched", "{dispatched}");
    let shown = fx.show(&run_id);
    assert_eq!(
        shown["state"], "running",
        "a transient outage at dispatch killed the run: {shown}"
    );
    assert_eq!(
        fx.run_list(&install)["runs"].as_array().unwrap().len(),
        1,
        "outage window wrote or removed a run"
    );
}

/// An auth denial or a corrupt price schema is an unknown refusal — the
/// run is not killed (it is not authority loss), but the refusal is NOT
/// a permissive skip either: the run stays exactly as it was, and a
/// later stale binding still invalidates. This is the boundary between
/// "transient probe miss" and "don't pretend the quote was fine".
#[test]
fn denied_and_corrupt_quotes_neither_kill_nor_skip() {
    for posture in [Posture::Denied, Posture::Corrupt] {
        let fx = Fx::start();
        let (install, binding_id, run_id) = fx.live_run(match posture {
            Posture::Denied => "denied",
            _ => "corrupt",
        });
        fx.dispatch_first(&run_id);
        let before = fx.show(&run_id);
        fx.provider.set_posture(posture);
        std::thread::sleep(Duration::from_secs(4));
        let shown = fx.show(&run_id);
        // Still running — not invalidated — and unchanged.
        assert_eq!(
            shown["state"], "running",
            "{posture:?} was reclassified as authority loss: {shown}"
        );
        assert_eq!(shown["steps"][0]["state"], "dispatched", "{shown}");
        // Not silently skipped either: once the door serves a good
        // quote again, then the binding is revoked, the stale state
        // still invalidates with its reason.
        fx.provider.set_posture(Posture::Up);
        fx.op(
            "app_binding_revoke",
            json!({"install_id": install, "binding_id": binding_id,
                "expected_revision": 1}),
        );
        fx.wait_state(&run_id, "failed", "stale binding still invalidates");
        let shown = fx.show(&run_id);
        let reason = step_reason(&shown, 0);
        assert!(
            !reason.trim().is_empty() && reason.contains("binding"),
            "post-{posture:?} invalidation lost its reason: {shown} (was {before})"
        );
    }
}

/// The operator rule is exercised at every route that runs the
/// binding/authority checks — an agent caller, an unproven caller and a
/// forged field are all refused, and nothing is written.
#[test]
fn agent_unproven_and_forged_fields_are_refused_and_write_nothing() {
    let fx = Fx::start();
    let (install, _binding_id, run_id) = fx.live_run("gate");
    let before_runs = fx.run_list(&install)["runs"].as_array().unwrap().len();

    // Non-operator callers are refused on each lifecycle verb that
    // reaches the run/binding proof.
    for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        for (method, params) in [
            ("app_run_create", json!({"install_id": install, "workflow": "read",
                "inputs": {"handle":"probe","writer":"writer"},
                "request_id":"forge-create", "owner_pm":"lead"})),
            ("app_run_approve", json!({"run_id": run_id, "digest": "sha256:forged"})),
            ("app_run_dispatch", json!({"run_id": run_id})),
            ("app_run_cancel", json!({"run_id": run_id})),
            ("app_binding_revoke", json!({"install_id": install,
                "binding_id": "forged-binding", "expected_revision": 1})),
            ("app_binding_create", json!({"install_id": install, "slot": "source",
                "connection_id": "forged-conn", "request_id": "forge-bind"})),
        ] {
            assert!(
                fx.rpc(who.clone(), method, params.clone()).is_err(),
                "{who:?} reached {method} with {params}"
            );
        }
    }

    // A forged identity/authority field inside an operator verb is
    // refused, never read: an unknown field, a forged digest and a
    // forged run id all reject before any write.
    for params in [
        // Forged field the allowlist refuses.
        json!({"install_id": install, "workflow": "read",
            "inputs": {"handle":"probe","writer":"writer"},
            "request_id":"forge-field", "owner_pm":"lead",
            "as_alias":"lead", "approval_id":"apv-forged"}),
        // Forged approval digest.
        json!({"run_id": run_id, "digest": "sha256:forged-digest"}),
        // A run id that does not exist at all.
        json!({"run_id": "run-nonexistent", "digest": "sha256:x"}),
    ] {
        let method = if params.get("request_id").is_some() {
            "app_run_create"
        } else {
            "app_run_approve"
        };
        assert!(
            fx.op_err(method, params.clone()).is_err(),
            "forged params accepted on {method}: {params}"
        );
    }

    // Nothing was written: same run count, same binding, the target run
    // still awaits its real approval (not approved/cancelled/failed).
    let shown = fx.show(&run_id);
    assert_eq!(
        shown["state"], "awaiting_approval",
        "a refused call moved the run: {shown}"
    );
    assert_eq!(
        fx.run_list(&install)["runs"].as_array().unwrap().len(),
        before_runs,
        "a refused call wrote a run"
    );
    // The operator's real approval still lands.
    fx.op(
        "app_run_approve",
        json!({"run_id": run_id, "digest": shown["snapshot_digest"]}),
    );
    assert_eq!(fx.show(&run_id)["state"], "approved");
}

/// A stale binding refuses the worker's own claim path — not just the
/// monitor's. Revoking the binding while the kickoff sits queued makes
/// the admission re-proof refuse, the step fail, and the run
/// invalidate with the authority reason, never silently admitting.
#[test]
fn stale_binding_at_claim_refuses_and_invalidates() {
    let fx = Fx::start();
    let (install, binding_id, run_id) = fx.live_run("claim");
    fx.dispatch_first(&run_id);
    // The kickoff is queued but not yet claimed by a worker turn.
    fx.op(
        "app_binding_revoke",
        json!({"install_id": install, "binding_id": binding_id,
            "expected_revision": 1}),
    );
    // The monitor's next tick — or the first worker claim — re-proves
    // the binding and finds it stale. The run invalidates.
    fx.wait_state(&run_id, "failed", "stale binding at claim");
    let shown = fx.show(&run_id);
    assert_eq!(shown["steps"][0]["state"], "failed", "{shown}");
    let reason = step_reason(&shown, 0);
    assert!(
        reason.contains("binding") || reason.contains("authorization"),
        "claim-time refusal carries the authority reason: {reason} / {shown}"
    );
    assert_eq!(
        fx.run_list(&install)["runs"].as_array().unwrap().len(),
        1,
        "claim refusal spawned a replacement run"
    );
}

/// A healthy door reporting a *moved* price is not a probe miss — it is
/// the real `Rejected` drift verdict: authority intact, price changed.
/// The monitor tick re-validates a live `running` run; a drifted quote
/// still invalidates it, naming the price change, while a *transient*
/// outage (asserted above) does not. This is the mirror image of the
/// Demo bug: the probe's answer is read, not its liveness.
#[test]
fn a_moved_live_price_still_invalidates() {
    let fx = Fx::start();
    let (install, _binding_id, run_id) = fx.live_run("drift");
    fx.dispatch_first(&run_id);
    // The binding is untouched and the door is up — but the price moved.
    fx.provider.set_posture(Posture::Priced);
    fx.wait_state(&run_id, "failed", "a moved price invalidates");
    let shown = fx.show(&run_id);
    assert_eq!(shown["steps"][0]["state"], "failed", "{shown}");
    let reason = step_reason(&shown, 0);
    assert!(
        !reason.trim().is_empty(),
        "price-drift step carries no reason: {shown}"
    );
    assert!(
        reason.contains("price") || reason.contains("capability"),
        "reason names the price/capability drift: {reason}"
    );
    assert_eq!(
        fx.run_list(&install)["runs"].as_array().unwrap().len(),
        1,
        "a drifted quote spawned a replacement run"
    );
}
