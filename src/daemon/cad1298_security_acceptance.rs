//! Independent CAD-1298 native security-boundary acceptance.
//!
//! These checks exercise the real daemon RPC authorization and registered-agent
//! identity digest. They do not construct a profile-backed delivery assignment,
//! qualify a reviewer, or prove creation/export of a native qualified receipt.

use crate::daemon::{ServeOptions, Shared};
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use tempfile::tempdir;

fn fixture() -> (tempfile::TempDir, std::sync::Arc<Shared>) {
    let root = tempdir().expect("temporary CAD-1298 daemon root");
    let pm = root.path().join("pm");
    crate::issue::Pm::init(&pm).expect("initialize isolated PM");
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().expect("PM path is UTF-8"));
    let shared = Shared::new(root.path(), &opts).expect("initialize isolated daemon store");
    for (alias, role) in [("worker", "worker"), ("reviewer", "reviewer")] {
        shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider: "fake",
                endpoint_kind: "fake",
                role,
                cwd: root.path().to_str().expect("fixture path is UTF-8"),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .expect("register fixture agent");
    }
    (root, shared)
}

fn review_evidence(
    shared: &Shared,
    caller: Asserted,
    params: Value,
) -> crate::error::Result<Value> {
    scoped(caller, || {
        shared.rpc_delivery_review_evidence(&params, std::process::id())
    })
}

#[test]
fn cad1298_security_review_evidence_refuses_agent_and_forged_operator_claims() {
    let (_root, shared) = fixture();
    let params = json!({"requests": [{
        "issue": "CAD-1298",
        "pr": "https://github.com/favcrm/cadence/pull/1298",
        "sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    }]});

    let agent_result = review_evidence(&shared, Asserted::Agent("reviewer".into()), params.clone());
    assert!(
        agent_result.is_err(),
        "registered reviewer must not call operator-only evidence export"
    );
    let agent_error = agent_result.unwrap_err().to_string().to_lowercase();
    assert!(
        agent_error.contains("operator"),
        "expected real operator-connection refusal, got {agent_error}"
    );

    let mut forged = params.as_object().expect("request object").clone();
    forged.insert("security_qualified".into(), json!(true));
    forged.insert("combined".into(), json!(true));
    forged.insert(
        "policy_digest".into(),
        json!("sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"),
    );
    let forged_result = review_evidence(&shared, Asserted::Agent("reviewer".into()), json!(forged));
    assert!(
        forged_result.is_err(),
        "caller qualification fields must not authorize export"
    );
    let forged_error = forged_result.unwrap_err().to_string().to_lowercase();
    assert!(
        forged_error.contains("operator"),
        "caller fields must not bypass actual caller guard, got {forged_error}"
    );

    // Authentication control: an asserted operator reaches actual batch validation.
    let control = review_evidence(&shared, Asserted::Operator, json!({"requests": []}));
    assert!(control.is_err(), "empty batch is invalid");
    let control_error = control.unwrap_err().to_string().to_lowercase();
    assert!(
        control_error.contains("between 1 and 100"),
        "operator should reach request validation, got {control_error}"
    );
}
