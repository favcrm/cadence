//! CAD-554: every operator-shaped script survives a vanished /proc hop.
//!
//! Each script waits for its `setsid -f` detach to settle by walking
//! /proc PPid links up from itself ([`op::LINEAGE_WAIT_PY`]). A hop can
//! vanish mid-walk — the detach's intermediates exit fast and the
//! kernel reparents only once they do — the shape behind CAD-545's 20s
//! `answer()` timeouts and rev-310's finding on #310: the walk raised
//! `FileNotFoundError` and the detached caller died before landing its
//! answer.
//!
//! These cases inject that vanished hop deterministically. A keeper
//! process holds a live hop in the script's ancestry, and a patched
//! `open` makes the walk's first hop read raise exactly the
//! `FileNotFoundError` the kernel race raises — once. The walk must
//! treat that as "not yet proven" and keep polling, and each script
//! must still land its answer. Delete the guard in
//! [`op::LINEAGE_WAIT_PY`] and every case here times out with no answer
//! landed.

// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

#[path = "support/operator.rs"]
mod op;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

/// How long a script may take to land its answer once its detach
/// settles. Generous: the injected hop adds one poll, not a wait.
const LANDED_BOUND: Duration = Duration::from_secs(20);

/// The reply the test's own socket/server fixtures give.
const RPC_REPLY: &str = "{\"ok\":true}\n";
const HTTP_REPLY: &str = "HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nhi";

/// Which operator script a case runs, and the fixtures its argv needs.
#[derive(Clone, Copy, Debug)]
enum Kind {
    /// [`op::rpc_script`]: the frame rides argv.
    Rpc,
    /// [`op::rpc_script_from_file`]: the frame rides a private file.
    RpcFile,
    /// [`op::cli_script`]: argv with empty stdin.
    Cli,
    /// [`op::cli_script_stdin_env`]: one env var to the child's stdin.
    CliStdin,
    /// [`op::exec_script`]: `{argv, env, cwd}` from a spec file.
    Exec,
    /// [`op::http_script`]: one HTTP request read from a file.
    Http,
}

/// Every live operator script, with the kind of fixture it needs.
fn cases() -> Vec<(&'static str, String, Kind)> {
    vec![
        ("rpc (frame on argv)", op::rpc_script(), Kind::Rpc),
        (
            "rpc (frame file)",
            op::rpc_script_from_file(),
            Kind::RpcFile,
        ),
        ("cli", op::cli_script(), Kind::Cli),
        (
            "cli (stdin env)",
            op::cli_script_stdin_env(),
            Kind::CliStdin,
        ),
        ("exec", op::exec_script(), Kind::Exec),
        ("http", op::http_script(), Kind::Http),
    ]
}

/// Every live operator script embeds the one guarded walk — no
/// hand-copied variant can come back (CAD-554).
#[test]
fn every_operator_script_is_built_on_the_shared_walk() {
    for (name, script, _) in cases() {
        assert!(
            script.contains(op::LINEAGE_WAIT_PY),
            "{name} does not embed op::LINEAGE_WAIT_PY"
        );
    }
}

#[test]
fn rpc_script_survives_a_vanished_hop() {
    let run = run_injected("rpc", op::rpc_script(), Kind::Rpc);
    assert_eq!(landed(&run), RPC_REPLY, "the response line must land");
}

#[test]
fn rpc_from_file_script_survives_a_vanished_hop() {
    let run = run_injected(
        "rpc (frame file)",
        op::rpc_script_from_file(),
        Kind::RpcFile,
    );
    assert_eq!(landed(&run), RPC_REPLY, "the response line must land");
}

#[test]
fn cli_script_survives_a_vanished_hop() {
    let run = run_injected("cli", op::cli_script(), Kind::Cli);
    let v: Value = serde_json::from_str(&landed(&run)).unwrap();
    assert_eq!(v["rc"], 0, "{v}");
    assert_eq!(v["stdout"], "ok", "{v}");
}

#[test]
fn cli_stdin_script_survives_a_vanished_hop() {
    let run = run_injected(
        "cli (stdin env)",
        op::cli_script_stdin_env(),
        Kind::CliStdin,
    );
    let v: Value = serde_json::from_str(&landed(&run)).unwrap();
    assert_eq!(v["rc"], 0, "{v}");
    assert_eq!(v["stdout"], "tok", "{v}");
}

#[test]
fn exec_script_survives_a_vanished_hop() {
    let run = run_injected("exec", op::exec_script(), Kind::Exec);
    let v: Value = serde_json::from_str(&landed(&run)).unwrap();
    assert_eq!(v["rc"], 0, "{v}");
    assert_eq!(
        std::fs::read_to_string(&run.side).unwrap(),
        "ok",
        "the spec'd command's stdout must land beside the rc"
    );
}

#[test]
fn http_script_survives_a_vanished_hop() {
    let run = run_injected("http", op::http_script(), Kind::Http);
    assert_eq!(landed(&run), HTTP_REPLY, "the raw reply must land");
}

/// The script's landed answer.
struct Run {
    _dir: tempfile::TempDir,
    /// The answer the script lands (`.json` for cli/exec, raw for
    /// rpc/http).
    out: PathBuf,
    /// The exec kind's captured stdout, beside `out`.
    side: PathBuf,
}

fn landed(run: &Run) -> String {
    std::fs::read_to_string(&run.out).unwrap()
}

/// Runs `script` as a keeper's child — the keeper is a live hop in its
/// /proc ancestry — with the injected vanished hop in front. Panics
/// unless the script lands its answer; asserts the hop actually fired,
/// so a case can never pass without exercising the guard.
fn run_injected(name: &str, script: String, kind: Kind) -> Run {
    let dir = tempfile::Builder::new()
        .prefix("onlin")
        .tempdir_in("/tmp")
        .unwrap();
    let fired = dir.path().join("fired");
    let script_path = dir.path().join("script.py");
    std::fs::write(&script_path, injected(&script, &fired)).unwrap();
    let keeper = dir.path().join("keeper.py");
    std::fs::write(&keeper, KEEPER_PY).unwrap();

    let (args, envs, out, side) = fixtures(dir.path(), kind);

    let mut child = Command::new("python3")
        .arg(&keeper)
        .arg(&fired)
        .arg(&script_path)
        .args(&args)
        .envs(envs)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + LANDED_BOUND;
    while !out.exists() {
        assert!(
            Instant::now() < deadline,
            "{name}: the script never landed its answer — the vanished hop killed it"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let status = child.wait().unwrap();
    assert!(status.success(), "{name}: the keeper failed: {status}");
    assert!(fired.exists(), "{name}: the injected hop never fired");
    let hop = std::fs::read_to_string(&fired).unwrap();
    assert!(
        hop.starts_with("/proc/") && hop.ends_with("/status"),
        "{name}: fired on {hop:?}, not a hop's /proc status"
    );
    Run {
        _dir: dir,
        out,
        side,
    }
}

/// `script` behind the injected vanished hop: the walk's first read of
/// a hop's `/proc/<pid>/status` — never its own — raises the exact
/// `FileNotFoundError` the kernel race raises, once, and marks `fired`.
fn injected(script: &str, fired: &Path) -> String {
    format!(
        r#"import builtins, os

_real_open = builtins.open
_fired_path = "{fired}"
_self = os.getpid()
_fired = [False]

def open(path, *args, **kwargs):
    if (not _fired[0] and isinstance(path, str)
            and path.startswith("/proc/") and path.endswith("/status")
            and path != "/proc/%d/status" % _self):
        _fired[0] = True
        with _real_open(_fired_path, "w") as f:
            f.write(path)
        raise FileNotFoundError(2, "No such file or directory", path)
    return _real_open(path, *args, **kwargs)

builtins.open = open

"#,
        fired = fired.display(),
    ) + script
}

/// Runs the script as this process's child — a live hop in its /proc
/// ancestry, so the walk always reaches a hop read — until the injected
/// hop fires, then leaves so the script reparents and the walk can
/// prove it is off `runner`'s ancestry.
const KEEPER_PY: &str = r#"import os, sys, time

fired, script = sys.argv[1:3]
argv = sys.argv[3:]

if os.fork() == 0:
    os.execvp("python3", ["python3", script] + argv)

deadline = time.time() + 60
while not os.path.exists(fired):
    if time.time() > deadline:
        os._exit(3)
    time.sleep(0.005)
os._exit(0)
"#;

/// The argv, env and answer paths one case's script needs. `out` is the
/// script's landed answer; `side` is the exec kind's captured stdout.
fn fixtures(dir: &Path, kind: Kind) -> (Vec<String>, Vec<(String, String)>, PathBuf, PathBuf) {
    let out = dir.join("out");
    let side = dir.join("out.stdout");
    let runner = std::process::id().to_string();
    match kind {
        Kind::Rpc | Kind::RpcFile => {
            let sock = dir.join("s.sock");
            let listener = UnixListener::bind(&sock).unwrap();
            thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(s.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                s.write_all(RPC_REPLY.as_bytes()).unwrap();
            });
            let frame = r#"{"method":"health","params":{}}"#;
            let frame_arg = if matches!(kind, Kind::Rpc) {
                frame.to_string()
            } else {
                let path = dir.join("frame.json");
                std::fs::write(&path, frame).unwrap();
                path.display().to_string()
            };
            (
                vec![
                    sock.display().to_string(),
                    frame_arg,
                    out.display().to_string(),
                    runner,
                ],
                Vec::new(),
                out,
                side,
            )
        }
        Kind::Cli => (
            vec![
                out.display().to_string(),
                runner,
                "/bin/sh".into(),
                "-c".into(),
                "printf ok".into(),
            ],
            Vec::new(),
            out,
            side,
        ),
        Kind::CliStdin => (
            vec![
                out.display().to_string(),
                runner,
                "/bin/sh".into(),
                "-c".into(),
                "cat".into(),
            ],
            vec![("TOKEN_STDIN".to_string(), "tok".to_string())],
            out,
            side,
        ),
        Kind::Exec => {
            let spec = dir.join("spec.json");
            std::fs::write(
                &spec,
                serde_json::json!({
                    "argv": ["/bin/sh", "-c", "printf ok"],
                    "env": {"PATH": std::env::var("PATH").unwrap_or_default()},
                    "cwd": dir.display().to_string(),
                })
                .to_string(),
            )
            .unwrap();
            (
                vec![
                    spec.display().to_string(),
                    out.display().to_string(),
                    runner,
                ],
                Vec::new(),
                out,
                side,
            )
        }
        Kind::Http => {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf).unwrap();
                s.write_all(HTTP_REPLY.as_bytes()).unwrap();
            });
            let req = dir.join("req.txt");
            std::fs::write(&req, "GET / HTTP/1.0\r\nHost: x\r\n\r\n").unwrap();
            (
                vec![
                    req.display().to_string(),
                    out.display().to_string(),
                    port.to_string(),
                    runner,
                ],
                Vec::new(),
                out,
                side,
            )
        }
    }
}
