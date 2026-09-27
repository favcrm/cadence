use super::*;

#[test]
fn cad631_explicit_local_schema_refuses_unsupported_execution_and_self_review() {
    use crate::store::app_runs::LocalWorkflow;
    let workflow = "---\ntitle: Local\ngoal: Reviewed text\n---\n## Write\nagent: writer\naction: local.text.produce\n\nWrite Markdown.\n\n## Review\nagent: reviewer\ndepends_on: 1\naction: local.text.review\n\nReview the exact artifact.\n";
    let inputs = std::collections::BTreeMap::new();
    assert!(LocalWorkflow::parse(workflow, &inputs).is_ok());
    for changed in [
        workflow.replace("local.text.produce", "local.publish"),
        workflow.replace(
            "action: local.text.produce",
            "action: local.text.produce\ntries: 2",
        ),
        workflow.replace("agent: reviewer", "agent: writer"),
        workflow.replace("depends_on: 1", "depends_on: CAD-1"),
    ] {
        assert!(LocalWorkflow::parse(&changed, &inputs).is_err());
    }
}

#[test]
fn cad631_material_digests_are_canonical_and_never_git_shas() {
    use crate::store::app_runs::{artifact_digest, material_digest};
    assert_eq!(
        material_digest(&json!({"b":2,"a":1})),
        material_digest(&json!({"a":1,"b":2}))
    );
    assert_ne!(
        material_digest(&json!({"a":1})),
        material_digest(&json!({"a":2}))
    );
    assert_eq!(
        artifact_digest(b"hello"),
        "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
}

fn runtime_fixture() -> (TempDir, Store, Value) {
    let (dir, s) = store();
    for (alias, role) in [("lead", "pm"), ("writer", "worker"), ("reviewer", "worker")] {
        s.register_agent(&NewAgent {
            alias,
            provider: "claude",
            endpoint_kind: "managed",
            role,
            cwd: dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: Some(&json!({"upstream":"lead"})),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
        s.set_identity(alias, &endpoint_at(4242)).unwrap();
    }
    let text="---\ntitle: Local\ngoal: Reviewed text\n---\n## Write\nagent: writer\naction: local.text.produce\n\nWrite Markdown.\n\n## Review\nagent: reviewer\ndepends_on: 1\naction: local.text.review\n\nReview the exact artifact.\n";
    let inputs = std::collections::BTreeMap::new();
    let workflow = crate::store::app_runs::LocalWorkflow::parse(text, &inputs).unwrap();
    s.app_capability_decide("install-1", "sha256:bundle", true)
        .unwrap();
    let run = s
        .app_run_create(
            "install-1",
            "sha256:bundle",
            &workflow,
            &inputs,
            "request-1",
            "lead",
            None,
        )
        .unwrap();
    (dir, s, run)
}

#[test]
fn cad631_generic_dispatch_and_reopen_cannot_escape_app_approval() {
    let (_dir, s, run) = runtime_fixture();
    let task = run["steps"][0]["task_id"].as_str().unwrap();
    assert!(
        s.dispatch_task(task, None, None, "operator").is_err(),
        "generic dispatch bypassed run approval"
    );
    assert!(
        s.reopen_task(task, "operator").is_err(),
        "generic reopen erased immutable app association"
    );
    assert!(
        s.create_task(
            run["id"].as_str().unwrap(),
            "forged-task",
            None,
            Some("writer"),
            None,
            None,
            None,
            None,
            None
        )
        .is_err(),
        "generic task insertion escaped frozen graph"
    );
}

#[test]
fn cad631_execution_approval_is_separate_and_create_is_idempotent() {
    let (_dir, s, run) = runtime_fixture();
    let id = run["id"].as_str().unwrap();
    assert!(s.app_run_dispatch(id, "sha256:bundle").is_err());
    assert!(s.app_run_decide(id, Some("forged"), false).is_err());
    s.app_run_decide(id, run["snapshot_digest"].as_str(), false)
        .unwrap();
    assert!(s
        .messages_for_task(run["steps"][0]["task_id"].as_str().unwrap())
        .unwrap()
        .is_empty());
    let first = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    let second = s.app_run_dispatch(id, "sha256:bundle").unwrap();
    assert_eq!(
        first["steps"][0]["message_id"],
        second["steps"][0]["message_id"]
    );
    assert_eq!(
        s.messages_for_task(run["steps"][0]["task_id"].as_str().unwrap())
            .unwrap()
            .len(),
        1
    );
    s.app_capability_decide("install-1", "sha256:bundle", false)
        .unwrap();
    assert!(s.app_run_dispatch(id, "sha256:bundle").is_err());
}
