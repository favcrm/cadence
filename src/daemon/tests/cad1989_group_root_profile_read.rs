//! CAD-1989: a stated exception, pinned so it is deliberate. A group-root agent
//! may register a member against a settings profile (a PM-run `join` needs the
//! job to write the member's briefing), and so may read that job through
//! `agent_show` with `include_profile_instructions`. An agent that is not the
//! member's group root still may not. The path predates CAD-1989.

use crate::daemon::Shared;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::json;

const JOB: &str = "kq-group-root-visible-job-4419: file the weekly report";

#[test]
fn a_group_root_reads_its_members_profile_job_and_a_stranger_does_not() {
    let dir = tempfile::Builder::new().prefix("c1989g").tempdir().unwrap();
    let pm = dir.path().join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = crate::daemon::ServeOptions {
        test_seam: true,
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let pid = std::process::id();
    let cwd = dir.path().to_str().unwrap().to_string();
    for alias in ["root", "stranger"] {
        shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider: "fake",
                endpoint_kind: "fake",
                role: "pm",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
    }
    scoped(Asserted::Operator, || {
        shared.dispatch(
            "agent_profile_create",
            &json!({"alias": "ledger", "catalogue": "developer", "job": JOB}),
            pid,
        )
    })
    .expect("operator creates the profile");

    // The root joins a member against the profile.
    scoped(Asserted::Agent("root".into()), || {
        shared.dispatch(
            "agent_register",
            &json!({
                "alias": "member", "provider": "fake", "endpoint_kind": "fake",
                "cwd": cwd, "profile": "ledger",
                "params": "{\"upstream\":\"root\"}",
            }),
            pid,
        )
    })
    .expect("a group root registers a member against a profile");

    let show = |who: &str| {
        scoped(Asserted::Agent(who.into()), || {
            shared.dispatch(
                "agent_show",
                &json!({"alias": "member", "include_profile_instructions": true}),
                pid,
            )
        })
    };
    let got = show("root").expect("the group root reads its member's row");
    assert!(
        got.to_string().contains(JOB),
        "the group root must see the profile job it registered: {got}"
    );
    let refused = show("stranger").expect_err("a non-root stranger is refused");
    assert!(!refused.to_string().contains(JOB));
}
