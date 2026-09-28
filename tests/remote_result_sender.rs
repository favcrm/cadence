use cadence_agent::remote_result_outbox::{
    deliver_with, DestinationPin, ResultCommand, ResultOutbox,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};

fn pin() -> DestinationPin {
    DestinationPin::new(
        "org-1",
        "https://board.example.invalid",
        "subject-1",
        "agent-1",
    )
    .unwrap()
}
fn command() -> ResultCommand {
    ResultCommand::parse_json(
        &json!({
            "version":"hosted-cadence-result.v1", "commandId":"command-1", "kind":"agent_result",
            "assignmentId":"assignment-1", "taskId":"task-1", "taskRevision":1,
            "turnId":"turn-1", "reportedHeadSha":"a".repeat(40), "text":"private result"
        })
        .to_string(),
    )
    .unwrap()
}
fn receipt(cmd: &ResultCommand) -> Value {
    json!({"ok":true,"receipt":{"commandId":"command-1","state":"queued",
        "acceptedAt":100,"expiresAt":200,"digest":cmd.digest()}})
}
const CHILD: &str = "hct_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq";
const ORG: &str = "org-1";
const AUDIENCE: &str = "https://board.example.invalid";

#[test]
fn pinned_bytes_destination_and_matching_receipt_survive_reopen() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let outbox = ResultOutbox::open(&path).unwrap();
    let cmd = command();
    outbox.enqueue(&pin(), &cmd).unwrap();
    let queued = deliver_with(
        &outbox,
        "command-1",
        ORG,
        AUDIENCE,
        CHILD,
        |url, bearer, body| {
            assert_eq!(
                url,
                "https://board.example.invalid/__platform/hosted-cadence/org-1/results"
            );
            assert_eq!(bearer, CHILD);
            assert_eq!(body, cmd.canonical_json());
            assert!(!body.contains("subject-1"));
            Ok((202, receipt(&cmd).to_string().into_bytes()))
        },
    )
    .unwrap();
    assert_eq!(queued.state(), "remote_queued");
    drop(outbox);
    let reopened = ResultOutbox::open(&path).unwrap();
    let saved = reopened.get("command-1").unwrap();
    assert_eq!(saved.state(), "remote_queued");
    assert_eq!(saved.queued_receipt().unwrap(), &queued);
    let replay = deliver_with(&reopened, "command-1", ORG, AUDIENCE, CHILD, |_, _, _| {
        Ok((202, receipt(&cmd).to_string().into_bytes()))
    })
    .unwrap();
    assert_eq!(replay, queued);
    let mut conflicting = receipt(&cmd);
    conflicting["receipt"]["acceptedAt"] = json!(101);
    assert!(
        deliver_with(&reopened, "command-1", ORG, AUDIENCE, CHILD, |_, _, _| {
            Ok((202, conflicting.to_string().into_bytes()))
        })
        .is_err()
    );
    assert_eq!(
        reopened.get("command-1").unwrap().queued_receipt(),
        Some(&queued)
    );
}

#[test]
fn invalid_credential_never_sends_and_untrusted_response_never_marks_queued() {
    let root = tempfile::tempdir().unwrap();
    let outbox = ResultOutbox::open(&root.path().join("outbox")).unwrap();
    let cmd = command();
    outbox.enqueue(&pin(), &cmd).unwrap();
    for bad in [
        "",
        "agc_tools-token",
        "Bearer hct_test",
        "hct_bad\nHeader: forged",
    ] {
        assert!(
            deliver_with(&outbox, "command-1", ORG, AUDIENCE, bad, |_, _, _| panic!(
                "network reached"
            ))
            .is_err()
        );
    }
    let mut changed_id = receipt(&cmd);
    changed_id["receipt"]["commandId"] = json!("other");
    let mut changed_digest = receipt(&cmd);
    changed_digest["receipt"]["digest"] = json!("b".repeat(64));
    let mut extra = receipt(&cmd);
    extra["receipt"]["applied"] = json!(true);
    let mut unsafe_time = receipt(&cmd);
    unsafe_time["receipt"]["acceptedAt"] = json!(9_007_199_254_740_992u64);
    let mut top_level_extra = receipt(&cmd);
    top_level_extra["applied"] = json!(true);
    for (status, value) in [
        (202, changed_id),
        (202, changed_digest),
        (202, extra),
        (202, unsafe_time),
        (202, top_level_extra),
        (200, receipt(&cmd)),
        (401, receipt(&cmd)),
        (403, receipt(&cmd)),
        (409, receipt(&cmd)),
        (429, receipt(&cmd)),
        (503, receipt(&cmd)),
        (302, receipt(&cmd)),
    ] {
        assert!(
            deliver_with(&outbox, "command-1", ORG, AUDIENCE, CHILD, |_, _, _| {
                Ok((status, value.to_string().into_bytes()))
            })
            .is_err()
        );
        assert_eq!(outbox.get("command-1").unwrap().state(), "local_pending");
    }
    assert!(
        deliver_with(&outbox, "command-1", ORG, AUDIENCE, CHILD, |_, _, _| {
            Err(cadence_agent::error::Error::rejected("transport failed"))
        })
        .is_err()
    );
    assert_eq!(outbox.get("command-1").unwrap().state(), "local_pending");
}

#[test]
fn changed_pin_is_refused_and_altered_stored_command_never_reaches_network() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let outbox = ResultOutbox::open(&path).unwrap();
    outbox.enqueue(&pin(), &command()).unwrap();
    let other = DestinationPin::new(
        "other",
        "https://other.example.invalid",
        "subject-1",
        "agent-1",
    )
    .unwrap();
    assert!(outbox.enqueue(&other, &command()).is_err());
    drop(outbox);
    let conn = Connection::open(path.join("results.sqlite3")).unwrap();
    conn.execute(
        "UPDATE pending_results SET payload=? WHERE command_id='command-1'",
        [command()
            .canonical_json()
            .replace("private result", "changed result")],
    )
    .unwrap();
    drop(conn);
    let reopened = ResultOutbox::open(&path).unwrap();
    assert!(deliver_with(
        &reopened,
        "command-1",
        ORG,
        AUDIENCE,
        CHILD,
        |_, _, _| panic!("network reached with altered command")
    )
    .is_err());
}

#[test]
fn explicit_enrollment_destination_must_match_stored_pin_before_network() {
    let root = tempfile::tempdir().unwrap();
    let outbox = ResultOutbox::open(&root.path().join("outbox")).unwrap();
    outbox.enqueue(&pin(), &command()).unwrap();
    for (org, audience) in [
        ("other", AUDIENCE),
        (ORG, "https://other.example.invalid"),
        (ORG, "https://board.example.invalid:443"),
    ] {
        assert!(deliver_with(
            &outbox,
            "command-1",
            org,
            audience,
            CHILD,
            |_, _, _| panic!("network reached after destination mismatch")
        )
        .is_err());
        assert_eq!(outbox.get("command-1").unwrap().state(), "local_pending");
    }
}

#[test]
fn v1_custody_upgrades_to_v2_without_losing_original_result() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let first = ResultOutbox::open(&path).unwrap();
    let saved = first.enqueue(&pin(), &command()).unwrap();
    drop(first);
    let conn = Connection::open(path.join("results.sqlite3")).unwrap();
    conn.execute_batch("DROP TABLE queued_receipts; PRAGMA user_version=1;")
        .unwrap();
    drop(conn);
    let reopened = ResultOutbox::open(&path).unwrap();
    let pending = reopened.get("command-1").unwrap();
    assert_eq!(pending.receipt(), &saved);
    assert_eq!(pending.state(), "local_pending");
    let upgraded = Connection::open(path.join("results.sqlite3")).unwrap();
    let version: u32 = upgraded
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 2);
}

#[test]
fn cli_requires_stdin_child_bearer_and_does_not_consult_tools_token() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    ResultOutbox::open(&path)
        .unwrap()
        .enqueue(&pin(), &command())
        .unwrap();
    let send = |stdin: &[u8], org: &str, audience: &str| {
        let mut file = tempfile::tempfile_in(root.path()).unwrap();
        file.write_all(stdin).unwrap();
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_cadence"));
        command
            .env_clear()
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path())
            .env("TMPDIR", root.path())
            .env("CADENCE_STATE_DIR", root.path().join("absent-state"))
            .env("CADENCE_TOKEN", "agc_tools-secret-must-not-print")
            .args(["remote", "result", "send", "--outbox-dir"])
            .arg(&path)
            .args([
                "--command-id",
                "command-1",
                "--org",
                org,
                "--audience",
                audience,
            ])
            .stdin(Stdio::from(file))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in ["RUSTUP_HOME", "CARGO_HOME"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        cadence_agent::reaper::spawn(&mut command)
            .unwrap()
            .wait_with_output()
            .unwrap()
    };
    for token in [
        b"".as_slice(),
        b"agc_tools-secret-must-not-print",
        b"hct_bad\nHeader: forged",
    ] {
        let output = send(token, ORG, AUDIENCE);
        assert!(!output.status.success());
        for stream in [&output.stdout, &output.stderr] {
            let message = String::from_utf8_lossy(stream);
            assert!(!message.contains("agc_tools-secret-must-not-print"));
            assert!(!message.contains("Header: forged"));
            assert!(!message.contains("private result"));
        }
        assert_eq!(
            ResultOutbox::open(&path)
                .unwrap()
                .get("command-1")
                .unwrap()
                .state(),
            "local_pending"
        );
    }
    let mismatch = send(CHILD.as_bytes(), ORG, "https://other.example.invalid");
    assert!(!mismatch.status.success());
    assert!(!String::from_utf8_lossy(&mismatch.stderr).contains(CHILD));
    assert_eq!(
        ResultOutbox::open(&path)
            .unwrap()
            .get("command-1")
            .unwrap()
            .state(),
        "local_pending"
    );
    let mut status_command = Command::new(env!("CARGO_BIN_EXE_cadence"));
    status_command
        .env_clear()
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path())
        .env("TMPDIR", root.path())
        .env("CADENCE_STATE_DIR", root.path().join("absent-state"))
        .args(["remote", "result", "status", "--outbox-dir"])
        .arg(&path)
        .args(["--command-id", "command-1"]);
    let status = cadence_agent::reaper::output(&mut status_command).unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let value: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(value["state"], "local_pending");
    assert_eq!(value["application"], "applied_unknown");
    assert!(!String::from_utf8_lossy(&status.stdout).contains("private result"));
    assert!(!root.path().join("absent-state").exists());
}

#[test]
fn concurrent_duplicate_sends_commit_one_immutable_queued_receipt() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("outbox");
    let cmd = command();
    ResultOutbox::open(&path)
        .unwrap()
        .enqueue(&pin(), &cmd)
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let results: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            let body = receipt(&cmd).to_string().into_bytes();
            std::thread::spawn(move || {
                let outbox = ResultOutbox::open(&path).unwrap();
                barrier.wait();
                deliver_with(&outbox, "command-1", ORG, AUDIENCE, CHILD, |_, _, _| {
                    Ok((202, body))
                })
                .unwrap()
            })
        })
        .collect();
    let first = results[0].thread().id();
    let receipts: Vec<_> = results.into_iter().map(|r| r.join().unwrap()).collect();
    assert_ne!(first, std::thread::current().id());
    assert_eq!(receipts[0], receipts[1]);
    assert_eq!(
        ResultOutbox::open(&path)
            .unwrap()
            .get("command-1")
            .unwrap()
            .state(),
        "remote_queued"
    );
}
