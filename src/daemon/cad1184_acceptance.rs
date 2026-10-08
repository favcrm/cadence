//! Independent CAD-1184 acceptance checks. These exercise the real
//! `Shared::dispatch`/`scoped_chat_assistant` guard and the real daemon +
//! board relay under the test caller-identity seam; no fake assistant API
//! or fake permission store is used. The assertions are derived from the
//! ticket and `/tmp/crm-assistant-pm/rpc-contract.md`, not implementation
//! behavior. Backend must not edit or weaken this file.
#![cfg(feature = "test-seam")]

use super::*;
use crate::store::app_contexts::ContextConfig;
use crate::store::app_records::{ConsentState, CustomerConsent, CustomerProfile, RecordStore};
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

const CRM_SOURCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/workspace-apps/crm");
const MASTER_GENERATION: &str = "0123456789abcdef0123456789abcdef";

struct Fx {
    dir: tempfile::TempDir,
    shared: Arc<Shared>,
    install: String,
    context: String,
}

struct ServerGuard {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        for thread in self.threads.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("c1184acc")
            .tempdir()
            .unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd = dir.path().to_str().unwrap();
        for (alias, role) in [("master", "worker"), ("w1", "worker")] {
            shared
                .store
                .register_agent(&NewAgent {
                    alias,
                    provider: "pi",
                    endpoint_kind: "managed",
                    role,
                    cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: None,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        shared
            .store
            .set_identity_with_quota(
                "master",
                &crate::adapter::Identity {
                    thread_id: "cad1184-thread".into(),
                    session_id: "cad1184-session".into(),
                    model: None,
                    effort: None,
                    pid: std::process::id(),
                    endpoint: None,
                    generation: Some(MASTER_GENERATION.into()),
                    attach: None,
                },
                None,
            )
            .unwrap();
        let installed = scoped(Asserted::Operator, || {
            shared.dispatch(
                "app_workspace_install",
                &json!({"source": CRM_SOURCE}),
                std::process::id(),
            )
        })
        .unwrap();
        let install = installed["install_id"].as_str().unwrap().to_owned();
        let config = ContextConfig::new("Acceptance", BTreeMap::new()).unwrap();
        let context = shared
            .store
            .app_context_create(&install, &config, "cad1184-context")
            .unwrap()["context"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let records = RecordStore::open(dir.path(), &install).unwrap();
        for id in ["cust-a", "cust-b"] {
            records
                .app_record_create(
                    &context,
                    id,
                    &CustomerProfile {
                        schema: 1,
                        display_name: format!("Customer {id}"),
                        email: Some(format!("{id}@example.test")),
                        phone: None,
                        tags: vec!["initial".into()],
                        source: Some("acceptance".into()),
                        consent: CustomerConsent {
                            email: ConsentState::Unknown,
                            sms: None,
                        },
                    },
                )
                .unwrap();
        }
        Self {
            dir,
            shared,
            install,
            context,
        }
    }

    fn call(&self, who: Asserted, method: &str, params: Value) -> Result<Value> {
        scoped(who, || {
            self.shared.dispatch(method, &params, std::process::id())
        })
    }
    fn operator(&self, method: &str, params: Value) -> Value {
        self.call(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }
    fn customer(&self, id: &str) -> Value {
        RecordStore::open(self.dir.path(), &self.install)
            .unwrap()
            .app_record_show(&self.context, id)
            .unwrap()["record"]
            .clone()
    }
    fn create_turn(&self, message: &str) -> String {
        let params = json!({"alias":"master", "install_id":self.install,
            "context_id":self.context, "general":true});
        scoped(Asserted::Operator, || {
            self.shared
                .dispatch("conversation_create", &params, std::process::id())
        })
        .unwrap();
        let sent = json!({"alias":"master", "text":"Perform the requested CRM action",
            "message":message, "app":{"install_id":self.install,"context_id":self.context}});
        scoped(Asserted::Operator, || {
            self.shared
                .dispatch("thread_send", &sent, std::process::id())
        })
        .unwrap();
        let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(MASTER_GENERATION);
        self.db()
            .execute(
                "UPDATE messages SET state='submitting', started=1.0 WHERE id=?",
                [message],
            )
            .unwrap();
        self.shared.store.mark_running(message, &token).unwrap();
        token
    }
    fn invoke(
        &self,
        message: &str,
        token: &str,
        action: &str,
        op: &str,
        input: Value,
    ) -> Result<Value> {
        self.call(
            Asserted::Agent("master".into()),
            "app_assistant_invoke",
            json!({
                "install_id":self.install,"context_id":self.context,"message":message,
                "token":token,"action_id":action,"operation_id":op,"input":input,
            }),
        )
    }
    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(crate::rollout::db_file(self.dir.path())).unwrap()
    }
    fn finish(&self, message: &str) {
        self.db()
            .execute(
                "UPDATE messages SET state='completed', started=2.0 WHERE id=?",
                [message],
            )
            .unwrap();
    }
}

fn refusal<T: std::fmt::Debug>(result: Result<T>, case: &str) {
    assert!(result.is_err(), "{case} was accepted: {result:?}");
}

fn operation_refused(result: Result<Value>, case: &str) {
    match result {
        Err(_) => {}
        Ok(value) => assert!(
            matches!(
                value.pointer("/operation/status").and_then(Value::as_str),
                Some("denied" | "failed")
            ),
            "{case} did not refuse before success: {value}"
        ),
    }
}

fn own_scope(fx: &Fx) -> Value {
    json!({"install_id":fx.install,"context_id":fx.context})
}

/// A live, correctly scoped assistant turn may execute a real bounded
/// read. The same turn cannot mint or decide its own permission, even
/// when it forges authority fields; the daemon guard refuses before any
/// permission state is created. This positive control prevents blanket
/// failure from satisfying the acceptance check.
#[test]
fn assistant_cannot_self_grant_or_decide_permissions() {
    let fx = Fx::new();
    let before = fx.customer("cust-a");
    let token = fx.create_turn("m-self-grant");
    let read = fx.invoke(
        "m-self-grant",
        &token,
        "customers.show",
        "op-read",
        json!({"customer_id":"cust-a"}),
    );
    assert!(read.is_ok(), "live scoped read control must work: {read:?}");
    for (method, mut params) in [
        (
            "app_assistant_decision",
            json!({"operation_id":"op-read","decision":"allow_once","expected_revision":1}),
        ),
        (
            "app_assistant_permission_block",
            json!({"action_id":"customer.tags.update","resource_id":"cust-a"}),
        ),
    ] {
        params["install_id"] = json!(fx.install);
        params["context_id"] = json!(fx.context);
        params["actor"] = json!("operator");
        params["operator"] = json!(true);
        let result = fx.call(Asserted::Agent("master".into()), method, params);
        refusal(result, method);
    }
    assert_eq!(
        fx.customer("cust-a"),
        before,
        "refused self-authorization changed customer data"
    );
    let permissions = fx.operator("app_assistant_permissions", own_scope(&fx));
    assert_eq!(
        permissions["permissions"].as_array().map(Vec::len),
        Some(0),
        "agent created permission state: {permissions}"
    );
    fx.finish("m-self-grant");
}

/// Registry and strict-schema guards reject arbitrary handler dispatch,
/// foreign install/context/customer scope and smuggled profile fields.
/// A failed request must leave both actual customer profiles untouched.
#[test]
fn invoke_refuses_forged_scope_unknown_actions_and_non_tag_profile_fields() {
    let fx = Fx::new();
    let before_a = fx.customer("cust-a");
    let before_b = fx.customer("cust-b");
    let token = fx.create_turn("m-forge");
    let cases = [
        (
            "unknown handler",
            "arbitrary.http.send",
            "op-unknown",
            json!({"url":"https://attacker.invalid"}),
        ),
        (
            "foreign customer",
            "customer.tags.update",
            "op-foreign-customer",
            json!({"customer_id":"not-owned","tags":["VIP"],"expected_revision":1}),
        ),
        (
            "consent smuggling",
            "customer.tags.update",
            "op-consent",
            json!({"customer_id":"cust-a","tags":["VIP"],"expected_revision":1,"consent":{"email":"granted"}}),
        ),
        (
            "email smuggling",
            "customer.tags.update",
            "op-email",
            json!({"customer_id":"cust-a","tags":["VIP"],"expected_revision":1,"email":"attacker@example.test"}),
        ),
        (
            "identity/profile smuggling",
            "customer.tags.update",
            "op-profile",
            json!({"customer_id":"cust-a","tags":["VIP"],"expected_revision":1,"display_name":"Changed","phone":"+85212345678","source":"changed"}),
        ),
        (
            "second customer smuggling",
            "customer.tags.update",
            "op-second",
            json!({"customer_id":"cust-a","tags":["VIP"],"expected_revision":1,"customer_ids":["cust-b"]}),
        ),
    ];
    for (case, action, operation, input) in cases {
        let result = fx.invoke("m-forge", &token, action, operation, input);
        refusal(result, case);
    }
    for (field, value) in [
        ("install_id", json!("another-install")),
        ("context_id", json!("another-context")),
    ] {
        let mut params = json!({"install_id":fx.install,"context_id":fx.context,
            "message":"m-forge","token":token,"action_id":"customers.show",
            "operation_id":format!("op-foreign-{field}"),"input":{"customer_id":"cust-a"}});
        params[field] = value;
        refusal(
            fx.call(
                Asserted::Agent("master".into()),
                "app_assistant_invoke",
                params,
            ),
            "foreign install/context scope",
        );
    }
    for extra in [
        json!({"handler":"email.send"}),
        json!({"resource_id":"cust-b"}),
        json!({"actor":"operator","approval_id":"forged"}),
    ] {
        let mut params = json!({"install_id":fx.install,"context_id":fx.context,
            "message":"m-forge","token":token,"action_id":"customers.show",
            "operation_id":"op-envelope","input":{"customer_id":"cust-a"}});
        for (key, value) in extra.as_object().unwrap() {
            params[key] = value.clone();
        }
        refusal(
            fx.call(
                Asserted::Agent("master".into()),
                "app_assistant_invoke",
                params,
            ),
            "forged invoke envelope",
        );
    }
    assert_eq!(
        fx.customer("cust-a"),
        before_a,
        "a refused invoke changed customer A"
    );
    assert_eq!(
        fx.customer("cust-b"),
        before_b,
        "a refused invoke changed customer B"
    );
    fx.finish("m-forge");
}

/// The tags pilot is permission-gated and resource-bound. Denying the
/// request changes no data and creates no standing grant; allow-always is
/// confined to one customer/action/installation, exact replay does not
/// write twice, a conflicting replay and stale revision refuse, and a
/// persisted block/revoke stops execution before the handler.
#[test]
fn tags_permission_decisions_are_exact_scoped_idempotent_and_revocable() {
    let fx = Fx::new();
    let before_a = fx.customer("cust-a");
    let before_b = fx.customer("cust-b");
    let token = fx.create_turn("m-tags-deny");
    let pending = fx
        .invoke(
            "m-tags-deny",
            &token,
            "customer.tags.update",
            "op-deny",
            json!({
                "customer_id":"cust-a","tags":["VIP"],"expected_revision":1,
            }),
        )
        .unwrap();
    assert_eq!(
        pending["operation"]["status"], "pending_permission",
        "tag write must await operator permission: {pending}"
    );
    let preview = &pending["operation"]["permission_request"]["preview"];
    assert_eq!(
        preview["customer_label"],
        before_a["profile"]["display_name"]
    );
    assert_eq!(preview["before_tags"], before_a["profile"]["tags"]);
    assert_eq!(preview["after_tags"], json!(["VIP"]));
    assert_eq!(preview["expected_revision"], before_a["revision"]);
    assert!(
        preview.get("email").is_none() && preview.get("consent").is_none(),
        "preview leaked profile fields: {preview}"
    );
    fx.finish("m-tags-deny");
    let denied = fx.operator(
        "app_assistant_decision",
        json!({
            "install_id":fx.install,"context_id":fx.context,"operation_id":"op-deny",
            "decision":"deny","expected_revision":pending["operation"]["revision"],
        }),
    );
    assert_eq!(
        denied["operation"]["status"], "denied",
        "deny must settle operation only: {denied}"
    );
    assert_eq!(fx.customer("cust-a"), before_a, "deny changed tags/profile");
    assert_eq!(
        fx.customer("cust-b"),
        before_b,
        "deny changed another customer"
    );
    let permissions = fx.operator("app_assistant_permissions", own_scope(&fx));
    assert_eq!(
        permissions["permissions"].as_array().map(Vec::len),
        Some(0),
        "deny-this-request created standing permission: {permissions}"
    );

    let token = fx.create_turn("m-tags-always");
    let pending = fx
        .invoke(
            "m-tags-always",
            &token,
            "customer.tags.update",
            "op-always",
            json!({
                "customer_id":"cust-a","tags":["VIP"],"expected_revision":1,
            }),
        )
        .unwrap();
    fx.finish("m-tags-always");
    let decision = fx.operator(
        "app_assistant_decision",
        json!({
            "install_id":fx.install,"context_id":fx.context,"operation_id":"op-always",
            "decision":"allow_always","expected_revision":pending["operation"]["revision"],
        }),
    );
    assert_eq!(
        decision["operation"]["status"], "succeeded",
        "valid exact operator decision must apply requested tags: {decision}"
    );
    let changed_a = fx.customer("cust-a");
    assert_eq!(
        changed_a["profile"]["tags"],
        json!(["VIP"]),
        "valid control did not apply tags"
    );
    assert_eq!(changed_a["profile"]["email"], before_a["profile"]["email"]);
    assert_eq!(
        changed_a["profile"]["consent"],
        before_a["profile"]["consent"]
    );
    assert_eq!(
        changed_a["profile"]["display_name"],
        before_a["profile"]["display_name"]
    );
    assert_eq!(
        fx.customer("cust-b"),
        before_b,
        "allow-always crossed customer scope"
    );
    let permissions = fx.operator("app_assistant_permissions", own_scope(&fx));
    let grant = permissions["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["effect"] == "allow" && p["state"] == "active")
        .expect("allow-always must be visible and revocable")
        .clone();
    assert_eq!(grant["action_id"], "customer.tags.update");
    assert_eq!(grant["resource_id"], "cust-a");
    let rev = grant["revision"].as_i64().expect("permission revision");

    // Same operation identity is exactly-once; reusing it for another
    // action/input or a different customer is a conflicting replay.
    let token = fx.create_turn("m-tags-replay");
    let exact = fx.invoke(
        "m-tags-replay",
        &token,
        "customer.tags.update",
        "op-always",
        json!({
            "customer_id":"cust-a","tags":["VIP"],"expected_revision":1,
        }),
    );
    assert!(
        exact.is_ok(),
        "exact replay returns the existing operation: {exact:?}"
    );
    let conflict = fx.invoke(
        "m-tags-replay",
        &token,
        "customer.tags.update",
        "op-always",
        json!({
            "customer_id":"cust-b","tags":["VIP"],"expected_revision":1,
        }),
    );
    refusal(conflict, "conflicting idempotency replay");
    let changed = fx.customer("cust-a");
    assert_eq!(
        changed["profile"]["tags"],
        json!(["VIP"]),
        "exact replay duplicated or altered write"
    );
    assert_eq!(
        fx.customer("cust-b"),
        before_b,
        "conflicting replay changed other customer"
    );
    fx.finish("m-tags-replay");

    // Grant does not transfer to a second customer. A new operation for
    // that resource must wait for a new decision and stale revisions fail.
    let token = fx.create_turn("m-tags-foreign");
    let foreign = fx
        .invoke(
            "m-tags-foreign",
            &token,
            "customer.tags.update",
            "op-foreign",
            json!({
                "customer_id":"cust-b","tags":["VIP"],"expected_revision":1,
            }),
        )
        .unwrap();
    assert_eq!(
        foreign["operation"]["status"], "pending_permission",
        "grant widened across resource: {foreign}"
    );
    fx.finish("m-tags-foreign");
    let stale = fx.call(Asserted::Operator, "app_assistant_decision", json!({
        "install_id":fx.install,"context_id":fx.context,"operation_id":"op-foreign",
        "decision":"allow_once","expected_revision":foreign["operation"]["revision"].as_i64().unwrap()-1,
    }));
    refusal(stale, "stale operation revision");
    assert_eq!(
        fx.customer("cust-b"),
        before_b,
        "stale decision changed record"
    );

    // Revocation invalidates the customer-A grant; a persistent block is
    // a separate deny and likewise prevents execution.
    fx.operator(
        "app_assistant_permission_revoke",
        json!({
            "install_id":fx.install,"context_id":fx.context,"permission_id":grant["id"],
            "expected_revision":rev,
        }),
    );
    let after_revoke_permissions = fx.operator("app_assistant_permissions", own_scope(&fx));
    assert!(
        after_revoke_permissions["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == grant["id"] && p["state"] == "revoked"),
        "revoked grant not visible: {after_revoke_permissions}"
    );
    let token = fx.create_turn("m-tags-revoked");
    let after_revoke = fx
        .invoke(
            "m-tags-revoked",
            &token,
            "customer.tags.update",
            "op-revoked",
            json!({
                "customer_id":"cust-a","tags":["VIP","post-revoke"],"expected_revision":2,
            }),
        )
        .unwrap();
    assert_eq!(
        after_revoke["operation"]["status"], "pending_permission",
        "revoked permission still authorized a write: {after_revoke}"
    );
    fx.finish("m-tags-revoked");
    let blocked = fx.operator(
        "app_assistant_permission_block",
        json!({
            "install_id":fx.install,"context_id":fx.context,
            "action_id":"customer.tags.update","resource_id":"cust-a",
        }),
    );
    assert!(
        blocked["permission"].is_object() || blocked["permissions"].is_array(),
        "block must persist an explicit deny: {blocked}"
    );
    let after_block = fx.operator("app_assistant_permissions", own_scope(&fx));
    assert!(
        after_block["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["action_id"] == "customer.tags.update"
                && p["resource_id"] == "cust-a"
                && p["effect"] == "deny"
                && p["state"] == "active"),
        "persistent block not visible as a scoped standing deny: {after_block}"
    );
    let token = fx.create_turn("m-tags-blocked");
    let blocked_write = fx.invoke(
        "m-tags-blocked",
        &token,
        "customer.tags.update",
        "op-blocked",
        json!({
            "customer_id":"cust-a","tags":["blocked"],"expected_revision":2,
        }),
    );
    operation_refused(blocked_write, "blocked tag update");
    assert_eq!(
        fx.customer("cust-a")["profile"]["tags"],
        json!(["VIP"]),
        "blocked write reached handler"
    );
    fx.finish("m-tags-blocked");
}

/// The permission preview is bound to the exact record revision. A real
/// operator edit after preview makes the decision stale; the decision is
/// refused and the intervening record stays unchanged by the assistant.
#[test]
fn stale_resource_revision_refuses_permission_decision() {
    let fx = Fx::new();
    let token = fx.create_turn("m-stale-resource");
    let pending = fx
        .invoke(
            "m-stale-resource",
            &token,
            "customer.tags.update",
            "op-stale-resource",
            json!({
                "customer_id":"cust-a","tags":["VIP"],"expected_revision":1,
            }),
        )
        .unwrap();
    let preview = &pending["operation"]["permission_request"]["preview"];
    assert_eq!(preview["before_tags"], json!(["initial"]));
    assert_eq!(preview["after_tags"], json!(["VIP"]));
    fx.finish("m-stale-resource");

    let records = RecordStore::open(fx.dir.path(), &fx.install).unwrap();
    let mut intervening: CustomerProfile =
        serde_json::from_value(fx.customer("cust-a")["profile"].clone()).unwrap();
    intervening.display_name = "Operator updated this customer".into();
    records
        .app_record_update(&fx.context, "cust-a", 1, &intervening, None)
        .unwrap();
    let changed = fx.customer("cust-a");
    let decision = fx.call(
        Asserted::Operator,
        "app_assistant_decision",
        json!({
            "install_id":fx.install,"context_id":fx.context,
            "operation_id":"op-stale-resource","decision":"allow_once",
            "expected_revision":pending["operation"]["revision"],
        }),
    );
    refusal(decision, "decision against stale customer revision");
    let after = fx.customer("cust-a");
    assert_eq!(
        after, changed,
        "stale decision changed the intervening operator edit"
    );
    assert_eq!(after["profile"]["tags"], json!(["initial"]));
    let shown = fx.operator(
        "app_assistant_operation_operator_show",
        json!({"install_id":fx.install,"context_id":fx.context,"operation_id":"op-stale-resource"}),
    );
    assert_eq!(
        shown["operation"]["status"], "pending_permission",
        "stale decision consumed pending operation: {shown}"
    );
}

/// No generic action is registered for send, freeze, content apply, or
/// operator approval. Attempts through a correctly proven turn fail
/// closed and leave CRM records and pending content state untouched.
#[test]
fn consequential_actions_are_not_available_through_generic_dispatch() {
    let fx = Fx::new();
    let before = fx.customer("cust-a");
    let token = fx.create_turn("m-consequential");
    for (action, operation, input) in [
        ("email.send", "op-send", json!({"campaign_id":"c1"})),
        ("audience.freeze", "op-freeze", json!({"campaign_id":"c1"})),
        ("content.apply", "op-apply", json!({"campaign_id":"c1"})),
        ("send.approve", "op-approve", json!({"campaign_id":"c1"})),
        (
            "campaigns.freeze",
            "op-freeze-alias",
            json!({"campaign_id":"c1"}),
        ),
    ] {
        refusal(
            fx.invoke("m-consequential", &token, action, operation, input),
            action,
        );
    }
    assert_eq!(
        fx.customer("cust-a"),
        before,
        "consequential refusal mutated customer"
    );
    fx.finish("m-consequential");
}

/// Agent-session HTTP calls cannot make operator decisions or standing
/// grants. This is the relay peer of the RPC actor-provenance check.
#[test]
fn board_http_refuses_agent_permission_decision_and_block() {
    let fx = Fx::new();
    let state = fx.dir.path().to_path_buf();
    let pm = state.join("pm");
    // Start the actual daemon socket and board against this fixture state.
    drop(fx.shared);
    let stop = Arc::new(AtomicBool::new(false));
    let daemon_stop = stop.clone();
    let daemon_state = state.clone();
    let opts = ServeOptions {
        test_seam: true,
        stop: Some(daemon_stop),
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let daemon = std::thread::spawn(move || serve_with(&daemon_state, opts).unwrap());
    let mut servers = ServerGuard {
        stop: stop.clone(),
        threads: vec![daemon],
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !state.join("cadence.sock").exists() || crate::test_seam::Seam::token_at(&state).is_none()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "daemon fixture did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // The board must bind a port of its own: `serve` reports Ok on the
    // startup channel only after its bind succeeds, so keeping that
    // thread is proof the listener answering `base` is this fixture's
    // board — a busy candidate means a neighbor holds it and is never
    // adopted or signalled. Retry only a real AddrInUse across the
    // finite candidate window under one shared 20s deadline; any other
    // startup result fails immediately.
    let startup_deadline = std::time::Instant::now() + Duration::from_secs(20);
    let first = (std::process::id() % 80) as u16;
    let port = {
        let mut offset = 0u16;
        loop {
            // The 20s startup deadline is shared by every candidate: no
            // new board thread starts once it has elapsed.
            assert!(
                std::time::Instant::now() < startup_deadline,
                "fixture board startup deadline elapsed"
            );
            let port = 3110 + (first + offset) % 80;
            let (ready, rx) = std::sync::mpsc::channel();
            let board_opts = crate::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(stop.clone()),
                startup: Some(ready),
                test_seam: true,
                ..Default::default()
            };
            let board_state = state.clone();
            let board_pm = pm.clone();
            let board = std::thread::spawn(move || {
                drop(crate::ui::serve(&board_state, &board_pm, &board_opts))
            });
            let remaining = startup_deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(remaining) {
                Ok(Ok(())) => {
                    servers.threads.push(board);
                    break port;
                }
                Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                    // The board's `serve` already returned on the bind
                    // failure; reap our own thread — surfacing a panic —
                    // before advancing. The shared stop flag stays unset:
                    // the fixture daemon still runs on it.
                    assert!(
                        board.join().is_ok(),
                        "fixture board thread panicked after AddrInUse on port {port}"
                    );
                    offset += 1;
                    assert!(offset < 80, "all 80 fixture board candidates busy");
                }
                Ok(Err(kind)) => {
                    assert!(
                        board.join().is_ok(),
                        "fixture board thread panicked after {kind:?} on port {port}"
                    );
                    panic!("fixture board startup failed on port {port}: {kind:?}");
                }
                Err(e) => {
                    // Channel dropped without a bind result, or the shared
                    // deadline elapsed — never a reason to try the next
                    // port. Hand the thread to the guard for cleanup.
                    servers.threads.push(board);
                    panic!("fixture board startup failed on port {port}: {e}");
                }
            }
        }
    };
    let token = crate::test_seam::Seam::token_at(&state).unwrap();
    let host = format!("cadence-{port}.localhost:{port}");
    let base = format!("http://127.0.0.1:{port}");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let call = |path: &str, body: Value| {
        let mut response = agent
            .post(format!("{base}{path}"))
            .header("Host", &host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{host}"))
            .header(crate::test_seam::AS_HEADER, "agent:master")
            .header(crate::test_seam::TOKEN_HEADER, &token)
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .unwrap();
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().unwrap_or_default();
        (status, text)
    };
    let path = format!(
        "/api/app-installations/{}/assistant/permissions/block?context_id={}",
        fx.install, fx.context
    );
    let (status, body) = call(
        &path,
        json!({"action_id":"customer.tags.update","resource_id":"cust-a"}),
    );
    assert!(
        status == 401 || status == 403,
        "agent-session block reached operator route: {status} {body}"
    );
    let (status, body) = call(
        &format!(
            "/api/app-installations/{}/assistant/operations/op-x/decision",
            fx.install
        ),
        json!({"decision":"allow_always","expected_revision":1,"actor":"operator"}),
    );
    assert!(
        status == 401 || status == 403,
        "agent-session decision reached operator route: {status} {body}"
    );

    // Positive control: the same route accepts a real operator connection
    // established through the fixture's one-time login link, not a body
    // field or test identity header alone.
    crate::operator_auth::ensure_secret(&state).unwrap();
    let secret = crate::operator_auth::read_secret(&state).unwrap();
    let nonce = scoped(Asserted::Operator, || {
        crate::client::rpc(
            &state,
            "operator_link_mint",
            json!({"secret":secret,"origin":"loopback"}),
        )
    })
    .unwrap()["nonce"]
        .clone();
    let session = agent
        .post(format!("{base}/api/session"))
        .header("Host", &host)
        .header("X-Cadence-Board", "1")
        .header("Origin", format!("http://{host}"))
        .header(crate::test_seam::AS_HEADER, "operator")
        .header(crate::test_seam::TOKEN_HEADER, &token)
        .header("Content-Type", "application/json")
        .send(json!({"nonce":nonce}).to_string())
        .unwrap();
    assert_eq!(
        session.status().as_u16(),
        200,
        "operator session positive control"
    );
    let set_cookie = session.headers()["set-cookie"].to_str().unwrap();
    let cookie = set_cookie[..set_cookie.find(';').unwrap()].to_owned();
    let session_body: Value = session.into_body().read_json().unwrap();
    let session_key = session_body["session_key"].as_str().unwrap();
    let mut block = agent
        .post(format!("{base}{path}"))
        .header("Host", &host)
        .header("X-Cadence-Board", "1")
        .header("Origin", format!("http://{host}"))
        .header(crate::test_seam::AS_HEADER, "operator")
        .header(crate::test_seam::TOKEN_HEADER, &token)
        .header("Cookie", cookie)
        .header("X-Cadence-Session", session_key)
        .header("Content-Type", "application/json")
        .send(json!({"action_id":"customer.tags.update","resource_id":"cust-a"}).to_string())
        .unwrap();
    let block_status = block.status().as_u16();
    let block_body = block.body_mut().read_to_string().unwrap_or_default();
    assert_eq!(
        block_status, 200,
        "real operator block should succeed: {block_body}"
    );

    drop(servers);
}

/// A live general CRM turn must complete the ticket's customer-to-campaign
/// journey against installation-owned records: find the seeded eligible
/// customer, save and preview a consented VIP segment, create a linked fresh
/// campaign draft, reload and show its audience link, then leave an editable
/// email proposal pending for the existing operator approval path.
#[test]
fn crm_general_turn_searches_segments_campaign_and_proposes_email() {
    let fx = Fx::new();
    let records = RecordStore::open(fx.dir.path(), &fx.install).unwrap();
    let mut eligible: CustomerProfile = serde_json::from_value(
        records.app_record_show(&fx.context, "cust-a").unwrap()["record"]["profile"].clone(),
    )
    .unwrap();
    eligible.tags = vec!["vip".into()];
    eligible.consent.email = ConsentState::Granted;
    records
        .app_record_update(
            &fx.context,
            "cust-a",
            1,
            &eligible,
            Some(&crate::store::app_records::ConsentProvenance {
                method: crate::store::app_records::ConsentMethod::WebForm,
                note: Some("CAD-1184 acceptance fixture".into()),
            }),
        )
        .unwrap();
    let seeded_a = records.app_record_show(&fx.context, "cust-a").unwrap();
    let seeded_b = records.app_record_show(&fx.context, "cust-b").unwrap();
    assert_eq!(seeded_a["record"]["profile"]["tags"], json!(["vip"]));
    assert_eq!(seeded_a["record"]["profile"]["consent"]["email"], "granted");
    assert_ne!(seeded_b["record"]["profile"]["tags"], json!(["vip"]));
    assert_ne!(seeded_b["record"]["profile"]["consent"]["email"], "granted");

    let token = fx.create_turn("m-crm-journey");
    let search = fx
        .invoke(
            "m-crm-journey",
            &token,
            "customers.search",
            "op-crm-search",
            json!({"query":"cust-a","limit":20}),
        )
        .unwrap();
    assert_eq!(search["operation"]["status"], "succeeded");
    let found = search["operation"]["result"]["records"]
        .as_array()
        .expect("customer search records");
    let found_ids: Vec<&str> = found
        .iter()
        .map(|row| {
            row.get("record").unwrap_or(row)["id"]
                .as_str()
                .expect("customer record id")
        })
        .collect();
    assert_eq!(
        found_ids,
        ["cust-a"],
        "search must find only the queried record"
    );
    assert!(search["operation"]["resource_refs"]
        .as_array()
        .is_some_and(|refs| {
            refs.iter()
                .any(|r| r["kind"] == "customer" && r["id"] == "cust-a")
        }));

    let predicates = json!([
        {"field":"consent_email","op":"eq","value":"granted"},
        {"field":"tag","op":"eq","value":"vip"}
    ]);
    let segment = fx
        .invoke(
            "m-crm-journey",
            &token,
            "segments.save",
            "op-crm-segment-save",
            json!({"segment_id":"seg-vip-consented","name":"Consented VIP","predicates":predicates}),
        )
        .unwrap();
    assert_eq!(segment["operation"]["status"], "succeeded");
    assert_eq!(
        segment["operation"]["result"]["segment"]["predicates"],
        predicates
    );
    assert!(segment["operation"]["resource_refs"]
        .as_array()
        .is_some_and(|refs| {
            refs.iter()
                .any(|r| r["kind"] == "segment" && r["id"] == "seg-vip-consented")
        }));

    let preview = fx
        .invoke(
            "m-crm-journey",
            &token,
            "segments.preview",
            "op-crm-segment-preview",
            json!({"segment_id":"seg-vip-consented"}),
        )
        .unwrap();
    assert_eq!(preview["operation"]["status"], "succeeded");
    assert_eq!(preview["operation"]["result"]["final_count"], 1);
    assert_eq!(
        preview["operation"]["result"]["sample"][0]["id"], "cust-a",
        "the real preview includes the seeded eligible customer"
    );
    assert!(preview["operation"]["resource_refs"]
        .as_array()
        .is_some_and(|refs| {
            refs.iter()
                .any(|r| r["kind"] == "segment" && r["id"] == "seg-vip-consented")
        }));

    let campaign = fx
        .invoke(
            "m-crm-journey",
            &token,
            "campaigns.create_draft",
            "op-crm-campaign-create",
            json!({"campaign_id":"campaign-vip-update","name":"VIP update","segment_id":"seg-vip-consented"}),
        )
        .unwrap();
    assert_eq!(campaign["operation"]["status"], "succeeded");
    assert_eq!(
        campaign["operation"]["result"]["content"]["campaign_id"],
        "campaign-vip-update"
    );
    assert_eq!(
        campaign["operation"]["result"]["content"]["name"],
        "VIP update"
    );
    assert!(campaign["operation"]["resource_refs"]
        .as_array()
        .is_some_and(|refs| {
            refs.iter()
                .any(|r| r["kind"] == "campaign" && r["id"] == "campaign-vip-update")
        }));

    // A separately opened record store proves persistence, not merely the
    // in-memory response. Keep the assertion on the content domain schema:
    // this optional field is null or a segment-linked draft audience.
    let reloaded = RecordStore::open(fx.dir.path(), &fx.install).unwrap();
    let stored_segment = reloaded
        .app_segment_show(&fx.context, "seg-vip-consented")
        .unwrap()["segment"]
        .clone();
    assert_eq!(stored_segment["predicates"], predicates);
    let stored_campaign = reloaded
        .app_content_show(&fx.context, "campaign-vip-update")
        .unwrap()["content"]
        .clone();
    assert_eq!(
        stored_campaign["draft_audience"],
        json!({"mode":"segment","segment_id":"seg-vip-consented"}),
        "campaign content must persist its selected segment after reload"
    );
    assert_eq!(stored_campaign["approval"]["valid"], false);

    let shown = fx
        .invoke(
            "m-crm-journey",
            &token,
            "campaigns.show",
            "op-crm-campaign-show",
            json!({"campaign_id":"campaign-vip-update"}),
        )
        .unwrap();
    assert_eq!(shown["operation"]["status"], "succeeded");
    assert_eq!(
        shown["operation"]["result"]["content"]["draft_audience"],
        stored_campaign["draft_audience"]
    );
    assert!(shown["operation"]["resource_refs"]
        .as_array()
        .is_some_and(|refs| {
            refs.iter()
                .any(|r| r["kind"] == "campaign" && r["id"] == "campaign-vip-update")
        }));

    let proposal = fx
        .invoke(
            "m-crm-journey",
            &token,
            "email.draft",
            "op-crm-email-draft",
            json!({
                "campaign_id":"campaign-vip-update",
                "proposal_id":"proposal-vip-update",
                "draft":{
                    "subject":"A note for our VIP members",
                    "preheader":"An update for you",
                    "blocks":[
                        {"type":"heading","text":"For our VIP members"},
                        {"type":"paragraph","text":"Thank you for being part of our community."}
                    ]
                }
            }),
        )
        .unwrap();
    assert_eq!(proposal["operation"]["status"], "succeeded");
    assert_eq!(
        proposal["operation"]["result"]["proposal"]["proposal_id"],
        "proposal-vip-update"
    );
    assert_eq!(
        proposal["operation"]["result"]["proposal"]["campaign_id"],
        "campaign-vip-update"
    );
    assert_eq!(
        proposal["operation"]["result"]["proposal"]["state"],
        "pending"
    );
    assert_eq!(
        proposal["operation"]["result"]["proposal"]["subject"],
        "A note for our VIP members"
    );
    assert!(proposal["operation"]["resource_refs"]
        .as_array()
        .is_some_and(|refs| {
            refs.iter()
                .any(|r| r["kind"] == "campaign" && r["id"] == "campaign-vip-update")
        }));
    let saved_proposal = RecordStore::open(fx.dir.path(), &fx.install)
        .unwrap()
        .app_content_proposal_show(&fx.context, "proposal-vip-update")
        .unwrap()["proposal"]
        .clone();
    assert_eq!(saved_proposal["state"], "pending");
    assert_eq!(saved_proposal["campaign_id"], "campaign-vip-update");
    let final_campaign = RecordStore::open(fx.dir.path(), &fx.install)
        .unwrap()
        .app_content_show(&fx.context, "campaign-vip-update")
        .unwrap()["content"]
        .clone();
    assert_eq!(
        final_campaign["draft_audience"],
        stored_campaign["draft_audience"]
    );
    assert_eq!(final_campaign["approval"]["valid"], false);
    assert!(
        RecordStore::open(fx.dir.path(), &fx.install)
            .unwrap()
            .app_campaign_sends(&fx.context, Some("campaign-vip-update"))
            .unwrap()
            .is_empty(),
        "drafting must not create a campaign send"
    );
    fx.finish("m-crm-journey");
}

/// Simulate a daemon interruption after the real tag side effect commits but
/// before its terminal operation receipt is trusted. Replaying the identical
/// operation must settle as unknown and must never execute the customer write
/// a second time; both operation read surfaces must expose the settlement.
#[test]
fn interrupted_running_operation_replay_is_unknown_without_duplicate_write() {
    let fx = Fx::new();
    let before = fx.customer("cust-a");
    let token = fx.create_turn("m-crash-after-write");
    let input = json!({
        "customer_id":"cust-a",
        "tags":["VIP"],
        "expected_revision":1
    });
    let pending = fx
        .invoke(
            "m-crash-after-write",
            &token,
            "customer.tags.update",
            "op-crash-after-write",
            input.clone(),
        )
        .unwrap();
    assert_eq!(pending["operation"]["status"], "pending_permission");
    let decided = fx.operator(
        "app_assistant_decision",
        json!({
            "install_id":fx.install,
            "context_id":fx.context,
            "operation_id":"op-crash-after-write",
            "decision":"allow_once",
            "expected_revision":pending["operation"]["revision"]
        }),
    );
    assert_eq!(decided["operation"]["status"], "succeeded");
    let after_write = fx.customer("cust-a");
    assert_eq!(after_write["profile"]["tags"], json!(input["tags"]));
    assert_eq!(
        after_write["revision"].as_i64(),
        before["revision"].as_i64().map(|revision| revision + 1),
        "the accepted operation must have committed exactly one customer revision"
    );

    // Recreate the durable mid-operation state from the actual stored
    // operation payload, preserving its normalized input and semantic digest.
    // This fixture-owned SQLite update models interruption after the customer
    // write but before a terminal receipt is durable; no production helper or
    // application RPC is faked or changed.
    let records = RecordStore::open(fx.dir.path(), &fx.install).unwrap();
    let conn = records.conn();
    let stored: String = conn
        .query_row(
            "SELECT payload FROM app_assistant_operations WHERE operation_id=? AND context_id=?",
            rusqlite::params!["op-crash-after-write", fx.context],
            |row| row.get(0),
        )
        .unwrap();
    let mut payload: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(payload["status"], "succeeded");
    assert_eq!(payload["_input"], input);
    assert!(payload["_operation_digest"].is_string());
    let retained_input = payload["_input"].clone();
    let retained_digest = payload["_operation_digest"].clone();
    payload["status"] = json!("running");
    payload["summary"] = json!("Interrupted before operation receipt");
    payload["result"] = Value::Null;
    payload["resource_refs"] = json!([]);
    payload["permission_request"] = Value::Null;
    payload["error"] = Value::Null;
    assert_eq!(payload["_input"], retained_input);
    assert_eq!(payload["_operation_digest"], retained_digest);
    let changed = conn
        .execute(
            "UPDATE app_assistant_operations SET status='running',payload=? WHERE operation_id=? AND context_id=?",
            rusqlite::params![payload.to_string(), "op-crash-after-write", fx.context],
        )
        .unwrap();
    assert_eq!(changed, 1, "fixture must alter the existing operation only");
    drop(conn);

    let replay = fx
        .invoke(
            "m-crash-after-write",
            &token,
            "customer.tags.update",
            "op-crash-after-write",
            input,
        )
        .unwrap();
    assert_eq!(
        replay["operation"]["status"], "unknown",
        "interrupted running operation must settle unknown, never resume"
    );
    let after_replay = fx.customer("cust-a");
    assert_eq!(
        after_replay["profile"], after_write["profile"],
        "replay repeated the customer side effect"
    );
    assert_eq!(
        after_replay["revision"], after_write["revision"],
        "replay changed the customer's revision a second time"
    );

    let shown = fx.operator(
        "app_assistant_operation_operator_show",
        json!({
            "install_id":fx.install,
            "context_id":fx.context,
            "operation_id":"op-crash-after-write"
        }),
    );
    assert_eq!(shown["operation"]["status"], "unknown");
    let listed = fx.operator("app_assistant_operations", own_scope(&fx));
    assert!(
        listed["operations"].as_array().is_some_and(|operations| {
            operations.iter().any(|operation| {
                operation["id"] == "op-crash-after-write" && operation["status"] == "unknown"
            })
        }),
        "operation listing must not leave the interrupted operation running: {listed}"
    );
    fx.finish("m-crash-after-write");
}
