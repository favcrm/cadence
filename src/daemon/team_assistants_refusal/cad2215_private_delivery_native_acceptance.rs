//! CAD-2215 independent native-guard acceptance. The product implementer
//! must not weaken this test. No remote publication or GitHub is involved.
use super::*;

#[test]
fn private_delivery_requires_verified_operator_and_explicit_bound_selection() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-delivery-acceptance");
    with_deferred_service(&fx);
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
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope":fx.member_envelope("sub-delivery-acceptance"),
            "title":"private delivery acceptance", "request_key":"private-delivery-acceptance",
            "issue":FIXTURE_ISSUE, "body":"work privately"
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-delivery-acceptance", &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let dispatch = fx.op(
        "task_dispatch",
        json!({"task":task,"to":rt.worker,"by":"operator"}),
    );
    let message = dispatch["message"]
        .as_str()
        .expect("real dispatch has no message")
        .to_string();
    let msg = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .expect("dispatch message missing");
    // Production finish records the actual dispatched worker result; the
    // independent operator verdict exercises the daemon's result binding.
    let sha = "1234567890abcdef1234567890abcdef12345678";
    let marker = "private-delivery-selected-result-2215";
    fx.shared
        .store
        .finish(
            &msg,
            "completed",
            &json!({"status":"completed","sha":sha,"text":marker}),
            None,
        )
        .unwrap();
    fx.op(
        "task_verdict",
        json!({"task":task,"sha":sha,"verdict":"pass"}),
    );

    let listing = fx.op("delivery_private_list", json!({}));
    let rows = listing["deliveries"]
        .as_array()
        .expect("missing private deliveries");
    let row = rows
        .iter()
        .find(|d| d["origin"]["job"] == job)
        .expect("production job delivery not recorded");
    assert_eq!(row["origin"]["task"], task);
    assert_eq!(row["origin"]["issue"], FIXTURE_ISSUE);
    assert_eq!(row["origin"]["project"], "fixture");
    assert_eq!(row["origin"]["owner_sub"], "sub-delivery-acceptance");
    assert_eq!(row["state"], "completed");
    let delivery = row["id"].as_str().expect("delivery has no id").to_string();
    assert!(
        row["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["message"] == message && r["body"] == marker),
        "no native result: {row}"
    );

    // An unrelated legacy candidate is operator recovery, never shared.
    fx.shared
        .delivery_adopt(
            "FIX-5",
            "fixture",
            &rt.worker,
            1,
            "ambiguous-private-report.md",
        )
        .unwrap();
    let shared = fx.op("delivery_list", json!({}));
    assert!(
        !shared.to_string().contains("ambiguous-private-report.md"),
        "legacy leaked in shared list: {shared}"
    );
    let legacy = fx.op("delivery_legacy_list", json!({}));
    assert!(
        legacy.to_string().contains("ambiguous-private-report.md"),
        "operator recovery omitted legacy: {legacy}"
    );

    let agent = || Asserted::Agent(rt.worker.clone());
    let agent_rows = fx.call(agent(), "delivery_private_list", json!({}));
    if let Ok(v) = agent_rows {
        assert_eq!(
            v["deliveries"],
            json!([]),
            "agent observed private delivery: {v}"
        );
    }
    assert!(
        fx.call(agent(), "delivery_legacy_list", json!({})).is_err(),
        "agent obtained legacy recovery"
    );
    let args =
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[message]});
    let before_tree = fx.tree();
    let before_ordinary = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let before_private = std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap();
    assert!(
        fx.call(agent(), "delivery_publish", args.clone()).is_err(),
        "agent published private delivery"
    );
    for (field, value) in [
        ("public_session", json!(true)),
        (
            "member_envelope",
            json!(fx.member_envelope("sub-delivery-acceptance")),
        ),
        ("actor", json!("operator")),
        ("owner", json!("sub-delivery-acceptance")),
        ("explicit", json!(true)),
    ] {
        let mut forged = args.clone();
        forged[field] = value.clone();
        assert!(
            fx.op_err("delivery_publish", forged.clone()).is_err(),
            "operator-transport relay forged {field}"
        );
        let mut forged_read = json!({});
        forged_read[field] = value.clone();
        assert!(
            fx.op_err("delivery_private_list", forged_read.clone())
                .is_err(),
            "operator-transport relay forged private list via {field}"
        );
        assert!(
            fx.op_err("delivery_legacy_list", forged_read).is_err(),
            "operator-transport relay forged legacy list via {field}"
        );
        assert!(
            fx.call(agent(), "delivery_publish", forged).is_err(),
            "agent forged {field}"
        );
        let mut query = json!({});
        query[field] = value;
        let read = fx.call(agent(), "delivery_private_list", query.clone());
        if let Ok(v) = read {
            assert_eq!(
                v["deliveries"],
                json!([]),
                "agent forged list identity via {field}: {v}"
            );
        }
        assert!(
            fx.call(agent(), "delivery_legacy_list", query).is_err(),
            "agent forged legacy identity via {field}"
        );
    }
    for bad in [
        json!({"delivery":delivery,"issue":"FIX-2","project":"fixture","results":[message]}),
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"wrong-project","results":[message]}),
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[]}),
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[message,message]}),
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":["wrong-result"]}),
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[message],"digest":"wrong-digest"}),
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[message],"target":"pr"}),
    ] {
        assert!(
            fx.op_err("delivery_publish", bad.clone()).is_err(),
            "operator invalid selection published: {bad}"
        );
    }
    assert_eq!(
        fx.tree(),
        before_tree,
        "refused publication changed shared issue or PR state"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        before_ordinary,
        "refusal changed ordinary PR loop"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap(),
        before_private,
        "refusal changed private ledger"
    );

    // Both real operator calls must reach the atomic claim before either
    // proceeds. The fixture-scoped seam fires outside the ledger lock.
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel::<()>();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let hook_release = Arc::clone(&release);
    let hook = Arc::new(move || {
        arrived_tx.send(()).expect("race observer receiver gone");
        let (lock, cv) = &*hook_release;
        let guard = lock.lock().unwrap();
        drop(cv.wait_while(guard, |released| !*released).unwrap());
    });
    let seam = crate::delivery::publication::arm_publication_race(fx.dir.path(), hook);
    let (first, second, arrivals) = std::thread::scope(|scope| {
        let spawn_call = || {
            let shared = Arc::clone(&fx.shared);
            let params = args.clone();
            scope.spawn(move || {
                scoped(Asserted::Operator, || {
                    shared.dispatch("delivery_publish", &params, std::process::id())
                })
            })
        };
        let a = spawn_call();
        let b = spawn_call();
        // Always release and join even if an earlier guard prevents arrival;
        // the timeout is a failure bound, not a scheduling sleep.
        let arrived_a = arrived_rx.recv_timeout(Duration::from_secs(10)).is_ok();
        let arrived_b = arrived_rx.recv_timeout(Duration::from_secs(10)).is_ok();
        let (lock, cv) = &*release;
        *lock.lock().unwrap() = true;
        cv.notify_all();
        (
            a.join().expect("first publish panicked"),
            b.join().expect("second publish panicked"),
            (arrived_a, arrived_b),
        )
    });
    drop(seam);
    assert_eq!(
        arrivals,
        (true, true),
        "both operator calls must reach the pre-claim seam"
    );
    assert_ne!(
        first.is_ok(),
        second.is_ok(),
        "exactly one simultaneous publication must succeed: first={first:?}, second={second:?}"
    );
    let receipt = first.or(second).expect("no successful publication");
    assert_eq!(receipt["delivery"], delivery);
    let ledger = crate::delivery::private::load(fx.dir.path()).unwrap();
    let stored = ledger.get(&delivery).expect("published delivery missing");
    assert_eq!(
        stored.publications.len(),
        1,
        "duplicate or missing publication receipts"
    );
    let published = &stored.publications[0];
    assert_eq!(published.delivery_id, delivery);
    assert_eq!(published.issue, FIXTURE_ISSUE);
    assert_eq!(published.project, "fixture");
    assert_eq!(published.actor, "operator");
    assert_eq!(published.results, vec![message.clone()]);
    assert!(
        matches!(
            &published.outcome,
            crate::delivery::publication::PublicationState::Published { .. }
        ),
        "receipt did not settle: {published:?}"
    );
    let issue = crate::issue::board::find_issue(&fx.pm(), FIXTURE_ISSUE).unwrap();
    assert_eq!(
        issue
            .comments
            .iter()
            .filter(|c| c.body.contains(marker))
            .count(),
        1,
        "selected result not published exactly once"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        before_ordinary,
        "publication changed ordinary PR loop"
    );
    let published_tree = fx.tree();
    let published_private = std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap();
    assert!(
        fx.op_err("delivery_publish", args).is_err(),
        "same result published twice"
    );
    assert_eq!(
        fx.tree(),
        published_tree,
        "duplicate publication changed issue state"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap(),
        published_private,
        "duplicate publication changed receipt"
    );
}

#[test]
fn ordinary_delivery_list_obeys_current_project_scope_and_operator_recovers() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-project-reader");
    let pm_dir = fx.dir.path().join("scope-ordinary-pm");
    std::fs::create_dir_all(&pm_dir).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "scope-ordinary-pm",
            provider: "pi",
            endpoint_kind: "managed",
            role: "pm",
            cwd: pm_dir.to_str().unwrap(),
            sandbox: "workspace-write",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET params=? WHERE alias='ordinary-worker'",
                rusqlite::params![json!({"upstream":"scope-ordinary-pm"}).to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    // Two real tracker projects. The worker/reader sees VIS but is not
    // admitted to SEC. No fabricated delivery provenance is written.
    for (project, prefix, policy) in [
        ("visible", "VIS", ""),
        (
            "secret",
            "SEC",
            "policy:\n  version: 1\n  visibility: private\n",
        ),
    ] {
        let dir = fx.pm().join(project);
        std::fs::create_dir_all(dir.join(format!("{prefix}-1"))).unwrap();
        std::fs::write(dir.join("project.yaml"), format!(
            "key: {project}\nprefix: {prefix}\nrepos:\n  - remote: https://github.com/acme/{project}\n{policy}"
        )).unwrap();
        std::fs::write(dir.join(format!("{prefix}-1/issue.md")), format!(
            "---\nid: {prefix}-1\ntitle: {prefix} ordinary issue\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"
        )).unwrap();
    }
    let reader = format!("agents/{}", rt.worker);
    let scope = fx
        .shared
        .project_scope_for_actor(&reader)
        .unwrap()
        .expect("agent is not the operator");
    assert!(
        scope.0.contains("visible"),
        "fixture reader must read visible project"
    );
    assert!(
        !scope.0.contains("secret"),
        "fixture reader unexpectedly reads secret project"
    );
    let conn = rusqlite::Connection::open(fx.dir.path().join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,repo,state,created,updated)
         VALUES('scope-vis-job','Visible ordinary','spec.md','scope-ordinary-pm','VIS-1','visible','open',1,1),
               ('scope-sec-job','Secret ordinary','spec.md','scope-ordinary-pm','SEC-1','secret','open',1,1);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
         VALUES('scope-vis-task','scope-vis-job','Visible task','ordinary-worker','draft',1,1),
               ('scope-sec-task','scope-sec-job','Secret task','ordinary-worker','draft',1,1);"
    ).unwrap();
    // The real dispatcher copies the task's assigned lane into its
    // receipt; ordinary provenance requires a real nonempty worktree.
    let lane = fx.dir.path().join("scope-delivery-worktree");
    std::fs::create_dir_all(&lane).unwrap();
    r5_git(&lane, &["init", "-q"]);
    let lane = lane.to_str().unwrap();
    assert_eq!(
        conn.execute(
            "UPDATE jobs SET repo=?1 WHERE id IN ('scope-vis-job','scope-sec-job')",
            [lane]
        )
        .unwrap(),
        2
    );
    assert_eq!(
        conn.execute(
            "UPDATE tasks SET worktree=?1 WHERE id IN ('scope-vis-task','scope-sec-task')",
            [lane]
        )
        .unwrap(),
        2
    );
    for (issue, project, task) in [
        ("VIS-1", "visible", "scope-vis-task"),
        ("SEC-1", "secret", "scope-sec-task"),
    ] {
        let dispatch = fx.op(
            "task_dispatch",
            json!({"task":task,"to":rt.worker,"by":"operator"}),
        );
        let message = dispatch["message"]
            .as_str()
            .expect("native dispatch produced no receipt");
        let native_receipt = fx
            .shared
            .store
            .message(message)
            .unwrap()
            .expect("native receipt missing");
        assert_eq!(native_receipt.source, "job_dispatch");
        assert_eq!(native_receipt.task_id.as_deref(), Some(task));
        assert_eq!(native_receipt.issue.as_deref(), Some(issue));
        assert_eq!(native_receipt.alias, rt.worker);
        assert_eq!(native_receipt.worktree.as_deref(), Some(lane));
        fx.shared
            .delivery_start(issue, project, &rt.worker, message)
            .expect("genuine ordinary dispatch failed provenance validation");
        let records = crate::delivery::records(fx.dir.path());
        let record = records.get(issue).expect("ordinary record missing");
        assert_eq!(record.origin.verified_ordinary(), Some(message));
        fx.shared
            .require_ordinary(record, "acceptance read")
            .expect("ordinary origin did not revalidate against native dispatch");
    }
    let pid = std::process::id();
    scoped(Asserted::Agent(rt.worker.clone()), || {
        assert!(matches!(
            fx.shared.agent_caller(pid, "delivery scope acceptance").unwrap(),
            AgentCaller::Agent(alias) if alias == rt.worker
        ))
    });
    let agent = fx
        .call(
            Asserted::Agent(rt.worker.clone()),
            "delivery_list",
            json!({}),
        )
        .expect("authorized agent cannot list ordinary visible deliveries");
    let rows = agent["records"]
        .as_array()
        .expect("delivery list has no records");
    assert!(
        rows.iter().any(|r| r["issue"] == "VIS-1"),
        "visible positive control lost: {agent}"
    );
    assert!(
        !agent.to_string().contains("SEC-1"),
        "agent learned private delivery: {agent}"
    );
    assert!(
        !agent.to_string().contains("scope-sec"),
        "agent learned private provenance: {agent}"
    );

    // No response-scoped ACK contract exists: even a proven operator
    // transport must not turn a claimed relay identity into an unscoped read.
    for claim in [
        json!({"read_as":"users/sub-project-reader"}),
        json!({"public_session":"forged-session"}),
        json!({"cli_envelope":"forged-cli-envelope"}),
    ] {
        assert!(
            fx.op_err("delivery_list", claim.clone()).is_err(),
            "operator transport accepted relay proof: {claim}"
        );
        assert!(
            fx.call(
                Asserted::Agent(rt.worker.clone()),
                "delivery_list",
                claim.clone()
            )
            .is_err(),
            "agent supplied relay proof: {claim}"
        );
    }
    // Policy changes are re-read for native callers; no relayed principal
    // or timing race is synthesized here.
    std::fs::write(fx.pm().join("visible/project.yaml"),
        "key: visible\nprefix: VIS\nrepos:\n  - remote: https://github.com/acme/visible\npolicy:\n  version: 1\n  visibility: private\n").unwrap();
    let revoked = fx
        .call(
            Asserted::Agent(rt.worker.clone()),
            "delivery_list",
            json!({}),
        )
        .expect("native reader scope after revocation");
    assert!(
        !revoked.to_string().contains("VIS-1"),
        "revoked agent retained visible delivery: {revoked}"
    );
    assert!(
        !revoked.to_string().contains("SEC-1"),
        "revoked agent learned private delivery: {revoked}"
    );
    let operator = fx.op("delivery_list", json!({}));
    let rows = operator["records"]
        .as_array()
        .expect("operator list has no records");
    for issue in ["VIS-1", "SEC-1"] {
        assert!(
            rows.iter().any(|r| r["issue"] == issue),
            "operator lost {issue}: {operator}"
        );
    }
}

#[test]
fn private_ledger_rejects_symlink_and_real_dispatch_replaces_permissive_mode() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-ledger-io");
    with_deferred_service(&fx);
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

    // Both paths are inside this fixture's own TempDir. The sentinel is a
    // valid empty ledger too: a reader that follows the symlink would
    // succeed, not accidentally pass by rejecting malformed JSON.
    let target = fx.dir.path().join("ledger-target-sentinel.json");
    let final_path = fx.dir.path().join("delivery-private.json");
    let sentinel = b"{\"version\":1,\"records\":{}}\n";
    std::fs::write(&target, sentinel).unwrap();
    symlink(&target, &final_path).expect("fixture planted ledger symlink");
    let denied = fx.op_err("delivery_private_list", json!({}));
    assert!(
        denied.is_err(),
        "operator read followed private ledger symlink: {denied:?}"
    );
    assert_eq!(
        std::fs::read(&target).unwrap(),
        sentinel.to_vec(),
        "refused symlink read changed target"
    );
    assert!(std::fs::symlink_metadata(&final_path)
        .unwrap()
        .file_type()
        .is_symlink());

    // Remove only this fixture's symlink and start with a real, valid but
    // deliberately permissive old ledger. A genuine member job dispatch
    // must write its own private record and replace that inode with 0600;
    // this observes the actual file mode without changing global umask.
    std::fs::remove_file(&final_path).unwrap();
    std::fs::write(&final_path, sentinel).unwrap();
    std::fs::set_permissions(&final_path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert_eq!(
        std::fs::metadata(&final_path).unwrap().permissions().mode() & 0o777,
        0o666
    );
    let receipt = fx.op(
        "member_job_request",
        json!({
            "member_envelope":fx.member_envelope("sub-ledger-io"),
            "title":"ledger io acceptance", "request_key":"ledger-io-acceptance",
            "issue":FIXTURE_ISSUE, "body":"private work"
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-ledger-io", &receipt);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    fx.op(
        "task_dispatch",
        json!({"task":task,"to":rt.worker,"by":"operator"}),
    );
    let listed = fx.op("delivery_private_list", json!({}));
    assert!(
        listed["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["origin"]["job"] == job && d["origin"]["task"] == task),
        "production dispatch did not persist a bound private ledger entry: {listed}"
    );
    let meta = std::fs::symlink_metadata(&final_path).unwrap();
    assert!(
        meta.file_type().is_file(),
        "private ledger final path is not a regular file"
    );
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o600,
        "private ledger was not owner-only"
    );
    assert_eq!(
        std::fs::read(&target).unwrap(),
        sentinel.to_vec(),
        "ledger write altered original symlink target"
    );
}

#[test]
fn private_delivery_recovers_durable_result_after_persistent_ledger_failure() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-ledger-recovery");
    with_deferred_service(&fx);
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
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope":fx.member_envelope("sub-ledger-recovery"),
            "title":"private recovery acceptance", "request_key":"ledger-recovery-acceptance",
            "issue":FIXTURE_ISSUE, "body":"private work"
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-ledger-recovery", &request);
    let origin = fx
        .shared
        .store
        .assist_request_for_job(&job)
        .unwrap()
        .expect("unique native owner request missing");
    assert_eq!(origin.job, job);
    assert_eq!(origin.sub, "sub-ledger-recovery");
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let ledger_path = fx.dir.path().join("delivery-private.json");
    // A directory is a deterministic bad final ledger path, not a global
    // permission/umask mutation. Only this fixture-owned directory is removed.
    std::fs::create_dir(&ledger_path).unwrap();
    let dispatch = fx.op(
        "task_dispatch",
        json!({"task":task,"to":rt.worker,"by":"operator"}),
    );
    assert_eq!(
        dispatch["private_delivery"]["state"], "unavailable",
        "ledger failure hidden by dispatch: {dispatch}"
    );
    let message = dispatch["message"]
        .as_str()
        .expect("durable kickoff missing")
        .to_string();
    let msg = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .expect("kickoff row missing");
    let marker = "private-recovery-result-2215";
    let sha = "1234567890abcdef1234567890abcdef12345678";
    fx.shared
        .store
        .finish(
            &msg,
            "completed",
            &json!({"status":"completed","sha":sha,"text":marker}),
            None,
        )
        .unwrap();
    let verdict = fx.op(
        "task_verdict",
        json!({"task":task,"sha":sha,"verdict":"pass"}),
    );
    assert_eq!(
        verdict["private_delivery"]["state"], "unavailable",
        "ledger failure hidden by completion: {verdict}"
    );
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    let message_before = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .unwrap()
        .to_json();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let tracker_before = fx.tree();
    assert!(
        !crate::issue::board::find_issue(&fx.pm(), FIXTURE_ISSUE)
            .unwrap()
            .comments
            .iter()
            .any(|c| c.body.contains(marker)),
        "private result reached shared issue before publication"
    );
    assert!(
        fx.op_err("delivery_private_list", json!({})).is_err(),
        "broken ledger listed as available"
    );
    let params = json!({"job":job});
    assert!(
        fx.call(
            Asserted::Agent(rt.worker.clone()),
            "delivery_private_reconcile",
            params.clone()
        )
        .is_err(),
        "agent invoked operator-only recovery"
    );
    for claim in [
        json!({"job":job,"read_as":"operator"}),
        json!({"job":job,"public_session":"forged"}),
        json!({"job":job,"member_envelope":fx.member_envelope("sub-ledger-recovery")}),
    ] {
        assert!(
            fx.op_err("delivery_private_reconcile", claim).is_err(),
            "relay claim was accepted by native recovery"
        );
    }
    // An API-level refusal or an explicit per-task unavailable result is
    // acceptable, but an unknown method or a reported successful recovery
    // while the fault persists is not.
    match fx.op_err("delivery_private_reconcile", params.clone()) {
        Err(e) => assert!(
            !e.to_string().to_lowercase().contains("unknown method"),
            "recovery entrypoint is not registered: {e}"
        ),
        Ok(value) => {
            assert_eq!(
                value["reconciled"], 0,
                "faulty storage reported recovery: {value}"
            );
            assert!(
                value["unavailable"]
                    .as_array()
                    .is_some_and(|rows| rows.iter().any(|r| r["job"] == job && r["task"] == task)),
                "missing truthful unavailable receipt: {value}"
            );
        }
    }
    assert_eq!(
        fx.tree(),
        tracker_before,
        "fault handling changed shared tracker"
    );
    std::fs::remove_dir(&ledger_path).unwrap();
    let recovered = fx.op("delivery_private_reconcile", params.clone());
    assert!(
        recovered["unavailable"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "restored ledger still unavailable: {recovered}"
    );
    assert!(
        recovered["reconciled"].as_u64().is_some_and(|n| n >= 1),
        "no durable result recovered: {recovered}"
    );
    let listed = fx.op("delivery_private_list", json!({}));
    let deliveries = listed["deliveries"].as_array().unwrap();
    let rows: Vec<_> = deliveries
        .iter()
        .filter(|d| d["origin"]["job"] == job && d["origin"]["task"] == task)
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "recovered delivery missing or duplicated: {listed}"
    );
    let row = rows[0];
    assert_eq!(row["origin"]["owner_sub"], "sub-ledger-recovery");
    assert_eq!(row["origin"]["issue"], FIXTURE_ISSUE);
    assert_eq!(row["state"], "completed");
    assert_eq!(
        row["results"].as_array().unwrap().len(),
        1,
        "result was not recovered exactly once"
    );
    assert_eq!(row["results"][0]["message"], message);
    assert_eq!(row["results"][0]["body"], marker);
    let delivery = row["id"].as_str().unwrap().to_string();
    let replay = fx.op("delivery_private_reconcile", params);
    assert!(
        replay["unavailable"].as_array().is_some_and(Vec::is_empty),
        "idempotent recovery failed: {replay}"
    );
    let after_replay = fx.op("delivery_private_list", json!({}));
    let repeated = after_replay["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == delivery)
        .unwrap();
    assert_eq!(
        repeated["results"].as_array().unwrap().len(),
        1,
        "recovery duplicated result"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "recovery reran or changed task"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json(),
        message_before,
        "recovery changed kickoff/result"
    );
    assert_eq!(
        fx.tree(),
        tracker_before,
        "recovery published to shared issue"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "recovery changed shared PR loop"
    );
    let publish =
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[message]});
    fx.op("delivery_publish", publish.clone());
    let issue = crate::issue::board::find_issue(&fx.pm(), FIXTURE_ISSUE).unwrap();
    assert_eq!(
        issue
            .comments
            .iter()
            .filter(|c| c.body.contains(marker))
            .count(),
        1,
        "recovered result was not published exactly once"
    );
    let after_publish = fx.tree();
    assert!(
        fx.op_err("delivery_publish", publish).is_err(),
        "recovered result published twice"
    );
    assert_eq!(
        fx.tree(),
        after_publish,
        "duplicate publication changed tracker"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "publication changed shared PR loop"
    );
}

/// Real member request, worker dispatch and durable result for publication
/// recovery tests. The identity seam is not an OS isolation proof.
fn pending_publication_fixture(sub: &str, marker: &str) -> (Fx, String, String, String) {
    let fx = Fx::new();
    let rt = fx.member_runtime(sub);
    with_deferred_service(&fx);
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
    let request = fx.op("member_job_request", json!({
        "member_envelope":fx.member_envelope(sub), "title":"pending publication acceptance",
        "request_key":"pending-publication-acceptance", "issue":FIXTURE_ISSUE, "body":"private work"
    }));
    let job = promote_member_fixture_job(&fx, sub, &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let dispatch = fx.op(
        "task_dispatch",
        json!({"task":task,"to":rt.worker,"by":"operator"}),
    );
    let message = dispatch["message"].as_str().unwrap().to_string();
    let msg = fx.shared.store.message(&message).unwrap().unwrap();
    let sha = "1234567890abcdef1234567890abcdef12345678";
    fx.shared
        .store
        .finish(
            &msg,
            "completed",
            &json!({"status":"completed","sha":sha,"text":marker}),
            None,
        )
        .unwrap();
    fx.op(
        "task_verdict",
        json!({"task":task,"sha":sha,"verdict":"pass"}),
    );
    let listed = fx.op("delivery_private_list", json!({}));
    let delivery = listed["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["origin"]["job"] == job && d["origin"]["task"] == task)
        .expect("real task produced no private delivery")["id"]
        .as_str()
        .unwrap()
        .to_string();
    (fx, task, message, delivery)
}

#[test]
fn pending_publication_reconcile_races_original_through_real_tracker_lock() {
    let _flag = AssistantFlagOn::arm();
    let marker = "pending-race-private-result-2215";
    let (fx, task, message, delivery) = pending_publication_fixture("sub-pending-race", marker);
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    let message_before = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .unwrap()
        .to_json();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let pm = crate::issue::Pm::at(&fx.pm()).unwrap();
    let issue_dir = fx.pm().join("fixture").join(FIXTURE_ISSUE);
    let tracker_lock = pm
        .lock_for_paths(&[issue_dir])
        .expect("acquire real tracker lock");
    let (pending_tx, pending_rx) = std::sync::mpsc::channel::<()>();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let hook_release = Arc::clone(&release);
    let pending_hook = Arc::new(
        move |_: &crate::delivery::publication::PublicationReceipt, _: &str| {
            pending_tx.send(()).unwrap();
            let (lock, cv) = &*hook_release;
            let guard = lock.lock().unwrap();
            // Bound cleanup if the test itself fails before releasing the hook.
            drop(
                cv.wait_timeout_while(guard, Duration::from_secs(30), |released| !*released)
                    .unwrap(),
            );
        },
    );
    let pending_seam = crate::delivery::publication::arm_post_pending(fx.dir.path(), pending_hook);
    let (tracker_tx, tracker_rx) = std::sync::mpsc::channel::<()>();
    let tracker_hook = Arc::new(move || {
        tracker_tx.send(()).unwrap();
    });
    let tracker_seam = crate::delivery::publication::arm_pre_tracker(fx.dir.path(), tracker_hook);
    let params =
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[message]});
    let (pending_arrived, tracker_arrived, attempt, reconcile, original) =
        std::thread::scope(|scope| {
            let shared = Arc::clone(&fx.shared);
            let publish_args = params.clone();
            let original = scope.spawn(move || {
                scoped(Asserted::Operator, || {
                    shared.dispatch("delivery_publish", &publish_args, std::process::id())
                })
            });
            let pending_arrived = pending_rx.recv_timeout(Duration::from_secs(10)).is_ok();
            let attempt = crate::delivery::private::load(fx.dir.path())
                .ok()
                .and_then(|all| {
                    all.get(&delivery)
                        .and_then(|d| d.publications.first())
                        .map(|p| p.attempt.clone())
                })
                .unwrap_or_default();
            let shared = Arc::clone(&fx.shared);
            let reconcile_args = json!({"delivery":delivery,"attempt":attempt});
            let reconcile = scope.spawn(move || {
                scoped(Asserted::Operator, || {
                    shared.dispatch(
                        "delivery_publish_reconcile",
                        &reconcile_args,
                        std::process::id(),
                    )
                })
            });
            let tracker_arrived = tracker_rx.recv_timeout(Duration::from_secs(10)).is_ok();
            // Reconcile is at the real lock while the original remains parked.
            drop(tracker_lock);
            let reconcile = reconcile.join().expect("reconcile thread panicked");
            let (lock, cv) = &*release;
            *lock.lock().unwrap() = true;
            cv.notify_all();
            let original = original.join().expect("publisher thread panicked");
            (
                pending_arrived,
                tracker_arrived,
                attempt,
                reconcile,
                original,
            )
        });
    drop(tracker_seam);
    drop(pending_seam);
    assert!(
        pending_arrived && tracker_arrived && !attempt.is_empty(),
        "both actual publication stages must be reached"
    );
    let reconcile = reconcile.expect("reconciliation failed against released tracker lock");
    assert_eq!(reconcile["state"], "published");
    // The original may observe that the concurrent reconciler already
    // settled its exact attempt; it must never cause a second effect.
    if let Err(e) = original {
        assert!(
            !e.to_string().contains("unknown method"),
            "publisher entrypoint missing: {e}"
        );
    }
    let all = crate::delivery::private::load(fx.dir.path()).unwrap();
    let row = all.get(&delivery).unwrap();
    assert_eq!(row.publications.len(), 1, "duplicate publication receipt");
    let receipt = &row.publications[0];
    assert_eq!(receipt.attempt, attempt);
    let comment = match &receipt.outcome {
        crate::delivery::publication::PublicationState::Published { comment } => comment.clone(),
        other => panic!("attempt did not settle: {other:?}"),
    };
    let issue = crate::issue::board::find_issue(&fx.pm(), FIXTURE_ISSUE).unwrap();
    assert_eq!(
        issue
            .comments
            .iter()
            .filter(|c| c.body.contains(marker))
            .count(),
        1,
        "result comment duplicated or missing"
    );
    assert_eq!(
        issue.comments.iter().filter(|c| c.name == comment).count(),
        1,
        "settled receipt does not name the comment"
    );
    let committed = r5_git(
        &fx.pm(),
        &[
            "show",
            &format!("HEAD:fixture/{FIXTURE_ISSUE}/comments/{comment}"),
        ],
    );
    assert!(
        committed.contains(marker),
        "keyed comment is not committed at HEAD"
    );
    assert!(
        fx.call(
            Asserted::Agent("ordinary-worker".into()),
            "delivery_publish_reconcile",
            json!({"delivery":delivery,"attempt":attempt})
        )
        .is_err(),
        "agent obtained operator publication recovery"
    );
    for claim in [
        json!({"delivery":delivery,"attempt":attempt,"public_session":"forged"}),
        json!({"delivery":delivery,"attempt":attempt,"read_as":"operator"}),
        json!({"delivery":delivery,"attempt":attempt,"cli_envelope":"forged"}),
    ] {
        assert!(
            fx.op_err("delivery_publish_reconcile", claim).is_err(),
            "relay proof obtained native publication recovery"
        );
    }
    let before_replay = fx.tree();
    let again = fx.op(
        "delivery_publish_reconcile",
        json!({"delivery":delivery,"attempt":attempt}),
    );
    assert_eq!(again["state"], "published");
    assert_eq!(
        fx.tree(),
        before_replay,
        "reconcile replay wrote another comment"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "publication reran the task"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json(),
        message_before,
        "publication changed task result"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "publication changed ordinary PR state"
    );
}

#[test]
fn reconcile_refuses_conflicting_uncommitted_keyed_comment_without_overwrite() {
    let _flag = AssistantFlagOn::arm();
    let marker = "pending-conflict-private-result-2215";
    let (fx, task, message, delivery) = pending_publication_fixture("sub-pending-conflict", marker);
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel::<()>();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let hook_release = Arc::clone(&release);
    let seam = crate::delivery::publication::arm_post_pending(
        fx.dir.path(),
        Arc::new(
            move |_: &crate::delivery::publication::PublicationReceipt, _: &str| {
                arrived_tx.send(()).unwrap();
                let (lock, cv) = &*hook_release;
                drop(
                    cv.wait_timeout_while(
                        lock.lock().unwrap(),
                        Duration::from_secs(30),
                        |released| !*released,
                    )
                    .unwrap(),
                );
            },
        ),
    );
    let args =
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[message]});
    let (arrived, attempt, path, bytes, reconcile, original) = std::thread::scope(|scope| {
        let shared = Arc::clone(&fx.shared);
        let publish_args = args.clone();
        let original = scope.spawn(move || {
            scoped(Asserted::Operator, || {
                shared.dispatch("delivery_publish", &publish_args, std::process::id())
            })
        });
        let arrived = arrived_rx.recv_timeout(Duration::from_secs(10)).is_ok();
        let all = crate::delivery::private::load(fx.dir.path()).unwrap();
        let receipt = all
            .get(&delivery)
            .and_then(|d| d.publications.first())
            .cloned();
        let attempt = receipt
            .as_ref()
            .map(|r| r.attempt.clone())
            .unwrap_or_default();
        let mut path = None;
        let mut bytes = Vec::new();
        if let Some(receipt) = receipt {
            let comments = fx.pm().join("fixture").join(FIXTURE_ISSUE).join("comments");
            std::fs::create_dir_all(&comments).unwrap();
            let name = format!(
                "{}-operator-{}.md",
                crate::issue::time::basic(receipt.at),
                receipt.attempt
            );
            let file = comments.join(name);
            let front = crate::issue::model::CommentFront {
                author: "operator".into(),
                at: crate::issue::time::iso(receipt.at),
                kind: Some("publication".into()),
            };
            bytes = crate::issue::parse::render(&front, "conflicting uncommitted keyed content\n")
                .unwrap()
                .into_bytes();
            std::fs::write(&file, &bytes).unwrap();
            path = Some(file);
        }
        let reconcile = fx.op_err(
            "delivery_publish_reconcile",
            json!({"delivery":delivery,"attempt":attempt}),
        );
        let (lock, cv) = &*release;
        *lock.lock().unwrap() = true;
        cv.notify_all();
        let original = original.join().expect("publisher thread panicked");
        (arrived, attempt, path, bytes, reconcile, original)
    });
    drop(seam);
    assert!(
        arrived && !attempt.is_empty(),
        "original never persisted pending receipt"
    );
    let path = path.expect("missing keyed conflict fixture");
    let rel = path
        .strip_prefix(fx.pm())
        .unwrap()
        .to_string_lossy()
        .to_string();
    let tracked = std::process::Command::new("git")
        .current_dir(fx.pm())
        .args(["cat-file", "-e", &format!("HEAD:{rel}")])
        .status()
        .unwrap();
    assert!(
        !tracked.success(),
        "conflict fixture unexpectedly exists at HEAD"
    );
    assert!(
        reconcile.is_err(),
        "uncommitted conflicting comment treated as published: {reconcile:?}"
    );
    assert!(
        original.is_err(),
        "original overwrote or claimed the conflicting comment"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "conflicting file was overwritten or removed"
    );
    let all = crate::delivery::private::load(fx.dir.path()).unwrap();
    let row = all.get(&delivery).unwrap();
    assert_eq!(row.publications.len(), 1);
    assert_eq!(row.publications[0].attempt, attempt);
    assert!(
        matches!(
            &row.publications[0].outcome,
            crate::delivery::publication::PublicationState::Pending
        ),
        "uncommitted conflict was settled"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "reconcile changed task"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "reconcile changed PR loop"
    );
}

/// Drive the real publisher to its durable pending state, then alter only a
/// disposable tracker's deterministic keyed path while the writer is paused.
fn keyed_head_fault_refuses_reconciliation(sub: &str, deleted: bool) {
    let _flag = AssistantFlagOn::arm();
    let marker = "keyed-head-recovery-private-result-2215";
    let (fx, task, message, delivery) = pending_publication_fixture(sub, marker);
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    let message_before = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .unwrap()
        .to_json();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let hook_release = Arc::clone(&release);
    let seam = crate::delivery::publication::arm_post_pending(
        fx.dir.path(),
        Arc::new(
            move |receipt: &crate::delivery::publication::PublicationReceipt, body: &str| {
                arrived_tx
                    .send((receipt.clone(), body.to_string()))
                    .unwrap();
                let (lock, cv) = &*hook_release;
                // Even an earlier fixture assertion cannot leave a detached
                // publisher parked forever.
                drop(
                    cv.wait_timeout_while(
                        lock.lock().unwrap(),
                        Duration::from_secs(30),
                        |released| !*released,
                    )
                    .unwrap(),
                );
            },
        ),
    );
    let shared = Arc::clone(&fx.shared);
    let publish =
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[message]});
    let original = std::thread::spawn(move || {
        scoped(Asserted::Operator, || {
            shared.dispatch("delivery_publish", &publish, std::process::id())
        })
    });
    let (receipt, body) = arrived_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("original did not persist a pending publication");
    assert_eq!(receipt.delivery_id, delivery);
    assert_eq!(receipt.actor, "operator");
    assert!(matches!(
        &receipt.outcome,
        crate::delivery::publication::PublicationState::Pending
    ));
    let attempt = receipt.attempt.clone();
    let name = format!(
        "{}-operator-{}.md",
        crate::issue::time::basic(receipt.at),
        attempt
    );
    let relative = format!("fixture/{FIXTURE_ISSUE}/comments/{name}");
    let path = fx.pm().join(&relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let front = crate::issue::model::CommentFront {
        author: "operator".into(),
        at: crate::issue::time::iso(receipt.at),
        kind: Some("publication".into()),
    };
    // This exact body was observed from the real publisher, never rebuilt
    // from an invented publication format.
    let expected = crate::issue::parse::render(&front, &body)
        .unwrap()
        .into_bytes();
    let committed = if deleted {
        expected.clone()
    } else {
        crate::issue::parse::render(&front, "different committed keyed comment\n")
            .unwrap()
            .into_bytes()
    };
    let pm = crate::issue::Pm::at(&fx.pm()).unwrap();
    let issue_dir = fx.pm().join("fixture").join(FIXTURE_ISSUE);
    let guard = pm
        .lock_for_paths(&[issue_dir])
        .expect("fixture tracker lock");
    std::fs::write(&path, &committed).unwrap();
    r5_git(&fx.pm(), &["add", "--", &relative]);
    r5_git(
        &fx.pm(),
        &[
            "commit",
            "-q",
            "-m",
            "fixture keyed committed comment",
            "--",
            &relative,
        ],
    );
    drop(guard);
    let head = std::process::Command::new("git")
        .current_dir(fx.pm())
        .args(["show", &format!("HEAD:{relative}")])
        .output()
        .unwrap();
    assert!(
        head.status.success(),
        "keyed fixture was not committed at HEAD"
    );
    assert_eq!(
        head.stdout, committed,
        "HEAD does not contain the fixture bytes"
    );
    if deleted {
        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());
    } else {
        assert_ne!(
            committed, expected,
            "fixture HEAD must differ from real intended bytes"
        );
        std::fs::write(&path, &expected).unwrap();
        r5_git(
            &fx.pm(),
            &["update-index", "--assume-unchanged", "--", &relative],
        );
        let diff = std::process::Command::new("git")
            .current_dir(fx.pm())
            .args(["diff", "--quiet", "HEAD", "--", &relative])
            .status()
            .unwrap();
        assert!(diff.success(), "fixture did not fool git diff --quiet");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            expected,
            "worktree is not the real intended comment"
        );
    }
    let attempt_args = json!({"delivery":delivery,"attempt":attempt});
    let reconcile = fx.op_err("delivery_publish_reconcile", attempt_args);
    assert!(
        reconcile.is_err(),
        "unprovable tracked keyed path was treated as committed: {reconcile:?}"
    );
    let error = reconcile.unwrap_err().to_string();
    assert!(
        !error.to_lowercase().contains("unknown method"),
        "reconcile endpoint is missing: {error}"
    );
    let all = crate::delivery::private::load(fx.dir.path()).unwrap();
    let row = all.get(&delivery).unwrap();
    assert_eq!(row.publications.len(), 1);
    assert_eq!(row.publications[0].attempt, attempt);
    assert!(
        matches!(
            &row.publications[0].outcome,
            crate::delivery::publication::PublicationState::Pending
        ),
        "unprovable path was settled"
    );
    if deleted {
        assert!(
            !path.exists(),
            "reconcile recreated deleted tracked comment"
        );
    } else {
        assert_eq!(
            std::fs::read(&path).unwrap(),
            expected,
            "reconcile overwrote worktree bytes"
        );
    }
    let (lock, cv) = &*release;
    *lock.lock().unwrap() = true;
    cv.notify_all();
    let original = original.join().expect("original publisher panicked");
    drop(seam);
    assert!(
        original.is_err(),
        "original publisher claimed an unprovable keyed path"
    );
    if deleted {
        assert!(!path.exists(), "original recreated deleted tracked comment");
    } else {
        assert_eq!(
            std::fs::read(&path).unwrap(),
            expected,
            "original overwrote worktree bytes"
        );
        r5_git(
            &fx.pm(),
            &["update-index", "--no-assume-unchanged", "--", &relative],
        );
    }
    let again = crate::delivery::private::load(fx.dir.path()).unwrap();
    assert!(matches!(
        &again[&delivery].publications[0].outcome,
        crate::delivery::publication::PublicationState::Pending
    ));
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "reconciliation changed task"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json(),
        message_before,
        "reconciliation changed durable result"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "reconciliation changed ordinary PR state"
    );
}

#[test]
fn reconcile_refuses_assume_unchanged_worktree_over_different_committed_blob() {
    keyed_head_fault_refuses_reconciliation("sub-keyed-assume-unchanged", false);
}

#[test]
fn reconcile_preserves_deleted_tracked_keyed_comment() {
    keyed_head_fault_refuses_reconciliation("sub-keyed-deleted", true);
}

#[test]
fn pending_publication_cannot_follow_issue_moved_to_another_project() {
    let _flag = AssistantFlagOn::arm();
    let marker = "moved-project-private-publication-2215";
    let (fx, task, message, delivery) = pending_publication_fixture("sub-pending-moved", marker);
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    let message_before = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .unwrap()
        .to_json();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let hook_release = Arc::clone(&release);
    let seam = crate::delivery::publication::arm_post_pending(
        fx.dir.path(),
        Arc::new(
            move |receipt: &crate::delivery::publication::PublicationReceipt, _: &str| {
                arrived_tx.send(receipt.clone()).unwrap();
                let (lock, cv) = &*hook_release;
                drop(
                    cv.wait_timeout_while(
                        lock.lock().unwrap(),
                        Duration::from_secs(30),
                        |released| !*released,
                    )
                    .unwrap(),
                );
            },
        ),
    );
    let shared = Arc::clone(&fx.shared);
    let publish =
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[message]});
    let original = std::thread::spawn(move || {
        scoped(Asserted::Operator, || {
            shared.dispatch("delivery_publish", &publish, std::process::id())
        })
    });
    let receipt = arrived_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("pending receipt not reached");
    assert_eq!(receipt.project, "fixture");
    assert_eq!(receipt.issue, FIXTURE_ISSUE);
    assert!(matches!(
        &receipt.outcome,
        crate::delivery::publication::PublicationState::Pending
    ));

    // Move the actual issue into an actual new project, with the tracker
    // writer lock held; this changes the source of truth rather than a
    // fake request field or a ledger origin. Commit only the new project
    // and issue in this disposable tracker.
    let target_project = fx.pm().join("moved");
    std::fs::create_dir_all(&target_project).unwrap();
    std::fs::write(
        target_project.join("project.yaml"),
        "key: moved\nprefix: MOV\n",
    )
    .unwrap();
    let old_issue = fx.pm().join("fixture").join(FIXTURE_ISSUE);
    let new_issue = target_project.join(FIXTURE_ISSUE);
    let pm = crate::issue::Pm::at(&fx.pm()).unwrap();
    let guard = pm
        .lock_for_paths(&[old_issue.clone(), target_project.clone()])
        .unwrap();
    std::fs::rename(&old_issue, &new_issue).unwrap();
    r5_git(
        &fx.pm(),
        &[
            "add",
            "--",
            "moved/project.yaml",
            &format!("moved/{FIXTURE_ISSUE}"),
        ],
    );
    r5_git(
        &fx.pm(),
        &[
            "commit",
            "-q",
            "-m",
            "move fixture issue to another project",
            "--",
            "moved/project.yaml",
            &format!("moved/{FIXTURE_ISSUE}"),
        ],
    );
    drop(guard);
    let resolved = crate::issue::board::find_issue(&fx.pm(), FIXTURE_ISSUE).unwrap();
    assert_eq!(
        resolved.project, "moved",
        "fixture did not change authoritative issue project"
    );
    assert!(!old_issue.exists());
    let tracker_after_move = fx.tree();
    let reconcile = fx.op_err(
        "delivery_publish_reconcile",
        json!({"delivery":delivery,"attempt":receipt.attempt}),
    );
    let (lock, cv) = &*release;
    *lock.lock().unwrap() = true;
    cv.notify_all();
    let original = original.join().expect("original publisher panicked");
    drop(seam);
    assert!(
        reconcile.is_err(),
        "operator reconciled private result into moved project: {reconcile:?}"
    );
    assert!(
        !reconcile
            .unwrap_err()
            .to_string()
            .to_lowercase()
            .contains("unknown method"),
        "reconcile endpoint missing"
    );
    assert!(
        original.is_err(),
        "paused original published into moved project"
    );
    assert_eq!(
        fx.tree(),
        tracker_after_move,
        "publication or reconciliation changed the moved tracker"
    );
    let moved = crate::issue::board::find_issue(&fx.pm(), FIXTURE_ISSUE).unwrap();
    assert_eq!(moved.project, "moved");
    assert!(
        moved.comments.iter().all(|c| !c.body.contains(marker)),
        "private result reached the moved issue"
    );
    let all = crate::delivery::private::load(fx.dir.path()).unwrap();
    let row = all.get(&delivery).unwrap();
    assert_eq!(row.publications.len(), 1);
    assert_eq!(row.publications[0].attempt, receipt.attempt);
    assert!(
        matches!(
            &row.publications[0].outcome,
            crate::delivery::publication::PublicationState::Pending
        ),
        "moved issue publication incorrectly settled"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "publication changed or reran task"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json(),
        message_before,
        "publication changed durable result"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "publication changed ordinary PR state"
    );
}

/// A genuine pending attempt is corrupted only after the real publisher
/// has durably recorded it. Reconcile must fail closed before sharing.
fn corrupted_pending_body_digest_is_refused(sub: &str, remove: bool) {
    let _flag = AssistantFlagOn::arm();
    let marker = "bad-pending-digest-private-result-2215";
    let (fx, task, message, delivery) = pending_publication_fixture(sub, marker);
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let (arrived_tx, arrived_rx) = std::sync::mpsc::channel();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let hook_release = Arc::clone(&release);
    let seam = crate::delivery::publication::arm_post_pending(
        fx.dir.path(),
        Arc::new(
            move |receipt: &crate::delivery::publication::PublicationReceipt, _: &str| {
                arrived_tx.send(receipt.clone()).unwrap();
                let (lock, cv) = &*hook_release;
                drop(
                    cv.wait_timeout_while(
                        lock.lock().unwrap(),
                        Duration::from_secs(30),
                        |released| !*released,
                    )
                    .unwrap(),
                );
                // Model the original writer crashing after the durable pending
                // receipt; only the independently invoked real reconcile can write.
                panic!("fixture interruption after pending receipt");
            },
        ),
    );
    let shared = Arc::clone(&fx.shared);
    let args =
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[message]});
    let original = std::thread::spawn(move || {
        scoped(Asserted::Operator, || {
            shared.dispatch("delivery_publish", &args, std::process::id())
        })
    });
    let receipt = arrived_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("real pending receipt missing");
    assert!(
        !receipt.body_digest.is_empty(),
        "publisher did not create a real body digest"
    );
    assert!(matches!(
        &receipt.outcome,
        crate::delivery::publication::PublicationState::Pending
    ));
    let path = fx.dir.path().join("delivery-private.json");
    let mut ledger: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let publication = ledger["records"][&delivery]["publications"][0]
        .as_object_mut()
        .expect("pending receipt missing from actual ledger");
    assert_eq!(
        publication.get("attempt").and_then(Value::as_str),
        Some(receipt.attempt.as_str())
    );
    if remove {
        assert!(publication.remove("body_digest").is_some());
    } else {
        publication.insert("body_digest".to_string(), json!(""));
    }
    let corrupted = serde_json::to_vec_pretty(&ledger).unwrap();
    std::fs::write(&path, &corrupted).unwrap();
    let tracker_before = fx.tree();
    let result = fx.op_err(
        "delivery_publish_reconcile",
        json!({"delivery":delivery,"attempt":receipt.attempt}),
    );
    assert!(
        result.is_err(),
        "operator reconcile accepted corrupt body digest: {result:?}"
    );
    assert!(
        !result
            .unwrap_err()
            .to_string()
            .to_lowercase()
            .contains("unknown method"),
        "reconcile endpoint missing"
    );
    assert_eq!(
        fx.tree(),
        tracker_before,
        "corrupt receipt caused a shared tracker write"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        corrupted,
        "reconcile rewrote the corrupt receipt"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "reconcile changed the task"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "reconcile changed ordinary PR state"
    );
    let (lock, cv) = &*release;
    *lock.lock().unwrap() = true;
    cv.notify_all();
    assert!(
        original.join().is_err(),
        "fixture failed to interrupt original publisher"
    );
    drop(seam);
    assert_eq!(
        fx.tree(),
        tracker_before,
        "interrupted publisher changed the tracker"
    );
}

#[test]
fn reconcile_refuses_pending_receipt_missing_body_digest() {
    corrupted_pending_body_digest_is_refused("sub-pending-digest-missing", true);
}

#[test]
fn reconcile_refuses_pending_receipt_with_empty_body_digest() {
    corrupted_pending_body_digest_is_refused("sub-pending-digest-empty", false);
}

#[test]
fn uncertain_commit_preserves_committed_comment_and_reconciles_once() {
    let _flag = AssistantFlagOn::arm();
    let marker = "uncertain-commit-private-result-2215";
    let (fx, task, message, delivery) = pending_publication_fixture("sub-uncertain-commit", marker);
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    let message_before = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .unwrap()
        .to_json();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fired_hook = Arc::clone(&fired);
    // Fires only after the actual keyed tracker commit has returned Ok,
    // then converts that result to an error in the real cleanup branch.
    let seam = crate::issue::write::arm_keyed_commit_after_success(
        &fx.pm(),
        Arc::new(move || {
            fired_hook.store(true, std::sync::atomic::Ordering::SeqCst);
            Err(crate::error::Error::rejected(
                "fixture uncertain commit after real success",
            ))
        }),
    );
    let args =
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[message]});
    let failed = fx.op_err("delivery_publish", args.clone());
    drop(seam);
    assert!(
        fired.load(std::sync::atomic::Ordering::SeqCst),
        "real tracker commit never reached post-success seam"
    );
    let err = failed
        .expect_err("uncertain commit must not be reported as settled success")
        .to_string();
    assert!(
        err.contains("fixture uncertain commit after real success"),
        "unexpected publication failure: {err}"
    );
    let ledger = crate::delivery::private::load(fx.dir.path()).unwrap();
    let row = ledger.get(&delivery).expect("private delivery missing");
    assert_eq!(row.publications.len(), 1);
    let pending = &row.publications[0];
    assert!(
        matches!(
            &pending.outcome,
            crate::delivery::publication::PublicationState::Pending
        ),
        "uncertain attempt was settled"
    );
    let attempt = pending.attempt.clone();
    let name = format!(
        "{}-operator-{}.md",
        crate::issue::time::basic(pending.at),
        attempt
    );
    let rel = format!("fixture/{FIXTURE_ISSUE}/comments/{name}");
    let path = fx.pm().join(&rel);
    let bytes = std::fs::read(&path).expect("error cleanup removed committed comment");
    assert!(
        String::from_utf8_lossy(&bytes).contains(marker),
        "committed comment lacks result"
    );
    let head = std::process::Command::new("git")
        .current_dir(fx.pm())
        .args(["show", &format!("HEAD:{rel}")])
        .output()
        .unwrap();
    assert!(
        head.status.success(),
        "comment did not reach HEAD before synthetic error"
    );
    assert_eq!(
        head.stdout, bytes,
        "worktree no longer equals committed comment"
    );
    let before_reconcile = fx.tree();
    assert!(
        fx.call(
            Asserted::Agent("ordinary-worker".into()),
            "delivery_publish_reconcile",
            json!({"delivery":delivery,"attempt":attempt})
        )
        .is_err(),
        "agent reconciled operator-only attempt"
    );
    let reconciled = fx.op(
        "delivery_publish_reconcile",
        json!({"delivery":delivery,"attempt":attempt}),
    );
    assert_eq!(reconciled["state"], "published");
    assert_eq!(reconciled["comment"], name);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "reconcile overwrote committed comment"
    );
    assert_eq!(
        fx.tree(),
        before_reconcile,
        "reconcile changed the existing committed tracker comment"
    );
    let replay = fx.op(
        "delivery_publish_reconcile",
        json!({"delivery":delivery,"attempt":attempt}),
    );
    assert_eq!(replay["state"], "published");
    assert_eq!(replay["comment"], name);
    assert_eq!(
        fx.tree(),
        before_reconcile,
        "reconcile replay changed tracker"
    );
    let ledger = crate::delivery::private::load(fx.dir.path()).unwrap();
    let row = ledger.get(&delivery).unwrap();
    assert_eq!(
        row.publications.len(),
        1,
        "uncertain recovery created duplicate receipts"
    );
    assert_eq!(row.publications[0].attempt, attempt);
    assert!(
        matches!(&row.publications[0].outcome, crate::delivery::publication::PublicationState::Published { comment } if comment == &name)
    );
    let issue = crate::issue::board::find_issue(&fx.pm(), FIXTURE_ISSUE).unwrap();
    assert_eq!(
        issue
            .comments
            .iter()
            .filter(|c| c.body.contains(marker))
            .count(),
        1,
        "uncertain commit produced duplicate comments"
    );
    assert!(
        fx.op_err("delivery_publish", args).is_err(),
        "published result could be shared twice"
    );
    assert_eq!(
        fx.tree(),
        before_reconcile,
        "duplicate publication altered tracker"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "recovery reran task"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json(),
        message_before,
        "recovery changed durable result"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "recovery changed ordinary PR loop"
    );
}

#[test]
fn private_list_refuses_incomplete_present_envelope_without_rewriting() {
    let _flag = AssistantFlagOn::arm();
    let marker = "incomplete-ledger-real-result-2215";
    let (fx, task, message, delivery) =
        pending_publication_fixture("sub-incomplete-ledger", marker);
    let before = fx.op("delivery_private_list", json!({}));
    assert!(
        before.to_string().contains(&delivery) && before.to_string().contains(marker),
        "real owner delivery positive missing: {before}"
    );
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    let message_before = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .unwrap()
        .to_json();
    let path = fx.dir.path().join("delivery-private.json");
    let original = std::fs::read(&path).unwrap();
    let corrupt = b"{\"version\":1}\n";
    std::fs::write(&path, corrupt).unwrap();
    let result = fx.op_err("delivery_private_list", json!({}));
    assert!(
        result.is_err(),
        "present incomplete ledger was treated as an empty list: {result:?}"
    );
    assert!(
        !result
            .unwrap_err()
            .to_string()
            .to_lowercase()
            .contains("unknown method"),
        "private list RPC missing"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        corrupt.to_vec(),
        "failed read rewrote incomplete ledger"
    );
    std::fs::write(&path, &original).unwrap();
    let restored = fx.op("delivery_private_list", json!({}));
    assert_eq!(
        restored, before,
        "restoring exact ledger bytes did not restore operator recovery"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "ledger read changed task"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json(),
        message_before,
        "ledger read changed durable result"
    );
}

#[test]
fn reopened_task_same_revision_keeps_distinct_private_dispatch_results() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-reopened-delivery");
    with_deferred_service(&fx);
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
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope":fx.member_envelope("sub-reopened-delivery"),
            "title":"reopened private delivery", "request_key":"reopened-delivery-acceptance",
            "issue":FIXTURE_ISSUE, "body":"private work"
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-reopened-delivery", &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let first = fx.op(
        "task_dispatch",
        json!({"task":task,"to":rt.worker,"by":"operator"}),
    );
    let first_message = first["message"]
        .as_str()
        .expect("first native dispatch missing")
        .to_string();
    let first_revision = first["task"]["revision"]
        .as_i64()
        .expect("first revision missing");
    assert_eq!(first_revision, 1);
    let first_msg = fx.shared.store.message(&first_message).unwrap().unwrap();
    let first_body = "REOPENED_PRIVATE_FIRST_RESULT_2215";
    let first_sha = "1111111111111111111111111111111111111111";
    fx.shared
        .store
        .finish(
            &first_msg,
            "completed",
            &json!({"status":"completed","sha":first_sha,"text":first_body}),
            None,
        )
        .unwrap();
    let verdict = fx.op(
        "task_verdict",
        json!({"task":task,"sha":first_sha,"verdict":"pass"}),
    );
    assert_eq!(
        verdict["task"]["state"], "verified",
        "first task did not enter reopenable state"
    );
    let first_list = fx.op("delivery_private_list", json!({}));
    let first_row = first_list["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| {
            r["origin"]["job"] == job && r["origin"]["task"] == task && r["state"] == "completed"
        })
        .expect("first genuine private result missing");
    let first_delivery = first_row["id"].as_str().unwrap().to_string();
    assert_eq!(first_row["origin"]["dispatch"], first_message);
    assert_eq!(first_row["results"].as_array().unwrap().len(), 1);
    assert_eq!(first_row["results"][0]["message"], first_message);
    assert_eq!(first_row["results"][0]["body"], first_body);
    let first_message_before = fx
        .shared
        .store
        .message(&first_message)
        .unwrap()
        .unwrap()
        .to_json();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let tracker_before = fx.tree();

    let reopened = fx.op("task_reopen", json!({"task":task}));
    assert_eq!(reopened["task"]["state"], "draft");
    assert_eq!(reopened["task"]["revision"], 0);
    let second = fx.op(
        "task_dispatch",
        json!({"task":task,"to":rt.worker,"by":"operator"}),
    );
    let second_message = second["message"]
        .as_str()
        .expect("second native dispatch missing")
        .to_string();
    let second_revision = second["task"]["revision"]
        .as_i64()
        .expect("second revision missing");
    assert_eq!(
        second_revision, first_revision,
        "fixture did not exercise revision reuse"
    );
    assert_ne!(
        second_message, first_message,
        "reopened dispatch reused the first kickoff"
    );
    let second_msg = fx.shared.store.message(&second_message).unwrap().unwrap();
    assert_eq!(second_msg.task_id.as_deref(), Some(task.as_str()));
    assert_eq!(second_msg.source, "job_dispatch");
    let running = fx.op("delivery_private_list", json!({}));
    let matching: Vec<_> = running["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["origin"]["job"] == job && r["origin"]["task"] == task)
        .collect();
    assert_eq!(
        matching.len(),
        2,
        "same-revision dispatches collapsed into one delivery: {running}"
    );
    let old = matching
        .iter()
        .find(|r| r["id"] == first_delivery)
        .expect("first delivery disappeared");
    assert_eq!(old["state"], "completed");
    assert_eq!(old["origin"]["dispatch"], first_message);
    assert_eq!(old["results"].as_array().unwrap().len(), 1);
    assert_eq!(old["results"][0]["message"], first_message);
    assert_eq!(old["results"][0]["body"], first_body);
    let new = matching
        .iter()
        .find(|r| r["id"] != first_delivery)
        .expect("second delivery missing");
    let second_delivery = new["id"].as_str().unwrap().to_string();
    assert_ne!(second_delivery, first_delivery);
    assert_eq!(
        new["state"], "running",
        "second dispatch inherited completed state: {new}"
    );
    assert_eq!(new["origin"]["dispatch"], second_message);
    assert!(
        new["results"].as_array().unwrap().is_empty(),
        "second dispatch inherited old results: {new}"
    );
    assert!(!new.to_string().contains(&first_message) && !new.to_string().contains(first_body));
    assert_eq!(
        fx.shared
            .store
            .message(&first_message)
            .unwrap()
            .unwrap()
            .to_json(),
        first_message_before,
        "reopen reran or rewrote first kickoff"
    );
    assert_eq!(
        fx.tree(),
        tracker_before,
        "reopen/dispatch published private result"
    );

    let second_body = "REOPENED_PRIVATE_SECOND_RESULT_2215";
    let second_sha = "2222222222222222222222222222222222222222";
    fx.shared
        .store
        .finish(
            &second_msg,
            "completed",
            &json!({"status":"completed","sha":second_sha,"text":second_body}),
            None,
        )
        .unwrap();
    fx.op(
        "task_verdict",
        json!({"task":task,"sha":second_sha,"verdict":"pass"}),
    );
    let completed = fx.op("delivery_private_list", json!({}));
    let rows = completed["deliveries"].as_array().unwrap();
    for (id, msg, body) in [
        (&first_delivery, &first_message, first_body),
        (&second_delivery, &second_message, second_body),
    ] {
        let row = rows
            .iter()
            .find(|r| r["id"].as_str() == Some(id.as_str()))
            .expect("completed dispatch delivery missing");
        assert_eq!(row["state"], "completed");
        assert_eq!(
            row["results"].as_array().unwrap().len(),
            1,
            "results crossed or duplicated: {row}"
        );
        assert_eq!(row["results"][0]["message"].as_str(), Some(msg.as_str()));
        assert_eq!(row["results"][0]["body"].as_str(), Some(body));
        assert!(row["publications"].as_array().unwrap().is_empty());
    }
    let wrong_first = json!({"delivery":first_delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[second_message]});
    let wrong_second = json!({"delivery":second_delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[first_message]});
    assert!(
        fx.op_err("delivery_publish", wrong_first).is_err(),
        "first delivery accepted second dispatch result"
    );
    assert!(
        fx.op_err("delivery_publish", wrong_second).is_err(),
        "second delivery accepted first dispatch result"
    );
    assert_eq!(
        fx.op("delivery_private_list", json!({})),
        completed,
        "crossed-result refusal changed private ledger"
    );
    assert_eq!(
        fx.tree(),
        tracker_before,
        "crossed-result refusal wrote shared tracker"
    );
    let publish_first = json!({"delivery":first_delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[first_message]});
    let publish_second = json!({"delivery":second_delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[second_message]});
    fx.op("delivery_publish", publish_first.clone());
    fx.op("delivery_publish", publish_second.clone());
    let issue = crate::issue::board::find_issue(&fx.pm(), FIXTURE_ISSUE).unwrap();
    assert_eq!(
        issue
            .comments
            .iter()
            .filter(|c| c.body.contains(first_body))
            .count(),
        1
    );
    assert_eq!(
        issue
            .comments
            .iter()
            .filter(|c| c.body.contains(second_body))
            .count(),
        1
    );
    assert!(
        issue
            .comments
            .iter()
            .all(|c| !(c.body.contains(first_body) && c.body.contains(second_body))),
        "dispatch results were combined into one publication"
    );
    let published = fx.op("delivery_private_list", json!({}));
    for (id, msg) in [
        (&first_delivery, &first_message),
        (&second_delivery, &second_message),
    ] {
        let row = published["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"].as_str() == Some(id.as_str()))
            .unwrap();
        assert_eq!(row["publications"].as_array().unwrap().len(), 1);
        assert_eq!(row["publications"][0]["results"], json!([msg]));
        assert_eq!(row["publications"][0]["outcome"]["state"], "published");
    }
    assert!(
        fx.op_err("delivery_publish", publish_first).is_err(),
        "first result published twice"
    );
    assert!(
        fx.op_err("delivery_publish", publish_second).is_err(),
        "second result published twice"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&first_message)
            .unwrap()
            .unwrap()
            .to_json(),
        first_message_before,
        "first kickoff was rewritten"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "private publication changed ordinary PR loop"
    );
}

#[test]
fn historical_private_recovery_uses_exact_durable_dispatch_and_report_events() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-historical-recovery");
    with_deferred_service(&fx);
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
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope":fx.member_envelope("sub-historical-recovery"),
            "title":"historical private recovery", "request_key":"historical-recovery-acceptance",
            "issue":FIXTURE_ISSUE, "body":"private work"
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-historical-recovery", &request);
    assert!(
        fx.shared
            .store
            .assist_request_for_job(&job)
            .unwrap()
            .is_some(),
        "unique real owner request missing"
    );
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let ledger_path = fx.dir.path().join("delivery-private.json");
    // Keep the real private ledger unavailable through BOTH dispatches and
    // completions; the fixture never fabricates a delivery or result.
    std::fs::create_dir(&ledger_path).unwrap();
    let first = fx.op(
        "task_dispatch",
        json!({"task":task,"to":rt.worker,"by":"operator"}),
    );
    assert_eq!(first["private_delivery"]["state"], "unavailable");
    let first_message = first["message"].as_str().unwrap().to_string();
    let first_revision = first["task"]["revision"].as_i64().unwrap();
    let first_msg = fx.shared.store.message(&first_message).unwrap().unwrap();
    let first_sha = "1111111111111111111111111111111111111111";
    let first_body = "HISTORICAL_FIRST_PRIVATE_RESULT_2215";
    fx.shared
        .store
        .finish(
            &first_msg,
            "completed",
            &json!({"status":"completed","sha":first_sha,"text":first_body}),
            None,
        )
        .unwrap();
    let verdict = fx.op(
        "task_verdict",
        json!({"task":task,"sha":first_sha,"verdict":"pass"}),
    );
    assert_eq!(verdict["private_delivery"]["state"], "unavailable");
    let reopened = fx.op("task_reopen", json!({"task":task}));
    assert_eq!(reopened["task"]["revision"], 0);
    let second = fx.op(
        "task_dispatch",
        json!({"task":task,"to":rt.worker,"by":"operator"}),
    );
    assert_eq!(second["private_delivery"]["state"], "unavailable");
    let second_message = second["message"].as_str().unwrap().to_string();
    let second_revision = second["task"]["revision"].as_i64().unwrap();
    assert_eq!(
        second_revision, first_revision,
        "fixture must reuse revision"
    );
    assert_ne!(
        second_message, first_message,
        "fixture must advance the kickoff pointer"
    );
    let second_msg = fx.shared.store.message(&second_message).unwrap().unwrap();
    let second_sha = "2222222222222222222222222222222222222222";
    let second_body = "HISTORICAL_SECOND_PRIVATE_RESULT_2215";
    fx.shared
        .store
        .finish(
            &second_msg,
            "completed",
            &json!({"status":"completed","sha":second_sha,"text":second_body}),
            None,
        )
        .unwrap();
    let verdict = fx.op(
        "task_verdict",
        json!({"task":task,"sha":second_sha,"verdict":"pass"}),
    );
    assert_eq!(verdict["private_delivery"]["state"], "unavailable");
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    let first_before = fx
        .shared
        .store
        .message(&first_message)
        .unwrap()
        .unwrap()
        .to_json();
    let second_before = fx
        .shared
        .store
        .message(&second_message)
        .unwrap()
        .unwrap()
        .to_json();
    let tracker_before = fx.tree();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();

    // Read the real producer events, never infer a historical revision or
    // SHA from the current task pointer. Exactly one dispatch/report pair
    // for each message must exist before the deliberate fault below.
    let db = rusqlite::Connection::open(fx.dir.path().join("cadence.sqlite3")).unwrap();
    let mut stmt = db.prepare("SELECT seq,kind,payload FROM events WHERE task_id=?1 AND kind IN ('task_dispatched','task_reported') ORDER BY seq").unwrap();
    let events: Vec<(i64, String, Value)> = stmt
        .query_map([&task], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .map(|r| {
            let (seq, kind, payload) = r.unwrap();
            (seq, kind, serde_json::from_str(&payload).unwrap())
        })
        .collect();
    let mut first_report_seq = None;
    let mut first_dispatch_seq = None;
    for (message, revision, sha) in [
        (&first_message, first_revision, first_sha),
        (&second_message, second_revision, second_sha),
    ] {
        for kind in ["task_dispatched", "task_reported"] {
            let matched: Vec<_> = events
                .iter()
                .filter(|(_, k, v)| k == kind && v["message"].as_str() == Some(message.as_str()))
                .collect();
            assert_eq!(
                matched.len(),
                1,
                "native {kind} event missing or ambiguous for {message}"
            );
            assert_eq!(matched[0].2["task"], task);
            assert_eq!(matched[0].2["job"], job);
            assert_eq!(matched[0].2["revision"].as_i64(), Some(revision));
            if kind == "task_dispatched" {
                assert_eq!(
                    matched[0].2["project"], "fixture",
                    "native dispatch did not record authoritative project"
                );
            }
            if kind == "task_dispatched" && message == &first_message {
                first_dispatch_seq = Some(matched[0].0);
            }
            if kind == "task_reported" {
                assert_eq!(matched[0].2["sha"].as_str(), Some(sha));
                if message == &first_message {
                    first_report_seq = Some(matched[0].0);
                }
            }
        }
    }
    drop(stmt);
    let report_seq = first_report_seq.unwrap();
    std::fs::remove_dir(&ledger_path).unwrap();
    // Both dispatches were produced under fixture/FIX-1 while their
    // ledger writes failed. Move that actual issue under the real tracker
    // writer lock before any recovery: neither the historical nor the
    // current pointer may be rebound to a new project.
    let pm = crate::issue::Pm::at(&fx.pm()).unwrap();
    let old_issue = fx.pm().join("fixture").join(FIXTURE_ISSUE);
    let moved_project = fx.pm().join("historical-moved");
    std::fs::create_dir_all(&moved_project).unwrap();
    std::fs::write(
        moved_project.join("project.yaml"),
        "key: historical-moved\nprefix: MOV\n",
    )
    .unwrap();
    let moved_issue = moved_project.join(FIXTURE_ISSUE);
    let guard = pm
        .lock_for_paths(&[old_issue.clone(), moved_project.clone()])
        .unwrap();
    std::fs::rename(&old_issue, &moved_issue).unwrap();
    drop(guard);
    assert_eq!(
        crate::issue::board::find_issue(&fx.pm(), FIXTURE_ISSUE)
            .unwrap()
            .project,
        "historical-moved",
        "fixture issue did not move"
    );
    let moved_tracker = fx.tree();
    let before_moved_reconcile = fx.op("delivery_private_list", json!({}));
    let moved = fx.op("delivery_private_reconcile", json!({"job":job}));
    let unavailable = moved["unavailable"]
        .as_array()
        .expect("moved recovery omitted unavailable report");
    assert!(
        unavailable
            .iter()
            .filter(|r| r["job"] == job && r["task"] == task)
            .count()
            >= 2,
        "both old and current dispatch recovery must report unavailable: {moved}"
    );
    assert_eq!(
        fx.op("delivery_private_list", json!({})),
        before_moved_reconcile,
        "moved recovery created or relabeled a private delivery"
    );
    assert_eq!(
        fx.tree(),
        moved_tracker,
        "moved recovery changed shared tracker"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "moved recovery changed task"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&first_message)
            .unwrap()
            .unwrap()
            .to_json(),
        first_before,
        "moved recovery changed historical message"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&second_message)
            .unwrap()
            .unwrap()
            .to_json(),
        second_before,
        "moved recovery changed current message"
    );
    let guard = pm
        .lock_for_paths(&[old_issue.clone(), moved_project.clone()])
        .unwrap();
    std::fs::rename(&moved_issue, &old_issue).unwrap();
    std::fs::remove_file(moved_project.join("project.yaml")).unwrap();
    std::fs::remove_dir(&moved_project).unwrap();
    drop(guard);
    assert_eq!(
        crate::issue::board::find_issue(&fx.pm(), FIXTURE_ISSUE)
            .unwrap()
            .project,
        "fixture",
        "fixture issue did not restore"
    );
    assert_eq!(
        fx.tree(),
        tracker_before,
        "move fixture left tracker changes after restoration"
    );
    // A completed native message whose dispatch event is missing must not
    // silently disappear from an event-only recovery scan.
    let dispatch_seq = first_dispatch_seq.unwrap();
    assert_eq!(db.execute("UPDATE events SET kind='fixture-hidden-task-dispatched' WHERE seq=?1 AND kind='task_dispatched'", [dispatch_seq]).unwrap(), 1);
    let missing_dispatch_args = json!({"job":job});
    match fx.op_err("delivery_private_reconcile", missing_dispatch_args) {
        Err(e) => assert!(
            !e.to_string().to_lowercase().contains("unknown method"),
            "reconcile RPC missing: {e}"
        ),
        Ok(value) => assert!(
            value["unavailable"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|r| r["job"] == job && r["task"] == task)),
            "missing dispatch event did not surface unavailable: {value}"
        ),
    }
    let partial_dispatch = fx.op("delivery_private_list", json!({}));
    assert!(
        !partial_dispatch["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["origin"]["dispatch"] == first_message),
        "missing dispatch event fabricated historical delivery: {partial_dispatch}"
    );
    assert!(
        !partial_dispatch.to_string().contains(first_body),
        "missing dispatch event exposed first result: {partial_dispatch}"
    );
    assert_eq!(
        fx.tree(),
        tracker_before,
        "missing-dispatch recovery published work"
    );
    assert_eq!(db.execute("UPDATE events SET kind='task_dispatched' WHERE seq=?1 AND kind='fixture-hidden-task-dispatched'", [dispatch_seq]).unwrap(), 1);
    // Hide one exact native report event; preserve its payload/sequence for
    // restoration. Recovery must not infer first SHA from current task.
    assert_eq!(db.execute("UPDATE events SET kind='fixture-hidden-task-reported' WHERE seq=?1 AND kind='task_reported'", [report_seq]).unwrap(), 1);
    let args = json!({"job":job});
    match fx.op_err("delivery_private_reconcile", args.clone()) {
        Err(e) => assert!(
            !e.to_string().to_lowercase().contains("unknown method"),
            "reconcile RPC missing: {e}"
        ),
        Ok(value) => assert!(
            value["unavailable"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|r| r["job"] == job && r["task"] == task)),
            "missing report did not surface unavailable: {value}"
        ),
    }
    let partial = fx.op("delivery_private_list", json!({}));
    assert!(
        !partial["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["origin"]["dispatch"] == first_message),
        "missing report fabricated a first historical delivery: {partial}"
    );
    assert!(
        !partial.to_string().contains(first_body),
        "missing report leaked or invented first result: {partial}"
    );
    assert_eq!(
        fx.tree(),
        tracker_before,
        "failed historical recovery published work"
    );
    assert_eq!(db.execute("UPDATE events SET kind='task_reported' WHERE seq=?1 AND kind='fixture-hidden-task-reported'", [report_seq]).unwrap(), 1);

    let recovered = fx.op("delivery_private_reconcile", args.clone());
    assert!(
        recovered["unavailable"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "restored evidence not recoverable: {recovered}"
    );
    let list = fx.op("delivery_private_list", json!({}));
    let rows = list["deliveries"].as_array().unwrap();
    let mut ids = Vec::new();
    for (message, revision, sha, body) in [
        (&first_message, first_revision, first_sha, first_body),
        (&second_message, second_revision, second_sha, second_body),
    ] {
        let found: Vec<_> = rows
            .iter()
            .filter(|r| {
                r["origin"]["job"] == job
                    && r["origin"]["task"] == task
                    && r["origin"]["dispatch"].as_str() == Some(message.as_str())
            })
            .collect();
        assert_eq!(
            found.len(),
            1,
            "historical dispatch missing or duplicated: {list}"
        );
        let row = found[0];
        assert_eq!(row["origin"]["revision"].as_i64(), Some(revision));
        assert_eq!(row["state"], "completed");
        assert_eq!(row["results"].as_array().unwrap().len(), 1);
        assert_eq!(
            row["results"][0]["message"].as_str(),
            Some(message.as_str())
        );
        assert_eq!(row["results"][0]["sha"].as_str(), Some(sha));
        assert_eq!(row["results"][0]["body"].as_str(), Some(body));
        assert!(
            row["publications"].as_array().unwrap().is_empty(),
            "reconcile published private work"
        );
        ids.push(row["id"].as_str().unwrap().to_string());
    }
    assert_ne!(
        ids[0], ids[1],
        "same-revision dispatches shared historical delivery id"
    );
    let replay = fx.op("delivery_private_reconcile", args);
    assert!(
        replay["unavailable"].as_array().is_some_and(Vec::is_empty),
        "reconcile replay failed: {replay}"
    );
    assert_eq!(
        fx.op("delivery_private_list", json!({})),
        list,
        "replay changed or duplicated historical results"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "historical recovery reran task"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&first_message)
            .unwrap()
            .unwrap()
            .to_json(),
        first_before,
        "historical recovery rewrote first result"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&second_message)
            .unwrap()
            .unwrap()
            .to_json(),
        second_before,
        "historical recovery rewrote second result"
    );
    assert_eq!(
        fx.tree(),
        tracker_before,
        "historical recovery wrote shared tracker"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "historical recovery changed PR state"
    );
}

#[test]
fn assist_dispatch_rechecks_project_after_held_live_authority_pull() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-dispatch-project-race");
    with_deferred_service(&fx);
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
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope":fx.member_envelope("sub-dispatch-project-race"),
            "title":"project policy dispatch race", "request_key":"project-dispatch-race",
            "issue":FIXTURE_ISSUE, "body":"private work"
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-dispatch-project-race", &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let actor = crate::project_policy::policy_user_actor("sub-dispatch-project-race");
    let scope = fx.shared.project_scope_for_actor(&actor).unwrap().unwrap();
    assert!(
        scope.0.contains("fixture"),
        "owner initially lacks project access"
    );
    let policy = fx.pm().join("fixture/project.yaml");
    let old_policy = std::fs::read(&policy).unwrap();
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    let db = rusqlite::Connection::open(fx.dir.path().join("cadence.sqlite3")).unwrap();
    let counts = || -> (i64, i64) {
        (
            db.query_row(
                "SELECT COUNT(*) FROM messages WHERE task_id=?1",
                [&task],
                |r| r.get(0),
            )
            .unwrap(),
            db.query_row(
                "SELECT COUNT(*) FROM events WHERE task_id=?1",
                [&task],
                |r| r.get(0),
            )
            .unwrap(),
        )
    };
    let counts_before = counts();
    let ledger_before = std::fs::read(fx.dir.path().join("delivery-private.json")).ok();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();

    // The production pre-capture precedes this genuine service-authenticated
    // platform revalidation pull. Pause the platform answer, not the store
    // guard, and change the actual tracker policy under its writer lock.
    fx.authority.lock().unwrap().hold_next = true;
    let shared = Arc::clone(&fx.shared);
    let task_for_thread = task.clone();
    let worker = rt.worker.clone();
    let dispatch = std::thread::spawn(move || {
        scoped(Asserted::Operator, || {
            shared.dispatch(
                "task_dispatch",
                &json!({"task":task_for_thread,"to":worker,"by":"operator"}),
                std::process::id(),
            )
        })
    });
    let entered = {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut state = fx.authority.lock().unwrap();
        while !state.held && std::time::Instant::now() < deadline {
            let (next, _) = fx
                .barrier
                .wait_timeout(state, Duration::from_millis(100))
                .unwrap();
            state = next;
        }
        state.held
    };
    // Even a failed barrier observation must release the real pull.
    if !entered {
        fx.release_barrier();
        let _ = dispatch.join();
        panic!("actual dispatch never reached the held platform revalidation pull");
    }
    let pm = crate::issue::Pm::at(&fx.pm()).unwrap();
    let project_dir = fx.pm().join("fixture");
    let guard = pm
        .lock_for_paths(std::slice::from_ref(&project_dir))
        .expect("real tracker writer lock");
    std::fs::write(
        &policy,
        "key: fixture\nprefix: FIX\npolicy:\n  version: 1\n  visibility: private\n",
    )
    .unwrap();
    drop(guard);
    let revoked_scope = fx.shared.project_scope_for_actor(&actor).unwrap().unwrap();
    assert!(
        !revoked_scope.0.contains("fixture"),
        "real policy change did not revoke owner access"
    );
    let tracker_after_policy = fx.tree();
    fx.release_barrier();
    let refused = dispatch.join().expect("dispatch thread panicked");
    assert!(
        refused.is_err(),
        "kickoff committed after owner lost project access: {refused:?}"
    );
    assert!(
        !refused
            .unwrap_err()
            .to_string()
            .to_lowercase()
            .contains("unknown method"),
        "task_dispatch endpoint missing"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "refused dispatch changed task/revision"
    );
    assert_eq!(
        counts(),
        counts_before,
        "refused dispatch created a message or task event"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery-private.json")).ok(),
        ledger_before,
        "refused dispatch wrote private ledger"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "refused dispatch changed ordinary PR state"
    );
    assert_eq!(
        fx.tree(),
        tracker_after_policy,
        "refused dispatch wrote shared tracker"
    );

    // Restore the exact policy under the same real lock. A fresh ordinary
    // no-race call must dispatch the same task, proving this is not a blanket
    // refusal or a fabricated member/worker fixture.
    let guard = pm.lock_for_paths(&[project_dir]).unwrap();
    std::fs::write(&policy, old_policy).unwrap();
    drop(guard);
    assert!(fx
        .shared
        .project_scope_for_actor(&actor)
        .unwrap()
        .unwrap()
        .0
        .contains("fixture"));
    let accepted = fx.op(
        "task_dispatch",
        json!({"task":task,"to":rt.worker,"by":"operator"}),
    );
    assert_eq!(accepted["task"]["id"], task);
    assert_eq!(
        accepted["task"]["revision"], 1,
        "refusal consumed a dispatch revision"
    );
    let message = accepted["message"]
        .as_str()
        .expect("authorized dispatch has no kickoff");
    let native = fx
        .shared
        .store
        .message(message)
        .unwrap()
        .expect("authorized kickoff missing");
    assert_eq!(native.source, "job_dispatch");
    assert_eq!(native.task_id.as_deref(), Some(task.as_str()));
    assert_eq!(native.issue.as_deref(), Some(FIXTURE_ISSUE));
    assert_eq!(native.alias, rt.worker);
    assert_eq!(
        accepted["private_delivery"]["state"], "opened",
        "authorized private delivery did not open"
    );
}

#[test]
fn raw_assist_dispatch_cannot_bypass_dispatch_time_project_proof() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-raw-dispatch-project");
    with_deferred_service(&fx);
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
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope":fx.member_envelope("sub-raw-dispatch-project"),
            "title":"raw project gate", "request_key":"raw-project-gate",
            "issue":FIXTURE_ISSUE, "body":"private work"
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-raw-dispatch-project", &request);
    let provenance = fx
        .shared
        .store
        .assist_request_for_job(&job)
        .unwrap()
        .expect("unique genuine owner request missing");
    let binding = fx
        .shared
        .store
        .assistant_binding(&provenance.org, &provenance.sub)
        .unwrap()
        .expect("genuine member binding missing");
    assert!(binding.is_active());
    assert_eq!(provenance.org, ORG);
    assert_eq!(provenance.sub, "sub-raw-dispatch-project");
    assert_eq!(
        provenance.credential_id.as_deref(),
        Some("cred-sub-raw-dispatch-project")
    );
    let expect = crate::store::AssistDispatchExpect {
        org: provenance.org.clone(),
        sub: provenance.sub.clone(),
        assistant_id: binding.assistant_id.clone(),
        alias: binding.alias.clone(),
        credential_id: provenance.credential_id.clone(),
        job: job.clone(),
    };
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    assert_eq!(task_before["state"], "draft");
    assert_eq!(task_before["revision"], 0);
    let worker = fx.shared.store.agent(&rt.worker).unwrap();
    let db = rusqlite::Connection::open(fx.dir.path().join("cadence.sqlite3")).unwrap();
    let counts = || -> (i64, i64) {
        (
            db.query_row(
                "SELECT COUNT(*) FROM messages WHERE task_id=?1",
                [&task],
                |r| r.get(0),
            )
            .unwrap(),
            db.query_row(
                "SELECT COUNT(*) FROM events WHERE task_id=?1",
                [&task],
                |r| r.get(0),
            )
            .unwrap(),
        )
    };
    let counts_before = counts();
    let ledger_before = std::fs::read(fx.dir.path().join("delivery-private.json")).ok();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let tracker_before = fx.tree();
    // Each raw store entry has the *real* exact member expectation, but
    // cannot carry the daemon-prepared, lock-proven dispatch project.
    // It must refuse at that exact gate, not at a fabricated identity.
    let attempts = [
        fx.shared.store.dispatch_task_expecting(
            &task,
            Some(&rt.worker),
            Some("fixture-raw-project-a"),
            "operator",
            &expect,
        ),
        fx.shared.store.dispatch_task_expecting_for(
            &task,
            Some(&rt.worker),
            Some("fixture-raw-project-b"),
            "operator",
            &expect,
            Some(&worker),
        ),
        fx.shared.store.dispatch_task_expecting_with_stall(
            &task,
            Some(&rt.worker),
            Some("fixture-raw-project-c"),
            "operator",
            &expect,
            Some(60),
        ),
    ];
    for result in attempts {
        let error = result
            .expect_err("raw assist dispatch bypassed project proof")
            .to_string();
        assert!(
            error.contains("no proven dispatch-time project"),
            "raw dispatch refused for an unrelated reason: {error}"
        );
        assert_eq!(
            fx.shared.store.task(&task).unwrap().to_json(),
            task_before,
            "raw refusal changed task/revision"
        );
        assert_eq!(
            counts(),
            counts_before,
            "raw refusal wrote kickoff or task event"
        );
        assert_eq!(
            std::fs::read(fx.dir.path().join("delivery-private.json")).ok(),
            ledger_before,
            "raw refusal wrote private ledger"
        );
        assert_eq!(
            std::fs::read(fx.dir.path().join("delivery.json")).ok(),
            ordinary_before,
            "raw refusal changed ordinary PR state"
        );
        assert_eq!(fx.tree(), tracker_before, "raw refusal changed tracker");
    }
    for id in [
        "fixture-raw-project-a",
        "fixture-raw-project-b",
        "fixture-raw-project-c",
    ] {
        assert!(
            fx.shared.store.message(id).unwrap().is_none(),
            "refused raw kickoff exists: {id}"
        );
    }
    // The real daemon path performs its current authority/project proof and
    // succeeds against exactly the same member, task and worker.
    let positive = fx.op(
        "task_dispatch",
        json!({"task":task,"to":rt.worker,"by":"operator"}),
    );
    assert_eq!(positive["task"]["id"], task);
    assert_eq!(positive["task"]["revision"], 1);
    assert_eq!(positive["private_delivery"]["state"], "opened");
    let kickoff = positive["message"]
        .as_str()
        .expect("authorized kickoff missing");
    let message = fx.shared.store.message(kickoff).unwrap().unwrap();
    assert_eq!(message.source, "job_dispatch");
    assert_eq!(message.task_id.as_deref(), Some(task.as_str()));
    assert_eq!(message.issue.as_deref(), Some(FIXTURE_ISSUE));
    assert_eq!(message.alias, rt.worker);
    let mut stmt = db
        .prepare("SELECT payload FROM events WHERE kind='task_dispatched' AND task_id=?1")
        .unwrap();
    let events: Vec<Value> = stmt
        .query_map([&task], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|payload| serde_json::from_str(&payload.unwrap()).unwrap())
        .collect();
    let matching: Vec<_> = events
        .iter()
        .filter(|e| e["message"].as_str() == Some(kickoff))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "authorized dispatch event missing or duplicated"
    );
    assert_eq!(matching[0]["project"], "fixture");
}

#[test]
fn historical_recovery_distinguishes_explicit_null_sha_from_missing_or_wrong_type() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-historical-null-sha");
    with_deferred_service(&fx);
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
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope":fx.member_envelope("sub-historical-null-sha"),
            "title":"historical null sha", "request_key":"historical-null-sha",
            "issue":FIXTURE_ISSUE, "body":"private work"
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-historical-null-sha", &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let ledger_path = fx.dir.path().join("delivery-private.json");
    std::fs::create_dir(&ledger_path).unwrap();
    let first = fx.op(
        "task_dispatch",
        json!({"task":task,"to":rt.worker,"by":"operator"}),
    );
    assert_eq!(first["private_delivery"]["state"], "unavailable");
    let first_message = first["message"].as_str().unwrap().to_string();
    let first_revision = first["task"]["revision"].as_i64().unwrap();
    let original_message = fx.shared.store.message(&first_message).unwrap().unwrap();
    let marker = "HISTORICAL_NULL_SHA_PRIVATE_RESULT_2215";
    // Genuine completion omits a SHA and contains no SHA-shaped text; the
    // store itself writes task_reported.sha = null in the same transaction.
    fx.shared
        .store
        .finish(
            &original_message,
            "completed",
            &json!({"status":"completed","text":marker}),
            None,
        )
        .unwrap();
    assert_eq!(fx.shared.store.task(&task).unwrap().head_sha, None);
    // A no-SHA report cannot pass a SHA verdict. The real operator marks it
    // failed, then uses the supported failed->draft reopen path.
    let failed = fx.op(
        "task_fail",
        json!({"task":task,"reason":"rescope after no commit","by":"operator"}),
    );
    assert_eq!(failed["task"]["state"], "failed");
    let reopened = fx.op("task_reopen", json!({"task":task}));
    assert_eq!(reopened["task"]["revision"], 0);
    let second = fx.op(
        "task_dispatch",
        json!({"task":task,"to":rt.worker,"by":"operator"}),
    );
    assert_eq!(second["private_delivery"]["state"], "unavailable");
    let second_message = second["message"].as_str().unwrap().to_string();
    assert_ne!(second_message, first_message);
    assert_eq!(second["task"]["revision"].as_i64(), Some(first_revision));
    let current_before = fx.shared.store.task(&task).unwrap().to_json();
    let first_before = fx
        .shared
        .store
        .message(&first_message)
        .unwrap()
        .unwrap()
        .to_json();
    let second_before = fx
        .shared
        .store
        .message(&second_message)
        .unwrap()
        .unwrap()
        .to_json();
    let tracker_before = fx.tree();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let db = rusqlite::Connection::open(fx.dir.path().join("cadence.sqlite3")).unwrap();
    let mut stmt = db
        .prepare(
            "SELECT seq,payload FROM events WHERE task_id=?1 AND kind='task_reported' ORDER BY seq",
        )
        .unwrap();
    let reports: Vec<(i64, String)> = stmt
        .query_map([&task], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    drop(stmt);
    let matched: Vec<_> = reports
        .iter()
        .filter(|(_, raw)| {
            serde_json::from_str::<Value>(raw).unwrap()["message"].as_str()
                == Some(first_message.as_str())
        })
        .collect();
    assert_eq!(
        matched.len(),
        1,
        "real null-SHA report missing or ambiguous"
    );
    let report_seq = matched[0].0;
    let original_payload = matched[0].1.clone();
    let original_event: Value = serde_json::from_str(&original_payload).unwrap();
    assert_eq!(original_event["task"], task);
    assert_eq!(original_event["job"], job);
    assert_eq!(original_event["revision"].as_i64(), Some(first_revision));
    assert!(
        original_event.get("sha").is_some_and(Value::is_null),
        "producer did not emit an explicit null sha: {original_event}"
    );
    std::fs::remove_dir(&ledger_path).unwrap();
    for case in ["missing", "wrong-type"] {
        let mut damaged = original_event.clone();
        let fields = damaged.as_object_mut().unwrap();
        if case == "missing" {
            fields.remove("sha");
        } else {
            fields.insert("sha".into(), json!(17));
        }
        assert_eq!(
            db.execute(
                "UPDATE events SET payload=?1 WHERE seq=?2 AND kind='task_reported'",
                rusqlite::params![damaged.to_string(), report_seq]
            )
            .unwrap(),
            1
        );
        let result = fx.op_err("delivery_private_reconcile", json!({"job":job}));
        match result {
            Err(e) => assert!(
                !e.to_string().to_lowercase().contains("unknown method"),
                "reconcile RPC missing: {e}"
            ),
            Ok(value) => assert!(
                value["unavailable"]
                    .as_array()
                    .is_some_and(|rows| rows.iter().any(|r| r["job"] == job && r["task"] == task)),
                "{case} sha did not report unavailable: {value}"
            ),
        }
        let list = fx.op("delivery_private_list", json!({}));
        assert!(
            !list["deliveries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["origin"]["dispatch"] == first_message),
            "{case} sha fabricated historical delivery: {list}"
        );
        assert!(
            !list.to_string().contains(marker),
            "{case} sha returned historical body: {list}"
        );
        assert_eq!(
            fx.tree(),
            tracker_before,
            "{case} sha recovery changed tracker"
        );
    }
    assert_eq!(
        db.execute(
            "UPDATE events SET payload=?1 WHERE seq=?2 AND kind='task_reported'",
            rusqlite::params![original_payload, report_seq]
        )
        .unwrap(),
        1
    );
    let recovered = fx.op("delivery_private_reconcile", json!({"job":job}));
    assert!(
        recovered["unavailable"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "real null-SHA evidence was not recoverable: {recovered}"
    );
    let list = fx.op("delivery_private_list", json!({}));
    let found: Vec<_> = list["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| {
            r["origin"]["job"] == job
                && r["origin"]["task"] == task
                && r["origin"]["dispatch"] == first_message
        })
        .collect();
    assert_eq!(
        found.len(),
        1,
        "historical null-SHA delivery missing or duplicated: {list}"
    );
    let row = found[0];
    assert_eq!(row["origin"]["revision"].as_i64(), Some(first_revision));
    assert_eq!(row["state"], "completed");
    assert_eq!(row["results"].as_array().unwrap().len(), 1);
    assert_eq!(row["results"][0]["message"], first_message);
    assert_eq!(row["results"][0]["body"], marker);
    assert!(
        row["results"][0].get("sha").is_some_and(Value::is_null),
        "recovery fabricated a SHA: {row}"
    );
    assert!(
        row["publications"].as_array().unwrap().is_empty(),
        "recovery published private work"
    );
    let replay = fx.op("delivery_private_reconcile", json!({"job":job}));
    assert!(replay["unavailable"].as_array().is_some_and(Vec::is_empty));
    assert_eq!(
        fx.op("delivery_private_list", json!({})),
        list,
        "replay duplicated or rewrote null-SHA result"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        current_before,
        "recovery reran task"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&first_message)
            .unwrap()
            .unwrap()
            .to_json(),
        first_before
    );
    assert_eq!(
        fx.shared
            .store
            .message(&second_message)
            .unwrap()
            .unwrap()
            .to_json(),
        second_before
    );
    assert_eq!(fx.tree(), tracker_before, "recovery changed shared tracker");
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "recovery changed PR state"
    );
}

#[test]
fn duplicate_issue_fails_dispatch_recovery_and_publication_until_resolved() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-duplicate-private-issue");
    with_deferred_service(&fx);
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
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope":fx.member_envelope("sub-duplicate-private-issue"),
            "title":"duplicate issue private work", "request_key":"duplicate-private-issue",
            "issue":FIXTURE_ISSUE, "body":"private work"
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-duplicate-private-issue", &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    let db = rusqlite::Connection::open(fx.dir.path().join("cadence.sqlite3")).unwrap();
    let counts = || -> (i64, i64) {
        (
            db.query_row(
                "SELECT COUNT(*) FROM messages WHERE task_id=?1",
                [&task],
                |r| r.get(0),
            )
            .unwrap(),
            db.query_row(
                "SELECT COUNT(*) FROM events WHERE task_id=?1",
                [&task],
                |r| r.get(0),
            )
            .unwrap(),
        )
    };
    let counts_before = counts();
    let ledger_before = std::fs::read(fx.dir.path().join("delivery-private.json")).ok();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let pm = crate::issue::Pm::at(&fx.pm()).unwrap();
    let original_issue = fx.pm().join("fixture").join(FIXTURE_ISSUE);
    let duplicate_project = fx.pm().join("duplicate");
    std::fs::create_dir_all(&duplicate_project).unwrap();
    std::fs::write(
        duplicate_project.join("project.yaml"),
        "key: duplicate\nprefix: DUP\n",
    )
    .unwrap();
    let duplicate_issue = duplicate_project.join(FIXTURE_ISSUE);
    let plant = || {
        let guard = pm
            .lock_for_paths(&[original_issue.clone(), duplicate_project.clone()])
            .unwrap();
        std::fs::create_dir(&duplicate_issue).unwrap();
        std::fs::copy(
            original_issue.join("issue.md"),
            duplicate_issue.join("issue.md"),
        )
        .unwrap();
        drop(guard);
        assert!(
            crate::issue::write::issue_dir(&pm, FIXTURE_ISSUE).is_err(),
            "actual duplicate did not make issue lookup ambiguous"
        );
    };
    let clear = || {
        let guard = pm
            .lock_for_paths(&[original_issue.clone(), duplicate_project.clone()])
            .unwrap();
        std::fs::remove_file(duplicate_issue.join("issue.md")).unwrap();
        std::fs::remove_dir(&duplicate_issue).unwrap();
        drop(guard);
        assert_eq!(
            crate::issue::write::issue_dir(&pm, FIXTURE_ISSUE)
                .unwrap()
                .0
                .key,
            "fixture"
        );
    };
    plant();
    let duplicate_tracker = fx.tree();
    let refused = fx.op_err(
        "task_dispatch",
        json!({"task":task,"to":rt.worker,"by":"operator"}),
    );
    assert!(
        refused.is_err(),
        "ambiguous actual issue dispatched private work: {refused:?}"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "ambiguous dispatch changed task/revision"
    );
    assert_eq!(
        counts(),
        counts_before,
        "ambiguous dispatch created kickoff/event"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery-private.json")).ok(),
        ledger_before,
        "ambiguous dispatch created ledger"
    );
    assert_eq!(
        fx.tree(),
        duplicate_tracker,
        "ambiguous dispatch changed tracker"
    );
    clear();

    // Positive: the same genuine request/task dispatches once the duplicate
    // is removed, and its result is produced by the actual worker finish path.
    let dispatch = fx.op(
        "task_dispatch",
        json!({"task":task,"to":rt.worker,"by":"operator"}),
    );
    let message = dispatch["message"]
        .as_str()
        .expect("valid dispatch missing kickoff")
        .to_string();
    let msg = fx.shared.store.message(&message).unwrap().unwrap();
    let marker = "DUPLICATE_ISSUE_PRIVATE_RESULT_2215";
    let sha = "1234567890abcdef1234567890abcdef12345678";
    fx.shared
        .store
        .finish(
            &msg,
            "completed",
            &json!({"status":"completed","sha":sha,"text":marker}),
            None,
        )
        .unwrap();
    fx.op(
        "task_verdict",
        json!({"task":task,"sha":sha,"verdict":"pass"}),
    );
    let list = fx.op("delivery_private_list", json!({}));
    let row = list["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["origin"]["dispatch"] == message)
        .expect("genuine private delivery missing");
    let delivery = row["id"].as_str().unwrap().to_string();
    assert_eq!(row["results"][0]["body"], marker);
    let task_after = fx.shared.store.task(&task).unwrap().to_json();
    let message_after = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .unwrap()
        .to_json();
    let ledger_valid = std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap();
    plant();
    let duplicate_tracker = fx.tree();
    let publish =
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[message]});
    assert!(
        fx.op_err("delivery_publish", publish.clone()).is_err(),
        "ambiguous issue accepted publication"
    );
    match fx.op_err("delivery_private_reconcile", json!({"job":job})) {
        Err(e) => assert!(
            !e.to_string().to_lowercase().contains("unknown method"),
            "reconcile RPC missing"
        ),
        Ok(v) => assert!(
            v["unavailable"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|r| r["job"] == job && r["task"] == task)),
            "ambiguous issue recovery did not report unavailable: {v}"
        ),
    }
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap(),
        ledger_valid,
        "ambiguous issue created/relabelled delivery or receipt"
    );
    assert_eq!(
        fx.tree(),
        duplicate_tracker,
        "ambiguous issue wrote shared tracker"
    );
    assert_eq!(fx.shared.store.task(&task).unwrap().to_json(), task_after);
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json(),
        message_after
    );
    clear();
    fx.op("delivery_publish", publish);
    let issue = crate::issue::board::find_issue(&fx.pm(), FIXTURE_ISSUE).unwrap();
    assert_eq!(
        issue
            .comments
            .iter()
            .filter(|c| c.body.contains(marker))
            .count(),
        1,
        "resolved issue positive did not publish exactly once"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "private effects changed ordinary PR state"
    );
}

#[test]
fn mismatched_origin_dispatch_cannot_create_publication_receipt() {
    let _flag = AssistantFlagOn::arm();
    let marker = "CORRUPT_ORIGIN_PRIVATE_RESULT_2215";
    let (fx, task, message, delivery) = pending_publication_fixture("sub-corrupt-origin", marker);
    let before = fx.op("delivery_private_list", json!({}));
    let row = before["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == delivery)
        .unwrap();
    assert_eq!(row["origin"]["dispatch"], message);
    assert_eq!(row["results"].as_array().unwrap().len(), 1);
    assert_eq!(row["results"][0]["message"], message);
    assert_eq!(row["results"][0]["body"], marker);
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    let message_before = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .unwrap()
        .to_json();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let path = fx.dir.path().join("delivery-private.json");
    let original = std::fs::read(&path).unwrap();
    let mut ledger: Value = serde_json::from_slice(&original).unwrap();
    // Synthetic on-disk corruption of the actual record; the result, id,
    // task and request remain the genuine ones.
    ledger["records"][&delivery]["origin"]["dispatch"] = json!("fixture-crossed-dispatch");
    let corrupted = serde_json::to_vec_pretty(&ledger).unwrap();
    std::fs::write(&path, &corrupted).unwrap();
    let tracker_before = fx.tree();
    let publish =
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[message]});
    let refused = fx.op_err("delivery_publish", publish.clone());
    assert!(
        refused.is_err(),
        "corrupted origin dispatch was published: {refused:?}"
    );
    assert!(
        !refused
            .unwrap_err()
            .to_string()
            .to_lowercase()
            .contains("unknown method"),
        "publication RPC missing"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        corrupted,
        "corrupt origin created or rewrote a pending receipt"
    );
    assert_eq!(
        fx.tree(),
        tracker_before,
        "corrupt origin wrote shared tracker"
    );
    assert_eq!(fx.shared.store.task(&task).unwrap().to_json(), task_before);
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json(),
        message_before
    );
    std::fs::write(&path, original).unwrap();
    fx.op("delivery_publish", publish);
    let issue = crate::issue::board::find_issue(&fx.pm(), FIXTURE_ISSUE).unwrap();
    assert_eq!(
        issue
            .comments
            .iter()
            .filter(|c| c.body.contains(marker))
            .count(),
        1,
        "restored genuine origin positive failed"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "publication changed ordinary PR state"
    );
}

#[test]
fn private_reconcile_refuses_missing_native_kickoff_then_recovers_exact_row() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-missing-native-kickoff");
    with_deferred_service(&fx);
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
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope":fx.member_envelope("sub-missing-native-kickoff"),
            "title":"missing kickoff recovery", "request_key":"missing-kickoff-recovery",
            "issue":FIXTURE_ISSUE, "body":"private work"
        }),
    );
    let job = promote_member_fixture_job(&fx, "sub-missing-native-kickoff", &request);
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let ledger_path = fx.dir.path().join("delivery-private.json");
    std::fs::create_dir(&ledger_path).unwrap();
    let dispatch = fx.op(
        "task_dispatch",
        json!({"task":task,"to":rt.worker,"by":"operator"}),
    );
    assert_eq!(
        dispatch["private_delivery"]["state"], "unavailable",
        "fixture did not lose ledger open"
    );
    let message = dispatch["message"]
        .as_str()
        .expect("actual kickoff missing")
        .to_string();
    let revision = dispatch["task"]["revision"].as_i64().unwrap();
    let msg = fx.shared.store.message(&message).unwrap().unwrap();
    assert_eq!(msg.source, "job_dispatch");
    assert_eq!(msg.task_id.as_deref(), Some(task.as_str()));
    assert_eq!(msg.issue.as_deref(), Some(FIXTURE_ISSUE));
    let marker = "MISSING_NATIVE_KICKOFF_PRIVATE_RESULT_2215";
    let sha = "1234567890abcdef1234567890abcdef12345678";
    fx.shared
        .store
        .finish(
            &msg,
            "completed",
            &json!({"status":"completed","sha":sha,"text":marker}),
            None,
        )
        .unwrap();
    let verdict = fx.op(
        "task_verdict",
        json!({"task":task,"sha":sha,"verdict":"pass"}),
    );
    assert_eq!(
        verdict["private_delivery"]["state"], "unavailable",
        "fixture did not lose ledger result"
    );
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    let message_before = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .unwrap()
        .to_json();
    let tracker_before = fx.tree();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    std::fs::remove_dir(&ledger_path).unwrap();

    // Hide only this fixture-owned genuine message by moving its primary
    // key; leave the task's original dispatch pointer and original durable
    // events untouched. Restore the key afterwards and compare the full
    // real row, rather than constructing a replacement message.
    let db = rusqlite::Connection::open(fx.dir.path().join("cadence.sqlite3")).unwrap();
    let event_payloads = || -> Vec<String> {
        let mut stmt = db.prepare("SELECT payload FROM events WHERE task_id=?1 AND kind='task_dispatched' ORDER BY seq").unwrap();
        let rows = stmt
            .query_map([&task], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        rows
    };
    let dispatch_events_before = event_payloads();
    let matching: Vec<Value> = dispatch_events_before
        .iter()
        .map(|p| serde_json::from_str::<Value>(p).unwrap())
        .filter(|v| v["message"].as_str() == Some(message.as_str()))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "genuine dispatch event missing or ambiguous"
    );
    assert_eq!(matching[0]["job"], job);
    assert_eq!(matching[0]["task"], task);
    assert_eq!(matching[0]["revision"].as_i64(), Some(revision));
    db.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    let hidden = "fixture-hidden-real-kickoff-2215";
    assert!(fx.shared.store.message(hidden).unwrap().is_none());
    assert_eq!(
        db.execute(
            "UPDATE messages SET id=?1 WHERE id=?2",
            rusqlite::params![hidden, message]
        )
        .unwrap(),
        1
    );
    assert!(
        fx.shared.store.message(&message).unwrap().is_none(),
        "task's exact native kickoff remains readable"
    );
    assert!(
        fx.shared.store.message(hidden).unwrap().is_some(),
        "fixture did not retain the real row for restoration"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "hiding message changed durable task pointer"
    );
    assert_eq!(
        event_payloads(),
        dispatch_events_before,
        "fixture changed durable dispatch evidence"
    );
    let args = json!({"job":job});
    match fx.op_err("delivery_private_reconcile", args.clone()) {
        Err(e) => assert!(
            !e.to_string().to_lowercase().contains("unknown method"),
            "reconcile RPC missing: {e}"
        ),
        Ok(value) => assert!(
            value["unavailable"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|r| r["job"] == job && r["task"] == task)),
            "missing kickoff did not report unavailable: {value}"
        ),
    }
    let refused_list = fx.op("delivery_private_list", json!({}));
    assert!(
        !refused_list["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["origin"]["job"] == job && r["origin"]["task"] == task),
        "missing kickoff invented a delivery/receipt: {refused_list}"
    );
    assert!(!refused_list.to_string().contains(marker));
    assert_eq!(
        fx.tree(),
        tracker_before,
        "missing-kickoff reconciliation wrote shared tracker"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "missing-kickoff reconciliation changed PR state"
    );
    assert_eq!(
        db.execute(
            "UPDATE messages SET id=?1 WHERE id=?2",
            rusqlite::params![message, hidden]
        )
        .unwrap(),
        1
    );
    assert!(fx.shared.store.message(hidden).unwrap().is_none());
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json(),
        message_before,
        "fixture failed to restore exact native message row"
    );
    assert_eq!(fx.shared.store.task(&task).unwrap().to_json(), task_before);

    let recovered = fx.op("delivery_private_reconcile", args.clone());
    assert!(
        recovered["unavailable"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "restored kickoff not recoverable: {recovered}"
    );
    let list = fx.op("delivery_private_list", json!({}));
    let rows: Vec<_> = list["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| {
            r["origin"]["job"] == job
                && r["origin"]["task"] == task
                && r["origin"]["dispatch"] == message
        })
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "restored native kickoff missing or duplicated: {list}"
    );
    assert_eq!(rows[0]["origin"]["revision"].as_i64(), Some(revision));
    assert_eq!(rows[0]["state"], "completed");
    assert_eq!(rows[0]["results"].as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["results"][0]["message"], message);
    assert_eq!(rows[0]["results"][0]["sha"], sha);
    assert_eq!(rows[0]["results"][0]["body"], marker);
    assert!(rows[0]["publications"].as_array().unwrap().is_empty());
    let replay = fx.op("delivery_private_reconcile", args);
    assert!(replay["unavailable"].as_array().is_some_and(Vec::is_empty));
    assert_eq!(
        fx.op("delivery_private_list", json!({})),
        list,
        "replay duplicated recovered result"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "recovery reran task"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json(),
        message_before,
        "recovery rewrote kickoff"
    );
    assert_eq!(fx.tree(), tracker_before, "recovery published private work");
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "recovery changed ordinary PR state"
    );
}

#[test]
fn private_publication_and_pending_reconcile_refuse_ambiguous_queue_owner() {
    let _flag = AssistantFlagOn::arm();
    let sub = "sub-publication-owner-conflict";
    let marker = "PUBLICATION_OWNER_CONFLICT_PRIVATE_2215";
    let (fx, task, message, delivery) = pending_publication_fixture(sub, marker);
    let job = fx.shared.store.task(&task).unwrap().job_id;
    let task_before = fx.shared.store.task(&task).unwrap().to_json();
    let message_before = fx
        .shared
        .store
        .message(&message)
        .unwrap()
        .unwrap()
        .to_json();
    let ordinary_before = std::fs::read(fx.dir.path().join("delivery.json")).ok();
    let db = rusqlite::Connection::open(fx.dir.path().join("cadence.sqlite3")).unwrap();
    let add_conflict = || {
        assert_eq!(db.execute(
            "INSERT INTO assist_request_queue(org,sub,request_key,issue,title,body,acceptance,credential_id,created,last_attempt,last_outcome,completed,job_id) SELECT org,'fixture-other-owner','fixture-publication-owner-conflict',issue,'synthetic conflict','','','cred-fixture-other-owner',created,0,NULL,1,job_id FROM assist_request_queue WHERE org=?1 AND sub=?2 AND request_key='pending-publication-acceptance' AND job_id=?3 AND completed=1",
            rusqlite::params![ORG, sub, job]).unwrap(), 1, "genuine completed owner queue row missing");
    };
    let remove_conflict = || {
        assert_eq!(db.execute(
            "DELETE FROM assist_request_queue WHERE org=?1 AND sub='fixture-other-owner' AND request_key='fixture-publication-owner-conflict' AND job_id=?2",
            rusqlite::params![ORG, job]).unwrap(), 1, "fixture conflict was not removed exactly once");
    };
    let publish =
        json!({"delivery":delivery,"issue":FIXTURE_ISSUE,"project":"fixture","results":[message]});
    let private = fx.op("delivery_private_list", json!({}));
    assert!(
        private.to_string().contains(&delivery) && private.to_string().contains(marker),
        "operator cannot inspect genuine delivery"
    );

    // The real result is unshared. A competing queue claim must stop the
    // first publication BEFORE it can create a pending receipt or comment.
    add_conflict();
    let ledger_before = std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap();
    let tracker_before = fx.tree();
    let inspected = fx.op("delivery_private_list", json!({}));
    assert!(
        inspected.to_string().contains(&delivery) && inspected.to_string().contains(marker),
        "operator lost recovery inspection during ambiguity"
    );
    let denied = fx.op_err("delivery_publish", publish.clone());
    assert!(
        denied.is_err(),
        "ambiguous owner accepted first publication: {denied:?}"
    );
    assert!(
        !denied
            .unwrap_err()
            .to_string()
            .to_lowercase()
            .contains("unknown method"),
        "publication RPC missing"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap(),
        ledger_before,
        "refused publication created a pending receipt"
    );
    assert_eq!(
        fx.tree(),
        tracker_before,
        "refused publication wrote shared tracker"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "refused publication changed PR loop"
    );
    remove_conflict();

    // Use the actual tracker lock to make a real authorized publication
    // attempt persist Pending then fail its write. No panic/test seam or
    // synthetic receipt is used; the original operator call returns an error.
    let pm = crate::issue::Pm::at(&fx.pm()).unwrap();
    let issue_dir = fx.pm().join("fixture").join(FIXTURE_ISSUE);
    let tracker_lock = pm.lock_for_paths(&[issue_dir]).unwrap();
    let incomplete = fx.op_err("delivery_publish", publish.clone());
    drop(tracker_lock);
    assert!(
        incomplete.is_err(),
        "held tracker lock did not produce a pending attempt: {incomplete:?}"
    );
    let ledger = crate::delivery::private::load(fx.dir.path()).unwrap();
    let row = ledger.get(&delivery).expect("private delivery missing");
    assert_eq!(
        row.publications.len(),
        1,
        "real pending attempt missing or duplicated"
    );
    let pending = &row.publications[0];
    assert!(
        matches!(
            &pending.outcome,
            crate::delivery::publication::PublicationState::Pending
        ),
        "attempt was not pending"
    );
    let attempt = pending.attempt.clone();
    assert!(!attempt.is_empty());
    let issue = crate::issue::board::find_issue(&fx.pm(), FIXTURE_ISSUE).unwrap();
    assert!(
        issue.comments.iter().all(|c| !c.body.contains(marker)),
        "pending attempt wrote a shared comment"
    );

    // Restore the competing claim while the genuine attempt is pending.
    // Proven local operators can inspect but cannot turn ambiguous owner
    // evidence into a shared side effect, even via reconcile.
    add_conflict();
    let pending_ledger = std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap();
    let pending_tracker = fx.tree();
    let inspected = fx.op("delivery_private_list", json!({}));
    assert!(inspected.to_string().contains(&delivery) && inspected.to_string().contains(marker));
    let reconcile = fx.op_err(
        "delivery_publish_reconcile",
        json!({"delivery":delivery,"attempt":attempt}),
    );
    assert!(
        reconcile.is_err(),
        "ambiguous owner reconciled a pending publication: {reconcile:?}"
    );
    assert!(
        !reconcile
            .unwrap_err()
            .to_string()
            .to_lowercase()
            .contains("unknown method"),
        "publish reconcile RPC missing"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery-private.json")).unwrap(),
        pending_ledger,
        "ambiguous reconciliation settled or rewrote receipt"
    );
    assert_eq!(
        fx.tree(),
        pending_tracker,
        "ambiguous reconciliation wrote shared tracker"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "ambiguous reconciliation changed PR loop"
    );
    remove_conflict();

    let recovered = fx.op(
        "delivery_publish_reconcile",
        json!({"delivery":delivery,"attempt":attempt}),
    );
    assert_eq!(
        recovered["state"], "published",
        "valid original owner did not recover"
    );
    let issue = crate::issue::board::find_issue(&fx.pm(), FIXTURE_ISSUE).unwrap();
    assert_eq!(
        issue
            .comments
            .iter()
            .filter(|c| c.body.contains(marker))
            .count(),
        1,
        "recovery did not produce exactly one comment"
    );
    let after = crate::delivery::private::load(fx.dir.path()).unwrap();
    let row = after.get(&delivery).unwrap();
    assert_eq!(row.publications.len(), 1);
    assert_eq!(row.publications[0].attempt, attempt);
    assert!(matches!(
        &row.publications[0].outcome,
        crate::delivery::publication::PublicationState::Published { .. }
    ));
    let tracker_after = fx.tree();
    assert_eq!(
        fx.op(
            "delivery_publish_reconcile",
            json!({"delivery":delivery,"attempt":attempt})
        )["state"],
        "published"
    );
    assert_eq!(
        fx.tree(),
        tracker_after,
        "recovery replay duplicated comment"
    );
    assert!(
        fx.op_err("delivery_publish", publish).is_err(),
        "recovered result was published twice"
    );
    assert_eq!(
        fx.tree(),
        tracker_after,
        "duplicate publication changed tracker"
    );
    assert_eq!(
        fx.shared.store.task(&task).unwrap().to_json(),
        task_before,
        "publication reran task"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&message)
            .unwrap()
            .unwrap()
            .to_json(),
        message_before,
        "publication changed durable result"
    );
    assert_eq!(
        std::fs::read(fx.dir.path().join("delivery.json")).ok(),
        ordinary_before,
        "publication changed ordinary PR state"
    );
}
