//! CAD-2215: daemon job-read boundary, with synthetic tracker policy.
//! The identity seam asserts caller identity; this is not an OS-isolation test.

use super::*;
use crate::test_seam::{scoped, Asserted};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// Fixture-owned scoped-cursor key material. Every key file lives in its own
/// owner-only root, outside the daemon state directory and outside the
/// tracker; nothing here generates, derives or stores key bytes in either.
/// The only setup operation is the real operator `scoped_cursor_initialize`
/// RPC, so a scoped read that succeeds proves the production seam.
struct CursorKeys {
    _dir: tempfile::TempDir,
    config: Value,
}

fn private_dir(path: &std::path::Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn write_private(path: &std::path::Path, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    drop(file);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn cursor_keys() -> CursorKeys {
    let dir = tempfile::Builder::new()
        .prefix("cad2215-keys-")
        .tempdir()
        .unwrap();
    private_dir(dir.path());
    for (name, byte) in [("identity.key", 0xa1u8), ("cursor-a.key", 0xb2u8)] {
        write_private(&dir.path().join(name), &[byte; 32]);
    }
    let id = "01".repeat(16);
    let config = json!({
        "version": 1,
        "identity_key_file": dir.path().join("identity.key").to_str().unwrap(),
        "active_key_id": &id,
        "keys": [{ "id": &id, "key_file": dir.path().join("cursor-a.key").to_str().unwrap() }],
    });
    CursorKeys { _dir: dir, config }
}

/// Write the fixed keyring record into `state` and bind it through the real
/// operator RPC. A scoped `job_events` read without this fails closed, so a
/// page that arrives proves the fixture admitted real key material.
fn initialize_cursor_keyring(shared: &Arc<Shared>, state: &std::path::Path, keys: &CursorKeys) {
    write_private(
        &state.join("scoped-cursors.json"),
        &serde_json::to_vec(&keys.config).unwrap(),
    );
    let answer = scoped(Asserted::Operator, || {
        shared.dispatch("scoped_cursor_initialize", &json!({}), std::process::id())
    })
    .expect("the operator must initialize the scoped-cursor keyring");
    assert_eq!(answer["initialized"], json!(true), "{answer}");
}

/// A scoped page carries no global sequence position at any depth: cursors are
/// opaque and row identity is keyed, never the shared `events.seq`.
fn assert_no_global_seq(value: &Value, what: &str) {
    match value {
        Value::Object(map) => {
            assert!(
                !map.contains_key("seq"),
                "{what} exposed a global sequence position: {value}"
            );
            for nested in map.values() {
                assert_no_global_seq(nested, what);
            }
        }
        Value::Array(items) => {
            for nested in items {
                assert_no_global_seq(nested, what);
            }
        }
        _ => {}
    }
}

fn make_synthetic_legacy_unbound_job(
    shared: &Shared,
    pm_dir: &std::path::Path,
    pm_alias: &str,
    state_dir: &std::path::Path,
) {
    let issue = pm_dir.join("open/OPEN-2");
    std::fs::create_dir_all(&issue).unwrap();
    std::fs::write(issue.join("issue.md"), "---\nid: OPEN-2\ntitle: legacy fixture\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n").unwrap();
    shared
        .store
        .create_job(
            "unbound-job",
            Some("Unbound"),
            "spec.md",
            "fixture-digest",
            pm_alias,
            Some("OPEN-2"),
            None,
            None,
            2,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    // Synthetic pre-contract legacy data: deliberately remove the binding
    // AFTER a valid readable issue-bound job exists; this is not a supported
    // current job-creation workflow.
    let db = rusqlite::Connection::open(state_dir.join("cadence.sqlite3")).unwrap();
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id=NULL WHERE id='unbound-job'", [])
            .unwrap(),
        1
    );
}

#[test]
fn job_show_hides_private_and_unbound_jobs_but_operator_can_recover() {
    let temp = tempfile::tempdir().unwrap();
    let pm_dir = temp.path().join("tracker");
    crate::issue::Pm::init(&pm_dir).unwrap();

    for (key, prefix, policy) in [
        ("open", "OPEN", ""),
        (
            "restricted",
            "REST",
            "policy:\n  version: 1\n  visibility: private\n",
        ),
    ] {
        let project = pm_dir.join(key);
        let issue_dir = project.join(format!("{prefix}-1"));
        std::fs::create_dir_all(&issue_dir).unwrap();
        std::fs::write(
            project.join("project.yaml"),
            format!("key: {key}\nprefix: {prefix}\n{policy}"),
        )
        .unwrap();
        std::fs::write(
            issue_dir.join("issue.md"),
            format!(
                "---\nid: {prefix}-1\ntitle: {key} issue\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"
            ),
        )
        .unwrap();
    }

    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let shared = Shared::new(temp.path(), &opts).unwrap();
    register(&shared, temp.path(), "reader");
    make_synthetic_legacy_unbound_job(&shared, &pm_dir, "reader", temp.path());
    let conn = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
         VALUES ('readable-job','Readable','spec.md','reader','OPEN-1','open',1,1),
                ('restricted-job','Restricted','spec.md','reader','REST-1','open',2,2);",
    )
    .unwrap();

    let pid = std::process::id();
    let reader = || Asserted::Agent("reader".to_string());
    scoped(reader(), || {
        assert!(matches!(
            shared.agent_caller(pid, "acceptance identity").unwrap(),
            AgentCaller::Agent(ref alias) if alias == "reader"
        ));
    });
    scoped(Asserted::Operator, || {
        assert!(matches!(
            shared.agent_caller(pid, "acceptance identity").unwrap(),
            AgentCaller::Operator
        ));
    });

    let show = |who: Asserted, id: &str| {
        scoped(who, || {
            shared.dispatch("job_show", &json!({"job": id}), pid)
        })
    };
    let public = show(reader(), "readable-job").expect("reader must see the open project job");
    assert_eq!(public["job"]["id"], "readable-job");

    // Compare both the rejection class and the complete diagnostic after
    // replacing only the requested identifier. This permits the normal
    // not-found message to name its input without revealing existence.
    let missing = show(reader(), "missing-job").expect_err("missing job must be refused");
    let missing_text = match missing {
        Error::Rejected(text) => text,
        other => panic!("missing job had a non-rejection error: {other}"),
    };
    assert!(
        missing_text.contains("missing-job"),
        "unexpected missing-job error: {missing_text}"
    );
    let expected = missing_text.replace("missing-job", "<requested>");
    for id in ["restricted-job", "unbound-job"] {
        let error = show(reader(), id).expect_err("unadmitted job must be refused");
        let text = match error {
            Error::Rejected(text) => text,
            other => panic!("{id}: non-rejection error: {other}"),
        };
        assert_eq!(
            text.replace(id, "<requested>"),
            expected,
            "{id} must be indistinguishable from a missing job"
        );
    }

    for id in ["restricted-job", "unbound-job"] {
        let view = show(Asserted::Operator, id).expect("operator recovery read must succeed");
        assert_eq!(view["job"]["id"], id);
    }
}

#[test]
fn visible_job_and_task_never_disclose_foreign_kickoff_credentials_or_bindings() {
    let temp = tempfile::tempdir().unwrap();
    let pm_dir = temp.path().join("tracker");
    crate::issue::Pm::init(&pm_dir).unwrap();
    for (key, prefix, policy) in [
        ("open", "OPEN", ""),
        (
            "restricted",
            "REST",
            "policy:\n  version: 1\n  visibility: private\n",
        ),
    ] {
        let project = pm_dir.join(key);
        let issue_dir = project.join(format!("{prefix}-1"));
        std::fs::create_dir_all(&issue_dir).unwrap();
        std::fs::write(
            project.join("project.yaml"),
            format!("key: {key}\nprefix: {prefix}\n{policy}"),
        )
        .unwrap();
        std::fs::write(
            issue_dir.join("issue.md"),
            format!("---\nid: {prefix}-1\ntitle: {key} issue\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"),
        )
        .unwrap();
    }
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let shared = Shared::new(temp.path(), &opts).unwrap();
    register(&shared, temp.path(), "reader");
    register(&shared, temp.path(), "worker");
    let conn = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
         VALUES ('visible-job','Visible','spec.md','reader','OPEN-1','open',1,1),
                ('private-job','Private','spec.md','reader','REST-1','open',2,2);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
         VALUES ('visible-task','visible-job','Visible task','worker','running',1,1),
                ('private-task','private-job','Private task','worker','running',2,2);",
    )
    .unwrap();
    shared
        .store
        .enqueue_task(
            "worker",
            "VISIBLE_KICKOFF_BODY",
            None,
            "visible-kickoff",
            "user",
            Some("visible-task"),
        )
        .unwrap();
    shared
        .store
        .enqueue_task(
            "worker",
            "PRIVATE_KICKOFF_BODY",
            None,
            "private-kickoff-sentinel",
            "user",
            Some("private-task"),
        )
        .unwrap();
    // Synthetic running credential and binding: the first pointer agrees.
    conn.execute_batch(
        "UPDATE messages SET state='running',turn_id='turn-credential-sentinel-7291'
         WHERE id='visible-kickoff';
         UPDATE messages SET state='unknown' WHERE id='private-kickoff-sentinel';
         UPDATE tasks SET dispatch_message='visible-kickoff' WHERE id='visible-task';",
    )
    .unwrap();

    let pid = std::process::id();
    let reader = || Asserted::Agent("reader".to_string());
    let call = |who: Asserted, method: &str, params: Value| {
        scoped(who, || shared.dispatch(method, &params, pid))
    };
    scoped(reader(), || {
        assert!(matches!(
            shared.agent_caller(pid, "acceptance identity").unwrap(),
            AgentCaller::Agent(ref alias) if alias == "reader"
        ))
    });
    scoped(Asserted::Operator, || {
        assert!(matches!(
            shared.agent_caller(pid, "acceptance identity").unwrap(),
            AgentCaller::Operator
        ))
    });
    for (method, params) in [
        ("job_show", json!({"job": "visible-job"})),
        ("task_show", json!({"task": "visible-task"})),
    ] {
        let answer =
            call(reader(), method, params.clone()).expect("visible work must remain readable");
        assert!(answer.to_string().contains("visible-task"));
        assert!(
            !answer.to_string().contains("turn-credential-sentinel-7291"),
            "{method} exposed a turn credential: {answer}"
        );
        let operator =
            call(Asserted::Operator, method, params).expect("operator may inspect visible work");
        assert!(
            operator.to_string().contains("visible-kickoff"),
            "operator lost kickoff recovery: {operator}"
        );
    }

    // Deliberately inconsistent retained data: unlike normal task dispatch,
    // the visible task's pointer now names a message bound to a private task.
    conn.execute(
        "UPDATE tasks SET dispatch_message='private-kickoff-sentinel' WHERE id='visible-task'",
        [],
    )
    .unwrap();
    for (method, params) in [
        ("job_show", json!({"job": "visible-job"})),
        ("task_show", json!({"task": "visible-task"})),
    ] {
        let answer = call(reader(), method, params.clone())
            .expect("a bad kickoff pointer must not hide the visible task");
        let text = answer.to_string();
        assert!(text.contains("visible-task"));
        for secret in [
            "private-kickoff-sentinel",
            "PRIVATE_KICKOFF_BODY",
            "private-task",
            "unknown",
        ] {
            assert!(
                !text.contains(secret),
                "{method} exposed {secret}: {answer}"
            );
        }
        let operator = call(Asserted::Operator, method, params)
            .expect("operator recovery must remain available");
        assert!(
            operator.to_string().contains("private-kickoff-sentinel"),
            "operator cannot inspect the pointer: {operator}"
        );
        assert!(
            operator.to_string().contains("unknown"),
            "operator cannot inspect the kickoff state: {operator}"
        );
    }
    let private_body = call(
        Asserted::Operator,
        "message_read",
        json!({"message": "private-kickoff-sentinel"}),
    )
    .expect("operator can inspect the mismatched message");
    assert!(private_body.to_string().contains("PRIVATE_KICKOFF_BODY"));
}

#[test]
fn agent_read_as_is_operator_only_and_relay_preserves_restricted_scope() {
    let temp = tempfile::tempdir().unwrap();
    let pm_dir = temp.path().join("tracker");
    crate::issue::Pm::init(&pm_dir).unwrap();
    for (key, prefix, policy) in [
        ("open", "OPEN", ""),
        (
            "restricted",
            "REST",
            "policy:\n  version: 1\n  visibility: private\n",
        ),
    ] {
        let project = pm_dir.join(key);
        let issue_dir = project.join(format!("{prefix}-1"));
        std::fs::create_dir_all(&issue_dir).unwrap();
        std::fs::write(
            project.join("project.yaml"),
            format!("key: {key}\nprefix: {prefix}\n{policy}"),
        )
        .unwrap();
        std::fs::write(
            issue_dir.join("issue.md"),
            format!("---\nid: {prefix}-1\ntitle: {key} issue\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"),
        ).unwrap();
    }
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let shared = Shared::new(temp.path(), &opts).unwrap();
    register(&shared, temp.path(), "reader");
    register(&shared, temp.path(), "worker");
    make_synthetic_legacy_unbound_job(&shared, &pm_dir, "reader", temp.path());
    let conn = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
         VALUES ('open-job','Open','spec.md','reader','OPEN-1','open',1,1),
                ('restricted-job','Restricted','spec.md','reader','REST-1','open',2,2);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
         VALUES ('open-task','open-job','Open task','worker','running',1,1),
                ('restricted-task','restricted-job','Restricted task','worker','running',2,2),
                ('unbound-task','unbound-job','Unbound task','worker','running',3,3);",
    )
    .unwrap();
    for (id, task, body) in [
        ("open-message", "open-task", "OPEN_RELAY_BODY"),
        (
            "restricted-message",
            "restricted-task",
            "RESTRICTED_RELAY_BODY",
        ),
        ("unbound-message", "unbound-task", "UNBOUND_RELAY_BODY"),
    ] {
        shared
            .store
            .enqueue_task("worker", body, None, id, "user", Some(task))
            .unwrap();
    }
    let pid = std::process::id();
    let reader = || Asserted::Agent("reader".to_string());
    let call = |who: Asserted, method: &str, params: Value| {
        scoped(who, || shared.dispatch(method, &params, pid))
    };
    scoped(reader(), || {
        assert!(matches!(
            shared.agent_caller(pid, "acceptance identity").unwrap(),
            AgentCaller::Agent(ref alias) if alias == "reader"
        ))
    });
    scoped(Asserted::Operator, || {
        assert!(matches!(
            shared.agent_caller(pid, "acceptance identity").unwrap(),
            AgentCaller::Operator
        ))
    });

    for principal in ["operator", "agents/worker", "agents/reader"] {
        for (method, params) in [
            ("agent_list", json!({"read_as": principal})),
            (
                "agent_show",
                json!({"alias": "worker", "read_as": principal}),
            ),
        ] {
            let error = call(reader(), method, params)
                .expect_err("an agent must not supply a relay identity, even its own");
            assert!(
                matches!(error, Error::Rejected(_)),
                "{method} {principal}: {error}"
            );
        }
    }

    let relayed_list = call(
        Asserted::Operator,
        "agent_list",
        json!({"read_as": "agents/reader"}),
    )
    .expect("operator relay as restricted reader must succeed");
    let worker = relayed_list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["alias"] == "worker")
        .expect("worker row must remain visible");
    assert_eq!(worker["tasks"], json!(["open-task"]));
    let relayed_show = call(
        Asserted::Operator,
        "agent_show",
        json!({
            "alias": "worker", "read_as": "agents/reader"
        }),
    )
    .expect("operator relay detail must succeed");
    let relay_text = relayed_show.to_string();
    assert!(relay_text.contains("OPEN_RELAY_BODY"));
    assert!(relay_text.contains("open-task"));
    for hidden in [
        "RESTRICTED_RELAY_BODY",
        "UNBOUND_RELAY_BODY",
        "restricted-task",
        "unbound-task",
    ] {
        assert!(
            !relay_text.contains(hidden),
            "relay exposed {hidden}: {relayed_show}"
        );
    }

    let recovery_list = call(Asserted::Operator, "agent_list", json!({}))
        .expect("operator without relay retains recovery access");
    let recovery_worker = recovery_list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["alias"] == "worker")
        .unwrap();
    let recovery_tasks = recovery_worker["tasks"].as_array().unwrap();
    for id in ["open-task", "restricted-task", "unbound-task"] {
        assert!(
            recovery_tasks
                .iter()
                .any(|value| value.as_str() == Some(id)),
            "operator lost {id}"
        );
    }
    let recovery_show = call(Asserted::Operator, "agent_show", json!({"alias": "worker"}))
        .expect("operator detail recovery must succeed");
    let recovery_text = recovery_show.to_string();
    for body in [
        "OPEN_RELAY_BODY",
        "RESTRICTED_RELAY_BODY",
        "UNBOUND_RELAY_BODY",
    ] {
        assert!(recovery_text.contains(body), "operator lost {body}");
    }
}

#[test]
fn thread_events_and_wait_do_not_disclose_inaccessible_work() {
    let temp = tempfile::tempdir().unwrap();
    private_dir(temp.path());
    let pm_dir = temp.path().join("tracker");
    crate::issue::Pm::init(&pm_dir).unwrap();
    for (key, prefix, policy) in [
        ("open", "OPEN", ""),
        (
            "restricted",
            "REST",
            "policy:\n  version: 1\n  visibility: private\n",
        ),
    ] {
        let project = pm_dir.join(key);
        let issue_dir = project.join(format!("{prefix}-1"));
        std::fs::create_dir_all(&issue_dir).unwrap();
        std::fs::write(
            project.join("project.yaml"),
            format!("key: {key}\nprefix: {prefix}\n{policy}"),
        )
        .unwrap();
        std::fs::write(
            issue_dir.join("issue.md"),
            format!("---\nid: {prefix}-1\ntitle: {key} issue\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"),
        ).unwrap();
    }
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let shared = Shared::new(temp.path(), &opts).unwrap();
    private_dir(&pm_dir);
    let keys = cursor_keys();
    initialize_cursor_keyring(&shared, temp.path(), &keys);
    register(&shared, temp.path(), "reader");
    register(&shared, temp.path(), "worker");
    make_synthetic_legacy_unbound_job(&shared, &pm_dir, "reader", temp.path());
    let conn = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
         VALUES ('open-job','Open','spec.md','reader','OPEN-1','open',1,1),
                ('restricted-job','Restricted','spec.md','reader','REST-1','open',2,2);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
         VALUES ('open-task','open-job','Open','worker','running',1,1),
                ('restricted-task','restricted-job','Restricted','worker','running',2,2),
                ('unbound-task','unbound-job','Unbound','worker','running',3,3);",
    )
    .unwrap();
    let pid = std::process::id();
    let reader = || Asserted::Agent("reader".to_string());
    let call = |who: Asserted, method: &str, params: Value| {
        scoped(who, || shared.dispatch(method, &params, pid))
    };
    scoped(reader(), || {
        assert!(matches!(
            shared.agent_caller(pid, "acceptance identity").unwrap(),
            AgentCaller::Agent(ref alias) if alias == "reader"
        ))
    });
    scoped(Asserted::Operator, || {
        assert!(matches!(
            shared.agent_caller(pid, "acceptance identity").unwrap(),
            AgentCaller::Operator
        ))
    });

    shared.store.ensure_thread("worker").unwrap();
    shared
        .store
        .enqueue_task(
            "worker",
            "OPEN_STREAM_BODY",
            None,
            "open-stream-message",
            "user",
            Some("open-task"),
        )
        .unwrap();
    let thread = call(
        reader(),
        "thread_read",
        json!({"alias": "worker", "cursor_format":"opaque-v1"}),
    )
    .expect("visible bound thread content must be readable");
    assert_eq!(
        thread["_reader_scope"],
        cadence_core::board_api::THREAD_OPAQUE_SCOPE_ACK
    );
    assert_no_global_seq(&thread, "thread page");
    assert!(thread.get("reply_seq").is_none() && thread.get("scanned_before").is_none());
    assert!(
        thread.to_string().contains("OPEN_STREAM_BODY"),
        "visible thread content missing: {thread}"
    );
    let thread_cursor = thread["cursor"]
        .as_str()
        .filter(|s| !s.is_empty() && s.parse::<i64>().is_err())
        .expect("opaque thread cursor")
        .to_string();
    let events = call(
        reader(),
        "agent_events",
        json!({
            "alias": "worker",
            "cursor_format": cadence_core::board_api::AGENT_EVENTS_CURSOR_FORMAT,
        }),
    )
    .expect("visible bound events must be readable");
    assert!(events.to_string().contains("open-stream-message"));
    let event_cursor = events["cursor"]
        .as_str()
        .filter(|cursor| !cursor.is_empty() && cursor.parse::<i64>().is_err())
        .expect("a scoped event page must carry an opaque cursor")
        .to_string();
    let empty_thread = call(
        reader(),
        "thread_read",
        json!({"alias": "worker", "cursor_format":"opaque-v1", "after": thread_cursor.clone()}),
    )
    .expect("empty thread continuation");
    assert_no_global_seq(&empty_thread, "empty thread page");
    let empty_events = call(
        reader(),
        "agent_events",
        json!({
            "alias": "worker",
            "cursor_format": cadence_core::board_api::AGENT_EVENTS_CURSOR_FORMAT,
            "after": event_cursor.clone(),
        }),
    )
    .expect("empty event continuation");

    for (id, task, body) in [
        (
            "restricted-stream-message",
            "restricted-task",
            "RESTRICTED_STREAM_BODY",
        ),
        (
            "unbound-stream-message",
            "unbound-task",
            "UNBOUND_STREAM_BODY",
        ),
    ] {
        shared
            .store
            .enqueue_task("worker", body, None, id, "user", Some(task))
            .unwrap();
    }
    for (method, params, before) in [
        (
            "thread_read",
            json!({"alias": "worker", "cursor_format":"opaque-v1", "after": thread_cursor.clone()}),
            &empty_thread,
        ),
        (
            "agent_events",
            json!({
                "alias": "worker",
                "cursor_format": cadence_core::board_api::AGENT_EVENTS_CURSOR_FORMAT,
                "after": event_cursor.clone(),
            }),
            &empty_events,
        ),
    ] {
        let after =
            call(reader(), method, params).expect("hidden work must not break continuation");
        assert_eq!(
            after["cursor"], before["cursor"],
            "{method} cursor disclosed hidden work"
        );
        if method == "thread_read" {
            assert_eq!(
                after["more_after"], before["more_after"],
                "hidden work changed thread page metadata"
            );
            assert_no_global_seq(&after, "continued thread page");
            assert!(after.get("reply_seq").is_none() && after.get("scanned_before").is_none());
        }
        let text = after.to_string();
        for secret in [
            "restricted-stream-message",
            "unbound-stream-message",
            "RESTRICTED_STREAM_BODY",
            "UNBOUND_STREAM_BODY",
            "restricted-task",
            "unbound-task",
        ] {
            assert!(!text.contains(secret), "{method} exposed {secret}: {after}");
        }
    }
    for (method, params) in [
        (
            "thread_read",
            json!({"alias": "worker", "tail": true, "cursor_format":"opaque-v1"}),
        ),
        (
            "agent_events",
            json!({
                "alias": "worker",
                "cursor_format": cadence_core::board_api::AGENT_EVENTS_CURSOR_FORMAT,
                "tail": true,
            }),
        ),
    ] {
        let scoped_page = call(reader(), method, params.clone()).expect("scoped tail must work");
        if method == "thread_read" {
            assert_eq!(
                scoped_page["_reader_scope"],
                cadence_core::board_api::THREAD_OPAQUE_SCOPE_ACK
            );
            assert_no_global_seq(&scoped_page, "thread tail");
        }
        let text = scoped_page.to_string();
        for secret in [
            "restricted-stream-message",
            "unbound-stream-message",
            "RESTRICTED_STREAM_BODY",
            "UNBOUND_STREAM_BODY",
            "restricted-task",
            "unbound-task",
        ] {
            assert!(
                !text.contains(secret),
                "{method} tail exposed {secret}: {scoped_page}"
            );
        }
        let operator_page =
            call(Asserted::Operator, method, params).expect("operator stream recovery");
        let operator_text = operator_page.to_string();
        assert!(operator_text.contains("restricted-stream-message"));
        assert!(operator_text.contains("unbound-stream-message"));
        if method == "thread_read" {
            assert!(operator_text.contains("RESTRICTED_STREAM_BODY"));
            assert!(operator_text.contains("UNBOUND_STREAM_BODY"));
            assert_eq!(
                operator_page["_reader_scope"],
                cadence_core::board_api::THREAD_OPERATOR_SCOPE_ACK
            );
            assert!(
                operator_page["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|e| e["seq"].is_number()),
                "operator lost numeric recovery: {operator_page}"
            );
        }
    }

    conn.execute_batch("UPDATE messages SET state='completed',completed=42 WHERE id IN ('open-stream-message','restricted-stream-message','unbound-stream-message');").unwrap();
    let wait = |who: Asserted, id: &str| {
        call(
            who,
            "agent_wait",
            json!({
                "alias": "worker", "until": "reported", "message": id, "timeout": 0
            }),
        )
    };
    let visible = wait(reader(), "open-stream-message").expect("visible completed message wait");
    assert_eq!(visible["message"], "open-stream-message");
    assert_eq!(visible["reason"], "reported");
    let absent =
        wait(reader(), "absent-stream-message").expect_err("missing message must be refused");
    let absent_text = match absent {
        Error::Rejected(text) => text,
        other => panic!("missing message had a non-rejection error: {other}"),
    };
    assert!(absent_text.contains("absent-stream-message"));
    let expected = absent_text.replace("absent-stream-message", "<requested>");
    for id in ["restricted-stream-message", "unbound-stream-message"] {
        let err = wait(reader(), id).expect_err("hidden message wait must be refused");
        let text = match err {
            Error::Rejected(text) => text,
            other => panic!("{id}: non-rejection error: {other}"),
        };
        assert_eq!(
            text.replace(id, "<requested>"),
            expected,
            "wait disclosed hidden message existence or state"
        );
        let recovered = wait(Asserted::Operator, id).expect("operator wait recovery");
        assert_eq!(recovered["message"], id);
        assert_eq!(recovered["reason"], "reported");
    }
}

#[test]
fn agent_events_preserves_authorized_cross_alias_task_dispatch() {
    let temp = tempfile::tempdir().unwrap();
    let pm_dir = temp.path().join("tracker");
    crate::issue::Pm::init(&pm_dir).unwrap();
    // The custody and keyring chains require owner-only roots, and `tempfile`
    // follows the ambient umask.
    private_dir(temp.path());
    private_dir(&pm_dir);
    for (key, prefix, policy) in [
        ("open", "OPEN", ""),
        (
            "restricted",
            "REST",
            "policy:\n  version: 1\n  visibility: private\n",
        ),
    ] {
        let project = pm_dir.join(key);
        let issue_dir = project.join(format!("{prefix}-1"));
        std::fs::create_dir_all(&issue_dir).unwrap();
        std::fs::write(
            project.join("project.yaml"),
            format!("key: {key}\nprefix: {prefix}\n{policy}"),
        )
        .unwrap();
        std::fs::write(
            issue_dir.join("issue.md"),
            format!("---\nid: {prefix}-1\ntitle: {key} issue\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"),
        ).unwrap();
    }
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let shared = Shared::new(temp.path(), &opts).unwrap();
    // A scoped `agent_events` read needs a bound keyring; without one the
    // daemon refuses the page for missing key material rather than for scope.
    let keys = cursor_keys();
    initialize_cursor_keyring(&shared, temp.path(), &keys);
    for alias in ["reader", "manager", "worker"] {
        register(&shared, temp.path(), alias);
    }
    let conn = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
         VALUES ('open-job','Open','spec.md','manager','OPEN-1','open',1,1),
                ('private-job','Private','spec.md','manager','REST-1','open',2,2);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
         VALUES ('open-task','open-job','Open','worker','running',1,1),
                ('private-task','private-job','Private','worker','running',2,2);",
    )
    .unwrap();
    shared
        .store
        .enqueue_task(
            "worker",
            "visible kickoff",
            None,
            "cross-alias-open-message",
            "user",
            Some("open-task"),
        )
        .unwrap();
    shared
        .store
        .enqueue_task(
            "worker",
            "private kickoff",
            None,
            "cross-alias-private-message",
            "user",
            Some("private-task"),
        )
        .unwrap();
    shared.store.event_public_scoped(
        "manager", "task_dispatched",
        json!({"task": "open-task", "job": "open-job", "assignee": "worker", "revision": 1, "message": "cross-alias-open-message", "by": "manager"}),
        Some("open-job"), Some("open-task"),
    ).unwrap();
    shared.store.event_public_scoped(
        "manager", "task_dispatched",
        json!({"task": "private-task", "job": "private-job", "assignee": "worker", "revision": 1, "message": "cross-alias-private-message", "by": "manager"}),
        Some("private-job"), Some("private-task"),
    ).unwrap();
    // An inconsistent retained event must not launder a private message
    // through an otherwise readable job/task binding.
    shared.store.event_public_scoped(
        "manager", "task_dispatched",
        json!({"task": "open-task", "job": "open-job", "assignee": "worker", "revision": 2, "message": "cross-alias-private-message", "by": "manager", "marker": "MISMATCHED_EVENT_SENTINEL"}),
        Some("open-job"), Some("open-task"),
    ).unwrap();
    // Synthetic unproved cloud ancestry on the worker, attached to the
    // readable task. These are real retained table rows, not cloud grants
    // or evidence of an authorized remote operation.
    conn.execute_batch(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,created)
         VALUES('cross-alias-unproved-cloud','worker','UNPROVED_CLOUD_BODY','cloud_agent','completed','open-task','OPEN-1',1);
         INSERT INTO messages(id,alias,body,source,state,task_id,issue,origin,created)
         VALUES('cross-alias-unproved-derived','worker','UNPROVED_DERIVED_BODY','worker_result','completed','open-task','OPEN-1','cross-alias-unproved-cloud',1);",
    ).unwrap();
    for (id, marker) in [
        ("cross-alias-unproved-cloud", "UNPROVED_CLOUD_EVENT"),
        ("cross-alias-unproved-derived", "UNPROVED_DERIVED_EVENT"),
    ] {
        shared.store.event_public_scoped(
            "manager", "task_dispatched",
            json!({"task":"open-task", "job":"open-job", "assignee":"worker", "message":id, "marker":marker}),
            Some("open-job"), Some("open-task"),
        ).unwrap();
    }
    let pid = std::process::id();
    let call = |who: Asserted| {
        scoped(who, || {
            shared.dispatch(
                "agent_events",
                &json!({
                    "alias": "manager",
                    "cursor_format": cadence_core::board_api::AGENT_EVENTS_CURSOR_FORMAT,
                }),
                pid,
            )
        })
    };
    scoped(Asserted::Agent("reader".to_string()), || {
        assert!(matches!(
            shared.agent_caller(pid, "acceptance identity").unwrap(),
            AgentCaller::Agent(ref alias) if alias == "reader"
        ))
    });
    scoped(Asserted::Operator, || {
        assert!(matches!(
            shared.agent_caller(pid, "acceptance identity").unwrap(),
            AgentCaller::Operator
        ))
    });
    let visible = call(Asserted::Agent("reader".to_string())).expect("reader event stream");
    let text = visible.to_string();
    for hidden in [
        "cross-alias-unproved-cloud",
        "cross-alias-unproved-derived",
        "UNPROVED_CLOUD_EVENT",
        "UNPROVED_DERIVED_EVENT",
    ] {
        assert!(
            !text.contains(hidden),
            "reader event page exposed unproved cloud ancestry {hidden}: {visible}"
        );
    }
    assert!(
        text.contains("cross-alias-open-message"),
        "authorized cross-alias dispatch missing: {visible}"
    );
    for hidden in [
        "cross-alias-private-message",
        "private-task",
        "private-job",
        "MISMATCHED_EVENT_SENTINEL",
    ] {
        assert!(
            !text.contains(hidden),
            "event stream disclosed {hidden}: {visible}"
        );
    }
    let operator = call(Asserted::Operator).expect("operator event recovery");
    let text = operator.to_string();
    for hidden in [
        "cross-alias-unproved-cloud",
        "cross-alias-unproved-derived",
        "UNPROVED_CLOUD_EVENT",
        "UNPROVED_DERIVED_EVENT",
    ] {
        assert!(!text.contains(hidden), "operator event page exposed cloud ancestry without proven backing authority {hidden}: {operator}");
    }
    for recovered in [
        "cross-alias-open-message",
        "cross-alias-private-message",
        "MISMATCHED_EVENT_SENTINEL",
    ] {
        assert!(
            text.contains(recovered),
            "operator event recovery lost {recovered}: {operator}"
        );
    }
}

#[test]
fn agent_identity_hides_unreadable_task_binding_but_preserves_own_turn_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let pm_dir = temp.path().join("tracker");
    crate::issue::Pm::init(&pm_dir).unwrap();
    let project = pm_dir.join("open");
    let issue_dir = project.join("OPEN-1");
    std::fs::create_dir_all(&issue_dir).unwrap();
    let policy_path = project.join("project.yaml");
    std::fs::write(&policy_path, "key: open\nprefix: OPEN\n").unwrap();
    std::fs::write(
        issue_dir.join("issue.md"),
        "---\nid: OPEN-1\ntitle: visible issue\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n",
    ).unwrap();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let shared = Shared::new(temp.path(), &opts).unwrap();
    register(&shared, temp.path(), "worker");
    make_synthetic_legacy_unbound_job(&shared, &pm_dir, "worker", temp.path());
    let conn = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
         VALUES ('visible-job','Visible','spec.md','worker','OPEN-1','open',1,1);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
         VALUES ('visible-held-task','visible-job','Visible','worker','running',1,1),
                ('unbound-held-task','unbound-job','Unbound','worker','running',2,2);",
    )
    .unwrap();
    let pid = std::process::id();
    let owner = || Asserted::Agent("worker".to_string());
    scoped(owner(), || {
        assert!(matches!(
            shared.caller_identity(pid).unwrap(), Caller::Agent(ref verified) if verified.agent.alias == "worker"
        ))
    });
    let identity = || {
        scoped(owner(), || {
            shared.dispatch("agent_identity", &json!({}), pid)
        })
        .expect("registered owner must inspect its held turn")
    };

    shared
        .store
        .enqueue_task(
            "worker",
            "visible turn",
            None,
            "visible-held-message",
            "user",
            Some("visible-held-task"),
        )
        .unwrap();
    shared
        .store
        .mark_running("visible-held-message", "visible-held-token-7391")
        .unwrap();
    let before = identity();
    let running = before["running"].as_array().unwrap();
    assert_eq!(running.len(), 1, "{before}");
    assert_eq!(running[0]["id"], "visible-held-message");
    assert_eq!(running[0]["turn_id"], "visible-held-token-7391");
    assert_eq!(running[0]["task"], "visible-held-task");

    // Change policy while the same turn is held: its message and token
    // remain necessary to report, but its task binding is no longer readable.
    std::fs::write(
        &policy_path,
        "key: open\nprefix: OPEN\npolicy:\n  version: 1\n  visibility: private\n",
    )
    .unwrap();
    let revoked = identity();
    let running = revoked["running"].as_array().unwrap();
    assert_eq!(running.len(), 1, "{revoked}");
    assert_eq!(running[0]["id"], "visible-held-message");
    assert_eq!(running[0]["turn_id"], "visible-held-token-7391");
    assert!(
        !revoked.to_string().contains("visible-held-task"),
        "revoked task leaked: {revoked}"
    );

    conn.execute(
        "UPDATE messages SET state='completed',completed=42 WHERE id='visible-held-message'",
        [],
    )
    .unwrap();
    shared
        .store
        .enqueue_task(
            "worker",
            "unbound turn",
            None,
            "unbound-held-message",
            "user",
            Some("unbound-held-task"),
        )
        .unwrap();
    shared
        .store
        .mark_running("unbound-held-message", "unbound-held-token-7392")
        .unwrap();
    let unbound = identity();
    let running = unbound["running"].as_array().unwrap();
    assert_eq!(running.len(), 1, "{unbound}");
    assert_eq!(running[0]["id"], "unbound-held-message");
    assert_eq!(running[0]["turn_id"], "unbound-held-token-7392");
    assert!(
        !unbound.to_string().contains("unbound-held-task"),
        "unbound task leaked: {unbound}"
    );
}

#[test]
fn unnamed_agent_wait_skips_hidden_completions_and_scans_beyond_first_page() {
    let temp = tempfile::tempdir().unwrap();
    let pm_dir = temp.path().join("tracker");
    crate::issue::Pm::init(&pm_dir).unwrap();
    for (key, prefix, policy) in [
        ("open", "OPEN", ""),
        (
            "restricted",
            "REST",
            "policy:\n  version: 1\n  visibility: private\n",
        ),
    ] {
        let project = pm_dir.join(key);
        let issue_dir = project.join(format!("{prefix}-1"));
        std::fs::create_dir_all(&issue_dir).unwrap();
        std::fs::write(
            project.join("project.yaml"),
            format!("key: {key}\nprefix: {prefix}\n{policy}"),
        )
        .unwrap();
        std::fs::write(
            issue_dir.join("issue.md"),
            format!("---\nid: {prefix}-1\ntitle: {key} issue\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"),
        ).unwrap();
    }
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<(String, i64)>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = std::sync::Mutex::new(release_rx);
    let mut opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    opts.wait_baseline_observer = Some(Arc::new(move |alias, cursor| {
        ready_tx.send((alias.to_string(), cursor)).unwrap();
        release_rx.lock().unwrap().recv().unwrap();
    }));
    let shared = Shared::new(temp.path(), &opts).unwrap();
    register(&shared, temp.path(), "reader");
    register(&shared, temp.path(), "worker");
    make_synthetic_legacy_unbound_job(&shared, &pm_dir, "reader", temp.path());
    let conn = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
         VALUES ('open-job','Open','spec.md','reader','OPEN-1','open',1,1),
                ('restricted-job','Restricted','spec.md','reader','REST-1','open',2,2);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
         VALUES ('open-task','open-job','Open','worker','running',1,1),
                ('restricted-task','restricted-job','Restricted','worker','running',2,2),
                ('unbound-task','unbound-job','Unbound','worker','running',3,3);",
    )
    .unwrap();
    for (id, task) in [
        ("wait-open-message", "open-task"),
        ("wait-restricted-message", "restricted-task"),
        ("wait-unbound-message", "unbound-task"),
    ] {
        shared
            .store
            .enqueue_task("worker", "fixture turn", None, id, "user", Some(task))
            .unwrap();
    }
    let start_wait = |who: Asserted| {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            scoped(who, || {
                shared.dispatch(
                    "agent_wait",
                    &json!({"alias": "worker", "until": "reported", "timeout": 10}),
                    std::process::id(),
                )
            })
        })
    };
    let reader_wait = start_wait(Asserted::Agent("reader".to_string()));
    let (alias, _) = ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("reader baseline witness");
    assert_eq!(alias, "worker");
    for n in 0..120 {
        shared
            .store
            .event_public_scoped(
                "worker",
                "fixture_progress",
                json!({"ordinal": n}),
                Some("open-job"),
                Some("open-task"),
            )
            .unwrap();
    }
    conn.execute_batch(
        "UPDATE messages SET state='completed',completed=42 WHERE id IN
         ('wait-restricted-message','wait-unbound-message','wait-open-message');",
    )
    .unwrap();
    for (id, job, task) in [
        (
            "wait-restricted-message",
            "restricted-job",
            "restricted-task",
        ),
        ("wait-unbound-message", "unbound-job", "unbound-task"),
        ("wait-open-message", "open-job", "open-task"),
    ] {
        shared
            .store
            .event_public_scoped(
                "worker",
                "turn_finished",
                json!({"message": id}),
                Some(job),
                Some(task),
            )
            .unwrap();
    }
    release_tx.send(()).unwrap();
    shared.wake();
    let reader_result = reader_wait
        .join()
        .expect("reader wait thread")
        .expect("visible completion after 120 events must be found");
    assert_eq!(reader_result["reason"], "reported");
    assert_eq!(reader_result["message"], "wait-open-message");
    let text = reader_result.to_string();
    for hidden in [
        "wait-restricted-message",
        "wait-unbound-message",
        "restricted-task",
        "unbound-task",
    ] {
        assert!(
            !text.contains(hidden),
            "unnamed wait disclosed {hidden}: {reader_result}"
        );
    }

    let operator_wait = start_wait(Asserted::Operator);
    let (alias, _) = ready_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("operator baseline witness");
    assert_eq!(alias, "worker");
    shared
        .store
        .event_public_scoped(
            "worker",
            "turn_finished",
            json!({"message": "wait-unbound-message"}),
            Some("unbound-job"),
            Some("unbound-task"),
        )
        .unwrap();
    release_tx.send(()).unwrap();
    shared.wake();
    let recovered = operator_wait
        .join()
        .expect("operator wait thread")
        .expect("operator completion recovery");
    assert_eq!(recovered["reason"], "reported");
    assert_eq!(recovered["message"], "wait-unbound-message");
}

#[test]
fn unnamed_wait_stays_pending_after_hidden_only_batch_and_operator_can_recover() {
    let temp = tempfile::tempdir().unwrap();
    let pm_dir = temp.path().join("tracker");
    crate::issue::Pm::init(&pm_dir).unwrap();
    for (key, prefix, policy) in [
        ("open", "OPEN", ""),
        (
            "restricted",
            "REST",
            "policy:\n  version: 1\n  visibility: private\n",
        ),
    ] {
        let project = pm_dir.join(key);
        let issue_dir = project.join(format!("{prefix}-1"));
        std::fs::create_dir_all(&issue_dir).unwrap();
        std::fs::write(
            project.join("project.yaml"),
            format!("key: {key}\nprefix: {prefix}\n{policy}"),
        )
        .unwrap();
        std::fs::write(
            issue_dir.join("issue.md"),
            format!("---\nid: {prefix}-1\ntitle: {key} issue\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"),
        ).unwrap();
    }
    let (baseline_tx, baseline_rx) = std::sync::mpsc::channel::<(String, i64)>();
    let (baseline_release_tx, baseline_release_rx) = std::sync::mpsc::channel::<()>();
    let baseline_release_rx = std::sync::Mutex::new(baseline_release_rx);
    let (scan_tx, scan_rx) = std::sync::mpsc::channel::<(String, i64)>();
    let (scan_release_tx, scan_release_rx) = std::sync::mpsc::channel::<()>();
    let scan_release_rx = std::sync::Mutex::new(scan_release_rx);
    let mut opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    opts.wait_baseline_observer = Some(Arc::new(move |alias, cursor| {
        baseline_tx.send((alias.to_string(), cursor)).unwrap();
        baseline_release_rx.lock().unwrap().recv().unwrap();
    }));
    opts.wait_scan_observer = Some(Arc::new(move |alias, cursor| {
        scan_tx.send((alias.to_string(), cursor)).unwrap();
        scan_release_rx.lock().unwrap().recv().unwrap();
    }));
    let shared = Shared::new(temp.path(), &opts).unwrap();
    register(&shared, temp.path(), "reader");
    register(&shared, temp.path(), "worker");
    make_synthetic_legacy_unbound_job(&shared, &pm_dir, "reader", temp.path());
    let conn = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
         VALUES ('open-job','Open','spec.md','reader','OPEN-1','open',1,1),
                ('private-job','Private','spec.md','reader','REST-1','open',2,2);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
         VALUES ('open-task','open-job','Open','worker','running',1,1),
                ('private-task','private-job','Private','worker','running',2,2),
                ('unbound-task','unbound-job','Unbound','worker','running',3,3);",
    )
    .unwrap();
    for (id, task) in [
        ("pending-open-message", "open-task"),
        ("pending-private-message", "private-task"),
        ("pending-unbound-message", "unbound-task"),
    ] {
        shared
            .store
            .enqueue_task("worker", "fixture turn", None, id, "user", Some(task))
            .unwrap();
    }
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let waiter_shared = Arc::clone(&shared);
    let waiter = std::thread::spawn(move || {
        let result = scoped(Asserted::Agent("reader".to_string()), || {
            waiter_shared.dispatch(
                "agent_wait",
                &json!({"alias": "worker", "until": "reported", "timeout": 10}),
                std::process::id(),
            )
        });
        result_tx.send(result).unwrap();
    });
    let (alias, _) = baseline_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("reader baseline witness");
    assert_eq!(alias, "worker");
    conn.execute_batch("UPDATE messages SET state='completed',completed=42 WHERE id IN ('pending-private-message','pending-unbound-message');").unwrap();
    for (id, job, task) in [
        ("pending-private-message", "private-job", "private-task"),
        ("pending-unbound-message", "unbound-job", "unbound-task"),
    ] {
        shared
            .store
            .event_public_scoped(
                "worker",
                "turn_finished",
                json!({"message": id}),
                Some(job),
                Some(task),
            )
            .unwrap();
    }
    baseline_release_tx.send(()).unwrap();
    let (alias, _) = scan_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("hidden-only batch must be scanned without a hit");
    assert_eq!(alias, "worker");
    assert!(
        matches!(
            result_rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ),
        "hidden-only completion must not return a result"
    );
    conn.execute(
        "UPDATE messages SET state='completed',completed=42 WHERE id='pending-open-message'",
        [],
    )
    .unwrap();
    shared
        .store
        .event_public_scoped(
            "worker",
            "turn_finished",
            json!({"message": "pending-open-message"}),
            Some("open-job"),
            Some("open-task"),
        )
        .unwrap();
    scan_release_tx.send(()).unwrap();
    shared.wake();
    let result = result_rx
        .recv_timeout(Duration::from_secs(12))
        .expect("reader wait result")
        .expect("visible completion must resolve the wait");
    waiter.join().unwrap();
    assert_eq!(result["reason"], "reported");
    assert_eq!(result["message"], "pending-open-message");
    for hidden in [
        "pending-private-message",
        "pending-unbound-message",
        "private-task",
        "unbound-task",
    ] {
        assert!(
            !result.to_string().contains(hidden),
            "reader wait disclosed {hidden}: {result}"
        );
    }

    let operator_shared = Arc::clone(&shared);
    let operator = std::thread::spawn(move || {
        scoped(Asserted::Operator, || {
            operator_shared.dispatch(
                "agent_wait",
                &json!({"alias": "worker", "until": "reported", "timeout": 10}),
                std::process::id(),
            )
        })
    });
    let (alias, _) = baseline_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("operator baseline witness");
    assert_eq!(alias, "worker");
    shared
        .store
        .event_public_scoped(
            "worker",
            "turn_finished",
            json!({"message": "pending-unbound-message"}),
            Some("unbound-job"),
            Some("unbound-task"),
        )
        .unwrap();
    baseline_release_tx.send(()).unwrap();
    shared.wake();
    let recovered = operator
        .join()
        .expect("operator wait thread")
        .expect("operator recovery completion");
    assert_eq!(recovered["reason"], "reported");
    assert_eq!(recovered["message"], "pending-unbound-message");
}

#[test]
fn member_thread_pages_exclude_cloud_ancestry_before_pagination() {
    let temp = tempfile::tempdir().unwrap();
    let shared = cad627_shared(temp.path());
    let thread = shared.store.ensure_thread("w1").unwrap();
    let conn = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
         VALUES('page-job','Page','spec.md','w1','OPEN-1','open',1,1);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
         VALUES('page-task','page-job','Page','w1','running',1,1);",
    )
    .unwrap();
    shared
        .store
        .enqueue_task(
            "w1",
            "LOCAL_BEFORE",
            None,
            "local-before",
            "user",
            Some("page-task"),
        )
        .unwrap();
    // Synthetic retained provenance, inserted directly for this SQL-only
    // test. The origin link is present at insertion; no credentials or
    // cloud authorization are claimed by these rows.
    conn.execute(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,created) VALUES('cloud-origin','w1','CLOUD_SOURCE','cloud_agent','completed','page-task','OPEN-1',1)", [],
    ).unwrap();
    shared
        .store
        .thread_append(
            "w1",
            crate::store::NewEntry {
                role: "system",
                kind: "message",
                text: "CLOUD_SOURCE",
                payload: None,
                message_id: Some("cloud-origin"),
            },
        )
        .unwrap()
        .expect("thread entry for cloud origin");
    shared
        .store
        .enqueue_task(
            "w1",
            "LOCAL_AFTER",
            None,
            "local-after",
            "user",
            Some("page-task"),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,origin,created) VALUES('cloud-derived','w1','CLOUD_DERIVED','worker_result','completed','page-task','OPEN-1','cloud-origin',1)", [],
    ).unwrap();
    shared
        .store
        .thread_append(
            "w1",
            crate::store::NewEntry {
                role: "system",
                kind: "message",
                text: "CLOUD_DERIVED",
                payload: None,
                message_id: Some("cloud-derived"),
            },
        )
        .unwrap()
        .expect("thread entry for cloud-derived result");
    // Retained routed row whose immutable origin points to missing data.
    conn.execute(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,origin,created) VALUES('orphan-derived','w1','ORPHAN_DERIVED','worker_result','running','page-task','OPEN-1','missing-origin',1)", [],
    ).unwrap();
    shared
        .store
        .thread_append(
            "w1",
            crate::store::NewEntry {
                role: "system",
                kind: "message",
                text: "ORPHAN_DERIVED",
                payload: None,
                message_id: Some("orphan-derived"),
            },
        )
        .unwrap()
        .expect("thread entry for orphan-derived result");
    assert_eq!(
        shared
            .store
            .message("orphan-derived")
            .unwrap()
            .unwrap()
            .origin
            .as_deref(),
        Some("missing-origin")
    );
    assert!(shared.store.message("missing-origin").unwrap().is_none());
    // Explicit synthetic corrupt-ledger fixture: two retained origins cycle.
    // Both links are fixed at row insertion; no cloud credential is invented.
    conn.execute_batch(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,origin,created)
         VALUES('cycle-first','w1','CYCLE_FIRST','worker_result','running','page-task','OPEN-1','cycle-second',1),
               ('cycle-second','w1','CYCLE_SECOND','worker_result','running','page-task','OPEN-1','cycle-first',1);",
    ).unwrap();
    for (id, text) in [
        ("cycle-first", "CYCLE_FIRST"),
        ("cycle-second", "CYCLE_SECOND"),
    ] {
        shared
            .store
            .thread_append(
                "w1",
                crate::store::NewEntry {
                    role: "system",
                    kind: "message",
                    text,
                    payload: None,
                    message_id: Some(id),
                },
            )
            .unwrap()
            .expect("thread entry for cyclic provenance");
        let (derived, ordinary) = shared.store.cloud_projection_rows("w1", Some(id)).unwrap();
        assert!(
            derived.iter().any(|row| row.message_id == id),
            "cyclic origin must classify untrusted: {id}"
        );
        assert!(
            !ordinary.iter().any(|row| row == id),
            "cyclic origin classified ordinary: {id}"
        );
    }
    let (orphan_cloud, orphan_ordinary) = shared
        .store
        .cloud_projection_rows("w1", Some("orphan-derived"))
        .unwrap();
    assert!(
        orphan_cloud
            .iter()
            .any(|row| row.message_id == "orphan-derived"),
        "missing origin must classify as untrusted cloud-derived"
    );
    assert!(
        !orphan_ordinary.iter().any(|id| id == "orphan-derived"),
        "missing origin was classified ordinary"
    );
    let (local_cloud, local_ordinary) = shared
        .store
        .cloud_projection_rows("w1", Some("local-after"))
        .unwrap();
    assert!(
        local_cloud.is_empty(),
        "ordinary local message classified as cloud-derived"
    );
    assert!(
        local_ordinary.iter().any(|id| id == "local-after"),
        "ordinary local message missing from ordinary projection"
    );
    shared
        .store
        .mark_running("local-after", "ordinary-page-token")
        .unwrap();
    let origin = shared.store.message("cloud-derived").unwrap().unwrap();
    assert_eq!(origin.origin.as_deref(), Some("cloud-origin"));
    assert_eq!(
        shared
            .store
            .message("cloud-origin")
            .unwrap()
            .unwrap()
            .source,
        "cloud_agent"
    );

    let denied = crate::store::PrivateJobDenials {
        exclude_cloud: true,
        ..Default::default()
    };
    let running = shared
        .store
        .scoped_running_message_ids(&["OPEN-1"], &denied)
        .unwrap();
    let visible_running = running
        .get("w1")
        .expect("ordinary running message must remain visible");
    assert!(visible_running.iter().any(|id| id == "local-after"));
    assert!(
        !visible_running.iter().any(|id| id == "orphan-derived"),
        "missing ancestry leaked in running ID projection"
    );
    for id in ["cycle-first", "cycle-second"] {
        assert!(
            !visible_running.iter().any(|row| row == id),
            "cyclic origin leaked in scoped running IDs: {id}"
        );
    }
    let operator_ids = shared
        .store
        .scoped_running_message_ids(&["OPEN-1"], &crate::store::PrivateJobDenials::default())
        .unwrap();
    assert!(
        operator_ids
            .get("w1")
            .unwrap()
            .iter()
            .any(|id| id == "orphan-derived"),
        "unexcluded store query lost retained orphan"
    );
    for id in ["cycle-first", "cycle-second"] {
        assert!(
            operator_ids.get("w1").unwrap().iter().any(|row| row == id),
            "unexcluded store query lost cyclic row: {id}"
        );
    }
    let first = shared
        .store
        .thread_entries_after_scoped(&thread.id, 0, 1, &["OPEN-1"], &denied, true)
        .unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].message_id.as_deref(), Some("local-before"));
    let second = shared
        .store
        .thread_entries_after_scoped(&thread.id, first[0].seq, 1, &["OPEN-1"], &denied, true)
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(
        second[0].message_id.as_deref(),
        Some("local-after"),
        "cloud rows consumed the member's forward page"
    );
    let third = shared
        .store
        .thread_entries_after_scoped(&thread.id, second[0].seq, 1, &["OPEN-1"], &denied, true)
        .unwrap();
    assert!(
        third.is_empty(),
        "cloud descendant advanced the member cursor: {third:?}"
    );
    let (tail, more) = shared
        .store
        .thread_entries_before_scoped(&thread.id, None, 1, &["OPEN-1"], &denied, true)
        .unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(
        tail[0].message_id.as_deref(),
        Some("local-after"),
        "cloud descendant consumed the member tail"
    );
    assert_eq!(tail[0].seq, second[0].seq);
    assert!(more, "the older visible row must remain reachable");

    let all = shared.store.thread_entries_of(&thread.id, 0, 100).unwrap();
    let ids = all
        .iter()
        .filter_map(|entry| entry.message_id.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            "local-before",
            "cloud-origin",
            "local-after",
            "cloud-derived",
            "orphan-derived",
            "cycle-first",
            "cycle-second"
        ]
    );
    let (unscoped_tail, _) = shared
        .store
        .thread_entries_before_of(&thread.id, None, 1)
        .unwrap();
    assert_eq!(unscoped_tail[0].message_id.as_deref(), Some("cycle-second"));
}

#[test]
fn scoped_thread_pagination_does_not_reveal_unrelated_hidden_global_sequence_volume() {
    fn observe(add_hidden: bool) -> Vec<Value> {
        let temp = tempfile::tempdir().unwrap();
        private_dir(temp.path());
        let pm_dir = temp.path().join("tracker");
        crate::issue::Pm::init(&pm_dir).unwrap();
        for (key, prefix, policy) in [
            ("open", "OPEN", ""),
            (
                "restricted",
                "REST",
                "policy:\n  version: 1\n  visibility: private\n",
            ),
        ] {
            let project = pm_dir.join(key);
            let issue = project.join(format!("{prefix}-1"));
            std::fs::create_dir_all(&issue).unwrap();
            std::fs::write(
                project.join("project.yaml"),
                format!("key: {key}\nprefix: {prefix}\n{policy}"),
            )
            .unwrap();
            std::fs::write(issue.join("issue.md"), format!(
                "---\nid: {prefix}-1\ntitle: {key} issue\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"
            )).unwrap();
        }
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
        let shared = Shared::new(temp.path(), &opts).unwrap();
        private_dir(&pm_dir);
        let keys = cursor_keys();
        initialize_cursor_keyring(&shared, temp.path(), &keys);
        for alias in ["reader", "visible-worker", "private-worker"] {
            register(&shared, temp.path(), alias);
        }
        shared.store.ensure_thread("visible-worker").unwrap();
        shared.store.ensure_thread("private-worker").unwrap();
        let db = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
        db.execute_batch(
            "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
             VALUES('public-job','Public','spec.md','reader','OPEN-1','open',1,1),
                   ('hidden-job','Hidden','spec.md','reader','REST-1','open',2,2);
             INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
             VALUES('public-task','public-job','Public','visible-worker','running',1,1),
                   ('hidden-task','hidden-job','Hidden','private-worker','running',2,2);",
        )
        .unwrap();
        shared
            .store
            .enqueue_task(
                "visible-worker",
                "VISIBLE_FIRST",
                None,
                "visible-first",
                "user",
                Some("public-task"),
            )
            .unwrap();
        if add_hidden {
            for id in ["hidden-one", "hidden-two", "hidden-three"] {
                shared
                    .store
                    .enqueue_task(
                        "private-worker",
                        "HIDDEN_VOLUME_SENTINEL",
                        None,
                        id,
                        "user",
                        Some("hidden-task"),
                    )
                    .unwrap();
            }
        }
        shared
            .store
            .enqueue_task(
                "visible-worker",
                "VISIBLE_SECOND",
                None,
                "visible-second",
                "user",
                Some("public-task"),
            )
            .unwrap();
        shared
            .store
            .thread_append(
                "visible-worker",
                crate::store::NewEntry {
                    role: "agent",
                    kind: "assistant_text",
                    text: "VISIBLE_REPLY",
                    payload: None,
                    message_id: Some("visible-second"),
                },
            )
            .unwrap()
            .expect("visible reply entry");
        let reader = || Asserted::Agent("reader".to_string());
        let call = |who: Asserted, params: Value| {
            scoped(who, || {
                shared.dispatch("thread_read", &params, std::process::id())
            })
        };
        let check = |page: &Value| {
            assert_eq!(
                page["_reader_scope"],
                cadence_core::board_api::THREAD_OPAQUE_SCOPE_ACK
            );
            assert_eq!(page["cursor_format"], "opaque-v1");
            assert_no_global_seq(page, "scoped thread page");
            assert!(page.get("reply_seq").is_none() && page.get("scanned_before").is_none());
            let rows = page["entries"].as_array().expect("entries");
            assert_eq!(rows.len(), 1, "expected exactly one visible row: {page}");
            assert!(
                rows[0]["id"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty() && id.parse::<i64>().is_err()),
                "opaque row id missing: {page}"
            );
            assert!(!page.to_string().contains("HIDDEN_VOLUME_SENTINEL"));
        };
        let token = |page: &Value| {
            page["cursor"]
                .as_str()
                .filter(|s| !s.is_empty() && s.parse::<i64>().is_err())
                .expect("opaque continuation cursor")
                .to_string()
        };
        let first = call(
            reader(),
            json!({"alias":"visible-worker", "cursor_format":"opaque-v1", "limit":1}),
        )
        .expect("first visible page");
        check(&first);
        assert!(
            first.to_string().contains("VISIBLE_FIRST"),
            "first visible entry missing: {first}"
        );
        let first_cursor = token(&first);
        let second = call(reader(), json!({"alias":"visible-worker", "cursor_format":"opaque-v1", "after":first_cursor, "limit":1})).expect("second visible page");
        check(&second);
        assert!(
            second.to_string().contains("VISIBLE_SECOND"),
            "second visible entry missing: {second}"
        );
        let second_cursor = token(&second);
        let third = call(reader(), json!({"alias":"visible-worker", "cursor_format":"opaque-v1", "after":second_cursor, "limit":1})).expect("visible reply page");
        check(&third);
        assert!(
            third.to_string().contains("VISIBLE_REPLY"),
            "visible reply missing: {third}"
        );
        let tail = call(
            reader(),
            json!({"alias":"visible-worker", "cursor_format":"opaque-v1", "tail":true, "limit":1}),
        )
        .expect("visible tail");
        check(&tail);
        assert!(
            tail.to_string().contains("VISIBLE_REPLY"),
            "tail must preserve visible reply: {tail}"
        );
        if add_hidden {
            let operator = call(Asserted::Operator, json!({"alias":"private-worker"}))
                .expect("operator private-thread recovery");
            assert!(
                operator.to_string().contains("HIDDEN_VOLUME_SENTINEL"),
                "hidden fixture never reached real thread: {operator}"
            );
        }
        // Nonces and workspace-bound IDs differ across fixtures; compare the
        // observable shape, length and visible semantics, never random bytes.
        [first, second, third, tail]
            .into_iter()
            .map(|page| {
                json!({
                    "text": page["entries"][0]["text"],
                    "cursor_length": page["cursor"].as_str().map(str::len),
                    "before_cursor_length": page["before_cursor"].as_str().map(str::len),
                    "more_before": page["more_before"],
                    "more_after": page["more_after"],
                })
            })
            .collect()
    }
    let without_hidden = observe(false);
    let with_hidden = observe(true);
    assert_eq!(
        with_hidden, without_hidden,
        "unrelated hidden thread traffic changed member-visible observable page or cursor metadata"
    );
}

#[test]
fn scoped_message_event_job_and_alert_counters_do_not_reveal_hidden_volume() {
    fn observe(add_hidden: bool) -> Value {
        let temp = tempfile::tempdir().unwrap();
        // The scoped-cursor record is read through the strict 0700 state chain.
        private_dir(temp.path());
        let pm_dir = temp.path().join("tracker");
        crate::issue::Pm::init(&pm_dir).unwrap();
        for (key, prefix, policy) in [
            ("open", "OPEN", ""),
            (
                "restricted",
                "REST",
                "policy:\n  version: 1\n  visibility: private\n",
            ),
        ] {
            let project = pm_dir.join(key);
            let issue = project.join(format!("{prefix}-1"));
            std::fs::create_dir_all(&issue).unwrap();
            std::fs::write(
                project.join("project.yaml"),
                format!("key: {key}\nprefix: {prefix}\n{policy}"),
            )
            .unwrap();
            std::fs::write(issue.join("issue.md"), format!(
                "---\nid: {prefix}-1\ntitle: {key} issue\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"
            )).unwrap();
        }
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
        let shared = Shared::new(temp.path(), &opts).unwrap();
        for alias in ["reader", "hidden-worker"] {
            register(&shared, temp.path(), alias);
        }
        // Strict keyring admission pins the tracker chain: a fixture tracker
        // must be owner-only (the runner umask can otherwise leave it 0775).
        private_dir(&pm_dir);
        let keys = cursor_keys();
        initialize_cursor_keyring(&shared, temp.path(), &keys);
        let db = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
        db.execute_batch(
            "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,repo,state,created,updated)
             VALUES('public-job','Public','spec.md','reader','OPEN-1','fixture','open',1,1),
                   ('hidden-job','Hidden','spec.md','reader','REST-1','fixture','open',2,2);
             INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
             VALUES('public-task','public-job','Public','reader','running',1,1),
                   ('hidden-task','hidden-job','Hidden','hidden-worker','running',2,2);",
        )
        .unwrap();
        let reader = || Asserted::Agent("reader".to_string());
        let call = |who: Asserted, method: &str, params: Value| {
            scoped(who, || shared.dispatch(method, &params, std::process::id()))
        };
        shared
            .store
            .enqueue_task(
                "reader",
                "VISIBLE_MESSAGE_A",
                None,
                "visible-message-a",
                "user",
                Some("public-task"),
            )
            .unwrap();
        if add_hidden {
            shared
                .store
                .enqueue_task(
                    "hidden-worker",
                    "HIDDEN_MESSAGE_SENTINEL",
                    None,
                    "hidden-message",
                    "user",
                    Some("hidden-task"),
                )
                .unwrap();
        }
        shared
            .store
            .enqueue_task(
                "reader",
                "VISIBLE_MESSAGE_B",
                None,
                "visible-message-b",
                "user",
                Some("public-task"),
            )
            .unwrap();
        let messages = call(reader(), "message_list", json!({"to":"reader"}))
            .expect("own readable message list");
        let rows = messages["messages"].as_array().unwrap();
        let message_seq = |id: &str| {
            rows.iter()
                .find(|row| row["id"] == id)
                .unwrap_or_else(|| panic!("missing {id}: {messages}"))["seq"]
                .clone()
        };
        let message_metadata = json!([
            message_seq("visible-message-a"),
            message_seq("visible-message-b")
        ]);
        let after_message = call(
            reader(),
            "message_list",
            json!({"to":"reader", "since":"visible-message-a"}),
        )
        .expect("message continuation");
        assert!(after_message.to_string().contains("visible-message-b"));

        shared
            .store
            .event_public_scoped(
                "reader",
                "fixture_event",
                json!({"marker":"VISIBLE_EVENT_A"}),
                Some("public-job"),
                Some("public-task"),
            )
            .unwrap();
        if add_hidden {
            shared
                .store
                .event_public_scoped(
                    "hidden-worker",
                    "fixture_event",
                    json!({"marker":"HIDDEN_EVENT_SENTINEL"}),
                    Some("hidden-job"),
                    Some("hidden-task"),
                )
                .unwrap();
        }
        shared
            .store
            .event_public_scoped(
                "reader",
                "fixture_event",
                json!({"marker":"VISIBLE_EVENT_B"}),
                Some("public-job"),
                Some("public-task"),
            )
            .unwrap();
        let agent_scope = cadence_core::board_api::AGENT_EVENTS_CURSOR_FORMAT;
        let events = call(
            reader(),
            "agent_events",
            json!({"alias":"reader", "cursor_format":agent_scope}),
        )
        .expect("visible agent events");
        let event_rows = events["events"].as_array().unwrap();
        let event = |marker: &str| {
            event_rows
                .iter()
                .find(|row| row["payload"]["marker"] == marker)
                .unwrap_or_else(|| panic!("missing {marker}: {events}"))
        };
        // CAD-2215: a scoped agent-event row carries no global position; the
        // continuation is the opaque per-row cursor.
        assert!(
            event("VISIBLE_EVENT_A").get("seq").is_none(),
            "a scoped agent-event row exposed a global position: {events}"
        );
        let event_a_cursor = event("VISIBLE_EVENT_A")["cursor"]
            .as_str()
            .filter(|cursor| !cursor.is_empty() && cursor.parse::<i64>().is_err())
            .expect("a scoped agent-event row must carry an opaque cursor")
            .to_string();
        let agent_next = call(
            reader(),
            "agent_events",
            json!({"alias":"reader", "cursor_format":agent_scope, "after":event_a_cursor}),
        )
        .expect("agent event continuation");
        assert!(agent_next.to_string().contains("VISIBLE_EVENT_B"));
        assert_no_global_seq(&events, "scoped agent event page");
        assert_no_global_seq(&agent_next, "scoped agent event continuation");
        assert!(
            !events.to_string().contains("HIDDEN_EVENT_SENTINEL")
                && !agent_next.to_string().contains("HIDDEN_EVENT_SENTINEL"),
            "scoped agent events exposed hidden activity: {events} {agent_next}"
        );
        // Enqueuing the two visible messages also emits legitimate `queued`
        // events without a fixture marker. Compare every visible event's
        // semantic fields, not just the two marker-bearing events.
        let observations = |rows: &[Value]| -> Vec<Value> {
            rows.iter()
                .map(|row| {
                    json!({
                        "kind": row["kind"],
                        "payload": row["payload"],
                        "job_id": row["job_id"],
                        "task_id": row["task_id"],
                    })
                })
                .collect()
        };
        let agent_markers = observations(event_rows);
        let agent_continued = observations(agent_next["events"].as_array().unwrap());
        // CAD-2215: a non-operator `job_events` read is scoped. It must name
        // the opaque format, and it continues from the opaque cursor the page
        // returned — never from a global sequence position.
        let job_page = |after: Option<&str>| {
            let mut params = json!({
                "job": "public-job",
                "cursor_format": cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT,
            });
            if let Some(token) = after {
                params["after"] = json!(token);
            }
            call(reader(), "job_events", params).expect("scoped job event page")
        };
        let markers = |page: &Value| -> Vec<String> {
            page["events"]
                .as_array()
                .unwrap_or_else(|| panic!("scoped job page has no events array: {page}"))
                .iter()
                .map(|row| row["payload"]["marker"].as_str().unwrap().to_string())
                .collect()
        };
        let ids = |page: &Value| -> Vec<String> {
            page["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    row["id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .unwrap_or_else(|| panic!("scoped event has no stable id: {page}"))
                        .to_string()
                })
                .collect()
        };
        let job_events = job_page(None);
        assert_eq!(
            job_events[cadence_core::board_api::READER_SCOPE_FIELD],
            json!(cadence_core::board_api::JOB_EVENTS_SCOPE_ACK),
            "scoped job event page did not name its reader mode: {job_events}"
        );
        assert_no_global_seq(&job_events, "scoped job event page");
        assert!(
            !job_events.to_string().contains("HIDDEN_EVENT_SENTINEL"),
            "scoped job events exposed hidden project activity: {job_events}"
        );
        assert_eq!(markers(&job_events), ["VISIBLE_EVENT_A", "VISIBLE_EVENT_B"]);
        let job_ids = ids(&job_events);
        assert_eq!(job_ids.len(), 2);
        assert_ne!(
            job_ids[0], job_ids[1],
            "two visible events share a stable id: {job_events}"
        );
        assert_eq!(
            ids(&job_page(None)),
            job_ids,
            "re-reading the job event page changed stable event identity"
        );
        let job_cursor = job_events["events"][0]["cursor"]
            .as_str()
            .filter(|cursor| !cursor.is_empty())
            .expect("a scoped event row must carry its forward cursor")
            .to_string();
        assert!(
            job_cursor.parse::<i64>().is_err(),
            "a scoped event cursor is a numeric position: {job_cursor}"
        );
        let job_next = job_page(Some(&job_cursor));
        assert_no_global_seq(&job_next, "scoped job event continuation");
        let job_continued = markers(&job_next);
        assert_eq!(
            job_continued,
            ["VISIBLE_EVENT_B"],
            "the opaque continuation did not resume after event A: {job_next}"
        );

        shared
            .store
            .register_monitor(
                "public-monitor",
                "fixture",
                "reader",
                60,
                &["public-task".to_string()],
                false,
                false,
            )
            .unwrap();
        shared
            .store
            .register_monitor(
                "hidden-monitor",
                "fixture",
                "hidden-worker",
                60,
                &["hidden-task".to_string()],
                false,
                false,
            )
            .unwrap();
        shared
            .store
            .event_public_scoped(
                "reader",
                "turn_stalled",
                json!({"marker":"VISIBLE_ALERT_A"}),
                Some("public-job"),
                Some("public-task"),
            )
            .unwrap();
        shared.store.check_monitor("public-monitor", 10.0).unwrap();
        if add_hidden {
            shared
                .store
                .event_public_scoped(
                    "hidden-worker",
                    "turn_stalled",
                    json!({"marker":"HIDDEN_ALERT_SENTINEL"}),
                    Some("hidden-job"),
                    Some("hidden-task"),
                )
                .unwrap();
            shared.store.check_monitor("hidden-monitor", 11.0).unwrap();
        }
        shared
            .store
            .event_public_scoped(
                "reader",
                "turn_stalled",
                json!({"marker":"VISIBLE_ALERT_B"}),
                Some("public-job"),
                Some("public-task"),
            )
            .unwrap();
        shared.store.check_monitor("public-monitor", 12.0).unwrap();
        let alert_scope = cadence_core::board_api::MONITOR_CURSOR_FORMAT;
        let first = call(
            reader(),
            "monitor_alerts",
            json!({"monitor":"public-monitor", "limit":1, "cursor_format":alert_scope}),
        )
        .expect("first visible alert");
        assert!(first.to_string().contains("VISIBLE_ALERT_A"));
        let first_cursor = first["cursor"]
            .as_str()
            .filter(|cursor| !cursor.is_empty() && cursor.parse::<i64>().is_err())
            .expect("a scoped alert page must carry an opaque cursor")
            .to_string();
        let second = call(
            reader(),
            "monitor_alerts",
            json!({"monitor":"public-monitor", "after":first_cursor, "limit":1, "cursor_format":alert_scope}),
        )
        .expect("second visible alert");
        assert!(second.to_string().contains("VISIBLE_ALERT_B"));
        let first_alert = &first["alerts"][0];
        let second_alert = &second["alerts"][0];
        for field in ["seq", "event_seq", "fingerprint"] {
            assert!(
                first_alert.get(field).is_none(),
                "a scoped alert exposed a global {field}: {first_alert}"
            );
        }
        // Scoped alert metadata is compared by visible behavior only: the
        // cursor and ack tokens are freshly sealed per response and the keyed
        // rows differ per workspace, so neither is a signal here.
        let alert_metadata = json!({
            "reader_scope":first[cadence_core::board_api::READER_SCOPE_FIELD],
            "first":first_alert["payload"]["marker"],
            "second":second_alert["payload"]["marker"],
            "first_state":first_alert["state"],
            "second_state":second_alert["state"],
        });
        let ack = call(
            reader(),
            "monitor_alert_ack",
            json!({"monitor":"public-monitor", "ack":first_alert["ack"].clone()}),
        )
        .expect("visible alert can be acknowledged");
        assert_eq!(ack["alert"]["state"], "acknowledged");
        if add_hidden {
            let operator_mail = call(
                Asserted::Operator,
                "message_list",
                json!({"to":"hidden-worker"}),
            )
            .expect("operator hidden mailbox recovery");
            assert!(operator_mail.to_string().contains("hidden-message"));
            let operator_events = call(
                Asserted::Operator,
                "agent_events",
                json!({"alias":"hidden-worker"}),
            )
            .expect("operator hidden event recovery");
            assert!(operator_events
                .to_string()
                .contains("HIDDEN_EVENT_SENTINEL"));
            let operator_job = call(
                Asserted::Operator,
                "job_events",
                json!({"job":"hidden-job"}),
            )
            .expect("operator hidden job event recovery");
            assert!(operator_job.to_string().contains("HIDDEN_EVENT_SENTINEL"));
            let operator_alerts = call(
                Asserted::Operator,
                "monitor_alerts",
                json!({"monitor":"hidden-monitor"}),
            )
            .expect("operator hidden alert recovery");
            assert!(operator_alerts
                .to_string()
                .contains("HIDDEN_ALERT_SENTINEL"));
        }
        json!({
            "messages":message_metadata,
            "agent_events":{
                "reader_scope":events[cadence_core::board_api::READER_SCOPE_FIELD],
                "markers":agent_markers,
                "continuation":agent_continued,
            },
            // Scoped job-event metadata is compared by visible behavior only:
            // opaque cursors and keyed row ids are freshly sealed per response
            // and per workspace, so their bytes are not a signal.
            "job_events":{
                "reader_scope":job_events[cadence_core::board_api::READER_SCOPE_FIELD],
                "markers":markers(&job_events),
                "continuation":job_continued,
            },
            "alerts":{
                "metadata":alert_metadata,
                "ack_state":ack["alert"]["state"],
            },
        })
    }
    let without_hidden = observe(false);
    let with_hidden = observe(true);
    assert_eq!(
        with_hidden, without_hidden,
        "hidden activity changed scoped message/event/job/alert counter metadata"
    );
}
