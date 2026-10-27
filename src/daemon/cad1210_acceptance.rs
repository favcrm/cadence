//! CAD-1210 independent acceptance check (written by the reviewer, not the
//! implementer; AGENTS.md "Gates and security work").
//!
//! The daemon takes its tracker from its own `provider_env`, not from the
//! process environment (`pm_dir_of`). A test daemon whose provider_env
//! carries the invoking user's real `~/pm` must refuse to start, even when
//! the process itself has no CADENCE_PM_DIR. The child re-run points the
//! "real home" at a fake stand-in (CADENCE_TEST_REAL_HOME, read only in test
//! builds) whose `pm` is a valid tracker, so only the guard can refuse.
use super::*;

const TEST: &str = "daemon::cad1210_acceptance::cad1210_daemon_refuses_a_provider_env_real_tracker";
const CHILD: &str = "CADENCE_CAD1210_DAEMON_CHILD";

#[test]
fn cad1210_daemon_refuses_a_provider_env_real_tracker() {
    if let Some(fake) = std::env::var_os(CHILD) {
        let fake = std::path::PathBuf::from(fake);
        let state = fake.join("daemon-state");
        std::fs::create_dir_all(&state).unwrap();
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", fake.join("pm").to_str().unwrap());
        match Shared::new(&state, &opts) {
            Ok(_) => println!("DAEMON=started"),
            Err(e) => println!("DAEMON=refused {e}"),
        }
        return;
    }
    let root = tempfile::Builder::new()
        .prefix("c1210d-")
        .tempdir_in("/tmp")
        .unwrap();
    let fake = root.path().join("realhome");
    // A valid tracker at the stand-in: without the guard the daemon starts.
    crate::issue::Pm::init(&fake.join("pm")).unwrap();
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", TEST, "--test-threads", "1", "--nocapture"])
        .env(CHILD, &fake)
        .env("CADENCE_TEST_REAL_HOME", &fake)
        .env("HOME", root.path().join("isolated-home"))
        .env_remove("CADENCE_PM_DIR")
        .env_remove("CADENCE_STATE_DIR")
        .env_remove("CADENCE_HOME");
    let out = crate::reaper::output(&mut cmd).unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("DAEMON=refused") && text.contains("CAD-1210"),
        "a daemon whose provider_env names the real tracker must refuse: {text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
