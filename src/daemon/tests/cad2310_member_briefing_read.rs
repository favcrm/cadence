//! CAD-2310: a group root that briefs its member (a PM-run `join` or
//! `agent bootstrap`) reads the member with `briefing::briefing_read`, and
//! that scoped read must still carry what the briefing is built from: the
//! profile job, `params` (whose `upstream` names the briefing directory and
//! the "Upstream" line) and `cwd`.

use crate::daemon::Shared;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::json;

const JOB: &str = "kq-member-briefing-job-2310: reconcile the ledger";

#[test]
fn a_group_roots_briefing_read_carries_the_members_job_params_and_cwd() {
    let dir = tempfile::Builder::new().prefix("c2310b").tempdir().unwrap();
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
    shared
        .store
        .register_agent(&NewAgent {
            alias: "root",
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
    scoped(Asserted::Operator, || {
        shared.dispatch(
            "agent_profile_create",
            &json!({"alias": "ledger", "catalogue": "developer", "job": JOB}),
            pid,
        )
    })
    .expect("operator creates the profile");

    // A member joined against a profile, and one joined without.
    for (member, profile) in [("profiled", Some("ledger")), ("plain", None)] {
        let as_root = |method: &str, params: &serde_json::Value| {
            scoped(Asserted::Agent("root".into()), || {
                shared.dispatch(method, params, pid)
            })
        };
        as_root(
            "agent_register",
            &json!({
                "alias": member, "provider": "fake", "endpoint_kind": "fake",
                "cwd": cwd, "profile": profile,
                "params": "{\"upstream\":\"root\"}",
            }),
        )
        .expect("a group root registers its member");
        let got = as_root("agent_show", &crate::briefing::briefing_read(member))
            .expect("the group root reads its member for the briefing");
        let agent = &got["agent"];
        assert_eq!(agent["params"]["upstream"], "root", "{member}: {got}");
        assert_eq!(agent["cwd"], json!(cwd), "{member}: {got}");
        assert_eq!(
            crate::client::briefing_path(dir.path(), &agent["params"], member),
            dir.path()
                .join("briefings")
                .join("root")
                .join(format!("BRIEFING-{member}.md")),
            "{member}: the briefing must land in its group root's directory"
        );
        if profile.is_some() {
            assert!(
                agent["instructions"]
                    .as_str()
                    .is_some_and(|text| text.contains(JOB)),
                "the profile job must reach the briefing: {got}"
            );
        }
    }
}
// CAD-2307 probe
