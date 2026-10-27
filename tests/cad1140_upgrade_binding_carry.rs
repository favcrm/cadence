//! CAD-1140 independent acceptance: a package upgrade whose slot
//! declarations are byte-identical carries each configured binding's
//! receipt onto the new bundle digest — no rebind, no operator step —
//! and the app stays usable. A widened declaration is never carried:
//! the binding keeps its old pin and the drift/Confirm path still owns
//! it. The upgrade is the operator's call on both surfaces: an agent or
//! unattributable caller is refused by the daemon RPC and by its board
//! peer, and no upgrade is staged for them.
//!
//! The checks run against a real in-process daemon and board (the
//! CAD-482 test seam asserts the caller), a package written at runtime,
//! and the installed app's own listing/run/quotes — never a fake. Each
//! test names the ticket acceptance line it proves.
#![cfg(feature = "test-seam")]

use cadence_agent::contract_fixture::{ToolTable, Verified};
use cadence_agent::platform::connections::{
    BoundActionMapping, CapabilityDescriptor, CapabilitySemantics, ProviderDescriptor,
};
use cadence_agent::platform::{AppCapabilityQuote, PlatformAdapter};
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
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

/// The workflow file for the widened bundle — `view_items` on `source`.
const WORKFLOW_WIDE: &str = r#"---
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
        let root = tempfile::Builder::new().prefix("c1140").tempdir().unwrap();
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
    fn bind(&self, install: &str) -> Value {
        let params = json!({"install_id": install, "slot": "source",
            "connection_id": self.hosted(), "request_id": "bind-1140"});
        self.op("app_binding_create", params)["binding"].clone()
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
    fn run(&self, who: Asserted, install: &str, request: &str) -> cadence_agent::Result<Value> {
        let params = json!({"install_id": install, "workflow": "read",
            "inputs": {"handle": "probe", "writer": "writer"},
            "request_id": request, "owner_pm": "lead"});
        self.rpc(who, "app_run_create", params)
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
    /// A board on 3110-3199 and the operator's session from the real
    /// login-link exchange. Answers `(base, host, cookie, session key)`.
    fn board(&mut self) -> (String, String, String, String) {
        let mut port = 3110 + (std::process::id() % 80) as u16;
        loop {
            let (startup, ready) = std::sync::mpsc::channel();
            let opts = cadence_agent::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(self.stop.clone()),
                startup: Some(startup),
                test_seam: true,
                ..Default::default()
            };
            let (state, pm) = (self.state(), self.pm());
            let thread =
                std::thread::spawn(move || drop(cadence_agent::ui::serve(&state, &pm, &opts)));
            match ready.recv_timeout(Duration::from_secs(20)).unwrap() {
                Ok(()) => {
                    self.threads.push(thread);
                    break;
                }
                Err(_) if port < 3199 => port += 1,
                Err(kind) => panic!("board could not bind: {kind:?}"),
            }
            thread.join().unwrap();
        }
        let host = format!("cadence-{port}.localhost:{port}");
        let base = format!("http://127.0.0.1:{port}");
        cadence_agent::operator_auth::ensure_secret(&self.state()).unwrap();
        let secret = cadence_agent::operator_auth::read_secret(&self.state()).unwrap();
        let nonce = self.op(
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )["nonce"]
            .clone();
        let (status, body, cookie) = self.http(
            &base,
            &host,
            "operator",
            None,
            "POST",
            "/api/session",
            Some(json!({"nonce": nonce})),
        );
        assert_eq!(status, 200, "{body}");
        let cookie = cookie.expect("session cookie");
        let key = body["session_key"].as_str().unwrap().to_string();
        (base, host, cookie, key)
    }
    /// One loopback board request asserted as `who`, with an optional
    /// operator session. Answers `(status, body, set-cookie)`.
    #[allow(clippy::too_many_arguments)]
    fn http(
        &self,
        base: &str,
        host: &str,
        who: &str,
        session: Option<(&str, &str)>,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value, Option<String>) {
        let config = ureq::Agent::config_builder().http_status_as_error(false);
        let agent: ureq::Agent = config.build().into();
        let token = Seam::token_at(&self.state()).unwrap();
        let url = format!("{base}{path}");
        let headers = |request: ureq::RequestBuilder<ureq::typestate::WithBody>| {
            let request = request
                .header("Host", host)
                .header("X-Cadence-Board", "1")
                .header("Origin", format!("http://{host}"))
                .header(AS_HEADER, who)
                .header(TOKEN_HEADER, &token)
                .header("Content-Type", "application/json");
            match session {
                Some((cookie, key)) => request
                    .header("Cookie", cookie)
                    .header("X-Cadence-Session", key),
                None => request,
            }
        };
        let mut response = if method == "GET" {
            let mut request = agent.get(&url);
            request = request
                .header("Host", host)
                .header("X-Cadence-Board", "1")
                .header("Origin", format!("http://{host}"))
                .header(AS_HEADER, who)
                .header(TOKEN_HEADER, &token);
            if let Some((cookie, key)) = session {
                request = request
                    .header("Cookie", cookie)
                    .header("X-Cadence-Session", key);
            }
            request.call().unwrap()
        } else {
            headers(agent.post(&url))
                .send(body.unwrap_or(json!({})).to_string())
                .unwrap()
        };
        let cookie = response
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .map(|v| v[..v.find(';').unwrap_or(v.len())].to_owned());
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().unwrap_or_default();
        (
            status,
            serde_json::from_str(&text).unwrap_or(json!({"raw": text})),
            cookie,
        )
    }
}

/// Acceptance 1: an upgrade that changes only uncontracted bytes
/// (`version`, screen/guide text, a workflow body) while every slot
/// declaration stays byte-identical rebuilds each configured binding's
/// receipt onto the new bundle digest — same binding id, revision +1,
/// only `config.bundle_digest` moved — and the app needs no operator
/// step: the binding lists as current under the new bundle, its quote
/// prices, and a run created after the upgrade approves and dispatches
/// without a rebind. Declared-but-unbound slots stage nothing, and the
/// retained journal carries only real binding receipts.
#[test]
fn unchanged_declarations_carry_the_binding_and_runs_continue() {
    let fx = Fx::start();
    let installed = fx.install();
    let install = installed["install_id"].as_str().unwrap().to_string();
    let old_digest = installed["digest"].as_str().unwrap().to_string();
    let bound = fx.bind(&install);
    let bound_id = bound["id"].as_str().unwrap().to_string();
    assert_eq!(bound["config"]["bundle_digest"], json!(old_digest));

    // Same declarations, different bundle bytes — the Demo failure:
    // version bump plus body-only file changes.
    fx.package(MANIFEST, "1.0.1", WORKFLOW);
    let check = fx.upgrade_check(&install, &installed);
    let new_digest = check["digest"].as_str().unwrap().to_string();
    assert_ne!(new_digest, old_digest);

    let upgraded = fx.op(
        "app_workspace_upgrade",
        fx.upgrade_params(&install, &installed, &check, "up-1140-same"),
    );
    assert_eq!(upgraded["committed"], true, "{upgraded}");
    assert_eq!(fx.digest(&install), new_digest, "{upgraded}");
    // The preflight report the upgrade row carries names this binding as
    // carried and none as needing a rebind.
    assert!(
        upgraded["compatibility"]["carried_bindings"]
            .as_array()
            .unwrap()
            .contains(&json!(bound_id)),
        "preflight must stage the unchanged binding: {upgraded}"
    );
    assert_eq!(
        upgraded["compatibility"]["incompatible_bindings"],
        json!([]),
        "{upgraded}"
    );

    // The receipt was rebuilt, not rebound: same id, new bundle pin.
    let carried = fx.slot_binding(&install, "source");
    assert_eq!(carried["id"], json!(bound_id), "{carried}");
    assert_eq!(carried["state"], json!("configured"), "{carried}");
    assert_eq!(
        carried["config"]["bundle_digest"],
        json!(new_digest),
        "{carried}"
    );
    assert_eq!(carried["revision"], json!(2), "{carried}");
    let mut expected = bound["config"].clone();
    expected["bundle_digest"] = json!(new_digest);
    assert_eq!(carried["config"], expected, "only bundle_digest moved");
    assert_eq!(carried["drift"]["state"], json!("current"), "{carried}");

    // Readiness without a rebind: the quote prices and a run created
    // after the upgrade flows through approval to dispatch.
    let quoted = fx
        .rpc(
            Asserted::Operator,
            "app_binding_quote",
            json!({"install_id": install, "slot": "source"}),
        )
        .unwrap();
    assert_eq!(quoted["quote"]["total_price_micros"], 2000, "{quoted}");
    let run = fx.run(Asserted::Operator, &install, "run-1140").unwrap();
    assert_eq!(run["state"], "awaiting_approval", "{run}");
    let approved = fx.op(
        "app_run_approve",
        json!({"run_id": run["id"], "digest": run["snapshot_digest"]}),
    );
    assert_eq!(approved["state"], "approved", "{approved}");
    let dispatched = fx.op("app_run_dispatch", json!({"run_id": run["id"]}));
    assert_eq!(dispatched["state"], "running", "{dispatched}");

    // The declared-but-never-bound `image` slot got no receipt: the
    // carry journals real bindings only.
    assert!(fx.slot_binding(&install, "image").is_null());
    assert_eq!(
        fx.journaled_carries(&install, "up-1140-same"),
        vec![bound_id.clone()],
        "the journal stages exactly the configured binding"
    );
    // The carry moved exactly one receipt field — proof is the row
    // itself above — and no pending upgrade is left behind.
    assert!(
        !fx.pm().join(".apps/upgrade-pending.yaml").exists(),
        "pending marker must be cleared"
    );
}

/// Acceptance 2: widening the slot declaration between packages —
/// `source` moves from `list_items` to `view_items` — never carries the
/// receipt. The binding keeps its old digest pin, the listing still
/// names it for the rebind path, and nothing touches the row: quote
/// and run stay gated on the operator's Confirm, exactly the drift
/// contract CAD-1119 proved.
#[test]
fn widened_declaration_is_never_carried() {
    let fx = Fx::start();
    let installed = fx.install();
    let install = installed["install_id"].as_str().unwrap().to_string();
    let old_digest = installed["digest"].as_str().unwrap().to_string();
    let bound = fx.bind(&install);
    let bound_id = bound["id"].as_str().unwrap().to_string();

    // The widened bundle: `source` now declares `view_items`.
    fx.package(
        &MANIFEST_WIDE.replace("{{ACTION}}", "view_items"),
        "1.0.1",
        WORKFLOW_WIDE,
    );
    let check = fx.upgrade_check(&install, &installed);
    let upgraded = fx.op(
        "app_workspace_upgrade",
        fx.upgrade_params(&install, &installed, &check, "up-1140-wide"),
    );
    assert_eq!(upgraded["committed"], true, "{upgraded}");
    assert!(
        upgraded["compatibility"]["incompatible_bindings"]
            .as_array()
            .unwrap()
            .contains(&json!(bound_id)),
        "the widened slot must stay on the rebind path: {upgraded}"
    );
    assert_eq!(
        upgraded["compatibility"]["carried_bindings"],
        json!([]),
        "{upgraded}"
    );

    // The row is exactly what the operator bound — old digest pin,
    // revision 1, no drift evaluation under the new bundle.
    let kept = fx.slot_binding(&install, "source");
    assert_eq!(kept["id"], json!(bound_id), "{kept}");
    assert_eq!(kept["revision"], json!(1), "the row is untouched: {kept}");
    assert_eq!(kept["config"], bound["config"], "{kept}");
    assert_eq!(kept["config"]["bundle_digest"], json!(old_digest), "{kept}");
    assert_eq!(
        fx.journaled_carries(&install, "up-1140-wide"),
        Vec::<String>::new(),
        "nothing may stage for a changed declaration"
    );

    // The binding is simply absent under the new bundle — no quote, no
    // run — until the operator re-binds, which is the Confirm.
    let quote = fx.rpc(
        Asserted::Operator,
        "app_binding_quote",
        json!({"install_id": install, "slot": "source"}),
    );
    assert!(quote.is_err(), "quote must not ride a stale pin: {quote:?}");
    let run = fx.run(Asserted::Operator, &install, "run-1140-wide");
    assert!(run.is_err(), "run must not ride a stale pin: {run:?}");
}

/// Acceptance 3: an agent caller — and a caller with no provable
/// identity — cannot drive the upgrade at all, on the daemon or through
/// the board: `upgrade`, `upgrade_check` and `upgrade_recover` are the
/// operator's. Their refusals stage nothing: no journal, no pending
/// marker, no catalog move. The same daemon path then accepts the
/// operator's upgrade.
#[test]
fn an_agent_cannot_upgrade_on_the_daemon_or_the_board() {
    let mut fx = Fx::start();
    let installed = fx.install();
    let install = installed["install_id"].as_str().unwrap().to_string();
    let bound = fx.bind(&install);
    fx.package(MANIFEST, "1.0.1", WORKFLOW);

    // The real daemon path: every upgrade verb, asserted as the agent
    // and as no-one.
    let check_params = json!({"install_id": install,
        "source": fx.source(), "expected_digest": installed["digest"],
        "expected_generation": installed["catalog_generation"]});
    let upgrade_params = json!({"install_id": install,
        "source": fx.source(), "expected_digest": installed["digest"],
        "expected_generation": installed["catalog_generation"],
        "expected_new_digest": "sha256:0000", "request_id": "up-agent"});
    let recover_params = json!({"install_id": install, "request_id": "up-agent"});
    for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        for (method, params) in [
            ("app_workspace_upgrade_check", check_params.clone()),
            ("app_workspace_upgrade", upgrade_params.clone()),
            ("app_workspace_upgrade_recover", recover_params.clone()),
        ] {
            let refused = fx.rpc(who.clone(), method, params);
            assert!(refused.is_err(), "{who:?} reached {method}: {refused:?}");
        }
    }
    // Nothing was staged: no journal, no pending marker, digest unmoved.
    let journals = fx.pm().join(".apps/upgrade-journals").join(&install);
    assert!(
        !journals.exists() || std::fs::read_dir(&journals).unwrap().next().is_none(),
        "an agent's refused upgrade must not stage a journal"
    );
    assert!(!fx.pm().join(".apps/upgrade-pending.yaml").exists());
    assert_eq!(fx.digest(&install), installed["digest"]);
    assert_eq!(
        fx.slot_binding(&install, "source")["config"]["bundle_digest"],
        installed["digest"]
    );

    // The board peer. Control first — the operator's own session drives
    // the real route — because the agent-with-operator-session probes
    // below trip the stolen-session guard, which revokes the session.
    let (base, host, cookie, key) = fx.board();
    let check_url = format!("/api/app-installations/{install}/upgrade/check");
    let (status, check, _) = fx.http(
        &base,
        &host,
        "operator",
        Some((&cookie, &key)),
        "POST",
        &check_url,
        Some(json!({"source": fx.source(), "expected_digest": installed["digest"], "expected_generation": installed["catalog_generation"]})),
    );
    assert_eq!(status, 200, "{check}");

    // The same writes asserted as the agent and as no-one are refused —
    // and an operator session in an agent's hands is refused too: the
    // board's caller check runs before the daemon's.
    let upgrade_url = format!("/api/app-installations/{install}/upgrade");
    let recover_url = format!("/api/app-installations/{install}/upgrade/recover");
    // The board's upgrade wire is the closed `{files|source, expected_*}`
    // object — the install id rides the URL, never the body.
    let bodies = [
        (
            check_url.clone(),
            json!({"source": fx.source(), "expected_digest": installed["digest"], "expected_generation": installed["catalog_generation"]}),
        ),
        (
            upgrade_url.clone(),
            json!({"source": fx.source(), "expected_digest": installed["digest"], "expected_generation": installed["catalog_generation"], "expected_new_digest": "sha256:0000", "request_id": "up-agent"}),
        ),
        (recover_url.clone(), json!({"request_id": "up-agent"})),
    ];
    for who in ["agent:writer", "unproven"] {
        for (path, body) in &bodies {
            let (status, reply, _) =
                fx.http(&base, &host, who, None, "POST", path, Some(body.clone()));
            assert!(status >= 400, "{who} POST {path}: {status} {reply}");
        }
        for (path, body) in &bodies {
            let (status, reply, _) = fx.http(
                &base,
                &host,
                who,
                Some((&cookie, &key)),
                "POST",
                path,
                Some(body.clone()),
            );
            assert!(
                status >= 400,
                "{who}+operator-session POST {path}: {status} {reply}"
            );
        }
    }
    // No refusal moved the install: still the old digest, and the agent
    // probes staged nothing — no journal, no pending marker.
    assert_eq!(fx.digest(&install), installed["digest"]);
    assert!(fx.slot_binding(&install, "source")["config"]["bundle_digest"] == installed["digest"]);
    assert!(
        !fx.pm().join(".apps/upgrade-pending.yaml").exists(),
        "no board refusal may leave a staged upgrade"
    );
    let new_digest = check["digest"].as_str().unwrap().to_string();
    let upgraded = fx.op(
        "app_workspace_upgrade",
        fx.upgrade_params(&install, &installed, &check, "up-1140-op"),
    );
    assert_eq!(fx.digest(&install), new_digest, "{upgraded}");
    // The carried binding still works after the board's own gate ran.
    assert_eq!(
        fx.slot_binding(&install, "source")["config"]["bundle_digest"],
        json!(new_digest)
    );
    assert_eq!(bound["id"], fx.slot_binding(&install, "source")["id"]);
}
