//! CAD-2215: monitor coverage, alert-read, and acknowledgement boundary.
//! The test seam asserts caller identities; it does not establish OS isolation.

use super::*;
use crate::test_seam::{scoped, Asserted};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

fn private_dir(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn write_private(path: &Path, bytes: &[u8]) {
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

#[test]
fn monitor_alerts_require_complete_readable_coverage_and_operator_can_recover() {
    let temp = tempfile::tempdir().unwrap();
    private_dir(temp.path());
    let pm_dir = temp.path().join("tracker");
    crate::issue::Pm::init(&pm_dir).unwrap();
    private_dir(&pm_dir);
    // Owner-provisioned key material for the scoped alert contract. The key
    // ring record is bound by the real operator initialization RPC below.
    let key_dir = tempfile::tempdir().unwrap();
    private_dir(key_dir.path());
    for (name, byte) in [("identity.key", 0x91u8), ("cursor.key", 0xa2u8)] {
        write_private(&key_dir.path().join(name), &[byte; 32]);
    }
    let key_id = "01".repeat(16);
    write_private(
        &temp.path().join("scoped-cursors.json"),
        &serde_json::to_vec(&json!({
            "version": 1,
            "identity_key_file": key_dir.path().join("identity.key").to_str().unwrap(),
            "active_key_id": key_id.clone(),
            "keys": [{
                "id": key_id,
                "key_file": key_dir.path().join("cursor.key").to_str().unwrap(),
            }],
        }))
        .unwrap(),
    );
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
        std::fs::write(
            issue.join("issue.md"),
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
    let conn = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,repo,state,created,updated)
         VALUES ('open-job','Open','spec.md','reader','OPEN-1','fixture','open',1,1),
                ('restricted-job','Restricted','spec.md','reader','REST-1','fixture','open',2,2),
                ('unbound-job','Unbound','spec.md','reader',NULL,'fixture','open',3,3),
                ('missing-issue-job','Missing','spec.md','reader','GONE-1','fixture','open',4,4);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
         VALUES ('open-task','open-job','Open','reader','running',1,1),
                ('restricted-task','restricted-job','Restricted','reader','running',2,2),
                ('unbound-task','unbound-job','Unbound','reader','running',3,3),
                ('missing-issue-task','missing-issue-job','Missing','reader','running',4,4);",
    )
    .unwrap();

    for (monitor, coverage) in [
        ("open-monitor", vec!["open-task"]),
        ("restricted-monitor", vec!["restricted-task"]),
        ("unbound-monitor", vec!["unbound-task"]),
        ("mixed-monitor", vec!["open-task", "restricted-task"]),
        ("missing-issue-monitor", vec!["missing-issue-task"]),
    ] {
        let tasks = coverage.into_iter().map(str::to_string).collect::<Vec<_>>();
        shared
            .store
            .register_monitor(monitor, "fixture", "reader", 60, &tasks, false, false)
            .unwrap();
    }
    conn.execute_batch(
        "INSERT INTO monitor_alerts(seq,monitor_id,task_id,event_seq,fingerprint,kind,payload,state,created,updated)
         VALUES (101,'open-monitor','open-task',1,'open','test','{\"marker\":\"open-alert\"}','open',1,1),
                (102,'restricted-monitor','restricted-task',2,'restricted','test','{\"marker\":\"restricted-alert\"}','open',1,1),
                (103,'unbound-monitor','unbound-task',3,'unbound','test','{\"marker\":\"unbound-alert\"}','open',1,1),
                (104,'mixed-monitor','open-task',4,'mixed','test','{\"marker\":\"mixed-alert\"}','open',1,1),
                (105,'missing-issue-monitor','missing-issue-task',5,'missing','test','{\"marker\":\"missing-issue-alert\"}','open',1,1);",
    )
    .unwrap();

    let pid = std::process::id();
    let reader = || Asserted::Agent("reader".to_string());
    scoped(reader(), || {
        assert!(
            matches!(shared.agent_caller(pid, "acceptance identity").unwrap(),
            AgentCaller::Agent(ref alias) if alias == "reader")
        );
    });
    scoped(Asserted::Operator, || {
        assert!(matches!(
            shared.agent_caller(pid, "acceptance identity").unwrap(),
            AgentCaller::Operator
        ));
    });
    let call = |who: Asserted, method: &str, params: Value| {
        scoped(who, || shared.dispatch(method, &params, pid))
    };
    let initialized = scoped(Asserted::Operator, || {
        shared.dispatch("scoped_cursor_initialize", &json!({}), pid)
    })
    .expect("the operator must initialize the keyring");
    assert_eq!(initialized["initialized"], json!(true), "{initialized}");
    let alert_format = cadence_core::board_api::MONITOR_CURSOR_FORMAT;

    let listing = call(reader(), "monitor_list", json!({})).unwrap();
    let ids = listing["monitors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["open-monitor"]);
    let public = call(
        reader(),
        "monitor_alerts",
        json!({"monitor": "open-monitor", "cursor_format": alert_format}),
    )
    .unwrap();
    assert_eq!(public["alerts"].as_array().unwrap().len(), 1);
    assert_eq!(public["alerts"][0]["payload"]["marker"], "open-alert");
    // The scoped acknowledgement is the alert's own encrypted token, never a
    // numeric position.
    let open_ack = public["alerts"][0]["ack"]
        .as_str()
        .filter(|ack| !ack.is_empty() && ack.parse::<i64>().is_err())
        .expect("a scoped alert must carry its encrypted ack token")
        .to_string();

    // A hidden monitor must be indistinguishable from a nonexistent one.
    // Compare the error class and entire diagnostic after replacing only
    // the requested monitor ID; this permits an echo of the input ID.
    for method in ["monitor_show", "monitor_alerts", "monitor_alert_ack"] {
        let params = |id: &str| match method {
            "monitor_alert_ack" => json!({"monitor": id, "ack": open_ack.clone()}),
            "monitor_alerts" => json!({"monitor": id, "cursor_format": alert_format}),
            _ => json!({"monitor": id}),
        };
        let missing = call(reader(), method, params("absent-monitor"))
            .expect_err("missing monitor must be refused");
        let missing_text = match missing {
            Error::Rejected(text) => text,
            other => panic!("{method}: missing monitor had a non-rejection error: {other}"),
        };
        assert!(
            missing_text.contains("absent-monitor"),
            "{method}: {missing_text}"
        );
        let expected = missing_text.replace("absent-monitor", "<requested>");
        for id in [
            "restricted-monitor",
            "unbound-monitor",
            "mixed-monitor",
            "missing-issue-monitor",
        ] {
            let err = call(reader(), method, params(id))
                .expect_err("inaccessible monitor must be refused");
            let text = match err {
                Error::Rejected(text) => text,
                other => panic!("{method} {id}: non-rejection error: {other}"),
            };
            assert_eq!(text.replace(id, "<requested>"), expected, "{method} {id}");
        }
    }

    // Use each hidden monitor's actual alert, not merely an unrelated seq.
    // Verify denied writes leave the persisted alert open.
    for (id, seq) in [
        ("restricted-monitor", 102),
        ("unbound-monitor", 103),
        ("mixed-monitor", 104),
        ("missing-issue-monitor", 105),
    ] {
        assert!(
            call(
                reader(),
                "monitor_alert_ack",
                json!({"monitor": id, "ack": open_ack.clone()})
            )
            .is_err(),
            "reader acknowledged {id}"
        );
        let state: String = conn
            .query_row(
                "SELECT state FROM monitor_alerts WHERE seq=?1",
                [seq],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "open", "denied acknowledgement changed {id}");
    }
    let ack = call(
        reader(),
        "monitor_alert_ack",
        json!({"monitor": "open-monitor", "ack": open_ack}),
    )
    .expect("reader can acknowledge an alert with fully readable coverage");
    assert_eq!(ack["alert"]["state"], "acknowledged");

    for (id, seq, marker) in [
        ("restricted-monitor", 102, "restricted-alert"),
        ("unbound-monitor", 103, "unbound-alert"),
        ("mixed-monitor", 104, "mixed-alert"),
        ("missing-issue-monitor", 105, "missing-issue-alert"),
    ] {
        let alerts = call(Asserted::Operator, "monitor_alerts", json!({"monitor": id}))
            .expect("operator recovery read must succeed");
        assert_eq!(alerts["alerts"][0]["payload"]["marker"], marker);
        let ack = call(
            Asserted::Operator,
            "monitor_alert_ack",
            json!({"monitor": id, "alert": seq}),
        )
        .expect("operator recovery acknowledgement must succeed");
        assert_eq!(ack["alert"]["state"], "acknowledged");
    }
}
