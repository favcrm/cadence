//! CAD-1119 result checks: installing an app is the operator's consent.
//! A real in-process daemon and board, a generic app package written at
//! runtime (no app-specific host code), and a provider whose reviewed
//! descriptor the test moves between calls, the way a provider release
//! moves it (D1.4: a tool rename and a revision bump). Each test names the
//! ticket outcome it proves.
#![cfg(feature = "test-seam")]

use cadence_agent::contract_fixture::{ToolTable, Verified};
use cadence_agent::platform::connections::{
    BoundActionMapping, CapabilityDescriptor, CapabilitySemantics, ProviderDescriptor,
};
use cadence_agent::platform::{
    AppCapabilityError, AppCapabilityOutput, AppCapabilityQuote, PlatformAdapter,
};
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon, store::Store};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PROVIDER: &str = "probe";
const OLD_TOOL: &str = "scrapecreators.instagram.user.posts";
const NEW_TOOL: &str = "read_instagram_posts";

/// What the provider's reviewed registration says right now.
#[derive(Clone)]
struct Shape {
    tool: &'static str,
    revision: &'static str,
    pin: &'static str,
    scopes: Vec<&'static str>,
    effect: &'static str,
    price_micros: u64,
}

impl Shape {
    fn original() -> Self {
        Self {
            tool: OLD_TOOL,
            revision: "probe-connections/2",
            pin: "probe-tools@2",
            scopes: vec!["provider.read"],
            effect: "read",
            price_micros: 2000,
        }
    }
    fn table(&self) -> ToolTable {
        ToolTable::from_json(&json!({"platform": PROVIDER, "manifest_version": self.pin,
            "tools": [{"tool": self.tool, "effect": self.effect, "scopes": self.scopes}]}))
        .unwrap()
    }
}

/// A provider with one reviewed read action whose descriptor can move.
/// Like the AgenticOS door after AOS-103, it prices only the tool name it
/// currently serves, so a receipt still naming the old tool cannot quote.
struct Moving {
    shape: Mutex<Shape>,
    table: Mutex<&'static ToolTable>,
    /// CAD-1171 reviewer fixture: records each actual provider invocation,
    /// including which frozen slot began execution.
    calls: Mutex<Vec<String>>,
    outcomes: Mutex<BTreeMap<String, AppCapabilityError>>,
}

impl Moving {
    fn new() -> Arc<Self> {
        let shape = Shape::original();
        let table = Box::leak(Box::new(shape.table()));
        Arc::new(Self {
            shape: Mutex::new(shape),
            table: Mutex::new(table),
            calls: Mutex::new(Vec::new()),
            outcomes: Mutex::new(BTreeMap::new()),
        })
    }
    fn shift(&self, change: impl FnOnce(&mut Shape)) {
        let mut shape = self.shape.lock().unwrap();
        change(&mut shape);
        *self.table.lock().unwrap() = Box::leak(Box::new(shape.table()));
    }
}

impl PlatformAdapter for Moving {
    fn table(&self) -> &ToolTable {
        *self.table.lock().unwrap()
    }
    fn connection_descriptor(&self) -> Option<ProviderDescriptor> {
        let s = self.shape.lock().unwrap().clone();
        let scopes: Vec<String> = s.scopes.iter().map(|v| v.to_string()).collect();
        Some(ProviderDescriptor {
            schema: 1,
            provider: PROVIDER.into(),
            revision: s.revision.into(),
            enrollment_shapes: vec!["token".into()],
            builtin_accounts: vec!["hosted".into()],
            capabilities: vec![CapabilityDescriptor {
                id: "probe.read".into(),
                version: 1,
                tools: vec![s.tool.into()],
                scopes: scopes.clone(),
                effect: s.effect.into(),
                semantics: CapabilitySemantics::MetadataRead,
            }],
            action_mappings: vec![BoundActionMapping {
                capability: "probe.read".into(),
                version: 1,
                action: "list_items".into(),
                resource_kind: "connection_account".into(),
                tool: s.tool.into(),
                scopes,
                effect: s.effect.into(),
                semantics: CapabilitySemantics::MetadataRead,
                input_contract: "probe.query@1".into(),
                output_contract: "probe.receipt@1".into(),
            }],
        })
    }
    fn connection_registration(&self) -> Option<String> {
        let s = self.shape.lock().unwrap();
        Some(format!("probe:{}:{}", s.revision, s.pin))
    }
    fn app_credentialless_account(&self, account: &str) -> bool {
        account == "hosted"
    }
    fn quote_app_capability(
        &self,
        _credential: &[u8],
        binding: &Value,
    ) -> Result<AppCapabilityQuote, String> {
        let micros = {
            let shape = self.shape.lock().unwrap();
            if binding["config"]["mapping"]["tool"] != shape.tool {
                return Err("not_allowlisted".into());
            }
            shape.price_micros
        };
        Ok(AppCapabilityQuote {
            schema: 1,
            currency: "USD".into(),
            unit_price_micros: micros,
            units: 1,
            total_price_micros: micros,
            price_revision: "probe-price/1".into(),
        })
    }
    /// CAD-1171 reviewer fixture: count the real daemon's call into the
    /// provider door, then return the configured confirmed refusal or
    /// transport uncertainty for that bound slot.
    fn execute_app_capability(
        &self,
        credential: &[u8],
        authority: &Value,
        input: &Value,
        idempotency_key: &str,
    ) -> Result<AppCapabilityOutput, String> {
        self.execute_app_capability_outcome(credential, authority, input, idempotency_key)
            .map_err(|error| error.to_string())
    }

    fn execute_app_capability_outcome(
        &self,
        _credential: &[u8],
        authority: &Value,
        _input: &Value,
        _idempotency_key: &str,
    ) -> Result<AppCapabilityOutput, AppCapabilityError> {
        let slot = authority["slot"].as_str().unwrap_or("unknown").to_string();
        self.calls.lock().unwrap().push(slot.clone());
        if let Some(error) = self.outcomes.lock().unwrap().get(&slot).cloned() {
            return Err(error);
        }
        Ok(AppCapabilityOutput {
            result: json!({"schema": 1, "posts": [], "profile": "probe"}),
            asset: None,
        })
    }
    fn reported_manifest_version(&self) -> Option<String> {
        Some(self.shape.lock().unwrap().pin.into())
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
    provider: Arc<Moving>,
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
        let root = tempfile::Builder::new().prefix("c1119").tempdir().unwrap();
        let provider = Moving::new();
        let mut fx = Self {
            root,
            provider,
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
        };
        cadence_agent::issue::Pm::init(&fx.pm()).unwrap();
        fx.package("1.0.0");
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
    /// The generic package, written at runtime as version `version`.
    fn package(&self, version: &str) {
        std::fs::create_dir_all(self.source().join("workflows")).unwrap();
        std::fs::write(
            self.source().join("app.md"),
            MANIFEST.replace("VERSION", version),
        )
        .unwrap();
        std::fs::write(self.source().join("workflows/read.md"), WORKFLOW).unwrap();
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
        self.bind_slot(install, "source", "bind-1119")
    }
    fn bind_slot(&self, install: &str, slot: &str, request_id: &str) -> Value {
        let params = json!({"install_id": install, "slot": slot,
            "connection_id": self.hosted(), "request_id": request_id});
        self.op("app_binding_create", params)["binding"].clone()
    }
    fn quote(&self, who: Asserted, install: &str) -> cadence_agent::Result<Value> {
        self.rpc(
            who,
            "app_binding_quote",
            json!({"install_id": install, "slot": "source"}),
        )
    }
    fn run(&self, who: Asserted, install: &str, request: &str) -> cadence_agent::Result<Value> {
        let params = json!({"install_id": install, "workflow": "read",
            "inputs": {"handle": "probe", "writer": "writer"},
            "request_id": request, "owner_pm": "lead"});
        self.rpc(who, "app_run_create", params)
    }
    fn binding(&self, install: &str) -> Value {
        let listed = self.op("app_binding_list", json!({"install_id": install}));
        listed["bindings"][0].clone()
    }
    fn approved(&self, install: &str) -> bool {
        let shown = self.op("app_workspace_show", json!({"install_id": install}));
        shown["approved"] == true
    }
    /// The daemon's audit stream, read from its store.
    fn audit(&self, kind: &str) -> Vec<Value> {
        let store = Store::open(&self.state().join("cadence.sqlite3")).unwrap();
        let events = store.events(Store::DAEMON_STREAM, 0, 10_000).unwrap();
        events
            .into_iter()
            .filter(|event| event.kind == kind)
            .map(|event| event.payload)
            .collect()
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
        let mint = json!({"secret": secret, "origin": "loopback"});
        let nonce = self.op("operator_link_mint", mint)["nonce"].clone();
        let session = self.http(
            &base,
            &host,
            "operator",
            None,
            "POST",
            "/api/session",
            Some(json!({"nonce": nonce})),
        );
        let (status, body, cookie) = session;
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
        let with = |request: ureq::RequestBuilder<ureq::typestate::WithBody>| {
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
            let request = agent
                .get(&url)
                .header("Host", host)
                .header("X-Cadence-Board", "1")
                .header("Origin", format!("http://{host}"))
                .header(AS_HEADER, who)
                .header(TOKEN_HEADER, &token);
            let request = match session {
                Some((cookie, key)) => request
                    .header("Cookie", cookie)
                    .header("X-Cadence-Session", key),
                None => request,
            };
            request.call().unwrap()
        } else {
            with(agent.post(&url))
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

/// R1: after the operator installs and binds, a quote succeeds over the
/// daemon RPC and the board, and the app runs: no approve step anywhere.
/// The install's consent is on the audit stream with the operator, the
/// exact digest and the capabilities the version declares.
#[test]
fn r1_install_and_bind_is_enough_to_quote_and_run() {
    let mut fx = Fx::start();
    let installed = fx.install();
    let install = installed["install_id"].as_str().unwrap().to_string();
    assert_eq!(installed["consent"]["recorded"], true, "{installed}");
    assert!(fx.approved(&install));
    let consent = fx.audit("app_install_capability_approved");
    assert_eq!(consent.len(), 1, "{consent:?}");
    assert_eq!(consent[0]["actor"], "operator");
    assert_eq!(consent[0]["via"], "install");
    assert_eq!(consent[0]["digest"], installed["digest"]);
    assert_eq!(
        consent[0]["capabilities"]["source"]["capability"],
        "probe.read"
    );
    fx.bind(&install);
    let quoted = fx.quote(Asserted::Operator, &install).unwrap();
    assert_eq!(quoted["quote"]["total_price_micros"], 2000, "{quoted}");
    let run = fx.run(Asserted::Operator, &install, "run-r1").unwrap();
    assert_eq!(run["state"], "awaiting_approval", "{run}");
    let (base, host, cookie, key) = fx.board();
    let path = format!("/api/app-installations/{install}/bindings/source/quote");
    let (status, body, _) = fx.http(
        &base,
        &host,
        "operator",
        Some((&cookie, &key)),
        "GET",
        &path,
        None,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["quote"]["total_price_micros"], 2000, "{body}");
}

/// R2: the D1.4 descriptor changes — the tool renamed to the generic slug
/// with the same capability, effect and scopes, and the @2 -> @3 revision
/// and pin bump — leave the app runnable with no operator action. The
/// binding migrates (new revision, same connection) and approval stays.
#[test]
fn r2_same_contract_descriptor_change_migrates_silently() {
    let fx = Fx::start();
    let install = fx.install()["install_id"].as_str().unwrap().to_string();
    let bound = fx.bind(&install);
    fx.provider.shift(|shape| {
        shape.tool = NEW_TOOL;
        shape.revision = "probe-connections/3";
        shape.pin = "probe-tools@3";
    });
    let listed = fx.binding(&install);
    assert_eq!(listed["drift"]["state"], "migrates", "{listed}");
    let quoted = fx.quote(Asserted::Operator, &install).unwrap();
    assert_eq!(quoted["quote"]["total_price_micros"], 2000, "{quoted}");
    let run = fx.run(Asserted::Operator, &install, "run-r2").unwrap();
    assert_eq!(run["state"], "awaiting_approval", "{run}");
    let migrated = fx.binding(&install);
    assert_eq!(migrated["id"], bound["id"]);
    assert_eq!(migrated["revision"], 2, "{migrated}");
    assert_eq!(migrated["config"]["mapping"]["tool"], NEW_TOOL);
    assert_eq!(
        migrated["config"]["descriptor_revision"],
        "probe-connections/3"
    );
    assert_eq!(
        migrated["config"]["connection_id"],
        bound["config"]["connection_id"]
    );
    assert_eq!(migrated["drift"]["state"], "current", "{migrated}");
    assert!(fx.approved(&install), "approval stays in force");
    assert!(fx.audit("app_install_capability_revoked").is_empty());
}

/// R3: a descriptor change that widens the slot's scopes blocks it — no
/// quote, no run — and the operator's view names the change. One operator
/// re-bind of the same connection confirms it and the app runs again.
/// A widened effect blocks too, and stays blocked: the app declared a
/// read, so even the operator cannot bind a draft action to it.
#[test]
fn r3_widened_scope_or_effect_blocks_until_the_operator_confirms() {
    let fx = Fx::start();
    let install = fx.install()["install_id"].as_str().unwrap().to_string();
    let bound = fx.bind(&install);
    fx.provider
        .shift(|shape| shape.scopes = vec!["provider.read", "provider.write"]);
    let listed = fx.binding(&install);
    assert_eq!(listed["drift"]["state"], "needs_confirm", "{listed}");
    let fields: Vec<&str> = listed["drift"]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["field"].as_str())
        .collect();
    assert!(fields.contains(&"mapping.scopes"), "{listed}");
    let quote = fx
        .quote(Asserted::Operator, &install)
        .unwrap_err()
        .to_string();
    assert!(quote.contains("mapping.scopes"), "{quote}");
    let run = fx.run(Asserted::Operator, &install, "run-r3a").unwrap_err();
    assert!(run.to_string().contains("confirm"), "{run}");
    assert_eq!(fx.binding(&install)["revision"], 1, "nothing migrated");
    // The operator's one inline confirm: re-bind the same connection.
    let confirm = json!({"install_id": install, "binding_id": bound["id"],
        "expected_revision": 1, "connection_id": bound["config"]["connection_id"]});
    let confirmed = fx.op("app_binding_update", confirm)["binding"].clone();
    assert_eq!(
        confirmed["config"]["mapping"]["scopes"],
        json!(["provider.read", "provider.write"])
    );
    fx.quote(Asserted::Operator, &install).unwrap();
    let run = fx.run(Asserted::Operator, &install, "run-r3b").unwrap();
    assert_eq!(run["state"], "awaiting_approval", "{run}");
    assert!(fx.approved(&install));

    fx.provider.shift(|shape| shape.effect = "draft");
    let listed = fx.binding(&install);
    assert_ne!(listed["drift"]["state"], "current", "{listed}");
    assert_ne!(listed["drift"]["state"], "migrates", "{listed}");
    assert!(fx.quote(Asserted::Operator, &install).is_err());
    assert!(fx.run(Asserted::Operator, &install, "run-r3c").is_err());
    let rebind = json!({"install_id": install, "binding_id": bound["id"],
        "expected_revision": 2, "connection_id": bound["config"]["connection_id"]});
    let refused = fx
        .rpc(Asserted::Operator, "app_binding_update", rebind)
        .unwrap_err();
    assert!(refused.to_string().contains("effect"), "{refused}");
}

/// R4: an agent and a detached child (no provable identity) cannot
/// install, update, bind, confirm or approve, over the RPC or the board,
/// so they never receive the consent an install records. A forged field
/// is refused even for the operator.
#[test]
fn r4_agents_and_detached_children_get_no_consent() {
    let mut fx = Fx::start();
    let source = fx.source();
    for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        let refused = fx.rpc(
            who.clone(),
            "app_workspace_install",
            json!({"source": source}),
        );
        assert!(refused.is_err(), "{who:?} installed: {refused:?}");
    }
    // Nothing was installed: the catalog does not even exist yet.
    match fx.rpc(Asserted::Operator, "app_workspace_list", json!({})) {
        Ok(listed) => assert_eq!(listed.as_array().map(Vec::len), Some(0), "{listed}"),
        Err(e) => assert!(e.to_string().contains("catalog source is missing"), "{e}"),
    }
    assert!(fx.audit("app_install_capability_approved").is_empty());
    // A forged consent field never rides an install, even the operator's.
    for forged in [
        json!({"source": source, "approved": true}),
        json!({"source": source, "actor": "operator"}),
    ] {
        let refused = fx.rpc(Asserted::Operator, "app_workspace_install", forged.clone());
        assert!(refused.is_err(), "{forged} accepted: {refused:?}");
    }
    let installed = fx.install();
    let install = installed["install_id"].as_str().unwrap().to_string();
    let bound = fx.bind(&install);
    // The operator withdraws consent; nothing an agent does restores it.
    let revoke = json!({"install_id": install, "digest": installed["digest"]});
    fx.op("app_local_install_revoke", revoke);
    assert!(!fx.approved(&install));
    fx.package("1.1.0");
    let check = fx.op(
        "app_workspace_upgrade_check",
        json!({"install_id": install,
        "source": source, "expected_digest": installed["digest"],
        "expected_generation": installed["catalog_generation"]}),
    );
    let upgrade = json!({"install_id": install, "source": source,
        "expected_digest": installed["digest"], "expected_generation": installed["catalog_generation"],
        "expected_new_digest": check["digest"], "request_id": "up-1119"});
    for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        let calls = [
            ("app_workspace_upgrade", upgrade.clone()),
            (
                "app_local_install_approve",
                json!({"install_id": install, "digest": installed["digest"]}),
            ),
            (
                "app_binding_create",
                json!({"install_id": install, "slot": "source",
                "connection_id": bound["config"]["connection_id"], "request_id": "agent-bind"}),
            ),
            (
                "app_binding_update",
                json!({"install_id": install, "binding_id": bound["id"],
                "expected_revision": 1, "connection_id": bound["config"]["connection_id"]}),
            ),
        ];
        for (method, params) in calls {
            let refused = fx.rpc(who.clone(), method, params);
            assert!(refused.is_err(), "{who:?} {method}: {refused:?}");
        }
        assert!(fx.run(who.clone(), &install, "agent-run").is_err());
    }
    assert!(!fx.approved(&install), "no implicit approval");
    assert_eq!(fx.binding(&install)["revision"], 1);
    assert_eq!(fx.audit("app_install_capability_approved").len(), 1);
    // The board: the same writes asserted as the agent are refused, and a
    // forged body field is refused for the operator's session too.
    let (base, host, cookie, key) = fx.board();
    let bindings = format!("/api/app-installations/{install}/bindings");
    let update = format!(
        "/api/app-installations/{install}/bindings/{}/update",
        bound["id"].as_str().unwrap()
    );
    let writes = [
        (
            "/api/app-installations".to_string(),
            json!({"source": source}),
        ),
        (
            bindings.clone(),
            json!({"slot": "source", "connection_id": bound["config"]["connection_id"], "request_id": "agent-http"}),
        ),
        (
            update.clone(),
            json!({"expected_revision": 1, "connection_id": bound["config"]["connection_id"]}),
        ),
    ];
    for (path, body) in &writes {
        let (status, reply, _) = fx.http(
            &base,
            &host,
            "agent:writer",
            None,
            "POST",
            path,
            Some(body.clone()),
        );
        assert!(status >= 400, "agent POST {path}: {status} {reply}");
    }
    let forged = json!({"expected_revision": 1, "connection_id": bound["config"]["connection_id"], "approved": true});
    let (status, reply, _) = fx.http(
        &base,
        &host,
        "operator",
        Some((&cookie, &key)),
        "POST",
        &update,
        Some(forged),
    );
    assert_eq!(status, 400, "{reply}");
    assert!(!fx.approved(&install));
    assert_eq!(fx.binding(&install)["revision"], 1);
    // The operator's own update is consent again.
    let upgraded = fx.op("app_workspace_upgrade", upgrade);
    assert_eq!(upgraded["consent"]["recorded"], true, "{upgraded}");
    let consent = fx.audit("app_install_capability_approved");
    assert_eq!(consent.last().unwrap()["via"], "upgrade");
    assert_eq!(consent.last().unwrap()["digest"], check["digest"]);
}

/// R5: install consent is not spend or publish consent. The installed,
/// bound app's run waits for the per-run frozen-price approval: it cannot
/// dispatch before it, and only the operator can give it. Install grants
/// no publication authority: there is no effect to release.
#[test]
fn r5_spend_and_publish_approvals_are_still_required() {
    let fx = Fx::start();
    let install = fx.install()["install_id"].as_str().unwrap().to_string();
    fx.bind(&install);
    let run = fx.run(Asserted::Operator, &install, "run-r5").unwrap();
    let id = run["id"].as_str().unwrap();
    assert_eq!(run["state"], "awaiting_approval", "{run}");
    let dispatched = fx.rpc(
        Asserted::Operator,
        "app_run_dispatch",
        json!({"run_id": id}),
    );
    let error = dispatched.unwrap_err().to_string();
    assert!(error.contains("approval"), "{error}");
    let decide = json!({"run_id": id, "digest": run["snapshot_digest"]});
    for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        assert!(fx.rpc(who, "app_run_approve", decide.clone()).is_err());
    }
    let shown = fx.op("app_run_show", json!({"run_id": id}));
    assert_eq!(shown["state"], "awaiting_approval", "{shown}");
    let approved = fx.op("app_run_approve", decide);
    assert_eq!(approved["state"], "approved", "{approved}");
    let effects = fx.op("app_effect_list", json!({"install_id": install}));
    assert_eq!(effects["effects"], json!([]), "{effects}");
}

/// R3 (credential): rotating the enrolled credential under a bound slot
/// moves `connection_revision`, which is the connection itself, not
/// provider bookkeeping. The slot needs the operator: the listing names the
/// change, quote and run refuse, and nothing migrates until the operator
/// confirms by re-binding; then the app runs again.
#[test]
fn r3_rotated_credential_needs_the_operator_before_runs_resume() {
    let fx = Fx::start();
    let install = fx.install()["install_id"].as_str().unwrap().to_string();
    // Secret-shaped material is built at runtime and only rides the RPC.
    let token = || format!("probe-{}", uuid::Uuid::new_v4().simple());
    let created = fx.op(
        "connection_create",
        json!({"provider": PROVIDER, "account": "acct",
        "shape": "token", "token": token(), "scopes": ["provider.read"],
        "accept_same_uid_risk": true}),
    );
    let connection = created["connection"]["id"].as_str().unwrap().to_string();
    let bind = json!({"install_id": install, "slot": "source",
        "connection_id": connection, "request_id": "bind-rotate"});
    let bound = fx.op("app_binding_create", bind)["binding"].clone();
    fx.quote(Asserted::Operator, &install).unwrap();
    fx.op(
        "connection_rotate",
        json!({"connection_id": connection, "token": token(),
        "scopes": ["provider.read"], "accept_same_uid_risk": true}),
    );
    let listed = fx.binding(&install);
    assert_eq!(listed["drift"]["state"], "needs_confirm", "{listed}");
    let fields: Vec<&str> = listed["drift"]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["field"].as_str())
        .collect();
    assert!(fields.contains(&"connection_revision"), "{listed}");
    let quote = fx
        .quote(Asserted::Operator, &install)
        .unwrap_err()
        .to_string();
    assert!(quote.contains("connection_revision"), "{quote}");
    assert!(fx.run(Asserted::Operator, &install, "run-rot-a").is_err());
    assert_eq!(fx.binding(&install)["revision"], 1, "nothing migrated");
    let confirm = json!({"install_id": install, "binding_id": bound["id"],
        "expected_revision": 1, "connection_id": connection});
    let confirmed = fx.op("app_binding_update", confirm)["binding"].clone();
    assert_eq!(confirmed["config"]["connection_revision"], 2, "{confirmed}");
    fx.quote(Asserted::Operator, &install).unwrap();
    let run = fx.run(Asserted::Operator, &install, "run-rot-b").unwrap();
    assert_eq!(run["state"], "awaiting_approval", "{run}");
}

/// CAD-1171 reviewer-authored acceptance (the implementer did not write
/// this): the host path through the real daemon. A host run is created
/// with no owner PM and no worker input, approved, and dispatched; the
/// daemon executes the bound capability in-process, retains the receipt,
/// and completes the run — with exactly one provider call and zero agents
/// anywhere (no owner, no assignments, no worker notify).
///
/// Harness note: the suggested `cad1123_hp4_acceptance.rs` board harness
/// is social-publish-only (a counting `PublishSender` door; no
/// install/bindings/app-run path), so this uses the `cad1119` Fx, which
/// is the harness that actually sets up an install, bindings, a fake
/// app-capability adapter, and an operator connection.
const HOST_WORKFLOW: &str = r#"---
title: "Read listing: {{handle}}"
goal: "Retain one bounded provider receipt"
label: Read listing host
capability_slots: [source]
execution: host
inputs:
  handle: { ask: "Listing handle", example: "probe" }
---
Read the selected listing through the bound `source` capability.

## Read listing: {{handle}}
size: S
action: local.capability.call

Call `source` once and report the receipt identity.

### Acceptance
- [ ] the receipt identity is reported
"#;

const TWO_SLOT_MANIFEST: &str = r#"---
app: probe-reader
title: Probe Reader
version: '1.0.0'
summary: Read two bounded listings through bound source capabilities.
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
    secondary:
      schema: 1
      capability: probe.read
      version: 1
      action: list_items
      resource_kind: connection_account
      effect: read
---

# Probe Reader

Reads two bounded listings through the bound `source` and `secondary` capabilities.
"#;

const TWO_SLOT_HOST_WORKFLOW: &str = r#"---
title: "Read listing: {{handle}}"
goal: "Retain two bounded provider receipts"
label: Read two listings host
capability_slots: [source, secondary]
execution: host
inputs:
  handle: { ask: "Listing handle", example: "probe" }
---
Read the selected listings through their bound capabilities.

## Read listings: {{handle}}
size: S
action: local.capability.call

Call both declared slots once and retain each receipt.

### Acceptance
- [ ] both receipt identities are retained
"#;

impl Fx {
    /// CAD-1171: add the host-execution workflow to the package before
    /// install, then install and bind the `source` slot exactly like r5.
    fn host_install(&self) -> String {
        std::fs::write(self.source().join("workflows/hostread.md"), HOST_WORKFLOW).unwrap();
        let install = self.install()["install_id"].as_str().unwrap().to_string();
        self.bind(&install);
        install
    }
    fn host_install_two_slots(&self) -> String {
        std::fs::write(self.source().join("app.md"), TWO_SLOT_MANIFEST).unwrap();
        std::fs::write(
            self.source().join("workflows/hostread.md"),
            TWO_SLOT_HOST_WORKFLOW,
        )
        .unwrap();
        let install = self.install()["install_id"].as_str().unwrap().to_string();
        self.bind_slot(&install, "source", "bind-host-source");
        self.bind_slot(&install, "secondary", "bind-host-secondary");
        install
    }
    /// CAD-1171: create a host run — no owner PM, no worker input.
    fn host_run(&self, install: &str, request: &str) -> cadence_agent::Result<Value> {
        let params = json!({"install_id": install, "workflow": "hostread",
            "inputs": {"handle": "probe"}, "request_id": request});
        self.rpc(Asserted::Operator, "app_run_create", params)
    }
    fn host_calls(&self) -> usize {
        self.provider.calls.lock().unwrap().len()
    }
    fn host_call_slots(&self) -> Vec<String> {
        self.provider.calls.lock().unwrap().clone()
    }
    fn set_host_outcome(&self, slot: &str, error: AppCapabilityError) {
        self.provider
            .outcomes
            .lock()
            .unwrap()
            .insert(slot.to_string(), error);
    }
    fn host_receipts(&self, run_id: &str) -> Value {
        self.op("app_run_capability_results", json!({"run_id": run_id}))
    }
    fn wait_for_host_advance_ticks(&self, run_id: &str) -> Value {
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        let mut run = self.op("app_run_show", json!({"run_id": run_id}));
        while std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            run = self.op("app_run_show", json!({"run_id": run_id}));
            if run["state"] == "failed" {
                // Ensure two one-second run-monitor intervals elapse after
                // failure, not just a direct re-dispatch.
                std::thread::sleep(Duration::from_millis(2300));
                return self.op("app_run_show", json!({"run_id": run_id}));
            }
        }
        run
    }
    fn host_approve_dispatch(&self, id: &str, digest: Value) -> Value {
        let approved = self.op("app_run_approve", json!({"run_id": id, "digest": digest}));
        assert_eq!(approved["state"], "approved", "{approved}");
        self.op("app_run_dispatch", json!({"run_id": id}))
    }
}

#[test]
fn cad1171_host_run_executes_in_process_with_no_agents() {
    let fx = Fx::start();
    let install = fx.host_install();
    let run = fx.host_run(&install, "host-e2e-1").unwrap();
    let id = run["id"].as_str().unwrap().to_string();
    let digest = run["snapshot_digest"].clone();
    assert_eq!(run["state"], "awaiting_approval", "{run}");
    assert_eq!(run["snapshot"]["owner_pm"], Value::Null, "{run}");
    assert_eq!(run["snapshot"]["assignments"], json!({}), "{run}");
    let done = fx.host_approve_dispatch(&id, digest);
    assert_eq!(done["state"], "succeeded", "{done}");
    assert_eq!(fx.host_calls(), 1, "one tap is one provider call");
    let completed = fx.audit("app_run_completed");
    let event = completed.last().expect("completion event");
    assert_eq!(event["run_id"], id.as_str());
    assert_eq!(event["host"], true);
    let shown = fx.op("app_run_show", json!({"run_id": id}));
    assert_eq!(shown["state"], "succeeded", "{shown}");
    assert_eq!(shown["snapshot"]["assignments"], json!({}), "{shown}");
}

#[test]
fn cad1171_host_run_refuses_changed_quote_before_any_provider_call() {
    let fx = Fx::start();
    let install = fx.host_install();
    let run = fx.host_run(&install, "host-quote-1").unwrap();
    let id = run["id"].as_str().unwrap().to_string();
    let digest = run["snapshot_digest"].clone();
    let approved = fx.op("app_run_approve", json!({"run_id": id, "digest": digest}));
    assert_eq!(approved["state"], "approved", "{approved}");
    // The provider reprices between approval and dispatch. The firing
    // gate is the dispatch entry-guard re-quote ("since run creation");
    // the execute-time comparison one layer down guards the same values
    // against read skew — defense-in-depth over an identical check.
    fx.provider.shift(|s| s.price_micros = 3000);
    let error = fx
        .rpc(
            Asserted::Operator,
            "app_run_dispatch",
            json!({"run_id": id}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("capability price changed since run creation"),
        "{error}"
    );
    assert_eq!(fx.host_calls(), 0, "no provider call before the refusal");
    let shown = fx.op("app_run_show", json!({"run_id": id}));
    assert_ne!(shown["state"], "succeeded", "{shown}");
}

#[test]
fn cad1171_host_run_refuses_send_effect_before_any_provider_call() {
    let fx = Fx::start();
    // A send-effect slot is not installable at all (install refuses the
    // capability contract up front), so the escalation below is the honest
    // way to put a send effect in front of the execution gate: the
    // reviewed descriptor moves read -> send after approval, and dispatch
    // must refuse before any provider call. The firing gate is the
    // execute-time config re-derivation ("effect differs from app
    // contract"); behind it stand the drift guard (`effect` is slot
    // contract, only `tool` may migrate) and the read/draft
    // classification — four layers, all refusing send effects, with the
    // install-time contract check having already refused a send slot at
    // bind. This test proves the execute-time layer.
    let install = fx.host_install();
    let run = fx.host_run(&install, "host-effect-1").unwrap();
    let id = run["id"].as_str().unwrap().to_string();
    let digest = run["snapshot_digest"].clone();
    let approved = fx.op("app_run_approve", json!({"run_id": id, "digest": digest}));
    assert_eq!(approved["state"], "approved", "{approved}");
    fx.provider.shift(|s| s.effect = "send");
    let error = fx
        .rpc(
            Asserted::Operator,
            "app_run_dispatch",
            json!({"run_id": id}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("effect differs from app contract"),
        "{error}"
    );
    assert_eq!(fx.host_calls(), 0, "no provider call before the refusal");
    let shown = fx.op("app_run_show", json!({"run_id": id}));
    assert_ne!(shown["state"], "succeeded", "{shown}");
}

/// CAD-1171 independent bad-case acceptance: the genuine host provider call
/// returns a typed refusal after entry. A non-operator cannot dispatch it;
/// the operator-visible failure retains the safe cause; the real daemon's
/// one-second run-monitor/advance thread and an explicit re-entry both leave
/// the counting provider at one call.
#[test]
fn cad1171_host_provider_refusal_is_durable_and_not_retried() {
    let mut fx = Fx::start();
    let install = fx.host_install();
    let run = fx.host_run(&install, "host-refusal-1").unwrap();
    let id = run["id"].as_str().unwrap().to_string();
    let denied = fx
        .rpc(
            Asserted::Operator,
            "app_run_dispatch",
            json!({"run_id": id}),
        )
        .unwrap_err()
        .to_string();
    assert!(denied.contains("approval"), "{denied}");
    assert_eq!(fx.host_calls(), 0, "approval precedes provider I/O");
    let digest = run["snapshot_digest"].clone();
    let approved = fx.op("app_run_approve", json!({"run_id": id, "digest": digest}));
    assert_eq!(approved["state"], "approved", "{approved}");
    fx.set_host_outcome(
        "source",
        AppCapabilityError::Refused("probe refused after provider entry".into()),
    );

    for caller in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        assert!(fx
            .rpc(caller, "app_run_dispatch", json!({"run_id": id}),)
            .is_err());
        assert_eq!(
            fx.host_calls(),
            0,
            "only the operator may reach the provider"
        );
    }

    // The board's HTTP dispatch route must enforce the same operator proof
    // as the daemon RPC it relays.
    let (base, host, cookie, key) = fx.board();
    let path = format!("/api/app-runs/{id}/dispatch");
    let (status, reply, _) = fx.http(
        &base,
        &host,
        "agent:writer",
        None,
        "POST",
        &path,
        Some(json!({})),
    );
    assert!(status >= 400, "agent HTTP dispatch: {status} {reply}");
    assert_eq!(
        fx.host_calls(),
        0,
        "agent HTTP dispatch made no provider call"
    );
    let (status, reply, _) = fx.http(
        &base,
        &host,
        "operator",
        None,
        "POST",
        &path,
        Some(json!({})),
    );
    assert!(status >= 400, "operator without session: {status} {reply}");
    assert_eq!(fx.host_calls(), 0, "missing session made no provider call");
    let (status, reply, _) = fx.http(
        &base,
        &host,
        "operator",
        Some((&cookie, &key)),
        "POST",
        &path,
        Some(json!({})),
    );
    assert!(status >= 400, "provider refusal: {status} {reply}");

    let failed = fx.wait_for_host_advance_ticks(&id);
    assert_eq!(failed["state"], "failed", "{failed}");
    assert_eq!(failed["failure"]["kind"], "refused", "{failed}");
    assert_eq!(
        failed["failure"]["reason"], "probe refused after provider entry",
        "{failed}"
    );
    assert_eq!(fx.host_receipts(&id)["results"], json!([]));
    assert_eq!(fx.host_call_slots(), vec!["source".to_string()]);

    let (status, reply, _) = fx.http(
        &base,
        &host,
        "operator",
        Some((&cookie, &key)),
        "POST",
        &path,
        Some(json!({})),
    );
    assert!(status >= 400, "terminal re-entry: {status} {reply}");
    assert_eq!(fx.host_call_slots(), vec!["source".to_string()]);
}

/// The adapter's `Uncertain` classification must survive the daemon and
/// remain distinct from a confirmed refusal. Neither the background advance
/// tick nor explicit re-entry may turn uncertainty into a new provider call.
#[test]
fn cad1171_host_uncertain_outcome_is_not_retried_or_misreported() {
    let fx = Fx::start();
    let install = fx.host_install();
    let run = fx.host_run(&install, "host-uncertain-1").unwrap();
    let id = run["id"].as_str().unwrap().to_string();
    let digest = run["snapshot_digest"].clone();
    let approved = fx.op("app_run_approve", json!({"run_id": id, "digest": digest}));
    assert_eq!(approved["state"], "approved", "{approved}");
    fx.set_host_outcome(
        "source",
        AppCapabilityError::Uncertain("probe transport outcome is uncertain".into()),
    );

    assert!(fx
        .rpc(
            Asserted::Operator,
            "app_run_dispatch",
            json!({"run_id": id}),
        )
        .is_err());
    let failed = fx.wait_for_host_advance_ticks(&id);
    assert_eq!(failed["state"], "failed", "{failed}");
    assert_eq!(failed["failure"]["kind"], "uncertain", "{failed}");
    assert_eq!(
        failed["failure"]["reason"], "probe transport outcome is uncertain",
        "{failed}"
    );
    assert_ne!(failed["failure"]["kind"], "refused", "{failed}");
    assert_ne!(failed["state"], "succeeded", "{failed}");
    assert_eq!(fx.host_call_slots(), vec!["source".to_string()]);

    assert!(fx
        .rpc(
            Asserted::Operator,
            "app_run_dispatch",
            json!({"run_id": id}),
        )
        .is_err());
    assert_eq!(fx.host_call_slots(), vec!["source".to_string()]);
}

#[test]
fn cad1171_host_two_bound_slots_complete_once_with_both_receipts() {
    let fx = Fx::start();
    let install = fx.host_install_two_slots();
    let run = fx.host_run(&install, "host-two-success-1").unwrap();
    let id = run["id"].as_str().unwrap().to_string();
    assert_eq!(run["state"], "awaiting_approval", "{run}");
    assert!(
        run["snapshot"]["capabilities"]["source"]["digest"].is_string(),
        "{run}"
    );
    assert!(
        run["snapshot"]["capabilities"]["secondary"]["digest"].is_string(),
        "{run}"
    );
    assert!(
        run["snapshot"]["quotes"]["source"]["total_price_micros"].is_number(),
        "{run}"
    );
    assert!(
        run["snapshot"]["quotes"]["secondary"]["total_price_micros"].is_number(),
        "{run}"
    );
    assert_eq!(run["snapshot"]["owner_pm"], Value::Null, "{run}");
    assert_eq!(run["snapshot"]["assignments"], json!({}), "{run}");

    let done = fx.host_approve_dispatch(&id, run["snapshot_digest"].clone());
    assert_eq!(done["state"], "succeeded", "{done}");
    assert_eq!(
        fx.host_call_slots(),
        vec!["source".to_string(), "secondary".to_string()]
    );
    let receipts = fx.host_receipts(&id)["results"].as_array().unwrap().clone();
    assert_eq!(receipts.len(), 2, "{receipts:?}");
    let mut receipt_slots: Vec<_> = receipts
        .iter()
        .map(|receipt| receipt["slot"].as_str().unwrap().to_string())
        .collect();
    receipt_slots.sort();
    assert_eq!(
        receipt_slots,
        vec!["secondary".to_string(), "source".to_string()]
    );
    let completions: Vec<_> = fx
        .audit("app_run_completed")
        .into_iter()
        .filter(|event| event["run_id"] == id)
        .collect();
    assert_eq!(completions.len(), 1, "{completions:?}");
}

fn assert_host_two_slot_second_failure(
    request: &str,
    outcome: AppCapabilityError,
    expected_kind: &str,
    expected_reason: &str,
) {
    let fx = Fx::start();
    let install = fx.host_install_two_slots();
    let run = fx.host_run(&install, request).unwrap();
    let id = run["id"].as_str().unwrap().to_string();
    assert!(
        run["snapshot"]["capabilities"]["source"]["digest"].is_string(),
        "{run}"
    );
    assert!(
        run["snapshot"]["capabilities"]["secondary"]["digest"].is_string(),
        "{run}"
    );
    assert!(
        run["snapshot"]["quotes"]["source"]["total_price_micros"].is_number(),
        "{run}"
    );
    assert!(
        run["snapshot"]["quotes"]["secondary"]["total_price_micros"].is_number(),
        "{run}"
    );
    let digest = run["snapshot_digest"].clone();
    let approved = fx.op("app_run_approve", json!({"run_id": id, "digest": digest}));
    assert_eq!(approved["state"], "approved", "{approved}");
    fx.set_host_outcome("secondary", outcome);

    assert!(fx
        .rpc(
            Asserted::Operator,
            "app_run_dispatch",
            json!({"run_id": id}),
        )
        .is_err());
    let failed = fx.wait_for_host_advance_ticks(&id);
    assert_eq!(failed["state"], "failed", "{failed}");
    assert_eq!(failed["failure"]["kind"], expected_kind, "{failed}");
    assert_eq!(failed["failure"]["reason"], expected_reason, "{failed}");
    assert_ne!(failed["state"], "succeeded", "{failed}");
    assert_eq!(
        fx.host_call_slots(),
        vec!["source".to_string(), "secondary".to_string()]
    );

    let receipts_before = fx.host_receipts(&id)["results"].as_array().unwrap().clone();
    assert_eq!(receipts_before.len(), 1, "{receipts_before:?}");
    assert_eq!(receipts_before[0]["slot"], "source", "{receipts_before:?}");
    assert_eq!(
        receipts_before[0]["result"]["profile"], "probe",
        "{receipts_before:?}"
    );
    let first_receipt_id = receipts_before[0]["id"].clone();
    assert!(fx
        .rpc(
            Asserted::Operator,
            "app_run_dispatch",
            json!({"run_id": id}),
        )
        .is_err());
    let receipts_after = fx.host_receipts(&id)["results"].as_array().unwrap().clone();
    assert_eq!(receipts_after, receipts_before);
    assert_eq!(receipts_after[0]["id"], first_receipt_id);
    assert_eq!(
        fx.host_call_slots(),
        vec!["source".to_string(), "secondary".to_string()]
    );
}

#[test]
fn cad1171_host_two_slot_second_refusal_keeps_first_receipt_without_replay() {
    assert_host_two_slot_second_failure(
        "host-two-refusal-1",
        AppCapabilityError::Refused("secondary refused after provider entry".into()),
        "refused",
        "secondary refused after provider entry",
    );
}

#[test]
fn cad1171_host_two_slot_second_uncertainty_keeps_first_receipt_without_replay() {
    assert_host_two_slot_second_failure(
        "host-two-uncertain-1",
        AppCapabilityError::Uncertain("secondary transport outcome is uncertain".into()),
        "uncertain",
        "secondary transport outcome is uncertain",
    );
}
