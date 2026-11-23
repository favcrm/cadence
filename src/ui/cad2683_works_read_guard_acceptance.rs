//! CAD-2683: the Works reads on a read-only screen mount. Drives a real
//! installed package through the real daemon and board:
//! 1. An operator read-only mount (the canvas mount) is admitted for
//!    `works.query` and `works.get`.
//! 2. A member read-only mount is refused for both reads, over the board and
//!    over the daemon's own session proof.
//! 3. Works writes stay refused on a read-only mount for the operator and a
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

fn args_for(operation: &str) -> Value {
    match operation {
        "works.query" | "works.create" => json!({}),
        _ => json!({"id": "doc-1"}),
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
    for read in READS {
        if let Err(error) = call_rpc(&rig, &action, creds, read) {
            let text = error.to_string();
            assert!(
                !text.contains("read-only screen admits") && !text.contains("operator-role"),
                "operator read {read} hit the guard: {text}"
            );
        }
    }
    for write in WRITES {
        let error = call_rpc(&rig, &action, creds, write)
            .expect_err(&format!("{write} must be refused on a read-only mount"));
        assert!(
            error.to_string().contains("read-only screen admits"),
            "{write}: {error}"
        );
    }

    // Member read-only mount: both reads and every write are refused.
    let owner = session(&rig, "owner", "owner");
    let member = session(&rig, "member", "member");
    let _ = owner;
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
