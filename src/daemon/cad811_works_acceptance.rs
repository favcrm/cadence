// CAD-1662 Works acceptance. Included as a CHILD of the unchanged
// cad811_screen_services_acceptance module (sibling of the owner child).
// Every case reaches the real screen relay or the real native RPC through
// the supported Native fixture; no fake dispatcher or permission store.
// Verified humans come from Native::owner(); the local operator uses
// Native::local(). Works reads keep CAD-1649's operator-only admission.

use super::independent_public_owner_attribution::Native;
use super::{call, Session};
use crate::operator_auth::{BoardUser, PublicRealm};
use crate::test_seam::Asserted;
use serde_json::{json, Value};

const WORKS_OPS: &[&str] = &[
    "works.query",
    "works.get",
    "works.create",
    "works.patch",
    "works.trash",
    "works.restore",
    "works.request_review",
    "works.review",
    "works.export",
];

// Store-shaped content per kind (deck needs slides[{title,body}],
// sheet needs columns + rows matching their width).
fn content_for(kind: &str) -> Value {
    match kind {
        "deck" => json!({"slides": [{"title": "s", "body": "b"}]}),
        "sheet" => json!({"columns": ["a"], "rows": [["1"]]}),
        _ => json!({"blocks": [{"type": "p", "text": "x"}]}),
    }
}

// A live public session for a verified board human, minted through the same
// open_public_bound constructor the owner fixture uses. owner() mints one
// fixed JTI, so a second session or a second human needs a distinct JTI.
// Test-only: no production path or gate is changed.
fn public_session(native: &Native, sub: &str, jti: &str) -> Session {
    let user = BoardUser {
        sub: sub.into(),
        email: format!("{sub}@example.test"),
        name: format!("Synthetic {sub}"),
        role: "operator".into(),
        handle: sub.into(),
    };
    let now = native.shared.operator_now();
    let opened = native
        .shared
        .operator_auth()
        .open_public_bound(
            user,
            jti,
            now + 60,
            "c811-isolated-acceptance",
            PublicRealm {
                issuer: native.issuer.clone(),
                company: "co_c811_owner".into(),
                host: native.fx.host.clone(),
            },
            now,
        )
        .expect("public session must open")
        .expect("fresh JTI yields a live session");
    Session {
        cookie: format!("__Host-aos-board-session={}", opened.token),
        token: opened.token,
        key: String::new(),
    }
}

// A typed refusal: a rejected/gate/conflict/invalid kind, never success.
// Used only where the exact kind is not the point (undeclared/wrong channel).
fn refused(result: crate::Result<Value>, case: &str) {
    match result {
        Ok(value) => panic!("{case}: daemon accepted a prohibited Works request: {value}"),
        Err(error) => assert!(
            matches!(
                error.kind(),
                "rejected" | "gate" | "conflict" | "invalid" | "forbidden"
            ),
            "{case}: refusal must be typed, got {}",
            error.kind()
        ),
    }
}

// Pins a refusal to an exact wire kind and store code, never a loose set.
fn refused_exact(result: crate::Result<Value>, case: &str, kind: &str, code: Option<&str>) {
    match result {
        Ok(value) => panic!("{case}: daemon accepted a prohibited Works request: {value}"),
        Err(error) => {
            assert_eq!(error.kind(), kind, "{case}: wrong refusal kind");
            if let Some(code) = code {
                assert_eq!(error.code(), Some(code), "{case}: wrong refusal code");
            }
        }
    }
}

// Drives a SLOT operation through the real protected flow: slot-prepare
// proves the request and fixes the facts; the host confirmation then runs
// slot-confirm, which re-proves the facts and calls the store. A refusal is
// asserted from the confirm step, so the store gate (not the screen gate)
// is what is observed. Works-local; the sibling slot() is unchanged.
/// One SLOT request: the operation, its screen arguments, and the host choice.
struct SlotRequest<'a> {
    operation: &'a str,
    args: Value,
    choice: &'a str,
}

fn slot_refusal(
    native: &Native,
    session: &Session,
    action: &str,
    request: SlotRequest<'_>,
    case: &str,
    code: &str,
) {
    let SlotRequest {
        operation,
        args,
        choice,
    } = request;
    // slot-prepare is its own RPC; the CALL endpoint refuses SLOT ops.
    let prepared = native
        .rpc(
            "app_screen_service_slot_prepare",
            native.credentials(session, "public", call(action, operation, args)),
        )
        .unwrap_or_else(|error| {
            panic!(
                "{case}: slot-prepare refused before the store: kind={} code={:?} msg={}",
                error.kind(),
                error.code(),
                error
            )
        });
    let confirm = native.rpc(
        "app_screen_service_slot_confirm",
        native.credentials(
            session,
            "public",
            json!({"action_token": action,
                "confirmation_token": prepared["confirmation_token"],
                "confirmation": {"confirmed": true, "choice": choice}}),
        ),
    );
    match confirm {
        Ok(value) => panic!("{case}: store accepted a prohibited write: {value}"),
        Err(error) => {
            assert_eq!(error.kind(), "rejected", "{case}: wrong refusal kind");
            assert_eq!(error.code(), Some(code), "{case}: wrong store refusal code");
        }
    }
}

#[test]
fn cad811_works_declared_ops_flow_through_relay_for_verified_owner() {
    let native = Native::new();
    let scope = native.fx.install("works-flow", WORKS_OPS);
    let (owner, _) = native.owner();
    let action = native.mount(&owner, "public", &scope, 1);
    for kind in ["doc", "deck", "sheet"] {
        let created = native.service(
            &owner,
            "public",
            &action,
            "works.create",
            json!({"kind": kind, "title": format!("{kind} one"), "content": content_for(kind),
                "tags": ["t"], "idempotency_key": format!("create-{kind}")}),
            false,
        );
        assert_eq!(
            created["revision"], 1,
            "{kind}: create starts at revision 1"
        );
    }
    let listed = native.service(&owner, "public", &action, "works.query", json!({}), false);
    assert!(listed.is_array(), "works.query returns a list");
}

#[test]
fn cad811_works_undeclared_and_wrong_channel_refuse() {
    let native = Native::new();
    // Declared set omits every works.* operation.
    let scope = native.fx.install("works-undeclared", &["records.list"]);
    let (owner, _) = native.owner();
    let action = native.mount(&owner, "public", &scope, 1);
    refused(
        native.fx.call(
            Asserted::Operator,
            &owner,
            call(
                &action,
                "works.create",
                json!({"kind": "doc", "title": "x",
                "content": {}, "idempotency_key": "k"}),
            ),
        ),
        "undeclared works.create",
    );
}

#[test]
fn cad811_works_read_only_member_refused_operator_reads() {
    let native = Native::new();
    let scope = native.fx.install("works-readonly", WORKS_OPS);
    let (owner, _) = native.owner();
    let action = native.mount(&owner, "public", &scope, 1);
    // Negative guard: read-only/member context refuses works.query/get.
    let local = native.local();
    refused(
        native.fx.call(
            Asserted::Operator,
            &local,
            call(&action, "works.query", json!({})),
        ),
        "read-only works.query",
    );
    refused(
        native.fx.call(
            Asserted::Operator,
            &local,
            call(&action, "works.get", json!({"id": "doc-x"})),
        ),
        "read-only works.get",
    );
    let ok = native.service(&owner, "public", &action, "works.query", json!({}), false);
    assert!(ok.is_array(), "operator screen works.query succeeds");
}

#[test]
fn cad811_works_same_human_second_session_review_refuses() {
    let native = Native::new();
    let scope = native.fx.install("works-self", WORKS_OPS);
    let (owner, _) = native.owner();
    let action = native.mount(&owner, "public", &scope, 1);
    let created = native.service(
        &owner,
        "public",
        &action,
        "works.create",
        json!({"kind": "doc", "title": "self", "content": {"blocks": [{"type": "p", "text": "x"}]},
            "idempotency_key": "self-create"}),
        false,
    );
    let id = created["id"].as_str().expect("created id").to_owned();
    let revision = created["revision"].clone();
    native.service(
        &owner,
        "public",
        &action,
        "works.request_review",
        json!({"id": id, "revision": revision}),
        false,
    );
    // Same verified human, second session: self-review must refuse at the
    // store, reached through the real SLOT prepare and confirm.
    let second = public_session(
        &native,
        "synthetic-c811-owner",
        "c811-works-second-session-jti",
    );
    // A mount is bound to the session that minted it: the second session mints
    // its own action for the same scope, so the refusal is the store's
    // self-review gate, not an unavailable-mount authority refusal.
    let second_action = native.mount(&second, "public", &scope, 2);
    slot_refusal(
        &native,
        &second,
        &second_action,
        SlotRequest {
            operation: "works.review",
            args: json!({"id": id, "revision": revision, "decision": "approve"}),
            choice: "approve",
        },
        "same-human second-session review",
        "forbidden",
    );
}

#[test]
fn cad811_works_unverified_review_refuses_without_writes() {
    let native = Native::new();
    let scope = native.fx.install("works-unverified", WORKS_OPS);
    let (owner, _) = native.owner();
    let action = native.mount(&owner, "public", &scope, 1);
    let created = native.service(
        &owner,
        "public",
        &action,
        "works.create",
        json!({"kind": "doc", "title": "unv", "content": {"blocks": [{"type": "p", "text": "x"}]},
            "idempotency_key": "unv-create"}),
        false,
    );
    let id = created["id"].as_str().expect("created id").to_owned();
    let before = native.durable_snapshot(&scope);
    let local = native.local();
    refused(
        native.fx.call(
            Asserted::Operator,
            &local,
            call(
                &action,
                "works.review",
                json!({"id": id,
                "revision": created["revision"], "decision": "approve"}),
            ),
        ),
        "unverified local review",
    );
    assert_eq!(
        native.durable_snapshot(&scope),
        before,
        "refused review must not write"
    );
}

#[test]
fn cad811_works_export_before_approval_refuses() {
    let native = Native::new();
    let scope = native.fx.install("works-export", WORKS_OPS);
    let (owner, _) = native.owner();
    let action = native.mount(&owner, "public", &scope, 1);
    let created = native.service(
        &owner,
        "public",
        &action,
        "works.create",
        json!({"kind": "doc", "title": "exp", "content": {"blocks": [{"type": "p", "text": "x"}]},
            "idempotency_key": "exp-create"}),
        false,
    );
    let id = created["id"].as_str().expect("created id").to_owned();
    slot_refusal(
        &native,
        &owner,
        &action,
        SlotRequest {
            operation: "works.export",
            args: json!({"id": id, "revision": created["revision"]}),
            choice: "html",
        },
        "export before approval",
        "needs_review",
    );
}

#[test]
fn cad811_works_stale_cas_and_replay_are_idempotent() {
    let native = Native::new();
    let scope = native.fx.install("works-cas", WORKS_OPS);
    let (owner, _) = native.owner();
    let action = native.mount(&owner, "public", &scope, 1);
    let created = native.service(
        &owner,
        "public",
        &action,
        "works.create",
        json!({"kind": "doc", "title": "cas", "content": {"blocks": [{"type": "p", "text": "x"}]},
            "idempotency_key": "cas-create"}),
        false,
    );
    let id = created["id"].as_str().expect("created id").to_owned();
    let patch = json!({"id": id, "if_revision": created["revision"],
        "operations": [{"type": "set_title", "title": "cas-two"}],
        "idempotency_key": "cas-patch"});
    let patched = native.service(
        &owner,
        "public",
        &action,
        "works.patch",
        patch.clone(),
        false,
    );
    assert_eq!(patched["revision"], 2, "CAS patch advances the revision");
    // Stale CAS: the original revision must change nothing.
    // Same authorized CALL path as native.service (credentials attached), so
    // the refusal is the store's CAS gate, not the screen authority gate.
    refused_exact(
        native.rpc(
            "app_screen_service_call",
            native.credentials(
                &owner,
                "public",
                call(
                    &action,
                    "works.patch",
                    json!({"id": id, "if_revision": 1,
                    "operations": [{"type": "set_title", "title": "stale"}],
                    "idempotency_key": "cas-stale"}),
                ),
            ),
        ),
        "stale CAS patch",
        "conflict",
        Some("conflict"),
    );
    // Replay of the original key returns the original result, no second write.
    let replay = native.service(&owner, "public", &action, "works.patch", patch, false);
    assert_eq!(
        replay, patched,
        "idempotent replay returns the original result"
    );
}

#[test]
fn cad811_works_cross_install_denies_without_disclosure() {
    let native = Native::new();
    let a = native.fx.install("works-xa", WORKS_OPS);
    let b = native.fx.install("works-xb", WORKS_OPS);
    let (owner, _) = native.owner();
    let action_a = native.mount(&owner, "public", &a, 1);
    let action_b = native.mount(&owner, "public", &b, 2);
    let created = native.service(
        &owner,
        "public",
        &action_a,
        "works.create",
        json!({"kind": "doc", "title": "private-a", "content": {"blocks": [{"type": "p", "text": "secret"}]},
            "idempotency_key": "xa-create"}),
        false,
    );
    let id = created["id"].as_str().expect("created id").to_owned();
    refused(
        native.fx.call(
            Asserted::Operator,
            &owner,
            call(&action_b, "works.get", json!({"id": id})),
        ),
        "cross-install works.get",
    );
}

#[test]
fn cad811_works_near_limit_bridge_payload_and_oversize_refusal() {
    let native = Native::new();
    let scope = native.fx.install("works-limit", WORKS_OPS);
    let (owner, _) = native.owner();
    let action = native.mount(&owner, "public", &scope, 1);
    // Near 64 KiB content must cross the real bridge.
    let near = native.service(
        &owner,
        "public",
        &action,
        "works.create",
        json!({"kind": "doc", "title": "near", "content": {"blocks": [{"type": "p", "text": "a".repeat(60 * 1024)}]},
            "idempotency_key": "near-create"}),
        false,
    );
    assert_eq!(
        near["revision"], 1,
        "near-limit valid content crosses the bridge"
    );
    // Beyond the 80 KiB envelope refuses without writes.
    let before = native.durable_snapshot(&scope);
    refused(
        native.fx.call(
            Asserted::Operator,
            &owner,
            call(&action, "works.create", json!({"kind": "doc", "title": "big",
                "content": {"blocks": [{"type": "p", "text": "b".repeat(90 * 1024)}]}, "idempotency_key": "big-create"})),
        ),
        "oversized works.create",
    );
    assert_eq!(
        native.durable_snapshot(&scope),
        before,
        "oversized refusal must not write"
    );
}

// CAD-1662 ticket-authored independent guard (manager, not the implementer):
// a child must not be able to smuggle identity or scope through Works args.
// Every forged field must refuse before a write, and the context must stay empty.
#[test]
fn cad1662_manager_guard_forged_scope_and_identity_fields_refuse_without_write() {
    let native = Native::new();
    let scope = native.fx.install("works-guard", WORKS_OPS);
    let (owner, _) = native.owner();
    let action = native.mount(&owner, "public", &scope, 1);
    for field in [
        "actor",
        "install_id",
        "context_id",
        "on_behalf_of",
        "public_session",
        "role",
    ] {
        let mut args = json!({
            "kind": "doc",
            "title": "guard",
            "content": content_for("doc"),
            "idempotency_key": "guard"
        });
        args[field] = json!("forged");
        refused_exact(
            native.fx.call(
                Asserted::Operator,
                &owner,
                call(&action, "works.create", args),
            ),
            field,
            "rejected",
            None,
        );
    }
    let listed = native.service(&owner, "public", &action, "works.query", json!({}), false);
    assert_eq!(
        listed.as_array().map(|rows| rows.len()),
        Some(0),
        "forged identity/scope must create nothing"
    );
}

// CAD-1662 two-operator success: a verified author creates and requests
// review; a distinct verified reviewer approves through the real SLOT
// prepare/confirm path; the author then exports the approved current revision
// and receives the receipt. Both humans are admitted by one fixture.
#[test]
fn cad811_works_two_verified_operators_approve_and_export() {
    let native = Native::new_with_subs(&["synthetic-c811-owner", "synthetic-c811-reviewer"]);
    let scope = native.fx.install("works-two-op", WORKS_OPS);
    let (author, _) = native.owner();
    let author_action = native.mount(&author, "public", &scope, 1);
    let reviewer = public_session(
        &native,
        "synthetic-c811-reviewer",
        "c811-works-reviewer-jti",
    );
    let reviewer_action = native.mount(&reviewer, "public", &scope, 2);

    let created = native.service(
        &author,
        "public",
        &author_action,
        "works.create",
        json!({"kind": "doc", "title": "two-op", "content": content_for("doc"),
            "idempotency_key": "two-op-create"}),
        false,
    );
    let id = created["id"].as_str().expect("created id").to_owned();
    let revision = created["revision"].clone();
    native.service(
        &author,
        "public",
        &author_action,
        "works.request_review",
        json!({"id": id, "revision": revision}),
        false,
    );

    let approved = native.slot(
        &reviewer,
        "public",
        &reviewer_action,
        "works.review",
        (
            json!({"id": id, "revision": revision, "decision": "approve"}),
            json!({"confirmed": true, "choice": "approve"}),
        ),
        false,
    );
    assert!(
        approved.is_object(),
        "distinct verified reviewer approves: {approved}"
    );

    let receipt = native.slot(
        &author,
        "public",
        &author_action,
        "works.export",
        (
            json!({"id": id, "revision": revision, "format": "html"}),
            json!({"confirmed": true, "choice": "export"}),
        ),
        false,
    );
    assert!(
        receipt.is_object(),
        "author exports the approved current revision: {receipt}"
    );
}

// Prepares a SLOT confirmation over the real prepare RPC and returns the
// token; the caller decides how (and whether) to confirm it.
fn prepare_slot(
    native: &Native,
    session: &Session,
    action: &str,
    operation: &str,
    args: Value,
) -> String {
    let prepared = native.rpc(
        "app_screen_service_slot_prepare",
        native.credentials(session, "public", call(action, operation, args)),
    );
    prepared.expect("slot prepare must succeed for a declared, authorized request")
        ["confirmation_token"]
        .as_str()
        .expect("confirmation token")
        .to_owned()
}

fn confirm_slot(
    native: &Native,
    session: &Session,
    action: &str,
    token: &str,
    choice: &str,
) -> crate::Result<Value> {
    native.rpc(
        "app_screen_service_slot_confirm",
        native.credentials(
            session,
            "public",
            json!({"action_token": action,
                "confirmation_token": token,
                "confirmation": {"confirmed": true, "choice": choice}}),
        ),
    )
}

// A confirmation is single-use: the first confirm consumes it; a replay with
// the same token must refuse as spent before any mutation.
#[test]
fn cad811_works_confirmation_reuse_refuses() {
    let native = Native::new_with_subs(&["synthetic-c811-owner", "synthetic-c811-reviewer"]);
    let scope = native.fx.install("works-reuse", WORKS_OPS);
    let (author, _) = native.owner();
    let author_action = native.mount(&author, "public", &scope, 1);
    let reviewer = public_session(&native, "synthetic-c811-reviewer", "c811-works-reuse-jti");
    let reviewer_action = native.mount(&reviewer, "public", &scope, 2);
    let created = native.service(
        &author,
        "public",
        &author_action,
        "works.create",
        json!({"kind": "doc", "title": "reuse", "content": content_for("doc"),
            "idempotency_key": "reuse-create"}),
        false,
    );
    let id = created["id"].as_str().expect("created id").to_owned();
    let revision = created["revision"].clone();
    native.service(
        &author,
        "public",
        &author_action,
        "works.request_review",
        json!({"id": id, "revision": revision}),
        false,
    );
    let token = prepare_slot(
        &native,
        &reviewer,
        &reviewer_action,
        "works.review",
        json!({"id": id, "revision": revision, "decision": "approve"}),
    );
    confirm_slot(&native, &reviewer, &reviewer_action, &token, "approve")
        .expect("first confirm succeeds");
    let replay = confirm_slot(&native, &reviewer, &reviewer_action, &token, "approve");
    let error = replay.expect_err("replayed confirmation must refuse");
    assert_eq!(error.kind(), "rejected", "replay: wrong refusal kind");
}

// Export before approval refuses at the store gate, through the real SLOT flow.
#[test]
fn cad811_works_export_before_approval_refuses_through_slot() {
    let native = Native::new_with_subs(&["synthetic-c811-owner", "synthetic-c811-reviewer"]);
    let scope = native.fx.install("works-export-gate", WORKS_OPS);
    let (author, _) = native.owner();
    let author_action = native.mount(&author, "public", &scope, 1);
    let created = native.service(
        &author,
        "public",
        &author_action,
        "works.create",
        json!({"kind": "doc", "title": "gate", "content": content_for("doc"),
            "idempotency_key": "gate-create"}),
        false,
    );
    let id = created["id"].as_str().expect("created id").to_owned();
    let revision = created["revision"].clone();
    let token = prepare_slot(
        &native,
        &author,
        &author_action,
        "works.export",
        json!({"id": id, "revision": revision, "format": "html"}),
    );
    let error = confirm_slot(&native, &author, &author_action, &token, "export")
        .expect_err("export before approval must refuse");
    assert_eq!(error.kind(), "rejected", "export gate: wrong refusal kind");
}
