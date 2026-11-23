//! CAD-1989 independent refusal acceptance. Authored from the ticket, not
//! the implementation: `agent_profile_create` (a hire) is the operator's act.
//! A registered agent connection must be refused by the real operator gate,
//! and the refusal must leave no profile and no agent (runtime) row behind.
//! The operator positive control proves the same params would have succeeded,
//! so the refusal is the guard and not a bad request.

use crate::daemon::Shared;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::json;

#[test]
fn agent_caller_cannot_hire_a_profile() {
    let dir = tempfile::Builder::new().prefix("c1989").tempdir().unwrap();
    let pm = dir.path().join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = crate::daemon::ServeOptions {
        test_seam: true,
        ..Default::default()
    };
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
    let profiles = |s: &Shared| -> Vec<String> {
        s.store
            .settings()
            .unwrap()
            .doc
            .profiles
            .iter()
            .map(|p| p.id.clone())
            .collect()
    };
    let agents = |s: &Shared| -> Vec<String> {
        s.store
            .agents()
            .unwrap()
            .into_iter()
            .map(|a| a.alias)
            .collect()
    };
    let profiles_before = profiles(&shared);
    let agents_before = agents(&shared);
    let params = json!({"alias": "hired-by-agent", "catalogue": "developer"});

    let refusal = scoped(Asserted::Agent("worker".into()), || {
        shared.dispatch("agent_profile_create", &params, pid)
    })
    .expect_err("an agent connection must not hire a profile")
    .to_string();
    assert!(
        refusal.contains("operator action") || refusal.contains("a hire is the operator"),
        "not stopped by the operator gate: {refusal}"
    );
    assert_eq!(profiles(&shared), profiles_before, "a profile was written");
    assert_eq!(agents(&shared), agents_before, "an agent row was written");
    assert!(shared.store.agent_opt("hired-by-agent").unwrap().is_none());

    // Positive control: the same params succeed for the operator, creating a
    // profile and still no runtime row.
    scoped(Asserted::Operator, || {
        shared.dispatch("agent_profile_create", &params, pid)
    })
    .expect("the operator can hire with the same params");
    assert!(profiles(&shared).contains(&"hired-by-agent".to_string()));
    assert_eq!(agents(&shared), agents_before, "a hire wrote an agent row");
}
