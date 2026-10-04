//! CAD-1142 independent acceptance — written from the ticket's required
//! security behavior, not as an oracle for the candidate's output.
//!
//! Required boundary (Demo symptom: an Instagram `source` run failed at
//! s1 in ~16s with no delivered message and no shown reason):
//!
//! - A transient provider quote/transport outage is NOT silently
//!   reclassified as authority loss: a `running` run must survive
//!   monitor ticks that cannot reach the quote door, and an approved
//!   run's first dispatch must not be refused by a dead probe.
//! - Dispatch/admission still re-proves the binding revision, the
//!   capability allowance and the content at the real call, so a
//!   genuinely stale or revoked binding STILL refuses and invalidates —
//!   carrying the actual plain-words reason onto the failed step.
//! - A provider quote error (unreachable, auth-denied) or a corrupt
//!   price schema is an unknown probe verdict — it must not mint or
//!   destroy authority by itself.
//! - The operator rule holds: agent and unproven callers, and a forged
//!   field, are refused on every app-lifecycle route that runs the
//!   binding/authority checks — with nothing written.
//!
//! Every check drives a real in-process seam daemon (the monitor tick
//! that re-runs `advance_app_runs`, exactly like Demo) plus a probe
//! provider whose quote door can be toggled between postures. Temp
//! HOME/XDG/TMP/PM/state are explicit; no live provider or real agent
//! launch.
//!
//! Honest coverage boundary: this fixture cannot mint a real managed
//! endpoint or a verified `/proc` ancestry, so the strict-caller
//! `app_run_capability_call` gate (worker-claim re-proof, the
//! call-time `max_charge_minor` spend ceiling, detached-child refusal)
//! is NOT exercised here and stays BLOCKED pending a sanctioned runner
//! with a real peer. These rows assert only the public daemon/seam
//! surface — dispatch, monitor re-validation, operator lifecycle and
//! binding receipt checks.
#![cfg(feature = "test-seam")]

use cadence_agent::contract_fixture::{ToolTable, Verified};
use cadence_agent::platform::connections::{
    BoundActionMapping, CapabilityDescriptor, CapabilitySemantics, ProviderDescriptor,
};
use cadence_agent::platform::{AppCapabilityQuote, PlatformAdapter};
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon, store::Store};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PROVIDER: &str = "probe1142";
const TOOL: &str = "read_probe_listing";

/// The five quote postures this provider can be told to serve. `Up`
/// returns the reviewed quote; `Down` models Demo's unreachable door;
/// `Denied` is an auth/permission refusal; `Corrupt` is a well-formed
/// transport carrying a price schema that fails `valid()` — a corrupt
/// price response, which is not a probe-liveness miss and must be
/// refused like any error; `Priced` is a healthy door reporting a
/// *different* live price than the frozen one — the real `Rejected`
/// drift path (authority intact, price moved), which still invalidates.
#[derive(Clone, Copy)]
enum Posture {
    Up,
    Down,
    Denied,
    Corrupt,
    Priced,
}

/// A probe provider whose quote door's *actually observed* posture is
/// witnessed by a per-posture counter incremented inside
/// `quote_app_capability` — after the posture is read, so a count can
/// never be attributed to the wrong mode. The counters let a test prove
/// the monitor really re-quoted during a failure window (a positive
/// witness), instead of asserting an absence over a blind sleep.
struct Probe {
    posture: Mutex<Posture>,
    quotes_up: AtomicU64,
    quotes_down: AtomicU64,
    quotes_denied: AtomicU64,
    quotes_corrupt: AtomicU64,
    quotes_priced: AtomicU64,
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
            quotes_up: AtomicU64::new(0),
            quotes_down: AtomicU64::new(0),
            quotes_denied: AtomicU64::new(0),
            quotes_corrupt: AtomicU64::new(0),
            quotes_priced: AtomicU64::new(0),
            table: Mutex::new(Box::leak(Box::new(table))),
        })
    }
    fn set_posture(&self, posture: Posture) {
        *self.posture.lock().unwrap() = posture;
    }
    /// How many times `quote_app_capability` actually served `posture`.
    fn quotes(&self, posture: Posture) -> u64 {
        match posture {
            Posture::Up => &self.quotes_up,
            Posture::Down => &self.quotes_down,
            Posture::Denied => &self.quotes_denied,
            Posture::Corrupt => &self.quotes_corrupt,
            Posture::Priced => &self.quotes_priced,
        }
        .load(SeqCst)
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
        // Witness the *observed* posture, not the request's start: the
        // counter is incremented inside the matched arm, after the
        // posture is read under the lock, so a flip mid-call is never
        // mis-attributed.
        match *self.posture.lock().unwrap() {
            // Demo's AgenticOS quote door: unreachable → refusal.
            Posture::Down => {
                self.quotes_down.fetch_add(1, SeqCst);
                Err("bound capability price discovery refused: door_unreachable".into())
            }
            // An auth/permission denial is not a price; it must surface
            // as a refusal the run cannot mistake for authority loss or
            // silently skip.
            Posture::Denied => {
                self.quotes_denied.fetch_add(1, SeqCst);
                Err("bound capability price discovery refused: auth_denied".into())
            }
            // A corrupt schema: units out of range → `valid()` is false.
            Posture::Corrupt => {
                self.quotes_corrupt.fetch_add(1, SeqCst);
                Ok(AppCapabilityQuote {
                    schema: 1,
                    currency: "USD".into(),
                    unit_price_micros: 2000,
                    units: 0,
                    total_price_micros: 0,
                    price_revision: String::new(),
                })
            }
            // A live quote that moved since approval: a well-formed,
            // valid quote whose total/revision differ from the frozen
            // one. The run's drift comparison must reject — a genuine
            // `Rejected`, not a probe miss — so the run invalidates.
            Posture::Priced => {
                self.quotes_priced.fetch_add(1, SeqCst);
                Ok(AppCapabilityQuote {
                    schema: 1,
                    currency: "USD".into(),
                    unit_price_micros: 9000,
                    units: 1,
                    total_price_micros: 9000,
                    price_revision: "probe-price/2".into(),
                })
            }
            Posture::Up => {
                self.quotes_up.fetch_add(1, SeqCst);
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
    fn binding_show(&self, install: &str, binding_id: &str) -> Value {
        self.op(
            "app_binding_show",
            json!({"install_id": install, "binding_id": binding_id}),
        )["binding"]
            .clone()
    }
    fn binding_list(&self, install: &str) -> Value {
        self.op("app_binding_list", json!({"install_id": install}))
    }
    /// The worker's durable queue via the public `agent_inbox` peek — a
    /// read that consumes nothing. Returns `(message_ids, unread)`.
    fn inbox_peek(&self, alias: &str) -> (Vec<String>, i64) {
        let reply = self.op("agent_inbox", json!({"alias": alias, "peek": true}));
        let ids = reply["messages"]
            .as_array()
            .map(|m| {
                m.iter()
                    .filter_map(|row| row["id"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        (ids, reply["unread"].as_i64().unwrap_or(0))
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
    /// Wait until the monitor has made at least `min` NEW quote calls
    /// while the door is in `posture` — a positive witness that the
    /// re-validation loop actually ran inside the failure window. This
    /// never manufactures a call; it only observes the provider's own
    /// counter the daemon's real `app_capability_quote` drove. Times
    /// out (and fails) if the monitor never ticks or stops listing the
    /// run, so a stalled loop can't pass the row.
    fn wait_quote_attempts(&self, posture: Posture, baseline: u64, min: u64, what: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let observed = self.provider.quotes(posture);
            assert!(
                observed >= baseline,
                "{what}: {posture:?} counter went backwards ({observed} < {baseline})"
            );
            if observed >= baseline + min {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{what}: only {} new {posture:?} quote attempts after \
                 30s — the monitor never re-quoted inside the window",
                observed - baseline
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    /// Install → bind → create → approve, returning
    /// `(install_id, binding_id, run_id)` with the run `approved` but
    /// NOT yet dispatched. The same operator RPC chain the Demo host
    /// ran before s1 dispatched.
    fn approved_run(&self, tag: &str) -> (String, String, String) {
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
        let run_id = self.create_and_approve(&install, tag);
        (install, bound["id"].as_str().unwrap().to_string(), run_id)
    }
    /// Create + approve one run on an already-bound installation,
    /// returning the run id `approved` but undispatched. A second run
    /// reuses the slot's existing configured binding — a fresh
    /// `app_binding_create` would collide on the live slot.
    fn create_and_approve(&self, install: &str, tag: &str) -> String {
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
        id
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
//
// Red/green is UNPROVEN until a sanctioned runner executes these on both
// baselines; the rows are written so the assertions that name the Demo
// symptom intend to fail on pre-fix code (any probe error invalidated a
// `running` run) and to pass on the candidate. Nothing here claims a
// pass, a compile or readiness.
// --------------------------------------------------------------------

/// A transient quote outage on the monitor's re-validation ticks must not
/// reclassify as authority loss: a running run survives *observed* dead-
/// door re-quotes, then still invalidates — with the real reason — once
/// the binding is genuinely revoked.
///
/// The `Down` counter is the positive witness: we only assert the run
/// survived after the daemon's own quote path demonstrably re-quoted at
/// least twice in the outage window, so a stalled or emptied monitor
/// loop cannot make this row pass vacuously.
#[test]
fn transient_outage_survives_then_stale_binding_invalidates_with_reason() {
    let fx = Fx::start();
    let (install, binding_id, run_id) = fx.approved_run("survive");
    fx.dispatch_first(&run_id);

    // The provider door goes down. Record the baseline, then require the
    // monitor's own `app_capability_quote` to attempt >=2 more quotes in
    // the Down window before we assert survival — the window must really
    // have run, not merely elapsed.
    let baseline = fx.provider.quotes(Posture::Down);
    fx.provider.set_posture(Posture::Down);
    fx.wait_quote_attempts(Posture::Down, baseline, 2, "outage survival");
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
/// dispatch path's own binding/quote re-proof treats a dead door as a
/// probe miss and dispatches the step, not a refusal.
///
/// Intended to fail on pre-fix code (where the operator `app_run_dispatch`
/// call itself returns the `Rejected` the probe raised — an error return,
/// not a monitor invalidation), to pass on the candidate. UNPROVEN.
#[test]
fn outage_at_first_dispatch_still_dispatches() {
    let fx = Fx::start();
    let (install, _binding_id, run_id) = fx.approved_run("queued");
    // Approved, first kickoff pending, door down. Baseline + require the
    // dispatch path's own quote attempt to have run before asserting.
    let baseline = fx.provider.quotes(Posture::Down);
    fx.provider.set_posture(Posture::Down);
    let dispatched = fx.op("app_run_dispatch", json!({"run_id": run_id}));
    // The operator call itself drives a quote attempt (dispatch re-proves
    // the binding and quotes); prove it happened in the Down window.
    assert!(
        fx.provider.quotes(Posture::Down) > baseline,
        "the dispatch path never re-quoted inside the outage"
    );
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

/// A provider quote error — auth-denied or a corrupt price schema — is an
/// unknown probe verdict: it does not mint or destroy authority by
/// itself. The run stays running (witnessed by the error posture's own
/// counter across monitor re-quotes), and a *separately real* binding
/// revoke still invalidates it with a binding reason.
///
/// This proves only quote-error-vs-revoke behavior on the daemon's public
/// re-validation path. It does NOT assert a broker-spend refusal or a
/// distinct recorded verdict — the worker-claim/call path needs the real
/// strict-caller peer this fixture cannot mint (see file header).
#[test]
fn quote_errors_do_not_kill_the_run_but_binding_revoke_does() {
    for posture in [Posture::Denied, Posture::Corrupt] {
        let fx = Fx::start();
        let (install, binding_id, run_id) = fx.approved_run(match posture {
            Posture::Denied => "denied",
            _ => "corrupt",
        });
        fx.dispatch_first(&run_id);
        let baseline = fx.provider.quotes(posture);
        fx.provider.set_posture(posture);
        fx.wait_quote_attempts(posture, baseline, 2, "quote-error survival");
        let shown = fx.show(&run_id);
        assert_eq!(
            shown["state"], "running",
            "{posture:?} was reclassified as authority loss: {shown}"
        );
        assert_eq!(shown["steps"][0]["state"], "dispatched", "{shown}");

        // A genuinely revoked binding — a real authority change — still
        // invalidates, so the probe-error window did not mask real
        // refusals either.
        fx.provider.set_posture(Posture::Up);
        fx.op(
            "app_binding_revoke",
            json!({"install_id": install, "binding_id": binding_id,
                "expected_revision": 1}),
        );
        fx.wait_state(&run_id, "failed", "revoke after quote-error window");
        let shown = fx.show(&run_id);
        let reason = step_reason(&shown, 0);
        assert!(
            !reason.trim().is_empty() && reason.contains("binding"),
            "post-{posture:?} invalidation lost its binding reason: {shown}"
        );
    }
}

/// The operator rule is exercised at every route that runs the
/// binding/authority checks — agent and unproven callers and forged
/// fields are refused, and nothing is written.
///
/// Zero-mutation is checked on real protected state, not a blanket
/// "no events": refusal audit events may legitimately append. We pin
/// the binding's `revision`/`state`/`config`/`digest`, the target run's
/// `state`/`approved_digest`/`snapshot_digest`/steps, the run count, the
/// worker's queued-message set, and explicitly assert the forged
/// request-ids produced no run/binding/message rows. A seeded
/// non-dispatched sibling run is inspected for invalidation
/// side-effects.
#[test]
fn agent_unproven_and_forged_fields_are_refused_and_write_nothing() {
    let fx = Fx::start();
    let (install, binding_id, run_id) = fx.approved_run("gate");
    // A sibling approved-but-undispatched run on the same installation
    // (reusing the bound slot), to observe whether a refused call's
    // side-effects touch other runs.
    let sibling_run = fx.create_and_approve(&install, "gate-sib");

    // Baseline protected state.
    let binding_before = fx.binding_show(&install, &binding_id);
    let target_before = fx.show(&run_id);
    let sibling_before = fx.show(&sibling_run);
    let (msgs_before, unread_before) = fx.inbox_peek("writer");
    let runs_before = fx.run_list(&install)["runs"].as_array().unwrap().len();
    let bindings_before = fx.binding_list(&install)["bindings"]
        .as_array()
        .unwrap()
        .len();

    // Non-operator callers are refused on each lifecycle verb that
    // reaches the run/binding proof. Each call is refused at the gate —
    // asserted by the error AND by nothing changing below.
    for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        for (method, params) in [
            ("app_run_create", json!({"install_id": install, "workflow": "read",
                "inputs": {"handle":"probe","writer":"writer"},
                "request_id":"forge-create", "owner_pm":"lead"})),
            ("app_run_approve", json!({"run_id": run_id, "digest": "sha256:forged"})),
            ("app_run_dispatch", json!({"run_id": run_id})),
            ("app_run_cancel", json!({"run_id": run_id})),
            ("app_binding_revoke", json!({"install_id": install,
                "binding_id": binding_id, "expected_revision": 1})),
            ("app_binding_create", json!({"install_id": install, "slot": "source",
                "connection_id": "forged-conn", "request_id": "forge-bind"})),
        ] {
            assert!(
                fx.rpc(who.clone(), method, params.clone()).is_err(),
                "{who:?} reached {method} with {params}"
            );
        }
    }

    // Forged identity/authority fields inside an operator verb are
    // refused, never read: unknown field, forged digest, forged run id.
    for params in [
        json!({"install_id": install, "workflow": "read",
            "inputs": {"handle":"probe","writer":"writer"},
            "request_id":"forge-field", "owner_pm":"lead",
            "as_alias":"lead", "approval_id":"apv-forged"}),
        json!({"run_id": run_id, "digest": "sha256:forged-digest"}),
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

    // ---- zero mutation on protected business state ----
    // The binding is untouched: same revision, still configured, same
    // config and digest.
    let binding_after = fx.binding_show(&install, &binding_id);
    assert_eq!(
        binding_after["revision"], binding_before["revision"],
        "a refused call bumped the binding revision: {binding_after}"
    );
    assert_eq!(
        binding_after["state"], binding_before["state"],
        "a refused call changed the binding state: {binding_after}"
    );
    assert_eq!(
        binding_after["config"], binding_before["config"],
        "a refused call rewrote the binding config: {binding_after}"
    );
    assert_eq!(
        binding_after["digest"], binding_before["digest"],
        "a refused call moved the binding digest: {binding_after}"
    );
    // No forged `forge-bind` binding row appeared.
    assert_eq!(
        fx.binding_list(&install)["bindings"].as_array().unwrap().len(),
        bindings_before,
        "a refused call wrote a binding"
    );
    // The target run is unchanged: same approval state, same
    // approved_digest/snapshot_digest, same step set.
    let target_after = fx.show(&run_id);
    assert_eq!(
        target_after["state"], target_before["state"],
        "a refused call moved the target run: {target_after}"
    );
    assert_eq!(
        target_after["approved_digest"], target_before["approved_digest"],
        "a refused approve wrote approved_digest: {target_after}"
    );
    assert_eq!(
        target_after["snapshot_digest"], target_before["snapshot_digest"],
        "a refused call rewrote the snapshot: {target_after}"
    );
    assert_eq!(
        target_after["steps"], target_before["steps"],
        "a refused call altered the steps: {target_after}"
    );
    // The sibling run is untouched (no invalidation side-effects).
    let sibling_after = fx.show(&sibling_run);
    assert_eq!(
        sibling_after["state"], sibling_before["state"],
        "a refused call touched the sibling run: {sibling_after}"
    );
    assert_eq!(
        sibling_after["approved_digest"], sibling_before["approved_digest"],
        "a refused call touched the sibling approval: {sibling_after}"
    );
    // No forged run or message was written: run count unchanged, the
    // worker's queued set identical, no `forge-create`/`forge-field` row.
    assert_eq!(
        fx.run_list(&install)["runs"].as_array().unwrap().len(),
        runs_before,
        "a refused call wrote a run"
    );
    let (msgs_after, unread_after) = fx.inbox_peek("writer");
    assert_eq!(msgs_after, msgs_before, "a refused call queued a message");
    assert_eq!(
        unread_after, unread_before,
        "a refused call changed the queued count"
    );
    // The operator's real approval still lands (gate admits, not a
    // blanket refuse).
    fx.op(
        "app_run_approve",
        json!({"run_id": run_id, "digest": target_after["snapshot_digest"]}),
    );
    assert_eq!(fx.show(&run_id)["state"], "approved");
}

/// A stale binding refuses the run on the daemon's own re-validation —
/// not just an operator call. Revoking the binding while the kickoff
/// sits queued makes the monitor's next `dispatch_app_run` re-proof
/// refuse (`Rejected`) and `app_run_invalidate` fail the run, the step
/// carrying the authority reason.
///
/// Boundary: this is the monitor/dispatch re-proof of a queued run, the
/// same `app_run_binding_current` the worker-claim path calls. The
/// actual worker `app_run_capability_call` strict-caller admission and
/// call-time spend ceiling are NOT exercised — they need a real managed
/// endpoint/peer this fixture cannot mint (see file header).
#[test]
fn stale_binding_revoked_while_queued_invalidates_with_reason() {
    let fx = Fx::start();
    let (install, binding_id, run_id) = fx.approved_run("claim");
    fx.dispatch_first(&run_id);
    // The kickoff is queued but unclaimed. A binding revoke — a real
    // authority change — must invalidate via the daemon's re-proof.
    fx.op(
        "app_binding_revoke",
        json!({"install_id": install, "binding_id": binding_id,
            "expected_revision": 1}),
    );
    fx.wait_state(&run_id, "failed", "revoked binding invalidates a queued run");
    let shown = fx.show(&run_id);
    assert_eq!(shown["steps"][0]["state"], "failed", "{shown}");
    let reason = step_reason(&shown, 0);
    assert!(
        reason.contains("binding") || reason.contains("authorization"),
        "revoke invalidation carries the authority reason: {reason} / {shown}"
    );
    assert_eq!(
        fx.run_list(&install)["runs"].as_array().unwrap().len(),
        1,
        "revoke invalidation spawned a replacement run"
    );
}

/// A healthy door reporting a *moved* price is not a probe miss — it is
/// the real `Rejected` drift verdict: authority intact, price changed.
/// The monitor tick re-validates a live `running` run; a drifted quote
/// still invalidates it, naming the price/capability, while a *transient*
/// outage (asserted above) does not. This is the mirror image of the
/// Demo bug: the probe's answer is read, not its liveness.
///
/// Intended to fail on pre-fix code is moot here — this drift already
/// invalidated before; the row pins that the *fixed* path still
/// distinguishes a real `Rejected` verdict from a liveness miss.
/// UNPROVEN.
#[test]
fn a_moved_live_price_still_invalidates() {
    let fx = Fx::start();
    let (install, _binding_id, run_id) = fx.approved_run("drift");
    fx.dispatch_first(&run_id);
    // The binding is untouched and the door is up — but the price moved.
    let baseline = fx.provider.quotes(Posture::Priced);
    fx.provider.set_posture(Posture::Priced);
    // Wait for the drift verdict's own quote attempt (positive witness
    // that the monitor re-quoted the moved price) then the invalidation.
    fx.wait_quote_attempts(Posture::Priced, baseline, 1, "price drift re-quote");
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
