//! CAD-1141 acceptance check, written by the independent reviewer
//! (cc13-rev-spec-1141) from the ticket and the operator decision of
//! 2026-10-26 ("Proceed within bound"), NOT by the implementer, who may not
//! edit or weaken it.
//!
//! The rule: price drift never refuses an app run; the run re-quotes and
//! proceeds, and the FRESH quote is the `max_charge_minor` ceiling the
//! provider receives. Still refused: a provider without the
//! `max_charge_minor@1` precondition, a quote beyond the supported bound,
//! and an agent or unproven caller.
//!
//! Real guards only, no production hook: one native `Shared` driven
//! through `Shared::dispatch` (`app_run_create` → `app_run_approve` →
//! `app_run_dispatch`, the declared host pipeline), the REAL
//! `AgenticosExternalAdapter` and a loopback door speaking the EXISTING
//! AgenticOS tool schema (`GET /v1/runtime/tools/read_instagram_posts`,
//! `POST /v1/runtime/tools/call`). The door's price and precondition move
//! AFTER the run froze its quote, so each case is a post-approval change.
//! File custody in the tempdir and a synthetic token; no real credential
//! or network beyond loopback. Every refusal asserts zero paid POSTs and
//! no retained receipt; case (a) is the positive control that the same
//! fixture does reach the provider.

use super::*;
use crate::platform::{agenticos_external, Custody, Key};
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const APP_MD: &str = r#"---
app: cad1141-fixture
title: CAD-1141 fixture
version: '1.0.0'
summary: One host-execution source read for the CAD-1141 drift acceptance.
needs:
  connections: []
  capabilities:
    source:
      schema: 1
      capability: social.read
      version: 1
      action: list_posts
      resource_kind: connection_account
      effect: read
---

# CAD-1141 fixture

One declared host-execution source read.
"#;

const WORKFLOW_MD: &str = r#"---
title: "Read public Instagram posts: {{profile_handle}}"
goal: "Retain one bounded provider receipt for the selected profile"
label: Find source
capability_slots: [source]
execution: host
inputs:
  profile_handle: { ask: "Public Instagram handle", example: "juicysuite_crm" }
---

## Acquire source: {{profile_handle}}
size: S
action: local.capability.call

The host executes this step itself: it calls the frozen `source` binding
with an empty input object and retains the actual provider response.

### Acceptance
- [ ] the provider action used only the frozen `source` binding
"#;

/// Approved price at create: 0.002000 USD = 2000 micros.
const APPROVED: u64 = 2_000;

struct Door {
    price_micros: AtomicU64,
    precondition: AtomicBool,
    calls: Mutex<Vec<Value>>,
}

fn amount(micros: u64) -> String {
    format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000)
}

fn door(state: Arc<Door>, stop: Arc<AtomicBool>) -> (String, std::thread::JoinHandle<()>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    let worker = std::thread::spawn(move || {
        while !stop.load(SeqCst) {
            let mut request = match server.recv_timeout(Duration::from_millis(100)) {
                Ok(Some(request)) => request,
                Ok(None) => continue,
                Err(_) => break,
            };
            let mut text = String::new();
            request.as_reader().read_to_string(&mut text).unwrap();
            let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let price = json!({"currency":"USD","scale":6,
                "amount": amount(state.price_micros.load(SeqCst))});
            let precondition = if state.precondition.load(SeqCst) {
                json!("max_charge_minor@1")
            } else {
                Value::Null
            };
            let reply = match (request.method().as_str(), request.url()) {
                ("GET", "/v1/runtime/tools/read_instagram_posts") => json!({"ok":true,"data":{
                    "slug":"read_instagram_posts","displayName":"Read Instagram posts",
                    "effect":"read","chargePrecondition":precondition,
                    "price":price,"unitPrice":null}}),
                ("POST", "/v1/runtime/tools/call") => {
                    state.calls.lock().unwrap().push(body);
                    json!({"ok":true,"data":{"slug":"read_instagram_posts","repeated":false,
                        "price":price,"result":{"success":true,"status":"ok",
                        "user":{"username":"juicysuite_crm","is_private":false},
                        "items":[{"id":"post-1","code":"AbCd123",
                            "created_at":"2026-09-27T00:00:00Z",
                            "caption":{"text":"Door caption"}}]}}})
                }
                _ => json!({"ok":false,"error":{"code":"not_found"}}),
            };
            let status = if reply["ok"] == true { 200 } else { 404 };
            let _ = request.respond(
                tiny_http::Response::from_string(reply.to_string())
                    .with_status_code(status)
                    .with_header(
                        tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                    ),
            );
        }
    });
    (base, worker)
}

struct Fx {
    _dir: tempfile::TempDir,
    shared: Arc<Shared>,
    install: String,
    door: Arc<Door>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("c1141").tempdir().unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let state = Arc::new(Door {
            price_micros: AtomicU64::new(APPROVED),
            precondition: AtomicBool::new(true),
            calls: Mutex::new(Vec::new()),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (base, worker) = door(state.clone(), stop.clone());
        let mut opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        opts.platforms.insert(
            agenticos_external::PLATFORM.into(),
            Arc::new(
                agenticos_external::AgenticosExternalAdapter::with_deployment_pin(
                    &base,
                    Some(agenticos_external::MANIFEST_PIN),
                )
                .unwrap(),
            ),
        );
        let mut shared = Shared::new(dir.path(), &opts).unwrap();
        Arc::get_mut(&mut shared).unwrap().platform_custody =
            Custody::file(dir.path().join("fixture-custody"));
        let token = b"synthetic-cad1141-token";
        const ACCOUNT: &str = "ws_11111111-1111-4111-8111-111111111111";
        let key = Key {
            platform: agenticos_external::PLATFORM,
            account: ACCOUNT,
        };
        let custody = shared.platform_custody.put(&key, token).unwrap();
        shared
            .store
            .platform_enroll(
                &crate::store::CredentialRecord {
                    connection_id: "src1141".into(),
                    credential_revision: 1,
                    platform: agenticos_external::PLATFORM.into(),
                    account: ACCOUNT.into(),
                    scopes: vec!["provider.read".into()],
                    fingerprint: crate::secret::fingerprint(token),
                    custody: custody.into(),
                    exchange: "token".into(),
                    enrolled_at: 1.0,
                    by: "operator".into(),
                },
                false,
                None,
            )
            .unwrap();
        let app = dir.path().join("app");
        std::fs::create_dir_all(app.join("workflows")).unwrap();
        std::fs::write(app.join("app.md"), APP_MD).unwrap();
        std::fs::write(app.join("workflows/read-source.md"), WORKFLOW_MD).unwrap();
        let op = |method: &str, params: Value| -> Result<Value> {
            scoped(Asserted::Operator, || {
                shared.dispatch(method, &params, std::process::id())
            })
        };
        let installed = op(
            "app_workspace_install",
            json!({"source": app.to_str().unwrap()}),
        )
        .expect("install");
        let install = installed["install_id"].as_str().unwrap().to_string();
        op(
            "app_local_install_approve",
            json!({"install_id": install, "digest": installed["digest"]}),
        )
        .expect("approve install");
        op(
            "app_binding_create",
            json!({"install_id": install, "slot": "source",
                "connection_id": "src1141", "request_id": "bind-1141"}),
        )
        .expect("the source slot must bind through the real declare path");
        Self {
            _dir: dir,
            shared,
            install,
            door: state,
            stop,
            worker: Some(worker),
        }
    }

    fn call(&self, who: Asserted, method: &str, params: Value) -> Result<Value> {
        scoped(who, || {
            self.shared.dispatch(method, &params, std::process::id())
        })
    }

    fn operator(&self, method: &str, params: Value) -> Result<Value> {
        self.call(Asserted::Operator, method, params)
    }

    /// A real operator session through the native link mint + open.
    fn session(&self) -> (String, String) {
        crate::operator_auth::ensure_secret(&self.shared.state_dir).unwrap();
        let secret = crate::operator_auth::read_secret(&self.shared.state_dir).unwrap();
        let nonce = self
            .operator(
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
            .unwrap()["nonce"]
            .clone();
        let opened = self
            .operator(
                "operator_session_open",
                json!({"nonce": nonce, "origin": "loopback"}),
            )
            .unwrap();
        (
            opened["token"].as_str().unwrap().to_string(),
            opened["key"].as_str().unwrap().to_string(),
        )
    }

    /// Create the run at the APPROVED price, freezing that quote.
    fn create(&self, request_id: &str) -> Value {
        let (token, key) = self.session();
        let run = self
            .operator(
                "app_run_create",
                json!({"install_id": self.install, "workflow": "read-source",
                    "inputs": {"profile_handle": "juicysuite_crm"},
                    "request_id": request_id,
                    "token": token, "key": key, "origin": "loopback"}),
            )
            .expect("run creation must freeze the live quote");
        assert_eq!(run["state"], "awaiting_approval", "{run}");
        assert_eq!(
            run["snapshot"]["quotes"]["source"]["total_price_micros"], APPROVED,
            "{run}"
        );
        run
    }

    fn approve_dispatch(&self, who: Asserted, run: &Value) -> Result<Value> {
        let id = run["id"].as_str().unwrap();
        self.call(
            who.clone(),
            "app_run_approve",
            json!({"run_id": id, "digest": run["snapshot_digest"]}),
        )?;
        self.call(who, "app_run_dispatch", json!({"run_id": id}))
    }

    fn paid_calls(&self) -> Vec<Value> {
        self.door.calls.lock().unwrap().clone()
    }

    fn receipts(&self, run: &Value) -> usize {
        self.operator("app_run_capability_results", json!({"run_id": run["id"]}))
            .expect("capability receipts")["results"]
            .as_array()
            .map_or(0, Vec::len)
    }

    fn assert_nothing_charged(&self, run: &Value, what: &str) {
        let calls = self.paid_calls();
        assert!(calls.is_empty(), "{what}: a paid call left: {calls:?}");
        assert_eq!(self.receipts(run), 0, "{what}: a receipt was retained");
    }
}

fn refused(result: Result<Value>, needle: &str, what: &str) {
    match result {
        Ok(value) => panic!("{what} was accepted: {value}"),
        Err(error) => assert!(
            error.to_string().contains(needle),
            "{what} refused by the wrong guard (want '{needle}'): {error}"
        ),
    }
}

/// (a) Same-slot price drift after approval: the run proceeds, the
/// provider receives the NEW quote as its `max_charge_minor` ceiling,
/// and the frozen, digest-covered snapshot still records the approved
/// quote.
#[test]
fn cad1141_drift_after_approval_proceeds_with_the_fresh_quote_as_the_ceiling() {
    let fx = Fx::new();
    let created = fx.create("run-1141-drift");
    let drifted = APPROVED * 3 + 7;
    fx.door.price_micros.store(drifted, SeqCst);
    let run = fx
        .approve_dispatch(Asserted::Operator, &created)
        .expect("a drifted price must not refuse the run");
    assert_eq!(run["state"], "succeeded", "{run}");
    let calls = fx.paid_calls();
    assert_eq!(calls.len(), 1, "exactly one paid call: {calls:?}");
    assert_eq!(
        calls[0]["max_charge_minor"], drifted,
        "the ceiling sent must be the fresh quote, not the approved one: {}",
        calls[0]
    );
    assert_eq!(calls[0]["query"]["handle"], "juicysuite_crm");
    assert_eq!(fx.receipts(&run), 1, "one retained receipt");
    let shown = fx
        .operator("app_run_show", json!({"run_id": run["id"]}))
        .unwrap();
    assert_eq!(
        shown["snapshot"]["quotes"]["source"]["total_price_micros"], APPROVED,
        "the frozen snapshot is never rewritten"
    );
    assert_eq!(shown["snapshot_digest"], created["snapshot_digest"]);
}

/// (b) A provider that drops the `max_charge_minor@1` precondition after
/// approval is refused before any paid call.
#[test]
fn cad1141_provider_without_the_charge_precondition_is_refused() {
    let fx = Fx::new();
    let created = fx.create("run-1141-precondition");
    fx.door.precondition.store(false, SeqCst);
    refused(
        fx.approve_dispatch(Asserted::Operator, &created),
        "bound capability price discovery refused",
        "a provider without max_charge_minor@1",
    );
    fx.assert_nothing_charged(&created, "missing precondition");
}

/// Bound: a fresh quote above the supported price bound (1e9 micros) is
/// refused before any paid call, even though drift itself is accepted.
#[test]
fn cad1141_fresh_quote_beyond_the_bound_is_refused() {
    let fx = Fx::new();
    let created = fx.create("run-1141-bound");
    fx.door.price_micros.store(1_000_000_001, SeqCst);
    refused(
        fx.approve_dispatch(Asserted::Operator, &created),
        "bound capability price discovery refused",
        "a quote beyond the supported bound",
    );
    fx.assert_nothing_charged(&created, "beyond the bound");
}

/// (c) Accepting drift opens no agent route: an agent or unproven caller
/// cannot approve or dispatch a drifted run, nothing is charged, and the
/// operator can then run it (control).
#[test]
fn cad1141_agent_and_unproven_callers_are_refused() {
    let fx = Fx::new();
    let created = fx.create("run-1141-agent");
    fx.door.price_micros.store(APPROVED + 1, SeqCst);
    let id = created["id"].as_str().unwrap();
    for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        refused(
            fx.call(
                who.clone(),
                "app_run_approve",
                json!({"run_id": id, "digest": created["snapshot_digest"]}),
            ),
            "operator",
            &format!("{who:?} app_run_approve"),
        );
        refused(
            fx.call(who.clone(), "app_run_dispatch", json!({"run_id": id})),
            "operator",
            &format!("{who:?} app_run_dispatch"),
        );
    }
    fx.assert_nothing_charged(&created, "non-operator callers");
    let run = fx
        .approve_dispatch(Asserted::Operator, &created)
        .expect("the operator control must run");
    assert_eq!(run["state"], "succeeded", "{run}");
    assert_eq!(fx.paid_calls().len(), 1);
}
