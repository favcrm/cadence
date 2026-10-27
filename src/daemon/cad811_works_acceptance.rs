// CAD-1662 Works acceptance. Included as a CHILD of the unchanged
// cad811_screen_services_acceptance module (sibling of the owner child).
// Every case reaches the real screen relay or the real native RPC through
// the supported Native fixture; no fake dispatcher or permission store.
// Verified humans come from Native::owner(); the local operator uses
// Native::local(). Works reads keep CAD-1649's operator-only admission.

use super::independent_public_owner_attribution::Native;
use super::{call, Session};
use crate::board_identity::Role;
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
fn public_session(native: &Native, sub: &str, jti: &str, role: &str) -> Session {
    let user = BoardUser {
        sub: sub.into(),
        email: format!("{sub}@example.test"),
        name: format!("Synthetic {sub}"),
        role: role.into(),
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

// Works-local durable snapshot: the four CAD-1649 Works tables only, read
// from the install's record DB opened by its public path. Works-only
// installs have no CRM tables, so durable_snapshot cannot be used here.
fn works_snapshot(native: &Native, install: &str) -> Vec<String> {
    let path = crate::store::app_records::record_db_path(native.fx.root.path(), install)
        .expect("installation record path");
    // No Works write has happened in a fresh install, so the record DB may
    // not exist yet. A missing DB is an empty Works snapshot, not a failure.
    if !path.exists() {
        return Vec::new();
    }
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("read-only works database");
    let mut rows = Vec::new();
    for table in [
        "app_works_items",
        "app_works_revisions",
        "app_works_idempotency",
        "app_works_audit",
    ] {
        let mut stmt = db
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .expect("read-only works snapshot");
        let columns = stmt.column_count();
        let table_rows = stmt
            .query_map([], |row| {
                (0..columns)
                    .map(|column| {
                        use rusqlite::types::ValueRef;
                        Ok(match row.get_ref(column)? {
                            ValueRef::Null => "null".into(),
                            ValueRef::Integer(value) => format!("i:{value}"),
                            ValueRef::Real(value) => format!("r:{value}"),
                            ValueRef::Text(value) => {
                                format!("t:{}", String::from_utf8_lossy(value))
                            }
                            ValueRef::Blob(_) => "blob".into(),
                        })
                    })
                    .collect::<rusqlite::Result<Vec<String>>>()
            })
            .expect("works snapshot query")
            .collect::<rusqlite::Result<Vec<Vec<String>>>>()
            .expect("works snapshot rows");
        for row in table_rows {
            rows.push(format!("{table}|{}", row.join("|")));
        }
    }
    rows
}

// Every Works request in this module crosses the real public-origin relay,
// never the loopback-only Fixture::call helper.
fn public_call(
    native: &Native,
    session: &Session,
    action: &str,
    operation: &str,
    args: Value,
) -> crate::Result<Value> {
    native.rpc(
        "app_screen_service_call",
        native.credentials(session, "public", call(action, operation, args)),
    )
}

#[test]
fn cad811_works_read_only_member_refused_operator_reads() {
    let native = Native::new_with_roles(&[
        ("synthetic-c811-owner", Role::Operator),
        ("c811-member", Role::Member),
    ]);
    let ops = [WORKS_OPS, &["records.list"]].concat();
    let scope = native.fx.install("works-readonly", &ops);
    let (owner, _) = native.owner();
    let member = public_session(&native, "c811-member", "c811-member-jti", "member");
    let member_action = native.mount(&member, "public", &scope, 1);
    let records = native.service(
        &member,
        "public",
        &member_action,
        "records.list",
        json!({}),
        false,
    );
    assert!(records.is_object(), "member read-only mount is valid");
    let before = works_snapshot(&native, &scope.install);
    refused(
        public_call(&native, &member, &member_action, "works.query", json!({})),
        "member works.query",
    );
    refused(
        public_call(
            &native,
            &member,
            &member_action,
            "works.get",
            json!({"id": "doc-x"}),
        ),
        "member works.get",
    );
    assert_eq!(
        works_snapshot(&native, &scope.install),
        before,
        "member refusal must not write"
    );
    let owner_action = native.mount(&owner, "public", &scope, 2);
    let ok = native.service(
        &owner,
        "public",
        &owner_action,
        "works.query",
        json!({}),
        false,
    );
    assert!(ok.is_array(), "operator public works.query succeeds");
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
        "operator",
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
    let before = works_snapshot(&native, &scope.install);
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
        works_snapshot(&native, &scope.install),
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
    // Success control at the same boundary: the owning install reads it.
    let own = native.service(
        &owner,
        "public",
        &action_a,
        "works.get",
        json!({"id": id}),
        false,
    );
    assert_eq!(
        own["id"],
        json!(id),
        "control: owning install reads the doc"
    );
    // Cross-install denial through the public relay, not loopback.
    let before_b = works_snapshot(&native, &b.install);
    refused(
        public_call(&native, &owner, &action_b, "works.get", json!({"id": id})),
        "cross-install works.get",
    );
    assert_eq!(
        works_snapshot(&native, &b.install),
        before_b,
        "cross-install denial must not write the other install"
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
    let before = works_snapshot(&native, &scope.install);
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
        works_snapshot(&native, &scope.install),
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
    let before = works_snapshot(&native, &scope.install);
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
        // Public-origin relay, same boundary as the success control below.
        refused_exact(
            public_call(&native, &owner, &action, "works.create", args),
            field,
            "rejected",
            None,
        );
    }
    assert_eq!(
        works_snapshot(&native, &scope.install),
        before,
        "forged identity/scope must write nothing"
    );
    // Success control at the same boundary: a clean create is accepted, so
    // the refusals above are the forgery, not the route.
    let ok = native.service(
        &owner,
        "public",
        &action,
        "works.create",
        json!({"kind": "doc", "title": "guard-ok", "content": content_for("doc"), "idempotency_key": "guard-ok"}),
        false,
    );
    assert!(ok["id"].is_string(), "control: a clean create succeeds");
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
        "operator",
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
            json!({"confirmed": true, "choice": "html"}),
        ),
        false,
    );
    assert!(
        receipt.is_object(),
        "author exports the approved current revision: {receipt}"
    );
    assert_eq!(receipt["id"], id);
    assert_eq!(receipt["revision"], revision);
    assert_eq!(receipt["format"], "html");
    assert_eq!(receipt["digest"], created["contentDigest"]);
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
    let reviewer = public_session(
        &native,
        "synthetic-c811-reviewer",
        "c811-works-reuse-jti",
        "operator",
    );
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
    let before = works_snapshot(&native, &scope.install);
    let error = confirm_slot(&native, &author, &author_action, &token, "export")
        .expect_err("export before approval must refuse");
    // Store refusals are `Error::invalid(code, ..)`: wire kind "rejected" with
    // the store's own code. The code must survive the SLOT boundary intact.
    assert_eq!(error.kind(), "rejected", "export gate: wire kind");
    assert_eq!(
        error.code(),
        Some("needs_review"),
        "export gate: store code needs_review must survive prepare/confirm"
    );
    assert_eq!(
        works_snapshot(&native, &scope.install),
        before,
        "refused export must write nothing"
    );
}

// Changed facts: a revision moved after prepare must refuse the confirm,
// and the approval must not land on the new revision.
#[test]
fn cad1662_changed_facts_after_prepare_refuse_through_slot_confirm() {
    let native = Native::new_with_subs(&["synthetic-c811-owner", "synthetic-c811-reviewer"]);
    let scope = native.fx.install("works-facts", WORKS_OPS);
    let (author, _) = native.owner();
    let author_action = native.mount(&author, "public", &scope, 1);
    let reviewer = public_session(
        &native,
        "synthetic-c811-reviewer",
        "c811-works-facts-jti",
        "operator",
    );
    let reviewer_action = native.mount(&reviewer, "public", &scope, 2);
    let created = native.service(
        &author,
        "public",
        &author_action,
        "works.create",
        json!({"kind": "doc", "title": "facts", "content": content_for("doc"),
            "idempotency_key": "facts-create"}),
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
    // The author edits after the reviewer's card was drawn: revision moves.
    native.service(
        &author,
        "public",
        &author_action,
        "works.patch",
        json!({"id": id, "if_revision": revision,
            "operations": [{"type": "set_title", "title": "facts v2"}],
            "idempotency_key": "facts-patch"}),
        false,
    );
    let error = confirm_slot(&native, &reviewer, &reviewer_action, &token, "approve")
        .expect_err("changed facts must refuse the confirm");
    assert_eq!(
        error.kind(),
        "conflict",
        "changed facts: wrong refusal kind"
    );
    assert_eq!(
        error.code(),
        Some("facts_changed"),
        "changed facts: wrong refusal code"
    );
    let shown = native.service(
        &author,
        "public",
        &author_action,
        "works.get",
        json!({"id": id}),
        false,
    );
    assert_eq!(
        shown["reviewState"], "draft",
        "approval must not land on the moved revision"
    );
}

// Expiry: a confirmation older than CONFIRM_TTL refuses at confirm, through
// the test-only clock seam; no production clock or visibility changes.
#[test]
fn cad1662_expired_confirmation_refuses_through_slot_confirm() {
    let native = Native::new_with_subs(&["synthetic-c811-owner", "synthetic-c811-reviewer"]);
    let scope = native.fx.install("works-expiry", WORKS_OPS);
    let (author, _) = native.owner();
    let author_action = native.mount(&author, "public", &scope, 1);
    let reviewer = public_session(
        &native,
        "synthetic-c811-reviewer",
        "c811-works-expiry-jti",
        "operator",
    );
    let reviewer_action = native.mount(&reviewer, "public", &scope, 2);
    let created = native.service(
        &author,
        "public",
        &author_action,
        "works.create",
        json!({"kind": "doc", "title": "expiry", "content": content_for("doc"),
            "idempotency_key": "expiry-create"}),
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
    native.shared.age_screen_confirmations_for_test();
    let error = confirm_slot(&native, &reviewer, &reviewer_action, &token, "approve")
        .expect_err("expired confirmation must refuse");
    assert_eq!(error.kind(), "rejected", "expiry: wrong refusal kind");
    let shown = native.service(
        &author,
        "public",
        &author_action,
        "works.get",
        json!({"id": id}),
        false,
    );
    assert_eq!(
        shown["reviewState"], "draft",
        "expired approval must not land"
    );
}

// Revocation: once the reviewer's membership is withdrawn, the old mount
// and approval authority must refuse; the doc stays unapproved.
#[test]
fn cad1662_retired_reviewer_authority_refuses_after_membership_withdrawal() {
    let native = Native::new_with_subs(&["synthetic-c811-owner", "synthetic-c811-reviewer"]);
    let scope = native.fx.install("works-revoke", WORKS_OPS);
    let (author, _) = native.owner();
    let author_action = native.mount(&author, "public", &scope, 1);
    let reviewer = public_session(
        &native,
        "synthetic-c811-reviewer",
        "c811-works-revoke-jti",
        "operator",
    );
    let reviewer_action = native.mount(&reviewer, "public", &scope, 2);
    let created = native.service(
        &author,
        "public",
        &author_action,
        "works.create",
        json!({"kind": "doc", "title": "revoke", "content": content_for("doc"),
            "idempotency_key": "revoke-create"}),
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
    native
        .member_live
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let token = native.rpc(
        "app_screen_service_slot_prepare",
        native.credentials(
            &reviewer,
            "public",
            call(
                &reviewer_action,
                "works.review",
                json!({"id": id, "revision": revision, "decision": "approve"}),
            ),
        ),
    );
    assert!(
        token.is_err(),
        "retired reviewer mount must refuse to prepare"
    );
    // Membership withdrawal is global to the fixture; restore it only to read
    // the item back and prove the refused approval left the review state put.
    native
        .member_live
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let shown = native.service(
        &author,
        "public",
        &author_action,
        "works.get",
        json!({"id": id}),
        false,
    );
    assert_eq!(
        shown["reviewState"], "draft",
        "retired reviewer must not approve"
    );
}
