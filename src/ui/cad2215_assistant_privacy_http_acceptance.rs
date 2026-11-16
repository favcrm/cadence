//! CAD-2215 independent assistant ownership privacy acceptance.
//! The product implementer must not edit or weaken this check.
//! Uses a real assistant binding and request ledger via signed HTTP.

use super::*;

fn principal_envelope(rig: &Rig, sub: &str, owner: bool) -> String {
    let now = crate::issue::time::now_epoch();
    let actor = if owner {
        "operator".to_string()
    } else {
        format!("users/{sub}")
    };
    let header = json!({"alg":"EdDSA", "typ":"agenticos-wiki-actor/1", "kid":KID});
    let claims = json!({
        "iss": rig.issuer,
        "aud": format!("http://{}", rig.public_host),
        "sub": sub,
        "actor": actor,
        "principal_kind": "user",
        "role_at_issue": if owner { "owner" } else { "member" },
        "organization_id": ORG,
        "scope": ["cli.read"],
        "credential_id": format!("cad2215-assistant-{sub}"),
        "iat": now,
        "exp": now + 120,
        "jti": format!("cad2215-assistant-cli-{}", uuid::Uuid::new_v4()),
    });
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    );
    format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(rig.key.sign(signed.as_bytes()).as_ref())
    )
}

fn read_as(rig: &Rig, sub: &str, owner: bool, verb: &str, args: Value) -> (u16, Value) {
    let url = format!("http://127.0.0.1:{}/api/cli/{verb}", rig.board_port);
    let mut response = http_agent()
        .post(&url)
        .header("Host", &rig.public_host)
        .header("Content-Type", "application/json")
        .header(
            "Authorization",
            &format!("Bearer wikienv_{}", principal_envelope(rig, sub, owner)),
        )
        .send_json(args)
        .expect("hosted CLI request failed");
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_json()
        .expect("hosted CLI response is not JSON");
    (status, body)
}

#[test]
fn private_assistant_and_issue_bound_job_are_visible_only_to_owner_and_operator() {
    let _flag = AssistantFlagOn::arm();
    let (rig, events) = queue_rig(&["ana", "ben"]);
    let _keys = initialize_queue_keyring(&rig);
    seed(&rig);
    // Ana alone is granted the private project. Ben is a signed member of
    // the same workspace but has neither the project grant nor subscription.
    std::fs::write(rig.pm.join("secret/project.yaml"),
        "key: secret\nprefix: SEC\npolicy:\n  version: 1\n  visibility: private\n  members:\n  - users/ana\n").unwrap();

    let env = rig.envelope("ana", json!({}), json!({}));
    let (status, requested) = rig.call("job_request", &env,
        &json!({"title":"CAD2215_PRIVATE_ASSIST_JOB", "request_key":"cad2215-private-job", "issue":"VIS-9"}));
    assert_eq!(
        status, 200,
        "real signed assistant job request failed: {requested}"
    );
    let job = promoted_job(&rig, &events, "ana", &requested);
    let state = rig._dir.path();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(state, &opts).unwrap();
    let binding = shared
        .store
        .assistant_binding(ORG, "ana")
        .unwrap()
        .expect("Ana binding missing");
    assert_eq!(binding.sub, "ana");
    assert!(
        shared.store.assist_request_owns(ORG, "ana", &job).unwrap(),
        "missing actual ownership ledger"
    );
    assert!(
        !shared.store.assist_request_owns(ORG, "ben", &job).unwrap(),
        "Ben unexpectedly owns job"
    );
    assert!(
        !shared.store.job_is_subscriber(&job, ORG, "ben").unwrap(),
        "Ben unexpectedly subscribes"
    );
    let alias = binding.alias;

    // The real request binds readable VIS-9. The following move to SEC-1 is a
    // synthetic policy transition, not a supported rebind API; task
    // assignment is synthetic. The accepted-request row moves with it because
    // the store treats a job whose queue row names a different issue as
    // untrusted provenance and denies it to every reader, its owner included
    // (`PRIVATE_PROVENANCE_CTE`); the ownership ledger itself is unchanged.
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id='SEC-1' WHERE id=?1", [&job])
            .unwrap(),
        1
    );
    assert_eq!(
        db.execute(
            "UPDATE assist_request_queue SET issue='SEC-1' WHERE job_id=?1",
            [&job]
        )
        .unwrap(),
        1
    );
    let task: String = db
        .query_row(
            "SELECT id FROM tasks WHERE job_id=?1 ORDER BY id LIMIT 1",
            [&job],
            |row| row.get(0),
        )
        .expect("real requested job has no task");
    assert_eq!(
        db.execute(
            "UPDATE tasks SET assignee=?1, state='running' WHERE id=?2",
            rusqlite::params![alias, task]
        )
        .unwrap(),
        1
    );
    shared
        .store
        .enqueue_task(
            &alias,
            "CAD2215_PRIVATE_ASSIST_MESSAGE",
            None,
            "cad2215-private-assist-message",
            "user",
            Some(&task),
        )
        .unwrap();

    // Independently establish that the owner is genuinely authorized on
    // the member-assistant route, not merely inferred from a label.
    let env = rig.envelope("ana", json!({}), json!({}));
    let (status, evidence) = rig.call("job_evidence", &env, &json!({"job":job}));
    assert_eq!(
        status, 200,
        "actual owning member lacks evidence access: {evidence}"
    );
    let env = rig.envelope("ben", json!({}), json!({}));
    let (status, _) = rig.call("job_evidence", &env, &json!({"job":job}));
    assert!(
        status == 401 || status == 403 || status == 404,
        "unentitled member reached evidence"
    );

    for (verb, args) in [
        ("agent_list", json!({})),
        ("agent_show", json!({"alias":alias})),
        ("status", json!({})),
    ] {
        let (owner_status, owner) = read_as(&rig, "ana", false, verb, args.clone());
        let (operator_status, operator) =
            read_as(&rig, "operator-person", true, verb, args.clone());
        let (other_status, other) = read_as(&rig, "ben", false, verb, args);
        assert_eq!(owner_status, 200, "owning member's {verb} refused");
        assert_eq!(operator_status, 200, "operator recovery {verb} refused");
        for (who, body) in [("owner", &owner), ("operator", &operator)] {
            let text = body.to_string();
            assert!(text.contains(&alias), "{who} {verb} omitted own assistant");
            assert!(text.contains(&task), "{who} {verb} omitted owned task");
            if verb == "agent_show" {
                assert!(
                    text.contains("CAD2215_PRIVATE_ASSIST_MESSAGE"),
                    "{who} cannot read own assistant work"
                );
            }
        }
        if verb == "agent_show" {
            assert!(
                other_status == 404 || other_status == 403 || other_status == 200,
                "unexpected foreign show response {other_status}"
            );
        } else {
            assert_eq!(other_status, 200, "other member {verb} unavailable");
        }
        let text = other.to_string();
        for hidden in [
            &alias,
            &task,
            &job,
            &"CAD2215_PRIVATE_ASSIST_JOB".to_string(),
            &"CAD2215_PRIVATE_ASSIST_MESSAGE".to_string(),
            &"cad2215-private-assist-message".to_string(),
        ] {
            assert!(
                !text.contains(hidden.as_str()),
                "other member {verb} leaked {hidden}"
            );
        }
    }
}

#[test]
fn shared_project_and_worker_do_not_grant_another_members_assistant_job() {
    let _flag = AssistantFlagOn::arm();
    let (rig, events) = queue_rig(&["ana", "ben"]);
    let _keys = initialize_queue_keyring(&rig);
    seed(&rig);

    // VIS-1 is workspace-visible to both members. The job is nevertheless
    // Ana's private assistant job, proven by the real signed request ledger.
    let env = rig.envelope("ana", json!({}), json!({}));
    let (status, requested) = rig.call("job_request", &env,
        &json!({"title":"CAD2215_SHARED_WORKER_PRIVATE_JOB", "request_key":"cad2215-shared-worker", "issue":"VIS-9"}));
    assert_eq!(
        status, 200,
        "real signed assistant job request failed: {requested}"
    );
    let job = promoted_job(&rig, &events, "ana", &requested);
    let state = rig._dir.path();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(state, &opts).unwrap();
    let binding = shared
        .store
        .assistant_binding(ORG, "ana")
        .unwrap()
        .expect("Ana binding missing");
    assert_ne!(
        binding.alias, "cad2215-worker",
        "fixture worker must not be the member's assistant"
    );
    assert!(
        shared.store.assist_request_owns(ORG, "ana", &job).unwrap(),
        "Ana ownership ledger missing"
    );
    assert!(
        !shared.store.assist_request_owns(ORG, "ben", &job).unwrap(),
        "Ben unexpectedly owns job"
    );
    assert!(
        !shared.store.job_is_subscriber(&job, ORG, "ben").unwrap(),
        "Ben unexpectedly subscribes"
    );

    // Synthetic issue transition and dispatch assignment only; these do not
    // create or replace the real owner/subscriber ledger or claim a rebind API.
    // The accepted-request row moves with the job so the ledger agrees with
    // itself: a queue row naming a different issue is untrusted provenance and
    // is denied to every reader, its owner included.
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id='VIS-1' WHERE id=?1", [&job])
            .unwrap(),
        1
    );
    assert_eq!(
        db.execute(
            "UPDATE assist_request_queue SET issue='VIS-1' WHERE job_id=?1",
            [&job]
        )
        .unwrap(),
        1
    );
    let task: String = db
        .query_row(
            "SELECT id FROM tasks WHERE job_id=?1 ORDER BY id LIMIT 1",
            [&job],
            |row| row.get(0),
        )
        .expect("real requested job has no task");
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
            "CAD2215_SHARED_WORKER_PRIVATE_MESSAGE",
            None,
            "cad2215-shared-worker-private-message",
            "user",
            Some(&task),
        )
        .unwrap();

    // This independent signed member-assistant control proves both the real
    // ownership and the other member's lack of access to this canonical job.
    let env = rig.envelope("ana", json!({}), json!({}));
    let (status, evidence) = rig.call("job_evidence", &env, &json!({"job":job}));
    assert_eq!(
        status, 200,
        "actual job owner lacks evidence access: {evidence}"
    );
    let env = rig.envelope("ben", json!({}), json!({}));
    let (status, _) = rig.call("job_evidence", &env, &json!({"job":job}));
    assert!(
        status == 401 || status == 403 || status == 404,
        "non-owner reached job evidence"
    );

    for (verb, args) in [
        ("agent_list", json!({})),
        ("agent_show", json!({"alias":"cad2215-worker"})),
        ("status", json!({})),
    ] {
        let (ana_status, ana) = read_as(&rig, "ana", false, verb, args.clone());
        let (owner_status, owner) = read_as(&rig, "operator-person", true, verb, args.clone());
        let (ben_status, ben) = read_as(&rig, "ben", false, verb, args);
        assert_eq!(ana_status, 200, "owning member {verb} refused");
        assert_eq!(owner_status, 200, "operator {verb} refused");
        assert_eq!(
            ben_status, 200,
            "shared worker {verb} refused to other member"
        );
        for (who, body) in [("Ana", &ana), ("operator", &owner)] {
            let text = body.to_string();
            assert!(
                text.contains("cad2215-worker"),
                "{who} {verb} omitted shared worker"
            );
            assert!(text.contains(&task), "{who} {verb} omitted authorized task");
            if verb == "agent_show" {
                assert!(
                    text.contains("CAD2215_SHARED_WORKER_PRIVATE_MESSAGE"),
                    "{who} {verb} omitted authorized message"
                );
            }
        }
        let text = ben.to_string();
        assert!(
            text.contains("cad2215-worker"),
            "other member lost ordinary shared worker in {verb}"
        );
        for hidden in [
            job.as_str(),
            task.as_str(),
            "CAD2215_SHARED_WORKER_PRIVATE_JOB",
            "CAD2215_SHARED_WORKER_PRIVATE_MESSAGE",
            "cad2215-shared-worker-private-message",
        ] {
            assert!(
                !text.contains(hidden),
                "project-readable non-owner {verb} leaked {hidden}"
            );
        }
    }
}

#[test]
fn hosted_cli_paginates_after_private_assistant_job_visibility() {
    let _flag = AssistantFlagOn::arm();
    let (rig, events) = queue_rig(&["ana", "ben"]);
    let _keys = initialize_queue_keyring(&rig);
    seed(&rig);
    let envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, requested) = rig.call("job_request", &envelope,
        &json!({"title":"CAD2215_PAGE_PRIVATE_JOB", "request_key":"cad2215-page-private", "issue":"VIS-9"}));
    assert_eq!(status, 200, "real signed job request failed: {requested}");
    let job = promoted_job(&rig, &events, "ana", &requested);
    let state = rig._dir.path();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(state, &opts).unwrap();
    assert!(
        shared.store.assist_request_owns(ORG, "ana", &job).unwrap(),
        "actual owner ledger missing"
    );
    assert!(!shared.store.assist_request_owns(ORG, "ben", &job).unwrap());
    assert!(!shared.store.job_is_subscriber(&job, ORG, "ben").unwrap());

    // Synthetic issue transition and ordinary-worker assignment; this does
    // not claim a supported rebind API. Ownership is from the signed request,
    // and the accepted-request row moves with the job so the ledger agrees
    // with itself.
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id='VIS-1' WHERE id=?1", [&job])
            .unwrap(),
        1
    );
    assert_eq!(
        db.execute(
            "UPDATE assist_request_queue SET issue='VIS-1' WHERE job_id=?1",
            [&job]
        )
        .unwrap(),
        1
    );
    let private_task: String = db
        .query_row(
            "SELECT id FROM tasks WHERE job_id=?1 ORDER BY id LIMIT 1",
            [&job],
            |r| r.get(0),
        )
        .expect("private job has no real task");
    assert_eq!(
        db.execute(
            "UPDATE tasks SET assignee='cad2215-worker', state='running' WHERE id=?1",
            [&private_task]
        )
        .unwrap(),
        1
    );
    for (id, task, body) in [
        (
            "cad2215-page-public",
            "cad2215-visible-task",
            "CAD2215_PAGE_PUBLIC_BODY",
        ),
        (
            "cad2215-page-private",
            private_task.as_str(),
            "CAD2215_PAGE_PRIVATE_BODY",
        ),
    ] {
        shared
            .store
            .enqueue_task("cad2215-worker", body, None, id, "user", Some(task))
            .unwrap();
    }
    assert_eq!(db.execute("UPDATE messages SET state='completed' WHERE id IN ('cad2215-page-public','cad2215-page-private')", []).unwrap(), 2);
    let (public_seq, private_seq): (i64, i64) = db.query_row(
        "SELECT (SELECT seq FROM messages WHERE id='cad2215-page-public'), (SELECT seq FROM messages WHERE id='cad2215-page-private')",
        [], |r| Ok((r.get(0)?, r.get(1)?))
    ).unwrap();
    assert!(
        public_seq < private_seq,
        "fixture did not create ordered terminal rows"
    );

    let args = json!({"alias":"cad2215-worker", "limit":1});
    let (ana_status, ana) = read_as(&rig, "ana", false, "agent_show", args.clone());
    let (operator_status, operator) =
        read_as(&rig, "operator-person", true, "agent_show", args.clone());
    let (ben_status, ben) = read_as(&rig, "ben", false, "agent_show", args);
    assert_eq!(ana_status, 200, "owning member show refused");
    assert_eq!(operator_status, 200, "operator recovery show refused");
    assert_eq!(
        ben_status, 200,
        "non-owning member's readable worker show refused"
    );
    for (who, body) in [("Ana", &ana), ("operator", &operator)] {
        let text = body.to_string();
        assert!(
            text.contains("CAD2215_PAGE_PRIVATE_BODY"),
            "{who} cannot read newest authorized terminal row"
        );
        assert!(
            !text.contains("CAD2215_PAGE_PUBLIC_BODY"),
            "{who} newest-terminal limit not enforced"
        );
    }
    let text = ben.to_string();
    assert!(
        text.contains("CAD2215_PAGE_PUBLIC_BODY"),
        "hidden private row displaced Ben's visible terminal row"
    );
    for hidden in [
        job.as_str(),
        private_task.as_str(),
        "CAD2215_PAGE_PRIVATE_JOB",
        "CAD2215_PAGE_PRIVATE_BODY",
        "cad2215-page-private",
    ] {
        assert!(
            !text.contains(hidden),
            "Ben learned hidden job/message/cursor data: {hidden}"
        );
    }
    let messages = ben["messages"]
        .as_array()
        .expect("Ben's message page missing");
    assert_eq!(
        messages.len(),
        1,
        "Ben did not receive exactly one readable terminal row"
    );
    // A hidden row must not alter a member-visible history count or cursor.
    if let Some(omitted) = ben.get("messages_omitted") {
        assert_eq!(
            omitted.as_u64(),
            Some(0),
            "hidden row leaked via omitted count"
        );
    }
    assert!(
        ben.get("event_cursor").is_none(),
        "member got an unscoped event cursor"
    );
}
