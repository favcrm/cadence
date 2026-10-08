//! CAD-1210 independent acceptance check (written by the reviewer, not the
//! implementer; AGENTS.md "Gates and security work").
//!
//! A test process must never resolve the invoking user's real tracker
//! (`~/pm`) or state dir (`~/.local/state/cadence`), even when a pane hands
//! it those paths through CADENCE_PM_DIR / CADENCE_STATE_DIR. The child
//! re-run points CADENCE_TEST_REAL_HOME at a fake home, so the check never
//! goes near the real paths.
#![cfg(feature = "test-seam")]

use std::fs;
use std::path::Path;
use std::process::Command;

const TEST: &str = "cad1210_resolvers_refuse_the_real_tracker_and_state_dir";
const CHILD: &str = "CADENCE_CAD1210_ACCEPT_CHILD";

fn child(fake: &Path, pm: &Path, state: &Path) -> std::process::Output {
    child_env(fake, Some(pm), Some(state), None)
}

/// CAD-1227: the same child with any of the three routing variables
/// set or absent.
fn child_env(
    fake: &Path,
    pm: Option<&Path>,
    state: Option<&Path>,
    home: Option<&Path>,
) -> std::process::Output {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", TEST, "--test-threads", "1", "--nocapture"])
        .env(CHILD, "1")
        .env("CADENCE_TEST_REAL_HOME", fake)
        .env("HOME", fake.join("isolated-home"))
        .env_remove("CADENCE_PM_DIR")
        .env_remove("CADENCE_STATE_DIR")
        .env_remove("CADENCE_HOME");
    if let Some(pm) = pm {
        cmd.env("CADENCE_PM_DIR", pm);
    }
    if let Some(state) = state {
        cmd.env("CADENCE_STATE_DIR", state);
    }
    if let Some(home) = home {
        cmd.env("CADENCE_HOME", home);
    }
    cadence_agent::reaper::output(&mut cmd).unwrap()
}

#[test]
fn cad1210_resolvers_refuse_the_real_tracker_and_state_dir() {
    if std::env::var_os(CHILD).is_some() {
        let pm = cadence_agent::home::tracker_dir();
        let state = cadence_agent::home::state_dir();
        println!(
            "TRACKER={}",
            match &pm {
                Ok(p) => format!("ok {}", p.display()),
                Err(e) => format!("err {e}"),
            }
        );
        println!(
            "STATE={}",
            match &state {
                Ok(p) => format!("ok {}", p.display()),
                Err(e) => format!("err {e}"),
            }
        );
        return;
    }
    let root = tempfile::Builder::new()
        .prefix("c1210acc-")
        .tempdir_in("/tmp")
        .unwrap();
    let fake = root.path().join("realhome");
    let real_pm = fake.join("pm");
    let real_state = fake.join(".local/state/cadence");
    fs::create_dir_all(&real_pm).unwrap();
    fs::create_dir_all(&real_state).unwrap();
    fs::create_dir_all(fake.join("isolated-home")).unwrap();

    // The production stand-ins (and paths under them) must refuse.
    for (pm, state) in [
        (real_pm.clone(), real_state.clone()),
        (real_pm.join("cadence"), real_state.join("sub")),
    ] {
        let out = child(&fake, &pm, &state);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("TRACKER=err") && text.contains("CAD-1210"),
            "tracker_dir resolved the real tracker {}: {text}",
            pm.display()
        );
        assert!(
            text.contains("STATE=err"),
            "state_dir resolved the real state dir {}: {text}",
            state.display()
        );
    }

    // CAD-1227: the real ~/.cadence as CADENCE_HOME refuses through
    // layout(), with no PM or state override to catch it first.
    let real_cadence = fake.join(".cadence");
    fs::create_dir_all(&real_cadence).unwrap();
    let out = child_env(&fake, None, None, Some(&real_cadence));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("TRACKER=err") && text.contains("STATE=err"),
        "CADENCE_HOME=<real>/.cadence must refuse via layout(): {text}"
    );
    let tmp_home = root.path().join("tmp-cadence-home");
    let out = child_env(&fake, None, None, Some(&tmp_home));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("TRACKER=ok") && text.contains("STATE=ok"),
        "a temp CADENCE_HOME must still resolve: {text}"
    );

    // CAD-1227: the test queue's per-job state dir (strictly under
    // <real state>/test-queue/run/) is the one allowlisted path; the
    // queue's own dirs stay refused.
    let job_state = real_state.join("test-queue/run/job-1/state");
    let tmp_pm = root.path().join("tmp-pm");
    let out = child_env(&fake, Some(&tmp_pm), Some(&job_state), None);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("STATE=ok {}", job_state.display())),
        "a queued job's own state dir must resolve: {text}"
    );
    for refused in [
        real_state.join("test-queue/jobs"),
        real_state.join("test-queue/run"),
        real_state.join("test-queue/run/job-1/../../.."),
    ] {
        let out = child_env(&fake, Some(&tmp_pm), Some(&refused), None);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("STATE=err"),
            "{} must stay refused: {text}",
            refused.display()
        );
    }

    // Isolated temp paths still resolve.
    let tmp_pm = root.path().join("tmp-pm");
    let tmp_state = root.path().join("tmp-state");
    let out = child(&fake, &tmp_pm, &tmp_state);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("TRACKER=ok {}", tmp_pm.display()))
            && text.contains(&format!("STATE=ok {}", tmp_state.display())),
        "isolated temp dirs must still resolve: {text}"
    );
}
