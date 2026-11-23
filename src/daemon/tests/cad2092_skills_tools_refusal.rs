//! CAD-2092 independent refusal acceptance. Authored from the ticket, not the
//! implementation; the implementer must not edit or weaken it (AGENTS.md
//! "Gates and security work").
//!
//! Ticket requirements proved here:
//! - An enrolled agent caller cannot change its own skills (`agent_skills_set`)
//!   or tools (`agent_tools_set`), nor bypass through `agent_set` with
//!   `skills` or `disabled_tools`; every refusal leaves stored params intact.
//!   Even a PM cannot mutate a downstream worker's skills or tools.
//! - Even the operator cannot turn off the required `cadence` skill or disable
//!   the required `cadence` tool.
//! - Positive control: the operator can update valid non-required `skills` and
//!   `disabled_tools`, and both responses report `applies: "next_launch"`.

use crate::daemon::Shared;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::json;

#[test]
fn agent_cannot_change_own_skills_or_tools_and_required_cannot_be_disabled() {
    let dir = tempfile::Builder::new().prefix("c2092").tempdir().unwrap();
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
            alias: "pm",
            provider: "pi",
            endpoint_kind: "managed",
            role: "pm",
            cwd: &cwd,
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "worker",
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: &cwd,
            sandbox: "read-only",
            instructions: None,
            params: Some(r#"{"upstream":"pm"}"#),
            team_role: None,
            model_policy: None,
        })
        .unwrap();

    let pid = std::process::id();
    let params_before = shared.store.agent("worker").unwrap().params;
    let valid_skills = json!({"alias": "worker", "skills": ["cadence", "deliverable-qa"]});
    let valid_tools = json!({"alias": "worker", "disabled_tools": ["bash"]});

    // 1. Enrolled agent caller (and its PM) cannot change skills via agent_skills_set.
    for caller in ["worker", "pm"] {
        let refusal = scoped(Asserted::Agent(caller.into()), || {
            shared.dispatch("agent_skills_set", &valid_skills, pid)
        })
        .expect_err("an agent caller must not change skills via agent_skills_set")
        .to_string();
        assert!(
            refusal.contains("operator action"),
            "agent_skills_set for '{caller}' was not stopped by the operator gate: {refusal}"
        );
        assert_eq!(
            shared.store.agent("worker").unwrap().params,
            params_before,
            "refused agent_skills_set by '{caller}' mutated stored params"
        );
    }

    // 2. Enrolled agent caller (and its PM) cannot change tools via agent_tools_set.
    for caller in ["worker", "pm"] {
        let refusal = scoped(Asserted::Agent(caller.into()), || {
            shared.dispatch("agent_tools_set", &valid_tools, pid)
        })
        .expect_err("an agent caller must not change tools via agent_tools_set")
        .to_string();
        assert!(
            refusal.contains("operator action"),
            "agent_tools_set for '{caller}' was not stopped by the operator gate: {refusal}"
        );
        assert_eq!(
            shared.store.agent("worker").unwrap().params,
            params_before,
            "refused agent_tools_set by '{caller}' mutated stored params"
        );
    }

    // 3. Enrolled agent caller (and its PM) cannot bypass via agent_set with
    //    `skills` or `disabled_tools`, with or without `next_launch`.
    for patch in [
        json!({"skills": ["cadence", "deliverable-qa"]}),
        json!({"disabled_tools": ["bash"]}),
    ] {
        for next_launch in [false, true] {
            let req = json!({
                "alias": "worker",
                "patch": patch,
                "next_launch": next_launch,
            });
            for caller in ["worker", "pm"] {
                let refusal = scoped(Asserted::Agent(caller.into()), || {
                    shared.dispatch("agent_set", &req, pid)
                })
                .expect_err("an agent caller must not bypass skills/tools via agent_set")
                .to_string();
                if caller == "pm" {
                    assert!(
                        refusal.contains("operator action"),
                        "PM agent_set bypass ({req}) was not stopped by the operator gate: {refusal}"
                    );
                }
                assert_eq!(
                    shared.store.agent("worker").unwrap().params,
                    params_before,
                    "refused agent_set ({req}) by '{caller}' mutated stored params"
                );
            }
        }
    }

    // 4. Even the operator cannot turn off the required `cadence` skill or tool.
    for bad_skills in [json!([]), json!(["deliverable-qa"])] {
        let refusal = scoped(Asserted::Operator, || {
            shared.dispatch(
                "agent_skills_set",
                &json!({"alias": "worker", "skills": bad_skills}),
                pid,
            )
        })
        .expect_err("required cadence skill must not be turned off")
        .to_string();
        assert!(
            refusal.contains("required") && refusal.contains("cadence"),
            "unexpected refusal when omitting required cadence skill: {refusal}"
        );
        assert_eq!(
            shared.store.agent("worker").unwrap().params,
            params_before,
            "refused required-skill write mutated stored params"
        );
    }
    for bad_tools in [json!(["cadence"]), json!(["bash", "cadence"])] {
        let refusal = scoped(Asserted::Operator, || {
            shared.dispatch(
                "agent_tools_set",
                &json!({"alias": "worker", "disabled_tools": bad_tools}),
                pid,
            )
        })
        .expect_err("required cadence tool must not be turned off")
        .to_string();
        assert!(
            refusal.contains("required") && refusal.contains("cadence"),
            "unexpected refusal when disabling required cadence tool: {refusal}"
        );
        assert_eq!(
            shared.store.agent("worker").unwrap().params,
            params_before,
            "refused required-tool write mutated stored params"
        );
    }

    // 5. Positive control: the operator updates valid non-required skills and
    //    disabled_tools, and both report `applies: "next_launch"`.
    let skills_out = scoped(Asserted::Operator, || {
        shared.dispatch("agent_skills_set", &valid_skills, pid)
    })
    .expect("operator must be able to update valid non-required skills");
    assert_eq!(skills_out["alias"], "worker");
    assert_eq!(skills_out["state"], "updated");
    assert_eq!(skills_out["applies"], "next_launch");
    assert_eq!(skills_out["skills"], json!(["cadence", "deliverable-qa"]));

    let tools_out = scoped(Asserted::Operator, || {
        shared.dispatch("agent_tools_set", &valid_tools, pid)
    })
    .expect("operator must be able to update valid non-required disabled_tools");
    assert_eq!(tools_out["alias"], "worker");
    assert_eq!(tools_out["state"], "updated");
    assert_eq!(tools_out["applies"], "next_launch");
    assert_eq!(tools_out["disabled_tools"], json!(["bash"]));

    let stored = shared
        .store
        .agent("worker")
        .unwrap()
        .params
        .expect("worker params persisted");
    assert_eq!(stored["skills"], json!(["cadence", "deliverable-qa"]));
    assert_eq!(stored["disabled_tools"], json!(["bash"]));
}
