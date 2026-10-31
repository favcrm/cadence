//! CAD-1922 independent acceptance: a planted sandbox marker must not let
//! `dev reset` delete an existing tracker. Written by the reviewer, not the
//! sandbox-guard implementer. Exercise the real CLI on disposable paths only.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::Builder;

fn reset(home: &Path, state: &Path, temp: &Path, name: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cadence"));
    command
        .env_clear()
        .env("HOME", home)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("CADENCE_SANDBOX_ROOT", home)
        .env("CADENCE_TEST_REAL_HOME", home.join("not-the-real-home"))
        .env("TMPDIR", temp)
        .env("PATH", "/usr/bin:/bin")
        .args(["dev", "reset", name]);
    // CAD-308: every child goes through the subreaper's spawn.
    cadence_agent::reaper::output(&mut command).expect("run isolated cadence dev reset")
}

#[test]
fn reset_refuses_a_planted_sandbox_marker_in_the_existing_tracker() {
    let root = Builder::new().prefix("c1922-").tempdir_in("/tmp").unwrap();
    let home = root.path().join("home");
    let state = root.path().join("xdg-state");
    let temp = root.path().join("tmp");
    for dir in [&home, &state, &temp] {
        std::fs::create_dir_all(dir).unwrap();
    }

    // Positive control: a real disposable sandbox under this base is reset.
    let safe = home.join("safe");
    std::fs::create_dir(&safe).unwrap();
    std::fs::write(safe.join(".cadence-sandbox"), r#"{"name":"safe"}"#).unwrap();
    let control = reset(&home, &state, &temp, "safe");
    assert!(
        control.status.success(),
        "isolated control must be accepted: {}",
        String::from_utf8_lossy(&control.stderr)
    );
    assert!(!safe.exists(), "the accepted control was not reset");

    // The same marker planted in the existing default tracker is not authority
    // to delete it, even though it is directly under the configured base.
    let tracker = home.join("pm");
    std::fs::create_dir(&tracker).unwrap();
    std::fs::write(tracker.join(".cadence-sandbox"), r#"{"name":"pm"}"#).unwrap();
    let canary = tracker.join("ticket-canary");
    std::fs::write(&canary, b"existing tracker data\n").unwrap();
    let refused = reset(&home, &state, &temp, "pm");
    let output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        matches!(refused.status.code(), Some(3 | 4)),
        "expected a real refusal, got {:?}: {output}",
        refused.status.code()
    );
    assert!(
        output.to_lowercase().contains("overlap"),
        "refusal must identify the overlap: {output}"
    );
    assert_eq!(
        std::fs::read(&canary).unwrap(),
        b"existing tracker data\n",
        "refusal must not delete the tracker"
    );
}

#[test]
fn reset_refuses_a_custom_tracker_nested_in_the_candidate() {
    assert_custom_tracker_refused(false);
    assert_custom_tracker_refused(true);
}

fn assert_custom_tracker_refused(forged_profile: bool) {
    let root = Builder::new().prefix("c1922-").tempdir_in("/tmp").unwrap();
    let home = root.path().join("home");
    let state = root.path().join("xdg-state");
    let temp = root.path().join("tmp");
    let candidate = home.join("candidate");
    let tracker = candidate.join("pm");
    for dir in [&home, &state, &temp, &tracker] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(
        candidate.join(".cadence-sandbox"),
        r#"{"name":"candidate"}"#,
    )
    .unwrap();
    let canary = tracker.join("ticket-canary");
    std::fs::write(&canary, b"custom tracker data\n").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_cadence"));
    command
        .env_clear()
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &state)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("CADENCE_SANDBOX_ROOT", &home)
        .env("CADENCE_PM_DIR", &tracker)
        .env("CADENCE_TEST_REAL_HOME", home.join("not-the-real-home"))
        .env("TMPDIR", &temp)
        .env("PATH", "/usr/bin:/bin")
        .args(["dev", "reset", "candidate"]);
    if forged_profile {
        command.env("CADENCE_PROFILE", "sandbox:candidate");
    }
    let result =
        cadence_agent::reaper::output(&mut command).expect("run isolated cadence dev reset");
    let output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        matches!(result.status.code(), Some(3 | 4)),
        "expected a real refusal, got {:?}: {output}",
        result.status.code()
    );
    assert!(
        output.to_lowercase().contains("overlap"),
        "refusal must identify the overlap: {output}"
    );
    assert_eq!(
        std::fs::read(&canary).unwrap(),
        b"custom tracker data\n",
        "refusal must not delete the tracker"
    );
}
