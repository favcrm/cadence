//! CAD-2683: the Works reads on a read-only screen mount. Drives a real
//! installed package through the real daemon and board:
//! 1. An operator read-only mount (the canvas mount) is admitted for
//!    `works.query` and `works.get`.
//! 2. A public operator-role (owner) session is admitted for both reads, over
//!    the board and on a read-only mount over the daemon's own session proof
//!    (CAD-2717: the guard admits a positive operator fact).
//! 3. A member read-only mount is refused for both reads, over the board and
//!    over the daemon's own session proof.
//! 4. Works writes stay refused on a read-only mount for the operator and a
//!    member, with the read-only refusal.

#![cfg(all(test, feature = "test-seam"))]

use super::cad2402_routines_list_guard_acceptance::{
    consume, cookie_token, install_app, operator_session, post, rpc_as, session, start,
    status_and_json,
};
use super::*;
use crate::test_seam::Asserted;

const READS: [&str; 2] = ["works.query", "works.get"];
const WRITES: [&str; 5] = [
    "works.create",
    "works.patch",
    "works.trash",
    "works.restore",
    "works.request_review",
];
const OPERATIONS: [&str; 7] = [
    "works.query",
    "works.get",
    "works.create",
    "works.patch",
    "works.trash",
    "works.restore",
    "works.request_review",
];

/// A validly shaped minimal argument set for each operation, so the closed-shape
/// check (which runs before the session proof and the read-only guard) passes
/// and the guard is the only reason left for a refusal.
fn args_for(operation: &str) -> Value {
    match operation {
        "works.query" => json!({}),
        "works.get" => json!({"id": "missing-doc"}),
        "works.create" => json!({"kind": "doc", "title": "T", "content": {},
            "idempotency_key": "create-1"}),
        "works.patch" => json!({"id": "missing-doc", "if_revision": 1, "operations": [],
            "idempotency_key": "patch-1"}),
        "works.trash" | "works.restore" => json!({"id": "missing-doc", "if_revision": 1,
            "idempotency_key": "trash-1"}),
        "works.request_review" => json!({"id": "missing-doc", "revision": 1}),
        other => panic!("no fixture arguments for {other}"),
    }
}

fn call_rpc(
    rig: &super::cad2402_routines_list_guard_acceptance::Rig,
    action: &str,
    credentials: (&str, &str, &str),
    operation: &str,
) -> Result<Value> {
    rpc_as(
        rig,
        Asserted::Operator,
        "app_screen_service_call",
        json!({"action_token": action, "operation": operation, "args": args_for(operation),
            "token": credentials.0, "key": credentials.1, "origin": credentials.2}),
    )
}

#[test]
fn cad2683_operator_read_only_mount_reads_works_and_member_is_refused() {
    let rig = start();
    let operator = operator_session(&rig);
    let creds = (operator.0.as_str(), operator.1.as_str(), "loopback");
    let (install, context) = install_app(&rig, "cad2683-works", &OPERATIONS);

    // Operator read-only mount: the reads are admitted, every write refused.
    let minted = rpc_as(
        &rig,
        Asserted::Operator,
        "app_screen_mint",
        json!({"install_id": install, "context_id": context, "tag": "main",
            "generation": 1, "token": operator.0, "key": operator.1,
            "origin": "loopback", "read_only": true}),
    )
    .expect("operator read-only mint");
    let advertised = minted["operations"].to_string();
    for read in READS {
        assert!(advertised.contains(read), "{read} advertised: {minted}");
    }
    for write in WRITES {
        assert!(
            !advertised.contains(write),
            "{write} not advertised: {minted}"
        );
    }
    let action = consume(&rig, &minted);
    let listed = call_rpc(&rig, &action, creds, "works.query")
        .expect("operator works.query is admitted on a read-only mount");
    assert!(
        listed.to_string().contains("[]") || listed.is_object() || listed.is_array(),
        "works.query reply: {listed}"
    );
    let error = call_rpc(&rig, &action, creds, "works.get")
        .expect_err("works.get of a missing id fails in the store");
    let text = error.to_string();
    assert!(
        !text.contains("read-only screen admits") && !text.contains("operator-role"),
        "operator works.get hit the guard: {text}"
    );
    assert!(
        text.contains("not_found") || text.contains("not found"),
        "works.get of a missing id is not_found: {text}"
    );
    for write in WRITES {
        let error = call_rpc(&rig, &action, creds, write)
            .expect_err(&format!("{write} must be refused on a read-only mount"));
        assert!(
            error.to_string().contains("read-only screen admits"),
            "{write}: {error}"
        );
    }

    // Public operator-role (owner) session: admitted for both reads.
    let owner = session(&rig, "owner", "owner");
    let (status, minted) = status_and_json(post(
        &rig,
        &format!("/api/app-installations/{install}/screens/main/mount"),
        Some(&owner),
        &json!({"context_id": context}),
    ));
    assert_eq!(status, 200, "owner mount failed: {minted}");
    let action = consume(&rig, &minted);
    let (status, body) = status_and_json(post(
        &rig,
        "/api/app-screen-services/call",
        Some(&owner),
        &json!({"action_token": action, "operation": "works.query", "args": args_for("works.query")}),
    ));
    assert_eq!(status, 200, "owner works.query over the board: {body}");
    let owner_creds = (cookie_token(&owner), "", "public");
    let minted = rpc_as(
        &rig,
        Asserted::Operator,
        "app_screen_mint",
        json!({"install_id": install, "context_id": context, "tag": "main",
            "generation": 2, "token": owner_creds.0, "key": owner_creds.1,
            "origin": "public", "read_only": true}),
    )
    .expect("owner read-only mint");
    let action = consume(&rig, &minted);
    call_rpc(&rig, &action, owner_creds, "works.query")
        .expect("owner works.query is admitted on a read-only mount");
    let text = call_rpc(&rig, &action, owner_creds, "works.get")
        .expect_err("works.get of a missing id fails in the store")
        .to_string();
    assert!(
        !text.contains("operator-role")
            && (text.contains("not_found") || text.contains("not found")),
        "owner works.get must reach the store: {text}"
    );

    // Member read-only mount: both reads and every write are refused.
    let member = session(&rig, "member", "member");
    let (status, minted) = status_and_json(post(
        &rig,
        &format!("/api/app-installations/{install}/screens/main/mount"),
        Some(&member),
        &json!({"context_id": context}),
    ));
    assert_eq!(status, 200, "member mount failed: {minted}");
    let action = consume(&rig, &minted);
    for operation in READS.iter().chain(WRITES.iter()) {
        let (status, body) = status_and_json(post(
            &rig,
            "/api/app-screen-services/call",
            Some(&member),
            &json!({"action_token": action, "operation": operation, "args": args_for(operation)}),
        ));
        assert!(
            matches!(status, 401 | 403),
            "member {operation} over the board must be refused, got {status}: {body}"
        );
        let error = call_rpc(
            &rig,
            &action,
            (cookie_token(&member), "", "public"),
            operation,
        )
        .expect_err(&format!("the daemon must refuse a member {operation}"));
        let expected = if READS.contains(operation) {
            "operator-role public session"
        } else {
            "read-only screen admits"
        };
        assert!(error.to_string().contains(expected), "{operation}: {error}");
    }
}
