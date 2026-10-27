//! CAD-1303: `app_social_draft_discard` is operator-only. An agent caller and
//! an unproven caller are refused at the operator gate, before any session,
//! mount or store work. The reply for the operator is a different refusal
//! (no live session), which shows the gate is the thing that stopped the rest.

use super::Shared;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};

fn params() -> Value {
    json!({
        "action_token": "a".repeat(64), "token": "t", "key": "k", "origin": "loopback",
        "tool_alias": "drafts", "draft_id": "sdr-1", "revision": 1,
    })
}

#[test]
fn discard_refuses_agent_and_unproven_callers_at_the_operator_gate() {
    let dir = tempfile::Builder::new().prefix("c1303").tempdir().unwrap();
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
    for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        let text = scoped(who.clone(), || {
            shared.dispatch("app_social_draft_discard", &params(), pid)
        })
        .expect_err("a non-operator caller must not discard")
        .to_string();
        assert!(
            text.contains("operator action") || text.contains("not provably the operator"),
            "{who:?} was not stopped by the operator gate: {text}"
        );
    }
    let operator = scoped(Asserted::Operator, || {
        shared.dispatch("app_social_draft_discard", &params(), pid)
    })
    .expect_err("no live session")
    .to_string();
    assert!(
        operator.contains("live operator session"),
        "operator reached the session check: {operator}"
    );
}
