//! backup_rollout: area tests split from tests/integration.rs (CAD-426).
//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use cadence_agent::daemon;
use cadence_agent::store::NewAgent;
use cadence_agent::store::Store;
use cadence_agent::store::Take;
use serde_json::json;
use serde_json::Value;
use std::sync::Arc;
use std::sync::Barrier;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use tempfile::TempDir;

#[test]
fn daemon_restart_skips_fenced_and_relaunches_healthy() {
    // Seed: one agent mid-flight (crash → unknown fence) + one healthy.
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    {
        let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
        let cwd = state.to_str().unwrap().to_string();
        for alias in ["fenced", "healthy"] {
            store
                .register_agent(&NewAgent {
                    alias,
                    provider: "fake",
                    endpoint_kind: "fake",
                    role: "worker",
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: None,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        store.set_agent_state("fenced", "idle", None).unwrap();
        store.set_agent_state("healthy", "idle", None).unwrap();
        store.enqueue("fenced", "work", None, "m1", "user").unwrap();
        match store.take_queued("fenced").unwrap() {
            Take::Message(m) => assert_eq!(m.id, "m1"),
            _ => panic!("expected a message"),
        }
        // Store dropped mid-flight — the crash this daemon recovers.
    }
    let d = TestDaemon::start_on(state);
    // Healthy relaunched; the fenced one was skipped, still attention.
    d.wait_agent("healthy", "idle", 15);
    let agent = d.wait_agent("fenced", "attention", 15);
    assert!(agent["endpoint"].is_null());
    // relaunch_skipped was emitted — no actor was spawned for it, so a
    // queued task is never taken.
    let kinds = event_kinds(&d, "fenced");
    assert!(
        kinds.iter().any(|k| k == "relaunch_skipped"),
        "events: {kinds:?}"
    );
    d.rpc(
        "agent_send",
        json!({"alias": "fenced", "text": "later", "message": "m2"}),
    )
    .unwrap();
    // CAD-184 kept sleep: absence window — no actor runs for a fenced or
    // stopped agent, so nothing records a refusal to poll for.
    thread::sleep(Duration::from_secs(1));
    assert_eq!(d.message_state("fenced", "m2"), "queued");
    // Unfence + resume still recovers it through the normal path.
    d.operator_rpc(
        "agent_unfence",
        json!({"alias": "fenced", "status": "interrupted"}),
    )
    .unwrap();
    d.rpc("agent_resume", json!({"alias": "fenced"})).unwrap();
    d.wait_agent("fenced", "idle", 15);
    d.wait_message("fenced", "m2", &["completed"], 15);
}

#[test]
fn daemon_restart_reports_fenced_turn() {
    // The failure half of the TURN column: a turn whose pane did not
    // survive prints `fenced` and the command exits non-zero. The
    // pane dies while the daemon still runs — unnoticed before the
    // stop — so the marker records it and the new daemon's pane proof
    // refuses it.
    let d = TestDaemon::start();
    let _reaper = DaemonReaper::new(&d.state);
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    let _token = pty_token(&d, "dv1", "m1");
    let pane_pid: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe { libc::killpg(pane_pid, libc::SIGKILL) };
    let home = TempDir::new().unwrap();
    hold_rollout_lease(home.path(), &d.state);
    let out = operator_cadence_at(
        home.path(),
        &d.state,
        &["daemon", "restart", "--as", "operator:test"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a fenced turn must fail the restart: {stdout} {stderr}"
    );
    assert!(stdout.contains("fenced"), "{stdout} {stderr}");
    let stop = operator_cadence_at(home.path(), &d.state, &["daemon", "stop"]);
    assert!(stop.status.success());
}

/// `daemon restart`'s table reports what happened to an in-flight
/// turn: `kept` when the hot restart adopted it.
#[test]
fn daemon_restart_reports_kept_turn() {
    let d = TestDaemon::start();
    let _reaper = DaemonReaper::new(&d.state);
    let _mock = d.mock_devin();
    d.register_devin("dv", None);
    d.wait_agent("dv", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "dv"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "task", "message": "m1"}),
    )
    .unwrap();
    let token = pty_token(&d, "dv", "m1");
    let home = TempDir::new().unwrap();
    hold_rollout_lease(home.path(), &d.state);
    let out = operator_cadence_at(
        home.path(),
        &d.state,
        &["daemon", "restart", "--as", "operator:test"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "restart failed: {stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("TURN"), "{stdout}");
    assert!(stdout.contains("kept"), "{stdout}");
    // The restarted daemon adopted the turn — its token completes.
    assert_eq!(d.message_state("dv", "m1"), "running");
    d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("dv", "m1", &["completed"], 15);
    let stop = operator_cadence_at(home.path(), &d.state, &["daemon", "stop"]);
    assert!(stop.status.success());
}

#[test]
fn daemon_restarts_after_owner_exit() {
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    {
        let d = TestDaemon::start_on(state.clone());
        d.rpc("health", json!({})).unwrap();
        // Drop shuts the daemon down and releases the singleton lock.
    }
    let d2 = TestDaemon::start_on(state);
    d2.rpc("health", json!({})).unwrap();
}

/// Why a restarted daemon fenced `alias`: its recorded error and the
/// newest events, read from the child daemon that now owns the state.
fn restart_diag(d: &TestDaemon, alias: &str) -> String {
    let error = d
        .rpc("agent_show", json!({"alias": alias}))
        .map(|v| v["agent"]["error"].clone())
        .unwrap_or_default();
    let events = d
        .rpc("events", json!({"alias": alias, "after": 0}))
        .map(|v| v["events"].clone())
        .unwrap_or_default();
    let tail: Vec<&Value> = events
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .take(6)
        .collect();
    format!("{alias} error: {error}\nnewest events: {tail:?}")
}

/// `daemon restart` through the real binary: a thread-daemon seeded
/// with a pty pane hands the lock to a fresh detached daemon, which
/// re-adopts the pane — the table must say the pid is the same.
#[test]
fn daemon_restart_keeps_pane_pid_and_reports_table() {
    let d = TestDaemon::start();
    let _reaper = DaemonReaper::new(&d.state);
    let _mock = d.mock_devin();
    d.register_devin("dv", None);
    d.register("w1");
    d.wait_agent("dv", "idle", 20);
    d.wait_agent("w1", "idle", 15);
    let pane_pid_before = d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"]["pid"]
        .as_u64()
        .unwrap();
    assert!(pane_pid_before > 0);
    let home = TempDir::new().unwrap();
    hold_rollout_lease(home.path(), &d.state);
    let out = operator_cadence_at(
        home.path(),
        &d.state,
        &["daemon", "restart", "--as", "operator:test"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "restart failed: {stdout} {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        restart_diag(&d, "dv")
    );
    assert!(stdout.contains("AGENT"), "{stdout}");
    assert!(stdout.contains("dv"), "{stdout}");
    assert!(stdout.contains("same"), "{stdout}");
    // The new daemon owns the state and reports the same pane pid.
    let probe = d.rpc("agent_probe", json!({"alias": "dv"})).unwrap();
    assert_eq!(probe["idle"], true, "{probe}");
    let pane_pid_after = d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"]["pid"]
        .as_u64()
        .unwrap();
    assert_eq!(pane_pid_before, pane_pid_after);
    let stop = operator_cadence_at(home.path(), &d.state, &["daemon", "stop"]);
    assert!(
        stop.status.success(),
        "stop after restart failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
}

/// --when-idle refuses to touch a busy fleet, then proceeds once the
/// pane clears.
#[test]
fn daemon_restart_when_idle_gates_and_proceeds() {
    let d = TestDaemon::start();
    let _reaper = DaemonReaper::new(&d.state);
    let mock = d.mock_devin();
    d.register_devin("dv", None);
    d.wait_agent("dv", "idle", 20);
    std::fs::write(
        d.pane_file(&mock, "dv", "tui-state"),
        "⠸ Thinking · 12s (esc twice to interrupt)\n❭ Guide Devin while it works\n",
    )
    .unwrap();
    let home = TempDir::new().unwrap();
    hold_rollout_lease(home.path(), &d.state);
    // Busy pane → timeout exits non-zero and the daemon is untouched.
    let out = operator_cadence_at(
        home.path(),
        &d.state,
        &[
            "daemon",
            "restart",
            "--when-idle",
            "--timeout",
            "3",
            "--as",
            "operator:test",
        ],
    );
    assert!(
        !out.status.success(),
        "when-idle restart ran on a busy pane: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        d.rpc("agent_probe", json!({"alias": "dv"})).is_ok(),
        "daemon must be untouched after a when-idle timeout"
    );
    // Pane goes idle — the same command now completes the restart.
    std::fs::remove_file(d.pane_file(&mock, "dv", "tui-state")).unwrap();
    let out = operator_cadence_at(
        home.path(),
        &d.state,
        &[
            "daemon",
            "restart",
            "--when-idle",
            "--timeout",
            "30",
            "--as",
            "operator:test",
        ],
    );
    assert!(
        out.status.success(),
        "when-idle restart failed on an idle pane: {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        restart_diag(&d, "dv")
    );
    let stop = operator_cadence_at(home.path(), &d.state, &["daemon", "stop"]);
    assert!(stop.status.success());
}

/// CAD-424: an interrupted restore's leftovers refuse `daemon restart`
/// before anything is shut down, with the recovery `mv` on stderr, and
/// the running daemon keeps serving. On a host an older binary started
/// that daemon next to the leftovers; here they appear after its start.
#[test]
fn cad424_daemon_restart_refuses_over_restore_leftovers_before_shutdown() {
    let d = TestDaemon::start();
    let _reaper = DaemonReaper::new(&d.state);
    let home = TempDir::new().unwrap();
    hold_rollout_lease(home.path(), &d.state);
    let aside = d
        .state
        .join("cadence.sqlite3.replaced-20260924T000000Z-deadbeef");
    std::fs::write(&aside, b"previous store").unwrap();

    let out = cadence_at(
        home.path(),
        &d.state,
        &["daemon", "restart", "--as", "operator:test"],
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "restart ran over restore leftovers: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(stderr.contains("refused before shutdown"), "{stderr}");
    assert!(
        stderr.contains(&format!("mv '{}'", aside.display())),
        "the recovery commands must be on stderr: {stderr}"
    );
    assert!(
        !d.handle.as_ref().unwrap().is_finished(),
        "the running daemon must not have been shut down"
    );
    assert!(
        d.rpc("health", json!({})).is_ok(),
        "daemon stopped answering"
    );
    assert!(aside.exists());
    std::fs::remove_file(&aside).unwrap();
}

/// `daemon stop` waits for the process to release the state-dir lock,
/// so `stop && start` no longer races the drain. Ten iterations — the
/// old code lost this race whenever the drain outlived a millisecond.
#[test]
fn daemon_stop_then_start_never_races_lock() {
    let home = TempDir::new().unwrap();
    // Bound, not a temporary: the parent must outlive the daemon.
    let state_root = TempDir::new().unwrap();
    let state = state_root.path().join("state");
    let _reaper = DaemonReaper::new(&state);
    let start = cadence_at(home.path(), &state, &["daemon", "start"]);
    assert!(
        start.status.success(),
        "initial start: {}",
        String::from_utf8_lossy(&start.stderr)
    );
    for i in 0..10 {
        let stop = cadence_at(home.path(), &state, &["daemon", "stop"]);
        assert!(
            stop.status.success(),
            "stop #{i}: {}",
            String::from_utf8_lossy(&stop.stderr)
        );
        let start = cadence_at(home.path(), &state, &["daemon", "start"]);
        assert!(
            start.status.success(),
            "start #{i} raced the drain: {}",
            String::from_utf8_lossy(&start.stderr)
        );
    }
    let stop = cadence_at(home.path(), &state, &["daemon", "stop"]);
    assert!(stop.status.success());
}

/// Restart without a lease must fail before shutdown. The daemon pid
/// from `daemon start` is still alive afterwards.
#[test]
fn daemon_restart_without_a_lease_leaves_the_pid_unchanged() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let _reaper = DaemonReaper::new(state.path());
    let start = cadence_at(home.path(), state.path(), &["daemon", "start"]);
    assert!(
        start.status.success(),
        "start: {}",
        String::from_utf8_lossy(&start.stderr)
    );
    let v: Value = serde_json::from_slice(&start.stdout).unwrap();
    let pid = v["pid"].as_u64().unwrap();
    let restart = cadence_at(home.path(), state.path(), &["daemon", "restart"]);
    let err = format!(
        "{} {}",
        String::from_utf8_lossy(&restart.stdout),
        String::from_utf8_lossy(&restart.stderr)
    );
    assert!(!restart.status.success(), "{err}");
    assert!(
        err.contains("before shutdown") && err.contains("rollout"),
        "{err}"
    );
    assert!(
        std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "daemon pid {pid} exited"
    );
    let stop = cadence_at(home.path(), state.path(), &["daemon", "stop"]);
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
}

/// A different build cannot start without the lease. The same build can.
#[test]
fn daemon_start_refuses_a_different_build_without_a_lease() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let _reaper = DaemonReaper::new(state.path());
    let start = cadence_at(home.path(), state.path(), &["daemon", "start"]);
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    let stop = cadence_at(home.path(), state.path(), &["daemon", "stop"]);
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let db = state.path().join("cadence.sqlite3");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE daemon_build SET commit_sha='deadbeefdead' WHERE id=1",
        [],
    )
    .unwrap();
    drop(conn);
    let before = sqlite_family(state.path());
    let refused = cadence_at(
        home.path(),
        state.path(),
        &["daemon", "start", "--as", "operator:test"],
    );
    let err = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "{err}");
    assert!(err.contains("deadbeefdead"), "{err}");
    assert_eq!(
        sqlite_family(state.path()),
        before,
        "a refused start must not rewrite the database or its sidecars"
    );
    hold_rollout_lease(home.path(), state.path());
    let start = cadence_at(
        home.path(),
        state.path(),
        &["daemon", "start", "--as", "operator:test"],
    );
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    let stop = cadence_at(home.path(), state.path(), &["daemon", "stop"]);
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
}

/// `--when-idle` queued by the holder must not shut down if the lease
/// is released while the fleet is still busy.
#[test]
fn restart_when_idle_aborts_when_the_lease_is_released() {
    let d = TestDaemon::start();
    let _reaper = DaemonReaper::new(&d.state);
    let mock = d.mock_devin();
    d.register_devin("dv", None);
    d.wait_agent("dv", "idle", 20);
    let busy = d.pane_file(&mock, "dv", "tui-state");
    std::fs::write(
        &busy,
        "⠸ Thinking · 12s (esc twice to interrupt)\n❭ Guide Devin while it works\n",
    )
    .unwrap();
    let probe = d.rpc("agent_probe", json!({"alias": "dv"})).unwrap();
    assert_eq!(
        probe["idle"], false,
        "pane must be busy before the wait: {probe}"
    );
    let home = TempDir::new().unwrap();
    hold_rollout_lease(home.path(), &d.state);
    let started = d.rpc("daemon_info", json!({})).unwrap()["started_at"].clone();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args([
            "daemon",
            "restart",
            "--when-idle",
            "--timeout",
            "20",
            "--as",
            "operator:test",
        ])
        .env("HOME", home.path())
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_ROLLOUT_AS")
        .envs(test_env().vars())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(800));
    assert!(
        child.try_wait().unwrap().is_none(),
        "restart finished before the lease was released"
    );
    let release = cadence_at(
        home.path(),
        &d.state,
        &["rollout", "release", "--as", "operator:test"],
    );
    assert!(
        release.status.success(),
        "release: {}",
        String::from_utf8_lossy(&release.stderr)
    );
    std::fs::remove_file(&busy).unwrap();
    let out = child.wait_with_output().unwrap();
    let err = format!(
        "{} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("released"), "{err}");
    assert!(err.contains("before shutdown"), "{err}");
    let info = d.rpc("daemon_info", json!({})).unwrap();
    assert_eq!(info["started_at"], started, "daemon was restarted: {info}");
}

fn sqlite_family(state: &std::path::Path) -> Vec<(String, Option<Vec<u8>>)> {
    let db = state.join("cadence.sqlite3");
    ["", "-wal", "-shm"]
        .into_iter()
        .map(|suffix| {
            let path = std::path::PathBuf::from(format!("{}{suffix}", db.display()));
            let bytes = std::fs::read(&path).ok();
            (suffix.to_string(), bytes)
        })
        .collect()
}

/// A refused schema crossing on the real `daemon start` path must not
/// rewrite the database or create `-wal`/`-shm`.
#[test]
fn daemon_start_refuses_a_lower_schema_without_rewriting_the_file() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let _reaper = DaemonReaper::new(state.path());
    let start = cadence_at(home.path(), state.path(), &["daemon", "start"]);
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    let stop = cadence_at(home.path(), state.path(), &["daemon", "stop"]);
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let db = state.path().join("cadence.sqlite3");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "DROP TABLE IF EXISTS rollout_leases;
         DROP TABLE IF EXISTS daemon_build;
         UPDATE schema_version SET version=11;
         PRAGMA wal_checkpoint(TRUNCATE);",
    )
    .unwrap();
    drop(conn);
    for suffix in ["-wal", "-shm"] {
        let path = std::path::PathBuf::from(format!("{}{suffix}", db.display()));
        let _ = std::fs::remove_file(path);
    }
    let before = sqlite_family(state.path());
    let refused = cadence_at(home.path(), state.path(), &["daemon", "start"]);
    assert!(
        !refused.status.success(),
        "{}",
        String::from_utf8_lossy(&refused.stdout)
    );
    let log = std::fs::read_to_string(state.path().join("daemon.log")).unwrap_or_default();
    let err = format!("{} {}", String::from_utf8_lossy(&refused.stderr), log);
    assert!(
        err.contains("refusing to migrate") || err.contains("rollout"),
        "{err}"
    );
    assert_eq!(sqlite_family(state.path()), before);
    let version: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 11);
}

/// A receipt is not enough: the migrating process has to be the holder.
#[test]
fn daemon_start_by_a_non_holder_does_not_migrate() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let _reaper = DaemonReaper::new(state.path());
    let start = cadence_at(home.path(), state.path(), &["daemon", "start"]);
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    let stop = cadence_at(home.path(), state.path(), &["daemon", "stop"]);
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let db = state.path().join("cadence.sqlite3");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "DROP TABLE IF EXISTS rollout_leases;
         DROP TABLE IF EXISTS daemon_build;
         UPDATE schema_version SET version=11;
         PRAGMA wal_checkpoint(TRUNCATE);",
    )
    .unwrap();
    drop(conn);
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(std::path::PathBuf::from(format!(
            "{}{suffix}",
            db.display()
        )));
    }
    let claim = cadence_at(
        home.path(),
        state.path(),
        &[
            "rollout",
            "claim",
            "--reason",
            "crossing",
            "--as",
            "operator:test",
            "--ttl",
            "2h",
        ],
    );
    assert!(
        claim.status.success(),
        "{}",
        String::from_utf8_lossy(&claim.stderr)
    );
    let backup = home.path().join("backup.sqlite3");
    std::fs::copy(&db, &backup).unwrap();
    let recorded = cadence_at(
        home.path(),
        state.path(),
        &[
            "rollout",
            "backup",
            "--path",
            backup.to_str().unwrap(),
            "--as",
            "operator:test",
        ],
    );
    assert!(
        recorded.status.success(),
        "{}",
        String::from_utf8_lossy(&recorded.stderr)
    );
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    drop(conn);
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(std::path::PathBuf::from(format!(
            "{}{suffix}",
            db.display()
        )));
    }
    let before = sqlite_family(state.path());
    let refused = cadence_at(home.path(), state.path(), &["daemon", "start"]);
    assert!(!refused.status.success());
    assert_eq!(sqlite_family(state.path()), before);
    let version: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 11);
}

#[test]
fn daemon_start_drops_the_forwarded_rollout_identity() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let _reaper = DaemonReaper::new(state.path());
    let start = cadence_at(
        home.path(),
        state.path(),
        &["daemon", "start", "--as", "operator:test"],
    );
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    let v: Value = serde_json::from_slice(&start.stdout).unwrap();
    let pid = v["pid"].as_u64().unwrap();
    let env = std::fs::read(format!("/proc/{pid}/environ")).unwrap();
    let leaked = env
        .split(|byte| *byte == 0)
        .any(|entry| entry.starts_with(b"CADENCE_ROLLOUT_AS="));
    let stop = cadence_at(home.path(), state.path(), &["daemon", "stop"]);
    assert!(!leaked, "daemon environ still contains CADENCE_ROLLOUT_AS");
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
}

/// CAD-302: `started` means the process this call spawned holds the
/// singleton lock (health reports its pid); a second start against the
/// live daemon reports `already_running` and leaves no second daemon.
#[test]
fn daemon_start_reports_started_for_its_child_and_already_running_after() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let _reaper = DaemonReaper::new(state.path());
    let start = cadence_at(home.path(), state.path(), &["daemon", "start"]);
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    let v: Value = serde_json::from_slice(&start.stdout).unwrap();
    assert_eq!(v["state"], "started", "{v}");
    let pid = v["pid"].as_u64().unwrap();
    assert_eq!(v["health"]["pid"].as_u64(), Some(pid), "{v}");
    assert_eq!(daemon_run_pids(state.path()), vec![pid]);

    let again = cadence_at(home.path(), state.path(), &["daemon", "start"]);
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    let v: Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(v["state"], "already_running", "{v}");
    assert!(v["pid"].is_null(), "{v}");
    assert_eq!(v["health"]["pid"].as_u64(), Some(pid), "{v}");
    assert_eq!(daemon_run_pids(state.path()), vec![pid]);

    let stop = cadence_at(home.path(), state.path(), &["daemon", "stop"]);
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
}

/// CAD-302: two starts racing on an empty state dir both spawn a child;
/// exactly one child takes the lock. Its starter reports `started`, the
/// other waits for its own child to lose the lock and reports
/// `already_running` — never two `started`.
#[test]
fn concurrent_daemon_starts_report_one_started_one_already_running() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let _reaper = DaemonReaper::new(state.path());
    let barrier = Arc::new(Barrier::new(2));
    let starts: Vec<_> = (0..2)
        .map(|_| {
            let barrier = barrier.clone();
            let home = home.path().to_path_buf();
            let state = state.path().to_path_buf();
            thread::spawn(move || {
                barrier.wait();
                cadence_at(&home, &state, &["daemon", "start"])
            })
        })
        .collect();
    let mut results: Vec<Value> = starts
        .into_iter()
        .map(|t| {
            let out = t.join().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            serde_json::from_slice(&out.stdout).unwrap()
        })
        .collect();
    results.sort_by_key(|v| v["state"].as_str().unwrap_or_default().to_string());
    let states: Vec<&str> = results
        .iter()
        .map(|v| v["state"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(states, ["already_running", "started"], "{results:?}");
    let pid = results[1]["pid"].as_u64().unwrap();
    for v in &results {
        assert_eq!(v["health"]["pid"].as_u64(), Some(pid), "{v}");
    }
    assert_eq!(daemon_run_pids(state.path()), vec![pid]);
    let stop = cadence_at(home.path(), state.path(), &["daemon", "stop"]);
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
}

/// CAD-321: a start whose pre-spawn `health` check failed while a
/// daemon is live (under load it can time out) spawns a child anyway.
/// It reports `already_running` for the live daemon and returns only
/// once its own child is gone — the live daemon stays the only one.
#[test]
fn daemon_start_whose_precheck_failed_leaves_only_the_live_daemon() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let _reaper = DaemonReaper::new(state.path());
    let start = cadence_at(home.path(), state.path(), &["daemon", "start"]);
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    let v: Value = serde_json::from_slice(&start.stdout).unwrap();
    assert_eq!(v["state"], "started", "{v}");
    let pid = v["pid"].as_u64().unwrap();

    let again = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(state.path())
        .args(["daemon", "start"])
        .env("HOME", home.path())
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_ROLLOUT_AS")
        .envs(test_env().vars())
        .env("CADENCE_TEST_START_PRECHECK_FAILS", "1")
        .output()
        .unwrap();
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    let v: Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(v["state"], "already_running", "{v}");
    assert_eq!(v["health"]["pid"].as_u64(), Some(pid), "{v}");
    assert_eq!(daemon_run_pids(state.path()), vec![pid]);

    let stop = cadence_at(home.path(), state.path(), &["daemon", "stop"]);
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
}

#[test]
fn restart_and_rollout_help_say_same_build_restart_is_lease_free() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    for args in [
        &["daemon", "restart", "--help"][..],
        &["rollout", "--help"][..],
        &["daemon", "stop", "--help"][..],
    ] {
        let out = cadence_at(home.path(), state.path(), args);
        let text = format!(
            "{} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{args:?}\n{text}");
        assert!(
            text.contains("lease-free"),
            "help for {args:?} should say same-build restart is lease-free:\n{text}"
        );
    }
}

/// A hand-run `daemon run` of a different build by a non-holder must
/// refuse before `hot_restart_begin` deletes the shutdown marker and
/// before `recover` writes. The marker and the sqlite family stay.
#[test]
fn direct_daemon_run_by_a_non_holder_keeps_the_hot_restart_marker() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let _reaper = DaemonReaper::new(state.path());
    let start = cadence_at(home.path(), state.path(), &["daemon", "start"]);
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    let stop = cadence_at(home.path(), state.path(), &["daemon", "stop"]);
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let marker = state.path().join("shutdown.json");
    let marker_before = std::fs::read(&marker).expect("clean stop writes shutdown.json");
    let db = state.path().join("cadence.sqlite3");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE daemon_build SET commit_sha='deadbeefdead' WHERE id=1",
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
    }
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(std::path::PathBuf::from(format!(
            "{}{suffix}",
            db.display()
        )));
    }
    let before = sqlite_family(state.path());
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(state.path())
        .args(["daemon", "run", "--rollout-as", "operator:intruder"])
        .env("HOME", home.path())
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_ROLLOUT_AS")
        .envs(test_env().vars())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut timed_out = false;
    loop {
        match child.try_wait().unwrap() {
            Some(_) => break,
            None if std::time::Instant::now() > deadline => {
                timed_out = true;
                let _ = child.kill();
                break;
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
    let out = child.wait_with_output().unwrap();
    let err = format!(
        "{} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !timed_out,
        "direct daemon run did not exit; killed it. output: {err}"
    );
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("deadbeefdead"), "{err}");
    assert_eq!(std::fs::read(&marker).unwrap(), marker_before);
    assert_eq!(sqlite_family(state.path()), before);
}

/// The holder claims, records a backup, and migrates through
/// `daemon start --as`. No `MIGRATION_HOLDER` override: the child learns
/// the holder from `--rollout-as`.
#[test]
fn holder_migrates_through_daemon_start_without_a_test_override() {
    let home = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let _reaper = DaemonReaper::new(state.path());
    let start = cadence_at(home.path(), state.path(), &["daemon", "start"]);
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    let stop = cadence_at(home.path(), state.path(), &["daemon", "stop"]);
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
    let db = state.path().join("cadence.sqlite3");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "DROP TABLE IF EXISTS rollout_leases;
         DROP TABLE IF EXISTS daemon_build;
         UPDATE schema_version SET version=11;
         PRAGMA wal_checkpoint(TRUNCATE);",
    )
    .unwrap();
    drop(conn);
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(std::path::PathBuf::from(format!(
            "{}{suffix}",
            db.display()
        )));
    }
    let version: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 11);
    let claim = cadence_at(
        home.path(),
        state.path(),
        &[
            "rollout",
            "claim",
            "--reason",
            "crossing",
            "--as",
            "operator:test",
            "--ttl",
            "2h",
        ],
    );
    assert!(
        claim.status.success(),
        "{}",
        String::from_utf8_lossy(&claim.stderr)
    );
    let backup = home.path().join("backup.sqlite3");
    std::fs::copy(&db, &backup).unwrap();
    let recorded = cadence_at(
        home.path(),
        state.path(),
        &[
            "rollout",
            "backup",
            "--path",
            backup.to_str().unwrap(),
            "--as",
            "operator:test",
        ],
    );
    assert!(
        recorded.status.success(),
        "{} {}",
        String::from_utf8_lossy(&recorded.stdout),
        String::from_utf8_lossy(&recorded.stderr)
    );
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    drop(conn);
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(std::path::PathBuf::from(format!(
            "{}{suffix}",
            db.display()
        )));
    }
    let start = cadence_at(
        home.path(),
        state.path(),
        &["daemon", "start", "--as", "operator:test"],
    );
    let started = start.status.success();
    if started {
        let stop = cadence_at(home.path(), state.path(), &["daemon", "stop"]);
        assert!(
            stop.status.success(),
            "{}",
            String::from_utf8_lossy(&stop.stderr)
        );
    }
    assert!(
        started,
        "holder migration failed: {} {}",
        String::from_utf8_lossy(&start.stdout),
        String::from_utf8_lossy(&start.stderr)
    );
    let version: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, cadence_agent::rollout::SCHEMA_VERSION);
}

fn auto_stop_status(d: &TestDaemon) -> Value {
    d.rpc("health", json!({})).unwrap()["agent_auto_stop"].clone()
}

/// Wait for the timer to finish a sweep whose clock reading falls in
/// `window` (clock seconds) — `last_kept` is that sweep's.
fn wait_auto_stop_check(d: &TestDaemon, window: std::ops::Range<f64>) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let status = auto_stop_status(d);
        if status["last_sweep_at"]
            .as_f64()
            .is_some_and(|at| window.contains(&at))
        {
            return status;
        }
        assert!(Instant::now() < deadline, "no auto-stop check: {status}");
        thread::sleep(Duration::from_millis(50));
    }
}

/// The timer's status once its sweep has published a stop.
fn wait_auto_stop_published(d: &TestDaemon) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = auto_stop_status(d);
        if status["stopped_total"].as_u64().unwrap_or(0) > 0 {
            return status;
        }
        assert!(Instant::now() < deadline, "no stop published: {status}");
        thread::sleep(Duration::from_millis(50));
    }
}

fn wall_secs() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

#[test]
fn auto_stop_idle_agent_stops_with_event_label_and_resumes() {
    let (d, offset) = auto_stop_daemon(daemon::AutoStopSetting::idle_after(3600));
    d.register_inbox("pm");
    register_fake_opts(&d, "w1", json!({"upstream": "pm"}));
    d.wait_agent("w1", "idle", 20);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "hello", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("w1", "m1", &["completed"], 20);
    let status = auto_stop_status(&d);
    assert_eq!(status["enabled"], true, "{status}");
    assert_eq!(status["idle_secs"], 3600, "{status}");

    // Two hours on the timer's clock: the next check stops w1.
    offset.store(7200, std::sync::atomic::Ordering::SeqCst);
    let agent = wait_auto_stopped(&d, "w1");
    assert_eq!(agent["enabled"], false, "{agent}");
    assert_eq!(agent["resumable"], true, "{agent}");
    let label = agent["state_label"].as_str().unwrap();
    assert!(label.starts_with("stopped (auto, idle "), "{label}");
    assert_eq!(agent["auto_stopped"]["bound_secs"], 3600, "{agent}");
    assert_eq!(
        agent["auto_stopped"]["resume"], "cadence agent resume w1",
        "{agent}"
    );
    // Recorded on w1's own stream, after the normal stop path's own
    // `stop_requested`.
    let events = d
        .rpc("agent_events", json!({"alias": "w1", "after": 0}))
        .unwrap()["events"]
        .as_array()
        .unwrap()
        .clone();
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["kind"].as_str()).collect();
    let stop_at = kinds.iter().position(|k| *k == "stop_requested").unwrap();
    let auto_at = kinds
        .iter()
        .position(|k| *k == "agent_auto_stopped")
        .unwrap();
    assert!(stop_at < auto_at, "{kinds:?}");
    let payload = &events[auto_at]["payload"];
    assert!(
        payload["idle_secs"].as_f64().unwrap() >= 7000.0,
        "{payload}"
    );
    assert_eq!(payload["bound_secs"], 3600, "{payload}");
    assert_eq!(payload["bound_source"], "[host] auto_stop_idle_secs");
    assert!(payload["reason"]
        .as_str()
        .unwrap()
        .contains("no queued, running, awaiting-report or unknown message"));
    let status = wait_auto_stop_published(&d);
    assert_eq!(status["last_stopped"], json!(["w1"]), "{status}");
    assert_eq!(status["stopped_total"], 1, "{status}");
    // agent list and `cadence status` both render it distinctly.
    let list = d.rpc("agent_list", json!({})).unwrap();
    let row = list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "w1")
        .unwrap()
        .clone();
    assert_eq!(row["state_label"], agent["state_label"], "{row}");
    let pm_dir = TempDir::new().unwrap();
    let envs = [("CADENCE_PM_DIR", pm_dir.path())];
    let view = status_json(&d.state, &[], &envs);
    let srow = view["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "w1")
        .unwrap()
        .clone();
    assert_eq!(srow["state"], "stopped", "{srow}");
    assert_eq!(srow["state_label"], agent["state_label"], "{srow}");
    let table = status_table(&d.state, &envs);
    assert!(table.contains(label), "{table}");

    // Resume brings it back on its saved thread; the marker is gone.
    let out = d.rpc("agent_resume", json!({"alias": "w1"})).unwrap();
    assert_eq!(out["state"], "starting", "{out}");
    let agent = d.wait_agent("w1", "idle", 20);
    assert!(agent["auto_stopped"].is_null(), "{agent}");
    assert!(agent["state_label"].is_null(), "{agent}");
    assert_eq!(agent["thread_id"], "fake-thread-w1", "{agent}");
    // The resume's `ready` restarted the idle clock: a check a minute
    // after it (the clock stepped back from +2h re-arms it) keeps w1.
    offset.store(60, std::sync::atomic::Ordering::SeqCst);
    let now = wall_secs();
    let status = wait_auto_stop_check(&d, now..now + 600.0);
    let why = status["last_kept"]["w1"].as_str().unwrap();
    assert!(why.starts_with("idle "), "{status}");
    assert_eq!(d.wait_agent("w1", "idle", 5)["state"], "idle");
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "again", "message": "m2"}),
    )
    .unwrap();
    d.wait_message("w1", "m2", &["completed"], 20);
}

#[test]
fn auto_stop_keeps_pm_inbox_opted_out_and_busy_agents() {
    let (d, offset) = auto_stop_daemon(daemon::AutoStopSetting::idle_after(3600));
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.fixture_rpc(
        "agent_register",
        json!({"alias": "pm", "provider": "fake", "endpoint_kind": "fake",
               "cwd": cwd, "role": "pm"}),
    )
    .unwrap();
    register_inbox_with(&d, "box", json!({"upstream": "pm"}));
    for alias in ["w-opt", "w-busy", "w-idle"] {
        register_fake_opts(&d, alias, json!({"upstream": "pm"}));
    }
    for alias in ["pm", "w-opt", "w-busy", "w-idle"] {
        d.wait_agent(alias, "idle", 20);
    }
    // Per-agent opt-out rides the ordinary `agent set` allowlist.
    d.operator_rpc(
        "agent_set",
        json!({"alias": "w-opt", "patch": {"auto_stop": "off"}}),
    )
    .unwrap();
    let err = d
        .operator_rpc(
            "agent_set",
            json!({"alias": "w-opt", "patch": {"auto_stop": "on"}}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("accepts \"off\""), "{err}");
    let err = d
        .operator_rpc(
            "agent_set",
            json!({"alias": "w-opt", "patch": {"auto_stop_idle_secs": "1h"}}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("non-negative integer"), "{err}");
    let err = d
        .operator_rpc(
            "agent_set",
            json!({"alias": "box", "patch": {"auto_stop": "off"}}),
        )
        .unwrap_err();
    assert!(err
        .to_string()
        .contains("only applies to endpoints with an actor"));
    // A turn held open: busy for the whole check.
    d.rpc(
        "agent_send",
        json!({"alias": "w-busy", "text": "SLEEP:30", "message": "hold"}),
    )
    .unwrap();
    d.wait_message("w-busy", "hold", &["running"], 20);

    offset.store(7200, std::sync::atomic::Ordering::SeqCst);
    // The control proves a check ran with everyone past the bound.
    wait_auto_stopped(&d, "w-idle");
    let status = wait_auto_stop_published(&d);
    assert_eq!(status["last_stopped"], json!(["w-idle"]), "{status}");
    let kept = &status["last_kept"];
    assert_eq!(kept["pm"], "group root (role pm)", "{status}");
    assert!(kept["box"].is_null(), "an inbox has no actor: {status}");
    assert_eq!(
        kept["w-opt"], "auto-stop off (agent auto_stop=off)",
        "{status}"
    );
    assert!(
        kept["w-busy"]
            .as_str()
            .is_some_and(|r| r.starts_with("state ") || r.starts_with("busy:")),
        "{status}"
    );
    for alias in ["pm", "w-opt"] {
        let agent = d.rpc("agent_show", json!({"alias": alias})).unwrap()["agent"].clone();
        assert_eq!(agent["state"], "idle", "{alias}: {agent}");
        assert!(agent["auto_stopped"].is_null(), "{alias}: {agent}");
    }
    assert_eq!(d.message_state("w-busy", "hold"), "running");
    let inbox = d.rpc("agent_show", json!({"alias": "box"})).unwrap()["agent"].clone();
    assert_eq!(inbox["state"], "idle", "{inbox}");
    // No auto-stop event anywhere but on the control.
    for alias in ["pm", "box", "w-opt", "w-busy"] {
        let events = d
            .rpc("agent_events", json!({"alias": alias, "after": 0}))
            .unwrap()["events"]
            .as_array()
            .unwrap()
            .clone();
        assert!(
            !events.iter().any(|e| e["kind"] == "agent_auto_stopped"),
            "{alias}: {events:?}"
        );
    }
}

#[test]
fn auto_stop_pinned_off_in_test_daemons_and_reported() {
    let d = TestDaemon::start();
    let status = auto_stop_status(&d);
    assert_eq!(status["enabled"], false, "{status}");
    assert_eq!(status["default_secs"], 3600, "{status}");
    assert!(status["attach_detection"]
        .as_str()
        .unwrap()
        .contains("not detectable"));
}

#[test]
fn auto_stop_keeps_pty_pane_with_attached_client_then_stops_it() {
    let (d, offset) = auto_stop_daemon(daemon::AutoStopSetting::idle_after(3600));
    let mock = d.mock_stub();
    d.register_inbox("pm");
    d.register_stub("s1", json!({"auto_ready": "verified", "upstream": "pm"}));
    let agent = d.wait_agent("s1", "idle", 20);
    assert!(agent["thread_id"].is_string(), "{agent}");
    let clients = d.stub_pane_file(&mock, "s1", "clients");
    atomic_write(clients.clone(), "/dev/pts/7\n");

    offset.store(7200, std::sync::atomic::Ordering::SeqCst);
    let now = wall_secs();
    let status = wait_auto_stop_check(&d, now + 7000.0..now + 8000.0);
    assert_eq!(
        status["last_kept"]["s1"], "1 terminal client(s) attached",
        "{status}"
    );
    assert_eq!(d.wait_agent("s1", "idle", 5)["state"], "idle");

    // Detached: the next check (a minute later on the timer's clock)
    // stops it through the normal pty stop path.
    std::fs::remove_file(&clients).unwrap();
    offset.store(7300, std::sync::atomic::Ordering::SeqCst);
    let agent = wait_auto_stopped(&d, "s1");
    assert!(
        agent["state_label"]
            .as_str()
            .unwrap()
            .starts_with("stopped (auto, idle "),
        "{agent}"
    );
    assert_eq!(agent["resumable"], true, "{agent}");
    let calls = std::fs::read_to_string(
        mock.dir
            .join("tmux-state")
            .join(socket_for(&d.state))
            .join("calls.log"),
    )
    .unwrap();
    assert!(calls.contains("list-clients -t =s1"), "{calls}");
    assert!(calls.contains("kill-session -t s1"), "{calls}");
}

// ---- CAD-413: work queued for an auto-stopped agent resumes it ----

/// The stop reason is durable: across a daemon restart, a message for
/// the agent the idle timer stopped resumes it and is delivered, while
/// an operator/PM-stopped agent — including one stopped by hand after
/// its auto-stop — stays stopped with the message queued.
#[test]
fn auto_resume_after_restart_only_for_the_timers_stop() {
    let root = TempDir::new().unwrap();
    let state = root.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let offset = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    {
        let d = TestDaemon::start_on_opts(
            state.clone(),
            auto_stop_opts(daemon::AutoStopSetting::idle_after(3600), &offset),
        );
        d.register_inbox("pm");
        for alias in ["w-auto", "w-op", "w-both"] {
            register_fake_opts(&d, alias, json!({"upstream": "pm"}));
            d.wait_agent(alias, "idle", 20);
        }
        // An operator/PM stop before the timer could act.
        d.rpc("agent_stop", json!({"alias": "w-op"})).unwrap();
        offset.store(7200, std::sync::atomic::Ordering::SeqCst);
        wait_auto_stopped(&d, "w-auto");
        wait_auto_stopped(&d, "w-both");
        offset.store(0, std::sync::atomic::Ordering::SeqCst);
        // A manual stop after the auto-stop supersedes it.
        d.rpc("agent_stop", json!({"alias": "w-both"})).unwrap();
        let both = d.rpc("agent_show", json!({"alias": "w-both"})).unwrap()["agent"].clone();
        assert!(both["auto_stopped"].is_null(), "{both}");
    }
    // A fresh daemon — auto-stop pinned off — reads the stop reasons
    // back from the durable event streams.
    let d = TestDaemon::start_on(state.clone());
    for alias in ["w-auto", "w-op", "w-both"] {
        assert_eq!(d.wait_agent(alias, "stopped", 10)["state"], "stopped");
    }
    for (alias, id) in [("w-op", "m-op"), ("w-both", "m-both"), ("w-auto", "m-auto")] {
        d.rpc(
            "agent_send",
            json!({"alias": alias, "text": "work", "message": id}),
        )
        .unwrap();
    }
    // The auto-stopped agent resumes on its saved thread and delivers.
    let done = d.wait_message("w-auto", "m-auto", &["completed"], 20);
    assert_eq!(done["state"], "completed", "{done}");
    let agent = d.wait_agent("w-auto", "idle", 10);
    assert_eq!(agent["enabled"], true, "{agent}");
    assert_eq!(agent["thread_id"], "fake-thread-w-auto", "{agent}");
    assert!(agent["auto_stopped"].is_null(), "{agent}");
    let resumed = d.wait_event("w-auto", "agent_auto_resumed", 5);
    assert_eq!(resumed["payload"]["message"], "m-auto", "{resumed}");
    assert_eq!(resumed["payload"]["queued"], 1, "{resumed}");
    let kinds = event_kinds(&d, "w-auto");
    let pos = |k: &str| kinds.iter().rposition(|x| x == k).unwrap();
    assert!(
        pos("agent_auto_stopped") < pos("agent_auto_resumed")
            && pos("agent_auto_resumed") < pos("ready"),
        "{kinds:?}"
    );
    // The sweep that resumed w-auto read every stopped agent's queue
    // after m-op and m-both were enqueued — and left both alone.
    for (alias, id) in [("w-op", "m-op"), ("w-both", "m-both")] {
        let agent = d.rpc("agent_show", json!({"alias": alias})).unwrap()["agent"].clone();
        assert_eq!(agent["state"], "stopped", "{alias}: {agent}");
        assert_eq!(agent["enabled"], false, "{alias}: {agent}");
        assert_eq!(d.message_state(alias, id), "queued", "{alias}");
        let kinds = event_kinds(&d, alias);
        assert!(
            !kinds.iter().any(|k| k.starts_with("agent_auto_resume")),
            "{alias}: {kinds:?}"
        );
    }
}
