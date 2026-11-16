//! CAD-2215 independent real-guard delivery provenance acceptance.
//! The implementer must not edit or weaken this check.
//!
//! Positive provenance is exercised only on an ORDINARY (non-member) job;
//! the member-private dispatch path is a NEGATIVE: it must never mint or keep
//! shared ordinary delivery. Every effect goes through a production function
//! (`task_dispatch`, `delivery_start`, `require_ordinary`, `delivery_adopt`,
//! `route_delivery`, `task_report::file`); no arbitrary receipt string is
//! trusted and nothing reads GitHub.

use super::*;

#[test]
fn ordinary_delivery_requires_a_genuine_dispatch_and_never_absorbs_private_work() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-prov");
    with_deferred_service(&fx);
    // Join the actual worker to the member assistant's group; the real
    // private dispatch must pass the existing group guard, not bypass it.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET params=? WHERE alias='ordinary-worker'",
                rusqlite::params![json!({"upstream":rt.alias}).to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    std::fs::write(
        fx.pm().join("fixture/project.yaml"),
        "key: fixture\nprefix: FIX\nrepos:\n  - remote: https://github.com/acme/fixture\n",
    )
    .unwrap();
    let records = || crate::delivery::records(fx.dir.path());

    // ---------- NEGATIVE: a promoted member-private dispatch ----------
    let req = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope("sub-prov"),
            "title": "private provenance job",
            "request_key": "prov-private",
            "body": "b",
            "issue": FIXTURE_ISSUE,
        }),
    );
    let private_job = promote_member_fixture_job(&fx, "sub-prov", &req);
    assert_eq!(
        fx.shared.store.job(&private_job).unwrap().pm_alias,
        rt.alias,
        "fixture worker joined the wrong PM group"
    );
    let private_task = fx
        .shared
        .store
        .tasks_for_job(&private_job)
        .unwrap()
        .first()
        .expect("promoted private job has no draft task")
        .id
        .clone();
    let private_dispatch = fx.op(
        "task_dispatch",
        json!({"task": private_task, "to": rt.worker, "by": "operator"}),
    )["message"]
        .as_str()
        .expect("private dispatch answered no kickoff id")
        .to_string();
    // The exact private kickoff may or may not open a record; either way it
    // must not enter shared ordinary delivery.
    let _ = fx
        .shared
        .delivery_start(FIXTURE_ISSUE, "fixture", &rt.worker, &private_dispatch);
    let all = records();
    assert!(
        crate::delivery::ordinary_records(&all)
            .iter()
            .all(|r| r.issue != FIXTURE_ISSUE),
        "a promoted member-private dispatch entered shared ordinary delivery: {all:?}"
    );
    for rec in all.values().filter(|r| r.issue == FIXTURE_ISSUE) {
        assert!(
            fx.shared.require_ordinary(rec, "routing").is_err(),
            "a member-private dispatch passed the ordinary revalidation"
        );
    }
    let _ = fx.shared.route_delivery().unwrap();
    assert!(
        crate::delivery::ordinary_records(&records())
            .iter()
            .all(|r| r.issue != FIXTURE_ISSUE),
        "routing promoted member-private work into shared ordinary delivery"
    );

    // ---------- POSITIVE: a real ordinary (non-member) dispatch ----------
    // A disposable git repo + worktree is the real lane the production
    // dispatch binds; the project declares it and FIX-2 records it open.
    let repo = fx.dir.path().join("ordinary-delivery-repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "fixture git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "cadence@example.invalid"]);
    git(&["config", "user.name", "cadence"]);
    std::fs::write(repo.join("README"), "fixture\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "fixture"]);
    let lane = crate::worktree::layout::worktrees_dir(&repo).join("fix-2");
    std::fs::create_dir_all(lane.parent().unwrap()).unwrap();
    git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "fix-2",
        lane.to_str().unwrap(),
    ]);
    let lane = std::fs::canonicalize(&lane).unwrap();
    std::fs::write(
        fx.pm().join("fixture/project.yaml"),
        format!("key: fixture\nprefix: FIX\nrepos:\n  - path: {}\n    remote: https://github.com/acme/fixture\n", repo.display()),
    ).unwrap();
    assert!(
        crate::delivery::project_pr_refusal(
            &fx.pm(),
            "fixture",
            "https://github.com/acme/fixture/pull/7"
        )
        .unwrap()
        .is_none(),
        "fixture project does not admit its own synthetic PR remote"
    );
    std::fs::write(
        fx.pm().join("fixture/FIX-2/issue.md"),
        format!(
            "---\nid: FIX-2\ntitle: Ordinary plain\nstatus: backlog\npriority: P2\n\
             created: '2026-01-01T00:00:00Z'\nrefs:\n  - kind: worktree\n    path: {}\n---\n",
            lane.display()
        ),
    )
    .unwrap();
    let db = rusqlite::Connection::open(fx.dir.path().join("cadence.sqlite3")).unwrap();
    db.execute_batch(&format!(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,repo,state,created,updated)
         VALUES('ordinary-prov-job','Ordinary','spec.md','ordinary-worker','FIX-2','fixture','open',1,1);
         INSERT INTO tasks(id,job_id,title,assignee,state,worktree,created,updated)
         VALUES('ordinary-prov-task','ordinary-prov-job','Ordinary task','ordinary-worker','draft','{}',1,1);",
        lane.display()
    ))
    .unwrap();
    let ordinary_dispatch = fx.op(
        "task_dispatch",
        json!({"task": "ordinary-prov-task", "to": rt.worker, "by": "operator"}),
    )["message"]
        .as_str()
        .expect("ordinary dispatch answered no kickoff id")
        .to_string();
    let kickoff = fx
        .shared
        .store
        .message(&ordinary_dispatch)
        .unwrap()
        .expect("ordinary dispatch row missing");
    assert_eq!(
        kickoff.source, "job_dispatch",
        "task dispatch producer contract changed"
    );
    assert_eq!(kickoff.task_id.as_deref(), Some("ordinary-prov-task"));
    assert_eq!(kickoff.issue.as_deref(), Some("FIX-2"));
    assert_eq!(kickoff.alias, rt.worker);
    assert_eq!(
        kickoff.worktree.as_deref(),
        Some(lane.to_str().unwrap()),
        "the ordinary dispatch bound the wrong lane: {kickoff:?}"
    );

    // Refusals on the ordinary row.
    assert!(
        fx.shared
            .delivery_start("FIX-3", "fixture", &rt.worker, "m-arbitrary")
            .is_err(),
        "an arbitrary string opened a delivery record"
    );
    fx.shared
        .store
        .enqueue_task(
            &rt.worker,
            "user body",
            None,
            "m-user",
            "user",
            Some("ordinary-prov-task"),
        )
        .unwrap();
    assert!(
        fx.shared
            .delivery_start("FIX-3", "fixture", &rt.worker, "m-user")
            .is_err(),
        "a plain user message passed as a dispatch receipt"
    );
    assert!(
        fx.shared
            .delivery_start("FIX-3", "fixture", &rt.worker, &ordinary_dispatch)
            .is_err(),
        "the genuine receipt bound a different issue"
    );
    assert!(
        fx.shared
            .delivery_start("FIX-2", "fixture", "someone-else", &ordinary_dispatch)
            .is_err(),
        "the genuine receipt bound a different worker"
    );
    let forged =
        crate::delivery::Record::new_ordinary("FIX-4", "fixture", &rt.worker, 1, "m-arbitrary");
    assert!(
        fx.shared.require_ordinary(&forged, "routing").is_err(),
        "an arbitrary receipt passed the ordinary revalidation"
    );

    // The genuine ordinary receipt opens and revalidates.
    fx.shared
        .delivery_start("FIX-2", "fixture", &rt.worker, &ordinary_dispatch)
        .unwrap();
    let all = records();
    let rec = all
        .get("FIX-2")
        .expect("ordinary dispatch opened no record");
    assert_eq!(
        rec.origin.verified_ordinary(),
        Some(ordinary_dispatch.as_str())
    );
    assert!(crate::delivery::ordinary_records(&all)
        .iter()
        .any(|r| r.issue == "FIX-2"));
    assert!(fx.shared.require_ordinary(rec, "routing").is_ok());

    // An unrelated report stays legacy and is never routed.
    fx.shared
        .delivery_adopt("FIX-5", "fixture", &rt.worker, 1, "unrelated-report.md")
        .unwrap();
    let legacy_key = crate::delivery::legacy_key("FIX-5", "unrelated-report.md");
    let all = records();
    let legacy = all
        .get(&legacy_key)
        .expect("adoption stored no legacy record");
    assert!(!legacy.origin.is_verified_ordinary());
    assert!(crate::delivery::ordinary_records(&all)
        .iter()
        .all(|r| r.issue != "FIX-5"));
    assert!(crate::delivery::legacy_records(&all)
        .iter()
        .any(|r| r.issue == "FIX-5"));
    assert!(fx.shared.require_ordinary(legacy, "routing").is_err());
    let moved = fx.shared.route_delivery().unwrap();
    assert_eq!(
        records()[&legacy_key].state,
        crate::delivery::State::Working,
        "a legacy candidate was routed (moved {moved})"
    );

    // The receipt completed with the same report path/head routes.
    let pm = crate::issue::Pm::at(&fx.pm()).unwrap();
    let prepared = crate::issue::task_report::prepare_as(
        &pm,
        "---\nsha: 0123456789abcdef0123456789abcdef01234567\npr: https://github.com/acme/fixture/pull/7\n---\n## Expected\nSynthetic expected result.\n\n## Evidence\nSynthetic fixture evidence.\n\n## Cause\nSynthetic fixture cause.\n\n## Correction\nSynthetic fixture correction.\n\n## Lesson\nSynthetic fixture lesson.\n\n## Next\nSynthetic fixture next action.\n",
        Some("FIX-2"),
        Some(crate::issue::task_report::Kind::Done),
        &rt.worker,
    )
    .unwrap();
    assert_eq!(
        prepared.front.agent.as_deref(),
        Some(rt.worker.as_str()),
        "worker-bound report author was not prepared"
    );
    crate::issue::task_report::store(&pm, &prepared, &rt.worker).unwrap();
    let issue_dir = fx.pm().join("fixture").join("FIX-2");
    let rows = crate::issue::task_report::list(&issue_dir, "FIX-2");
    let report = rows
        .iter()
        .find(|r| r["kind"] == "done" && r["agent"].as_str() == Some(rt.worker.as_str()))
        .expect("filed done report missing");
    let report_path = report["path"].as_str().unwrap().to_string();
    db.execute(
        "UPDATE messages SET state='completed', result=? WHERE id=?",
        [
            json!({"text": format!("Report: {report_path}\nSHA: 0123456789abcdef0123456789abcdef01234567")}).to_string(),
            ordinary_dispatch.clone(),
        ],
    )
    .unwrap();
    let moved = fx.shared.route_delivery().unwrap();
    assert!(moved >= 1, "the matching receipt/report did not route");
    let rec = records()
        .remove("FIX-2")
        .expect("routed record disappeared");
    assert_ne!(
        rec.state,
        crate::delivery::State::Working,
        "the matching report left the loop in Working: {rec:?}"
    );
}
