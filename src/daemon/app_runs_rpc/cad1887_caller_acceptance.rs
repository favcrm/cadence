//! CAD-1887: agent and unproven callers cannot record a disposition,
//! reserve a ticket, or bind one. The check goes through `dispatch`,
//! so it is the daemon's `operator_connection` guard, and the store
//! stays unchanged.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::daemon::{ServeOptions, Shared};
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};

fn fixture() -> (tempfile::TempDir, Arc<Shared>) {
    let root = tempfile::tempdir().expect("temporary CAD-1887 daemon root");
    let pm = root.path().join("pm");
    crate::issue::Pm::init(&pm).expect("initialize isolated PM");
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().expect("PM path is UTF-8"));
    let shared = Shared::new(root.path(), &opts).expect("initialize isolated daemon store");
    shared
        .store
        .register_agent(&NewAgent {
            alias: "worker",
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: root.path().to_str().expect("fixture path is UTF-8"),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .expect("register fixture agent");
    (root, shared)
}

fn call(shared: &Arc<Shared>, who: Asserted, method: &str, params: Value) -> crate::Result<Value> {
    scoped(who, || shared.dispatch(method, &params, std::process::id()))
}

fn rows(shared: &Shared, table: &str) -> i64 {
    let conn = shared.store.try_conn().expect("store connection");
    let sql = format!("SELECT COUNT(*) FROM {table}");
    conn.query_row(&sql, [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("count {table}: {error}"))
}

fn params(method: &str) -> Value {
    let digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    match method {
        "app_run_disposition" => json!({
            "run_id": "run-0123456789abcdef0123456789abcdef",
            "request_id": "req-1887-disposition",
            "artifact_id": "artifact-1",
            "digest": digest,
            "decision": "accept",
        }),
        "app_run_ticket_reserve" => json!({
            "run_id": "run-0123456789abcdef0123456789abcdef",
            "request_id": "req-1887-reserve",
            "artifact_id": "artifact-1",
            "digest": digest,
            "index": 1,
        }),
        "app_run_ticket_bind" => json!({
            "run_id": "run-0123456789abcdef0123456789abcdef",
            "request_id": "req-1887-bind",
            "issue_id": "CAD-1",
        }),
        _ => panic!("unexpected method {method}"),
    }
}

#[test]
fn cad1887_agent_and_unproven_callers_cannot_decide_or_ticket() {
    let (_root, shared) = fixture();
    let methods = [
        "app_run_disposition",
        "app_run_ticket_reserve",
        "app_run_ticket_bind",
    ];
    let callers = [Asserted::Agent("worker".into()), Asserted::Unproven];
    for method in methods {
        // Disposition and reserve look up the run. Bind looks up the
        // reservation first, so a missing one fails there. Either string
        // is the store, past operator_connection.
        let store_error = if method == "app_run_ticket_bind" {
            "ticket bind needs the reserved request"
        } else {
            "unknown app run"
        };
        for who in &callers {
            let error = call(&shared, who.clone(), method, params(method))
                .expect_err("a non-operator must be refused")
                .to_string();
            assert!(
                error.contains("is an operator action"),
                "{method} as {who:?} did not hit operator_connection: {error}"
            );
            assert!(
                !error.contains(store_error),
                "{method} as {who:?} reached the store: {error}"
            );
        }
        let operator = call(&shared, Asserted::Operator, method, params(method))
            .expect_err("a missing run is not a decision")
            .to_string();
        assert!(
            operator.contains(store_error),
            "{method} operator did not reach the store: {operator}"
        );
        assert!(
            !operator.contains("is an operator action"),
            "{method} operator was refused as a caller: {operator}"
        );
    }
    assert_eq!(rows(&shared, "app_run_dispositions"), 0);
    assert_eq!(rows(&shared, "app_run_tickets"), 0);
}
