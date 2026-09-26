//! board_ui_daemon: area tests split from tests/board.rs (CAD-537).
//! Board e2e: the `cadence issue` CLI against a temp PM dir, and the
//! `cadence ui` HTTP server in-process.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

use cadence_agent::client;
use cadence_agent::store::Store;
use serde_json::json;
use serde_json::Value;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;

/// CAD-471: CI runs outside every pane, so the suite from an agent pane
/// is only covered here. Re-run the probe below in a child whose
/// environment carries an agent's alias, the shape of a run from a
/// pane: its in-process daemon and board must stop, and the child must
/// finish, instead of the drop's join waiting forever.
#[test]
fn in_process_daemon_stops_from_an_agent_runner() {
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "in_process_daemon_stops_from_an_agent_runner_probe",
            "--ignored",
        ])
        .env("CADENCE_ALIAS", "cad471-runner")
        .env("CAD471_PROBE", "1")
        // A plain libtest child: the outer run's suite lock and
        // nextest markers are not its to honour.
        .env_remove("CADENCE_SUITE_LOCK")
        .env_remove("CADENCE_REVIEW_SUITE_LOCK_HELD")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("NEXTEST") {
            child.env_remove(key);
        }
    }
    let mut child = child.spawn().unwrap();
    let deadline = Instant::now() + DAEMON_STOP_BOUND + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            // Our own child: killing it can never touch another process.
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "the agent-shaped probe hung: its in-process daemon or board did not stop \
                 (a drop waiting on a `shutdown` the caller rule refuses, CAD-471)"
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("1 passed"),
        "the probe must actually run: {text}"
    );
}

#[test]
#[ignore = "run by in_process_daemon_stops_from_an_agent_runner as an agent-shaped child"]
fn in_process_daemon_stops_from_an_agent_runner_probe() {
    if std::env::var("CAD471_PROBE").as_deref() != Ok("1") {
        return;
    }
    assert!(std::env::var("CADENCE_ALIAS").is_ok());
    let d = UiDaemon::start();
    let pm = TempDir::new().unwrap();
    seed(pm.path(), &d.state());
    let (port, board) = start_ui(pm.path().to_path_buf(), d.state());
    // The gate is untouched: this process carries an agent's
    // environment, so it may not stop the daemon over its socket.
    let err = d.rpc_opt("shutdown", json!({})).unwrap_err();
    assert!(err.to_string().contains("carries CADENCE_ALIAS"), "{err}");
    assert_eq!(d.rpc("health", json!({}))["state"], "ready");
    // The in-process stops need no connection: both return.
    drop(board);
    wait_port_closed(port);
    drop(d);
}

/// A dropped [`BoardStop`] closes its board's port within its accept
/// poll (CAD-471) — the board does not serve on for the rest of the run.
#[test]
fn board_stops_when_its_guard_drops() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (port, board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    assert_eq!(http(port, "GET", "/api/health", &host).0, 200);
    drop(board);
    wait_port_closed(port);
}

/// Wait until nothing accepts on `port`; fail after 10 s.
fn wait_port_closed(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_ok() {
        assert!(
            Instant::now() < deadline,
            "the board on {port} still accepts after its stop"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// CAD-254: the write guards stop browsers, not local processes. A
/// board write from a process that descends from a registered pane is
/// that agent's write — its alias lands in the commit and as the
/// comment author, never `operator` — while a peer on no pane's lineage
/// (this test process, standing in for the operator's browser) writes
/// as `operator (ui)` only with the operator's session (CAD-313) and is
/// refused without one. With the store present but the daemon gone the
/// panes are unknowable, so a write is refused, not guessed.
#[test]
fn ui_write_caller_derives_from_pane_ancestry() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");

    // The pane: a bash that waits for a go line, then runs the client
    // as its CHILD — a plain HTTP write over bash's /dev/tcp, so the
    // peer holding the socket descends from the planted pane pid.
    let body = r#"{"body":"from the pane"}"#;
    let request = format!(
        "POST /api/issues/CAD-3/comments HTTP/1.0\r\nHost: {host}\r\n\
         Content-Type: application/json\r\nX-Cadence-Board: 1\r\n\
         Origin: http://{host}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut pane = Command::new("bash")
        .args(["-c", r#"read -r _; bash -c "$CLIENT"; true"#])
        .env(
            "CLIENT",
            r#"exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf '%s' "$REQ" >&3; cat <&3"#,
        )
        .env("PORT", port.to_string())
        .env("REQ", &request)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    plant_pane(&d, "pane-w", pane.id());
    pane.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let mut response = String::new();
    pane.stdout
        .take()
        .unwrap()
        .read_to_string(&mut response)
        .unwrap();
    assert!(pane.wait().unwrap().success());
    assert!(
        response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200"),
        "{response}"
    );
    let json_body = response.split_once("\r\n\r\n").unwrap().1;
    let v: Value = serde_json::from_str(json_body).unwrap();
    let comment = v["issue"]["comments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["body"] == "from the pane")
        .unwrap()
        .clone();
    assert_eq!(comment["author"], "pane-w", "{comment}");
    let (_, last) = git(pm.path(), &["log", "-1", "--format=%B"]);
    assert!(last.contains("(pane-w)"), "{last}");
    assert!(last.contains("Actor: pane-w"), "{last}");
    assert!(!last.contains("operator"), "{last}");

    // On no pane's lineage is not the operator (CAD-313, F1): without
    // a session this test process is refused and nothing is written.
    let commits_before = commits(pm.path());
    let (code, _, body) = write_json(
        port,
        "PATCH",
        "/api/issues/CAD-3",
        &host,
        r#"{"priority":"P1"}"#,
    );
    assert_eq!(code, 403, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["check"], "operator_session_required", "{v}");
    assert_eq!(commits(pm.path()), commits_before);
    // The operator: this test process, signed in.
    let op = sign_in(state.path(), port);
    let (code, _, _) = op_write_json(
        &op,
        port,
        "PATCH",
        "/api/issues/CAD-3",
        &host,
        r#"{"priority":"P1"}"#,
    );
    assert_eq!(code, 200);
    let (_, last) = git(pm.path(), &["log", "-1", "--format=%B"]);
    assert!(last.contains("Actor: operator (ui)"), "{last}");

    // Fail closed: the store exists but no daemon can name the panes.
    let commits_before = commits(pm.path());
    drop(d);
    let (code, _, body) = write_json(
        port,
        "PATCH",
        "/api/issues/CAD-3",
        &host,
        r#"{"priority":"P2"}"#,
    );
    assert_eq!(code, 403, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["check"], "caller_identity", "{v}");
    assert_eq!(commits(pm.path()), commits_before);
}

/// CAD-337: the board relays a model-defaults write to the daemon over
/// its OWN connection, so the daemon's operator gate sees the board
/// process, not the HTTP caller. A caller the board attributes to a
/// pane is an agent and is refused here, before any relay, naming the
/// rule — else an agent could launder the write through the board and
/// land it as the operator's. Nothing changes.
#[test]
fn ui_model_defaults_refuses_pane_agent() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    let doc = r#"{"expected_revision":0,"config":{"schema":1,"providers":{"claude":{"default":{"mode":"model","model":"forged-model"},"roles":{}}}}}"#;
    let request = format!(
        "POST /api/settings/model-defaults HTTP/1.0\r\nHost: {host}\r\n\
         Content-Type: application/json\r\nX-Cadence-Board: 1\r\n\
         Origin: http://{host}\r\nContent-Length: {}\r\n\r\n{doc}",
        doc.len()
    );
    let mut pane = Command::new("bash")
        .args(["-c", r#"read -r _; bash -c "$CLIENT"; true"#])
        .env(
            "CLIENT",
            r#"exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf '%s' "$REQ" >&3; cat <&3"#,
        )
        .env("PORT", port.to_string())
        .env("REQ", &request)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    plant_pane(&d, "pane-m", pane.id());
    pane.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let mut response = String::new();
    pane.stdout
        .take()
        .unwrap()
        .read_to_string(&mut response)
        .unwrap();
    assert!(pane.wait().unwrap().success());
    assert!(
        response.starts_with("HTTP/1.1 403") || response.starts_with("HTTP/1.0 403"),
        "{response}"
    );
    let v: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(v["check"], "operator_only", "{v}");
    let msg = v["error"].as_str().unwrap_or_default();
    assert!(msg.contains("pane-m") && msg.contains("operator"), "{v}");
    let (code, body) = http(port, "GET", "/api/settings/model-defaults", &host);
    assert_eq!(code, 200, "{body}");
    let current: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(current["revision"], 0, "{current}");
}

/// A POST of `body` as a comment on CAD-3 — the raw request a pane-side
/// client writes over bash's /dev/tcp.
fn comment_request(host: &str, body: &str) -> String {
    let body = format!(r#"{{"body":"{body}"}}"#);
    format!(
        "POST /api/issues/CAD-3/comments HTTP/1.0\r\nHost: {host}\r\n\
         Content-Type: application/json\r\nX-Cadence-Board: 1\r\n\
         Origin: http://{host}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// The comment `body` in a board write's HTTP reply.
fn replied_comment(response: &str, body: &str) -> Value {
    assert!(
        response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200"),
        "{response}"
    );
    let v: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    v["issue"]["comments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["body"] == body)
        .unwrap()
        .clone()
}

/// CAD-263: a `setsid`'d child of a registered pane has no pane on its
/// `/proc` ancestry, but a detach keeps stdio — it still holds the
/// pane's pty, the process signal the board shares with the daemon, so
/// the write is the agent's, never `operator`'s. The client
/// double-forks (`setsid -f`) and connects only once the pane is
/// provably off its ancestry, so ancestry cannot carry it.
#[test]
fn ui_write_caller_attributes_a_setsid_child_of_a_pane() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let out_dir = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    let out = out_dir.path().join("response");
    // The client waits until the pane pid is off its ancestry (the
    // `setsid -f` intermediate has exited and it was reparented), then
    // writes over bash's /dev/tcp and lands the reply atomically. Its
    // stdio stays the pane's pty.
    let client = r#"
        on_pane_lineage() {
            p=$$
            while [ "$p" -gt 1 ]; do
                [ "$p" = "$PANE" ] && return 0
                p=$(awk '/^PPid:/{print $2}' "/proc/$p/status") || return 0
                [ -n "$p" ] || return 0
            done
            return 1
        }
        while on_pane_lineage; do sleep 0.02; done
        exec 3<>"/dev/tcp/127.0.0.1/$PORT"
        printf '%s' "$REQ" >&3
        cat <&3 >"$OUT.tmp" && mv "$OUT.tmp" "$OUT"
    "#;
    let mut pane = Command::new("python3")
        .args([
            "-c",
            PTY_PANE_PY,
            r#"read -r _; PANE=$$ setsid -f bash -c "$CLIENT"; read -r _; true"#,
        ])
        .env("CADENCE_ALIAS", "pane-s")
        .env("CLIENT", client)
        .env("PORT", port.to_string())
        .env("REQ", comment_request(&host, "from a detached child"))
        .env("OUT", &out)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(pane.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    let pane_pid: u32 = first.trim().parse().unwrap();
    plant_pane(&d, "pane-s", pane_pid);
    let mut stdin = pane.stdin.take().unwrap();
    stdin.write_all(b"go\n").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !out.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the detached client never answered"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    stdin.write_all(b"done\n").unwrap();
    drop(stdin);
    assert!(pane.wait().unwrap().success());
    let response = std::fs::read_to_string(&out).unwrap();
    let comment = replied_comment(&response, "from a detached child");
    assert_eq!(comment["author"], "pane-s", "{comment}");
    let (_, last) = git(pm.path(), &["log", "-1", "--format=%B"]);
    assert!(last.contains("Actor: pane-s"), "{last}");
    assert!(!last.contains("operator"), "{last}");
}

/// CAD-263 review: `CADENCE_ALIAS` is caller-chosen, so on the board it
/// never attributes by itself. A pane-less process exporting a
/// registered pane's alias — no ancestry, no pane pty — is not that
/// agent, and (CAD-313, flipped deliberately per ADR 0004 §6) not the
/// operator either: without a session it is refused and writes nothing.
#[test]
fn ui_write_caller_ignores_an_uncorroborated_env_alias() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    // pane-b is a registered, live pane the client is unrelated to.
    let mut pane_b = Command::new("sleep")
        .arg("600")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    plant_pane(&d, "pane-b", pane_b.id());
    let client = Command::new("bash")
        .args([
            "-c",
            r#"exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf '%s' "$REQ" >&3; cat <&3"#,
        ])
        .env("CADENCE_ALIAS", "pane-b")
        .env("PORT", port.to_string())
        .env("REQ", comment_request(&host, "forged alias"))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .unwrap();
    let _ = pane_b.kill();
    let _ = pane_b.wait();
    let response = String::from_utf8(client.stdout).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 403") || response.starts_with("HTTP/1.0 403"),
        "{response}"
    );
    assert!(response.contains("operator_session_required"), "{response}");
    let (_, last) = git(pm.path(), &["log", "-1", "--format=%B"]);
    assert!(!last.contains("forged alias"), "{last}");
    assert!(!last.contains("pane-b"), "{last}");
}

/// CAD-276 PINS AN ACCEPTED RESIDUAL — see the PM decision on CAD-276
/// and the `src/peer.rs` module doc. The pty-on-stdio tie is
/// caller-choosable: a same-uid process on NO pane's ancestry (a child
/// of this test, no `CADENCE_ALIAS`) opens a registered pane's
/// `/dev/pts/N` onto its stderr and is attributed as that pane's agent
/// — lateral authorship forgery, no privilege over `operator (ui)`.
/// The tie stays because dropping it sends `setsid` children of panes
/// back to `operator (ui)` (an escalation). A future fix
/// (operator-by-positive-proof, or a second signal) must flip this
/// test DELIBERATELY: the expected author then stops being `pane-v`.
#[test]
fn ui_write_caller_pty_tie_is_forgeable_residual_pinned() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    // The victim pane: a bash whose stdio is a real pty, idling.
    let mut pane = Command::new("python3")
        .args(["-c", PTY_PANE_PY, "read -r _; true"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(pane.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    let pane_pid: u32 = first.trim().parse().unwrap();
    plant_pane(&d, "pane-v", pane_pid);
    let pts = std::fs::read_link(format!("/proc/{pane_pid}/fd/0")).unwrap();
    assert!(pts.to_string_lossy().starts_with("/dev/pts/"), "{pts:?}");
    // The forger: this test's child — never on the pane's ancestry —
    // with the pane's pts opened onto its stderr, nothing else.
    let client = Command::new("bash")
        .args([
            "-c",
            r#"exec 2>"$PTS"; exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf '%s' "$REQ" >&3; cat <&3"#,
        ])
        .env_remove("CADENCE_ALIAS")
        .env("PTS", &pts)
        .env("PORT", port.to_string())
        .env("REQ", comment_request(&host, "forged via pty"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let mut stdin = pane.stdin.take().unwrap();
    stdin.write_all(b"done\n").unwrap();
    drop(stdin);
    let _ = pane.wait();
    let response = String::from_utf8(client.stdout).unwrap();
    let comment = replied_comment(&response, "forged via pty");
    assert_eq!(
        comment["author"], "pane-v",
        "CAD-276 residual changed — if deliberate, flip this pin: {comment}"
    );
    let (_, last) = git(pm.path(), &["log", "-1", "--format=%B"]);
    assert!(last.contains("Actor: pane-v"), "{last}");
}

/// Seed the tracker and daemon-side world for the binding tests:
/// pm + wk fake agents, a job bound to `issue`, one task for `wk`
/// dispatched so the task is live. Returns (job_id, task_id).
fn bound_job(pm: &Path, d: &UiDaemon, issue: &str) -> (String, String) {
    let cwd = pm.to_str().unwrap();
    let _ = d.operator_rpc(
        "agent_register",
        json!({"alias": "pm", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd}),
    );
    // The worker must sit in the pm's group or dispatch refuses it.
    let _ = d.operator_rpc(
        "agent_register",
        json!({"alias": "wk", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd,
               "params": "{\"upstream\":\"pm\"}"}),
    );
    let spec = pm.join("spec.md");
    std::fs::write(&spec, "# spec\n").unwrap();
    let job = d.rpc(
        "job_new",
        json!({"pm": "pm", "spec": spec, "spec_sha256": "test",
               "issue": issue, "title": "bound job"}),
    );
    let job_id = job["job"]["id"].as_str().unwrap().to_string();
    // The acceptance text names a DIFFERENT issue on purpose — the
    // kickoff body embeds it ahead of the real "tracks issue <id>"
    // line, so a message-text scan would bind `wk` to CAD-1 while the
    // task join binds the job's real issue.
    let task = d.rpc(
        "task_new",
        json!({"job": job_id, "assignee": "wk", "title": "worker task",
               "acceptance": "verify against CAD-1"}),
    );
    let task_id = task["task"]["id"].as_str().unwrap().to_string();
    let _ = d.operator_rpc("task_dispatch", json!({"task": task_id, "by": "operator"}));
    (job_id, task_id)
}

#[test]
fn ui_job_state_drives_status_and_binding() {
    let pm = TempDir::new().unwrap();
    let d = UiDaemon::start();
    seed(pm.path(), &d.state());
    // CAD-3 is the leaf — CAD-1 is a container whose roll-up legitimately
    // outranks any job.
    let (_, task_id) = bound_job(pm.path(), &d, "CAD-3");
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");
    // CAD-313: a board write is the operator's only with a session —
    // this test process signs in and writes as `operator (ui)`.
    let op = sign_in(&d.state(), port);
    let write_json = |port: u16, method: &str, path: &str, host: &str, body: &str| {
        op_write_json(&op, port, method, path, host, body)
    };

    // Card: job state wins over notes/file and binds the agent.
    let (code, body) = http(port, "GET", "/api/issues", &host);
    assert_eq!(code, 200);
    let issues: Value = serde_json::from_str(&body).unwrap();
    let card = issues["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "CAD-3")
        .cloned()
        .expect("CAD-3 card");
    assert_eq!(card["status_source"], "job");
    assert!(
        matches!(card["status"].as_str(), Some("doing" | "review" | "done")),
        "job-derived status: {}",
        card["status"]
    );
    let bound = &card["agents"];
    assert!(
        bound
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["alias"] == "wk" && a["task"] == task_id),
        "card agents strip: {bound}"
    );

    // Detail: same strip on the drawer payload.
    let (code, body) = http(port, "GET", "/api/issues/CAD-3", &host);
    assert_eq!(code, 200);
    let detail: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(detail["status_source"], "job");
    assert!(
        detail["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["alias"] == "wk" && a["task"] == task_id),
        "detail agents strip: {}",
        detail["agents"]
    );

    // Agents payload: the exact join — wk is `on` CAD-3 through its
    // task, and by_issue carries the strip for the card.
    let (code, body) = http(port, "GET", "/api/agents", &host);
    assert_eq!(code, 200);
    let agents: Value = serde_json::from_str(&body).unwrap();
    let wk = agents["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "wk")
        .cloned()
        .expect("wk row");
    assert!(
        wk["on"].as_array().unwrap().iter().any(|i| i == "CAD-3"),
        "wk.on: {}",
        wk["on"]
    );
    assert!(
        wk["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["task"] == task_id && t["issue"] == "CAD-3"),
        "wk.tasks: {}",
        wk["tasks"]
    );
    assert!(
        agents["by_issue"]["CAD-3"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["alias"] == "wk" && a["task"] == task_id),
        "by_issue.CAD-3: {}",
        agents["by_issue"]["CAD-3"]
    );
    // The kickoff body names CAD-1 (acceptance text) ahead of the real
    // issue line — a message-text scan would bind `wk` there. The join
    // must not.
    let binds_cad1 = agents["by_issue"]["CAD-1"]
        .as_array()
        .map(|v| v.iter().any(|a| a["alias"] == "wk"))
        .unwrap_or(false)
        || wk["on"].as_array().unwrap().iter().any(|i| i == "CAD-1");
    assert!(!binds_cad1, "decoy issue id in the message must not bind");

    // A manual status write against a job-derived status is refused —
    // the same 409 the CLI enforces on notes/rollup-derived statuses.
    let (code, _, body) = write_json(
        port,
        "PATCH",
        "/api/issues/CAD-3",
        &host,
        r#"{"status":"done"}"#,
    );
    assert_eq!(code, 409, "derived status write must conflict: {body}");
    assert!(body.contains("derived"), "409 names the cause: {body}");
}

#[test]
fn ui_overview_surfaces_durable_monitor_alert_and_acknowledges_it() {
    let pm = TempDir::new().unwrap();
    let d = UiDaemon::start();
    seed(pm.path(), &d.state());
    let cwd = pm.path().to_str().unwrap();
    let _ = d.operator_rpc(
        "agent_register",
        json!({"alias": "pm", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd}),
    );
    let _ = d.operator_rpc(
        "agent_register",
        json!({"alias": "wk", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd,
               "params": "{\"upstream\":\"pm\"}"}),
    );
    let spec = pm.path().join("monitor-ui.md");
    std::fs::write(&spec, "# monitor ui\n").unwrap();
    d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "ui-monitor-job", "spec": spec,
               "spec_sha256": "synthetic", "repo": "cadence", "issue": "CAD-3"}),
    );
    d.rpc(
        "task_new",
        json!({"job": "ui-monitor-job", "task": "ui-monitor-task",
               "assignee": "wk", "acceptance": "observe the monitor"}),
    );
    let _ = d.operator_rpc(
        "monitor_register",
        json!({"monitor": "ui-monitor", "project": "cadence",
               "owner": "watchdog", "tasks": ["ui-monitor-task"],
               "interval_secs": 1}),
    );
    let active_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let monitor = d.rpc("monitor_show", json!({"monitor": "ui-monitor"}))["monitor"].clone();
        if monitor["monitoring"] == "active" {
            assert!(monitor["last_success_at"].is_number(), "{monitor}");
            break;
        }
        assert!(
            Instant::now() < active_deadline,
            "monitor never became active: {monitor}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let store = Store::open(&d.state.join("cadence.sqlite3")).unwrap();
    store
        .event_public_scoped(
            "wk",
            "turn_stalled",
            json!({"message": "synthetic-stall", "episode": 1}),
            Some("ui-monitor-job"),
            Some("ui-monitor-task"),
        )
        .unwrap();
    let alert = loop {
        let page = d.rpc("monitor_alerts", json!({"monitor": "ui-monitor"}));
        if let Some(alert) = page["alerts"].as_array().and_then(|a| a.first()) {
            break alert.clone();
        }
        assert!(
            Instant::now() < active_deadline + Duration::from_secs(5),
            "alert not observed: {page}"
        );
        thread::sleep(Duration::from_millis(50));
    };

    // The ack relays through the board's own daemon connection, which
    // the caller rule proves (CAD-384) — an operator-shaped board, so
    // this passes from an agent pane too (CAD-471, as CAD-380).
    let (port, _board) = start_operator_ui(pm.path(), &d.state());
    let host = format!("127.0.0.1:{port}");
    // CAD-313: a board write is the operator's only with a session —
    // this test process signs in and writes as `operator (ui)`.
    let op = sign_in(&d.state(), port);
    let write_json = |port: u16, method: &str, path: &str, host: &str, body: &str| {
        op_write_json(&op, port, method, path, host, body)
    };
    let (code, body) = http(port, "GET", "/api/overview", &host);
    assert_eq!(code, 200, "{body}");
    let overview: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(overview["monitoring"]["state"], "active", "{overview}");
    assert!(
        overview["monitoring"]["last_success_at"].is_number(),
        "{overview}"
    );
    let monitor = &overview["monitoring"]["monitors"][0];
    assert!(monitor["heartbeat_at"].is_number(), "{overview}");
    assert!(monitor["last_check_at"].is_number(), "{overview}");
    assert_eq!(
        monitor["coverage"],
        json!(["ui-monitor-task"]),
        "{overview}"
    );
    assert_eq!(overview["monitoring"]["open_alerts"], 1, "{overview}");
    assert_eq!(overview["monitoring"]["alerts"][0]["project"], "cadence");
    assert_eq!(
        overview["monitoring"]["alerts"][0]["next_owner"],
        "watchdog"
    );
    assert_eq!(
        overview["monitoring"]["alerts"][0]["evidence"]["event_seq"],
        alert["event_seq"]
    );

    let seq = alert["seq"].as_i64().unwrap();
    let (code, _, body) = write_json(
        port,
        "POST",
        &format!("/api/monitors/ui-monitor/alerts/{seq}/ack"),
        &host,
        "{}",
    );
    assert_eq!(code, 200, "{body}");
    let ack: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(ack["alert"]["state"], "acknowledged", "{ack}");

    let (code, body) = http(port, "GET", "/api/overview", &host);
    assert_eq!(code, 200, "{body}");
    let overview: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(overview["monitoring"]["open_alerts"], 0, "{overview}");
    assert_eq!(
        overview["monitoring"]["alerts"][0]["state"], "acknowledged",
        "{overview}"
    );

    // The durable acknowledgement remains behind the board's existing
    // read-only guard; a shared browse-only board cannot claim it handled
    // an operator alert.
    let (read_only_port, _ro_board) = start_ui_opts(pm.path().to_path_buf(), d.state(), |opts| {
        opts.read_only = true;
    });
    let read_only_host = format!("127.0.0.1:{read_only_port}");
    let (code, _, body) = write_json(
        read_only_port,
        "POST",
        &format!("/api/monitors/ui-monitor/alerts/{seq}/ack"),
        &read_only_host,
        "{}",
    );
    assert_eq!(code, 403, "read-only board must refuse monitor ack: {body}");
}

#[test]
fn ui_agent_detail_route_and_guards() {
    let pm = TempDir::new().unwrap();
    let d = UiDaemon::start();
    seed(pm.path(), &d.state());
    let _ = d.operator_rpc(
        "agent_register",
        json!({"alias": "wk", "provider": "fake",
               "endpoint_kind": "fake", "cwd": pm.path().to_str().unwrap()}),
    );
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");

    let (code, body) = http(port, "GET", "/api/agents/wk", &host);
    assert_eq!(code, 200);
    let detail: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(detail["agent"]["alias"], "wk");
    assert_eq!(detail["fenced"], false);
    assert!(detail["events"].is_array());
    assert!(detail["agent"]["capabilities"].is_object());

    // Alias grammar is enforced before the daemon is asked.
    let (code, _) = http(port, "GET", "/api/agents/bad%20alias", &host);
    assert_eq!(code, 400);
    let (code, _) = http(port, "GET", "/api/agents/..%2Fetc", &host);
    assert!(matches!(code, 400 | 404));
    // A well-formed but unknown alias is a daemon-level 404.
    let (code, _) = http(port, "GET", "/api/agents/ghost-1", &host);
    assert_eq!(code, 404);
}

/// CAD-542: `/api/agents/<alias>` relays `agent_events` — an unscoped
/// `Rule::Read` — onto unauthenticated HTTP, so it is exactly as
/// strict as the RPC: a brokered approval's `request_opened` carries
/// routing fields only, never the input-derived text, which stays on
/// the operator/owner/PM-scoped `agent_requests` row.
#[test]
fn ui_agent_detail_request_opened_carries_no_input() {
    let pm = TempDir::new().unwrap();
    let d = UiDaemon::start();
    seed(pm.path(), &d.state());

    // w1's own connection opens the request: a pane child's daemon
    // call derives the agent from /proc ancestry — the same proof the
    // real `mcp-permission` server has.
    let sock = client::socket_path(&d.state());
    let req_file = pm.path().join("open.json");
    std::fs::write(
        &req_file,
        cadence_agent::proto::request(
            "request_open",
            json!({"alias": "w1", "kind": "approval", "tool": "Bash",
                   "input_summary": "rm -rf CANARY-BOARD-5e7f /tmp/x",
                   "input": {"command": "CANARY-BOARD-5e7f"},
                   "request": "h-board"}),
        )
        .to_string(),
    )
    .unwrap();
    let mut pane = Command::new("bash")
        .args(["-c", r#"read -r _; bash -c "$CLIENT"; true"#])
        .env(
            "CLIENT",
            r#"python3 -c 'import socket,sys;s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],"rb").read()+b"\n");print(s.makefile().readline())' "$SOCK" "$REQ""#,
        )
        .env("SOCK", &sock)
        .env("REQ", &req_file)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    plant_pane(&d, "w1", pane.id());
    let conn = rusqlite::Connection::open(d.state().join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET params=?1, state='busy' WHERE alias='w1'",
        rusqlite::params![r#"{"broker_approvals": true}"#],
    )
    .unwrap();
    pane.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let mut frame = String::new();
    pane.stdout
        .take()
        .unwrap()
        .read_to_string(&mut frame)
        .unwrap();
    assert!(pane.wait().unwrap().success());
    assert!(frame.contains("\"ok\":true"), "{frame}");

    // The drawer's feed is the same unscoped lane over HTTP — no
    // canary, whatever it relays. (`agent_events_tail` still dials the
    // pre-CAD-384 name `events`, which no daemon has ever dispatched,
    // so today it relays nothing at all; if that stale call is ever
    // revived the feed must still carry routing fields only — checked
    // below so the revival cannot smuggle the input back.)
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");
    let (code, body) = http(port, "GET", "/api/agents/w1", &host);
    assert_eq!(code, 200, "{body}");
    assert!(!body.contains("CANARY-BOARD-5e7f"), "{body}");
    let detail: Value = serde_json::from_str(&body).unwrap();
    for e in detail["events"].as_array().unwrap() {
        if e["kind"] == "request_opened" && e["payload"]["request"] == "h-board" {
            let payload = &e["payload"];
            assert!(payload.get("input_summary").is_none(), "{e}");
            assert!(payload.get("input").is_none(), "{e}");
        }
    }
}

#[test]
fn ui_stream_sse_and_guards() {
    let pm = TempDir::new().unwrap();
    let d = UiDaemon::start();
    seed(pm.path(), &d.state());
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");

    // Method guard: HEAD on the stream is refused, not hung.
    let (code, _, _) = http_full(port, "HEAD", "/api/stream", &host);
    assert_eq!(code, 405);
    // Host guard applies to the stream exactly like any other route.
    let (code, _, _) = http_full(port, "GET", "/api/stream", "evil.example");
    assert_eq!(code, 421);

    // GET streams SSE: headers first, then frames. The reader emits a
    // `: ping` immediately, so first bytes arrive fast.
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
    write!(s, "GET /api/stream HTTP/1.0\r\nHost: {host}\r\n\r\n").unwrap();
    let mut raw = Vec::new();
    let mut tmp = [0u8; 2048];
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                raw.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&raw);
                if text.contains(": ping") {
                    break;
                }
            }
        }
    }
    let text = String::from_utf8_lossy(&raw).to_string();
    assert!(text.contains("text/event-stream"), "headers: {text}");
    assert!(
        text.contains(": ping"),
        "first frame is the keepalive: {text}"
    );

    // CAD-258: each frame names the resources it invalidates, so the
    // client refetches only those. The event name stays the source.
    const ISSUES_FRAME: &str =
        "event: issues\ndata: {\"resources\":[\"issues\",\"projects\",\"issue\",\"overview\",\"workflows\",\"apps\",\"app\",\"app_runs\",\"outbox\",\"app_outputs\"]}\n\n";
    const AGENTS_FRAME: &str =
        "event: agents\ndata: {\"resources\":[\"agents\",\"issue\",\"overview\",\"outbox\",\"apps\",\"app\"]}\n\n";
    const JOBS_FRAME: &str =
        "event: jobs\ndata: {\"resources\":[\"issues\",\"agents\",\"issue\",\"overview\",\"app_runs\",\"outbox\",\"app_outputs\"]}\n\n";

    // A tracker write moves the mtime fingerprint → `event: issues`.
    std::fs::write(pm.path().join("poke.txt"), "x").unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut got_issues = false;
    while Instant::now() < deadline {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                raw.extend_from_slice(&tmp[..n]);
                if String::from_utf8_lossy(&raw).contains(ISSUES_FRAME) {
                    got_issues = true;
                    break;
                }
            }
        }
    }
    assert!(
        got_issues,
        "no issues event within 8s: {}",
        String::from_utf8_lossy(&raw)
    );

    // An agent change moves the agent fingerprint → `event: agents`.
    let _ = d.operator_rpc(
        "agent_register",
        json!({"alias": "late", "provider": "fake",
               "endpoint_kind": "fake", "cwd": pm.path().to_str().unwrap()}),
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut got_agents = false;
    while Instant::now() < deadline {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                raw.extend_from_slice(&tmp[..n]);
                if String::from_utf8_lossy(&raw).contains(AGENTS_FRAME) {
                    got_agents = true;
                    break;
                }
            }
        }
    }
    assert!(got_agents, "no agents event within 8s");

    // A dispatch moves the job fingerprint → `event: jobs`. `bound_job`
    // creates + dispatches, both of which change `job_list`.
    bound_job(pm.path(), &d, "CAD-3");
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut got_jobs = false;
    while Instant::now() < deadline {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                raw.extend_from_slice(&tmp[..n]);
                if String::from_utf8_lossy(&raw).contains(JOBS_FRAME) {
                    got_jobs = true;
                    break;
                }
            }
        }
    }
    assert!(got_jobs, "no jobs event within 8s");
}

/// Poll `agent_show` until `pred` holds or the deadline passes — the
/// board-test equivalent of the common harness's wait_agent.
fn wait_agent_pred(d: &UiDaemon, alias: &str, secs: u64, pred: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let show = d.rpc("agent_show", json!({"alias": alias}));
        if pred(&show) {
            return show;
        }
        assert!(
            Instant::now() < deadline,
            "agent {alias} never reached condition: {show}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn ui_agents_payload_covers_all_kinds() {
    let pm = TempDir::new().unwrap();
    let d = UiDaemon::start();
    seed(pm.path(), &d.state());
    let cwd = pm.path().to_str().unwrap();

    // Mailbox — inbox endpoints are counted separately, never fenced.
    let _ = d.operator_rpc(
        "agent_register",
        json!({"alias": "obs", "provider": "inbox",
               "endpoint_kind": "inbox", "cwd": cwd}),
    );
    // Idle worker — registered, no turn in flight.
    let _ = d.operator_rpc(
        "agent_register",
        json!({"alias": "idle1", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd}),
    );
    // Busy worker — SLEEP holds the turn so `running` stays up.
    let _ = d.operator_rpc(
        "agent_register",
        json!({"alias": "busy1", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd}),
    );
    d.rpc(
        "agent_send",
        json!({"alias": "busy1", "text": "SLEEP:30", "message": "b1"}),
    );
    wait_agent_pred(&d, "busy1", 10, |s| {
        s["messages"]
            .as_array()
            .map(|ms| ms.iter().any(|m| m["state"] == "running"))
            .unwrap_or(false)
    });
    // Stopped worker — registered then stopped.
    let _ = d.operator_rpc(
        "agent_register",
        json!({"alias": "stop1", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd}),
    );
    let _ = d.operator_rpc("agent_stop", json!({"alias": "stop1"}));
    // Fenced worker — DISCONNECT drops mid-turn → unknown → attention.
    let _ = d.operator_rpc(
        "agent_register",
        json!({"alias": "fenced1", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd}),
    );
    d.rpc(
        "agent_send",
        json!({"alias": "fenced1", "text": "DISCONNECT", "message": "f1"}),
    );
    wait_agent_pred(&d, "fenced1", 15, |s| {
        s["unknown"].as_i64().unwrap_or(0) > 0 || s["agent"]["state"].as_str() == Some("attention")
    });

    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");
    let (code, body) = http(port, "GET", "/api/agents", &host);
    assert_eq!(code, 200);
    let payload: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(payload["daemon"], "reachable");
    let row = |alias: &str| {
        payload["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["alias"] == alias)
            .cloned()
            .unwrap_or_else(|| panic!("{alias} row: {}", payload["agents"]))
    };

    let obs = row("obs");
    assert_eq!(obs["inbox"], true);
    assert_eq!(obs["state"], "inbox");
    assert_eq!(obs["fenced"], false);

    let idle = row("idle1");
    assert_eq!(idle["fenced"], false);
    assert!(matches!(idle["state"].as_str(), Some("idle" | "stopped")));

    let busy = row("busy1");
    assert!(
        busy["running"].as_i64().unwrap_or(0) >= 1,
        "busy row: {busy}"
    );
    assert_eq!(busy["message"]["id"], "b1");

    let fenced = row("fenced1");
    assert_eq!(fenced["fenced"], true, "fenced row: {fenced}");
    assert!(
        fenced["recovery"].as_str().is_some_and(|t| !t.is_empty()),
        "fenced row carries the daemon recovery text: {fenced}"
    );

    let totals = &payload["totals"];
    assert!(totals["running"].as_i64().unwrap_or(0) >= 1);
    assert!(totals["fenced"].as_i64().unwrap_or(0) >= 1);
    assert_eq!(totals["inboxes"], 1);
}
