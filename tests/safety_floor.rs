//! CAD-1073 safety-floor binary: the minimum executable coverage retained
//! while the legacy test suite is retired. Proves release-boundary and
//! refusal contracts that the reduced gate still enforces.
//!
//! This is intentionally small: it exercises the CLI's own refusal paths and
//! the test-seam exclusion without depending on the retired fixture
//! infrastructure.

use cadence_agent::reaper;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::{Builder, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");

// Never leave a child launched by this file running after a failed assertion.
struct OwnChild(Option<Child>);

impl OwnChild {
    fn pending(&mut self) -> bool {
        self.0.as_mut().unwrap().try_wait().unwrap().is_none()
    }

    fn wait(&mut self, deadline: Duration) -> Output {
        let until = Instant::now() + deadline;
        while self.pending() {
            assert!(Instant::now() < until, "child exceeded {deadline:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
        self.0.take().unwrap().wait_with_output().unwrap()
    }
}

impl Drop for OwnChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

struct FloorHost {
    root: TempDir,
    sandbox_started: bool,
}

impl FloorHost {
    fn new() -> Self {
        let root = Builder::new()
            .prefix("cad1073-")
            .tempdir_in("/tmp")
            .unwrap();
        for dir in ["home", "xdg", "tmp", "boxes", "locks"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
        }
        Self {
            root,
            sandbox_started: false,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(BINARY);
        cmd.env("HOME", self.path("home"))
            .env("XDG_STATE_HOME", self.path("xdg"))
            .env("TMPDIR", self.path("tmp"))
            .env("CADENCE_SANDBOX_ROOT", self.path("boxes"))
            .env("CADENCE_TEST_PORT_LOCK_DIR", self.path("locks"))
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_PM_DIR")
            .env_remove("CADENCE_PROFILE")
            .env_remove("CADENCE_ALIAS")
            .env_remove("CADENCE_ROLLOUT_AS")
            .env_remove("CADENCE_SANDBOX_ALLOW_GLOBAL")
            .env_remove("XDG_DATA_HOME");
        for name in cadence_agent::adapter::PROVIDER_COMMAND_VARS {
            cmd.env(name, cadence_agent::adapter::REFUSED_COMMAND);
        }
        // An isolated HOME must not hide the host's installed Rust toolchain.
        for name in ["CARGO_HOME", "RUSTUP_HOME"] {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = self.command();
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        OwnChild(cmd.spawn().unwrap()).wait(Duration::from_secs(25))
    }
}

impl Drop for FloorHost {
    fn drop(&mut self) {
        if self.sandbox_started {
            let _ = self.run(&["sandbox", "down", "floor"]);
            let _ = self.run(&["sandbox", "reset", "floor"]);
        }
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[test]
fn safety_floor_test_seam_refuses_release_build() {
    // The release binary must never carry the test seam. This assertion is
    // checked by the CI build job's "Assert the release binary carries no
    // test seam" step; we mirror the check here so the floor itself can fail.
    let mut cmd = Command::new("cargo");
    cmd.args(["check", "--release", "--locked", "--features", "test-seam"]);
    let out = reaper::output(&mut cmd).expect("cargo check failed");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "test-seam release build must fail: {stderr}"
    );
    assert!(
        stderr.contains("feature `test-seam` arms a caller-identity override that bypasses"),
        "expected test-seam compile_error, not a toolchain/dependency failure: {stderr}"
    );
}

#[test]
fn safety_floor_suite_lock_serializes_review() {
    let host = FloorHost::new();
    let origin = host.path("origin.git");
    let repo = host.path("repo");
    let state = host.path("state");
    let bin = host.path("bin");
    std::fs::create_dir(&bin).unwrap();
    git(
        host.root.path(),
        &["init", "-q", "--bare", origin.to_str().unwrap()],
    );
    git(
        host.root.path(),
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            repo.to_str().unwrap(),
        ],
    );
    git(&repo, &["config", "user.email", "floor@example.test"]);
    git(&repo, &["config", "user.name", "floor"]);
    git(&repo, &["checkout", "-qb", "main"]);
    let ready = host.path("gate-ready");
    let ran = host.path("suite-ran");
    std::fs::write(repo.join("cadence-review.toml"), format!(
        "prepare = []\ngates = ['touch {}']\nfull_suite = 'touch {}'\ntest_globs = ['tests/**']\ntest_command = 'true'\n",
        ready.display(), ran.display()
    )).unwrap();
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["checkout", "-qb", "pr-7"]);
    std::fs::write(repo.join("pr.txt"), "head\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "head"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["push", "-q", "origin", "HEAD:refs/pull/7/head"]);
    let gh = bin.join("gh");
    std::fs::write(&gh, "#!/bin/sh\ncase \"$1 $2\" in\n  'pr view') printf '%s\\n' \"$FLOOR_PR_JSON\" ;;\n  'pr list') printf '[]\\n' ;;\n  *) exit 1 ;;\nesac\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let pr = serde_json::json!({
        "number": 7, "title": "floor", "url": "https://example.test/7",
        "headRefName": "pr-7", "headRefOid": head, "baseRefName": "main",
        "files": [{"path": "pr.txt"}], "state": "OPEN"
    });
    let lock = host.path("suite.lock");
    let held = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock)
        .unwrap();
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    let mut cmd = host.command();
    cmd.args([
        "--state-dir",
        state.to_str().unwrap(),
        "review",
        "--repo",
        "o/r",
        "7",
        "--stress",
        "0",
    ])
    .current_dir(&repo)
    .env(
        "PATH",
        format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
    )
    .env("FLOOR_PR_JSON", pr.to_string())
    .env("CADENCE_SUITE_LOCK", &lock)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = OwnChild(Some(cmd.spawn().unwrap()));
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready.exists() && child.pending() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        ready.exists(),
        "review never reached its gate before suite lock"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(child.pending(), "review finished despite held suite lock");
    assert!(
        !ran.exists(),
        "full suite ran before the flock was released"
    );
    drop(held);
    let out = child.wait(Duration::from_secs(20));
    assert!(
        ran.exists(),
        "suite did not run after flock release: {out:?}"
    );
    assert!(
        out.status.code().is_some_and(|code| code <= 2),
        "review failed before writing a verdict: {out:?}"
    );
    let reports = state.join("reviews");
    let report = std::fs::read_dir(reports)
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(report.path()).unwrap()).unwrap();
    assert_eq!(
        report["suite_lock"]["ownership"], "outer-review",
        "{report}"
    );
    assert_eq!(report["full_suite"]["outcome"], "ok", "{report}");
}

#[test]
fn safety_floor_port_fence_refuses_occupied_lock() {
    let mut host = FloorHost::new();
    // Reserve only a test-range port that is currently bindable. The flock,
    // not a bound socket, must be what refuses the explicit sandbox pick.
    let port = (3110..=3199)
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .unwrap();
    let lock = host.path("locks").join(format!("{port}.lock"));
    let held = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock)
        .unwrap();
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    let port_arg = port.to_string();
    // Mark before spawning: a timeout or panic during `up` may leave our own
    // detached children, which Drop must stop even when the CLI fails.
    host.sandbox_started = true;
    let out = host.run(&["sandbox", "up", "floor", "--port", &port_arg]);
    assert!(
        !out.status.success(),
        "fenced sandbox up succeeded: {out:?}"
    );
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(
        error.contains(&format!("port {port} is in use")),
        "wrong refusal: {error}"
    );
    assert!(
        !host.path("boxes/floor/.cadence-sandbox").exists(),
        "fenced port created a sandbox"
    );
    drop(held);
    let out = host.run(&["sandbox", "up", "floor", "--port", &port_arg]);
    assert!(
        out.status.success(),
        "own port should work after release: {out:?}"
    );
    let up: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(up["port"], port, "{up}");
    let down = host.run(&["sandbox", "down", "floor"]);
    assert!(down.status.success(), "own sandbox down: {down:?}");
    let reset = host.run(&["sandbox", "reset", "floor"]);
    assert!(reset.status.success(), "own sandbox reset: {reset:?}");
    host.sandbox_started = false;
}

#[test]
fn safety_floor_cli_refuses_unknown_verb() {
    // Basic CLI refusal: unknown verbs exit non-zero. This proves the binary
    // still enforces its command surface during the clean-slate window.
    let mut cmd = Command::new("target/debug/cadence");
    cmd.args(["nonexistent-verb"]);
    let out = reaper::output(&mut cmd).expect("cadence binary must exist to test refusal");
    assert!(!out.status.success(), "unknown verb must refuse");
}
