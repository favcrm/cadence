//! CAD-1726 refusal acceptance check, written by the acceptance-check
//! author (not the implementer): the master's `cadence artifact put --file`
//! must read only through `master::open_command_file` — a regular,
//! non-symlink file strictly inside `<state>/master/tmp` — and never take
//! stdin. Exercises the real `cadence` binary against a private temp state
//! dir with a synthetic `CADENCE_ALIAS=master` set only inside the spawned
//! child (the CAD-1184 harness): no operator identity, production state or
//! credential is touched, and no daemon is started.
//!
//! Each refusal is asserted by its reason. The control — a regular file
//! inside master/tmp — passes the guard and goes on to contact the daemon
//! (absent here), so a harness that refuses everything cannot pass.

use cadence_agent::reaper;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::{Builder, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
const SECRET: &str = "Zed Canvas Outside Secret";

/// Never leave a spawned child running after a failed assertion.
struct OwnChild(Option<Child>);

impl OwnChild {
    fn wait(&mut self, deadline: Duration) -> Output {
        let until = Instant::now() + deadline;
        loop {
            let pending = self.0.as_mut().unwrap().try_wait().unwrap().is_none();
            if !pending {
                return self.0.take().unwrap().wait_with_output().unwrap();
            }
            assert!(Instant::now() < until, "child exceeded {deadline:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
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

struct Host {
    root: TempDir,
}

impl Host {
    fn new() -> Self {
        let root = Builder::new()
            .prefix("cad1726-put-")
            .tempdir_in("/tmp")
            .unwrap();
        for dir in ["home", "xdg", "tmp", "state", "state/master/tmp", "outside"] {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// `cadence --state-dir <state> artifact put --kind md --title T
    /// --file <file>` as the synthetic master, confined to this fixture.
    fn put_as_master(&self, file: &str) -> (bool, String) {
        let state = self.path("state");
        let mut cmd = Command::new(BINARY);
        cmd.env("HOME", self.path("home"))
            .env("XDG_STATE_HOME", self.path("xdg"))
            .env("TMPDIR", self.path("tmp"))
            .env("CADENCE_SANDBOX_ROOT", self.path("boxes"))
            .env("CADENCE_ALIAS", "master")
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_PM_DIR")
            .env_remove("CADENCE_PROFILE")
            .env_remove("CADENCE_ROLLOUT_AS")
            .env_remove("CADENCE_SANDBOX_ALLOW_GLOBAL")
            .env_remove("XDG_DATA_HOME");
        for name in cadence_agent::adapter::PROVIDER_COMMAND_VARS {
            cmd.env(name, cadence_agent::adapter::REFUSED_COMMAND);
        }
        for name in ["CARGO_HOME", "RUSTUP_HOME"] {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
        cmd.args([
            "--state-dir",
            state.to_str().unwrap(),
            "artifact",
            "put",
            "--kind",
            "md",
            "--title",
            "Confined",
            "--file",
            file,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let output = OwnChild(Some(reaper::spawn(&mut cmd).unwrap())).wait(Duration::from_secs(25));
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), text)
    }

    /// Nothing the put could have stored is present: no custody dir and
    /// no daemon database under the fixture's state dir.
    fn assert_nothing_stored(&self, what: &str) {
        let state = self.path("state");
        assert!(
            !state.join("artifacts").exists(),
            "{what}: an artifacts custody dir appeared"
        );
        let stored: Vec<_> = std::fs::read_dir(&state)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "master")
            .collect();
        assert!(stored.is_empty(), "{what}: state gained {stored:?}");
    }
}

#[test]
fn cad1726_master_artifact_put_file_is_confined_to_master_tmp() {
    let host = Host::new();
    let tmp = host.path("state/master/tmp");
    let outside = host.path("outside").join("private.md");
    std::fs::write(&outside, format!("# {SECRET}\n")).unwrap();
    let tmp_shown = tmp.display().to_string();

    // ---- A path outside master/tmp: refused, nothing read. ----
    let path = outside.to_str().unwrap().to_string();
    let (ok, text) = host.put_as_master(&path);
    assert!(!ok, "outside path accepted: {text}");
    assert!(
        text.contains(&format!(
            "use the write tool into {tmp_shown}, then --file <that path> — refused '{path}'"
        )),
        "outside path: {text}"
    );
    assert!(!text.contains(SECRET), "outside path leaked bytes: {text}");
    host.assert_nothing_stored("outside path");

    // ---- A `..` walk out of master/tmp: refused the same way. ----
    let walk = tmp
        .join("..")
        .join("..")
        .join("..")
        .join("outside/private.md");
    let walk = walk.to_str().unwrap().to_string();
    let (ok, text) = host.put_as_master(&walk);
    assert!(!ok, "dot-dot walk accepted: {text}");
    assert!(
        text.contains("then --file <that path> — refused") && text.contains(&walk),
        "dot-dot walk: {text}"
    );
    assert!(!text.contains(SECRET), "dot-dot walk leaked bytes: {text}");
    host.assert_nothing_stored("dot-dot walk");

    // ---- A symlink inside master/tmp pointing outside: refused. ----
    let link = tmp.join("link.md");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let link = link.to_str().unwrap().to_string();
    let (ok, text) = host.put_as_master(&link);
    assert!(!ok, "symlink accepted: {text}");
    assert!(
        text.contains(&format!(
            "use the write tool into {tmp_shown}, then --file <that path> — refused '{link}': symlink"
        )),
        "symlink: {text}"
    );
    assert!(!text.contains(SECRET), "symlink leaked bytes: {text}");
    host.assert_nothing_stored("symlink");

    // ---- Stdin: refused for the master. ----
    let (ok, text) = host.put_as_master("-");
    assert!(!ok, "stdin accepted: {text}");
    assert!(
        text.contains("no stdin — use the write tool into master/tmp, then --file <that path>"),
        "stdin: {text}"
    );
    host.assert_nothing_stored("stdin");

    // ---- Control: a regular file inside master/tmp passes the guard
    // and the CLI goes on to the daemon (none runs in this fixture). ----
    let inside = tmp.join("report.md");
    std::fs::write(&inside, "# inside\n").unwrap();
    let (ok, text) = host.put_as_master(inside.to_str().unwrap());
    assert!(
        !ok,
        "no daemon runs here, so the put cannot succeed: {text}"
    );
    assert!(
        !text.contains("refused '") && !text.contains("no stdin"),
        "a regular file inside master/tmp was refused by the confinement: {text}"
    );
    assert!(
        text.contains("Daemon is not reachable"),
        "the control must reach the daemon call, past the file guard: {text}"
    );
}
