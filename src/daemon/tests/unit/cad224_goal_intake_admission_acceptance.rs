use super::*;
use crate::issue::{board, plan, write, Pm};
use crate::test_seam::{scoped, Asserted};

#[test]
fn approved_intake_after_restart_refuses_provider_attention_without_dispatch() {
    fn git(repo: &Path, args: &[&str]) -> String {
        let mut command = std::process::Command::new("git");
        command.arg("-C").arg(repo).args(args);
        let output = crate::reaper::output(&mut command).unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("source");
    std::fs::create_dir_all(repo.join("docs/cadence")).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.name", "Acceptance"]);
    git(
        &repo,
        &["config", "user.email", "acceptance@example.invalid"],
    );
    std::fs::write(repo.join("docs/scope.md"), "Approved scope\n").unwrap();
    std::fs::write(repo.join("src/module.rs"), "// module\n").unwrap();
    std::fs::write(
        repo.join("docs/cadence/project-context.yaml"),
        "schema: 1\nproject: cadence\ndocuments:\n  - id: scope\n    kind: scope\n    path: docs/scope.md\n    title: Scope\n    required: true\n    roles: []\n",
    )
    .unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "Initial source"]);

    let pm_dir = dir.path().join("pm");
    let pm = Pm::init(&pm_dir).unwrap();
    write::project_add(
        &pm,
        "cadence",
        "CAD",
        &[repo.to_string_lossy().into_owned()],
        &[],
        &[],
        None,
    )
    .unwrap();
    let draft = "---\ntitle: Deliver capability\ngoal: Deliver the scoped capability\nintake:\n  constraints: [Preserve integration]\n  success_criteria: [Capability available]\n  unknowns: []\n---\n\n## Implement capability\nagent: worker-1\nsources: scope\npaths: src/module.rs\n\n### Acceptance\n- [ ] Capability is available\n";
    drop(pm);

    let opts = ServeOptions {
        test_seam: true,
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let request = json!({"project": "cadence", "text": draft});
    let proposal = scoped(Asserted::Operator, || {
        shared.rpc_plan_propose(&request, std::process::id())
    })
    .unwrap();
    let epic = proposal["epic"].as_str().unwrap();
    let issue = proposal["tickets"][0].as_str().unwrap();
    scoped(Asserted::Operator, || {
        shared.rpc_plan_decide(&json!({"epic": epic}), std::process::id(), true)
    })
    .unwrap();
    let master = crate::master::ALIAS;
    register(&shared, dir.path(), master);
    shared
        .store
        .set_identity(
            master,
            &adapter::Identity {
                thread_id: "master-thread".into(),
                session_id: "master-session".into(),
                model: None,
                effort: None,
                pid: std::process::id(),
                endpoint: None,
                generation: Some("224aabbccddeeff0224aabbccddeeff0".into()),
                attach: None,
            },
        )
        .unwrap();
    register(&shared, dir.path(), "worker-1");
    shared
        .store
        .set_state_detached(
            "worker-1",
            "attention",
            Some("Provider connection is closed"),
        )
        .unwrap();
    drop(shared);
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let event_counts = || {
        let conn = rusqlite::Connection::open(dir.path().join("cadence.sqlite3")).unwrap();
        ["plan_proposed", "plan_approved"].map(|kind| {
            conn.query_row(
                "SELECT COUNT(*) FROM events WHERE kind = ?1",
                [kind],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
        })
    };
    let before_events = event_counts();
    assert_eq!(before_events, [1, 1]);
    let before_head = git(&pm_dir, &["rev-parse", "HEAD"]);
    let before_replay_queue = shared.store.queued_count("worker-1").unwrap();
    let replay = scoped(Asserted::Operator, || {
        shared.rpc_plan_propose(&request, std::process::id())
    })
    .unwrap();
    assert_eq!(replay["epic"], epic);
    assert_eq!(replay["replayed"], true);
    assert_eq!(git(&pm_dir, &["rev-parse", "HEAD"]), before_head);
    assert_eq!(event_counts(), before_events);
    assert_eq!(
        shared.store.queued_count("worker-1").unwrap(),
        before_replay_queue
    );
    let pm = Pm::at(&pm_dir).unwrap();
    let before = board::find_issue(&pm.dir, issue).unwrap();
    assert_eq!(before.front.status, "ready");
    assert!(before.front.claim.is_none());
    plan::gate_id(&pm.dir, issue).unwrap();
    assert_eq!(shared.store.agent("worker-1").unwrap().state, "attention");
    let issue_file = pm.dir.join("cadence").join(issue).join("issue.md");
    let before_issue = std::fs::read(&issue_file).unwrap();
    let before_lanes = git(&repo, &["worktree", "list", "--porcelain"]);
    let before_queued = shared.store.queued_count("worker-1").unwrap();
    let before_delivery = crate::delivery::load(dir.path()).unwrap();
    assert_eq!(before_queued, 0);
    let result = scoped(Asserted::Agent(master.into()), || {
        shared.rpc_master_dispatch(&json!({"issue": issue}), std::process::id())
    });
    let refusal = result.unwrap_err().to_string();
    assert!(refusal.contains("fenced"), "{refusal}");
    assert_eq!(std::fs::read(&issue_file).unwrap(), before_issue);
    assert_eq!(
        git(&repo, &["worktree", "list", "--porcelain"]),
        before_lanes
    );
    assert_eq!(
        shared.store.queued_count("worker-1").unwrap(),
        before_queued
    );
    assert_eq!(
        crate::delivery::load(dir.path()).unwrap().len(),
        before_delivery.len()
    );
    let after = board::find_issue(&pm.dir, issue).unwrap();
    assert_eq!(after.front.status, "ready");
    assert!(after.front.claim.is_none());
}

#[test]
fn fresh_dispatch_refuses_stale_approved_intake_evidence() {
    fn git(repo: &Path, args: &[&str]) -> String {
        let mut command = std::process::Command::new("git");
        command.arg("-C").arg(repo).args(args);
        let output = crate::reaper::output(&mut command).unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("source");
    std::fs::create_dir_all(repo.join("docs/cadence")).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.name", "Acceptance"]);
    git(
        &repo,
        &["config", "user.email", "acceptance@example.invalid"],
    );
    std::fs::write(repo.join("docs/scope.md"), "Approved scope\n").unwrap();
    std::fs::write(repo.join("src/module.rs"), "// module\n").unwrap();
    std::fs::write(
        repo.join("docs/cadence/project-context.yaml"),
        "schema: 1\nproject: cadence\ndocuments:\n  - id: scope\n    kind: scope\n    path: docs/scope.md\n    title: Scope\n    required: true\n    roles: []\n",
    )
    .unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "Initial source"]);

    let pm_dir = dir.path().join("pm");
    let pm = Pm::init(&pm_dir).unwrap();
    write::project_add(
        &pm,
        "cadence",
        "CAD",
        &[repo.to_string_lossy().into_owned()],
        &[],
        &[],
        None,
    )
    .unwrap();
    drop(pm);
    let opts = ServeOptions {
        test_seam: true,
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let draft = "---\ntitle: Deliver capability\ngoal: Deliver the scoped capability\nintake:\n  constraints: [Preserve integration]\n  success_criteria: [Capability available]\n  unknowns: []\n---\n\n## Implement capability\nagent: worker-1\nsources: scope\npaths: src/module.rs\n\n### Acceptance\n- [ ] Capability is available\n";
    let proposal = scoped(Asserted::Operator, || {
        shared.rpc_plan_propose(
            &json!({"project": "cadence", "text": draft}),
            std::process::id(),
        )
    })
    .unwrap();
    let epic = proposal["epic"].as_str().unwrap();
    let issue = proposal["tickets"][0].as_str().unwrap();
    scoped(Asserted::Operator, || {
        shared.rpc_plan_decide(&json!({"epic": epic}), std::process::id(), true)
    })
    .unwrap();
    register(&shared, dir.path(), "worker-1");

    let lane = repo.join(".cadence/wt/acceptance");
    std::fs::create_dir_all(lane.parent().unwrap()).unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-qb",
            "acceptance-lane",
            lane.to_str().unwrap(),
        ],
    );
    let pm = Pm::at(&pm_dir).unwrap();
    let (_, issue_dir) = write::issue_dir(&pm, issue).unwrap();
    let (mut front, body) = write::load_front(&issue_dir).unwrap();
    plan::gate(&pm.dir, &front, &body).unwrap();
    front.status = "doing".into();
    front.owner = Some("worker-1".into());
    front.claim = Some(crate::issue::claim::new_claim("worker-1", None));
    front.refs.push(crate::issue::model::Ref {
        kind: "worktree".into(),
        url: None,
        path: Some(lane.to_string_lossy().into_owned()),
        label: None,
        closed: None,
        worktree: None,
        cargo_target: None,
        agent: None,
    });
    write::save_front(&issue_dir, &front, &body).unwrap();
    git(&pm_dir, &["add", "."]);
    git(&pm_dir, &["commit", "-qm", "Record worker lane and claim"]);
    let (project, _) = write::issue_dir(&pm, issue).unwrap();
    assert_eq!(
        shared
            .dispatch_lane("dispatch send", &project, &front)
            .unwrap(),
        lane.canonicalize().unwrap()
    );

    std::fs::write(repo.join("docs/scope.md"), "Changed scope\n").unwrap();
    git(&repo, &["add", "docs/scope.md"]);
    git(&repo, &["commit", "-qm", "Change cited scope"]);
    let issue_file = issue_dir.join("issue.md");
    let before_issue = std::fs::read(&issue_file).unwrap();
    let before_queued = shared.store.queued_count("worker-1").unwrap();
    let before_delivery = serde_json::to_value(crate::delivery::load(dir.path()).unwrap()).unwrap();
    let message = "cad224freshdispatch00000000000001";
    assert!(!shared.store.message_exists(message).unwrap());
    let result = scoped(Asserted::Operator, || {
        shared.rpc_dispatch_send(
            &json!({
                "alias": "worker-1",
                "issue": issue,
                "text": "Begin the approved work",
                "reply_to": "worker-1",
                "message": message,
            }),
            std::process::id(),
        )
    });
    let refusal = result.unwrap_err().to_string();
    assert!(refusal.contains("plan_evidence_stale"), "{refusal}");
    assert!(!shared.store.message_exists(message).unwrap());
    assert_eq!(
        shared.store.queued_count("worker-1").unwrap(),
        before_queued
    );
    assert_eq!(
        serde_json::to_value(crate::delivery::load(dir.path()).unwrap()).unwrap(),
        before_delivery
    );
    assert_eq!(std::fs::read(&issue_file).unwrap(), before_issue);
}
