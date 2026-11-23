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
    opts.provider_env.set("CADENCE_DEVIN_API_KEY", "");
    opts.provider_env.set("CADENCE_DEVIN_ORG_ID", "");
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
    shared
        .store
        .register_agent(&NewAgent {
            alias: "worker-1",
            provider: "devin",
            endpoint_kind: "cloud",
            role: "worker",
            cwd: dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    // Take the real actor's join handle while its lifecycle lock is held;
    // completion is deterministic and cannot race the fence assertion.
    let actor = {
        let mut lifecycle = shared.lifecycle.lock().unwrap();
        assert!(shared
            .start_actor_locked(&mut lifecycle, "worker-1", false)
            .unwrap());
        let handle = lifecycle
            .agents
            .get("worker-1")
            .unwrap()
            .thread
            .lock()
            .unwrap()
            .take()
            .unwrap();
        handle
    };
    actor.join().unwrap();
    let failed = shared.store.agent("worker-1").unwrap();
    assert_eq!(failed.state, "attention");
    assert!(
        failed
            .error
            .as_deref()
            .unwrap_or("")
            .contains("DEVIN_API_KEY"),
        "{failed:?}"
    );
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
    let worker = shared.store.agent("worker-1").unwrap();
    assert_eq!(worker.state, "attention");
    assert!(
        worker
            .error
            .as_deref()
            .unwrap_or("")
            .contains("DEVIN_API_KEY"),
        "{worker:?}"
    );
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
    git(
        &pm_dir,
        &[
            "-c",
            "user.name=Acceptance",
            "-c",
            "user.email=acceptance@example.invalid",
            "commit",
            "-qm",
            "Record worker lane and claim",
        ],
    );
    let (project, _) = write::issue_dir(&pm, issue).unwrap();
    assert_eq!(
        shared
            .dispatch_lane("dispatch send", &project, &front)
            .unwrap(),
        lane.canonicalize().unwrap()
    );

    let scope = plan::scope_digest(&front, &body).unwrap();
    let accepted_message = "cad224accepteddispatch0000000001";
    let accepted_request = json!({
        "alias": "worker-1",
        "issue": issue,
        "text": "Begin the approved work",
        "reply_to": "worker-1",
        "message": accepted_message,
        "intake_revision": epic,
        "intake_scope": scope,
    });
    let accepted = scoped(Asserted::Operator, || {
        shared.rpc_dispatch_send(&accepted_request, std::process::id())
    })
    .unwrap();
    assert_eq!(accepted["duplicate"], false, "{accepted}");
    assert!(shared.store.message_exists(accepted_message).unwrap());
    let accepted_queue = shared.store.queued_count("worker-1").unwrap();
    assert_eq!(accepted_queue, 1);

    let spec = dir.path().join("intake-job.md");
    std::fs::write(&spec, "Deliver the approved capability.\n").unwrap();
    shared
        .store
        .create_job(
            "cad224-job",
            None,
            spec.to_str().unwrap(),
            &"0".repeat(64),
            "worker-1",
            Some(issue),
            Some(repo.to_str().unwrap()),
            None,
            2,
            None,
            Some("Deliver the capability"),
            Some(lane.to_str().unwrap()),
            None,
            None,
            Some("worker-1"),
            Some("Capability is available"),
        )
        .unwrap();
    shared
        .store
        .create_task(
            "cad224-job",
            "cad224-fresh-task",
            Some("Fresh work"),
            Some("worker-1"),
            None,
            Some("Capability is available"),
            None,
            None,
            None,
        )
        .unwrap();
    shared
        .store
        .register_monitor(
            "cad224-monitor",
            repo.to_str().unwrap(),
            "worker-1",
            60,
            &["cad224-job-t1".to_string(), "cad224-fresh-task".to_string()],
            true,
            true,
        )
        .unwrap();
    let job_request =
        json!({"task": "cad224-job-t1", "by": "worker-1", "message": "cad224-job-message"});
    let accepted_job = shared.rpc_task_dispatch(&job_request).unwrap();
    assert_eq!(accepted_job["duplicate"], false, "{accepted_job}");
    let queue_with_job = shared.store.queued_count("worker-1").unwrap();
    assert_eq!(queue_with_job, accepted_queue + 1);

    std::fs::write(repo.join("docs/scope.md"), "Changed scope\n").unwrap();
    git(&repo, &["add", "docs/scope.md"]);
    git(&repo, &["commit", "-qm", "Change cited scope"]);
    let issue_file = issue_dir.join("issue.md");
    let before_issue = std::fs::read(&issue_file).unwrap();
    let before_queued = shared.store.queued_count("worker-1").unwrap();
    let before_delivery = serde_json::to_value(crate::delivery::load(dir.path()).unwrap()).unwrap();
    let before_head = git(&pm_dir, &["rev-parse", "HEAD"]);
    let before_lanes = git(&repo, &["worktree", "list", "--porcelain"]);
    let replay = scoped(Asserted::Operator, || {
        shared.rpc_dispatch_send(&accepted_request, std::process::id())
    })
    .unwrap();
    assert_eq!(replay["duplicate"], true, "{replay}");
    assert_eq!(replay["message"], accepted_message);
    assert_eq!(
        shared.store.queued_count("worker-1").unwrap(),
        queue_with_job
    );
    let replay_job = shared.rpc_task_dispatch(&job_request).unwrap();
    assert_eq!(replay_job["duplicate"], true, "{replay_job}");
    assert_eq!(replay_job["message"], accepted_job["message"]);
    let replay_monitor = scoped(Asserted::Operator, || {
        shared.rpc_monitor_dispatch(
            &json!({"monitor": "cad224-monitor", "task": "cad224-job-t1"}),
            std::process::id(),
        )
    })
    .unwrap();
    assert_eq!(replay_monitor["duplicate"], true, "{replay_monitor}");
    assert_eq!(replay_monitor["message"], accepted_job["message"]);
    let replay_automatic = shared
        .monitor_dispatch_task("cad224-monitor", "cad224-job-t1", true)
        .unwrap();
    assert_eq!(replay_automatic["duplicate"], true, "{replay_automatic}");
    assert_eq!(replay_automatic["message"], accepted_job["message"]);
    let before_fresh_task = shared.store.task("cad224-fresh-task").unwrap().to_json();
    for automatic in [false, true] {
        let refusal = shared
            .monitor_dispatch_task("cad224-monitor", "cad224-fresh-task", automatic)
            .unwrap_err();
        assert_eq!(refusal.code(), Some("plan_evidence_stale"), "{refusal:?}");
        assert!(shared
            .store
            .messages_for_task("cad224-fresh-task")
            .unwrap()
            .is_empty());
    }
    assert_eq!(
        shared.store.task("cad224-fresh-task").unwrap().to_json(),
        before_fresh_task,
    );
    let refusal = shared
        .rpc_task_dispatch(&json!({
            "task": "cad224-fresh-task", "by": "worker-1", "message": "cad224-fresh-job"
        }))
        .unwrap_err();
    assert_eq!(refusal.code(), Some("plan_evidence_stale"), "{refusal:?}");
    assert!(!shared.store.message_exists("cad224-fresh-job").unwrap());
    assert_eq!(
        shared.store.task("cad224-fresh-task").unwrap().to_json(),
        before_fresh_task,
    );
    assert_eq!(
        shared.store.queued_count("worker-1").unwrap(),
        queue_with_job
    );
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
                "intake_revision": epic,
                "intake_scope": scope,
            }),
            std::process::id(),
        )
    });
    let refusal = result.unwrap_err();
    assert_eq!(refusal.code(), Some("plan_evidence_stale"), "{refusal}");
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
    assert_eq!(git(&pm_dir, &["rev-parse", "HEAD"]), before_head);
    assert_eq!(
        git(&repo, &["worktree", "list", "--porcelain"]),
        before_lanes
    );
}
