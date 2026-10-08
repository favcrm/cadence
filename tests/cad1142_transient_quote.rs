//! CAD-1142 result checks: a transient provider quote outage must not kill
//! a running app run, while genuinely stale authority still invalidates —
//! and every step failure carries its plain-words reason.
//!
//! A real in-process daemon (so the 1s monitor tick runs `advance_app_runs`
//! against the run, exactly like Demo) and a probe provider whose quote can
//! be toggled to fail the way the AgenticOS quote door did on Demo (~15s
//! read outage → `Rejected` → run invalidated before the worker's turn).
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

struct Probe {
    quote_down: Mutex<bool>,
    table: Mutex<&'static ToolTable>,
}

impl Probe {
    fn new() -> Arc<Self> {
        let table = ToolTable::from_json(&json!({"platform": PROVIDER,
            "manifest_version": "probe-tools@1",
            "tools": [{"tool": TOOL, "effect": "read", "scopes": ["provider.read"]}]}))
        .unwrap();
        Arc::new(Self {
            quote_down: Mutex::new(false),
            table: Mutex::new(Box::leak(Box::new(table))),
        })
    }
    fn set_quote_down(&self, down: bool) {
        *self.quote_down.lock().unwrap() = down;
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
        if *self.quote_down.lock().unwrap() {
            // Demo's AgenticOS quote door: unreachable → timeout → refusal.
            return Err("bound capability price discovery refused: door_unreachable".into());
        }
        if binding["config"]["mapping"]["tool"] != TOOL {
            return Err("not_allowlisted".into());
        }
        Ok(AppCapabilityQuote {
            schema: 1,
            currency: "USD".into(),
            unit_price_micros: 2000,
            units: 1,
            total_price_micros: 2000,
            price_revision: "probe-price/1".into(),
        })
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
        let root = tempfile::Builder::new().prefix("c1142").tempdir().unwrap();
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
    fn show(&self, id: &str) -> Value {
        self.op("app_run_show", json!({"run_id": id}))
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
}

/// A transient quote outage must not fail a running run: the run survives
/// monitor ticks that cannot reach the provider, then still invalidates on
/// genuinely stale authority — with the step carrying the real reason.
#[test]
fn transient_quote_outage_does_not_kill_the_run_but_stale_authority_does() {
    let fx = Fx::start();
    let installed = fx.op("app_workspace_install", json!({"source": fx.source()}));
    let install = installed["install_id"].as_str().unwrap().to_string();
    let rows = fx.op("connection_list", json!({}))["connections"].clone();
    let connection = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["provider"] == PROVIDER && row["account"] == "hosted")
        .expect("probe hosted connection")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let bound = fx.op(
        "app_binding_create",
        json!({"install_id": install, "slot": "source",
            "connection_id": connection, "request_id": "bind-1142"}),
    )["binding"]
        .clone();
    let run = fx
        .rpc(
            Asserted::Operator,
            "app_run_create",
            json!({"install_id": install, "workflow": "read",
                "inputs": {"handle": "probe", "writer": "writer"},
                "request_id": "run-1142", "owner_pm": "lead"}),
        )
        .unwrap();
    let id = run["id"].as_str().unwrap().to_string();
    fx.op(
        "app_run_approve",
        json!({"run_id": id, "digest": run["snapshot_digest"]}),
    );
    let dispatched = fx.op("app_run_dispatch", json!({"run_id": id}));
    assert_eq!(dispatched["state"], "running", "{dispatched}");
    assert_eq!(
        dispatched["steps"][0]["state"], "dispatched",
        "{dispatched}"
    );
    assert!(dispatched["steps"][0]["message_id"].is_string());

    // The provider door goes down. Monitor ticks keep re-validating; the
    // run must stay `running` instead of failing before the first turn.
    fx.provider.set_quote_down(true);
    std::thread::sleep(Duration::from_secs(4));
    let shown = fx.show(&id);
    assert_eq!(shown["state"], "running", "{shown}");
    assert_eq!(shown["steps"][0]["state"], "dispatched", "{shown}");

    // The quote recovers, then the binding is genuinely revoked: the next
    // tick must invalidate — and the step must name the real cause.
    fx.provider.set_quote_down(false);
    fx.op(
        "app_binding_revoke",
        json!({"install_id": install, "binding_id": bound["id"],
            "expected_revision": 1}),
    );
    fx.wait_state(&id, "failed", "stale binding invalidates");
    let shown = fx.show(&id);
    assert_eq!(shown["steps"][0]["state"], "failed", "{shown}");
    let reason = shown["steps"][0]["reason"].as_str().unwrap_or("");
    assert!(
        !reason.trim().is_empty(),
        "failed step carries no reason: {shown}"
    );
    assert!(
        reason.contains("binding"),
        "reason names the stale authority: {reason}"
    );
}
