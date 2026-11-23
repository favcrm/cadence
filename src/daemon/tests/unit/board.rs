use super::*;

#[test]
fn cad631_retained_app_provider_events_do_not_publish_material() {
    let dir = tempfile::tempdir().unwrap();
    let shared = cad627_shared(dir.path());
    shared.store.ensure_thread("w1").unwrap();
    fn payload(marker: &str) -> Value {
        json!({"item":{"type":"agentMessage","phase":"commentary","text":marker},"text":marker,"tool":"read","summary":marker,"tool_use_id":"test-tool","trigger":marker})
    }
    for method in [
        "item/completed",
        "cadence/assistant_text",
        "cadence/tool_use",
        "cadence/session_compacted",
    ] {
        shared.on_provider_event("w1", method, payload("control-event-sentinel"));
    }
    assert!(format!("{:?}", shared.store.events("w1", 0, 100).unwrap())
        .contains("control-event-sentinel"));
    assert!(
        format!("{:?}", shared.store.thread_entries("w1", 0, 100).unwrap())
            .contains("control-event-sentinel")
    );
    let conn = rusqlite::Connection::open(dir.path().join("cadence.sqlite3")).unwrap();
    conn.execute_batch("INSERT INTO jobs(id,spec_path,pm_alias,state,created,updated) VALUES('private-job','app-run','w1','done',1,1);
            INSERT INTO tasks(id,job_id,assignee,state,created,updated) VALUES('private-task','private-job','w1','done',1,1);
            INSERT INTO app_runs(id,install_id,epoch,bundle_digest,snapshot,snapshot_digest,owner_pm,request_id,state,created,updated) VALUES('private-job','private-install',1,'digest','{}','digest','w1','request','succeeded',1,1);
            INSERT INTO app_run_steps(run_id,step_id,task_id,spec,identity_digest,state) VALUES('private-job','s1','private-task','{}','identity','succeeded');").unwrap();
    for method in [
        "item/completed",
        "cadence/assistant_text",
        "cadence/tool_use",
        "cadence/session_compacted",
    ] {
        shared.on_provider_event("w1", method, payload("private-event-sentinel"));
    }
    let events = shared.store.events("w1", 0, 100).unwrap();
    assert!(!format!("{events:?}").contains("private-event-sentinel"));
    let entries = shared.store.thread_entries("w1", 0, 100).unwrap();
    assert!(!format!("{entries:?}").contains("private-event-sentinel"));
}

#[test]
fn cad1221_agent_show_binds_only_the_aliases_open_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let shared = cad627_shared(dir.path());
    let conn = rusqlite::Connection::open(dir.path().join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
               VALUES('job-bound','Bound job','spec.md','pm','CAD-9','running',1,1),
                     ('job-free','Free job','spec.md','pm',NULL,'running',1,1);
             INSERT INTO tasks(id,job_id,title,assignee,state,created,updated) VALUES
               ('t-open','job-bound','Open task','w1','running',1,2),
               ('t-free','job-free','Unbound task','w1','queued',1,3),
               ('t-done','job-bound','Done task','w1','done',1,4),
               ('t-other','job-bound','Other task','w2','running',1,5);",
    )
    .unwrap();
    let show = shared
        .dispatch(
            "agent_show",
            &json!({"alias": "w1", "active_only": true}),
            std::process::id(),
        )
        .unwrap();
    assert_eq!(
        show["task_bindings"],
        json!([
            {"id": "t-open", "job": "job-bound", "title": "Open task",
             "state": "running", "issue": "CAD-9",
             "job_title": "Bound job", "job_state": "running"},
            {"id": "t-free", "job": "job-free", "title": "Unbound task",
             "state": "queued", "issue": null,
             "job_title": "Free job", "job_state": "running"},
        ])
    );
    let fleet = shared
        .dispatch("agent_list", &json!({}), std::process::id())
        .unwrap();
    let row = fleet["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "w1")
        .unwrap();
    assert_eq!(row["tasks"], json!(["t-open", "t-free"]));
}

#[test]
fn cad1280_job_list_open_tasks_only_trims_the_task_list() {
    let dir = tempfile::tempdir().unwrap();
    let shared = cad627_shared(dir.path());
    let conn = rusqlite::Connection::open(dir.path().join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
               VALUES('job-a','A','spec.md','pm','CAD-9','running',1,1),
                     ('job-b','B','spec.md','pm',NULL,'done',2,2);
             INSERT INTO tasks(id,job_id,title,assignee,state,created,updated) VALUES
               ('t-open','job-a','Open','w1','running',1,2),
               ('t-done','job-a','Done','w1','done',2,3),
               ('t-bad','job-a','Failed','w1','failed',3,4),
               ('t-b','job-b','Other','w2','verified',1,5);",
    )
    .unwrap();
    let list = |params: Value| {
        shared
            .dispatch("job_list", &params, std::process::id())
            .unwrap()["jobs"]
            .clone()
    };
    let ids = |job: &Value| -> Vec<String> {
        job["task_list"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_str().unwrap().to_string())
            .collect()
    };
    let counts_a = json!({"running": 1, "done": 1, "failed": 1});
    let plain = list(json!({"all": true}));
    assert_eq!(plain[0]["tasks"], counts_a);
    assert!(plain[0].get("task_list").is_none());
    let full = list(json!({"all": true, "tasks_detail": true}));
    assert_eq!(ids(&full[0]), ["t-open", "t-done", "t-bad"]);
    assert_eq!(ids(&full[1]), ["t-b"]);
    let open = list(json!({"all": true, "tasks_detail": true, "open_tasks_only": true}));
    assert_eq!(ids(&open[0]), ["t-open"]);
    assert!(ids(&open[1]).is_empty());
    assert_eq!(open[0]["tasks"], counts_a);
    assert_eq!(open[1]["tasks"], json!({"verified": 1}));
}

#[test]
fn cad627_board_poll_does_not_decode_terminal_history() {
    let dir = tempfile::tempdir().unwrap();
    let shared = cad627_shared(dir.path());
    shared
        .store
        .enqueue("w1", "live", None, "live", "user")
        .unwrap();
    shared.store.mark_running("live", "private-token").unwrap();
    let created = shared.store.agent("w1").unwrap().created;
    let mut conn = rusqlite::Connection::open(dir.path().join("cadence.sqlite3")).unwrap();
    let tx = conn.transaction().unwrap();
    let result = json!({"text": "x".repeat(8192)}).to_string();
    for i in 0..2000 {
        tx.execute(
            "INSERT INTO messages(id,alias,body,source,state,result,created)
                 VALUES (?1,'w1',?2,'user','completed',?2,?3)",
            rusqlite::params![format!("history-{i}"), result, created + 1.0],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    store::take_decoded_messages();
    let started = Instant::now();
    for _ in 0..10 {
        let view = shared.board_view("w1", std::process::id()).unwrap();
        assert_eq!(view["messages"].as_array().unwrap().len(), 1);
        assert_eq!(view["messages"][0]["id"], "live");
        assert!(view["messages"][0].get("turn_id").is_none());
        assert_eq!(view["parked"], 0);
    }
    let decoded = store::take_decoded_messages();
    eprintln!(
        "CAD-627: 10 board reads, 2000 terminal rows, decoded={decoded}, elapsed={:?}",
        started.elapsed()
    );
    assert_eq!(
        decoded, 10,
        "terminal history must not be decoded by board polls"
    );
}

#[test]
fn cad627_board_summary_matches_history_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let shared = cad627_shared(dir.path());
    let created = shared.store.agent("w1").unwrap().created;
    let conn = rusqlite::Connection::open(dir.path().join("cadence.sqlite3")).unwrap();
    for (id, alias, state, result, at) in [
        (
            "old",
            "w1",
            "failed",
            r#"{"via":"pty_render_miss"}"#,
            created - 1.0,
        ),
        (
            "park",
            "w1",
            "failed",
            r#"{"via":"pty_render_miss"}"#,
            created + 1.0,
        ),
        (
            "clock-step",
            "w1",
            "unknown",
            r#"{"via":"pty_render_miss"}"#,
            created - 1.0,
        ),
        ("queued", "w1", "queued", "null", created + 1.0),
        ("bad", "w1", "failed", "not json", created + 1.0),
        ("scalar", "w1", "failed", "42", created + 1.0),
        (
            "live",
            "w1",
            "running",
            r#"{"via":"pty_render_miss"}"#,
            created - 1.0,
        ),
        (
            "other",
            "w2",
            "failed",
            r#"{"via":"pty_render_miss"}"#,
            created + 1.0,
        ),
    ] {
        conn.execute(
            "INSERT INTO messages(id,alias,body,source,state,result,created,turn_id)
                 VALUES (?1,?2,'body','user',?3,?4,?5,'private-token')",
            rusqlite::params![id, alias, state, result, at],
        )
        .unwrap();
    }
    let history = shared.store.messages("w1").unwrap();
    let expected: Vec<Value> = history
        .iter()
        .filter(|m| m.state == "running")
        .map(|m| {
            let mut row = m.to_json();
            row.as_object_mut().unwrap().remove("turn_id");
            row
        })
        .collect();
    let view = shared.board_view("w1", std::process::id()).unwrap();
    assert_eq!(view["messages"], json!(expected));
    assert_eq!(view["parked"], 2);
    assert_eq!(view["queued"], 1);
    assert_eq!(view["unknown"], 1);
    assert_eq!(shared.store.messages("w1").unwrap().len(), history.len());
}

#[test]
fn cad627_active_agent_show_keeps_unknowns_and_default_history() {
    let dir = tempfile::tempdir().unwrap();
    let shared = cad627_shared(dir.path());
    let conn = rusqlite::Connection::open(dir.path().join("cadence.sqlite3")).unwrap();
    let created = shared.store.agent("w1").unwrap().created + 1.0;
    for (id, state) in [
        ("done", "completed"),
        ("fenced", "unknown"),
        ("live", "running"),
    ] {
        conn.execute(
            "INSERT INTO messages(id,alias,body,source,state,created,turn_id)
                 VALUES (?1,'w1','body','user',?2,?3,'private-token')",
            rusqlite::params![id, state, created],
        )
        .unwrap();
    }
    let before = shared.store.event_cursor("w1").unwrap();
    let active = shared
        .dispatch(
            "agent_show",
            &json!({"alias": "w1", "active_only": true}),
            std::process::id(),
        )
        .unwrap();
    let ids: Vec<&str> = active["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["fenced", "live"]);
    assert_eq!(active["unknown"], 1);
    assert!(!active.to_string().contains("private-token"));
    let full = shared
        .dispatch("agent_show", &json!({"alias": "w1"}), std::process::id())
        .unwrap();
    assert_eq!(full["messages"].as_array().unwrap().len(), 3);
    assert_eq!(shared.store.event_cursor("w1").unwrap(), before);
}

#[test]
fn cad879_agent_show_window_is_bounded_and_keeps_live_work() {
    let dir = tempfile::tempdir().unwrap();
    let shared = cad627_shared(dir.path());
    let mut conn = rusqlite::Connection::open(dir.path().join("cadence.sqlite3")).unwrap();
    let created = shared.store.agent("w1").unwrap().created;
    let tx = conn.transaction().unwrap();
    // An old unfinished row (oldest of all) and 600 finished ones.
    tx.execute(
        "INSERT INTO messages(id,alias,body,source,state,created,turn_id)
             VALUES ('old-live','w1','b','user','running',?1,'private-token')",
        [created + 0.5],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO messages(id,alias,body,source,state,created)
             VALUES ('old-unknown','w1','b','user','unknown',?1)",
        [created + 0.6],
    )
    .unwrap();
    // A previous registration's rows (CAD-304 S4): a finished one is
    // never listed or counted; an unfinished one is always listed.
    tx.execute(
        "INSERT INTO messages(id,alias,body,source,state,created)
             VALUES ('prev-done','w1','b','user','completed',?1)",
        [created - 10.0],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO messages(id,alias,body,source,state,created)
             VALUES ('prev-live','w1','b','user','queued',?1)",
        [created - 9.0],
    )
    .unwrap();
    for i in 0..600 {
        tx.execute(
            "INSERT INTO messages(id,alias,body,source,state,created)
                 VALUES (?1,'w1','b','user','completed',?2)",
            rusqlite::params![format!("h-{i}"), created + 1.0 + i as f64],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let show = |params: Value| {
        shared
            .dispatch("agent_show", &params, std::process::id())
            .unwrap()
    };
    let ids = |v: &Value| -> Vec<String> {
        v["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect()
    };
    // No fields: today's full history, nothing omitted.
    let full = show(json!({"alias": "w1"}));
    assert_eq!(full["messages"].as_array().unwrap().len(), 603);
    assert!(full.get("messages_omitted").is_none());
    // Default window: the last 20 plus both unfinished rows.
    let win = show(json!({"alias": "w1", "limit": 20}));
    let got = ids(&win);
    assert_eq!(got.len(), 23, "{got:?}");
    assert!(got.contains(&"prev-live".to_string()));
    assert!(!got.contains(&"prev-done".to_string()));
    assert_eq!(got[3], "h-580");
    assert_eq!(got[22], "h-599");
    // 580 finished rows left out; the previous registration's
    // finished row is not counted.
    assert_eq!(win["messages_omitted"], 580);
    // The window is the same size whatever the history length.
    assert!(win.to_string().len() < full.to_string().len() / 10);
    assert!(!win.to_string().contains("private-token"));
    // A window wider than the history reaches the previous
    // registration's finished row and must still stop short of it.
    let wide = show(json!({"alias": "w1", "limit": 5000}));
    assert!(!ids(&wide).contains(&"prev-done".to_string()));
    assert_eq!(ids(&wide).len(), 603);
    assert_eq!(wide["messages_omitted"], 0);
    // limit 0: only unfinished work.
    assert_eq!(ids(&show(json!({"alias": "w1", "limit": 0}))).len(), 3);
    // since by message id: rows after it, plus unfinished ones.
    let by_id = ids(&show(json!({"alias": "w1", "since": "h-595"})));
    assert_eq!(
        by_id,
        [
            "old-live",
            "old-unknown",
            "prev-live",
            "h-596",
            "h-597",
            "h-598",
            "h-599"
        ]
    );
    // since + limit compose.
    let both = ids(&show(json!({"alias": "w1", "since": "h-595", "limit": 2})));
    assert_eq!(
        both,
        ["old-live", "old-unknown", "prev-live", "h-598", "h-599"]
    );
    // since by timestamp.
    let ts = (created + 1.0 + 597.0).to_string();
    let by_ts = ids(&show(json!({"alias": "w1", "since": ts})));
    assert_eq!(
        by_ts,
        [
            "old-live",
            "old-unknown",
            "prev-live",
            "h-597",
            "h-598",
            "h-599"
        ]
    );
    // since that is neither an id nor a number is refused.
    assert!(shared
        .dispatch(
            "agent_show",
            &json!({"alias": "w1", "since": "no-such-id"}),
            std::process::id()
        )
        .is_err());
    // active_only (what `cadence self` sends) is history-free.
    let active = show(json!({"alias": "w1", "active_only": true}));
    assert_eq!(ids(&active), ["old-live", "old-unknown", "prev-live"]);
    assert!(active.get("messages_omitted").is_none());
    // A malformed `limit` is refused, never a silent full history.
    for bad in [json!(-1), json!("20"), json!(5.0), json!(true)] {
        let err = shared
            .dispatch(
                "agent_show",
                &json!({"alias": "w1", "limit": bad}),
                std::process::id(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("limit"), "{bad}: {err}");
    }
}

#[test]
fn cad1591_internal_notifications_keep_protocol_and_reserved_sources_refuse_callers() {
    let dir = tempfile::tempdir().unwrap();
    let shared = cad627_shared(dir.path());
    let body = "internal notification payload\n\"content\" remains protocol data";

    for source in ["wake", "answer", "effect", "permission"] {
        let forged_id = format!("caller-{source}");
        assert!(
            shared
                .rpc_send(&json!({
                    "alias": "w1",
                    "text": body,
                    "message": forged_id,
                    "source": source,
                }))
                .is_err(),
            "an ordinary caller claimed reserved source {source}"
        );
        assert!(
            shared.store.message(&forged_id).unwrap().is_none(),
            "refused source {source} left a message"
        );

        let id = crate::proto::daemon_message_id(source, "cad1591-internal-protocol");
        shared
            .store
            .enqueue_daemon("w1", body, &id, source)
            .unwrap();
        let Take::Message(message) = shared.store.take_queued("w1").unwrap() else {
            panic!("daemon notification {source} was not queued");
        };
        assert_eq!(message.id, id);
        assert_eq!(message.body, body, "stored {source} body changed");
        assert_eq!(
            shared.delivery_body("w1", "managed", &message, None),
            body,
            "daemon notification {source} changed its delivery protocol"
        );
        shared
            .store
            .finish(&message, "completed", &json!({"text": "ok"}), None)
            .unwrap();
    }

    shared
        .rpc_send(&json!({
            "alias": "w1",
            "text": body,
            "message": "caller-ordinary",
            "source": "user",
        }))
        .expect("ordinary sends must remain admitted");
    assert_eq!(
        shared
            .store
            .message("caller-ordinary")
            .unwrap()
            .unwrap()
            .body,
        body
    );
}
