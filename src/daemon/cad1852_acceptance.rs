//! CAD-1852 acceptance check, written by the independent Spec/security
//! reviewer from the ticket (operator comments of 2026-10-27 02:05:36Z and
//! 02:15:16Z), NOT by the implementer, who may not edit or weaken it.
//!
//! The rule: once the operator allows an agent, it uses its tools
//! regardless of cost. "Always allow" on a paid Social action is a standing
//! grant; a later agent call runs with no card even when the live price
//! moved, and the provider receives the FRESH quote as `max_charge_minor`.
//! Balance is the only cost stop: a 402 `insufficient_funds` charges
//! nothing and says "Not enough credit — top up to continue". Authority is
//! unchanged: only the operator grants or revokes, revoke stops the agent,
//! and Designer image refine stays allow-once (CAD-1868).
//!
//! Real guards only: every step goes through `Shared::dispatch` —
//! `app_assistant_invoke` behind the live-turn verifier, and
//! `app_assistant_decision` / `app_assistant_permission_revoke` /
//! `app_assistant_permission_block` behind the operator-connection proof —
//! with the real `AgenticosExternalAdapter` and the CAD-1327 loopback door.
//! The one store-level step is parking a refine operation (case e), which
//! the decision RPC then judges exactly as it would a parked card; a
//! positive control in the same case shows the decision reaches that gate.
//! Every refusal asserts its reason, not just an error.

use super::cad1327_acceptance::{descriptor, pending, refused, Fx, DRAFT, FETCH, STATUS};
use crate::test_seam::Asserted;
use serde_json::{json, Value};
use std::sync::atomic::Ordering::SeqCst;

const OUT_OF_CREDIT: &str = "Not enough credit — top up to continue";

/// `max_charge_minor` of every paid call the door received for `slug`.
fn ceilings(fx: &Fx, slug: &str) -> Vec<u64> {
    fx.door
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|body| body["slug"] == slug)
        .map(|body| {
            body["max_charge_minor"]
                .as_u64()
                .unwrap_or_else(|| panic!("paid call without max_charge_minor: {body}"))
        })
        .collect()
}

fn grants(fx: &Fx) -> Vec<Value> {
    fx.operator(
        "app_assistant_permissions",
        json!({"install_id":fx.install,"context_id":fx.context}),
    )["permissions"]
        .as_array()
        .unwrap()
        .clone()
}

fn active_grant(fx: &Fx, action: &str) -> Option<Value> {
    grants(fx)
        .into_iter()
        .find(|g| g["action_id"] == action && g["effect"] == "allow" && g["state"] == "active")
}

fn receipts(fx: &Fx) -> i64 {
    fx.db()
        .query_row("SELECT COUNT(*) FROM app_tool_results", [], |r| r.get(0))
        .unwrap()
}

/// A reply that ran with no card: no permission request is outstanding.
fn ran_without_card(reply: &Value, what: &str) -> Value {
    let op = reply["operation"].clone();
    assert_eq!(op["status"], "succeeded", "{what}: {reply}");
    assert!(
        op["permission_request"].is_null(),
        "{what} still carries a card: {reply}"
    );
    op
}

/// (a) After the operator's "Always allow", a second agent call of the same
/// paid action runs with no card, even though the live price is now above
/// the first card's estimate; the provider receives the fresh quote as
/// `max_charge_minor`. Covers both paid Social actions.
#[test]
fn a_allow_always_runs_the_next_paid_call_with_no_card_at_the_fresh_quote() {
    let fx = Fx::new();
    let token = fx.create_turn("m-a");

    // social.posts.fetch
    let card = pending(
        &fx.invoke("m-a", &token, "social.posts.fetch", "op-a1", json!({}))
            .unwrap(),
    );
    let reason = card["permission_request"]["reason"].as_str().unwrap();
    assert!(reason.contains("USD 0.002"), "card estimate: {reason}");
    let done = fx
        .decide(Asserted::Operator, &card, "allow_always")
        .unwrap();
    assert_eq!(done["operation"]["status"], "succeeded", "{done}");
    assert_eq!(ceilings(&fx, "read_instagram_posts"), vec![2000]);
    let grant = active_grant(&fx, "social.posts.fetch").expect("standing fetch grant");
    assert_eq!(grant["resource_id"], "any", "{grant}");

    *fx.door.read_price.lock().unwrap() = "0.007500".into();
    let ran = fx
        .invoke("m-a", &token, "social.posts.fetch", "op-a2", json!({}))
        .unwrap();
    ran_without_card(&ran, "granted fetch above the estimate");
    assert_eq!(
        ceilings(&fx, "read_instagram_posts"),
        vec![2000, 7500],
        "the second fetch must carry the fresh quote, not the card's estimate"
    );
    assert_eq!(fx.door.reads.load(SeqCst), 2);

    // social.draft.create
    let card = pending(
        &fx.invoke(
            "m-a",
            &token,
            "social.draft.create",
            "op-a3",
            json!({"post":"newest"}),
        )
        .unwrap(),
    );
    let done = fx
        .decide(Asserted::Operator, &card, "allow_always")
        .unwrap();
    assert_eq!(done["operation"]["status"], "succeeded", "{done}");
    assert_eq!(ceilings(&fx, "generate_text"), vec![2000]);
    assert!(active_grant(&fx, "social.draft.create").is_some());

    *fx.door.text_price.lock().unwrap() = "0.004000".into();
    let ran = fx
        .invoke(
            "m-a",
            &token,
            "social.draft.create",
            "op-a4",
            json!({"post":"newest"}),
        )
        .unwrap();
    let op = ran_without_card(&ran, "granted draft above the estimate");
    assert!(op["result"]["draft_id"].is_string(), "{ran}");
    assert_eq!(
        ceilings(&fx, "generate_text"),
        vec![2000, 4000],
        "the second draft must carry the fresh quote"
    );
    assert_eq!(fx.door.text_calls.load(SeqCst), 2);
    assert_eq!(fx.drafts(&fx.context), 2);
}

/// (b) A 402 `insufficient_funds` charges nothing and the operation says
/// "Not enough credit — top up to continue" — on the operator's decision and
/// under a standing grant alike. A replay never re-calls the provider.
#[test]
fn b_insufficient_funds_charges_nothing_and_says_not_enough_credit() {
    let fx = Fx::new();
    let token = fx.create_turn("m-b");
    fx.fetched("m-b", &token, "op-bf");
    let before = receipts(&fx);
    let card = pending(
        &fx.invoke(
            "m-b",
            &token,
            "social.draft.create",
            "op-b1",
            json!({"post":"newest"}),
        )
        .unwrap(),
    );
    fx.door.no_credit.store(true, SeqCst);
    let done = fx
        .decide(Asserted::Operator, &card, "allow_always")
        .unwrap();
    assert_eq!(done["operation"]["status"], "failed", "{done}");
    assert_eq!(done["operation"]["summary"], OUT_OF_CREDIT, "{done}");
    assert_eq!(
        ceilings(&fx, "generate_text").len(),
        1,
        "the call must reach the provider"
    );
    assert_eq!(
        fx.door.text_calls.load(SeqCst),
        0,
        "an unfunded call charged"
    );
    assert_eq!(receipts(&fx), before, "an unfunded call left a receipt");

    // Under the standing grant: still the plain message, still no charge.
    let ran = fx
        .invoke(
            "m-b",
            &token,
            "social.draft.create",
            "op-b2",
            json!({"post":"newest"}),
        )
        .unwrap();
    assert_eq!(ran["operation"]["status"], "failed", "{ran}");
    assert_eq!(ran["operation"]["summary"], OUT_OF_CREDIT, "{ran}");
    assert_eq!(receipts(&fx), before);
    // A replay of the refused operation returns its outcome; no new call.
    let calls = fx.door.calls.lock().unwrap().len();
    let replay = fx
        .invoke(
            "m-b",
            &token,
            "social.draft.create",
            "op-b2",
            json!({"post":"newest"}),
        )
        .unwrap();
    assert_eq!(replay["operation"]["status"], "failed", "{replay}");
    assert_eq!(
        fx.door.calls.lock().unwrap().len(),
        calls,
        "replay re-called"
    );

    // Positive control: with credit the same granted action charges once.
    fx.door.no_credit.store(false, SeqCst);
    let ran = fx
        .invoke(
            "m-b",
            &token,
            "social.draft.create",
            "op-b3",
            json!({"post":"newest"}),
        )
        .unwrap();
    ran_without_card(&ran, "funded granted draft");
    assert_eq!(fx.door.text_calls.load(SeqCst), 1);
    assert_eq!(receipts(&fx), before + 1);
}

/// (c) An agent caller cannot decide "Always allow", cannot create a grant
/// by any operator verb, and cannot revoke one.
#[test]
fn c_an_agent_cannot_decide_always_create_a_grant_or_revoke() {
    let fx = Fx::new();
    let token = fx.create_turn("m-c");
    let card = pending(
        &fx.invoke("m-c", &token, "social.posts.fetch", "op-c1", json!({}))
            .unwrap(),
    );
    refused(
        fx.decide(Asserted::Agent("master".into()), &card, "allow_always"),
        "not app_assistant_decision",
        "the requesting agent's Always allow",
    );
    refused(
        fx.decide(Asserted::Agent("w1".into()), &card, "allow_always"),
        "operator action",
        "another agent's Always allow",
    );
    refused(
        fx.decide(Asserted::Unproven, &card, "allow_always"),
        "operator",
        "an unproven Always allow",
    );
    for action in ["social.posts.fetch", "social.draft.create"] {
        refused(
            fx.call(
                Asserted::Agent("w1".into()),
                "app_assistant_permission_block",
                json!({"install_id":fx.install,"context_id":fx.context,"action_id":action,"resource_id":"any"}),
            ),
            "operator action",
            "an agent standing-rule write",
        );
    }
    assert!(grants(&fx).is_empty(), "an agent created a grant");
    assert_eq!(fx.door.spent(), (0, 0, 0), "a refused decision spent");
    let shown = fx.show("op-c1");
    assert_eq!(shown["status"], "pending_permission", "{shown}");
    assert_eq!(shown["revision"], card["revision"], "{shown}");
    // The next agent call still parks a card: no grant exists.
    pending(
        &fx.invoke("m-c", &token, "social.posts.fetch", "op-c2", json!({}))
            .unwrap(),
    );
    assert_eq!(fx.door.spent(), (0, 0, 0));

    // The operator grants (positive control); the agent cannot revoke it.
    fx.decide(Asserted::Operator, &card, "allow_always")
        .unwrap();
    let grant = active_grant(&fx, "social.posts.fetch").expect("operator grant");
    let revoke = json!({"install_id":fx.install,"context_id":fx.context,
        "permission_id":grant["id"],"expected_revision":grant["revision"]});
    refused(
        fx.call(
            Asserted::Agent("master".into()),
            "app_assistant_permission_revoke",
            revoke.clone(),
        ),
        "not app_assistant_permission_revoke",
        "the agent's revoke",
    );
    refused(
        fx.call(
            Asserted::Agent("w1".into()),
            "app_assistant_permission_revoke",
            revoke,
        ),
        "operator action",
        "another agent's revoke",
    );
    assert!(
        active_grant(&fx, "social.posts.fetch").is_some(),
        "an agent revoked the operator's grant"
    );
}

/// (d) After the operator revokes the standing grant, the next agent call
/// parks a card again and does not run.
#[test]
fn d_after_an_operator_revoke_the_next_call_parks_and_does_not_run() {
    let fx = Fx::new();
    let token = fx.create_turn("m-d");
    let card = pending(
        &fx.invoke("m-d", &token, "social.posts.fetch", "op-d1", json!({}))
            .unwrap(),
    );
    fx.decide(Asserted::Operator, &card, "allow_always")
        .unwrap();
    let ran = fx
        .invoke("m-d", &token, "social.posts.fetch", "op-d2", json!({}))
        .unwrap();
    ran_without_card(&ran, "granted fetch before revoke");
    assert_eq!(fx.door.reads.load(SeqCst), 2);

    let grant = active_grant(&fx, "social.posts.fetch").expect("grant");
    let revoked = fx.operator(
        "app_assistant_permission_revoke",
        json!({"install_id":fx.install,"context_id":fx.context,
            "permission_id":grant["id"],"expected_revision":grant["revision"]}),
    );
    assert_eq!(revoked["permission"]["state"], "revoked", "{revoked}");

    let parked = pending(
        &fx.invoke("m-d", &token, "social.posts.fetch", "op-d3", json!({}))
            .unwrap(),
    );
    assert_eq!(parked["status"], "pending_permission");
    assert_eq!(
        fx.door.reads.load(SeqCst),
        2,
        "a call after revoke reached the provider"
    );
    assert_eq!(ceilings(&fx, "read_instagram_posts").len(), 2);
    // A replay of the pre-revoke operation returns its outcome, no new call.
    let replay = fx
        .invoke("m-d", &token, "social.posts.fetch", "op-d2", json!({}))
        .unwrap();
    assert_eq!(replay["operation"]["status"], "succeeded", "{replay}");
    assert_eq!(fx.door.reads.load(SeqCst), 2, "a replay charged again");
}

/// (e) Designer image refine still refuses "Always allow" (the deliberate
/// CAD-1868 scope limit), and no grant is created.
#[test]
fn e_media_refine_still_refuses_allow_always() {
    const REFINE: &str = r#"{ "id": "media.refine.prepare", "description": "Refine a library image.", "input_schema": { "type": "object", "properties": { "asset_id": { "type": "string", "minLength": 1, "maxLength": 64 }, "revision": { "type": "integer", "minimum": 1 }, "sha256": { "type": "string", "minLength": 71, "maxLength": 71 }, "instructions": { "type": "string", "minLength": 1, "maxLength": 2000 } }, "required": ["asset_id", "revision", "sha256", "instructions"], "additionalProperties": false }, "effect": "draft", "confirmation": "permission_required", "availability": "requires_context" }"#;
    let fx = Fx::with_social(Some(&descriptor(
        Some("social-content"),
        &[FETCH, DRAFT, STATUS, REFINE],
    )));
    let digest = fx.operator(
        "app_assistant_actions_operator",
        json!({"install_id":fx.install,"context_id":fx.context}),
    )["descriptor_digest"]
        .as_str()
        .unwrap()
        .to_string();
    let meta = crate::app_assistant::registered_action("media.refine.prepare").unwrap();
    let input = json!({"asset_id":"asset-1","revision":1,
        "sha256":format!("sha256:{}", "a".repeat(64)),"instructions":"Brighten it"});
    let operation_digest = crate::store::app_runs::material_digest(
        &json!({"contract":crate::issue::app_assistant::CONTRACT,"action":"media.refine.prepare","schema":meta.input_schema,"handler_version":meta.handler_version,"descriptor_digest":digest,"input":input}),
    );
    let records = fx.records();
    records
        .app_assistant_operation_create(
            "op-e1",
            &fx.context,
            "media.refine.prepare",
            &operation_digest,
            &input,
        )
        .unwrap();
    let parked = records
        .app_assistant_operation_set(crate::store::app_records::AssistantOperationUpdate {
            operation_id: "op-e1",
            context: &fx.context,
            expected_revision: 1,
            status: "pending_permission",
            summary: "Review this reference edit before it costs anything",
            result: &Value::Null,
            resource_refs: &json!([]),
            permission_request: &json!({"reason":"refine","scope":{"install_id":fx.install,
                "context_id":fx.context,"action_id":"media.refine.prepare","resource_id":"asset-1"},
                "allow_always":false,"preview":{}}),
            error: None,
        })
        .unwrap();
    refused(
        fx.decide(Asserted::Operator, &parked, "allow_always"),
        "allowed one time only",
        "an operator Always allow on a reference edit",
    );
    assert!(grants(&fx).is_empty(), "a refine grant was created");
    let shown = fx.show("op-e1");
    assert_eq!(shown["status"], "pending_permission", "{shown}");
    assert_eq!(fx.door.spent().2, 0);
    // Control: allow_once on the same operation passes that gate (it then
    // stops later, on the missing library image) — so the refusal above is
    // the allow-once gate, not a fixture artefact.
    let once = fx.decide(Asserted::Operator, &parked, "allow_once");
    if let Err(error) = &once {
        assert!(
            !error.to_string().contains("one time only"),
            "allow_once hit the allow-once gate: {error}"
        );
    }
    assert!(grants(&fx).is_empty());
    assert_eq!(fx.door.spent().2, 0);
}
