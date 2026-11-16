//! CAD-1857: endpoint mutations require a positively proven operator.

use super::Shared;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::json;

#[test]
fn endpoint_mutations_refuse_agent_and_unproven_callers() {
    let dir = tempfile::Builder::new().prefix("c1857").tempdir().unwrap();
    let pm = dir.path().join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = crate::daemon::ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "worker",
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: &cwd,
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let pid = std::process::id();
    let params = json!({
        "id": "ep_missing",
        "agent": "worker",
        "kind": "email",
        "slug": "billing",
        "project": "missing",
        "workflow": "missing",
    });
    for method in [
        "endpoint_add",
        "endpoint_verify",
        "endpoint_enable",
        "endpoint_disable",
        "endpoint_remove",
    ] {
        for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
            let text = scoped(who.clone(), || shared.dispatch(method, &params, pid))
                .expect_err("a non-operator caller must not mutate endpoints")
                .to_string();
            assert!(
                text.contains("operator action") || text.contains("not provably the operator"),
                "{method}: {who:?} was not stopped by the operator gate: {text}"
            );
        }
    }
    let operator = scoped(Asserted::Operator, || {
        shared.dispatch("endpoint_enable", &params, pid)
    })
    .expect_err("the endpoint does not exist")
    .to_string();
    assert!(
        operator.contains("Unknown endpoint"),
        "operator did not reach the store check: {operator}"
    );
}
