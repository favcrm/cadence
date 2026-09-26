//! pty_interact: area tests split from tests/integration.rs (CAD-426).
//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use serde_json::json;
use serde_json::Value;
use std::path::Path;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;

/// CAD-250: `--nudge` is pty-only and caller-rule neutral — refused on a
/// managed endpoint and a mailbox (naming the provider kind), with a
/// `reply_to`, and together with `--ready`.
#[test]
fn nudge_refused_off_pty_and_with_ready() {
    let d = TestDaemon::start();
    d.register("mgd");
    d.register_inbox("box");
    d.wait_agent("mgd", "idle", 10);
    for (alias, kind) in [("mgd", "fake/fake"), ("box", "inbox/inbox")] {
        let err = d
            .send(alias, json!({"text": "steer", "nudge": true}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("pty") && err.contains(kind), "{alias}: {err}");
        // The forged-source path takes the same check.
        let err = d
            .send(alias, json!({"text": "steer", "source": "nudge"}))
            .unwrap_err()
            .to_string();
        assert!(err.contains(kind), "{alias}: {err}");
    }
    let err = d
        .send(
            "mgd",
            json!({"text": "steer", "nudge": true, "reply_to": "box"}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("reply_to") || err.contains("pty"), "{err}");
    // A clap conflict: refused before any RPC, error names both flags.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["send", "mgd", "--nudge", "--ready", "--text", "steer"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "--nudge with --ready must be refused"
    );
    assert!(
        stderr.contains("--nudge") && stderr.contains("--ready"),
        "{stderr}"
    );
    // Nothing was enqueued by any refusal.
    for alias in ["mgd", "box"] {
        let show = d.rpc("agent_show", json!({"alias": alias})).unwrap();
        assert!(
            show["messages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["body"] != "steer"),
            "{show}"
        );
    }
}

/// An open approval menu refuses a routed notice the same way it
/// refuses a claimed paste. Reuses the Devin menu fixture.
#[test]
fn pty_routed_notice_waits_on_approval_menu() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("pm", None);
    d.register("w1");
    d.wait_agent("pm", "idle", 20);
    d.wait_agent("w1", "idle", 10);
    atomic_write(d.pane_file(&mock, "pm", "tui-state"), DEVIN_MENU);
    let routed_id = route_worker_result(&d, "w1", "pm", "work-menu", "notice under menu");
    let wait = d.wait_event_where(
        "pm",
        "gate_wait",
        |event| event["payload"]["message"] == routed_id,
        10,
    );
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("approval menu"),
        "{wait}"
    );
    assert_eq!(d.message_state("pm", &routed_id), "queued");
    assert!(std::fs::read_to_string(d.pane_file(&mock, "pm", "input"))
        .unwrap_or_default()
        .is_empty());
}

/// A running turn on a static pty screen raises `turn_stalled` exactly
/// once, flags the agent views, and sends one `worker_notice` to the
/// message's `reply_to` — the turn itself is never touched.
#[test]
fn pty_stall_static_screen_fires_once_and_notices() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    stall_sample(2);
    d.register_inbox("pm");
    d.register_stub("w1", json!({"auto_ready": "verified", "stall_secs": 2}));
    d.wait_agent("w1", "idle", 20);
    d.send(
        "w1",
        json!({"text": "do work", "reply_to": "pm", "message": "ms1"}),
    )
    .unwrap();
    d.wait_message("w1", "ms1", &["running"], 15);

    let e = d.wait_event("w1", "turn_stalled", 30);
    assert_eq!(e["payload"]["message"], "ms1", "{e}");
    assert!(
        e["payload"]["silent_secs"].as_u64().unwrap_or(0) >= 2,
        "{e}"
    );
    assert!(
        e["payload"]["last_activity"].as_f64().unwrap_or(0.0) > 0.0,
        "{e}"
    );

    // Once per episode: the silence continues but no second event fires.
    // CAD-184 kept sleep: timing is the behaviour (stall ticks over time).
    thread::sleep(Duration::from_secs(7));
    assert_eq!(wait_event_count(&d, "w1", "turn_stalled", 1, 2).len(), 1);

    // One `worker_notice` went to reply_to — fire-and-forget, so the
    // mailbox holds it with no reply path of its own.
    let notices = wait_source(&d, "pm", "worker_notice", 1, 10);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0]["reply_to"].is_null(), "{notices:?}");
    let body = notices[0]["body"].as_str().unwrap_or("");
    assert!(
        body.contains("w1") && body.contains("no activity"),
        "{body}"
    );

    // The views carry the flag — and the turn is still running:
    // stalling is informational, never an interruption.
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["stalled"], true, "{agent}");
    assert!(agent["silent_secs"].as_u64().unwrap_or(0) >= 2, "{agent}");
    assert_eq!(d.message_state("w1", "ms1"), "running");
    let list = d.rpc("agent_list", json!({})).unwrap()["agents"].clone();
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"].as_str() == Some("w1"))
        .cloned()
        .unwrap();
    assert_eq!(row["stalled"], true, "{row}");

    // A real result still lands normally after the notice.
    let token = pty_token(&d, "w1", "ms1");
    d.report("ms1", &token, "result", "done").unwrap();
    d.wait_message("w1", "ms1", &["completed"], 10);
    stall_sample(0);
}

/// A stalled turn resumes on real screen motion and re-arms: a second
/// silence raises a second `turn_stalled`. A ticking status counter
/// (`· Ns`) alone is not activity — it must never resume.
#[test]
fn pty_stall_resume_rearms_and_spinner_is_not_activity() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    stall_sample(2);
    d.register_inbox("pm");
    d.register_stub("w1", json!({"auto_ready": "verified", "stall_secs": 2}));
    d.wait_agent("w1", "idle", 20);
    d.send(
        "w1",
        json!({"text": "do work", "reply_to": "pm", "message": "ms2"}),
    )
    .unwrap();
    d.wait_message("w1", "ms2", &["running"], 15);
    // The status line exists from the baseline sample on — only its
    // counter will move.
    atomic_write(
        d.stub_pane_file(&mock, "w1", "tui-state"),
        "⠋ Working · 1s\n",
    );
    d.wait_event("w1", "turn_stalled", 30);

    // The counter ticks past several samples — normalized identically,
    // so nothing resumes.
    for i in 2..=4u64 {
        atomic_write(
            d.stub_pane_file(&mock, "w1", "tui-state"),
            format!("⠋ Working · {i}s\n"),
        );
        // CAD-184 kept sleep: timing is the behaviour (the counter must tick
        // across screen samples).
        thread::sleep(Duration::from_secs(2));
    }
    assert!(
        d.events("w1")
            .iter()
            .all(|e| e["kind"].as_str() != Some("turn_resumed")),
        "spinner ticking resumed the turn: {:?}",
        d.events("w1")
    );

    // Real transcript motion resumes — same recipient gets the resolved
    // notice — and detection re-arms for the next silence.
    atomic_write(
        d.stub_pane_file(&mock, "w1", "tui-state"),
        "⠋ Working · 5s\nBUILD OK\n",
    );
    let e = d.wait_event("w1", "turn_resumed", 15);
    assert_eq!(e["payload"]["message"], "ms2", "{e}");
    wait_event_count(&d, "w1", "turn_stalled", 2, 20);
    assert_eq!(d.message_state("w1", "ms2"), "running");

    let notices = wait_source(&d, "pm", "worker_notice", 3, 10);
    let stalls = notices
        .iter()
        .filter(|m| m["body"].as_str().unwrap_or("").contains("no activity"))
        .count();
    let resumes = notices
        .iter()
        .filter(|m| m["body"].as_str().unwrap_or("").contains("active again"))
        .count();
    assert_eq!((stalls, resumes), (2, 1), "{notices:?}");
    stall_sample(0);
}

/// Screen activity is debounced: a hash seen for exactly one sample —
/// a capture taken mid-repaint — can neither reset the silence clock
/// nor resume a stalled turn. The mock's one-shot `tui-once` frame
/// pins the empty tail to exactly one sighting — one capture claims
/// it by rename, so no write timing can show it twice (CAD-451: the
/// old write-then-revert protocol raced the capture's read). Real
/// persistent motion still resumes, one interval later.
#[test]
fn pty_stall_transient_sample_neither_resumes_nor_resets() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    stall_sample(1);
    d.register_inbox("pm");
    d.register_stub("w1", json!({"auto_ready": "verified", "stall_secs": 8}));
    d.wait_agent("w1", "idle", 20);
    d.send(
        "w1",
        json!({"text": "do work", "reply_to": "pm", "message": "mtr"}),
    )
    .unwrap();
    d.wait_message("w1", "mtr", &["running"], 15);
    atomic_write(d.stub_pane_file(&mock, "w1", "tui-state"), "⠋ Working\n");

    let captures = || {
        std::fs::read_to_string(d.stub_pane_file(&mock, "w1", "captures"))
            .unwrap_or_default()
            .len()
    };
    let wait_capture = |from: usize| {
        let deadline = Instant::now() + Duration::from_secs(15);
        while captures() <= from {
            assert!(Instant::now() < deadline, "no capture landed");
            thread::sleep(Duration::from_millis(30));
        }
    };
    let silent = || {
        d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"]["silent_secs"]
            .as_u64()
            .unwrap_or(0)
    };
    // `contents` is visible to exactly one sample: the capture that
    // claims the one-shot frame. Once it is claimed, that capture has
    // ticked the counter, so the count read then already includes it.
    // The ticker starts a capture only after the previous sample
    // landed and was folded in, so two more captures starting proves
    // the transient sample and one restored sample after it were both
    // folded — and the tick that folded the transient has finished
    // its events.
    let transient = |contents: &str| {
        let once = d.stub_pane_file(&mock, "w1", "tui-once");
        atomic_write(once.clone(), contents);
        let deadline = Instant::now() + Duration::from_secs(15);
        while once.exists() {
            assert!(Instant::now() < deadline, "no capture claimed the frame");
            thread::sleep(Duration::from_millis(30));
        }
        wait_capture(captures() + 1);
    };

    // Let the baseline settle: captures n0 + 1 and n0 + 2 started after
    // the write above, so both saw this screen, and n0 + 3 starting
    // proves both were folded in — however the samples before them
    // straddled the write.
    let n0 = captures();
    wait_capture(n0 + 2);
    // An empty tail for one sample must not reset the silence clock.
    let before = silent();
    transient("");
    let after = silent();
    assert!(
        after > before,
        "one-sample transient reset the silence clock: {before} -> {after}"
    );

    // The stall fires on budget — the clock kept accruing.
    d.wait_event("w1", "turn_stalled", 15);

    // Mid-stall, the same one-sample transient cannot resume.
    transient("");
    assert!(
        d.events("w1")
            .iter()
            .all(|e| e["kind"].as_str() != Some("turn_resumed")),
        "one-sample transient resumed the turn: {:?}",
        d.events("w1")
    );
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["stalled"], true, "{agent}");
    assert_eq!(
        messages_for(&d, "pm")
            .iter()
            .filter(|m| m["source"].as_str() == Some("worker_notice"))
            .count(),
        1,
        "one-sample transient sent a notice"
    );

    // Real motion still resumes — two DIFFERENT consecutive samples
    // confirm too (the scrolling-pane clause: a busy pane is never
    // starved into a false stall) — and the same recipient hears it.
    atomic_write(d.stub_pane_file(&mock, "w1", "tui-state"), "BUILD 1\n");
    let n = captures();
    wait_capture(n); // first differing sample — held as a candidate
    atomic_write(d.stub_pane_file(&mock, "w1", "tui-state"), "BUILD 2\n");
    let e = d.wait_event("w1", "turn_resumed", 20);
    assert_eq!(e["payload"]["message"], "mtr", "{e}");
    wait_source(&d, "pm", "worker_notice", 2, 10);
    stall_sample(0);
}

/// The managed path (the fake's `SLEEP` is the managed mock) stalls on
/// silence and just ends if the turn finishes while stalled. An open
/// brokered approval wait is activity — it can never stall.
#[test]
fn fake_silent_turn_stalls_and_brokered_wait_does_not() {
    let d = TestDaemon::start();
    d.register_inbox("pm");
    register_fake_opts(&d, "w1", json!({"stall_secs": 2}));
    d.wait_agent("w1", "idle", 10);

    // A brokered wait outlasts the budget without stalling.
    d.send(
        "w1",
        json!({"text": "NEED_INPUT:hold", "reply_to": "pm", "message": "m-need"}),
    )
    .unwrap();
    d.wait_agent("w1", "waiting_input", 10);
    // Positive window-opened proof before any absence assertion
    // (CAD-221, the canary pattern from #70's slot-plant test): a
    // pending brokered request refreshes the turn's activity on every
    // stall tick, so a wait older than the budget still reporting
    // silence *under* the budget can only happen while the refresh
    // path runs. A dead or skipping ticker reports wall-clock age
    // instead and this loop fails loudly — absence is never asserted
    // inside a window that may not have opened.
    let wait_started = Instant::now();
    let canary_deadline = wait_started + Duration::from_secs(15);
    loop {
        let a = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
        let silent = a["silent_secs"].as_u64().unwrap_or(u64::MAX);
        if wait_started.elapsed() > Duration::from_secs(4) && silent < 2 {
            break;
        }
        assert!(
            Instant::now() < canary_deadline,
            "stall ticker never refreshed the brokered wait — the \
             absence window never provably opened: {a}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        d.events("w1")
            .iter()
            .all(|e| e["kind"].as_str() != Some("turn_stalled")),
        "brokered wait stalled: {:?}",
        d.events("w1")
    );
    let requests = d
        .operator_rpc("agent_requests", json!({"alias": "w1"}))
        .unwrap();
    let handle = requests["requests"][0]["request"].as_str().unwrap();
    d.operator_rpc(
        "agent_respond",
        json!({"alias": "w1", "request": handle, "decision": "accept"}),
    )
    .unwrap();
    d.wait_message("w1", "m-need", &["completed"], 15);

    // A genuinely silent turn stalls once — then simply ends; no
    // recovery event is owed for a finished message. The wait selects
    // by payload: a `turn_stalled` for m-need landing late (between
    // respond and completion on a slow host) must not be returned
    // here (CAD-222).
    d.send(
        "w1",
        json!({"text": "SLEEP:12", "reply_to": "pm", "message": "m-sleep"}),
    )
    .unwrap();
    let e = d.wait_event_where(
        "w1",
        "turn_stalled",
        |e| e["payload"]["message"].as_str() == Some("m-sleep"),
        20,
    );
    assert_eq!(e["payload"]["message"], "m-sleep", "{e}");
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["stalled"], true, "{agent}");
    d.wait_message("w1", "m-sleep", &["completed"], 20);
    assert!(
        d.events("w1")
            .iter()
            .all(|e| e["kind"].as_str() != Some("turn_resumed")),
        "finished turn reported resumed"
    );
    let notices = wait_source(&d, "pm", "worker_notice", 1, 5);
    assert_eq!(notices.len(), 1, "{notices:?}");
}

/// `stall_secs=0` disables detection entirely while `silent_secs` keeps
/// accruing for the views; a live `agent set` re-budgets the same
/// running turn (string values, as `agent set` sends them).
#[test]
fn stall_secs_zero_disables_and_live_set_rearms() {
    let d = TestDaemon::start();
    d.register_inbox("pm");
    register_fake_opts(&d, "w1", json!({"stall_secs": 0}));
    d.wait_agent("w1", "idle", 10);
    d.send(
        "w1",
        json!({"text": "SLEEP:14", "reply_to": "pm", "message": "m-zero"}),
    )
    .unwrap();
    d.wait_message("w1", "m-zero", &["running"], 10);
    // CAD-184 kept sleep: timing is the behaviour (silence past a budget
    // that is disabled).
    thread::sleep(Duration::from_secs(7));
    assert!(
        d.events("w1")
            .iter()
            .all(|e| e["kind"].as_str() != Some("turn_stalled")),
        "stall_secs=0 still fired"
    );
    let agent = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["agent"].clone();
    assert_eq!(agent["stalled"], false, "{agent}");
    assert!(agent["silent_secs"].as_u64().unwrap_or(0) > 0, "{agent}");

    // Live-set to a real budget — silence already past it fires on the
    // next tick; the string form proves `agent set` parsing.
    d.operator_rpc(
        "agent_set",
        json!({"alias": "w1", "patch": {"stall_secs": "3"}}),
    )
    .unwrap();
    let e = d.wait_event("w1", "turn_stalled", 15);
    assert_eq!(e["payload"]["message"], "m-zero", "{e}");
}

/// The ticker samples every live pty pane — idle included, so an
/// approval menu raised with no message in flight still surfaces —
/// and stops only when the agent does.
#[test]
fn pty_stall_sampling_runs_while_the_pane_lives() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    stall_sample(1);
    // A high budget keeps this test out of stall semantics entirely —
    // it only measures the sampling clock.
    d.register_stub("w1", json!({"auto_ready": "verified", "stall_secs": 600}));
    d.wait_agent("w1", "idle", 20);
    let captures = || {
        std::fs::read_to_string(d.stub_pane_file(&mock, "w1", "captures"))
            .unwrap_or_default()
            .len()
    };
    // Nothing has ever been sent — an idle-but-live pane still
    // samples: the approval-menu watch needs the frames.
    let baseline = captures();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut grew = false;
    while Instant::now() < deadline {
        if captures() > baseline {
            grew = true;
            break;
        }
        thread::sleep(Duration::from_millis(200));
    }
    assert!(grew, "idle pane was never sampled: {baseline}");

    // Stopping the pane mid-sampling returns promptly — the ticker
    // never holds the adapter across a capture.
    let stop_at = Instant::now();
    d.operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
    assert!(
        stop_at.elapsed() < Duration::from_secs(10),
        "agent stop delayed by sampling: {:?}",
        stop_at.elapsed()
    );
    // The actor is gone — sampling stops with it. Settle first so a
    // sample already in flight at the stop lands in the baseline.
    d.wait_agent("w1", "stopped", 15);
    // CAD-184 kept sleep: timing is the behaviour (sampler cadence).
    thread::sleep(Duration::from_secs(4));
    let idle_count = captures();
    thread::sleep(Duration::from_secs(4));
    assert_eq!(
        captures(),
        idle_count,
        "capture-pane ran after the agent stopped"
    );
    stall_sample(0);
}

/// A numbered menu over the busy box is `approval_menu`, never busy:
/// the gate refuses pastes under it (the claim survives untouched),
/// `agent answer` sends the option's one keystroke and records
/// `approval_answered`, and the sampled rise lands an `approval_menu`
/// event with the menu line.
#[test]
fn pty_approval_menu_blocks_pastes_and_answers() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    stall_sample(1);
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);
    d.send("dv", json!({"text": "do work", "message": "m1"}))
        .unwrap();
    pty_token(&d, "dv", "m1");

    // The menu appears mid-turn.
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);
    let rise = d.wait_event("dv", "approval_menu", 20);
    assert_eq!(rise["payload"]["message"], "m1", "{rise}");
    assert_eq!(rise["payload"]["line"], "$ printenv FOO", "{rise}");

    // The probe reads the menu line, not busy churn.
    let probe = d.rpc("agent_probe", json!({"alias": "dv"})).unwrap();
    assert_eq!(probe["approval_menu"], true, "{probe}");
    assert_eq!(probe["reason"], "$ printenv FOO", "{probe}");
    // The views carry the menu line while the turn runs.
    let show = d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"].clone();
    assert_eq!(show["pane_menu"], "$ printenv FOO", "{show}");
    // CAD-250: m1 reports so the actor may claim m2 at all.
    pty_report_done(&d, "dv", "m1");

    // A paste under the menu is refused: m2 queues behind a gate_wait
    // naming the menu, and no claim is eaten by the refusal.
    d.operator_rpc("agent_ready", json!({"alias": "dv", "force": true}))
        .unwrap();
    d.send("dv", json!({"text": "wait for idle", "message": "m2"}))
        .unwrap();
    let wait = d.wait_event("dv", "gate_wait", 15);
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("approval menu"),
        "{wait}"
    );
    assert_eq!(d.message_state("dv", "m2"), "queued");

    // `agent answer` validates the choice against the visible menu —
    // 9 is not on it — then sends the one digit key.
    assert!(d
        .rpc("agent_answer", json!({"alias": "dv", "choice": "9"}))
        .is_err());
    let answered = d
        .rpc(
            "agent_answer",
            json!({"alias": "dv", "choice": "8", "by": "test-op"}),
        )
        .unwrap();
    assert_eq!(answered["state"], "answered", "{answered}");
    let input = std::fs::read_to_string(d.pane_file(&mock, "dv", "input")).unwrap();
    assert!(
        input.ends_with("<KEY:8>"),
        "the digit key, never a paste: {input}"
    );
    let ev = d.wait_event("dv", "approval_answered", 10);
    assert_eq!(ev["payload"]["choice"], "8", "{ev}");
    assert_eq!(ev["payload"]["line"], "$ printenv FOO", "{ev}");
    // Identity is derived from the peer pid — the test process sits
    // outside every pane, so `by` is `operator` when it holds a
    // foreign terminal (a suite on a pty) and `unknown` when fully
    // detached; the supplied name survives only as a claim.
    let want = if (0..=2).any(|fd| {
        std::fs::read_link(format!("/proc/self/fd/{fd}"))
            .is_ok_and(|p| p.to_string_lossy().starts_with("/dev/pts/"))
    }) {
        "operator"
    } else {
        "unknown"
    };
    assert_eq!(ev["payload"]["by"], want, "{ev}");
    assert_eq!(ev["payload"]["claimed_by"], "test-op", "{ev}");
    assert_eq!(ev["payload"]["probe"]["approval_menu"], true, "{ev}");

    // With the menu cleared (operator closed it), the surviving claim
    // delivers m2 — no second `agent ready` needed: the refusal ate
    // nothing. And an answer on a non-menu pane refuses.
    std::fs::remove_file(d.pane_file(&mock, "dv", "tui-state")).unwrap();
    assert!(d
        .rpc("agent_answer", json!({"alias": "dv", "choice": "8"}))
        .is_err());
    d.wait_message("dv", "m2", &["running"], 20);
    pty_report_done(&d, "dv", "m2");
    stall_sample(0);
}

/// CAD-468: at the `turn_silent_end` edge the daemon itself prompts the
/// worker in band — one `sys-nudge-` reminder carrying the exact
/// `cadence message result` command with a `<token from `cadence self`>`
/// placeholder (never the live token — a peer reading scrollback or the
/// row could forge a report with it), pasted past the held turn like
/// any nudge and completed at its confirmed paste.
/// The reminder owns no turn and never resolves anything: the worker's
/// own report still finishes ms9, and no `unknown` is minted while the
/// bound has time left. Once per turn — a second idle sweep never sends
/// a second reminder.
#[test]
fn pty_silent_end_sends_one_report_reminder() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    stall_sample(1);
    d.register_stub(
        "w1",
        json!({"auto_ready": "verified", "silent_end_secs": 4}),
    );
    d.wait_agent("w1", "idle", 20);
    d.send("w1", json!({"text": "do work", "message": "ms9"}))
        .unwrap();
    let token = pty_token(&d, "w1", "ms9");
    d.wait_event("w1", "turn_silent_end", 40);

    // The daemon's reminder: a `sys-nudge-` row only the daemon can
    // mint, on the turnless nudge lane so it delivers while ms9 holds.
    let reminders = |d: &TestDaemon| -> Vec<Value> {
        d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| {
                m["source"] == "nudge"
                    && m["id"]
                        .as_str()
                        .is_some_and(|i| i.starts_with("sys-nudge-"))
            })
            .cloned()
            .collect()
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    let rid = loop {
        if let Some(m) = reminders(&d).first() {
            break m["id"].as_str().unwrap().to_string();
        }
        assert!(Instant::now() < deadline, "no report reminder was queued");
        thread::sleep(Duration::from_millis(50));
    };
    let n = d.wait_message("w1", &rid, &["completed"], 20);
    assert_eq!(n["nudge"], true, "{n}");
    assert_eq!(n["result"]["via"], "pty_nudge", "{n}");
    assert!(n["reply_to"].is_null(), "a reminder owes no report: {n}");
    let body = n["body"].as_str().unwrap();
    // The command names the message but never the live token — the row
    // is durable and the paste lands in scrollback, both readable by a
    // same-uid peer who could forge the report with it. The worker gets
    // the token itself from `cadence self`.
    assert!(
        body.contains(
            "cadence message result ms9 --token <token from `cadence self`> --text '<summary>'"
        ),
        "the reminder carries the exact command with the token placeholder: {body}"
    );
    assert!(
        !body.contains(&token),
        "the live turn token is never pasted: {body}"
    );
    assert!(body.contains("report_timeout_secs"), "{body}");

    // Once per turn: the pane stays idle across more sweeps and no
    // second reminder is minted.
    thread::sleep(Duration::from_secs(6));
    assert_eq!(reminders(&d).len(), 1, "{:?}", reminders(&d));
    // The overview's silent_end row notes the worker was prompted.
    let home = TempDir::new().unwrap();
    let view = overview_at(home.path(), &d.state, None, &[]);
    let ended = view["needs_me"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["kind"] == "silent_end")
        .expect("silent_end row");
    assert!(
        ended["title"].as_str().unwrap().contains("reminder sent"),
        "{ended}"
    );

    // The held turn is untouched — still running — and the worker's own
    // report resolves it normally: never `unknown` on a timely answer.
    assert_eq!(d.message_state("w1", "ms9"), "running");
    d.report("ms9", &token, "result", "done").unwrap();
    d.wait_message("w1", "ms9", &["completed"], 10);
    assert_eq!(
        d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["unknown"],
        0
    );
    stall_sample(0);
}

/// CAD-468: the reminder's `(nudge, "report-reminder:<message>:<turn>")`
/// id makes the edge once-per-turn across a daemon restart — the adopted
/// turn's pane re-probes idle and `turn_silent_end` fires again, but no
/// second reminder is minted.
#[test]
fn pty_silent_end_reminder_is_not_resent_after_restart() {
    let mut d = TestDaemon::start();
    let _mock = d.mock_stub();
    stall_sample(1);
    d.register_stub(
        "w1",
        json!({"auto_ready": "verified", "silent_end_secs": 4}),
    );
    d.wait_agent("w1", "idle", 20);
    d.send("w1", json!({"text": "do work", "message": "ms9"}))
        .unwrap();
    pty_token(&d, "w1", "ms9");
    d.wait_event("w1", "turn_silent_end", 40);
    let reminders = |d: &TestDaemon| -> Vec<Value> {
        d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| {
                m["source"] == "nudge"
                    && m["id"]
                        .as_str()
                        .is_some_and(|i| i.starts_with("sys-nudge-"))
            })
            .cloned()
            .collect()
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while reminders(&d).is_empty() {
        assert!(Instant::now() < deadline, "no report reminder was queued");
        thread::sleep(Duration::from_millis(50));
    }

    // Restart over the same state: ms9 is adopted still-running, its
    // pane re-probes idle, the edge fires a second time — the daemon
    // message dedupes, so the one reminder row is all there ever is.
    // `shutdown` is operator-gated, so it takes the operator seam.
    d.operator_rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let state = d.state.clone();
    std::mem::forget(d);
    let d = TestDaemon::start_on(state);
    wait_event_count(&d, "w1", "turn_adopted", 1, 25);
    let fires = wait_event_count(&d, "w1", "turn_silent_end", 2, 40);
    assert_eq!(fires.len(), 2, "the edge re-fired after restart: {fires:?}");
    assert_eq!(reminders(&d).len(), 1, "{:?}", reminders(&d));
    assert_eq!(d.message_state("w1", "ms9"), "running");
    stall_sample(0);
}

/// A menu that opens BEFORE any turn starts — the pane sits blocked
/// with a message still queued — must surface identically to a
/// mid-turn one: `approval_menu` fires against the queued head
/// (marked `queued`), the views carry `pane_menu`, the needs-me row
/// names the answer command, and `agent answer` unblocks delivery.
#[test]
fn pty_queued_menu_surfaces_and_answers() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    stall_sample(1);
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);

    // The menu opens first; the send behind it can only queue.
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);
    d.send("dv", json!({"text": "blocked send", "message": "mq"}))
        .unwrap();
    // `queued` or `submitting` — the delivery loop may have already
    // claimed the head and be gate-waiting on the menu; it cannot be
    // running while the pane shows a menu.
    let st = d.message_state("dv", "mq");
    assert!(st == "queued" || st == "submitting", "{st}");

    // The queued head is tracked for menu detection: the event names
    // the waiting message and marks it queued, the views carry the
    // menu line with no running turn at all.
    let rise = d.wait_event("dv", "approval_menu", 20);
    assert_eq!(rise["payload"]["message"], "mq", "{rise}");
    assert_eq!(rise["payload"]["queued"], true, "{rise}");
    assert_eq!(rise["payload"]["line"], "$ printenv FOO", "{rise}");
    let show = d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"].clone();
    assert_eq!(show["pane_menu"], "$ printenv FOO", "{show}");
    let home = TempDir::new().unwrap();
    let view = overview_at(home.path(), &d.state, None, &[]);
    let needs = view["needs_me"].as_array().unwrap();
    assert!(
        needs.iter().any(|n| n["kind"] == "approval_menu"
            && n["command"]
                .as_str()
                .unwrap_or("")
                .starts_with("cadence agent answer dv ")),
        "{needs:?}"
    );

    // A garbage index is rejected at the RPC — the count never
    // reaches a key vector — and the daemon answers normally after.
    let err = d
        .rpc(
            "agent_answer",
            json!({"alias": "dv", "choice": "4000000000"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("no option 4000000000"), "{err}");
    // And a menu whose option block cannot be parsed refuses rather
    // than walking blind — the legend anchor alone, no option rows.
    atomic_write(
        d.pane_file(&mock, "dv", "tui-state"),
        "↑↓ select · ↵ confirm · esc cancel\n⠸ Thinking · 5s (esc twice to interrupt)\n",
    );
    let probe = d.rpc("agent_probe", json!({"alias": "dv"})).unwrap();
    assert_eq!(probe["approval_menu"], true, "{probe}");
    assert!(d
        .rpc("agent_answer", json!({"alias": "dv", "choice": "1"}))
        .is_err());
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);

    // Answering the menu unblocks the queued send — the pane claims
    // cleanly once the operator's menu is gone. A real TUI consumes
    // the answer key; the mock leaves it staged, so clear the input
    // file the way an answered menu would.
    d.rpc("agent_answer", json!({"alias": "dv", "choice": "8"}))
        .unwrap();
    std::fs::remove_file(d.pane_file(&mock, "dv", "tui-state")).unwrap();
    atomic_write(d.pane_file(&mock, "dv", "input"), "");
    d.operator_rpc("agent_ready", json!({"alias": "dv"}))
        .unwrap();
    d.wait_message("dv", "mq", &["running"], 20);
    let token = pty_token(&d, "dv", "mq");
    d.report("mq", &token, "result", "done").unwrap();
    d.wait_message("dv", "mq", &["completed"], 10);
    stall_sample(0);
}

/// A menu raised on a pane with NO message at all still surfaces —
/// the stall watch samples idle panes, the event fires `idle: true`
/// and attributes no message, and the needs-me row names the remedy.
#[test]
fn pty_idle_pane_menu_surfaces() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    stall_sample(1);
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);

    // Nothing was ever sent — the menu arrives on a quiet pane.
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);
    let rise = d.wait_event("dv", "approval_menu", 20);
    assert_eq!(rise["payload"]["idle"], true, "{rise}");
    assert_eq!(rise["payload"]["line"], "$ printenv FOO", "{rise}");
    assert!(rise["payload"]["message"].is_null(), "{rise}");
    let show = d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"].clone();
    assert_eq!(show["pane_menu"], "$ printenv FOO", "{show}");
    let home = TempDir::new().unwrap();
    let view = overview_at(home.path(), &d.state, None, &[]);
    let needs = view["needs_me"].as_array().unwrap();
    assert!(
        needs.iter().any(|n| n["kind"] == "approval_menu"
            && n["command"]
                .as_str()
                .unwrap_or("")
                .starts_with("cadence agent answer dv ")),
        "{needs:?}"
    );

    // The operator can answer it straight away.
    d.rpc("agent_answer", json!({"alias": "dv", "choice": "8"}))
        .unwrap();
    stall_sample(0);
}

/// A menu that closes and later reopens is a NEW approval — the same
/// subject must fire `approval_menu` again. The event history is
/// scoped to the open menu: it clears when the menu closes, so the
/// second occurrence is never deduped away.
#[test]
fn pty_menu_event_refires_after_close() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    stall_sample(1);
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);

    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);
    let first = d.wait_event("dv", "approval_menu", 20);
    assert_eq!(first["payload"]["line"], "$ printenv FOO", "{first}");

    // Menu closes — the idle screen returns. The sampler must observe
    // at least one non-menu frame before the reopen.
    std::fs::remove_file(d.pane_file(&mock, "dv", "tui-state")).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let show = d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"].clone();
        if show["pane_menu"].is_null() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{show}");
        thread::sleep(Duration::from_millis(250));
    }

    // The same menu reopens — same subject — and fires again.
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);
    let events = wait_event_count(&d, "dv", "approval_menu", 2, 20);
    assert_eq!(events[1]["payload"]["line"], "$ printenv FOO", "{events:?}");
    stall_sample(0);
}

/// The answerer's identity is derived from the socket peer's pid
/// walking its /proc ancestry into a pane — never from a `by` the
/// client chose. A caller inside the target's own pane is refused
/// outright; inside another agent's pane it stamps that agent.
#[test]
fn pty_answer_derives_caller_from_peer_pid() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.register_devin_opts("peer", json!({}));
    d.wait_agent("dv", "idle", 20);
    d.wait_agent("peer", "idle", 20);
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);

    // Point a pane pid at this test process and every RPC it makes
    // descends from that pane — the unforgeable "caller is inside
    // the agent" signal.
    let me = std::process::id() as i64;
    let set_pid = |alias: &str, pid: i64| {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE agents SET pid=?1, pid_start=?3 WHERE alias=?2",
            rusqlite::params![pid, alias, proc_start(pid as u32)],
        )
        .unwrap();
    };

    // Self-approval refuses before any key is sent — even with `by`
    // claiming to be an operator.
    set_pid("dv", me);
    let err = d
        .rpc(
            "agent_answer",
            json!({"alias": "dv", "choice": "8", "by": "operator"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("its own pane"), "{err}");
    let input = std::fs::read_to_string(d.pane_file(&mock, "dv", "input")).unwrap_or_default();
    assert!(!input.contains("<KEY"), "no key sent: {input}");

    // A caller inside ANOTHER agent's pane stamps that agent; a `by`
    // claiming otherwise is kept only as a claim.
    set_pid("dv", 999_999_999);
    set_pid("peer", me);
    d.rpc(
        "agent_answer",
        json!({"alias": "dv", "choice": "8", "by": "dv"}),
    )
    .unwrap();
    let ev = d.wait_event("dv", "approval_answered", 10);
    assert_eq!(ev["payload"]["by"], "peer", "{ev}");
    assert_eq!(ev["payload"]["by_kind"], "agent", "{ev}");
    assert_eq!(ev["payload"]["claimed_by"], "dv", "{ev}");
    assert_eq!(ev["payload"]["caller_pid"], me, "{ev}");

    // Outside every pane the caller is an operator only when it holds
    // a foreign terminal — this test process inherits one when the
    // suite runs on a pty, none under piped CI — and `unknown`
    // otherwise. A `by` naming the target is a claim, not an
    // attribution either way.
    set_pid("peer", 999_999_999);
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);
    d.rpc(
        "agent_answer",
        json!({"alias": "dv", "choice": "1", "by": "dv"}),
    )
    .unwrap();
    let on_tty = (0..=2).any(|fd| {
        std::fs::read_link(format!("/proc/self/fd/{fd}"))
            .is_ok_and(|p| p.to_string_lossy().starts_with("/dev/pts/"))
    });
    let want = if on_tty { "operator" } else { "unknown" };
    let evs = wait_event_count(&d, "dv", "approval_answered", 2, 10);
    assert_eq!(evs[1]["payload"]["by"], want, "{evs:?}");
    assert_eq!(evs[1]["payload"]["by_kind"], want, "{evs:?}");
    assert_eq!(evs[1]["payload"]["claimed_by"], "dv", "{evs:?}");
}

/// `setsid` detaches the caller from the pane's /proc ancestry — the
/// self-approval guard must still see through it. The pane's own
/// `CADENCE_ALIAS` env survives the detach, so `setsid env
/// CADENCE_ALIAS=<self> cadence agent answer <self>` is refused rather
/// than stamped `operator`. A detached caller carrying another pane's
/// alias attributes to that pane — an agent, never an operator.
#[test]
fn pty_answer_setsid_cannot_launder_self_approval() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.register_devin_opts("peer", json!({}));
    d.wait_agent("dv", "idle", 20);
    d.wait_agent("peer", "idle", 20);
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);

    // The detached call names its own pane in env — refused, no key
    // reaches the input, no event is stamped.
    let out = std::process::Command::new("setsid")
        .arg("env")
        .arg("CADENCE_ALIAS=dv")
        .arg(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["agent", "answer", "dv", "8"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "self-answer must refuse: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let input = std::fs::read_to_string(d.pane_file(&mock, "dv", "input")).unwrap_or_default();
    assert!(!input.contains("<KEY"), "no key sent: {input}");
    let events = d.rpc("agent_events", json!({"alias": "dv"})).unwrap();
    assert!(
        !events.to_string().contains("approval_answered"),
        "self-answer must not stamp an event: {events}"
    );

    // Detached and carrying ANOTHER pane's env — stamps that pane as
    // the agent, not `operator`.
    let out = std::process::Command::new("setsid")
        .arg("env")
        .arg("CADENCE_ALIAS=peer")
        .arg(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["agent", "answer", "dv", "8"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ev = d.wait_event("dv", "approval_answered", 10);
    assert_eq!(ev["payload"]["by"], "peer", "{ev}");
    assert_eq!(ev["payload"]["by_kind"], "agent", "{ev}");
}

/// A *full* detach — `env -u CADENCE_ALIAS setsid sh -c 'cadence agent
/// answer <self> </dev/null >/dev/null 2>&1'` — clears ancestry, env
/// and tty at once. The caller matches nothing, so the honest stamp is
/// `unknown`, never `operator`: `operator` needs positive terminal
/// evidence the detached process cannot carry.
#[test]
fn pty_answer_full_detach_stamps_unknown() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);
    atomic_write(d.pane_file(&mock, "dv", "tui-state"), DEVIN_MENU);

    let out = std::process::Command::new("setsid")
        .arg("env")
        .arg("-u")
        .arg("CADENCE_ALIAS")
        .arg(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&d.state)
        .args(["agent", "answer", "dv", "8"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "unmatched-but-unproven caller answers as unknown: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ev = d.wait_event("dv", "approval_answered", 10);
    assert_eq!(ev["payload"]["by"], "unknown", "{ev}");
    assert_eq!(ev["payload"]["by_kind"], "unknown", "{ev}");
}

/// A transcript that quotes a real menu verbatim — anchor, `❯`-led
/// numbered run and all — cannot flip the pane: a live menu replaces
/// the input box's interior, so the boxed `❯` prompt still rendered
/// below the quote proves the menu-looking rows are text. The probe
/// stays inert and `agent answer` refuses rather than keying a digit
/// into the live input line.
#[test]
fn pty_claude_quoted_menu_above_input_box_is_inert() {
    let d = TestDaemon::start();
    let mock = d.mock_claude_tui();
    d.register_claude_pty("cl", json!({"auto_ready": "verified"}));
    d.wait_agent("cl", "idle", 20);
    atomic_write(
        d.claude_pane_file(&mock, "cl", "tui-state"),
        "● I reproduced it. The pane printed:\n\n    Do you want to proceed?\n    ❯ 1. Yes\n      2. No, and tell Claude what to do differently\n\n  So it is waiting on you.\n────────────────────\n❯ \n",
    );
    let probe = d.rpc("agent_probe", json!({"alias": "cl"})).unwrap();
    assert_eq!(probe["approval_menu"], false, "{probe}");
    assert_eq!(probe["idle"], true, "{probe}");
    let err = d
        .rpc("agent_answer", json!({"alias": "cl", "choice": "2"}))
        .unwrap_err();
    assert!(err.to_string().contains("no approval menu"), "{err}");
    let input =
        std::fs::read_to_string(d.claude_pane_file(&mock, "cl", "input")).unwrap_or_default();
    assert!(!input.contains("<KEY"), "no key sent: {input}");
}

/// Same class on the Devin profile (CAD-102 r6): a verbatim quoted
/// menu above the live idle input box probes inert — the legend is
/// transcript text, and the editable `❭` row vetoes the region —
/// and `agent answer` refuses rather than keying the digit into the
/// input line.
#[test]
fn pty_devin_quoted_menu_above_input_box_is_inert() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);
    atomic_write(
        d.pane_file(&mock, "dv", "tui-state"),
        "● The pane showed:\n\n  Allow this tool call?\n  ❭ 1 Yes  (Approve once)\n  · 2 Yes, allow `env` commands\n  · 8 No\n  ↑↓ select · ↵ confirm · esc cancel\n\n  So it is waiting.\n\n────────────────────\n❭ Ask Devin to build features, fix bugs, or work on your code\n────────────────────\nSWE-2 Max   Context: 43k / 262k\n",
    );
    let probe = d.rpc("agent_probe", json!({"alias": "dv"})).unwrap();
    assert_eq!(probe["approval_menu"], false, "{probe}");
    assert_eq!(probe["idle"], true, "{probe}");
    let err = d
        .rpc("agent_answer", json!({"alias": "dv", "choice": "2"}))
        .unwrap_err();
    assert!(err.to_string().contains("no approval menu"), "{err}");
    let input = std::fs::read_to_string(d.pane_file(&mock, "dv", "input")).unwrap_or_default();
    assert!(!input.contains("<KEY"), "no key sent: {input}");
}

/// A transient `capture-pane` failure inside the gate probe refuses
/// the send like a busy pane — `gate_wait`, message still queued —
/// never an actor-fatal provider error. The daemon survives and the
/// send delivers once the outage clears.
#[test]
fn pty_gate_probe_failure_is_a_gate_refusal() {
    let d = TestDaemon::start();
    let _mock = d.mock_devin();
    d.register_devin_opts("dv", json!({"auto_ready": "verified"}));
    d.wait_agent("dv", "idle", 20);

    // `MOCK_TMUX_FAIL` is process-global — a parallel test's mock
    // calls could trip on it inside this window. The outage is
    // seconds-long and the failure mode (a gate retry) is benign, so
    // the knob stays env-global rather than growing a per-pane
    // failure file.
    std::env::set_var("MOCK_TMUX_FAIL", "capture-pane");
    d.send("dv", json!({"text": "during outage", "message": "mf"}))
        .unwrap();
    let wait = d.wait_event("dv", "gate_wait", 15);
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("pane probe failed"),
        "{wait}"
    );
    assert_eq!(d.message_state("dv", "mf"), "queued");
    std::env::remove_var("MOCK_TMUX_FAIL");

    d.wait_message("dv", "mf", &["running"], 20);
    let token = pty_token(&d, "dv", "mf");
    d.report("mf", &token, "result", "done").unwrap();
    d.wait_message("dv", "mf", &["completed"], 10);
}

/// `agent answer` is a menu channel only: a pane with no menu refuses
/// (idle, busy or fenced alike), and a non-pty endpoint has no such
/// channel at all.
#[test]
fn pty_answer_refuses_without_a_menu() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    d.register_stub("w1", json!({}));
    d.register("fx");
    d.wait_agent("w1", "idle", 20);
    d.wait_agent("fx", "idle", 10);
    let err = d
        .rpc("agent_answer", json!({"alias": "w1", "choice": "1"}))
        .unwrap_err();
    assert!(err.to_string().contains("no approval menu"), "{err}");
    let err = d
        .rpc("agent_answer", json!({"alias": "fx", "choice": "1"}))
        .unwrap_err();
    assert!(
        err.to_string().contains("no approval-menu channel"),
        "{err}"
    );
}

/// The AOS-11 shape on a stub pane: task message `id` was pasted and is
/// `running` under its turn token, unreported, and its draft sits in the
/// input line again as if the submit keystroke had been lost — no busy
/// marker, no menu. The delivery's own paste and Enter are the only
/// keys the pane has seen. Answers the turn token.
fn stuck_draft(d: &TestDaemon, mock: &MockStub, alias: &str, id: &str) -> String {
    d.send(alias, json!({"text": RECOVER_BODY, "message": id}))
        .unwrap();
    let token = pty_token(d, alias, id);
    atomic_write(d.stub_pane_file(mock, alias, "input"), RECOVER_BODY);
    let probe = d.rpc("agent_probe", json!({"alias": alias})).unwrap();
    assert_eq!(probe["input_nonempty"], true, "{probe}");
    assert_eq!(probe["busy_marker"], false, "{probe}");
    token
}

/// The mock tmux's call log for the test's socket; `mock_dir` is the
/// stub or Devin mock's install dir.
fn tmux_log(d: &TestDaemon, mock_dir: &Path) -> String {
    std::fs::read_to_string(
        mock_dir
            .join("tmux-state")
            .join(socket_for(&d.state))
            .join("calls.log"),
    )
    .unwrap_or_default()
}

/// `(pastes, enters)` the mock tmux delivered to `alias`'s pane.
fn pane_keys(d: &TestDaemon, mock_dir: &Path, alias: &str) -> (usize, usize) {
    let log = tmux_log(d, mock_dir);
    let target = format!("-t {alias}");
    let pastes = log
        .lines()
        .filter(|l| l.starts_with("paste-buffer") && l.ends_with(&target))
        .count();
    let enters = log
        .lines()
        .filter(|l| *l == format!("send-keys -t {alias} Enter"))
        .count();
    (pastes, enters)
}

fn recover_events(d: &TestDaemon, alias: &str, kind: &str) -> Vec<Value> {
    d.events(alias)
        .into_iter()
        .filter(|e| e["kind"] == kind)
        .map(|e| e["payload"].clone())
        .collect()
}

/// A refusal names its check, sends nothing — the delivery's one paste
/// and one Enter stay the only keys, the draft stays staged and the
/// message stays `running` — and records `submit_recover_refused` with
/// the caller, alias, generation, message id, before state and result,
/// never the body.
fn assert_recover_refused(
    d: &TestDaemon,
    mock: &MockStub,
    alias: &str,
    id: &str,
    err: cadence_agent::Error,
    check: &str,
) {
    let text = err.to_string();
    assert!(
        text.contains(&format!("agent recover-submit refused ({check})")),
        "{text}"
    );
    assert!(!text.contains("CAD152-KICKOFF"), "{text}");
    assert_eq!(
        pane_keys(d, &mock.dir, alias),
        (1, 1),
        "{}",
        tmux_log(d, &mock.dir)
    );
    assert!(recover_events(d, alias, "submit_recovered").is_empty());
    let refused = recover_events(d, alias, "submit_recover_refused");
    let event = refused
        .iter()
        .find(|e| e["check"] == check)
        .unwrap_or_else(|| panic!("no {check} refusal recorded: {refused:?}"));
    let agent = d.rpc("agent_show", json!({"alias": alias})).unwrap()["agent"].clone();
    assert_eq!(event["by"], "operator", "{event}");
    assert_eq!(event["by_kind"], "operator", "{event}");
    assert_eq!(event["alias"], alias, "{event}");
    assert_eq!(event["message"], id, "{event}");
    assert_eq!(event["generation"], agent["generation"], "{event}");
    assert_eq!(event["result"], "refused", "{event}");
    assert!(event["after"].is_null(), "{event}");
    assert!(!event.to_string().contains("CAD152-KICKOFF"), "{event}");
}

/// `text` word-wrapped the way a TUI renders a long draft: greedy, a
/// break consumes the space, every row at most `width` characters.
fn word_wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    for word in text.split(' ') {
        match rows.last_mut() {
            Some(row) if row.chars().count() + 1 + word.chars().count() <= width => {
                row.push(' ');
                row.push_str(word);
            }
            _ => rows.push(word.to_string()),
        }
    }
    rows
}

fn recover(d: &TestDaemon, alias: &str, id: &str) -> cadence_agent::Result<Value> {
    d.operator_rpc(
        "agent_recover_submit",
        json!({"alias": alias, "message": id}),
    )
}

/// A recoverable stub agent with one stuck kickoff `m1`.
fn recover_fixture(params: Value) -> (TestDaemon, MockStub, String) {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    d.register_stub("st", params);
    d.wait_agent("st", "idle", 20);
    let token = stuck_draft(&d, &mock, "st", "m1");
    (d, mock, token)
}

/// CAD-152 acceptance 1, 3, 5: the operator recovers a lost submit
/// through the CLI — the draft may be wrapped across rows — with
/// exactly one Enter and no second paste. The message keeps its turn
/// token and completes through the normal report; a second recovery of
/// the same message refuses as already submitted. The audit event
/// carries caller, alias, generation, message id, before/after state
/// and result — never the body.
#[test]
fn recover_submit_sends_one_enter_and_keeps_correlation() {
    let (d, mock, token) = recover_fixture(json!({"auto_ready": "verified"}));
    // The TUI word-wraps a long draft at its input width.
    atomic_write(
        d.stub_pane_file(&mock, "st", "input"),
        word_wrap(RECOVER_BODY, STUB_INPUT_WIDTH).join("\n"),
    );
    let generation = d.rpc("agent_show", json!({"alias": "st"})).unwrap()["agent"]["generation"]
        .as_str()
        .unwrap()
        .to_string();
    let (ok, out, err) = d.operator_cadence(&[
        "agent",
        "recover-submit",
        "st",
        "--message",
        "m1",
        "--generation",
        &generation,
    ]);
    assert!(ok, "{out}\n{err}");
    let out: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(out["state"], "submitted", "{out}");
    assert_eq!(out["before"]["input_nonempty"], true, "{out}");
    assert_eq!(out["after"]["input_nonempty"], false, "{out}");
    // One Enter on top of the delivery's; no re-paste.
    assert_eq!(
        pane_keys(&d, &mock.dir, "st"),
        (1, 2),
        "{}",
        tmux_log(&d, &mock.dir)
    );
    // The TUI took the staged draft as the turn.
    let screen = std::fs::read_to_string(d.stub_pane_file(&mock, "st", "screen")).unwrap();
    assert_eq!(
        screen.matches("STUB_REPLY: CAD152-KICKOFF").count(),
        2,
        "{screen}"
    );
    // Normal correlation: still running under the token minted at
    // paste (read from the store — CAD-375 withholds it from this
    // connection); the report completes it.
    d.wait_message("st", "m1", &["running"], 5);
    assert_eq!(running_token(&d, "m1"), token);
    // A second recovery of the same message — racing, or later —
    // refuses and sends nothing.
    atomic_write(d.stub_pane_file(&mock, "st", "input"), RECOVER_BODY);
    let e = recover(&d, "st", "m1").unwrap_err().to_string();
    assert!(
        e.contains("agent recover-submit refused (already_submitted)"),
        "{e}"
    );
    assert_eq!(pane_keys(&d, &mock.dir, "st"), (1, 2));
    atomic_write(d.stub_pane_file(&mock, "st", "input"), "");
    d.report("m1", &token, "result", "done").unwrap();
    d.wait_message("st", "m1", &["completed"], 10);
    let e = recover(&d, "st", "m1").unwrap_err().to_string();
    assert!(e.contains("(already_submitted)"), "{e}");
    // The audit record.
    let sent = recover_events(&d, "st", "submit_recovered");
    assert_eq!(sent.len(), 1, "{sent:?}");
    let ev = &sent[0];
    assert_eq!(ev["by"], "operator", "{ev}");
    assert_eq!(ev["by_kind"], "operator", "{ev}");
    assert_eq!(ev["alias"], "st", "{ev}");
    assert_eq!(ev["message"], "m1", "{ev}");
    assert_eq!(ev["generation"], generation.as_str(), "{ev}");
    assert_eq!(ev["result"], "submitted", "{ev}");
    assert_eq!(ev["before"]["input_nonempty"], true, "{ev}");
    assert_eq!(ev["after"]["input_nonempty"], false, "{ev}");
    for e in d
        .events("st")
        .iter()
        .filter(|e| e["kind"] == "submit_recovered" || e["kind"] == "submit_recover_refused")
    {
        assert!(!e.to_string().contains("CAD152-KICKOFF"), "{e}");
    }
    let refused = recover_events(&d, "st", "submit_recover_refused");
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert!(refused.iter().all(|e| e["check"] == "already_submitted"));
}

/// CAD-152: a generation the operator inspected that is no longer the
/// live one, and a message whose turn token was minted under an earlier
/// endpoint generation, both refuse as `stale_generation`.
#[test]
fn recover_submit_refuses_stale_generation() {
    let (d, mock, _token) = recover_fixture(json!({"auto_ready": "verified"}));
    let err = d
        .operator_rpc(
            "agent_recover_submit",
            json!({"alias": "st", "message": "m1", "generation": "an-earlier-life"}),
        )
        .unwrap_err();
    assert_recover_refused(&d, &mock, "st", "m1", err, "stale_generation");
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE messages SET turn_id=?1 WHERE id='m1'",
        [format!("pty-earlier-{}", uuid::Uuid::new_v4())],
    )
    .unwrap();
    let err = recover(&d, "st", "m1").unwrap_err();
    assert!(
        err.to_string().contains("earlier endpoint generation"),
        "{err}"
    );
    assert_recover_refused(&d, &mock, "st", "m1", err, "stale_generation");
}

/// CAD-152: the id of a message that was never pasted — queued behind
/// the stuck one — refuses and names the pending pasted message.
#[test]
fn recover_submit_refuses_other_message_pending() {
    let (d, mock, _token) = recover_fixture(json!({"auto_ready": "verified"}));
    d.send("st", json!({"text": "a follow-up", "message": "m2"}))
        .unwrap();
    assert_eq!(d.message_state("st", "m2"), "queued");
    let err = recover(&d, "st", "m2").unwrap_err();
    assert!(err.to_string().contains("holds is m1"), "{err}");
    assert_recover_refused(&d, &mock, "st", "m2", err, "other_message_pending");
}

/// CAD-152: nothing staged in the input line — nothing to submit.
#[test]
fn recover_submit_refuses_empty_input() {
    let (d, mock, _token) = recover_fixture(json!({"auto_ready": "verified"}));
    atomic_write(d.stub_pane_file(&mock, "st", "input"), "");
    let err = recover(&d, "st", "m1").unwrap_err();
    assert_recover_refused(&d, &mock, "st", "m1", err, "empty_input");
}

/// CAD-152: a busy TUI (an active worker) refuses even with the exact
/// draft staged.
#[test]
fn recover_submit_refuses_busy_pane() {
    let (d, mock, _token) = recover_fixture(json!({"auto_ready": "verified"}));
    atomic_write(d.stub_pane_file(&mock, "st", "tui-state"), "stub working\n");
    let err = recover(&d, "st", "m1").unwrap_err();
    assert_recover_refused(&d, &mock, "st", "m1", err, "busy");
}

/// CAD-152: an open approval menu refuses — an Enter would answer it.
#[test]
fn recover_submit_refuses_open_approval_menu() {
    let (d, mock, _token) = recover_fixture(json!({"auto_ready": "verified"}));
    atomic_write(
        d.stub_pane_file(&mock, "st", "tui-state"),
        "stub approval: run tests?\n",
    );
    let err = recover(&d, "st", "m1").unwrap_err();
    assert!(err.to_string().contains("cadence agent answer"), "{err}");
    assert_recover_refused(&d, &mock, "st", "m1", err, "approval_menu");
}

/// CAD-152: a draft that is not the message body — edited by a user,
/// or other text — refuses without quoting it.
#[test]
fn recover_submit_refuses_edited_draft() {
    let (d, mock, _token) = recover_fixture(json!({"auto_ready": "verified"}));
    let edited = RECOVER_BODY.replace("docs/brief.md", "docs/other.md");
    atomic_write(d.stub_pane_file(&mock, "st", "input"), &edited);
    let err = recover(&d, "st", "m1").unwrap_err();
    assert_recover_refused(&d, &mock, "st", "m1", err, "draft_mismatch");
    atomic_write(
        d.stub_pane_file(&mock, "st", "input"),
        "operator note: hold on",
    );
    let err = recover(&d, "st", "m1").unwrap_err();
    assert_recover_refused(&d, &mock, "st", "m1", err, "draft_mismatch");
}

/// CAD-152: only part of the body visible (a scrolled or clipped input
/// line), or a whitespace difference a wrap could hide, is ambiguous —
/// never a blind submit.
#[test]
fn recover_submit_refuses_ambiguous_wrap() {
    let (d, mock, _token) = recover_fixture(json!({"auto_ready": "verified"}));
    atomic_write(
        d.stub_pane_file(&mock, "st", "input"),
        &RECOVER_BODY[RECOVER_BODY.len() - 40..],
    );
    let err = recover(&d, "st", "m1").unwrap_err();
    assert_recover_refused(&d, &mock, "st", "m1", err, "ambiguous_wrap");
    atomic_write(
        d.stub_pane_file(&mock, "st", "input"),
        RECOVER_BODY.replacen(" the brief", "thebrief", 1),
    );
    let err = recover(&d, "st", "m1").unwrap_err();
    assert_recover_refused(&d, &mock, "st", "m1", err, "ambiguous_wrap");
}

/// CAD-152: a non-pty endpoint has no draft to submit — refused before
/// any message lookup, naming the endpoint.
#[test]
fn recover_submit_refuses_non_pty_endpoint() {
    let d = TestDaemon::start();
    d.register("fx");
    d.wait_agent("fx", "idle", 10);
    let err = recover(&d, "fx", "m1").unwrap_err().to_string();
    assert!(
        err.contains("agent recover-submit refused (unsupported_endpoint)"),
        "{err}"
    );
}

/// CAD-152 acceptance 4: only the operator or the agent's own PM may
/// recover-submit — derived from the connection (CAD-149). A peer
/// worker, another group's PM, the agent on itself, a caller that is
/// not provably the operator and a claimed identity are all refused
/// with the rule named, and send nothing; the PM's recovery goes
/// through and is attributed to the PM.
#[test]
fn recover_submit_caller_rule_per_caller_kind() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    let mut p = guard_panes(&d);
    d.register_stub("st", json!({"upstream": "pm", "auto_ready": "verified"}));
    d.wait_agent("st", "idle", 20);
    stuck_draft(&d, &mock, "st", "m1");
    let req = json!({"alias": "st", "message": "m1"});
    let r = p.w1.rpc(&d.state, "agent_recover_submit", req.clone());
    let e = frame_err(&r);
    assert!(
        e.contains("agent 'w1' cannot change another agent") && e.contains("its PM 'pm'"),
        "{r}"
    );
    let r = p.pm2.rpc(&d.state, "agent_recover_submit", req.clone());
    assert!(
        frame_err(&r).contains("agent 'pm2' cannot change another agent"),
        "{r}"
    );
    let r = unprovable_rpc(&d, "agent_recover_submit", req.clone());
    assert!(frame_err(&r).contains("not provably the operator"), "{r}");
    let r = p.w1.rpc(
        &d.state,
        "agent_recover_submit",
        json!({"alias": "w1", "message": "m1"}),
    );
    assert!(
        frame_err(&r).contains("agent 'w1' cannot make this change to itself"),
        "{r}"
    );
    let mut forged = req.clone();
    forged["by"] = json!("operator");
    let r = p.w1.rpc(&d.state, "agent_recover_submit", forged);
    assert!(frame_err(&r).contains("'by' is not accepted"), "{r}");
    // Nothing sent, nothing recorded for the refused callers.
    assert_eq!(
        pane_keys(&d, &mock.dir, "st"),
        (1, 1),
        "{}",
        tmux_log(&d, &mock.dir)
    );
    assert!(recover_events(&d, "st", "submit_recovered").is_empty());
    assert!(recover_events(&d, "st", "submit_recover_refused").is_empty());
    // The target's own PM.
    let r = p.pm.rpc(&d.state, "agent_recover_submit", req);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["state"], "submitted", "{r}");
    assert_eq!(pane_keys(&d, &mock.dir, "st"), (1, 2));
    let sent = recover_events(&d, "st", "submit_recovered");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["by"], "pm", "{sent:?}");
    assert_eq!(sent[0]["by_kind"], "agent", "{sent:?}");
}

/// CAD-152: the AOS-11 reproduction on the Devin profile it was seen
/// on — a Devin pane whose kickoff sits unsubmitted in the input box,
/// between the box rule and the model bar — recovered entirely through
/// cadence; the turn then reports under its original token.
#[test]
fn recover_submit_devin_lost_submit() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    // The live box's bottom rule and model bar under the input row.
    atomic_write(
        d.pane_file(&mock, "dv1", "tui-state"),
        format!("{}\nSWE-2 Max\n", "─".repeat(60)),
    );
    d.operator_rpc("agent_ready", json!({"alias": "dv1"}))
        .unwrap();
    d.send("dv1", json!({"text": RECOVER_BODY, "message": "k1"}))
        .unwrap();
    let token = pty_token(&d, "dv1", "k1");
    atomic_write(d.pane_file(&mock, "dv1", "input"), RECOVER_BODY);
    let r = recover(&d, "dv1", "k1").unwrap();
    assert_eq!(r["state"], "submitted", "{r}");
    // The delivery's paste and Enter, plus exactly one Enter.
    assert_eq!(
        pane_keys(&d, &mock.dir, "dv1"),
        (1, 2),
        "{}",
        tmux_log(&d, &mock.dir)
    );
    let screen = std::fs::read_to_string(d.pane_file(&mock, "dv1", "screen")).unwrap();
    assert_eq!(
        screen.matches("MOCK_REPLY: CAD152-KICKOFF").count(),
        2,
        "{screen}"
    );
    d.report("k1", &token, "result", "done").unwrap();
    d.wait_message("dv1", "k1", &["completed"], 10);
}

/// CAD-152: a pane in a tmux mode (copy/view) refuses — the Enter
/// would scroll the mode, not reach the TUI.
#[test]
fn recover_submit_refuses_pane_in_tmux_mode() {
    let (d, mock, _token) = recover_fixture(json!({"auto_ready": "verified"}));
    atomic_write(d.stub_pane_file(&mock, "st", "mode"), "1");
    let err = recover(&d, "st", "m1").unwrap_err();
    assert_recover_refused(&d, &mock, "st", "m1", err, "pane_mode");
}

/// CAD-152 (qa-pr227 note 4): the durable record precedes the Enter —
/// when it cannot be written, nothing is sent.
#[test]
fn recover_submit_sends_nothing_when_the_record_cannot_be_written() {
    let (d, mock, _token) = recover_fixture(json!({"auto_ready": "verified"}));
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER cad152_no_record BEFORE INSERT ON events \
         WHEN NEW.kind='submit_recovered' BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )
    .unwrap();
    let err = recover(&d, "st", "m1").unwrap_err();
    assert!(err.to_string().contains("could not be recorded"), "{err}");
    assert_recover_refused(&d, &mock, "st", "m1", err, "record");
}

/// CAD-152 (qa-pr227 note 5, note 7): an Enter the TUI drops leaves the
/// draft staged — no positive evidence, so `unconfirmed` (CLI exit 1),
/// recorded as such, never retried: a second recovery refuses.
#[test]
fn recover_submit_reports_unconfirmed_when_the_enter_is_dropped() {
    let (d, mock, _token) = recover_fixture(json!({"auto_ready": "verified"}));
    atomic_write(d.stub_pane_file(&mock, "st", "hold-enter"), "1");
    let (ok, out, err) = d.operator_cadence(&["agent", "recover-submit", "st", "--message", "m1"]);
    assert!(!ok, "unconfirmed must exit non-zero: {out}\n{err}");
    let out: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(out["state"], "unconfirmed", "{out}");
    assert_eq!(out["after"]["input_nonempty"], true, "{out}");
    assert_eq!(pane_keys(&d, &mock.dir, "st"), (1, 2));
    let sent = recover_events(&d, "st", "submit_recovered");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["result"], "unconfirmed", "{sent:?}");
    let e = recover(&d, "st", "m1").unwrap_err().to_string();
    assert!(e.contains("(already_submitted)"), "{e}");
    assert_eq!(pane_keys(&d, &mock.dir, "st"), (1, 2));
}

/// CAD-152 (qa-pr227 I2): the operator and the agent's PM recover the
/// same stuck draft at once while the TUI keeps dropping the Enter —
/// the draft stays byte-identical, so every pane check passes for both.
/// Exactly one Enter goes out and one recovery is recorded; the other
/// caller is refused as already submitted.
#[test]
fn recover_submit_concurrent_operator_and_pm_send_one_enter() {
    let d = TestDaemon::start();
    let mock = d.mock_stub();
    let mut p = guard_panes(&d);
    d.register_stub("st", json!({"upstream": "pm", "auto_ready": "verified"}));
    d.wait_agent("st", "idle", 20);
    stuck_draft(&d, &mock, "st", "m1");
    atomic_write(d.stub_pane_file(&mock, "st", "hold-enter"), "1");
    let req = json!({"alias": "st", "message": "m1"});
    let (operator, pm) = std::thread::scope(|scope| {
        let operator = scope.spawn(|| d.operator_rpc("agent_recover_submit", req.clone()));
        let pm = p.pm.rpc(&d.state, "agent_recover_submit", req.clone());
        (operator.join().unwrap(), pm)
    });
    // One Enter on top of the delivery's, whatever else happened.
    assert_eq!(
        pane_keys(&d, &mock.dir, "st"),
        (1, 2),
        "{}",
        tmux_log(&d, &mock.dir)
    );
    let pm_ok = pm["ok"] == true;
    let operator_ok = operator.is_ok();
    assert!(
        pm_ok != operator_ok,
        "exactly one recovery must go through: operator {operator:?}, pm {pm}"
    );
    let refusal = if pm_ok {
        operator.unwrap_err().to_string()
    } else {
        frame_err(&pm)
    };
    assert!(refusal.contains("(already_submitted)"), "{refusal}");
    assert_eq!(recover_events(&d, "st", "submit_recovered").len(), 1);
}

/// CAD-152 (qa-pr227 I1): the recovery Enter restarts the CAD-250
/// report clock — a lost submit found late is not fenced moments after
/// the worker finally receives it.
#[test]
fn recover_submit_restarts_the_report_clock() {
    let (d, _mock, _token) = recover_fixture(json!({"auto_ready": "verified",
                                                    "report_timeout_secs": 30}));
    // Pasted 26 s ago: 4 s of the bound left before the recovery.
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute("UPDATE messages SET started=started-26 WHERE id='m1'", [])
        .unwrap();
    let r = recover(&d, "st", "m1").unwrap();
    assert_eq!(r["state"], "submitted", "{r}");
    let waiting =
        d.rpc("agent_show", json!({"alias": "st"})).unwrap()["agent"]["awaiting_report"].clone();
    assert_eq!(waiting["message"], "m1", "{waiting}");
    assert!(
        waiting["since_secs"].as_u64().unwrap() < 20,
        "the clock restarts at the recovery: {waiting}"
    );
    // Past the original bound: still running, not fenced.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        assert_eq!(d.message_state("st", "m1"), "running");
        let agent = d.rpc("agent_show", json!({"alias": "st"})).unwrap()["agent"].clone();
        assert_ne!(agent["state"], "attention", "{agent}");
        thread::sleep(Duration::from_millis(250));
    }
}

/// A message row by id, from `agent_show`.
fn shown_message(d: &TestDaemon, alias: &str, id: &str) -> Value {
    d.rpc("agent_show", json!({"alias": alias})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
        .cloned()
        .unwrap_or(Value::Null)
}

/// CAD-158: only the operator or the recipient's own PM may send an
/// urgent or superseding message — one route per caller kind, each
/// refusal naming the steering rule and changing nothing; the caller is
/// derived from the connection and stamped on every superseded row.
#[test]
fn steering_send_caller_rule_per_route() {
    let d = TestDaemon::start();
    let mut p = guard_panes(&d);
    let w3 = LaneShell::spawn(p._home.path());
    plant_member_pane(&d, "w3", "inbox", Some("pm"), w3.pid());
    let mut w3 = w3;
    // Planted rows own no actor: w1's queue stays queued.
    for id in ["q1", "q2", "q3"] {
        d.send("w1", json!({"text": format!("stale {id}"), "message": id}))
            .unwrap();
    }
    let steer = |id: &str, target: &str| {
        json!({"alias": target, "text": "the current instruction", "message": id,
               "priority": "urgent", "supersedes": ["q1"]})
    };
    let refused: Vec<(&str, Value, &str)> = vec![
        // A worker to its own PM: never urgent.
        (
            "w1->pm",
            p.w1.rpc(&d.state, "agent_send", steer("r1", "pm")),
            "agent 'w1'",
        ),
        // A worker on itself.
        (
            "w1->w1",
            p.w1.rpc(&d.state, "agent_send", steer("r2", "w1")),
            "agent 'w1'",
        ),
        // A peer worker in the same group.
        (
            "w3->w1",
            w3.rpc(&d.state, "agent_send", steer("r3", "w1")),
            "agent 'w3'",
        ),
        // Another group's PM.
        (
            "pm2->w1",
            p.pm2.rpc(&d.state, "agent_send", steer("r4", "w1")),
            "agent 'pm2'",
        ),
    ];
    for (route, frame, who) in &refused {
        let e = frame_err(frame);
        assert!(e.contains("steering rule"), "{route}: {frame}");
        assert!(e.contains(who), "{route}: {frame}");
    }
    // Tied to no pane and not provably the operator.
    let r = unprovable_rpc(&d, "agent_send", steer("r5", "w1"));
    assert!(frame_err(&r).contains("not provably the operator"), "{r}");
    // A claimed identity is refused, never read.
    let mut forged = steer("r6", "w1");
    forged["by"] = json!("operator");
    let r = p.w1.rpc(&d.state, "agent_send", forged);
    assert!(frame_err(&r).contains("'by' is not accepted"), "{r}");
    // Urgent alone is gated too — and the CLI path from a worker pane.
    let r = p.w1.rpc(
        &d.state,
        "agent_send",
        json!({"alias": "pm", "text": "done", "message": "r7", "priority": "urgent"}),
    );
    assert!(frame_err(&r).contains("steering rule"), "{r}");
    let (rc, out) = p.w1.cadence(
        &d.state,
        "send pm --text done --message r8 --priority urgent",
    );
    assert_ne!(rc, 0, "{out}");
    assert!(out.contains("steering rule"), "{out}");
    // Nothing changed on any refused route.
    for id in ["q1", "q2", "q3"] {
        assert_eq!(shown_message(&d, "w1", id)["state"], "queued", "{id}");
    }
    for id in ["r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8"] {
        for alias in ["w1", "pm"] {
            assert!(
                shown_message(&d, alias, id).is_null(),
                "{id} queued on {alias}"
            );
        }
    }

    // The recipient's PM may.
    let r = p.pm.rpc(&d.state, "agent_send", steer("s1", "w1"));
    assert_eq!(r["ok"], true, "{r}");
    let q1 = shown_message(&d, "w1", "q1");
    assert_eq!(q1["state"], "cancelled");
    assert_eq!(q1["result"]["reason"], "superseded by s1");
    assert_eq!(q1["result"]["by"], "pm");
    assert_eq!(q1["result"]["by_kind"], "agent");
    assert_eq!(shown_message(&d, "w1", "s1")["priority"], "urgent");
    // So may the operator, through the CLI.
    let (ok, stdout, stderr) = d.operator_cadence(&[
        "send",
        "w1",
        "--text",
        "one current instruction",
        "--message",
        "s2",
        "--priority",
        "urgent",
        "--supersedes",
        "q2,q3",
    ]);
    assert!(ok, "{stdout}{stderr}");
    for id in ["q2", "q3"] {
        let m = shown_message(&d, "w1", id);
        assert_eq!(m["state"], "cancelled", "{id}");
        assert_eq!(m["result"]["reason"], "superseded by s2");
        assert_eq!(m["result"]["by"], "operator");
    }
    // A supersede naming an already superseded row changes nothing.
    let (ok, stdout, stderr) = d.operator_cadence(&[
        "send",
        "w1",
        "--text",
        "again",
        "--message",
        "s3",
        "--supersedes",
        "s2,q1",
    ]);
    assert!(!ok, "{stdout}");
    assert!(
        stderr.contains("message 'q1' is cancelled") && stderr.contains("nothing changed"),
        "{stderr}"
    );
    assert_eq!(shown_message(&d, "w1", "s2")["state"], "queued");
    assert!(shown_message(&d, "w1", "s3").is_null());
}

/// CAD-158 acceptance 4, end to end on a managed (fake) actor: an urgent
/// message sent while a turn is parked on an open approval neither
/// interrupts the turn nor answers the approval; once the turn ends it
/// is delivered ahead of the normal messages queued before it, and
/// those keep their FIFO order. A managed actor's loop is serial, so
/// this does not exercise the CAD-250 hold —
/// `pty_urgent_waits_for_the_held_turn_then_goes_first` does.
#[test]
fn urgent_waits_for_the_open_approval_then_goes_first() {
    let d = TestDaemon::start();
    d.register("w1");
    d.wait_agent("w1", "idle", 10);
    d.send("w1", json!({"text": "NEED_INPUT:hold", "message": "m0"}))
        .unwrap();
    d.wait_agent("w1", "waiting_input", 10);
    for id in ["n1", "n2"] {
        d.send("w1", json!({"text": format!("normal {id}"), "message": id}))
            .unwrap();
    }
    // CAD-482: --priority/--supersedes is the operator's send (CAD-149)
    // — assert it through the seam, not the runner's ancestry.
    d.operator_send(
        "w1",
        json!({"text": "urgent correction", "message": "u1", "priority": "urgent"}),
    )
    .unwrap();
    // The open approval and its turn are untouched.
    assert_eq!(shown_message(&d, "w1", "m0")["state"], "running");
    assert_eq!(shown_message(&d, "w1", "u1")["state"], "queued");
    let requests = d
        .operator_rpc("agent_requests", json!({"alias": "w1"}))
        .unwrap();
    let list = requests["requests"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{requests}");
    let handle = list[0]["request"].as_str().unwrap().to_string();
    d.operator_rpc(
        "agent_respond",
        json!({"alias": "w1", "request": handle, "decision": "accept"}),
    )
    .unwrap();
    for id in ["m0", "u1", "n1", "n2"] {
        d.wait_message("w1", id, &["completed"], 15);
    }
    let started: Vec<String> = d
        .events("w1")
        .iter()
        .filter(|e| e["kind"] == "submitting")
        .map(|e| e["payload"]["message"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(started, ["m0", "u1", "n1", "n2"]);
}

/// CAD-158 acceptance 4 on a pty lane (PR #252 QA N3): the CAD-250 hold
/// is what keeps an urgent message from interrupting a running turn. A
/// pty worker holds one unreported turn; an urgent message queued behind
/// it stays `queued` even while the actor demonstrably keeps claiming
/// (a routed notice queued after it is delivered). The report is the
/// boundary: the urgent message is the next turn, ahead of the normal
/// ones queued before it, which keep their order.
#[test]
fn pty_urgent_waits_for_the_held_turn_then_goes_first() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    d.register("helper");
    d.register_stub("w1", json!({"auto_ready": "verified"}));
    d.wait_agent("helper", "idle", 10);
    d.wait_agent("w1", "idle", 20);
    d.send("w1", json!({"text": "held turn", "message": "h0"}))
        .unwrap();
    let token0 = pty_token(&d, "w1", "h0");
    for id in ["n1", "n2"] {
        d.send("w1", json!({"text": format!("normal {id}"), "message": id}))
            .unwrap();
    }
    // CAD-482: --priority/--supersedes is the operator's send (CAD-149)
    // — assert it through the seam, not the runner's ancestry.
    d.operator_send(
        "w1",
        json!({"text": "urgent correction", "message": "u1", "priority": "urgent"}),
    )
    .unwrap();
    // The actor keeps claiming past the hold: a routed notice queued
    // after u1 is pasted, while u1 stays queued behind the held turn.
    d.send(
        "helper",
        json!({"text": "ping", "message": "x1", "reply_to": "w1"}),
    )
    .unwrap();
    d.wait_message("helper", "x1", &["completed"], 15);
    let routed = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["source"] == "worker_result")
        .expect("routed result queued on w1")["id"]
        .as_str()
        .unwrap()
        .to_string();
    d.wait_message("w1", &routed, &["completed"], 20);
    assert_eq!(d.message_state("w1", "h0"), "running");
    for id in ["u1", "n1", "n2"] {
        assert_eq!(d.message_state("w1", id), "queued", "{id}");
    }
    // The report is the safe boundary: u1 is the next turn.
    d.report("h0", &token0, "result", "done").unwrap();
    let token1 = pty_token(&d, "w1", "u1");
    assert_eq!(d.message_state("w1", "n1"), "queued");
    assert_eq!(d.message_state("w1", "n2"), "queued");
    d.report("u1", &token1, "result", "done").unwrap();
    pty_token(&d, "w1", "n1");
    assert_eq!(d.message_state("w1", "n2"), "queued");
}

/// CAD-520 F24: `send --nudge` on a busy Devin pane pastes into the
/// "Guide Devin while it works" box. The first Enter only *queues* the
/// draft in the TUI — its own `Press Enter to send queued messages`
/// invitation takes the second Enter that flushes the steer into the
/// running turn. The nudge completes at the confirmed paste, never
/// holds a turn. A busy pane without the guide watermark refuses the
/// nudge (busy is still busy), and a modal menu covering the box does
/// too — steering is only ever the live guide box.
/// Ordering: the actor's queue is strictly head-first, so the nudges
/// run before the wedged task below can sit at the head.
#[test]
fn pty_devin_busy_nudge_flushes_the_send_queue() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    let keys = |want| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let got = pane_keys(&d, &mock.dir, "dv1");
            if got == want {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "pane_keys {got:?}, wanted {want:?}: {}",
                tmux_log(&d, &mock.dir)
            );
            thread::sleep(Duration::from_millis(100));
        }
    };
    let gate_reason = |id: &str| {
        d.wait_event_where("dv1", "gate_wait", |e| e["payload"]["message"] == id, 15)["payload"]
            ["reason"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    let busy = |watermark: &str| {
        atomic_write(d.pane_file(&mock, "dv1", "inputbox"), watermark);
        atomic_write(
            d.pane_file(&mock, "dv1", "status"),
            "⠸ Thinking · 12s (esc twice to interrupt)\n",
        );
    };
    busy("Guide Devin while it works\n");
    let probe = d.rpc("agent_probe", json!({"alias": "dv1"})).unwrap();
    assert_eq!(probe["idle"], false, "{probe}");
    assert_eq!(probe["busy_marker"], true, "{probe}");
    assert_eq!(probe["steerable"], true, "{probe}");

    // The steer: the submit Enter stages the draft in the TUI's queue,
    // the queue invitation's flush Enter sends it — one paste, two
    // Enters, and the steer echoes into the transcript like a turn.
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "steer toward the small fix",
               "message": "n1", "nudge": true}),
    )
    .unwrap();
    let n = d.wait_message("dv1", "n1", &["completed"], 25);
    assert_eq!(n["result"]["via"], "pty_nudge", "{n}");
    assert_eq!(n["nudge"], true, "{n}");
    keys((1, 2));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let screen =
            std::fs::read_to_string(d.pane_file(&mock, "dv1", "screen")).unwrap_or_default();
        if screen.contains("> steer toward the small fix") {
            break;
        }
        assert!(Instant::now() < deadline, "nudge never echoed: {screen}");
        thread::sleep(Duration::from_millis(100));
    }
    // It never owned a turn.
    assert!(d
        .events("dv1")
        .iter()
        .all(|e| !(e["kind"] == "turn_started" && e["payload"]["message"] == "n1")));

    // Busy but no guide box — the status row alone makes a busy pane
    // whose input takes no steering: the nudge refuses like any send.
    busy("Ask Devin to build features, fix bugs, or work on your code\n");
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "no box to steer into",
               "message": "n2", "nudge": true}),
    )
    .unwrap();
    assert!(gate_reason("n2").contains("busy"));
    assert_eq!(
        pane_keys(&d, &mock.dir, "dv1"),
        (1, 2),
        "a refused nudge pasted"
    );
    // The guide box back: the queued nudge steers in on its next gate
    // retry — wedged, not dropped.
    busy("Guide Devin while it works\n");
    d.wait_message("dv1", "n2", &["completed"], 25);
    keys((2, 4));

    // A modal menu over the box is never steerable — the nudge waits
    // out the menu instead of keying text into a selection.
    atomic_write(d.pane_file(&mock, "dv1", "tui-state"), DEVIN_MENU);
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "wait out the menu",
               "message": "n3", "nudge": true}),
    )
    .unwrap();
    assert!(gate_reason("n3").contains("approval menu"));
    assert_eq!(
        pane_keys(&d, &mock.dir, "dv1"),
        (2, 4),
        "a menued nudge pasted"
    );
    std::fs::remove_file(d.pane_file(&mock, "dv1", "tui-state")).unwrap();
    d.wait_message("dv1", "n3", &["completed"], 25);
    keys((3, 6));

    // Ordinary work still refuses the busy pane — the refusal is the
    // screen's busy verdict, not a missing claim.
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "plain task", "message": "w1"}),
    )
    .unwrap();
    assert!(gate_reason("w1").contains("busy"));
    assert_eq!(d.message_state("dv1", "w1"), "queued");
}

/// CAD-520 F28: a paste the transcript never echoes is not proof of a
/// drop. `.noecho` leaves the body off-screen but moves the pane to a
/// busy status — the deadline re-probe sees the gate's idle probe went
/// busy, so the turn is `running`, not `unknown`: no fence, no
/// `paste_not_rendered`, nothing for the operator.
#[test]
fn pty_devin_render_miss_reprobe_accepts_the_busy_turn() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    atomic_write(d.pane_file(&mock, "dv1", "noecho"), "1");
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "quietly taken", "message": "m1"}),
    )
    .unwrap();
    let token = pty_token(&d, "dv1", "m1");
    assert!(token.starts_with("pty-"), "{token}");
    assert!(d
        .events("dv1")
        .iter()
        .all(|e| e["kind"].as_str() != Some("paste_not_rendered")));
    let agent = d.rpc("agent_show", json!({"alias": "dv1"})).unwrap()["agent"].clone();
    assert_eq!(agent["state"], "busy", "{agent}");
    pty_report_done(&d, "dv1", "m1");
}

/// CAD-520 F28 evidence: a genuinely dropped paste still fences — the
/// re-probe only clears a pane that shows work. The `paste_not_rendered`
/// event carries both screen tails plus the admitting probe and the
/// re-probe verdict, so a fenced turn records what the pane showed.
#[test]
fn pty_devin_render_miss_records_reprobe_evidence() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    d.register_devin("dv1", None);
    d.wait_agent("dv1", "idle", 20);
    atomic_write(d.pane_file(&mock, "dv1", "swallow"), "1");
    d.rpc(
        "agent_send",
        json!({"alias": "dv1", "text": "dropped on the floor", "message": "m1"}),
    )
    .unwrap();
    d.wait_agent("dv1", "attention", 30);
    assert_eq!(d.message_state("dv1", "m1"), "unknown");
    let miss = d.wait_event("dv1", "paste_not_rendered", 10);
    let reprobe = &miss["payload"]["reprobe"];
    assert_eq!(
        reprobe["probe"]["idle"], true,
        "the pane still probes idle — the paste truly dropped: {miss}"
    );
    assert_eq!(reprobe["steer"], false, "{miss}");
    assert!(
        miss["payload"]["claim_probe"]["idle"].as_bool() == Some(true),
        "{miss}"
    );
    // The pre-fence `submitting` row kept the pane from ever claiming
    // the send as work: no turn_started, no ready_claimed consumed
    // twice.
    assert!(d
        .events("dv1")
        .iter()
        .all(|e| e["kind"].as_str() != Some("turn_started")));
}

/// CAD-520 F26 watchdog: a queued head that outlives
/// `delivery_watch_secs` while the pane probes *idle* is the wedge —
/// the F26 failure left a message queued for an hour in front of a
/// ready pane with no log entry. The daemon fires one
/// `delivery_stalled` per tracked head (event + agent flag + needs-me
/// row), never re-fires while it sits, and clears when the wedge does.
/// The wedge here is a tmux mode: the gate probe refuses on
/// `pane_in_mode` while the screen sample still reads idle — exactly
/// the split verdict that makes a queued-on-idle wait undetectable
/// without the watchdog.
#[test]
fn pty_delivery_stalled_fires_once_and_clears() {
    let d = TestDaemon::start();
    let mock = d.mock_devin();
    stall_sample(1);
    d.register_devin_opts("dv", json!({"delivery_watch_secs": 2}));
    d.wait_agent("dv", "idle", 20);
    atomic_write(d.pane_file(&mock, "dv", "mode"), "1");
    d.rpc(
        "agent_send",
        json!({"alias": "dv", "text": "stuck behind a mode", "message": "m1"}),
    )
    .unwrap();
    let wait = d.wait_event_where("dv", "gate_wait", |e| e["payload"]["message"] == "m1", 15);
    assert!(
        wait["payload"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("tmux mode"),
        "{wait}"
    );
    let ev = d.wait_event("dv", "delivery_stalled", 30);
    assert_eq!(ev["payload"]["message"], "m1", "{ev}");
    assert_eq!(ev["payload"]["bound_secs"], 2, "{ev}");
    assert_eq!(ev["payload"]["probe"]["idle"], true, "{ev}");
    let agent = d.rpc("agent_show", json!({"alias": "dv"})).unwrap()["agent"].clone();
    assert_eq!(agent["delivery_stalled"]["message"], "m1", "{agent}");
    assert_eq!(agent["delivery_stalled"]["verdict"], "idle", "{agent}");
    // The overview needs-me row names the agent, the wedged message
    // and the probe's verdict — not just that something stalled.
    let home = TempDir::new().unwrap();
    let view = overview_at(home.path(), &d.state, None, &[]);
    let row = view["needs_me"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["kind"] == "delivery_stalled")
        .expect("no delivery_stalled needs-me row");
    let title = row["title"].as_str().unwrap_or("");
    assert!(
        title.contains("dv") && title.contains("m1") && title.contains("idle"),
        "{row}"
    );
    // Once per head: a few more idle samples re-fire nothing.
    thread::sleep(Duration::from_secs(4));
    assert_eq!(
        wait_event_count(&d, "dv", "delivery_stalled", 1, 2).len(),
        1
    );
    // Clearing the mode delivers — the wedge was never the message.
    std::fs::remove_file(d.pane_file(&mock, "dv", "mode")).unwrap();
    pty_report_done(&d, "dv", "m1");
    stall_sample(0);
}
