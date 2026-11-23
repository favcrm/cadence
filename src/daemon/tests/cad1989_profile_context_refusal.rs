//! CAD-1989 independent refusal acceptance. Authored from the ticket, not the
//! implementation: a profile's standing job text is the operator's. The
//! `agent_profile_context` RPC is allowed for the operator and for an agent
//! whose own runtime row is registered against that profile, and refused for
//! every other caller. `agent_list` returns a profile's `job` only to the
//! operator.

use crate::daemon::Shared;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};

const JOB: &str = "zq-standing-job-text-7731: reconcile the ledgers nightly";

fn register(shared: &Shared, cwd: &str, alias: &str, params: Option<&str>) {
    shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd,
            sandbox: "read-only",
            instructions: None,
            params,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
}

#[test]
fn profile_job_is_the_operators_or_the_owning_runtimes() {
    let dir = tempfile::Builder::new().prefix("c1989c").tempdir().unwrap();
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

    scoped(Asserted::Operator, || {
        shared.dispatch(
            "agent_profile_create",
            &json!({"alias": "ledger", "catalogue": "developer", "job": JOB}),
            pid,
        )
    })
    .expect("operator creates the profile");
    scoped(Asserted::Operator, || {
        shared.dispatch(
            "agent_profile_create",
            &json!({"alias": "other", "catalogue": "developer", "job": "unrelated job"}),
            pid,
        )
    })
    .expect("operator creates a second profile");

    register(&shared, &cwd, "owner", Some(r#"{"profile":"ledger"}"#));
    register(&shared, &cwd, "stranger", Some(r#"{"profile":"other"}"#));
    register(&shared, &cwd, "bare", None);

    let ctx = |who: Asserted| -> Result<Value, String> {
        scoped(who, || {
            shared.dispatch("agent_profile_context", &json!({"id": "ledger"}), pid)
        })
        .map_err(|e| e.to_string())
    };

    // Allowed: the operator and the profile's own runtime both get the job.
    for who in [Asserted::Operator, Asserted::Agent("owner".into())] {
        let got = ctx(who).expect("an allowed caller reads the profile context");
        assert!(got.to_string().contains(JOB), "allowed caller lost the job");
    }

    // Refused: another profile's runtime, a profile-less runtime, and a
    // caller with no agent identity that is not the operator.
    let unproven = Asserted::Agent("no-such-agent".into());
    for who in [
        Asserted::Agent("stranger".into()),
        Asserted::Agent("bare".into()),
        unproven,
    ] {
        let err = ctx(who).expect_err("a non-owning caller must be refused");
        assert!(!err.contains(JOB), "refusal leaked the job: {err}");
    }

    // agent_list: the job is the operator's alone.
    let listed = |who: Asserted| -> Value {
        scoped(who, || shared.dispatch("agent_list", &json!({}), pid))
            .expect("agent_list is a read")
    };
    let jobs_of = |v: &Value| -> Vec<Value> {
        v["profiles"]
            .as_array()
            .expect("profiles array")
            .iter()
            .map(|p| p.get("job").cloned().unwrap_or(Value::Null))
            .collect()
    };
    let op = listed(Asserted::Operator);
    assert!(
        jobs_of(&op).iter().any(|j| j.as_str() == Some(JOB)),
        "operator must see the job: {op}"
    );
    for who in [
        Asserted::Agent("owner".into()),
        Asserted::Agent("stranger".into()),
        Asserted::Agent("bare".into()),
    ] {
        let v = listed(who);
        assert!(!v["profiles"].as_array().unwrap().is_empty());
        assert!(
            jobs_of(&v).iter().all(Value::is_null),
            "non-operator saw job text: {v}"
        );
        assert!(!v.to_string().contains(JOB));
    }
}
