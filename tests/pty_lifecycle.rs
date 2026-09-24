//! pty_lifecycle: area tests split from tests/integration.rs (CAD-426).
//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use cadence_agent::store::{NewAgent, Store};
use serde_json::{json, Value};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// G2: a pty pane that survives a daemon restart mid-turn is re-adopted
/// by the normal resume path after the operator unfences — the adapter
/// reattaches to the surviving tmux session and verifies the pane's
/// native-session lock against the stored thread_id.
#[test]
fn pty_restart_fence_unfence_resume_readopts_pane() {
    let mut d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 20);
    let native = agent["thread_id"].as_str().unwrap().to_string();
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    pty_token(&d, "dv1", "m1");
    let pane_pid: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Daemon dies mid-turn — the tmux pane survives (detached). The
    // stop itself is clean; deleting the marker makes it a crash —
    // this test asserts the fence path, not hot-restart adoption.
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    drop_shutdown_marker(&d.state);
    let state = d.state.clone();
    // Leak d's TempDir — it owns both the state dir and the mock's pane
    // state, which must outlive the second daemon. forget() also keeps
    // Drop from shutting the new daemon's socket.
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    // Recover fenced the in-flight turn; the restart skipped relaunch.
    let agent = d.wait_agent("dv1", "attention", 15);
    assert!(agent["endpoint"].is_null());
    assert!(event_kinds(&d, "dv1")
        .iter()
        .any(|k| k == "relaunch_skipped"));
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    // The pane itself is still alive.
    let pid_now: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pid_now, pane_pid);
    // Unfence → stopped, then resume adopts the surviving pane.
    d.operator_rpc(
        "agent_unfence",
        json!({"alias": "dv1", "status": "interrupted"}),
    )
    .unwrap();
    d.wait_agent("dv1", "stopped", 10);
    d.rpc("agent_resume", json!({"alias": "dv1"})).unwrap();
    let agent = d.wait_agent("dv1", "idle", 20);
    // Same pane pid + same native session = adopted, not respawned.
    let pid_after: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pid_after, pane_pid, "pane was not re-adopted");
    assert_eq!(agent["thread_id"].as_str().unwrap(), native);
    // New work flows over the adopted pane.
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "again", "message": "m2"}),
    )
    .unwrap();
    d.wait_message("dv1", "m2", &["running"], 20);
}

#[test]
fn pty_reattach_waits_for_session_proof() {
    // The reattach branch once read `owned_session` exactly once: a
    // proof that was merely not-yet-visible (a provider still
    // registering, a raced /proc scan) fenced a live pane — and the
    // failed open then closed it, destroying the session it was meant
    // to adopt. The stub's `owned_misses` file replays that window
    // deterministically: the first probes see nothing while the pane
    // holds its lock throughout, and adoption must still succeed.
    let dir = TempDir::new().unwrap();
    let seeded = dir.path().join("state");
    std::fs::create_dir_all(&seeded).unwrap();
    let fixtures = TempDir::new().unwrap();
    {
        let mock = install_mock_stub(fixtures.path());
        let d = TestDaemon::start_on(seeded.clone());
        d.register_stub("st1", json!({}));
        let agent = d.wait_agent("st1", "idle", 20);
        let native = agent["thread_id"].as_str().unwrap().to_string();
        let pane_pid = agent["pid"].as_i64().unwrap();
        drop(d);
        // The next owned_session calls report no session — the
        // transient-miss window the reattach must wait out rather than
        // fence on. The knob is a file in this stub's locks dir,
        // scoped to this fixture, never process env.
        std::fs::write(mock.locks.join("owned_misses"), "3").unwrap();
        let d2 = TestDaemon::start_on(seeded.clone());
        let agent2 = d2.wait_agent("st1", "idle", 25);
        assert_eq!(agent2["thread_id"].as_str().unwrap(), native);
        assert_eq!(agent2["pid"].as_i64().unwrap(), pane_pid);
    }
}

#[test]
fn pty_shutdown_straggler_detaches_pane() {
    // Regression for the force-close half of the re-adoption race:
    // stop_ctls once ran adapter.close() on actors still working when
    // the stop grace expired — even during daemon shutdown, where the
    // contract is detach. kill-session SIGKILLed the pane the restart
    // was meant to re-adopt, and resume then legitimately spawned a
    // replacement ("pane was not re-adopted"). MOCK_TMUX_HOLD stretches
    // every `#{pane_dead}` read past the grace window, so the actor is a
    // deterministic straggler — no sleeps, the latency lives in the
    // mock's response time. (Only pane_dead is held: holding every
    // display-message would let a held cursor read burn the render
    // deadline and fail the send before it ever reaches `running`.)
    let mut d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 25);
    let native = agent["thread_id"].as_str().unwrap().to_string();
    let pane_pid: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Real env, under MockDevin's ENV_LOCK: the mock tmux reads its hold
    // knobs per call from the env it inherits from the daemon.
    std::env::set_var("MOCK_TMUX_HOLD", "4"); // > STOP_GRACE (3s)
    std::env::set_var("MOCK_TMUX_HOLD_FMT", "#{pane_dead}");
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    // Once the turn is running the actor is inside held adapter probes
    // (gate, then the idle loop's disconnected check); it cannot finish
    // inside the 3s grace, so the straggler path fires every run.
    d.wait_message("dv1", "m1", &["running"], 40);
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    // The straggler stop was clean; a crash leaves no marker — this
    // test asserts the fence-then-resume path, not hot adoption.
    drop_shutdown_marker(&d.state);
    let state = d.state.clone();
    std::env::remove_var("MOCK_TMUX_HOLD");
    std::env::remove_var("MOCK_TMUX_HOLD_FMT");
    // Leak d's TempDir — it owns the state dir and the mock's pane
    // state, which must outlive the second daemon.
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    // Recover fenced the in-flight turn; the pane itself survived the
    // straggler stop, so unfence → resume re-adopts the same pane.
    d.wait_agent("dv1", "attention", 15);
    d.operator_rpc(
        "agent_unfence",
        json!({"alias": "dv1", "status": "interrupted"}),
    )
    .unwrap();
    d.wait_agent("dv1", "stopped", 10);
    d.rpc("agent_resume", json!({"alias": "dv1"})).unwrap();
    let agent = d.wait_agent("dv1", "idle", 25);
    let pid_after: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        pid_after, pane_pid,
        "straggler pane was killed, not detached"
    );
    assert_eq!(agent["thread_id"].as_str().unwrap(), native);
}

#[test]
fn pty_hot_restart_adopts_running_turn() {
    // The acceptance shape: mid-turn, `shutdown`, a new daemon on the
    // same state — the agent never fences, the message stays running,
    // the pane is the same, `turn_adopted` is emitted, and the
    // ORIGINAL token still completes the turn and routes the result.
    let (state, mock, token, pane_pid) = stopped_mid_turn_devin();
    let d = TestDaemon::start_on(state);
    let agent = d.wait_agent("dv1", "idle", 25);
    assert_eq!(d.message_state("dv1", "m1"), "running");
    let pid_now: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pid_now, pane_pid, "adopted pane changed pid");
    assert_eq!(agent["pid"].as_i64().unwrap() as i32, pane_pid);
    let kinds = event_kinds(&d, "dv1");
    assert!(kinds.iter().any(|k| k == "turn_adopted"), "{kinds:?}");
    assert!(
        !kinds.iter().any(|k| k == "turn_adopt_refused"),
        "{kinds:?}"
    );
    // The turn's original token is still authoritative — the recorded
    // generation was reused, so the report validates.
    d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("dv1", "m1", &["completed"], 15);
    // ... and the result routed to reply_to as always.
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
    assert!(
        pm["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["source"] == "worker_result"),
        "adopted turn's result never routed: {pm}"
    );
}

/// CAD-385 acceptance 1 + 3: a daemon restart re-records each adopted
/// pane's process start time — even over a row recorded without one
/// (the pre-v14 shape) — so a live pane keeps its identity: an RPC from
/// inside the pane resolves to its agent. The same row with the pid's
/// start changed (a reused pid) is no pane at all — the caller is
/// placed exactly as an unregistered process — and with no start the
/// call is refused, naming the remedy.
#[test]
fn cad385_hot_restart_rerecords_pane_start_and_the_pane_keeps_its_identity() {
    let (state, mock, _token, pane_pid) = stopped_mid_turn_devin();
    let db = state.join("cadence.sqlite3");
    let set_start = |start: Option<i64>| {
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute(
                "UPDATE agents SET pid_start=?1 WHERE alias='dv1'",
                rusqlite::params![start],
            )
            .unwrap();
    };
    // The row the stopped daemon left, as a pre-v14 daemon would have.
    set_start(None);
    let d = TestDaemon::start_on(state.clone());
    d.wait_agent("dv1", "idle", 25);
    let kinds = event_kinds(&d, "dv1");
    assert!(kinds.iter().any(|k| k == "turn_adopted"), "{kinds:?}");
    let agent = d.rpc("agent_show", json!({"alias": "dv1"})).unwrap()["agent"].clone();
    assert_eq!(agent["pid"].as_i64(), Some(i64::from(pane_pid)), "{agent}");
    let start = proc_start(pane_pid as u32).expect("the adopted pane is alive");
    assert_eq!(agent["pid_start"].as_i64(), Some(start), "{agent}");

    // The live pane keeps working: its own RPC derives its lane.
    d.memory_rpc(&mock, "dv1", "slot_status", json!({}))
        .expect("the adopted pane's caller identity resolves");

    // The pid now "reused": same number, a different recorded start.
    set_start(Some(start - 1));
    let err = d
        .memory_rpc(&mock, "dv1", "slot_status", json!({}))
        .unwrap_err();
    assert!(
        err.contains("descends from no registered pane"),
        "a stale row must place the caller as unregistered: {err}"
    );
    // No recorded start: refused, naming the remedy.
    set_start(None);
    let err = d
        .memory_rpc(&mock, "dv1", "slot_status", json!({}))
        .unwrap_err();
    assert!(
        err.contains("'dv1'") && err.contains("cadence daemon restart"),
        "{err}"
    );
    set_start(Some(start));
    d.memory_rpc(&mock, "dv1", "slot_status", json!({}))
        .expect("restored");
}

#[test]
fn pty_hot_restart_no_marker_fences() {
    // No marker == crash: the pane survives but the turn fences
    // exactly as before — unknown, attention, no adoption events.
    let (state, _mock, _token, _pid) = stopped_mid_turn_devin();
    drop_shutdown_marker(&state);
    let d = TestDaemon::start_on(state);
    let agent = d.wait_agent("dv1", "attention", 20);
    let error = agent["error"].as_str().unwrap_or("");
    assert!(
        error.contains("Runtime restarted during provider turn"),
        "{error}"
    );
    assert!(error.contains("does not prove"), "{error}");
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    let kinds = event_kinds(&d, "dv1");
    assert!(!kinds.iter().any(|k| k == "turn_adopted"), "{kinds:?}");
    assert!(
        !kinds.iter().any(|k| k == "turn_adopt_refused"),
        "no marker means nothing to refuse: {kinds:?}"
    );
}

#[test]
fn pty_hot_restart_stale_instance_fences() {
    // A marker bound to a different daemon run is not proof this
    // daemon stopped cleanly — every recorded entry is refused.
    let (state, _mock, _token, _pid) = stopped_mid_turn_devin();
    std::fs::write(state.join("daemon-instance"), "some-other-run\n").unwrap();
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    let reason = adopt_refusal(&d, "dv1").expect("missing refusal");
    assert!(reason.contains("daemon run"), "{reason}");
}

#[test]
fn pty_hot_restart_expired_marker_fences() {
    // A marker older than its bound reads stale pane state as fresh —
    // it expires into the fence path instead.
    let (state, _mock, _token, _pid) = stopped_mid_turn_devin();
    let path = state.join("shutdown.json");
    let mut marker = read_marker(&state);
    marker["at"] = json!(marker["at"].as_f64().unwrap() - 3600.0);
    std::fs::write(&path, marker.to_string()).unwrap();
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    let reason = adopt_refusal(&d, "dv1").expect("missing refusal");
    assert!(reason.contains("expired"), "{reason}");
}

#[test]
fn pty_hot_restart_dead_pane_fences() {
    // The pane died between stop and start — nothing left to adopt.
    let (state, _mock, _token, pane_pid) = stopped_mid_turn_devin();
    unsafe { libc::killpg(pane_pid, libc::SIGKILL) };
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    let reason = adopt_refusal(&d, "dv1").expect("missing refusal");
    assert!(reason.contains("pane"), "{reason}");
}

#[test]
fn pty_hot_restart_native_mismatch_fences() {
    // The pane is alive but cannot prove it still holds the recorded
    // native session — the recorded checks fail, the turn fences.
    let (state, _mock, _token, _pid) = stopped_mid_turn_devin();
    let path = state.join("shutdown.json");
    let mut marker = read_marker(&state);
    marker["entries"][0]["native_session"] = json!("bogus-session");
    std::fs::write(&path, marker.to_string()).unwrap();
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    let reason = adopt_refusal(&d, "dv1").expect("missing refusal");
    assert!(reason.contains("session"), "{reason}");
}

#[test]
fn pty_hot_restart_submitting_never_records_running() {
    // A paste whose render never proved must not be recorded running:
    // held captures + a swallowed paste keep m1 `submitting` through
    // the drain, where the render deadline resolves it `unknown` — the
    // restart then fences it like any other uncertain outcome.
    let mut d = TestDaemon::start();
    let mock = d.mock_devin();
    // Real env, under MockDevin's ENV_LOCK: the mock tmux reads its
    // hold knobs per call from the env it inherits from the daemon.
    std::env::set_var("MOCK_TMUX_HOLD", "5");
    std::env::set_var("MOCK_TMUX_HOLD_CMD", "capture-pane");
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 40);
    atomic_write(d.pane_file(&mock, "dv1", "swallow"), "1");
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("dv1", "m1", &["submitting"], 15);
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    std::env::remove_var("MOCK_TMUX_HOLD");
    std::env::remove_var("MOCK_TMUX_HOLD_CMD");
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    let m1 = d.message_state("dv1", "m1");
    assert!(
        m1 == "unknown" || m1 == "queued",
        "submitting survived: {m1}"
    );
    assert!(
        !event_kinds(&d, "dv1").iter().any(|k| k == "turn_adopted"),
        "an unproven paste must never be adopted"
    );
}

#[test]
fn pty_hot_restart_render_during_stop_adopts() {
    // The other half of the drain rule: a paste whose render check is
    // still in flight when the stop lands is allowed to finish — it IS
    // a proven running turn and is adopted like any other.
    let mut d = TestDaemon::start();
    let _mock = d.mock_devin();
    // Real env, under MockDevin's ENV_LOCK: the mock tmux reads its
    // hold knobs per call from the env it inherits from the daemon.
    // 2s hold vs the 4s render deadline: comfortably inside it while
    // still spanning the stop that must land mid-render.
    std::env::set_var("MOCK_TMUX_HOLD", "2");
    std::env::set_var("MOCK_TMUX_HOLD_CMD", "capture-pane");
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 40);
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("dv1", "m1", &["submitting"], 15);
    // The stop lands mid-render; the actor finishes the proof before
    // detaching, so the marker records a genuinely running turn.
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    std::env::remove_var("MOCK_TMUX_HOLD");
    std::env::remove_var("MOCK_TMUX_HOLD_CMD");
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "idle", 25);
    assert_eq!(d.message_state("dv1", "m1"), "running");
    assert!(
        event_kinds(&d, "dv1").iter().any(|k| k == "turn_adopted"),
        "render proven during the drain must adopt"
    );
}

#[test]
fn hot_restart_managed_turn_stays_unknown() {
    // Managed endpoints are never adoptable — the provider process
    // dies with its daemon, so a clean stop changes nothing for them.
    let mut d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 25);
    d.rpc(
        "agent_send",
        json!({"alias": "w1", "text": "SLEEP:60", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("w1", "m1", &["running"], 25);
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    d.wait_agent("w1", "attention", 25);
    d.wait_message("w1", "m1", &["unknown"], 25);
}

#[test]
fn pty_hot_restart_report_before_adoption_rejected_stale() {
    // The pre-verification window: the store keeps the message
    // `running` but clears `generation` until `open_adopted` proves
    // the pane. A report landing inside the window must be refused
    // as stale — the turn is not yet known to be alive — and the
    // same token must complete once `turn_adopted` fires.
    let (state, _mock, token, _pid) = stopped_mid_turn_devin();
    // Hold the first pane check inside open_adopted so the socket is
    // serving while the adoption is still unproven. Real env, under
    // MockDevin's ENV_LOCK: the mock tmux reads its hold knobs per
    // call from the env it inherits from the daemon.
    std::env::set_var("MOCK_TMUX_HOLD", "8");
    std::env::set_var("MOCK_TMUX_HOLD_CMD", "has-session");
    let d = TestDaemon::start_on(state);
    let early = d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "result",
               "text": "early"}),
    );
    std::env::remove_var("MOCK_TMUX_HOLD");
    std::env::remove_var("MOCK_TMUX_HOLD_CMD");
    let err = early.expect_err("pre-proof report must be refused");
    assert!(err.to_string().contains("stale"), "{err}");
    let agent = d.wait_agent("dv1", "idle", 25);
    assert!(
        agent["generation"].as_str().is_some_and(|g| !g.is_empty()),
        "adoption must restore the recorded generation: {agent}"
    );
    assert!(
        event_kinds(&d, "dv1").iter().any(|k| k == "turn_adopted"),
        "pane proof did not adopt the turn"
    );
    d.rpc(
        "message_report",
        json!({"message": "m1", "token": token, "kind": "result",
               "text": "done"}),
    )
    .unwrap();
    d.wait_message("dv1", "m1", &["completed"], 15);
}

#[test]
fn pty_hot_restart_adopts_multiple_running_turns() {
    // Pasted pty messages stay `running` until reported. Since CAD-250
    // `take_queued` holds one report-owing turn per actor, but rows that
    // accumulated before that rule can still hold more than one
    // in-flight turn on an alias. Every qualifying turn is
    // adopted — same pane proof covers them all — not just the last
    // one the marker listed. The mock pane reads busy through m1's
    // turn, so the second `running` row is crafted the way the real
    // race would leave it: a queued send flipped to running under the
    // same generation.
    let mut d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 20);
    let generation = agent["generation"].as_str().unwrap().to_string();
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    let token1 = pty_token(&d, "dv1", "m1");
    // m2's row as a second proven turn on the same pane — inserted
    // `running` outright so the actor never sees it `queued` and the
    // marker records it verbatim.
    let token2 = format!("pty-{generation}-turn-m2");
    {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "INSERT INTO messages(id,alias,body,reply_to,source,state,turn_id,
                 created)
             VALUES('m2','dv1','task',NULL,'test','running',?1,?2)",
            // A fresh row: since CAD-250 F2 every turn-holding row is
            // bounded from its delivery, so an epoch-1970 one would expire.
            rusqlite::params![
                token2,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs_f64()
            ],
        )
        .unwrap();
    }
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    let marker = read_marker(&state);
    assert_eq!(
        marker["entries"].as_array().unwrap().len(),
        2,
        "both running turns must be recorded: {marker}"
    );
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "idle", 25);
    wait_event_count(&d, "dv1", "turn_adopted", 2, 15);
    assert_eq!(d.message_state("dv1", "m1"), "running");
    assert_eq!(d.message_state("dv1", "m2"), "running");
    for (id, token) in [("m1", token1), ("m2", token2)] {
        d.rpc(
            "message_report",
            json!({"message": id, "token": token, "kind": "result",
                   "text": "done"}),
        )
        .unwrap();
        d.wait_message("dv1", id, &["completed"], 15);
    }
}

/// CAD-250: a nudge is steering for its moment — one still queued when
/// the daemon stops is cancelled at the next start with a
/// `nudge_cancelled` event, never pasted into the later pane.
#[test]
fn pty_nudge_queued_at_restart_is_cancelled_not_replayed() {
    let mut d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    // An open approval menu holds every paste at the gate.
    atomic_write(d.pane_file(&mock, "dv1", "tui-state"), DEVIN_MENU);
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "late steer", "message": "n1", "nudge": true}),
    )
    .unwrap();
    d.wait_event_where("dv1", "gate_wait", |e| e["payload"]["message"] == "n1", 20);
    assert_eq!(d.message_state("dv1", "n1"), "queued");
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    std::mem::forget(d);
    std::fs::remove_file(d_pane(&mock, &state, "dv1", "tui-state")).unwrap();
    let d = TestDaemon::start_on(state);
    let m = d.wait_message("dv1", "n1", &["cancelled", "unknown"], 20);
    assert_eq!(m["state"], "cancelled", "{m}");
    // The actor's own exit at shutdown cancelled it (N2); a crash would
    // leave it to the next start's recovery (`restart_cancelled`).
    assert_eq!(m["result"]["via"], "shutdown_cancelled", "{m}");
    let ev = d.wait_event_where(
        "dv1",
        "nudge_cancelled",
        |e| e["payload"]["message"] == "n1",
        10,
    );
    assert_eq!(ev["payload"]["was"], "queued", "{ev}");
    // The pane never received it, and it does not fence the agent.
    d.wait_agent("dv1", "idle", 25);
    let input =
        std::fs::read_to_string(d_pane(&mock, &d.state, "dv1", "input")).unwrap_or_default();
    assert!(!input.contains("late steer"), "nudge replayed: {input}");
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "dv1"})).unwrap()["unknown"],
        0
    );
}

/// CAD-250 reconcile path for rows that accumulated before one turn per
/// actor (the live aos-pm shape: delivered `user` turns, no `reply_to`,
/// days old, never reported). A hot restart adopts them like any proven
/// turn; they read `awaiting_report`, and the actor's first pass retires
/// every overdue one to `unknown` through the report bound — one
/// `report_timeout` event each, agent fenced — nothing completed,
/// deleted or replayed, and a queued send behind them stays queued.
#[test]
fn pty_hot_restart_retires_stale_awaiting_report_rows() {
    let mut d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 20);
    let generation = agent["generation"].as_str().unwrap().to_string();
    let old = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        - 3.0 * 86_400.0;
    {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        for id in ["old1", "old2"] {
            conn.execute(
                "INSERT INTO messages(id,alias,body,reply_to,source,state,turn_id,
                     result,created,started)
                 VALUES(?1,'dv1','nudge',NULL,'user','running',?2,
                     '{\"status\":\"submitted\",\"ack\":null}',?3,?3)",
                rusqlite::params![id, format!("pty-{generation}-turn-{id}"), old],
            )
            .unwrap();
        }
        // F2: a row adopted before its `submitted` marker landed holds the
        // queue just the same — and is bounded just the same.
        conn.execute(
            "INSERT INTO messages(id,alias,body,reply_to,source,state,turn_id,
                 result,created,started)
             VALUES('old3','dv1','nudge',NULL,'user','running',?1,NULL,?2,?2)",
            rusqlite::params![format!("pty-{generation}-turn-old3"), old],
        )
        .unwrap();
    }
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    assert_eq!(
        read_marker(&state)["entries"].as_array().unwrap().len(),
        3,
        "every stale row is recorded like any running turn"
    );
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    wait_event_count(&d, "dv1", "turn_adopted", 3, 25);
    for id in ["old1", "old2", "old3"] {
        let m = d.wait_message("dv1", id, &["unknown"], 20);
        assert_eq!(m["result"]["via"], "report_timeout", "{m}");
    }
    d.wait_agent("dv1", "attention", 20);
    let timeouts = wait_event_count(&d, "dv1", "report_timeout", 3, 5);
    assert_eq!(timeouts.len(), 3);
    // Fenced, so a later send is held, never delivered to the pane.
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "after", "message": "new1"}),
    )
    .unwrap();
    assert_eq!(d.message_state("dv1", "new1"), "queued");
    let show = d.rpc("agent_show", json!({"alias": "dv1"})).unwrap();
    assert_eq!(show["unknown"], 3, "{show}");
    assert!(show["agent"]["awaiting_report"].is_null(), "{show}");
}

#[test]
fn pty_shutdown_facts_before_detach_rpc() {
    restart_idle_pty_after_forced_detach(false);
}

#[test]
fn pty_shutdown_facts_before_detach_signal() {
    // SIGTERM goes to a child process that hosts only this test's
    // daemon; the daemon's signal hook is what requests shutdown.
    run_signal_child(
        "pty_shutdown_facts_before_detach_signal_child",
        Duration::from_secs(180),
    );
}

#[test]
#[ignore = "run by pty_shutdown_facts_before_detach_signal in its own process"]
fn pty_shutdown_facts_before_detach_signal_child() {
    if std::env::var(SIGNAL_CHILD_ENV).as_deref()
        != Ok("pty_shutdown_facts_before_detach_signal_child")
    {
        return;
    }
    restart_idle_pty_after_forced_detach(true);
}

/// CAD-406 guard: the shared test process refuses to signal itself, and
/// no test sends itself a signal except through `sigterm_own_process`.
#[test]
fn test_suite_never_signals_the_shared_process() {
    assert!(self_signal_refusal(None).is_some());
    assert!(self_signal_refusal(Some("")).is_some());
    assert!(self_signal_refusal(Some("pty_x_child")).is_none());
    assert!(
        std::env::var_os(SIGNAL_CHILD_ENV).is_none(),
        "the suite itself must not carry {SIGNAL_CHILD_ENV}"
    );
    // Split so this test's own source does not match.
    let patterns = [
        concat!("kill(std::process", "::id()"),
        concat!("kill(libc::", "getpid()"),
        concat!("kill(0", ","),
        concat!("libc::", "raise("),
        concat!("low_level::", "raise("),
    ];
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut hits = Vec::new();
    let mut stack = vec![dir.clone()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            let flat: String = src.chars().filter(|c| !c.is_whitespace()).collect();
            for p in patterns {
                let n = flat.matches(p).count();
                if n > 0 {
                    hits.push(format!("{}: {p} x{n}", path.display()));
                }
            }
        }
    }
    hits.sort();
    let sanctioned = format!(
        "{}: {} x1",
        dir.join("common/mod.rs").display(),
        patterns[0]
    );
    assert_eq!(
        hits,
        vec![sanctioned],
        "self-signal outside sigterm_own_process would stop every concurrent \
         TestDaemon under plain cargo test (CAD-406)"
    );
}

#[test]
fn pty_shutdown_facts_before_detach_unprovable() {
    // Identity cleared before the snapshot is not inferred. Each
    // in-flight turn is refused by name; nothing is adopted or replayed.
    let mut d = TestDaemon::start();
    let mock = d.mock_devin();
    let (_token1, _token2, _pid) = park_idle_labelled_turns(&d, &mock);
    let screen_before = std::fs::read_to_string(d.pane_file(&mock, "dv1", "screen")).unwrap();
    {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE agents SET pid=NULL, generation=NULL, endpoint=NULL
             WHERE alias='dv1'",
            [],
        )
        .unwrap();
    }
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    let marker = read_marker(&state);
    assert!(
        marker["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["alias"].as_str() != Some("dv1")),
        "unprovable identity must not be recorded: {marker}"
    );
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    assert_eq!(d.message_state("dv1", "m2"), "unknown");
    assert_eq!(message_ids(&d, "dv1", "m1"), 1);
    assert_eq!(message_ids(&d, "dv1", "m2"), 1);
    assert!(
        !event_kinds(&d, "dv1").iter().any(|k| k == "turn_adopted"),
        "unprovable identity must not be adopted"
    );
    let refusals: Vec<Value> = d.rpc("agent_events", json!({"alias": "dv1"})).unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"].as_str() == Some("turn_adopt_refused"))
        .cloned()
        .collect();
    assert_eq!(refusals.len(), 2, "{refusals:?}");
    for id in ["m1", "m2"] {
        let reason = refusals
            .iter()
            .find(|e| e["payload"]["message"].as_str() == Some(id))
            .unwrap_or_else(|| panic!("missing refusal for {id}: {refusals:?}"));
        let text = reason["payload"]["reason"].as_str().unwrap_or("");
        assert!(
            text.contains("not provable") && text.contains("do not replay"),
            "{id}: {text}"
        );
    }
    let screen_after = std::fs::read_to_string(d.pane_file(&mock, "dv1", "screen")).unwrap();
    assert_eq!(
        screen_after, screen_before,
        "refusal must not replay a paste"
    );
}

#[test]
fn pty_hot_restart_token_predates_generation_fences() {
    // Facts are captured when shutdown is requested, before actors
    // detach, while RPC threads are still live: a resume that re-opened
    // the pane under a newer generation leaves old tokens stale forever. Recording
    // such a turn would roll the agent back to a dead generation —
    // `shutdown_entries` skips it and names the refusal instead.
    // Doctoring the row is the deterministic stand-in for that race.
    let mut d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    let _token = pty_token(&d, "dv1", "m1");
    // A "resume" that minted a newer generation than m1's token.
    {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE agents SET generation='gen-rewritten' WHERE alias='dv1'",
            [],
        )
        .unwrap();
    }
    d.rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    let marker = read_marker(&state);
    assert!(
        marker["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["message_id"].as_str() != Some("m1")),
        "a stale-generation turn must not be recorded: {marker}"
    );
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    let reason = adopt_refusal(&d, "dv1").expect("missing refusal");
    assert!(reason.contains("predates"), "{reason}");
}

#[test]
fn pty_hot_restart_fenced_agent_resume_opens_fresh() {
    // A kept turn on an agent that still fences at restart — a swept
    // sibling left it `unknown` — is orphaned, and its adoption entry
    // must be discarded with it. A later unfence+resume is an ordinary
    // open: a fresh generation, no `turn_adopted` for a reconciled
    // message. Otherwise the stale entry would let the resume reuse
    // the pre-restart generation for a turn already fenced `unknown`.
    let (state, _mock, _token, _pid) = stopped_mid_turn_devin();
    // A second in-flight turn the marker never recorded — inserted
    // `running` post-stop, the sweep makes it `unknown` and the
    // unknown fences the agent, stranding m1's kept entry.
    let generation = read_marker(&state)["entries"][0]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    {
        let conn = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "INSERT INTO messages(id,alias,body,reply_to,source,state,turn_id,
                 created)
             VALUES('m2','dv1','task',NULL,'test','running',?1,1.0)",
            rusqlite::params![format!("pty-{generation}-turn-m2")],
        )
        .unwrap();
    }
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    assert_eq!(d.message_state("dv1", "m2"), "unknown");
    let reason = adopt_refusal(&d, "dv1").expect("missing refusal");
    assert!(reason.contains("agent fenced"), "{reason}");
    // Unfence + resume: the pane is still alive so the open re-adopts
    // it by session ownership — but under a NEW generation, and with
    // no adoption event.
    d.operator_rpc(
        "agent_unfence",
        json!({"alias": "dv1", "status": "interrupted"}),
    )
    .unwrap();
    d.wait_agent("dv1", "stopped", 10);
    d.rpc("agent_resume", json!({"alias": "dv1"})).unwrap();
    let agent = d.wait_agent("dv1", "idle", 25);
    assert_ne!(
        agent["generation"].as_str().unwrap(),
        generation,
        "resume after a restart fence must mint a fresh generation"
    );
    let kinds = event_kinds(&d, "dv1");
    assert!(
        !kinds.iter().any(|k| k == "turn_adopted"),
        "no adoption may fire for a discarded entry: {kinds:?}"
    );
}

#[test]
fn pty_hot_restart_divergent_marker_entries_fence() {
    // One pane proof covers a whole alias list — every entry must
    // describe the same endpoint facts. `shutdown_entries` writes one
    // facts tuple per alias, so a list whose records disagree can only
    // come from a hand-built or corrupt marker: the whole list refuses
    // and the turns fence like any other unprovable restart.
    let (state, _mock, _token, _pid) = stopped_mid_turn_devin();
    // m2 is a real `running` turn that would qualify on its own — its
    // marker entry carries a DIFFERENT pane_pid than m1's.
    let marker = read_marker(&state);
    let generation = marker["entries"][0]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    {
        let conn = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "INSERT INTO messages(id,alias,body,reply_to,source,state,turn_id,
                 created)
             VALUES('m2','dv1','task',NULL,'test','running',?1,1.0)",
            rusqlite::params![format!("pty-{generation}-turn-m2")],
        )
        .unwrap();
    }
    let mut marker = marker;
    let mut entry2 = marker["entries"][0].clone();
    entry2["message_id"] = json!("m2");
    entry2["turn_id"] = json!(format!("pty-{generation}-turn-m2"));
    entry2["pane_pid"] = json!(1);
    marker["entries"].as_array_mut().unwrap().push(entry2);
    std::fs::write(state.join("shutdown.json"), marker.to_string()).unwrap();
    let d = TestDaemon::start_on(state);
    d.wait_agent("dv1", "attention", 20);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    assert_eq!(d.message_state("dv1", "m2"), "unknown");
    let reason = adopt_refusal(&d, "dv1").expect("missing refusal");
    assert!(reason.contains("disagree"), "{reason}");
}

#[test]
fn pty_dead_pane_fences_submitted_and_stops_actor() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    pty_token(&d, "dv1", "m1");

    // Kill the pane's whole process group: the submitted message can no
    // longer be confirmed — it must go `unknown`, never silently replay.
    let pid: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    unsafe { libc::killpg(pid, libc::SIGKILL) };
    let agent = d.wait_agent("dv1", "attention", 20);
    // This kill is submitted-pane loss, not a session-lock refusal, so
    // the fence must keep that account plus the inspect guidance. A
    // lock substring is not an alternate success for this path.
    let error = agent["error"].as_str().unwrap_or("");
    assert!(error.contains("endpoint lost after submission"), "{agent}");
    assert!(error.contains("does not prove"), "{error}");
    assert!(!error.contains("then `cadence agent resume"), "{error}");
    d.wait_message("dv1", "m1", &["unknown"], 15);
}

#[test]
fn pty_restart_reattaches_same_native_session() {
    let dir = TempDir::new().unwrap();
    let seeded = dir.path().join("state");
    std::fs::create_dir_all(&seeded).unwrap();
    let fixtures = TempDir::new().unwrap();
    {
        let _mock = install_mock_devin(fixtures.path());
        let d = TestDaemon::start_on(seeded.clone());
        d.register_devin("dv1", None);
        let agent = d.wait_agent("dv1", "idle", 20);
        let native = agent["thread_id"].as_str().unwrap().to_string();
        let pane_pid = agent["pid"].as_i64().unwrap();
        // Daemon restart: the mock tmux server (fixture dir) outlives it,
        // so the pane is still alive and must be reattached, not relaunched.
        drop(d);
        let d2 = TestDaemon::start_on(seeded.clone());
        let agent2 = d2.wait_agent("dv1", "idle", 25);
        assert_eq!(agent2["thread_id"].as_str().unwrap(), native);
        assert_eq!(agent2["pid"].as_i64().unwrap(), pane_pid);
    }
}

#[test]
fn pty_stop_kills_only_owned_session() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    let pidfile = d.pane_file(&mock, "dv1", "pid");
    d.rpc("agent_stop", json!({"alias": "dv1"})).unwrap();
    d.wait_agent("dv1", "stopped", 15);
    wait_pid_gone(&pidfile, 10);
}

#[test]
fn pty_unrendered_task_fences_unknown() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);
    // The pane drops the paste entirely (TUI swallowed the input).
    atomic_write(d.pane_file(&mock, "dv", "swallow"), "1");
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "task that vanishes", "message": "m1"}),
    )
    .unwrap();
    // A provable paste miss on a task message: unknown + attention —
    // never a blind replay.
    d.wait_message("dv", "m1", &["unknown"], 20);
    d.wait_agent("dv", "attention", 15);
    let e = d.wait_event("dv", "paste_not_rendered", 5);
    assert_eq!(e["payload"]["message"], "m1");
    assert_eq!(e["payload"]["retry"], false, "{e}");
}

/// A fence detaches the pane — never kills it. The surviving pane
/// stays inspectable and `agent resume` re-adopts it after the
/// operator reconciles: same pid, same native session.
#[test]
fn pty_fence_detaches_pane_for_resume() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 20);
    let native = agent["thread_id"].as_str().unwrap().to_string();
    let pidfile = d.pane_file(&mock, "dv1", "pid");
    let pane_pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // The pane drops the paste entirely: unrendered → fence.
    atomic_write(d.pane_file(&mock, "dv1", "swallow"), "1");
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    d.wait_message("dv1", "m1", &["unknown"], 20);
    d.wait_agent("dv1", "attention", 15);
    // The fence detached — the pane process and its pid file are
    // unchanged, and the screen is still there to inspect.
    assert_eq!(
        unsafe { libc::kill(pane_pid, 0) },
        0,
        "fence killed the pane"
    );
    let pid_now: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pid_now, pane_pid);
    // Reconcile, then resume: the surviving pane is re-adopted.
    d.operator_rpc(
        "agent_unfence",
        json!({"alias": "dv1", "status": "interrupted"}),
    )
    .unwrap();
    d.wait_agent("dv1", "stopped", 10);
    std::fs::remove_file(d.pane_file(&mock, "dv1", "swallow")).unwrap();
    d.rpc("agent_resume", json!({"alias": "dv1"})).unwrap();
    let agent = d.wait_agent("dv1", "idle", 20);
    let pid_after: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pid_after, pane_pid, "pane was not re-adopted");
    assert_eq!(agent["thread_id"].as_str().unwrap(), native);
    // New work flows over the adopted pane.
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "again", "message": "m2"}),
    )
    .unwrap();
    d.wait_message("dv1", "m2", &["running"], 20);
}

/// Explicit lifecycle verbs own the kill: `agent stop`, `agent remove`,
/// and `agent gc` each kill a fenced agent's surviving pane — the row
/// never drops leaving an orphan session on the private socket.
#[test]
fn pty_stop_remove_gc_kill_surviving_panes() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    for alias in ["dv-stop", "dv-rm", "dv-gc"] {
        d.register_devin(alias, None);
        d.wait_agent(alias, "idle", 20);
    }
    // Fence all three: each pane survives its fence for inspection.
    for alias in ["dv-stop", "dv-rm", "dv-gc"] {
        atomic_write(d.pane_file(&mock, alias, "swallow"), "1");
        d.rpc("agent_ready", json!({"alias": alias})).unwrap();
        d.rpc(
            "agent_send",
            json!({"alias": alias, "text": "task",
                   "message": format!("m-{alias}")}),
        )
        .unwrap();
    }
    for alias in ["dv-stop", "dv-rm", "dv-gc"] {
        d.wait_agent(alias, "attention", 25);
    }
    // `agent stop` on a fenced agent is the explicit kill.
    d.rpc("agent_stop", json!({"alias": "dv-stop"})).unwrap();
    wait_pid_gone(&d.pane_file(&mock, "dv-stop", "pid"), 10);
    // The fence's `unknown` message is open work (CAD-284): `agent
    // remove` — even `--force` — refuses and leaves the pane alone;
    // once reconciled, remove drops the row and kills the pane.
    let err = d
        .operator_rpc("agent_remove", json!({"alias": "dv-rm"}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("message m-dv-rm (unknown)"), "{err}");
    let err = d
        .operator_rpc("agent_remove", json!({"alias": "dv-rm", "force": true}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("message reconcile m-dv-rm"), "{err}");
    assert!(pid_alive(&d.pane_file(&mock, "dv-rm", "pid")));
    d.operator_rpc(
        "message_reconcile",
        json!({"message": "m-dv-rm", "status": "interrupted"}),
    )
    .unwrap();
    d.operator_rpc("agent_remove", json!({"alias": "dv-rm"}))
        .unwrap();
    wait_pid_gone(&d.pane_file(&mock, "dv-rm", "pid"), 10);
    assert!(d.rpc("agent_show", json!({"alias": "dv-rm"})).is_err());
    // `agent gc` never forces: it skips the fenced agent until its
    // unknown is reconciled, then kills the pane with the row.
    let swept = d.operator_rpc("agent_gc", json!({})).unwrap();
    assert!(
        !swept["removed"]
            .as_array()
            .unwrap()
            .contains(&json!("dv-gc")),
        "{swept}"
    );
    assert!(pid_alive(&d.pane_file(&mock, "dv-gc", "pid")));
    d.operator_rpc(
        "message_reconcile",
        json!({"message": "m-dv-gc", "status": "interrupted"}),
    )
    .unwrap();
    let swept = d.operator_rpc("agent_gc", json!({})).unwrap();
    assert!(
        swept["removed"]
            .as_array()
            .unwrap()
            .contains(&json!("dv-gc")),
        "{swept}"
    );
    wait_pid_gone(&d.pane_file(&mock, "dv-gc", "pid"), 10);
    assert!(d.rpc("agent_show", json!({"alias": "dv-gc"})).is_err());
}

/// `agent_unfence` with `resume: true` reconciles and resumes in one
/// call and reports what the endpoint actually did. A surviving pane
/// is adopted — same pid, same native session.
#[test]
fn pty_unfence_resume_reports_adopted_pane() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 20);
    let native = agent["thread_id"].as_str().unwrap().to_string();
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "task", "message": "m1"}),
    )
    .unwrap();
    pty_report_done(&d, "dv1", "m1");
    let pane_pid: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Fence: the paste never renders; the pane is detached, not killed.
    atomic_write(d.pane_file(&mock, "dv1", "swallow"), "1");
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "dropped", "message": "m2"}),
    )
    .unwrap();
    d.wait_agent("dv1", "attention", 25);
    // One call reconciles the unknown and brings the agent back.
    let r = d
        .operator_rpc(
            "agent_unfence",
            json!({"alias": "dv1", "status": "interrupted",
                   "resume": true}),
        )
        .unwrap();
    assert_eq!(r["reconciled"], json!(["m2"]), "{r}");
    assert_eq!(r["resumed"], true, "{r}");
    assert_eq!(r["pane"], "adopted", "{r}");
    let agent = d.wait_agent("dv1", "idle", 20);
    let pid_after: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pid_after, pane_pid, "adopted pane changed pid");
    assert_eq!(agent["thread_id"].as_str().unwrap(), native);
}

/// The same call reports `respawned` when no pane survived: a new
/// pane is launched on the recorded session — new pid, same native
/// thread.
#[test]
fn pty_unfence_resume_reports_respawned_pane() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    let agent = d.wait_agent("dv1", "idle", 20);
    let native = agent["thread_id"].as_str().unwrap().to_string();
    let pane_pid: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    atomic_write(d.pane_file(&mock, "dv1", "swallow"), "1");
    d.rpc("agent_ready", json!({"alias": "dv1"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "dropped", "message": "m1"}),
    )
    .unwrap();
    d.wait_agent("dv1", "attention", 25);
    // The detached pane dies out-of-band before the resume — nothing
    // left to adopt.
    std::process::Command::new(mock.dir.join("tmux"))
        .args(["-L", &socket_for(&d.state), "kill-session", "-t", "dv1"])
        .env("MOCK_TMUX_STATE", mock.dir.join("tmux-state"))
        .output()
        .unwrap();
    wait_pid_gone(&d.pane_file(&mock, "dv1", "pid"), 10);
    let r = d
        .operator_rpc(
            "agent_unfence",
            json!({"alias": "dv1", "status": "interrupted",
                   "resume": true}),
        )
        .unwrap();
    assert_eq!(r["resumed"], true, "{r}");
    assert_eq!(r["pane"], "respawned", "{r}");
    let agent = d.wait_agent("dv1", "idle", 20);
    let pid_after: i32 = std::fs::read_to_string(d.pane_file(&mock, "dv1", "pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_ne!(pid_after, pane_pid, "respawn kept the dead pid");
    assert_eq!(agent["thread_id"].as_str().unwrap(), native);
}

/// Adopted does not mean idle: a surviving pane that is visibly busy
/// is still adopted by resume, but sends stay gated until the screen
/// probe sees it idle.
#[test]
fn pty_unfence_resume_busy_adopted_pane_stays_gated() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    d.register_stub("st", json!({"auto_ready": "verified"}));
    d.wait_agent("st", "idle", 20);
    // Fence; the pane survives detached.
    atomic_write(d.stub_pane_file(&mock, "st", "swallow"), "1");
    d.rpc("agent_ready", json!({"alias": "st"})).unwrap();
    d.rpc(
        "agent_send",
        json!({"alias": "st", "text": "dropped", "message": "m1"}),
    )
    .unwrap();
    d.wait_agent("st", "attention", 25);
    // The surviving pane is visibly busy — adoption still lands.
    atomic_write(d.stub_pane_file(&mock, "st", "tui-state"), "stub working\n");
    let r = d
        .operator_rpc(
            "agent_unfence",
            json!({"alias": "st", "status": "interrupted",
                   "resume": true}),
        )
        .unwrap();
    assert_eq!(r["resumed"], true, "{r}");
    assert_eq!(r["pane"], "adopted", "{r}");
    d.wait_agent("st", "idle", 20);
    // A send without ready must not paste into the busy pane.
    d.rpc(
        "agent_send",
        json!({"alias": "st", "text": "wait for idle", "message": "m2"}),
    )
    .unwrap();
    let wait = d.wait_event("st", "gate_wait", 15);
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("busy"),
        "{wait}"
    );
    assert_eq!(d.message_state("st", "m2"), "queued");
    // Operator clears the pane: busy marker off, the swallowed draft
    // out of the input line, the pane accepting pastes again — then
    // the queued send delivers.
    std::fs::remove_file(d.stub_pane_file(&mock, "st", "tui-state")).unwrap();
    std::fs::remove_file(d.stub_pane_file(&mock, "st", "swallow")).unwrap();
    atomic_write(d.stub_pane_file(&mock, "st", "input"), "");
    pty_token(&d, "st", "m2");
}

/// The registry claiming a different native session than the one we
/// asked for is a changed-owner fence, never an adoption.
#[test]
fn pty_claude_session_mismatch_fences_closed() {
    let d = TestDaemon::start();
    let _mock = d.mock_claude_tui();
    // Real env, under the TUI mock's ENV_LOCK: the pane's mock process
    // reads the swap knob from the env the daemon passes to tmux.
    std::env::set_var("MOCK_CLAUDE_SWAP", "1");
    d.register_claude_pty("cl", json!({"session": "want-session"}));
    let agent = d.wait_agent("cl", "attention", 20);
    let err = agent["error"].as_str().unwrap_or("");
    assert!(err.contains("changed owner fails closed"), "{err}");
}

/// The pane opening a different chat than the one we asked for is a
/// changed-owner fence, never an adoption.
#[test]
fn pty_cursor_session_mismatch_fences_closed() {
    let d = TestDaemon::start();
    let _mock = d.mock_cursor_tui();
    // Real env, under the TUI mock's ENV_LOCK: the pane's mock process
    // reads the swap knob from the env the daemon passes to tmux.
    std::env::set_var("MOCK_CURSOR_SWAP", "1");
    d.register_cursor_pty("cu", json!({"session": "want-chat"}));
    let agent = d.wait_agent("cu", "attention", 20);
    let err = agent["error"].as_str().unwrap_or("");
    assert!(err.contains("changed owner fails closed"), "{err}");
}

/// A pane that survives a daemon restart is re-adopted under the same
/// chat — the surviving store.db fd is the ownership proof again.
#[test]
fn pty_cursor_restart_readopts_pane() {
    let dir = TempDir::new().unwrap();
    let seeded = dir.path().join("state");
    std::fs::create_dir_all(&seeded).unwrap();
    let fixtures = TempDir::new().unwrap();
    {
        let _mock = install_mock_cursor_tui(fixtures.path());
        let d = TestDaemon::start_on(seeded.clone());
        d.register_cursor_pty("cu", json!({}));
        let agent = d.wait_agent("cu", "idle", 20);
        let native = agent["thread_id"].as_str().unwrap().to_string();
        let pane_pid = agent["pid"].as_i64().unwrap();
        // Daemon restart: the mock tmux server (fixture dir) outlives
        // it, so the pane is still alive and must be reattached, not
        // relaunched.
        drop(d);
        let d2 = TestDaemon::start_on(seeded.clone());
        let agent2 = d2.wait_agent("cu", "idle", 25);
        assert_eq!(agent2["thread_id"].as_str().unwrap(), native);
        assert_eq!(agent2["pid"].as_i64().unwrap(), pane_pid);
    }
}

// ==== CAD-201 / CAD-202: pty lane process tree and cwd integrity ====

/// `(state, sid, start_time)` from `/proc/<pid>/stat` — `None` once the
/// pid is gone.
fn lane_stat(pid: u32) -> Option<(char, u32, u64)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let end = text.rfind(')')?;
    let f: Vec<&str> = text[end + 1..].split_whitespace().collect();
    Some((
        f.first()?.chars().next()?,
        f.get(3)?.parse().ok()?,
        f.get(19)?.parse().ok()?,
    ))
}

/// Live (non-zombie) pids whose session id is `sid`.
fn lane_session_pids(sid: u32) -> Vec<u32> {
    std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().parse::<u32>().ok())
        .filter(|&pid| lane_stat(pid).is_some_and(|(state, s, _)| s == sid && state != 'Z'))
        .collect()
}

/// Test-owned `sleep` children, SIGKILLed on drop if a failed assertion
/// left them running — matched by pid + start time, never pid alone.
struct LaneSleepers(Vec<(u32, u64)>);

impl Drop for LaneSleepers {
    fn drop(&mut self) {
        for &(pid, start) in &self.0 {
            if lane_stat(pid).is_some_and(|(state, _, s)| s == start && state != 'Z') {
                unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            }
        }
    }
}

fn wait_pid_file(path: &Path, secs: u64) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(pid) = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// CAD-201: a pty lane records its pane root (pid + start time +
/// session id + generation) at open and `agent stop` reaps the pane's
/// session after the pane is killed: SIGTERM, a bounded drain, a
/// re-sample by identity and SIGKILL only for matching survivors.
/// The pane spawns two children into their own process groups (so
/// killing the pane's group misses them, as with real MCP servers):
/// one exits on SIGTERM, one ignores it and needs the SIGKILL. A
/// process outside the session is untouched.
#[test]
fn pty_stop_reaps_pane_session_tree() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    test_env().set("CADENCE_PTY_DRAIN_SECS", "1.5");
    let kids = d.dir.path().join("kids");
    std::fs::create_dir_all(&kids).unwrap();
    // `set -m` gives each background job its own process group inside
    // the pane's session; the spawning shell then exits, so both
    // children are reparented — exactly the escaped-MCP-child shape.
    let spawn = format!(
        "bash -c 'set -m; sleep 600 & echo $! > {k}/term.pid; \
         (trap \"\" TERM; exec sleep 601) & echo $! > {k}/kill.pid'; \
         python3 {stub} {locks}",
        k = kids.display(),
        stub = mock.dir.join("mock-stub.py").display(),
        locks = mock.locks.display(),
    );
    test_env().set("CADENCE_STUB_COMMAND", spawn);
    // Unrelated process: the test's own child, outside the pane session.
    let mut outsider = std::process::Command::new("sleep")
        .arg("600")
        .spawn()
        .unwrap();
    let outsider_start = lane_stat(outsider.id()).unwrap().2;
    let mut guard = LaneSleepers(vec![(outsider.id(), outsider_start)]);

    d.register_stub("st", json!({}));
    let agent = d.wait_agent("st", "idle", 20);
    let root = agent["pane_root"].clone();
    assert_eq!(root["session_leader"], true, "{agent}");
    assert_eq!(root["current"], true, "{agent}");
    assert_eq!(root["generation"], agent["generation"], "{agent}");
    let sid = root["sid"].as_u64().unwrap() as u32;
    assert_eq!(root["pid"].as_u64().unwrap() as u32, sid, "{agent}");
    assert_eq!(
        lane_stat(sid).unwrap().2,
        root["start_time"].as_u64().unwrap(),
        "{agent}"
    );
    let term_pid = wait_pid_file(&kids.join("term.pid"), 10);
    let kill_pid = wait_pid_file(&kids.join("kill.pid"), 10);
    for pid in [term_pid, kill_pid] {
        let (_, s, start) = lane_stat(pid).expect("child alive");
        assert_eq!(s, sid, "child {pid} must be in the pane session");
        guard.0.push((pid, start));
    }
    let recorded = d.wait_event("st", "pane_root", 5);
    assert_eq!(recorded["payload"]["sid"], root["sid"], "{recorded}");

    d.rpc("agent_stop", json!({"alias": "st"})).unwrap();
    d.wait_agent("st", "stopped", 15);
    let intent = d.wait_event("st", "pane_tree_reap_intent", 15);
    let members: Vec<u64> = intent["payload"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["pid"].as_u64().unwrap())
        .collect();
    assert!(
        members.contains(&(term_pid as u64)) && members.contains(&(kill_pid as u64)),
        "{intent}"
    );
    let reaped = d.wait_event("st", "pane_tree_reaped", 20);
    let pids = |key: &str| -> Vec<u64> {
        reaped["payload"][key]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["pid"].as_u64().unwrap())
            .collect()
    };
    assert!(reaped["payload"]["refused"].is_null(), "{reaped}");
    assert!(pids("terminated").contains(&(term_pid as u64)), "{reaped}");
    assert!(pids("exited").contains(&(term_pid as u64)), "{reaped}");
    // Only the SIGTERM-ignoring child needed the SIGKILL.
    assert_eq!(pids("killed"), vec![kill_pid as u64], "{reaped}");
    assert!(pids("residue").is_empty(), "{reaped}");
    // Nothing from the pane's session survives the drain…
    let deadline = Instant::now() + Duration::from_secs(5);
    while !lane_session_pids(sid).is_empty() {
        assert!(
            Instant::now() < deadline,
            "session {sid} survivors: {:?}",
            lane_session_pids(sid)
        );
        thread::sleep(Duration::from_millis(50));
    }
    // …and the process outside it is untouched.
    assert!(
        lane_stat(outsider.id()).is_some_and(|(state, _, s)| s == outsider_start && state != 'Z'),
        "the outside process must survive"
    );

    // A second stop is idempotent: the tree is already reaped.
    d.rpc("agent_stop", json!({"alias": "st"})).unwrap();
    // CAD-184 kept sleep: absence window — a declined reap records
    // nothing; a started one would land on its own thread.
    thread::sleep(Duration::from_millis(300));
    let intents = d
        .events("st")
        .into_iter()
        .filter(|e| e["kind"] == "pane_tree_reap_intent")
        .count();
    assert_eq!(intents, 1);

    let _ = outsider.kill();
    let _ = outsider.wait();
    drop(guard);
    test_env().remove("CADENCE_PTY_DRAIN_SECS");
}

/// CAD-201: an agent with no recorded pane root (opened before the
/// identity was recorded) is never reaped — the stop records that the
/// tree is unowned and signals nothing beyond the pane.
#[test]
fn pty_stop_without_pane_root_records_unowned_tree() {
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    {
        let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
        let cwd = state.to_str().unwrap().to_string();
        store
            .register_agent(&NewAgent {
                alias: "old",
                provider: "tui-stub",
                endpoint_kind: "pty",
                role: "worker",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        store.set_enabled("old", false).unwrap();
        store.set_agent_state("old", "stopped", None).unwrap();
    }
    let d = TestDaemon::start_on(state);
    let _mock = d.mock_stub();
    d.rpc("agent_stop", json!({"alias": "old"})).unwrap();
    let e = d.wait_event("old", "pane_tree_unowned", 10);
    assert!(
        e["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("no pane root identity"),
        "{e}"
    );
    assert!(d
        .events("old")
        .iter()
        .all(|e| e["kind"] != "pane_tree_reap_intent"));
}

/// CAD-202: a pty pane whose cwd was deleted refuses delivery at the
/// gate with a named reason — the message stays queued, never failed —
/// and `agent show` and `status` surface `cwd_deleted`.
#[test]
fn pty_deleted_cwd_refuses_delivery_and_is_surfaced() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    let lane = d.dir.path().join("lane-wt");
    std::fs::create_dir_all(&lane).unwrap();
    d.rpc(
        "agent_register",
        json!({"alias": "st", "provider": "tui-stub", "endpoint_kind": "pty",
               "cwd": lane.to_str().unwrap(),
               "params": json!({"auto_ready": "verified"}).to_string()}),
    )
    .unwrap();
    let agent = d.wait_agent("st", "idle", 20);
    assert_eq!(agent["cwd_deleted"], false, "{agent}");
    assert_eq!(
        agent["pane_cwd"]["path"].as_str().unwrap(),
        lane.canonicalize().unwrap().to_str().unwrap(),
        "{agent}"
    );
    // The worktree is removed under the live pane.
    std::fs::remove_dir_all(&lane).unwrap();
    let agent = d.rpc("agent_show", json!({"alias": "st"})).unwrap()["agent"].clone();
    assert_eq!(agent["cwd_deleted"], true, "{agent}");
    assert_eq!(agent["pane_cwd"]["deleted"], true, "{agent}");

    d.rpc(
        "agent_send",
        json!({"alias": "st", "text": "work here", "message": "m1"}),
    )
    .unwrap();
    let wait = d.wait_event("st", "gate_wait", 15);
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .starts_with("cwd_deleted:"),
        "{wait}"
    );
    assert_eq!(d.message_state("st", "m1"), "queued");

    let tmp = TempDir::new().unwrap();
    let (pm_dir, home) = (tmp.path().join("pm"), tmp.path().join("home"));
    std::fs::create_dir_all(&home).unwrap();
    let (ok, view) = lane_cli(&d, &pm_dir, &home, &["status", "--json"]);
    assert!(ok, "{view}");
    let row = view["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "st")
        .cloned()
        .unwrap();
    assert_eq!(row["cwd_deleted"], true, "{row}");
    // Still queued after the status probe — refusal, not failure.
    assert_eq!(d.message_state("st", "m1"), "queued");
}
