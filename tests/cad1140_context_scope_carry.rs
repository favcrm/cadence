//! CAD-1140 reviewer-written acceptance (Spec/security review of PR #956):
//! the Default-context case. A binding scoped to an app context
//! (`context_id` set) is carried by an upgrade whose slot declarations
//! are byte-identical exactly like an installation-scope binding: it
//! moves to the new bundle digest, keeps its `context_id` and context
//! receipt, lists as current, quotes, and a run in that context approves
//! and dispatches with no rebind. A context-scoped binding whose slot
//! declaration widened is never carried: it keeps its old pin and the
//! run in that context stays refused until the operator binds again.
//!
//! Review finding (Critical) regression: a binding a previous upgrade
//! left on an older pin (its declaration was widened) must not be staged
//! when a later bundle restores that declaration. The later upgrade
//! commits, the stale row stays exactly as it was, no upgrade-pending
//! marker is left to block the catalog, and the operator can rebind.
//!
//! Same real in-process daemon (CAD-482 test seam) and runtime-written
//! package as `cad1140_upgrade_binding_carry.rs`; no production code is
//! called directly.
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
use std::sync::Arc;
use std::time::Duration;

const PROVIDER: &str = "probe";
const TOOL: &str = "probe.read.posts";

/// A static provider: one reviewed read action whose descriptor never
/// moves — this ticket's drift is the package's slot declaration, not
/// the provider's. A quote still proves the binding is usable.
struct Static;

impl PlatformAdapter for Static {
    fn table(&self) -> &ToolTable {
        static TABLE: std::sync::OnceLock<ToolTable> = std::sync::OnceLock::new();
        TABLE.get_or_init(|| {
            ToolTable::from_json(&json!({"platform": PROVIDER,
                "manifest_version": "probe-tools@1",
                "tools": [{"tool": TOOL, "effect": "read",
                    "scopes": ["provider.read"]}]}))
            .unwrap()
        })
    }
    fn connection_descriptor(&self) -> Option<ProviderDescriptor> {
        let scopes = vec!["provider.read".to_string()];
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
                scopes: scopes.clone(),
                effect: "read".into(),
                semantics: CapabilitySemantics::MetadataRead,
            }],
            action_mappings: vec![BoundActionMapping {
                capability: "probe.read".into(),
                version: 1,
                action: "list_items".into(),
                resource_kind: "connection_account".into(),
                tool: TOOL.into(),
                scopes,
                effect: "read".into(),
                semantics: CapabilitySemantics::MetadataRead,
                input_contract: "probe.query@1".into(),
                output_contract: "probe.receipt@1".into(),
            }],
        })
    }
    fn connection_registration(&self) -> Option<String> {
        Some("probe:probe-connections/1:probe-tools@1".into())
    }
    fn app_credentialless_account(&self, account: &str) -> bool {
        account == "hosted"
    }
    fn quote_app_capability(
        &self,
        _credential: &[u8],
        _binding: &Value,
    ) -> Result<AppCapabilityQuote, String> {
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

/// `source` declares `probe.read`'s `list_items`; `image` declares the
/// same capability's `view_items` — never registered in the provider's
/// descriptor, so nothing can bind `image`. The carry journal must not
/// manufacture a receipt for it.
const MANIFEST: &str = r#"---
app: probe-reader
title: Probe Reader
version: 'VERSION'
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
    image:
      schema: 1
      capability: probe.read
      version: 1
      action: view_items
      resource_kind: connection_account
      effect: read
---

# Probe Reader

Reads one bounded listing through the bound `source` capability.
"#;

/// `{{ACTION}}` widens `source` from `list_items` to `view_items` — a
/// different action is a different contract, so the binding must not
/// silently carry.
const MANIFEST_WIDE: &str = r#"---
app: probe-reader
title: Probe Reader
version: 'VERSION'
summary: Read one bounded listing through a bound source.
needs:
  connections: []
  capabilities:
    source:
      schema: 1
      capability: probe.read
      version: 1
      action: {{ACTION}}
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
        let root = tempfile::Builder::new().prefix("c1140x").tempdir().unwrap();
        let mut fx = Self {
            root,
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
        };
        cadence_agent::issue::Pm::init(&fx.pm()).unwrap();
        fx.package(MANIFEST, "1.0.0", WORKFLOW);
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
            Arc::new(Static) as Arc<dyn PlatformAdapter>,
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
    /// The generic package, written at runtime: manifest `template` at
    /// `version`, with `workflow` as the read workflow's body.
    fn package(&self, template: &str, version: &str, workflow: &str) {
        std::fs::create_dir_all(self.source().join("workflows")).unwrap();
        std::fs::write(
            self.source().join("app.md"),
            template.replace("VERSION", version),
        )
        .unwrap();
        std::fs::write(self.source().join("workflows/read.md"), workflow).unwrap();
    }
    /// A PM and its managed worker, registered as `cadence join` would.
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
    fn install(&self) -> Value {
        let source = self.source();
        self.op("app_workspace_install", json!({"source": source}))
    }
    fn hosted(&self) -> String {
        let rows = self.op("connection_list", json!({}))["connections"].clone();
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["provider"] == PROVIDER && row["account"] == "hosted");
        row.expect("probe hosted connection")["id"]
            .as_str()
            .unwrap()
            .into()
    }
    /// The live binding row for `slot`, or `Value::Null` when none.
    fn slot_binding(&self, install: &str, slot: &str) -> Value {
        let listed = self.op("app_binding_list", json!({"install_id": install}));
        listed["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["slot"] == slot)
            .cloned()
            .unwrap_or(Value::Null)
    }
    /// The reviewed new digest `upgrade_check` returns for `source`.
    fn upgrade_check(&self, install: &str, installed: &Value) -> Value {
        self.op(
            "app_workspace_upgrade_check",
            json!({"install_id": install,
                "source": self.source(),
                "expected_digest": installed["digest"],
                "expected_generation": installed["catalog_generation"]}),
        )
    }
    /// The operator's upgrade call parameters after a `check`.
    fn upgrade_params(
        &self,
        install: &str,
        installed: &Value,
        check: &Value,
        request: &str,
    ) -> Value {
        json!({"install_id": install, "source": self.source(),
            "expected_digest": installed["digest"],
            "expected_generation": installed["catalog_generation"],
            "expected_new_digest": check["digest"], "request_id": request})
    }
    /// The current bundle digest the installation row reports.
    fn digest(&self, install: &str) -> String {
        self.op("app_workspace_show", json!({"install_id": install}))["digest"]
            .as_str()
            .unwrap()
            .to_string()
    }
    /// The journaled carry set `.apps/upgrade-journals/<id>/<request>.yaml`
    /// retains — read as a consumer would, never through production code.
    fn journaled_carries(&self, install: &str, request: &str) -> Vec<String> {
        let path = self
            .pm()
            .join(".apps/upgrade-journals")
            .join(install)
            .join(format!("{request}.yaml"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("upgrade journal {} unreadable: {e}", path.display()));
        let journal: Value = serde_yaml::from_str(&text).unwrap();
        journal["bindings"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }
    fn bind_install(&self, install: &str, request: &str) -> Value {
        let params = json!({"install_id": install, "slot": "source",
            "connection_id": self.hosted(), "request_id": request});
        self.op("app_binding_create", params)["binding"].clone()
    }
    /// The operator's context, created as the board's Default context is.
    fn context(&self, install: &str) -> String {
        let made = self.op(
            "app_context_create",
            json!({"install_id": install, "label": "Default", "input_defaults": {},
                "request_id": "ctx-1140"}),
        );
        made["context"]["id"]
            .as_str()
            .or_else(|| made["id"].as_str())
            .unwrap_or_else(|| panic!("context id: {made}"))
            .to_string()
    }
    fn bind_in(&self, install: &str, context: &str) -> Value {
        let params = json!({"install_id": install, "context_id": context,
            "slot": "source", "connection_id": self.hosted(),
            "request_id": "bind-ctx-1140"});
        self.op("app_binding_create", params)["binding"].clone()
    }
    /// The live binding row for `slot` in `context`, or `Value::Null`.
    fn context_binding(&self, install: &str, context: &str, slot: &str) -> Value {
        let listed = self.op(
            "app_binding_list",
            json!({"install_id": install, "context_id": context}),
        );
        listed["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["slot"] == slot && row["state"] == "configured")
            .cloned()
            .unwrap_or(Value::Null)
    }
    fn run_in(&self, install: &str, context: &str, request: &str) -> cadence_agent::Result<Value> {
        let params = json!({"install_id": install, "workflow": "read",
            "inputs": {"handle": "probe", "writer": "writer"},
            "request_id": request, "owner_pm": "lead", "context_id": context});
        self.rpc(Asserted::Operator, "app_run_create", params)
    }
}

/// A context-scoped binding is carried by a same-declaration upgrade:
/// same id, revision +1, `context_id` and the context receipt unchanged,
/// only `config.bundle_digest` moved; it lists as current, quotes in its
/// context, and a run in that context approves and dispatches without a
/// rebind.
#[test]
fn context_scoped_binding_carries_and_its_runs_continue() {
    let fx = Fx::start();
    let installed = fx.install();
    let install = installed["install_id"].as_str().unwrap().to_string();
    let old_digest = installed["digest"].as_str().unwrap().to_string();
    let context = fx.context(&install);
    let bound = fx.bind_in(&install, &context);
    let bound_id = bound["id"].as_str().unwrap().to_string();
    assert_eq!(bound["context_id"], json!(context), "{bound}");
    assert_eq!(bound["config"]["bundle_digest"], json!(old_digest));
    assert_eq!(
        fx.slot_binding(&install, "source")["context_id"],
        json!(context),
        "the only live `source` binding is the context-scoped one"
    );

    fx.package(MANIFEST, "1.0.1", WORKFLOW);
    let check = fx.upgrade_check(&install, &installed);
    let new_digest = check["digest"].as_str().unwrap().to_string();
    assert_ne!(new_digest, old_digest);
    let upgraded = fx.op(
        "app_workspace_upgrade",
        fx.upgrade_params(&install, &installed, &check, "up-1140-ctx"),
    );
    assert_eq!(upgraded["committed"], true, "{upgraded}");
    assert_eq!(fx.digest(&install), new_digest, "{upgraded}");
    assert_eq!(
        upgraded["compatibility"]["carried_bindings"],
        json!([bound_id]),
        "{upgraded}"
    );
    assert_eq!(
        fx.journaled_carries(&install, "up-1140-ctx"),
        vec![bound_id.clone()]
    );

    let carried = fx.context_binding(&install, &context, "source");
    assert_eq!(carried["id"], json!(bound_id), "{carried}");
    assert_eq!(carried["context_id"], json!(context), "{carried}");
    assert_eq!(carried["revision"], json!(2), "{carried}");
    let mut expected = bound["config"].clone();
    expected["bundle_digest"] = json!(new_digest);
    assert_eq!(carried["config"], expected, "only bundle_digest moved");
    assert_eq!(carried["config"]["context"], bound["config"]["context"]);
    assert_eq!(carried["drift"]["state"], json!("current"), "{carried}");

    let quoted = fx.op(
        "app_binding_quote",
        json!({"install_id": install, "context_id": context, "slot": "source"}),
    );
    assert_eq!(quoted["quote"]["total_price_micros"], 2000, "{quoted}");
    let run = fx.run_in(&install, &context, "run-1140-ctx").unwrap();
    assert_eq!(run["context_id"], json!(context), "{run}");
    assert_eq!(run["state"], "awaiting_approval", "{run}");
    let approved = fx.op(
        "app_run_approve",
        json!({"run_id": run["id"], "digest": run["snapshot_digest"]}),
    );
    assert_eq!(approved["state"], "approved", "{approved}");
    let dispatched = fx.op("app_run_dispatch", json!({"run_id": run["id"]}));
    assert_eq!(dispatched["state"], "running", "{dispatched}");
    assert!(!fx.pm().join(".apps/upgrade-pending.yaml").exists());
}

/// A context-scoped binding whose slot declaration widened is never
/// carried: it keeps its old pin and revision, the journal stages
/// nothing, and a run in that context is refused until the operator
/// binds the slot again (the Confirm path).
#[test]
fn context_scoped_binding_with_widened_declaration_is_not_carried() {
    let fx = Fx::start();
    let installed = fx.install();
    let install = installed["install_id"].as_str().unwrap().to_string();
    let old_digest = installed["digest"].as_str().unwrap().to_string();
    let context = fx.context(&install);
    let bound = fx.bind_in(&install, &context);
    let bound_id = bound["id"].as_str().unwrap().to_string();

    fx.package(
        &MANIFEST_WIDE.replace("{{ACTION}}", "view_items"),
        "1.0.1",
        WORKFLOW,
    );
    let check = fx.upgrade_check(&install, &installed);
    let upgraded = fx.op(
        "app_workspace_upgrade",
        fx.upgrade_params(&install, &installed, &check, "up-1140-ctx-wide"),
    );
    assert_eq!(upgraded["committed"], true, "{upgraded}");
    assert_eq!(
        upgraded["compatibility"]["incompatible_bindings"],
        json!([bound_id]),
        "{upgraded}"
    );
    assert_eq!(upgraded["compatibility"]["carried_bindings"], json!([]));
    assert_eq!(
        fx.journaled_carries(&install, "up-1140-ctx-wide"),
        Vec::<String>::new()
    );

    let kept = fx.context_binding(&install, &context, "source");
    assert_eq!(kept["id"], json!(bound_id), "{kept}");
    assert_eq!(kept["context_id"], json!(context), "{kept}");
    assert_eq!(kept["revision"], json!(1), "the row is untouched: {kept}");
    assert_eq!(kept["config"], bound["config"], "{kept}");
    assert_eq!(kept["config"]["bundle_digest"], json!(old_digest));

    let run = fx.run_in(&install, &context, "run-1140-ctx-wide");
    assert!(run.is_err(), "run must not ride a stale pin: {run:?}");
}

/// Regression for the review's Critical: v1 binds `source`; v2 widens
/// `source`, so the binding correctly stays on the v1 pin; v3 restores
/// the v1 declaration. The v3 upgrade must commit without staging the
/// stale v1-pinned row (it is not a receipt of the bundle being
/// replaced), leave the row untouched, leave no upgrade-pending marker,
/// keep the catalog readable, and leave the operator's rebind available.
#[test]
fn stale_pin_with_restored_declaration_does_not_wedge_the_upgrade() {
    let fx = Fx::start();
    let v1 = fx.install();
    let install = v1["install_id"].as_str().unwrap().to_string();
    let v1_digest = v1["digest"].as_str().unwrap().to_string();
    let bound = fx.bind_install(&install, "bind-stale-1");
    let bound_id = bound["id"].as_str().unwrap().to_string();

    fx.package(
        &MANIFEST_WIDE.replace("{{ACTION}}", "view_items"),
        "1.0.1",
        WORKFLOW,
    );
    let check2 = fx.upgrade_check(&install, &v1);
    let up2 = fx.op(
        "app_workspace_upgrade",
        fx.upgrade_params(&install, &v1, &check2, "up-1140-stale-2"),
    );
    assert_eq!(up2["committed"], true, "{up2}");
    let stale = fx.slot_binding(&install, "source");
    assert_eq!(stale["id"], json!(bound_id), "{stale}");
    assert_eq!(
        stale["config"]["bundle_digest"],
        json!(v1_digest),
        "{stale}"
    );

    let v2 = fx.op("app_workspace_show", json!({"install_id": install}));
    fx.package(MANIFEST, "1.0.2", WORKFLOW);
    let check3 = fx.upgrade_check(&install, &v2);
    let v3_digest = check3["digest"].as_str().unwrap().to_string();
    let up3 = fx
        .rpc(
            Asserted::Operator,
            "app_workspace_upgrade",
            fx.upgrade_params(&install, &v2, &check3, "up-1140-stale-3"),
        )
        .unwrap_or_else(|e| panic!("v3 upgrade must commit: {e}"));
    assert_eq!(up3["committed"], true, "{up3}");
    assert_eq!(
        up3["compatibility"]["carried_bindings"],
        json!([]),
        "a row pinned to an older bundle is never staged: {up3}"
    );
    assert_eq!(
        fx.journaled_carries(&install, "up-1140-stale-3"),
        Vec::<String>::new()
    );
    assert!(
        !fx.pm().join(".apps/upgrade-pending.yaml").exists(),
        "no upgrade-pending marker may be left behind"
    );
    let shown = fx
        .rpc(
            Asserted::Operator,
            "app_workspace_show",
            json!({"install_id": install}),
        )
        .unwrap_or_else(|e| panic!("the catalog must still load: {e}"));
    assert_eq!(shown["digest"], json!(v3_digest), "{shown}");

    let listed = fx.op("app_binding_list", json!({"install_id": install}));
    let kept = listed["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == json!(bound_id))
        .cloned()
        .expect("the stale row is still listed");
    assert_eq!(kept["revision"], json!(1), "{kept}");
    assert_eq!(kept["config"], bound["config"], "{kept}");

    // The rebind (Confirm) path is still open under v3, and a run follows.
    let rebound = fx.bind_install(&install, "bind-stale-3");
    assert_eq!(
        rebound["config"]["bundle_digest"],
        json!(v3_digest),
        "{rebound}"
    );
    let params = json!({"install_id": install, "workflow": "read",
        "inputs": {"handle": "probe", "writer": "writer"},
        "request_id": "run-1140-stale", "owner_pm": "lead"});
    let run = fx
        .rpc(Asserted::Operator, "app_run_create", params)
        .unwrap_or_else(|e| panic!("run after rebind: {e}"));
    assert_eq!(run["state"], "awaiting_approval", "{run}");
}
