//! CAD-1415 independent acceptance for generic package-declared record
//! actions (`app-actions/v1`, `record.create` / `record.update`).
//!
//! Authored independently of the source implementer from
//! `/tmp/crm-admin-01a13117/revised-acceptance.md`, not from the historical
//! core-first ticket body and not by adapting the obsolete
//! `cad1415_acceptance.rs` (which stays unregistered and untouched). Every
//! case drives the real `Shared::dispatch` guards: the live-turn proof on
//! `app_assistant_invoke`, the operator-connection proof on
//! `app_assistant_decision`, install consent and the PM/release guard on
//! `app_local_install_*`. The HTTP case reaches the real board route through
//! the test-seam board and daemon. Each case uses a per-test synthetic copy
//! of the CRM bundle with a paired `app-actions.json`; no registry action is
//! added. Every refusal sits beside a positive native control in the same
//! fixture, so a setup failure cannot count as a refusal.
//!
//! Not covered here: a live withdrawal interleaved with a pending decision
//! (needs a deterministic source seam; BLOCKED, see the final test's note),
//! a changed-digest declaration on the same install id, and package-only
//! A-to-B rollout and browser QA (separate pending outcomes).
use super::*;
use crate::store::app_contexts::ContextConfig;
use crate::store::app_records::{ConsentState, CustomerConsent, CustomerProfile, RecordStore};
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

#[path = "cad1415_runtime_fixture.rs"]
mod runtime_fixture;

const CRM_SOURCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/apps/crm");
const MASTER_GENERATION: &str = "0123456789abcdef0123456789abcdef";
const CREATE: &str = "crm.customer.create";
const UPDATE: &str = "crm.customer.update";

/// Paired package declarations: non-registry ids, closed operations, the
/// customer record kind and only the legacy `CustomerProfile` fields with
/// their exact declared types (see `validate_customer_bundle`).
const DECLARATIONS: &str = r#"{
  "contract": "app-actions/v1",
  "app": "crm",
  "title": "CRM customer actions",
  "actions": [
    {
      "id": "crm.customer.create",
      "title": "Create customer",
      "operation": "record.create",
      "record": "customer",
      "input": {"fields": [
        {"id": "display_name", "label": "Name", "type": "text", "required": true, "maxLength": 120},
        {"id": "email", "label": "Email", "type": "text", "maxLength": 200},
        {"id": "phone", "label": "Phone", "type": "text", "maxLength": 32},
        {"id": "tags", "label": "Tags", "type": "tags", "maxItems": 8},
        {"id": "source", "label": "Source", "type": "text", "maxLength": 64}
      ]}
    },
    {
      "id": "crm.customer.update",
      "title": "Update customer",
      "operation": "record.update",
      "record": "customer",
      "input": {"fields": [
        {"id": "phone", "label": "Phone", "type": "text", "maxLength": 32},
        {"id": "tags", "label": "Tags", "type": "tags", "maxItems": 8}
      ]}
    }
  ]
}
"#;

/// Copy the active CRM bundle into a per-test directory (symlinks skipped).
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let file_type = entry.file_type().unwrap();
        let target = to.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree(&entry.path(), &target);
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

struct Fx {
    dir: tempfile::TempDir,
    shared: Arc<Shared>,
    install: String,
    digest: String,
    context: String,
}

impl Fx {
    /// Synthetic fresh state: the CRM bundle plus `app-actions.json`,
    /// installed, approved on its current digest, one context and a seeded
    /// `cust-a` whose email consent is granted (so updates must preserve it).
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("c1415app")
            .tempdir()
            .unwrap();
        let source = dir.path().join("crm-src");
        copy_tree(Path::new(CRM_SOURCE), &source);
        std::fs::write(source.join(crate::issue::app_action::FILE), DECLARATIONS).unwrap();
        Self::seeded(dir, &source)
    }

    /// Same seeded state as [`Fx::new`], installing the bundle at `source`
    /// exactly as given. No declaration file is added here.
    fn seeded(dir: tempfile::TempDir, source: &Path) -> Self {
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd = dir.path().to_str().unwrap();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "master",
                provider: "pi",
                endpoint_kind: "managed",
                role: "worker",
                cwd,
                sandbox: "read-only",
                instructions: None,
                params: Some(r#"{"model":"fixture/model"}"#),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        crate::test_seam::seed_pi_fixture_policy(&shared.store, "fixture/model").unwrap();
        shared
            .store
            .set_identity_with_quota(
                "master",
                &crate::adapter::Identity {
                    thread_id: "cad1415-app-thread".into(),
                    session_id: "cad1415-app-session".into(),
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
                &json!({"source": source.to_str().unwrap()}),
                std::process::id(),
            )
        })
        .unwrap();
        let install = installed["install_id"].as_str().unwrap().to_owned();
        let digest = installed["digest"].as_str().unwrap().to_owned();
        // Native operator consent on the CURRENT digest; a refusal here is a
        // fixture failure, never a counted refusal case.
        scoped(Asserted::Operator, || {
            shared.dispatch(
                "app_local_install_approve",
                &json!({"install_id": install, "digest": digest}),
                std::process::id(),
            )
        })
        .expect("operator approval of the current installation digest");
        let config = ContextConfig::new("CAD-1415 app acceptance", BTreeMap::new()).unwrap();
        let context = shared
            .store
            .app_context_create(&install, &config, "cad1415-app-context")
            .unwrap()["context"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let records = RecordStore::open(dir.path(), &install).unwrap();
        records
            .app_record_create(
                &context,
                "cust-a",
                &CustomerProfile {
                    schema: 1,
                    display_name: "Customer cust-a".into(),
                    email: Some("cust-a@example.test".into()),
                    phone: Some("+85250000001".into()),
                    tags: vec!["initial".into()],
                    source: Some("acceptance".into()),
                    consent: CustomerConsent {
                        email: ConsentState::Granted,
                        sms: None,
                    },
                },
            )
            .unwrap();
        Self {
            dir,
            shared,
            install,
            digest,
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
    fn records(&self) -> RecordStore {
        RecordStore::open(self.dir.path(), &self.install).unwrap()
    }
    fn customer(&self, id: &str) -> Value {
        self.records().app_record_show(&self.context, id).unwrap()["record"].clone()
    }
    fn customer_maybe(&self, id: &str) -> Result<Value> {
        self.records().app_record_show(&self.context, id)
    }
    fn op_status(&self, op: &str) -> String {
        self.records()
            .app_assistant_operation_get(op, &self.context)
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    /// Start a chat turn and mark it running under a minted turn token.
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
    /// Agent proposal through the real caller path, with explicit scope.
    #[allow(clippy::too_many_arguments)]
    fn invoke_in(
        &self,
        install: &str,
        context: &str,
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
                "install_id":install,"context_id":context,"message":message,
                "token":token,"action_id":action,"operation_id":op,"input":input,
            }),
        )
    }
    fn invoke(
        &self,
        message: &str,
        token: &str,
        action: &str,
        op: &str,
        input: Value,
    ) -> Result<Value> {
        self.invoke_in(
            &self.install,
            &self.context,
            message,
            token,
            action,
            op,
            input,
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
    /// Operator decision on a pending operation (`Err` means refused).
    fn decide(&self, op: &str, decision: &str, expected_revision: i64) -> Result<Value> {
        self.call(
            Asserted::Operator,
            "app_assistant_decision",
            json!({
                "install_id":self.install,"context_id":self.context,
                "operation_id":op,"decision":decision,"expected_revision":expected_revision,
            }),
        )
    }
}

fn refusal<T: std::fmt::Debug>(result: Result<T>, case: &str) {
    assert!(result.is_err(), "{case} was accepted: {result:?}");
}

/// A refused invoke may also return a durable denied/failed receipt; both
/// are honest refusals. `pending_permission`, `running` and `succeeded` are not.
fn operation_refused(result: Result<Value>, case: &str) {
    match result {
        Err(_) => {}
        Ok(value) => assert!(
            matches!(
                value.pointer("/operation/status").and_then(Value::as_str),
                Some("denied" | "failed")
            ),
            "{case} did not refuse: {value}"
        ),
    }
}

fn pending(result: Result<Value>, case: &str) -> Value {
    let value = result.unwrap_or_else(|e| panic!("{case} must reach the guard: {e}"));
    assert_eq!(
        value["operation"]["status"], "pending_permission",
        "{case} must wait for an operator decision, got {value}"
    );
    value
}

fn op_revision(pending: &Value) -> i64 {
    pending["operation"]["revision"]
        .as_i64()
        .expect("pending operation revision")
}

fn create_input(id: &str, name: &str) -> Value {
    json!({
        "record_id":id,"display_name":name,
        "email":format!("{id}@example.test"),"phone":"+85250000009",
        "tags":["new"],"source":"chat",
    })
}

/// Positive native control: an accepted create reads back with unknown
/// consent defaults, and an accepted update patches only the declared
/// fields while consent and every unpatched field carry over.
#[test]
fn native_create_and_update_apply_only_after_exact_allow_once() {
    let fx = Fx::new();
    let token = fx.create_turn("m-control");
    let created = pending(
        fx.invoke(
            "m-control",
            &token,
            CREATE,
            "op-create",
            create_input("cust-new", "New Customer"),
        ),
        "create control",
    );
    let decided = fx
        .decide("op-create", "allow_once", op_revision(&created))
        .unwrap();
    assert_eq!(decided["operation"]["status"], "succeeded");
    let new = fx.customer("cust-new")["profile"].clone();
    assert_eq!(new["display_name"], "New Customer");
    assert_eq!(
        new["consent"]["email"], "unknown",
        "create must default consent to unknown"
    );

    let before = fx.customer("cust-a");
    let updated = pending(
        fx.invoke(
            "m-control",
            &token,
            UPDATE,
            "op-update",
            json!({"record_id":"cust-a","expected_revision":before["revision"],
                   "phone":"+85250000777","tags":["vip"]}),
        ),
        "update control",
    );
    let decided = fx
        .decide("op-update", "allow_once", op_revision(&updated))
        .unwrap();
    assert_eq!(decided["operation"]["status"], "succeeded");
    let after = fx.customer("cust-a");
    assert_eq!(after["profile"]["phone"], "+85250000777");
    assert_eq!(after["profile"]["tags"], json!(["vip"]));
    assert_eq!(
        after["profile"]["display_name"],
        before["profile"]["display_name"]
    );
    assert_eq!(after["profile"]["email"], before["profile"]["email"]);
    assert_eq!(after["profile"]["source"], before["profile"]["source"]);
    assert_eq!(
        after["profile"]["consent"], before["profile"]["consent"],
        "update must preserve consent"
    );
    assert_ne!(
        before["profile"]["consent"]["email"], "unknown",
        "seed consent must be non-default so preservation is meaningful"
    );
    assert!(after["revision"].as_i64() > before["revision"].as_i64());
    fx.finish("m-control");
}

/// Native refusals, each beside a positive control in the same fixture.
/// Every refused case leaves `cust-a` and the other customers unchanged.
#[test]
fn native_refusals_leave_customer_records_unchanged() {
    let fx = Fx::new();
    let ctx = fx.context.clone();
    let token = fx.create_turn("m-battery");
    let before = fx.customer("cust-a");

    // Positive control: a valid proposal in this turn settles succeeded.
    let control = pending(
        fx.invoke(
            "m-battery",
            &token,
            CREATE,
            "op-ok",
            create_input("cust-ok", "OK Customer"),
        ),
        "battery control",
    );
    fx.decide("op-ok", "allow_once", op_revision(&control))
        .unwrap();
    let ok_record = fx.customer("cust-ok");

    // Wrong installation and wrong context: the turn is not in that scope.
    operation_refused(
        fx.invoke_in(
            "inst-foreign",
            &ctx,
            "m-battery",
            &token,
            CREATE,
            "op-foreign-inst",
            create_input("cust-x1", "X1"),
        ),
        "wrong installation",
    );
    operation_refused(
        fx.invoke_in(
            &fx.install,
            "ctx-foreign",
            "m-battery",
            &token,
            CREATE,
            "op-foreign-ctx",
            create_input("cust-x2", "X2"),
        ),
        "wrong context",
    );
    // Foreign or forged turn token on a running message.
    refusal(
        fx.invoke(
            "m-battery",
            "pi-forged-turn-token",
            CREATE,
            "op-forged-token",
            create_input("cust-x3", "X3"),
        ),
        "forged turn token",
    );
    // Forged consent and unknown keys are refused at admission.
    let mut forged = create_input("cust-x4", "X4");
    forged["consent"] = json!({"email":"granted"});
    operation_refused(
        fx.invoke("m-battery", &token, CREATE, "op-consent", forged),
        "forged consent",
    );
    let mut unknown = create_input("cust-x5", "X5");
    unknown["customer_id"] = json!("cust-x5");
    operation_refused(
        fx.invoke("m-battery", &token, CREATE, "op-unknown", unknown),
        "unknown key",
    );
    let mut create_rev = create_input("cust-x6", "X6");
    create_rev["expected_revision"] = json!(1);
    operation_refused(
        fx.invoke("m-battery", &token, CREATE, "op-create-rev", create_rev),
        "create with revision",
    );
    // Update without a record id: no implicit target selection.
    operation_refused(
        fx.invoke(
            "m-battery",
            &token,
            UPDATE,
            "op-no-target",
            json!({"expected_revision":before["revision"],"phone":"+1"}),
        ),
        "update without record id",
    );
    // Duplicate id on create refuses and does not overwrite cust-a.
    operation_refused(
        fx.invoke(
            "m-battery",
            &token,
            CREATE,
            "op-dup",
            create_input("cust-a", "Overwrite"),
        ),
        "duplicate create",
    );
    // Stale expected revision on update refuses at proposal time.
    operation_refused(
        fx.invoke(
            "m-battery",
            &token,
            UPDATE,
            "op-stale-proposal",
            json!({"record_id":"cust-a","expected_revision":before["revision"].as_i64().unwrap() + 7,"phone":"+1"}),
        ),
        "stale proposal revision",
    );
    for id in [
        "cust-x1", "cust-x2", "cust-x3", "cust-x4", "cust-x5", "cust-x6", "cust-x6", "cust-a",
    ] {
        if id == "cust-a" {
            continue;
        }
        assert!(
            fx.customer_maybe(id).is_err(),
            "{id} was stored by a refused proposal"
        );
    }
    assert_eq!(
        fx.customer("cust-a"),
        before,
        "refused proposals changed cust-a"
    );

    // Agent self-decision on a valid pending update: every route is refused.
    let self_pending = pending(
        fx.invoke(
            "m-battery",
            &token,
            UPDATE,
            "op-self",
            json!({"record_id":"cust-a","expected_revision":before["revision"],"tags":["self"]}),
        ),
        "self-decision arm",
    );
    let self_rev = op_revision(&self_pending);
    for who in [
        Asserted::Agent("master".into()),
        Asserted::Agent("ghost".into()),
        Asserted::Unproven,
    ] {
        refusal(
            fx.call(
                who,
                "app_assistant_decision",
                json!({"install_id":fx.install,"context_id":ctx,"operation_id":"op-self",
                       "decision":"allow_once","expected_revision":self_rev}),
            ),
            "non-operator decision",
        );
    }
    refusal(
        fx.call(
            Asserted::Agent("master".into()),
            "app_assistant_decision",
            json!({"install_id":fx.install,"context_id":ctx,"operation_id":"op-self",
                   "decision":"allow_once","expected_revision":self_rev,"actor":"operator","operator":true}),
        ),
        "forged identity fields on decision",
    );
    refusal(
        fx.decide("op-self", "allow_always", self_rev),
        "allow_always on a record action",
    );
    refusal(
        fx.call(
            Asserted::Operator,
            "app_assistant_decision",
            json!({"install_id":fx.install,"context_id":"ctx-foreign","operation_id":"op-self",
                   "decision":"allow_once","expected_revision":self_rev}),
        ),
        "operator decision under a foreign context",
    );
    assert_eq!(
        fx.op_status("op-self"),
        "pending_permission",
        "refused decisions moved the operation"
    );
    assert_eq!(
        fx.customer("cust-a"),
        before,
        "a refused self-decision mutated cust-a"
    );

    // The real operator allows once; the update applies exactly the preview.
    fx.decide("op-self", "allow_once", self_rev).unwrap();
    let patched = fx.customer("cust-a");
    assert_eq!(patched["profile"]["tags"], json!(["self"]));
    assert_eq!(patched["profile"]["phone"], before["profile"]["phone"]);
    assert_eq!(patched["profile"]["consent"], before["profile"]["consent"]);

    // Stale CAS: the record moves after the preview; the decision refuses
    // and settles failed without overwriting the newer record.
    let stale_pending = pending(
        fx.invoke(
            "m-battery",
            &token,
            UPDATE,
            "op-stale-cas",
            json!({"record_id":"cust-a","expected_revision":patched["revision"],"phone":"+85250000321"}),
        ),
        "stale CAS arm",
    );
    let mut moved = fx.customer("cust-a")["profile"].clone();
    moved["phone"] = json!("+85255555555");
    let moved = CustomerProfile::parse(&moved).unwrap();
    fx.records()
        .app_record_update(
            &ctx,
            "cust-a",
            patched["revision"].as_i64().unwrap(),
            &moved,
            None,
        )
        .unwrap();
    let newer = fx.customer("cust-a");
    refusal(
        fx.decide("op-stale-cas", "allow_once", op_revision(&stale_pending)),
        "decision on a stale preview",
    );
    assert_eq!(fx.op_status("op-stale-cas"), "failed");
    assert_eq!(
        fx.customer("cust-a"),
        newer,
        "stale decision overwrote the newer record"
    );

    // Denied: nothing is stored.
    let denied = pending(
        fx.invoke(
            "m-battery",
            &token,
            CREATE,
            "op-deny",
            create_input("cust-deny", "Denied"),
        ),
        "denial arm",
    );
    let decided = fx.decide("op-deny", "deny", op_revision(&denied)).unwrap();
    assert_eq!(decided["operation"]["status"], "denied");
    assert!(
        fx.customer_maybe("cust-deny").is_err(),
        "denied create stored a customer"
    );

    // Identical immutable receipt replay: no second write.
    let replay = fx
        .invoke(
            "m-battery",
            &token,
            CREATE,
            "op-ok",
            create_input("cust-ok", "OK Customer"),
        )
        .unwrap();
    assert_eq!(replay["operation"]["status"], "succeeded");
    assert_eq!(
        fx.customer("cust-ok"),
        ok_record,
        "identical replay rewrote cust-ok"
    );
    // Changed payload reusing the same operation id refuses.
    refusal(
        fx.invoke(
            "m-battery",
            &token,
            CREATE,
            "op-ok",
            create_input("cust-ok", "Changed Name"),
        ),
        "changed payload reuse",
    );
    assert_eq!(
        fx.customer("cust-ok"),
        ok_record,
        "changed payload reuse rewrote cust-ok"
    );

    // Foreign or stale turn: once the turn is no longer running, nothing
    // is proposed and no operation row is created.
    fx.finish("m-battery");
    refusal(
        fx.invoke(
            "m-battery",
            &token,
            CREATE,
            "op-stale-turn",
            create_input("cust-st", "Stale"),
        ),
        "stale turn",
    );
    assert!(
        fx.records()
            .app_assistant_operation_get("op-stale-turn", &ctx)
            .is_err(),
        "stale-turn proposal created an operation row"
    );
    assert!(fx.customer_maybe("cust-st").is_err());
}

/// Withdrawn package declaration, sequential: the operator revokes the
/// install through the real PM/release-guarded lifecycle. A pending decision
/// and a new proposal then refuse, with unchanged data. The live
/// withdrawal-during-decision interleaving is NOT exercised here (BLOCKED:
/// it needs a deterministic source seam; no timing sleeps are used).
#[test]
fn native_withdrawn_declaration_refuses_pending_and_new_proposals() {
    let fx = Fx::new();
    let token = fx.create_turn("m-withdraw");
    let control = pending(
        fx.invoke(
            "m-withdraw",
            &token,
            CREATE,
            "op-w1",
            create_input("cust-w1", "Before Revoke"),
        ),
        "withdrawal control",
    );
    fx.decide("op-w1", "allow_once", op_revision(&control))
        .unwrap();
    assert_eq!(
        fx.customer("cust-w1")["profile"]["display_name"],
        "Before Revoke"
    );

    let waiting = pending(
        fx.invoke(
            "m-withdraw",
            &token,
            CREATE,
            "op-w2",
            create_input("cust-w2", "Pending At Revoke"),
        ),
        "pending before withdrawal",
    );
    let before = fx.customer("cust-a");
    fx.operator(
        "app_local_install_revoke",
        json!({"install_id":fx.install,"digest":fx.digest}),
    );

    refusal(
        fx.decide("op-w2", "allow_once", op_revision(&waiting)),
        "decision after withdrawal",
    );
    assert!(
        fx.customer_maybe("cust-w2").is_err(),
        "withdrawn declaration stored cust-w2"
    );
    refusal(
        fx.invoke(
            "m-withdraw",
            &token,
            CREATE,
            "op-w3",
            create_input("cust-w3", "After Revoke"),
        ),
        "new proposal after withdrawal",
    );
    assert!(
        fx.customer_maybe("cust-w3").is_err(),
        "withdrawn declaration proposed cust-w3"
    );
    if let Ok(actions) = fx.call(
        Asserted::Operator,
        "app_assistant_actions_operator",
        json!({"install_id":fx.install,"context_id":fx.context}),
    ) {
        let listed = actions["actions"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .any(|row| row["id"] == CREATE || row["id"] == UPDATE)
            })
            .unwrap_or(false);
        assert!(!listed, "withdrawn declaration still discoverable");
    }
    assert_eq!(fx.customer("cust-a"), before, "withdrawal changed cust-a");
    assert_eq!(
        fx.customer("cust-w1")["profile"]["display_name"],
        "Before Revoke",
        "withdrawal removed prior data"
    );
    fx.finish("m-withdraw");
}

/// Real board HTTP peer on the same daemon state. The agent session and a
/// forged-field body are refused by the route; the real operator session
/// (login link, session key, cookie) applies the reviewed create once with
/// unknown consent; a replayed decision on the settled operation is refused.
#[test]
fn http_board_decision_is_no_weaker_than_the_daemon_guard() {
    if let Ok(root) = std::env::var(runtime_fixture::ROOT_ENV) {
        runtime_fixture::run(Path::new(&root));
    }
    let fx = Fx::new();
    let token = fx.create_turn("m-http");
    let pending = pending(
        fx.invoke(
            "m-http",
            &token,
            CREATE,
            "op-http",
            create_input("cust-http", "HTTP Customer"),
        ),
        "http create arm",
    );
    fx.finish("m-http");
    let rev = op_revision(&pending);
    // Consume the fixture: the tempdir stays alive as `dir` through HTTP
    // teardown; `Shared` is dropped before the daemon reopens the same state.
    let Fx {
        dir,
        shared,
        install,
        context: ctx,
        ..
    } = fx;
    let state = dir.path().to_path_buf();
    let pm = state.join("pm");
    drop(shared);
    let stop = Arc::new(AtomicBool::new(false));
    let opts = ServeOptions {
        test_seam: true,
        stop: Some(stop.clone()),
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let daemon_state = state.clone();
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
    let startup_deadline = std::time::Instant::now() + Duration::from_secs(20);
    let first = (std::process::id() % 80) as u16;
    let port = {
        let mut offset = 0u16;
        loop {
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
                    assert!(board.join().is_ok(), "board thread panicked on port {port}");
                    offset += 1;
                    assert!(offset < 80, "all 80 fixture board candidates busy");
                }
                Ok(Err(kind)) => {
                    assert!(board.join().is_ok(), "board thread panicked on port {port}");
                    panic!("fixture board startup failed on port {port}: {kind:?}");
                }
                Err(e) => {
                    servers.threads.push(board);
                    panic!("fixture board startup failed on port {port}: {e}");
                }
            }
        }
    };
    let seam_token = crate::test_seam::Seam::token_at(&state).unwrap();
    let host = format!("cadence-{port}.localhost:{port}");
    let base = format!("http://127.0.0.1:{port}");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let post = |path: &str, as_identity: &str, extra: &[(&str, String)], body: Value| {
        let mut request = agent
            .post(format!("{base}{path}"))
            .header("Host", &host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{host}"))
            .header(crate::test_seam::AS_HEADER, as_identity)
            .header(crate::test_seam::TOKEN_HEADER, &seam_token)
            .header("Content-Type", "application/json");
        for (name, value) in extra {
            request = request.header(*name, value);
        }
        let mut response = request.send(body.to_string()).unwrap();
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().unwrap_or_default();
        (status, text)
    };
    // The operator route pattern is `/api/app-installations/*/assistant/operations/*/decision`.
    let decision_path =
        format!("/api/app-installations/{install}/assistant/operations/op-http/decision");
    let records = RecordStore::open(&state, &install).unwrap();
    let read = |id: &str| records.app_record_show(&ctx, id).unwrap()["record"].clone();
    let before = read("cust-a");

    // Agent session: refused by the route before any operator logic runs.
    let (status, body) = post(
        &decision_path,
        "agent:master",
        &[],
        json!({"decision":"allow_once","expected_revision":rev}),
    );
    assert!(
        status == 401 || status == 403 || status == 404,
        "agent-session HTTP decision reached the operator route: {status} {body}"
    );
    // Forged identity fields are refused by the strict schema.
    let (status, body) = post(
        &decision_path,
        "agent:master",
        &[],
        json!({"decision":"allow_once","expected_revision":rev,"actor":"operator","approval_id":"forged"}),
    );
    assert!(
        matches!(status, 400 | 401 | 403 | 404),
        "forged decision fields reached the operator route: {status} {body}"
    );
    assert!(
        records.app_record_show(&ctx, "cust-http").is_err(),
        "agent HTTP path stored cust-http"
    );
    assert_eq!(
        records
            .app_assistant_operation_get("op-http", &ctx)
            .unwrap()["status"],
        "pending_permission",
        "refused HTTP decisions moved the operation"
    );
    assert_eq!(
        read("cust-a"),
        before,
        "refused HTTP decisions changed cust-a"
    );

    // Positive control: a real operator session (login link, session key,
    // cookie) decides the pending create exactly once over the same route.
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
        .header(crate::test_seam::TOKEN_HEADER, &seam_token)
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
    let session_key = session_body["session_key"].as_str().unwrap().to_owned();
    let operator_headers = [
        ("Cookie", cookie.clone()),
        ("X-Cadence-Session", session_key),
    ];
    let (status, body) = post(
        &decision_path,
        "operator",
        &operator_headers,
        json!({"decision":"allow_once","expected_revision":rev}),
    );
    assert_eq!(
        status, 200,
        "real operator HTTP decision must apply the create: {status} {body}"
    );
    let stored = read("cust-http");
    assert_eq!(stored["profile"]["display_name"], "HTTP Customer");
    assert_eq!(
        stored["profile"]["consent"]["email"], "unknown",
        "HTTP create must default consent to unknown"
    );
    assert_eq!(read("cust-a"), before, "HTTP create changed cust-a");

    // Replay of the settled operation over the same route is refused.
    let (status, body) = post(
        &decision_path,
        "operator",
        &operator_headers,
        json!({"decision":"allow_once","expected_revision":rev}),
    );
    assert!(
        status == 409 || status == 404 || status == 400,
        "settled-operation HTTP replay reached apply: {status} {body}"
    );
    assert_eq!(read("cust-http"), stored, "HTTP replay rewrote the record");

    drop(servers);
}

/// Archived-context re-proof, sequential and deterministic. The outer
/// admission proves the context is active; an operator archive then lands
/// before the decision. `decide_record_action` is called directly with the
/// raw stored operation, so its own fenced re-proof (inside the PM snapshot,
/// under the release lock) must refuse with no write. This is a helper-level
/// boundary proof only: it schedules no threads and exercises no operator
/// auth (the other four tests cover those).
#[test]
fn archived_context_refuses_fenced_decision_with_unchanged_records() {
    let fx = Fx::new();
    let ctx = fx.context.clone();
    let token = fx.create_turn("m-archived");

    // Positive native control in the active context.
    let control = pending(
        fx.invoke(
            "m-archived",
            &token,
            CREATE,
            "op-arch-ok",
            create_input("cust-arch-ok", "Archive Control"),
        ),
        "archived-context control",
    );
    fx.decide("op-arch-ok", "allow_once", op_revision(&control))
        .unwrap();
    let control_record = fx.customer("cust-arch-ok");
    let before_customer = fx.customer("cust-a");

    // Second pending request, then the raw stored operation and its input.
    let waiting = pending(
        fx.invoke(
            "m-archived",
            &token,
            CREATE,
            "op-arch-2",
            create_input("cust-arch-2", "Pending Archive"),
        ),
        "archived-context pending request",
    );
    let records = fx.records();
    let raw_op = records
        .app_assistant_operation_get("op-arch-2", &ctx)
        .unwrap();
    let pending_revision = op_revision(&waiting);
    assert_eq!(raw_op["status"], "pending_permission");
    assert_eq!(raw_op["revision"].as_i64(), Some(pending_revision));
    let raw_before = raw_op.clone();

    // Explicitly prove the active outer admission state first.
    let (_, proof) = fx
        .shared
        .store
        .app_context_proof(&fx.install, &ctx)
        .expect("context active before archive");
    assert_eq!(proof.id, ctx);

    // Archive through the real operator RPC at the current revision.
    let shown = fx.operator(
        "app_context_show",
        json!({"install_id":fx.install,"context_id":ctx}),
    );
    let revision = shown
        .pointer("/context/revision")
        .and_then(Value::as_i64)
        .expect("context revision");
    assert_eq!(revision, proof.revision);
    fx.operator(
        "app_context_archive",
        json!({"install_id":fx.install,"context_id":ctx,"expected_revision":revision}),
    );
    assert!(
        fx.shared
            .store
            .app_context_proof(&fx.install, &ctx)
            .is_err(),
        "archive did not retire the context"
    );

    // Direct call into the real fenced helper with the raw pending row.
    match super::app_record_actions_rpc::decide_record_action(
        &fx.shared,
        &records,
        &fx.install,
        &ctx,
        &raw_op,
        "op-arch-2",
        pending_revision,
    ) {
        Err(error) => assert!(
            error.to_string().contains("archived"),
            "wrong refusal after archive: {error}"
        ),
        Ok(value) => panic!("decision after archive was accepted: {value}"),
    }
    assert!(
        fx.customer_maybe("cust-arch-2").is_err(),
        "archived context stored the proposed customer"
    );
    assert_eq!(
        records
            .app_assistant_operation_get("op-arch-2", &ctx)
            .unwrap(),
        raw_before,
        "pending row or revision changed after refused decision"
    );
    assert_eq!(
        fx.customer("cust-arch-ok"),
        control_record,
        "archive changed the positive control"
    );
    assert_eq!(
        fx.customer("cust-a"),
        before_customer,
        "archive changed cust-a"
    );
    fx.finish("m-archived");
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
