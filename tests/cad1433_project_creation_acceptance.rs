//! CAD-1433 independent acceptance, authored from the ticket before the
//! implementation change. The implementation author must not edit or weaken
//! this check.
//!
//! Exercises the real `project_new` daemon RPC and caller guard with a
//! test-seam-derived ordinary agent connection. The seam is a test-only
//! caller fixture, not a production alias/UID spoof and not proof of a fresh
//! Home-master identity. The unauthorized request omits `repo` (the new
//! repo-less form); the assertion requires an authorization refusal rather
//! than accepting a later request-validation error. A proven-operator fixture
//! creates a real Git-backed project as the positive control. PM files and
//! the attempted repo path must remain byte-for-byte unchanged on refusal.
//!
//! No HTTP board route relaying `project_new` exists in the inspected source
//! (`src/ui`, `src/daemon` dispatch); this acceptance therefore covers native
//! RPC only. It does not prove Home conversation behavior or a fresh native
//! Home caller.
//!
//! Run only in an admitted focused test slot. The check is a module of the
//! consolidated `suite` target; a standalone
//! `--test cad1433_project_creation_acceptance` target no longer exists:
//! `cadence build-slot run test --org local -- env CARGO_BUILD_JOBS=4 cargo test --features test-seam --test suite cad1433_project_creation_acceptance::project_new_refuses_an_agent_before_any_repo_less_write -- --exact --test-threads 1`
#![cfg(feature = "test-seam")]

use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{adapter::ProviderEnv, client, daemon, issue::Pm};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering::SeqCst},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

struct Fixture {
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<cadence_agent::Result<()>>>,
}

impl Fixture {
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("bp1433-")
            .tempdir_in("/tmp")
            .unwrap();
        for dir in ["home", "xdg", "tmp", "locks", "state"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
        }
        std::env::set_var("HOME", root.path().join("home"));
        std::env::set_var("XDG_STATE_HOME", root.path().join("xdg"));
        std::env::set_var("XDG_CONFIG_HOME", root.path().join("home/config"));
        std::env::set_var("XDG_DATA_HOME", root.path().join("home/data"));
        std::env::set_var("TMPDIR", root.path());
        std::env::set_var("CADENCE_SUITE_LOCK", root.path().join("locks/suite"));
        // Git's automatic maintenance and auto-GC can run detached and leave
        // transient scratch files (for example
        // `.git/objects/bitmap-ref-tips_*`) inside the tracker while this
        // fixture snapshots every byte. Pin one private configuration before
        // `Pm::init` — which runs `git init` and the skeleton commit — and
        // before the control repository is created, so system, inherited
        // global and command-scope configuration cannot re-enable it.
        let git_config = root.path().join("home/gitconfig");
        std::fs::write(
            &git_config,
            "[maintenance]\n\tauto = false\n[gc]\n\tauto = 0\n",
        )
        .unwrap();
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        std::env::set_var("GIT_CONFIG_GLOBAL", &git_config);
        std::env::remove_var("GIT_CONFIG_PARAMETERS");
        std::env::remove_var("GIT_CONFIG_COUNT");
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let provider_env = ProviderEnv::refusing_providers();
        provider_env.set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let state = root.path().join("state");
        let stop = Arc::new(AtomicBool::new(false));
        let opts = daemon::ServeOptions {
            provider_env,
            stop: Some(stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let run_state = state.clone();
        let daemon = thread::spawn(move || daemon::serve_with(&run_state, opts));
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_ok() {
                break;
            }
            assert!(
                !daemon.is_finished() && Instant::now() < deadline,
                "isolated daemon failed to start"
            );
            thread::sleep(Duration::from_millis(50));
        }
        Self {
            root,
            stop,
            daemon: Some(daemon),
        }
    }

    fn rpc(&self, caller: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(caller, || {
            client::rpc(&self.root.path().join("state"), method, params)
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(daemon) = self.daemon.take() {
            let _ = daemon.join();
        }
    }
}

fn files_under(path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let ty = entry.file_type().unwrap();
        if ty.is_dir() {
            files_under(&path, out);
        } else if ty.is_file() {
            out.insert(path, std::fs::read(entry.path()).unwrap());
        } else {
            panic!(
                "unexpected non-file in isolated tracker: {}",
                path.display()
            );
        }
    }
}

fn tracker_bytes(pm: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    files_under(pm, &mut out);
    out
}

#[test]
fn project_new_refuses_an_agent_before_any_repo_less_write() {
    let fx = Fixture::start();
    let pm = fx.root.path().join("pm");
    let repo_less_project = pm.join("crm");

    // Control: the real RPC is live and an operator can register a valid
    // existing Git repository. This prevents a blanket-failure false pass.
    let valid_repo = fx.root.path().join("operator-project");
    std::fs::create_dir(&valid_repo).unwrap();
    let git = cadence_agent::reaper::status(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&valid_repo),
    )
    .unwrap();
    assert!(
        git.success(),
        "could not initialize isolated control repository"
    );
    let operator = fx
        .rpc(
            Asserted::Operator,
            "project_new",
            json!({"key":"control","prefix":"CTL","repo":valid_repo}),
        )
        .expect("operator positive-control project registration must succeed");
    assert_eq!(operator["changed"], true);
    assert!(pm.join("control/PROJECT.md").is_file());

    let before = tracker_bytes(&pm);
    assert!(!repo_less_project.exists());
    let attempt = fx.rpc(
        Asserted::Agent("cad1433-untrusted-worker".into()),
        "project_new",
        json!({"key":"crm","prefix":"CRM"}),
    );
    let refusal = attempt.expect_err("ordinary agent must not create a project");
    let message = refusal.to_string();
    assert!(
        message.contains("operator action") || message.contains("operator connection"),
        "must refuse at the caller authorization guard, not merely reject request shape: {message}"
    );
    assert_eq!(
        tracker_bytes(&pm),
        before,
        "refused RPC changed tracker bytes"
    );
    assert!(
        !repo_less_project.exists(),
        "refused RPC created the repo-less project directory"
    );
}
