//! monitor: area tests split from tests/integration.rs (CAD-426).
//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use cadence_agent::store::Store;
use serde_json::json;
use serde_json::Value;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use tempfile::TempDir;

// ==================== persistent monitors (CAD-176) ====================

#[test]
fn monitor_migration_from_v6_bridges_provider_effort_before_v8_v9_and_v10() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("cadence.sqlite3");
    let store = Store::open(&path).unwrap();
    drop(store);
    // Model a v6 database before either change: CAD-176 must reserve the v7
    // column contract before advancing directly to its v8 tables.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP TABLE monitor_alerts;
         DROP TABLE monitor_tasks;
         DROP TABLE monitors;
         ALTER TABLE agents DROP COLUMN effort;
         UPDATE schema_version SET version=6;",
    )
    .unwrap();
    drop(conn);
    let store = Store::open_for_schema_tests(&path).unwrap();
    assert!(store.monitors().unwrap().is_empty());
    let conn = rusqlite::Connection::open(&path).unwrap();
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(agents)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let version: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, cadence_agent::rollout::SCHEMA_VERSION);
    assert!(columns.iter().any(|column| column == "effort"));
    let monitor_columns: Vec<String> = conn
        .prepare("PRAGMA table_info(monitors)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(monitor_columns
        .iter()
        .any(|column| column == "auto_dispatch_enabled"));
}

#[test]
fn monitor_migration_after_provider_effort_v7_is_v8_v9_and_v10() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("cadence.sqlite3");
    let store = Store::open(&path).unwrap();
    drop(store);
    // Model PR80 first: v7 owns the effort column and CAD-176 owns the next
    // slot. Reopening must add monitors without touching the v7 contract.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP TABLE monitor_alerts;
         DROP TABLE monitor_tasks;
         DROP TABLE monitors;
         UPDATE schema_version SET version=7;",
    )
    .unwrap();
    drop(conn);
    let store = Store::open_for_schema_tests(&path).unwrap();
    assert!(store.monitors().unwrap().is_empty());
    let conn = rusqlite::Connection::open(&path).unwrap();
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(agents)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let version: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, cadence_agent::rollout::SCHEMA_VERSION);
    assert!(columns.iter().any(|column| column == "effort"));
    let monitor_columns: Vec<String> = conn
        .prepare("PRAGMA table_info(monitors)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(monitor_columns
        .iter()
        .any(|column| column == "auto_dispatch_enabled"));
}

#[test]
fn monitor_migration_from_v8_defaults_auto_dispatch_off() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("cadence.sqlite3");
    let store = Store::open(&path).unwrap();
    drop(store);
    // Model a v8 store with an existing manual registration. The new
    // background bit must be added as an explicit opt-in and default off;
    // upgrading an old manual monitor must not start a scheduler.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "INSERT INTO monitors(
             id,project,owner,interval_secs,state,next_check_at,event_cursor,
             delivery_configured,delivery_state,dispatch_enabled,error,created,updated)
         VALUES('legacy','repo','operator',60,'active',NULL,0,0,'unconfigured',1,NULL,0,0)",
        [],
    )
    .unwrap();
    conn.execute_batch(
        "ALTER TABLE monitors DROP COLUMN auto_dispatch_enabled;
         UPDATE schema_version SET version=8;",
    )
    .unwrap();
    drop(conn);

    let store = Store::open_for_schema_tests(&path).unwrap();
    assert!(!store.monitor("legacy").unwrap().auto_dispatch_enabled);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, cadence_agent::rollout::SCHEMA_VERSION);
}

#[test]
fn monitor_migration_repairs_legacy_pr100_schema9_without_quota() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("cadence.sqlite3");
    let store = Store::open(&path).unwrap();
    drop(store);
    // An older PR100 candidate used v9 for the monitor consent column. A
    // provider-quota migration landing afterwards must not skip its agent
    // column merely because the shared version marker already says 9.
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "ALTER TABLE agents DROP COLUMN quota;
         UPDATE schema_version SET version=9;",
    )
    .unwrap();
    drop(conn);

    let store = Store::open_for_schema_tests(&path).unwrap();
    assert!(store.monitors().unwrap().is_empty());
    let conn = rusqlite::Connection::open(&path).unwrap();
    let agent_columns: Vec<String> = conn
        .prepare("PRAGMA table_info(agents)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let monitor_columns: Vec<String> = conn
        .prepare("PRAGMA table_info(monitors)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let version: i64 = conn
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, cadence_agent::rollout::SCHEMA_VERSION);
    assert!(agent_columns.iter().any(|column| column == "quota"));
    assert!(monitor_columns
        .iter()
        .any(|column| column == "auto_dispatch_enabled"));
}

#[test]
fn monitor_check_failure_is_degraded_without_healthy_claim() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("degraded-monitor.md", "check failure");
    let project = d.dir.path().to_str().unwrap().to_string();
    d.job_new_repo("pm", "badjob", &spec, &sha, &project)
        .unwrap();
    d.task_new_ac("badjob", "badjob-watch", "w1", "observe failures")
        .unwrap();
    d.rpc(
        "monitor_register",
        json!({"monitor": "bad", "project": project, "owner": "operator",
               "tasks": ["badjob-watch"], "interval_secs": 1}),
    )
    .unwrap();
    let active = wait_monitor_state(&d, "bad", "active", 5);
    let last_success = active["last_success_at"].as_f64().unwrap();
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    // Invalid event evidence makes the check fail; it must remain visible as
    // degraded instead of being treated as a healthy empty scan.
    conn.execute(
        "INSERT INTO events(alias,kind,payload,job_id,task_id,at)
         VALUES(?,?,?,?,?,?)",
        rusqlite::params!["w1", "turn_finished", "{}", "badjob", "badjob-watch", "bad"],
    )
    .unwrap();
    drop(conn);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let monitor = d.rpc("monitor_show", json!({"monitor": "bad"})).unwrap()["monitor"].clone();
        if monitor["monitoring"] == "degraded" {
            assert!(
                monitor["error"].as_str().is_some_and(|e| !e.is_empty()),
                "{monitor}"
            );
            assert_eq!(
                monitor["last_success_at"].as_f64(),
                Some(last_success),
                "{monitor}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "monitor did not degrade: {monitor}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// Wait for a successful check of `monitor` that began after `after`
/// (epoch seconds): the positive barrier for "the watcher looked and
/// did nothing", instead of sleeping one interval and hoping it ran.
/// `last_success_at` is the time its pass started, so that pass saw
/// every row committed before `after`; passes run one after another,
/// so every earlier pass (dispatch reconcile included) has finished.
fn wait_monitor_check_after(d: &TestDaemon, monitor: &str, after: f64, secs: u64) -> Value {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let value = d.rpc("monitor_show", json!({"monitor": monitor})).unwrap()["monitor"].clone();
        if value["last_success_at"]
            .as_f64()
            .is_some_and(|at| at > after)
        {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "monitor {monitor} ran no check after {after}: {value}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn monitor_persists_coverage_heartbeats_and_deduplicates_alerts() {
    let mut d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("monitor-spec.md", "watch this task");
    let project = d.dir.path().to_str().unwrap().to_string();
    d.job_new_repo("pm", "mjob", &spec, &sha, &project).unwrap();
    d.task_new_ac("mjob", "mjob-watch", "w1", "observe the task")
        .unwrap();
    let registered = d
        .rpc(
            "monitor_register",
            json!({"monitor": "m1", "project": project,
                   "owner": "operator", "tasks": ["mjob-watch"],
                   "interval_secs": 1}),
        )
        .unwrap();
    assert_eq!(
        registered["monitor"]["monitoring"], "degraded",
        "{registered}"
    );
    assert_eq!(registered["monitor"]["delivery"]["configured"], false);
    assert_eq!(registered["monitor"]["coverage"], json!(["mjob-watch"]));
    let active = wait_monitor_state(&d, "m1", "active", 5);
    assert!(active["last_success_at"].is_number(), "{active}");
    assert!(active["heartbeat_at"].is_number(), "{active}");
    assert!(active["next_check_at"].is_number(), "{active}");

    // Receipt-only task events do not manufacture an alert. The monitor
    // still advances its cursor, but no worker health is inferred.
    let store = Store::open(&d.state.join("cadence.sqlite3")).unwrap();
    store
        .event_public_scoped(
            "w1",
            "turn_finished",
            json!({"message": "m-finished"}),
            Some("mjob"),
            Some("mjob-watch"),
        )
        .unwrap();
    wait_monitor_check_after(&d, "m1", epoch_now(), 5);
    let no_alert = d.rpc("monitor_alerts", json!({"monitor": "m1"})).unwrap();
    assert!(
        no_alert["alerts"].as_array().unwrap().is_empty(),
        "{no_alert}"
    );

    // Inject one concrete task-scoped stall observation through the same
    // durable event table the daemon consumes.
    store
        .event_public_scoped(
            "w1",
            "turn_stalled",
            json!({"message": "m-stall", "episode": 1}),
            Some("mjob"),
            Some("mjob-watch"),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let alert = loop {
        let page = d.rpc("monitor_alerts", json!({"monitor": "m1"})).unwrap();
        if let Some(alert) = page["alerts"].as_array().and_then(|a| a.first()) {
            break alert.clone();
        }
        assert!(Instant::now() < deadline, "monitor did not alert: {page}");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(alert["kind"], "turn_stalled", "{alert}");
    assert_eq!(alert["task"], "mjob-watch");
    assert_eq!(alert["state"], "open");
    let cursor = d.rpc("monitor_show", json!({"monitor": "m1"})).unwrap()["monitor"]
        ["event_cursor"]
        .as_i64()
        .unwrap();
    wait_monitor_check_after(&d, "m1", epoch_now(), 5);
    let again = d.rpc("monitor_alerts", json!({"monitor": "m1"})).unwrap();
    assert_eq!(again["alerts"].as_array().unwrap().len(), 1, "{again}");
    assert!(
        again["alerts"][0]["event_seq"].as_i64().unwrap() <= cursor,
        "alert event must be at or behind the durable monitor cursor: {again}"
    );

    let acked = d
        .rpc(
            "monitor_alert_ack",
            json!({"monitor": "m1", "alert": alert["seq"], "by": "operator"}),
        )
        .unwrap();
    assert_eq!(acked["alert"]["state"], "acknowledged", "{acked}");
    let open = d
        .rpc("monitor_alerts", json!({"monitor": "m1", "open": true}))
        .unwrap();
    assert!(open["alerts"].as_array().unwrap().is_empty(), "{open}");

    // The registration and cursor survive a daemon restart; the observed
    // event is not replayed as a second alert.
    let state = d.state.clone();
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let d2 = TestDaemon::start_on(state);
    let restored = wait_monitor_state(&d2, "m1", "active", 5);
    assert_eq!(restored["coverage"], json!(["mjob-watch"]));
    let after_restart = d2.rpc("monitor_alerts", json!({"monitor": "m1"})).unwrap();
    assert_eq!(after_restart["alerts"].as_array().unwrap().len(), 1);
    let _ = d2.operator_rpc("monitor_stop", json!({"monitor": "m1"}));
}

#[test]
fn monitor_alerts_task_unknown_outcome_is_scoped_and_restart_safe() {
    let mut d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("unknown-monitor-spec.md", "inspect uncertain work");
    let project = d.dir.path().to_str().unwrap().to_string();
    d.job_new_repo("pm", "unknown-job", &spec, &sha, &project)
        .unwrap();
    d.task_new_ac(
        "unknown-job",
        "unknown-task",
        "w1",
        "inspect uncertain work",
    )
    .unwrap();
    d.rpc(
        "monitor_register",
        json!({"monitor": "unknown-monitor", "project": project,
               "owner": "operator", "tasks": ["unknown-task"],
               "interval_secs": 1}),
    )
    .unwrap();
    wait_monitor_state(&d, "unknown-monitor", "active", 5);

    // Stop before dispatch. The kickoff stays queued on the stopped worker
    // until its body is the fake provider's DISCONNECT fixture and resume
    // starts the actor. Opening Store here would run crash recovery against
    // the live daemon, so the body rewrite uses a plain connection.
    d.rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    let stopped = d.wait_agent("w1", "stopped", 10);
    assert_eq!(stopped["enabled"], false, "{stopped}");
    assert!(stopped["endpoint"].is_null(), "{stopped}");
    let dispatched = d
        .rpc("task_dispatch", json!({"task": "unknown-task"}))
        .unwrap();
    assert_eq!(dispatched["duplicate"], false, "{dispatched}");
    let kickoff = dispatched["message"].as_str().unwrap().to_string();
    let queued = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["id"] == kickoff)
        .unwrap()
        .clone();
    assert_eq!(queued["state"], "queued", "{queued}");
    assert_eq!(queued["source"], "job_dispatch", "{queued}");
    assert_eq!(queued["task_id"], "unknown-task", "{queued}");
    let still_stopped = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(still_stopped["state"], "stopped", "{still_stopped}");
    assert_eq!(still_stopped["enabled"], false, "{still_stopped}");
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE messages SET body='DISCONNECT' WHERE id=?",
        rusqlite::params![kickoff],
    )
    .unwrap();
    drop(conn);
    let armed = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["id"] == kickoff)
        .unwrap()
        .clone();
    assert_eq!(armed["state"], "queued", "{armed}");
    assert_eq!(armed["body"], "DISCONNECT", "{armed}");
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"]["state"],
        "stopped"
    );
    d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();

    let message = d.wait_message("w1", &kickoff, &["unknown"], 15);
    assert_eq!(message["state"], "unknown", "{message}");
    assert_eq!(message["source"], "job_dispatch", "{message}");
    assert_eq!(
        message["error"], "Connection lost during turn; provider outcome is unknown",
        "{message}"
    );
    assert_eq!(message["result"]["status"], "unknown", "{message}");
    assert!(
        message["turn_id"]
            .as_str()
            .unwrap()
            .starts_with("fake-turn-"),
        "{message}"
    );
    assert_eq!(d.task_state("unknown-task"), "running");
    let task = d.rpc("task_show", json!({"task": "unknown-task"})).unwrap()["task"].clone();
    assert_eq!(task["revision"], 1, "{task}");
    assert!(task["head_sha"].is_null(), "{task}");
    d.wait_agent("w1", "attention", 10);
    d.send("w1", json!({"text": "after", "message": "after-unknown"}))
        .unwrap();
    // CAD-184 kept sleep: absence window — no actor runs for a fenced or
    // stopped agent, so nothing records a refusal to poll for.
    thread::sleep(Duration::from_millis(400));
    assert_eq!(d.message_state("w1", "after-unknown"), "queued");
    assert_eq!(d.message_state("w1", &kickoff), "unknown");

    let deadline = Instant::now() + Duration::from_secs(5);
    let alert = loop {
        let page = d
            .rpc(
                "monitor_alerts",
                json!({"monitor": "unknown-monitor", "open": true}),
            )
            .unwrap();
        if let Some(alert) = page["alerts"].as_array().and_then(|alerts| alerts.first()) {
            break alert.clone();
        }
        assert!(Instant::now() < deadline, "monitor did not alert: {page}");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(alert["kind"], "turn_unknown", "{alert}");
    assert_eq!(alert["task"], "unknown-task", "{alert}");
    assert_eq!(alert["state"], "open", "{alert}");
    let evidence = &alert["payload"]["payload"];
    assert_eq!(evidence["message"], kickoff);
    assert_eq!(
        evidence["reason"],
        "Connection lost during turn; provider outcome is unknown"
    );
    assert_eq!(evidence["owner"], "operator");
    assert!(evidence["next_action"]
        .as_str()
        .unwrap()
        .contains("reconcil"));
    let events = d
        .rpc("job_events", json!({"job": "unknown-job", "tail": true}))
        .unwrap()["events"]
        .as_array()
        .unwrap()
        .clone();
    let unknown_events: Vec<_> = events
        .iter()
        .filter(|event| event["kind"] == "turn_unknown")
        .collect();
    assert_eq!(unknown_events.len(), 1, "{events:?}");
    assert_eq!(unknown_events[0]["job_id"], "unknown-job");
    assert_eq!(unknown_events[0]["task_id"], "unknown-task");
    assert!(
        event_kinds(&d, "w1")
            .iter()
            .any(|kind| kind == "turn_finished"),
        "compatibility turn_finished stays on the agent stream"
    );
    let alert_seq = alert["seq"].clone();
    let fingerprint = alert["fingerprint"].clone();

    // The cursor and event fingerprint make repeated monitor ticks one alert.
    let tick = wait_monitor_check_after(&d, "unknown-monitor", epoch_now(), 5);
    wait_monitor_check_after(
        &d,
        "unknown-monitor",
        tick["last_success_at"].as_f64().unwrap(),
        5,
    );
    let repeated = d
        .rpc(
            "monitor_alerts",
            json!({"monitor": "unknown-monitor", "open": true}),
        )
        .unwrap();
    assert_eq!(
        repeated["alerts"].as_array().unwrap().len(),
        1,
        "{repeated}"
    );
    assert_eq!(repeated["alerts"][0]["seq"], alert_seq, "{repeated}");
    assert_eq!(
        repeated["alerts"][0]["fingerprint"], fingerprint,
        "{repeated}"
    );
    // A same-kind event without task scope is outside this monitor's fixed
    // coverage and must remain unmonitored. A raw insert avoids Store::open,
    // whose recovery would rewrite the live daemon.
    let at = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "INSERT INTO events(alias,kind,payload,job_id,task_id,at)
         VALUES('w1','turn_unknown',?,NULL,NULL,?)",
        rusqlite::params![r#"{"reason":"unscoped"}"#, at],
    )
    .unwrap();
    drop(conn);
    wait_monitor_check_after(&d, "unknown-monitor", epoch_now(), 5);
    let unscoped = d
        .rpc(
            "monitor_alerts",
            json!({"monitor": "unknown-monitor", "open": true}),
        )
        .unwrap();
    assert_eq!(
        unscoped["alerts"].as_array().unwrap().len(),
        1,
        "{unscoped}"
    );
    assert_eq!(
        unscoped["alerts"][0]["fingerprint"], fingerprint,
        "{unscoped}"
    );
    assert_eq!(
        event_kinds(&d, "w1")
            .iter()
            .filter(|kind| kind.as_str() == "turn_unknown")
            .count(),
        2,
        "scoped finish plus the unscoped insert"
    );
    let scoped = d
        .rpc("job_events", json!({"job": "unknown-job", "tail": true}))
        .unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "turn_unknown")
        .count();
    assert_eq!(scoped, 1);
    assert_eq!(d.task_state("unknown-task"), "running");
    assert_eq!(d.message_state("w1", &kickoff), "unknown");

    // Restarting restores the monitor cursor and keeps the same open alert;
    // the unknown attempt remains fenced and the task never reaches review.
    let state = d.state.clone();
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let restarted_at = epoch_now();
    let d2 = TestDaemon::start_on(state);
    wait_monitor_state(&d2, "unknown-monitor", "active", 5);
    wait_monitor_check_after(&d2, "unknown-monitor", restarted_at, 5);
    let restored = d2
        .rpc(
            "monitor_alerts",
            json!({"monitor": "unknown-monitor", "open": true}),
        )
        .unwrap();
    assert_eq!(
        restored["alerts"].as_array().unwrap().len(),
        1,
        "{restored}"
    );
    assert_eq!(
        restored["alerts"][0]["fingerprint"], fingerprint,
        "{restored}"
    );
    assert_eq!(restored["alerts"][0]["seq"], alert_seq, "{restored}");
    assert_eq!(d2.message_state("w1", &kickoff), "unknown");
    assert_eq!(d2.message_state("w1", "after-unknown"), "queued");
    assert_eq!(d2.task_state("unknown-task"), "running");
    let restored_task = d2
        .rpc("task_show", json!({"task": "unknown-task"}))
        .unwrap()["task"]
        .clone();
    assert_eq!(restored_task["revision"], 1, "{restored_task}");
    assert!(restored_task["head_sha"].is_null(), "{restored_task}");
    d2.wait_agent("w1", "attention", 5);
    let _ = d2.operator_rpc("monitor_stop", json!({"monitor": "unknown-monitor"}));
}

#[test]
fn monitor_dispatch_requires_explicit_safe_eligibility() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.register_member("w2", "pm");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    d.wait_agent("w2", "idle", 10);
    let (spec, sha) = d.spec_file("dispatch-monitor.md", "safe dispatch");
    let project = d.dir.path().to_str().unwrap().to_string();
    d.job_new_repo("pm", "djob", &spec, &sha, &project).unwrap();
    d.task_new_ac("djob", "djob-ready", "w1", "run focused checks")
        .unwrap();
    d.task_new_ac("djob", "djob-repeat", "w2", "reuse the live kickoff")
        .unwrap();
    d.rpc(
        "monitor_register",
        json!({"monitor": "dm", "project": project, "owner": "operator",
               "tasks": ["djob-ready", "djob-repeat"], "interval_secs": 1,
               "dispatch_enabled": true}),
    )
    .unwrap();
    wait_monitor_state(&d, "dm", "active", 5);
    // The legacy manual permission remains inert under the background
    // watcher. Automatic reconciliation requires its separate opt-in bit.
    wait_monitor_check_after(&d, "dm", epoch_now(), 5);
    assert_eq!(d.task_state("djob-ready"), "draft");
    let before_manual = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(
        before_manual["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["source"] == "job_dispatch")
            .count(),
        0
    );

    let result = d
        .operator_rpc(
            "monitor_dispatch",
            json!({"monitor": "dm", "task": "djob-ready"}),
        )
        .unwrap();
    assert_eq!(result["duplicate"], false, "{result}");
    assert_eq!(result["task"]["state"], "dispatched", "{result}");
    let kickoff = result["message"].as_str().unwrap().to_string();
    // Seed a second task with an existing queued kickoff while its worker is
    // stopped. The monitor retry must take the duplicate-only branch even
    // though a fresh dispatch would fail the live-worker eligibility gate.
    d.rpc("agent_stop", json!({"alias": "w2"})).unwrap();
    d.wait_agent("w2", "stopped", 10);
    let store = Store::open(&d.state.join("cadence.sqlite3")).unwrap();
    let (_, existing_kickoff, existing_duplicate, _) = store
        .dispatch_task("djob-repeat", None, None, "operator")
        .unwrap();
    assert!(!existing_duplicate);
    let duplicate = d
        .operator_rpc(
            "monitor_dispatch",
            json!({"monitor": "dm", "task": "djob-repeat"}),
        )
        .unwrap();
    assert_eq!(duplicate["duplicate"], true, "{duplicate}");
    assert_eq!(duplicate["message"], existing_kickoff);
    let messages = d.rpc("agent_show", json!({"alias": "w2"})).unwrap();
    assert_eq!(
        messages["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["source"] == "job_dispatch")
            .count(),
        1,
        "{messages}"
    );
    assert_ne!(kickoff, existing_kickoff);
}
