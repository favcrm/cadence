// CAD-876: one exit code per error kind, and clap usage errors as the
// same JSON. A fake daemon answers every request with an error frame of
// the kind under test, so each kind is proved end to end through the
// real client (`proto::unwrap`) and the real `main`, with no daemon.
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Output};

fn cadence(home: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin");
    cmd
}

fn stderr_json(out: &Output) -> Value {
    serde_json::from_slice(&out.stderr).unwrap_or_else(|e| {
        panic!(
            "stderr is not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// Run `agent list` against a fake daemon that answers with `frame`.
fn against_fake_daemon(frame: Value) -> Output {
    let root = tempfile::Builder::new()
        .prefix("cx876")
        .tempdir_in("/tmp")
        .unwrap();
    let sock = root.path().join("d.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let reply = frame.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut line = String::new();
            let _ = BufReader::new(&stream).read_line(&mut line);
            let _ = writeln!(stream, "{reply}");
        }
    });
    let mut cmd = cadence(root.path());
    cmd.env("CADENCE_SOCKET", &sock)
        .arg("--state-dir")
        .arg(root.path())
        .args(["agent", "list"]);
    cadence_agent::reaper::output(&mut cmd).unwrap()
}

fn error_frame(kind: &str, code: Option<&str>) -> Value {
    let mut error = serde_json::json!({"kind": kind, "message": format!("a {kind} failure")});
    if let Some(code) = code {
        error["code"] = code.into();
    }
    serde_json::json!({"ok": false, "error": error})
}

#[test]
fn every_kind_exits_with_its_table_code_and_prints_json() {
    let cases: [(&str, Option<&str>, i32, &str); 11] = [
        ("rejected", None, 3, "rejected"),
        ("gate", None, 4, "gate"),
        ("conflict", Some("revision_conflict"), 5, "conflict"),
        ("provider", None, 6, "provider"),
        ("internal", None, 70, "internal"),
        ("busy", Some("resource_busy"), 75, "busy"),
        ("unknown", None, 1, "unknown"),
        // A kind this build does not know is a failure, never a success
        // and never a code that claims a meaning.
        ("from_the_future", None, 70, "internal"),
        // The kind is chosen by one match whether or not a code is present.
        ("gate", Some("legacy_write_lock"), 4, "gate"),
        ("from_the_future", Some("x"), 70, "internal"),
        ("busy", None, 75, "busy"),
    ];
    for (kind, code, want_exit, want_kind) in cases {
        let out = against_fake_daemon(error_frame(kind, code));
        let json = stderr_json(&out);
        assert_eq!(out.status.code(), Some(want_exit), "{kind}: {json}");
        assert_eq!(json["kind"], want_kind, "{kind}: {json}");
        assert_eq!(json["error"], format!("a {kind} failure"), "{kind}");
    }
}

#[test]
fn a_legacy_write_lock_refusal_exits_4_not_75() {
    // The wire shape `PmLock::busy_error(legacy)` produces: a coded gate.
    let out = against_fake_daemon(error_frame("gate", Some("legacy_write_lock")));
    let json = stderr_json(&out);
    assert_eq!(out.status.code(), Some(4), "{json}");
    assert_eq!(json["code"], "legacy_write_lock");
}

#[test]
fn a_bare_parent_command_prints_plain_help_on_stderr_with_exit_2() {
    let home = tempfile::tempdir().unwrap();
    for args in [vec![], vec!["issue"], vec!["agent"]] {
        let mut cmd = cadence(home.path());
        cmd.args(&args);
        let out = cadence_agent::reaper::output(&mut cmd).unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(err.contains("Usage:"), "{args:?}: {err}");
        assert!(
            serde_json::from_str::<Value>(&err).is_err(),
            "{args:?}: help must be plain text, not JSON"
        );
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn a_coded_rejection_stays_final() {
    let out = against_fake_daemon(error_frame("rejected", Some("workflow_unapproved")));
    let json = stderr_json(&out);
    assert_eq!(out.status.code(), Some(3), "{json}");
    assert_eq!(json["code"], "workflow_unapproved");
}

#[test]
fn the_exit_table_has_no_collisions_with_the_verb_specific_codes() {
    // 0 success, 1 generic failure / `unknown`, 2 usage and the verb
    // specific "no-go" (`doctor --host`, `audit`, `agent-uid provision`).
    let mut seen = std::collections::HashSet::new();
    for (kind, code) in cadence_agent::error::EXIT_TABLE {
        assert!(*code != 0, "{kind} must not exit 0");
        if !matches!(*kind, "unknown" | "usage") {
            assert!(*code > 2, "{kind} collides with the 0/1/2 verb codes");
            assert!(seen.insert(*code), "{kind} reuses code {code}");
        }
    }
}

#[test]
fn a_usage_error_is_json_with_exit_2() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        vec!["frobnicate"],
        vec!["agent", "list", "--no-such-flag"],
        vec!["agent", "list", "extra", "args"],
    ] {
        let mut cmd = cadence(home.path());
        cmd.args(&args);
        let out = cadence_agent::reaper::output(&mut cmd).unwrap();
        let json = stderr_json(&out);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {json}");
        assert_eq!(json["kind"], "usage", "{args:?}");
        let message = json["error"].as_str().unwrap();
        assert!(
            !message.is_empty() && !message.starts_with("error:"),
            "{message}"
        );
        assert!(
            out.stdout.is_empty(),
            "{args:?}: usage errors never touch stdout"
        );
    }
}

#[test]
fn help_and_version_still_exit_0_on_stdout() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["agent", "--help"],
        vec!["agent", "list", "-h"],
    ] {
        let mut cmd = cadence(home.path());
        cmd.args(&args);
        let out = cadence_agent::reaper::output(&mut cmd).unwrap();
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert!(!out.stdout.is_empty(), "{args:?}");
        assert!(
            out.stderr.is_empty(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn a_closed_stderr_pipe_keeps_the_kind_exit_code() {
    // The error print itself panics on EPIPE; the hook must exit with the
    // code `main` committed to (2 for usage), not 0.
    let home = tempfile::tempdir().unwrap();
    let mut cmd = cadence(home.path());
    cmd.arg("frobnicate")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cadence_agent::reaper::spawn(&mut cmd).unwrap();
    drop(child.stdout.take());
    drop(child.stderr.take());
    assert_eq!(child.wait().unwrap().code(), Some(2));
}
