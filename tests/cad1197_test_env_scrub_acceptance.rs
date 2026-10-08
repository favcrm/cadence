//! CAD-1197 independent acceptance check (written by the reviewer, not the
//! implementer; AGENTS.md "Gates and security work").
//!
//! The suite runner must never hand a caller's tracker or state routing to
//! the tests it runs: a pane's CADENCE_PM_DIR/CADENCE_STATE_DIR once made the
//! lib suite hold the production tracker's write lock. Runs the real
//! `scripts/run-result-tests` with a stub `cargo` that records the
//! environment it was given, under a fake HOME whose `pm` and
//! `.local/state/cadence` stand in for production.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Rig {
    _root: tempfile::TempDir,
    home: PathBuf,
    bin: PathBuf,
    seen: PathBuf,
}

fn rig() -> Rig {
    let root = tempfile::Builder::new()
        .prefix("c1197acc-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = root.path().join("home");
    let bin = root.path().join("bin");
    let seen = root.path().join("seen");
    fs::create_dir_all(home.join("pm")).unwrap();
    fs::create_dir_all(home.join(".local/state/cadence")).unwrap();
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&seen).unwrap();
    // The stub records every invocation's CADENCE_* environment.
    let stub = bin.join("cargo");
    fs::write(
        &stub,
        format!(
            "#!/bin/sh\nenv | grep '^CADENCE_' > \"{}/run-$$.env\"\nexit 0\n",
            seen.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    Rig {
        _root: root,
        home,
        bin,
        seen,
    }
}

fn runner(rig: &Rig) -> Command {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/run-result-tests");
    let mut cmd = Command::new("bash");
    cmd.arg(script)
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", rig.bin.display()))
        .env("HOME", &rig.home)
        .env("CARGO_HOME", rig.home.join(".cargo"))
        .env("RUSTUP_HOME", rig.home.join(".rustup"))
        .env("TMPDIR", rig.home.join("tmp"));
    fs::create_dir_all(rig.home.join("tmp")).unwrap();
    cmd
}

fn recorded(rig: &Rig) -> Vec<String> {
    let mut out = Vec::new();
    for e in fs::read_dir(&rig.seen).unwrap().flatten() {
        out.extend(
            fs::read_to_string(e.path())
                .unwrap()
                .lines()
                .map(str::to_string),
        );
    }
    out
}

#[test]
fn cad1197_production_routing_never_reaches_the_tests() {
    let rig = rig();
    let pm = rig.home.join("pm");
    let state = rig.home.join(".local/state/cadence");
    let mut cmd = runner(&rig);
    cmd.env("CADENCE_PM_DIR", &pm)
        .env("CADENCE_STATE_DIR", &state)
        .env("CADENCE_HOME", rig.home.join(".cadence"))
        .env("CADENCE_ALIAS", "pane-agent")
        .env("CADENCE_ORG", "local");
    let out = cadence_agent::reaper::output(&mut cmd).unwrap();
    let runs = fs::read_dir(&rig.seen).unwrap().count();
    assert!(
        runs > 0,
        "the stub cargo never ran, so the check proves nothing: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let leaked: Vec<String> = recorded(&rig)
        .into_iter()
        .filter(|l| !l.starts_with("CADENCE_SUITE_LOCK="))
        .collect();
    assert!(
        leaked.is_empty(),
        "caller CADENCE_* routing reached cargo: {leaked:?}"
    );
    for line in recorded(&rig) {
        let value = line.split_once('=').map(|(_, v)| v).unwrap_or("");
        assert!(
            !value.starts_with(pm.to_str().unwrap()) && !value.starts_with(state.to_str().unwrap()),
            "a value handed to cargo points into the production stand-ins: {line}"
        );
    }
}

#[test]
fn cad1197_a_suite_lock_inside_the_tracker_refuses_before_cargo() {
    let rig = rig();
    let mut cmd = runner(&rig);
    cmd.env("CADENCE_SUITE_LOCK", rig.home.join("pm/suite.lock"));
    let out = cadence_agent::reaper::output(&mut cmd).unwrap();
    assert!(
        !out.status.success(),
        "the runner accepted a suite lock inside the tracker"
    );
    assert_eq!(
        fs::read_dir(&rig.seen).unwrap().count(),
        0,
        "cargo ran despite the refusal"
    );
}
