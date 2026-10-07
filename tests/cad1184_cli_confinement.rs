//! CAD-1184 CLI file-confinement acceptance: a master-scoped caller's
//! `cadence app assistant invoke --input <file>` must open the input file
//! only through `master::open_command_file` — a regular file strictly
//! inside `<state>/master/tmp` — never a bare path open. The typed-JSON
//! reader added for the assistant verbs must not widen the file-reading
//! authority the other agent-facing `--file` verbs already refuse.
//!
//! Authored independently of the implementer (spec/security reviewer for
//! PR #827, head e3518bb6cfd8fc1e30a2f104b06c328753e6b6e8). Exercises the
//! real `cadence` binary against a private temp state dir with a
//! synthetic `CADENCE_ALIAS=master` set only inside the spawned child —
//! no real operator identity, production state, credential or identity
//! variable is touched. Expected baseline on the unfixed candidate:
//! the reader's `std::fs::File::open` accepts the outside path and the
//! run proceeds to a daemon/schema failure — this test FAILS there,
//! which is the captured bad case, not a green pass.
//!
//! If a front gate (e.g. a master command proof) refuses before the
//! reader is reached, the refusal text will not contain the confinement
//! message; this file asserts on the confinement marker so such a
//! refusal cannot be misreported as the file guard working.

use cadence_agent::reaper;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::{Builder, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");

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
            // A reaped child's exit output is irrelevant; killing is the
            // cleanup guarantee.
            let _ = child;
        }
    }
}

struct Host {
    root: TempDir,
}

impl Host {
    fn new() -> Self {
        let root = Builder::new()
            .prefix("cad1184-cli-confine-")
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

    /// A `cadence` invocation confined to this fixture's HOME/XDG/TMPDIR
    /// and this fixture's state dir, presenting as the synthetic master
    /// caller. The outside input file sits under `outside/`, clearly
    /// outside `<state>/master/tmp`.
    fn run_as_master(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(BINARY);
        cmd.env("HOME", self.path("home"))
            .env("XDG_STATE_HOME", self.path("xdg"))
            .env("TMPDIR", self.path("tmp"))
            .env("CADENCE_SANDBOX_ROOT", self.path("boxes"))
            .env("CADENCE_ALIAS", "master")
            // A managed caller may not select an org; --state-dir is the
            // scoped selector and stays inside the fixture.
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
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        OwnChild(Some(reaper::spawn(&mut cmd).unwrap())).wait(Duration::from_secs(25))
    }
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The marker every confinement refusal shares (`master::open_command_file`
/// / `file_refused`): the caller is told to write into `master/tmp`.
fn confinement_marker(text: &str) -> bool {
    text.contains("master/tmp") || text.contains("master\\tmp")
}

/// A real typed-JSON input (arrays, an integer revision) written OUTSIDE
/// the state dir must be refused by the file confinement guard with the
/// documented master/tmp refusal — before or at the point of opening the
/// input, regardless of whether a daemon is reachable. A schema error,
/// an "unreachable" daemon error, or an argument-usage error does NOT
/// prove the file guard ran, so they are assertion failures here.
#[test]
fn assistant_invoke_refuses_input_outside_master_tmp() {
    let host = Host::new();
    let outside = host.path("outside");
    let input = outside.join("tags.json");
    std::fs::write(
        &input,
        serde_json::json!({
            "customer_id": "cust-x",
            "tags": ["vip", "pilot"],
            "expected_revision": 1
        })
        .to_string(),
    )
    .unwrap();

    let state = host.path("state");
    let output = host.run_as_master(&[
        "--state-dir",
        state.to_str().unwrap(),
        "app",
        "assistant",
        "invoke",
        "install-fixture",
        "--context-id",
        "ctx-fixture",
        "--message",
        "m-fixture",
        "--token",
        "fixture-token",
        "--action-id",
        "customer.tags.update",
        "--operation-id",
        "op-fixture",
        "--input",
        input.to_str().unwrap(),
    ]);
    let text = stderr_text(&output);
    assert!(
        !output.status.success(),
        "an outside-path input must not reach a successful RPC: {text}"
    );
    assert!(
        confinement_marker(&text),
        "refusal must be the master/tmp file confinement, not a daemon or \
         schema failure — got: {text}"
    );
    assert!(
        !text.contains("daemon") && !text.contains("unreachable"),
        "the guard must fire before any daemon contact: {text}"
    );
}

/// A typed input file written INSIDE `<state>/master/tmp` stays openable
/// by the confined reader — the guard confines location, not JSON type.
/// The run is then free to fail downstream (no daemon in this fixture);
/// the assertion is only that the confinement refusal is absent, proving
/// typed objects/arrays/integers are not what the guard rejects.
#[test]
fn assistant_invoke_input_inside_master_tmp_is_not_confined() {
    let host = Host::new();
    let state = host.path("state");
    let inside = state.join("master/tmp/input.json");
    std::fs::write(
        &inside,
        serde_json::json!({
            "customer_id": "cust-x",
            "tags": ["vip"],
            "expected_revision": 1
        })
        .to_string(),
    )
    .unwrap();

    let output = host.run_as_master(&[
        "--state-dir",
        state.to_str().unwrap(),
        "app",
        "assistant",
        "invoke",
        "install-fixture",
        "--context-id",
        "ctx-fixture",
        "--message",
        "m-fixture",
        "--token",
        "fixture-token",
        "--action-id",
        "customer.tags.update",
        "--operation-id",
        "op-fixture-inside",
        "--input",
        inside.to_str().unwrap(),
    ]);
    let text = stderr_text(&output);
    assert!(
        !confinement_marker(&text),
        "a regular file inside master/tmp must not hit the confinement \
         refusal — got: {text}"
    );
}
