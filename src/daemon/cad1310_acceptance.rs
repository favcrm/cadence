//! Independent CAD-1310 refusal acceptance against the native project RPC.
//!
//! A registered agent with forged operator-shaped request fields must be
//! refused by the real daemon guard, without changing project policy or the
//! native work-approval state.

use crate::daemon::{ServeOptions, Shared};
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::json;
use tempfile::tempdir;

#[test]
fn cad1310_project_enable_lean_refuses_registered_agent_without_mutation() {
    let root = tempdir().expect("isolated CAD-1310 daemon root");
    let pm = root.path().join("pm");
    crate::issue::Pm::init(&pm).expect("initialize isolated PM");

    // A registered project with no PROJECT.md has the legacy default policy.
    // The missing optional file is intentional: refusal must not create it.
    let project_dir = pm.join("cadence");
    std::fs::create_dir_all(&project_dir).expect("create isolated project directory");
    let project_file = project_dir.join("project.yaml");
    std::fs::write(&project_file, "key: cadence\nprefix: CAD\n")
        .expect("write registered default-policy project");

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
        .expect("register real fixture agent");

    let project_before = std::fs::read(&project_file).expect("read project before RPC");
    let project_doc = project_dir.join("PROJECT.md");
    assert!(
        !project_doc.exists(),
        "default project starts without PROJECT.md"
    );
    let approvals_before = shared
        .store
        .work_approvals()
        .expect("read native approvals");

    // This is a valid project selection plus forged identity claims, not a
    // malformed-parameters-only refusal. The test seam represents the
    // registered worker's actual daemon caller identity.
    let params = json!({"key": "cadence", "as": "operator", "operator": true});
    let result = scoped(Asserted::Agent("worker".into()), || {
        shared.rpc_project_enable_lean(&params, std::process::id())
    });
    let error = result.expect_err("registered agent must not enable project policy");
    let message = error.to_string().to_lowercase();
    assert!(
        message.contains("operator"),
        "expected native operator-connection refusal, got {message}"
    );

    assert_eq!(
        std::fs::read(&project_file).expect("read project after RPC"),
        project_before,
        "refusal must leave registered project policy bytes unchanged"
    );
    assert!(
        !project_doc.exists(),
        "refusal must not create PROJECT.md or enable a policy"
    );
    assert_eq!(
        shared
            .store
            .work_approvals()
            .expect("read native approvals after RPC"),
        approvals_before,
        "refusal must not add or alter native approval state"
    );
}

#[test]
fn cad1310_project_enable_lean_activates_and_is_idempotent() {
    let root = tempdir().expect("isolated CAD-1310 daemon root");
    let pm = root.path().join("pm");
    crate::issue::Pm::init(&pm).expect("initialize isolated PM");
    let project_dir = pm.join("cadence");
    std::fs::create_dir_all(&project_dir).expect("create isolated project directory");
    let project_file = project_dir.join("project.yaml");
    std::fs::write(&project_file, "key: cadence\nprefix: CAD\n").expect("write registered project");

    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().expect("PM path is UTF-8"));
    let shared = Shared::new(root.path(), &opts).expect("initialize isolated daemon store");
    let project_doc = project_dir.join("PROJECT.md");
    assert!(
        !project_doc.exists(),
        "activation starts without PROJECT.md"
    );

    let first = scoped(Asserted::Operator, || {
        shared.rpc_project_enable_lean(&json!({"key": "cadence"}), std::process::id())
    })
    .expect("native operator activation");
    assert_eq!(first["enabled"], true);
    assert_eq!(first["profile"], "solo_operator");
    assert_eq!(first["version"], 1);
    assert_eq!(first["approval"]["by"], "operator");

    let text = std::fs::read_to_string(&project_doc).expect("read activated project policy");
    let policy = crate::issue::delivery_policy::parse(&text)
        .expect("activated policy parses")
        .expect("activated policy exists");
    assert_eq!(
        policy.solo_operator,
        Some(crate::issue::delivery_policy::SoloOperatorProfile { version: 1 })
    );
    assert_eq!(
        crate::issue::delivery_policy::digest(&policy),
        first["digest"]
    );
    let approvals = shared.store.work_approvals().expect("read native approval");
    let approval = approvals
        .get("cadence")
        .expect("approval recorded for project");
    let approved = crate::issue::delivery_policy::approved_from(approval)
        .expect("recorded approval validates");
    let resolved =
        crate::issue::delivery_policy::effective("cadence", Ok(Some(policy)), Some(&approved));
    assert_eq!(resolved.source, "approved");
    assert_eq!(resolved.digest, first["digest"].as_str().unwrap());
    assert_eq!(approved.digest, first["digest"].as_str().unwrap());

    let file_before_repeat = std::fs::read(&project_doc).expect("snapshot project file");
    let approvals_before_repeat = approvals;
    let second = scoped(Asserted::Operator, || {
        shared.rpc_project_enable_lean(&json!({"key": "cadence"}), std::process::id())
    })
    .expect("repeated native operator activation");
    assert_eq!(second, first, "repeated activation returns the same result");
    assert_eq!(
        std::fs::read(&project_doc).expect("read repeated activation file"),
        file_before_repeat,
        "repeated activation does not rewrite policy text"
    );
    assert_eq!(
        shared
            .store
            .work_approvals()
            .expect("read repeated approvals"),
        approvals_before_repeat,
        "repeated activation does not change approval evidence"
    );
}

#[test]
fn cad1310_project_enable_lean_preserves_root_comment_and_body() {
    let root = tempdir().expect("isolated CAD-1310 daemon root");
    let pm = root.path().join("pm");
    crate::issue::Pm::init(&pm).expect("initialize isolated PM");
    let project_dir = pm.join("cadence");
    std::fs::create_dir_all(&project_dir).expect("create isolated project directory");
    std::fs::write(
        project_dir.join("project.yaml"),
        "key: cadence\nprefix: CAD\n",
    )
    .expect("write registered project");

    let delivery_yaml = serde_yaml::to_string(&crate::issue::delivery_policy::default_policy())
        .expect("serialize default policy");
    let indented = delivery_yaml
        .lines()
        .map(|line| format!("  {line}\n"))
        .collect::<String>()
        .replacen("  risk:", "# preserve operator note\n  risk:", 1);
    let original = format!(
        "---\nstages: [backlog, build, review, done]\noperator_stages: [build]\nstaffing: {{owner: ops}}\ndelivery:\n{indented}---\n\n# Existing body\nKeep this text.\n"
    );
    crate::issue::delivery_policy::parse(&original)
        .expect("original delivery YAML is valid")
        .expect("original default delivery policy exists");
    let project_doc = project_dir.join("PROJECT.md");
    std::fs::write(&project_doc, &original).expect("write existing project document");

    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().expect("PM path is UTF-8"));
    let shared = Shared::new(root.path(), &opts).expect("initialize isolated daemon store");
    scoped(Asserted::Operator, || {
        shared.rpc_project_work_approve(&json!({"project": "cadence"}), std::process::id())
    })
    .expect("approve existing stage gates through native operator RPC");
    let result = scoped(Asserted::Operator, || {
        shared.rpc_project_enable_lean(&json!({"key": "cadence"}), std::process::id())
    });
    let result = result.expect("native activation preserves valid commented policy");
    let updated = std::fs::read_to_string(&project_doc).expect("read updated project document");
    assert!(updated.contains("# preserve operator note\n"));
    assert!(updated.contains("# Existing body\nKeep this text.\n"));
    assert!(updated.contains("staffing: {owner: ops}"));
    assert!(updated.contains("stages: [backlog, build, review, done]"));
    let policy = crate::issue::delivery_policy::parse(&updated)
        .expect("updated policy remains valid YAML")
        .expect("updated delivery policy exists");
    assert_eq!(
        policy.solo_operator,
        Some(crate::issue::delivery_policy::SoloOperatorProfile { version: 1 })
    );
    assert_eq!(
        crate::issue::delivery_policy::digest(&policy),
        result["digest"]
    );
}
