//! CAD-2215 independent signed-board same-project private-job acceptance.
//! The product implementer must not edit or weaken this check.
//! This tests HTTP/SSE authorization, not local-agent OS isolation.

use super::*;

fn board_get_status(rig: &Rig, cookie: &str, path: &str) -> (u16, Value) {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let mut response = http_agent()
        .get(&url)
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("Cookie", cookie)
        .call()
        .expect("signed board GET failed");
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_json()
        .expect("board response is not JSON");
    (status, body)
}

#[test]
fn signed_board_same_project_does_not_expose_private_job_or_foreign_assistant() {
    let _flag = AssistantFlagOn::arm();
    let (rig, events) = queue_rig(&["ana", "ben"]);
    seed(&rig);
    // Make every registered project readable to both members so the actual
    // member SSE admission is a positive control, not a project-wide denial.
    std::fs::write(
        rig.pm.join("secret/project.yaml"),
        "key: secret\nprefix: SEC\n",
    )
    .unwrap();
    let envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, requested) = rig.call("job_request", &envelope,
        &json!({"title":"CAD2215_BOARD_PRIVATE_JOB", "request_key":"cad2215-board-private", "issue":"VIS-9"}));
    assert_eq!(status, 200, "signed job request failed: {requested}");
    let job = promoted_job(&rig, &events, "ana", &requested);
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    let binding = shared
        .store
        .assistant_binding(ORG, "ana")
        .unwrap()
        .expect("owner assistant binding missing");
    let alias = binding.alias;
    assert_ne!(
        alias, "cad2215-worker",
        "assistant must not be ordinary worker"
    );
    assert!(shared.store.assist_request_owns(ORG, "ana", &job).unwrap());
    assert!(!shared.store.assist_request_owns(ORG, "ben", &job).unwrap());
    assert!(!shared.store.job_is_subscriber(&job, ORG, "ben").unwrap());
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    // Synthetic issue transition and assignment to a shared ordinary worker;
    // not a supported rebind API. Ownership is the real HTTP ledger.
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id='VIS-1' WHERE id=?1", [&job])
            .unwrap(),
        1
    );
    let task: String = db
        .query_row(
            "SELECT id FROM tasks WHERE job_id=?1 ORDER BY id LIMIT 1",
            [&job],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        db.execute(
            "UPDATE tasks SET assignee='cad2215-worker', state='running' WHERE id=?1",
            [&task]
        )
        .unwrap(),
        1
    );
    shared
        .store
        .enqueue_task(
            "cad2215-worker",
            "CAD2215_BOARD_PRIVATE_MESSAGE",
            None,
            "cad2215-board-private-message",
            "user",
            Some(&task),
        )
        .unwrap();
    db.execute(
        "UPDATE messages SET state='running' WHERE id='cad2215-board-private-message'",
        [],
    )
    .unwrap();
    db.execute("INSERT INTO events(alias,kind,payload,job_id,task_id,at) VALUES(?1,'cad2215_private_event',?2,?3,?4,1)",
        rusqlite::params!["cad2215-worker", "{\"message\":\"cad2215-board-private-message\",\"marker\":\"CAD2215_BOARD_PRIVATE_EVENT\"}", job, task]).unwrap();

    // Provenance-only fixture: these rows exercise cached message-ID
    // projection, not the cloud consent/operation authorization system.
    // All three messages bind to the same normally readable ordinary task.
    for (id, text) in [
        ("cad2215-provenance-local", "CAD2215_PROVENANCE_LOCAL"),
        ("cad2215-provenance-cloud", "CAD2215_PROVENANCE_CLOUD"),
        ("cad2215-provenance-derived", "CAD2215_PROVENANCE_DERIVED"),
    ] {
        shared
            .store
            .enqueue_task(
                "cad2215-worker",
                text,
                None,
                id,
                "user",
                Some("cad2215-visible-task"),
            )
            .unwrap();
    }
    assert_eq!(db.execute("UPDATE messages SET state='running' WHERE id IN ('cad2215-provenance-local','cad2215-provenance-cloud','cad2215-provenance-derived')", []).unwrap(), 3);
    assert_eq!(
        db.execute(
            "UPDATE messages SET source='cloud_agent' WHERE id='cad2215-provenance-cloud'",
            []
        )
        .unwrap(),
        1
    );
    assert_eq!(db.execute("UPDATE messages SET origin='cad2215-provenance-cloud' WHERE id='cad2215-provenance-derived'", []).unwrap(), 1);

    let ana = login(&rig, "ana", "member");
    let ben = login(&rig, "ben", "member");
    let operator = login(&rig, "cad2215-board-operator", "owner");
    let env = rig.envelope("ana", json!({}), json!({}));
    let (status, evidence) = rig.call("job_evidence", &env, &json!({"job":job}));
    assert_eq!(status, 200, "real owner evidence unavailable: {evidence}");
    let env = rig.envelope("ben", json!({}), json!({}));
    let (status, _) = rig.call("job_evidence", &env, &json!({"job":job}));
    assert!(
        status == 401 || status == 403 || status == 404,
        "non-owner reached evidence"
    );

    for path in ["/api/agents", "/api/agents/cad2215-worker", "/api/overview"] {
        let (owner_status, owner_body) = board_get_status(&rig, &ana, path);
        let (operator_status, operator_body) = board_get_status(&rig, &operator, path);
        let (ben_status, ben_body) = board_get_status(&rig, &ben, path);
        assert_eq!(owner_status, 200, "owner {path} refused");
        assert_eq!(operator_status, 200, "operator {path} refused");
        assert_eq!(ben_status, 200, "member {path} refused");
        let owner_text = owner_body.to_string();
        let operator_text = operator_body.to_string();
        let ben_text = ben_body.to_string();
        if path != "/api/overview" {
            for (who, text) in [("owner", &owner_text), ("operator", &operator_text)] {
                assert!(text.contains("cad2215-worker"), "{who} {path} lost worker");
                assert!(
                    text.contains(&task),
                    "{who} {path} lost authorized private task"
                );
            }
            assert!(
                ben_text.contains("cad2215-worker"),
                "Ben lost ordinary worker"
            );
            assert!(
                ben_text.contains("cad2215-visible-task"),
                "Ben lost authorized ordinary task"
            );
        }
        if path == "/api/agents" {
            // Synthetic provenance rows have no proven cloud owner. Both
            // viewers retain ordinary local mail but neither may claim these
            // orphaned cloud rows; this is not cloud-owner recovery coverage.
            assert!(
                operator_text.contains("cad2215-provenance-local"),
                "operator lost ordinary local running-message ID"
            );
            for id in ["cad2215-provenance-cloud", "cad2215-provenance-derived"] {
                assert!(
                    !operator_text.contains(id),
                    "operator received unproved cloud-derived ID {id}"
                );
            }
            assert!(ben_text.contains("cad2215-provenance-local"), "Ben lost readable local running-message ID; member response: {ben_text}; operator response: {operator_text}");
            for id in ["cad2215-provenance-cloud", "cad2215-provenance-derived"] {
                assert!(
                    !ben_text.contains(id),
                    "Ben agent list leaked cloud-derived ID {id}"
                );
            }
        }
        if path == "/api/agents" {
            assert!(owner_text.contains(&alias), "owner lost own assistant");
            assert!(
                operator_text.contains(&alias),
                "operator cannot recover assistant"
            );
        }
        if path == "/api/agents/cad2215-worker" {
            assert!(
                operator_text.contains("CAD2215_BOARD_PRIVATE_EVENT"),
                "operator cannot recover private event"
            );
        }
        for hidden in [
            alias.as_str(),
            job.as_str(),
            task.as_str(),
            "CAD2215_BOARD_PRIVATE_JOB",
            "CAD2215_BOARD_PRIVATE_MESSAGE",
            "cad2215-board-private-message",
            "CAD2215_BOARD_PRIVATE_EVENT",
        ] {
            assert!(!ben_text.contains(hidden), "Ben {path} leaked {hidden}");
        }
    }
    // Issue cards/detail also embed the shared by_issue agent strip. The
    // issue and ordinary worker/task are readable to both members; the
    // private assistant job is not authorized merely by sharing VIS-1.
    for path in ["/api/issues", "/api/issues/VIS-1"] {
        let (ana_status, ana_body) = board_get_status(&rig, &ana, path);
        let (operator_status, operator_body) = board_get_status(&rig, &operator, path);
        let (ben_status, ben_body) = board_get_status(&rig, &ben, path);
        assert_eq!(ana_status, 200, "owner issue read {path} refused");
        assert_eq!(operator_status, 200, "operator issue read {path} refused");
        assert_eq!(ben_status, 200, "member issue read {path} refused");
        let ana_text = ana_body.to_string();
        let operator_text = operator_body.to_string();
        let ben_text = ben_body.to_string();
        for (who, text) in [
            ("owner", &ana_text),
            ("operator", &operator_text),
            ("Ben", &ben_text),
        ] {
            assert!(text.contains("VIS-1"), "{who} {path} lost readable issue");
            assert!(
                text.contains("cad2215-worker"),
                "{who} {path} lost ordinary worker"
            );
            assert!(
                text.contains("cad2215-visible-task"),
                "{who} {path} lost ordinary task"
            );
        }
        // No actual cloud operation/source authority was seeded here; the
        // operator comparison must not be interpreted as cloud-owner recovery.
        assert!(
            operator_text.contains("cad2215-provenance-local"),
            "operator {path} lost ordinary local running-message ID"
        );
        for id in ["cad2215-provenance-cloud", "cad2215-provenance-derived"] {
            assert!(
                !operator_text.contains(id),
                "operator {path} received unproved cloud-derived ID {id}"
            );
        }
        assert!(
            ben_text.contains("cad2215-provenance-local"),
            "Ben {path} lost local running-message ID"
        );
        for id in ["cad2215-provenance-cloud", "cad2215-provenance-derived"] {
            assert!(
                !ben_text.contains(id),
                "Ben {path} leaked cloud-derived ID {id}"
            );
        }
        assert!(
            ana_text.contains(&task),
            "owner {path} lost authorized task"
        );
        assert!(
            operator_text.contains(&task),
            "operator {path} lost recovery task"
        );
        for hidden in [
            alias.as_str(),
            job.as_str(),
            task.as_str(),
            "CAD2215_BOARD_PRIVATE_JOB",
            "CAD2215_BOARD_PRIVATE_MESSAGE",
            "cad2215-board-private-message",
            "CAD2215_BOARD_PRIVATE_EVENT",
        ] {
            assert!(!ben_text.contains(hidden), "Ben {path} leaked {hidden}");
        }
    }
    let assistant_path = format!("/api/agents/{alias}");
    for (who, cookie) in [("owner", &ana), ("operator", &operator)] {
        let (status, body) = board_get_status(&rig, cookie, &assistant_path);
        assert_eq!(status, 200, "{who} cannot recover own assistant: {body}");
        assert!(
            body.to_string().contains(&alias),
            "{who} got wrong assistant"
        );
    }
    let (status, _) = board_get_status(&rig, &ben, &assistant_path);
    assert!(
        status == 403 || status == 404,
        "Ben could fetch foreign private assistant"
    );

    let mut control = EventStream::open(&rig, &operator, "private-job operator");
    let mut member = EventStream::open(&rig, &ben, "private-job Ben");
    // Hidden-only change: a second private task-bound message changes the
    // shared agent entity. No ordinary worker/job changes accompany it.
    shared
        .store
        .enqueue_task(
            "cad2215-worker",
            "CAD2215_BOARD_PRIVATE_STREAM",
            None,
            "cad2215-board-private-stream-message",
            "user",
            Some(&task),
        )
        .unwrap();
    assert_eq!(
        db.execute(
            "UPDATE messages SET state='running' WHERE id='cad2215-board-private-stream-message'",
            []
        )
        .unwrap(),
        1
    );
    let owner_frames = control.through("CAD2215_BOARD_PRIVATE_STREAM");
    assert!(
        owner_frames.contains(&task),
        "operator stream did not observe private task change"
    );
    // A later readable local change positively drives a member entity frame.
    // Cloud/derived siblings are synthetic provenance labels only; no
    // statement about whether an operator has cloud authority follows.
    for (id, text) in [
        (
            "cad2215-provenance-local-stream",
            "CAD2215_PROVENANCE_LOCAL_STREAM",
        ),
        (
            "cad2215-provenance-cloud-stream",
            "CAD2215_PROVENANCE_CLOUD_STREAM",
        ),
        (
            "cad2215-provenance-derived-stream",
            "CAD2215_PROVENANCE_DERIVED_STREAM",
        ),
    ] {
        shared
            .store
            .enqueue_task(
                "cad2215-worker",
                text,
                None,
                id,
                "user",
                Some("cad2215-visible-task"),
            )
            .unwrap();
    }
    assert_eq!(db.execute("UPDATE messages SET state='running' WHERE id IN ('cad2215-provenance-local-stream','cad2215-provenance-cloud-stream','cad2215-provenance-derived-stream')", []).unwrap(), 3);
    assert_eq!(
        db.execute(
            "UPDATE messages SET source='cloud_agent' WHERE id='cad2215-provenance-cloud-stream'",
            []
        )
        .unwrap(),
        1
    );
    assert_eq!(db.execute("UPDATE messages SET origin='cad2215-provenance-cloud-stream' WHERE id='cad2215-provenance-derived-stream'", []).unwrap(), 1);
    let owner_local = control.through("CAD2215_PROVENANCE_LOCAL_STREAM");
    assert!(
        owner_local.contains("cad2215-provenance-local-stream"),
        "operator stream did not observe local fixture message"
    );
    // Subsequent public issue event orders the shared watcher and is a
    // positive member checkpoint. No sleeps and no absence-by-timeout.
    std::fs::write(
        rig.pm.join("visible/VIS-1/issue.md"),
        issue_text("VIS-1", "CAD2215_BOARD_PUBLIC_CHECKPOINT"),
    )
    .unwrap();
    let _ = control.through("CAD2215_BOARD_PUBLIC_CHECKPOINT");
    let member_frames = member.through("CAD2215_BOARD_PUBLIC_CHECKPOINT");
    assert!(
        member_frames.contains("cad2215-provenance-local-stream"),
        "member stream lost readable local running-message ID; frames: {member_frames}"
    );
    for id in [
        "cad2215-provenance-cloud-stream",
        "cad2215-provenance-derived-stream",
    ] {
        assert!(
            !member_frames.contains(id),
            "member stream leaked cloud-derived ID {id}; frames: {member_frames}"
        );
    }
    for hidden in [
        alias.as_str(),
        job.as_str(),
        task.as_str(),
        "CAD2215_BOARD_PRIVATE_JOB",
        "CAD2215_BOARD_PRIVATE_STREAM",
        "cad2215-board-private-stream-message",
    ] {
        assert!(
            !member_frames.contains(hidden),
            "Ben stream leaked {hidden}; received frames: {member_frames}"
        );
    }
}
