use serde_json::Value;
use std::path::Path;

/// The tracker a `cadence status` run reads: the test's own
/// (`CADENCE_PM_DIR`, or `HOME/pm`) when it passes one, else a per-call
/// path that does not exist — never the host's `$HOME/pm`, which
/// `status` would otherwise read and, since CAD-403, cache line times
/// into (the rule `daemon_opts` applies to test daemons).
pub fn status_tracker_env(cmd: &mut std::process::Command, envs: &[(&str, &Path)]) {
    cmd.env_remove("CADENCE_PM_DIR");
    if !envs
        .iter()
        .any(|(k, _)| matches!(*k, "CADENCE_PM_DIR" | "HOME"))
    {
        let none = std::env::temp_dir().join(format!(
            "cadence-test-no-pm-{}",
            uuid::Uuid::new_v4().simple()
        ));
        cmd.env("CADENCE_PM_DIR", none);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
}

/// `cadence status --json` against a daemon's socket — the JSON shape
/// is the contract; extra args (`--group`) and env (`CADENCE_PM_DIR`)
/// thread through.
pub fn status_json(state: &Path, extra: &[&str], envs: &[(&str, &Path)]) -> Value {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .arg("status")
        .arg("--json")
        .args(extra)
        .env_remove("CADENCE_ALIAS");
    status_tracker_env(&mut cmd, envs);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "status output not json: {e}: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// `cadence status` table form — same invocation, no --json.
pub fn status_table(state: &Path, envs: &[(&str, &Path)]) -> String {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .arg("status")
        .env_remove("CADENCE_ALIAS");
    status_tracker_env(&mut cmd, envs);
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}
