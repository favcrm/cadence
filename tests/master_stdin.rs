//! CAD-614: the Claude master shares the allowlist and has no Pi guard.
//! `plan propose`, `report file` (including a verdict and bare intake),
//! and `master escalate` must refuse `--file -` and a missing `--file`
//! when `CADENCE_ALIAS=master`, before stdin is read and before anything
//! is stored.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SECRET: &str = "STDIN-SECRET-proc-self-environ-cad614";

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cadence"))
}

fn run(pm: &Path, state: &Path, master: bool, args: &[&str]) -> (bool, String) {
    let mut cmd = bin();
    cmd.arg("--state-dir")
        .arg(state)
        .args(args)
        .env("CADENCE_PM_DIR", pm)
        .env("CADENCE_STATE_DIR", state)
        .env("HOME", state.join("home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if master {
        cmd.env("CADENCE_ALIAS", "master");
    } else {
        cmd.env_remove("CADENCE_ALIAS");
    }
    let mut child = cadence_agent::reaper::spawn(&mut cmd).unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(SECRET.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn contains_secret(dir: &Path) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in rd.flatten() {
            let path = ent.path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            let Ok(ft) = ent.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                if bytes.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()) {
                    return Some(path);
                }
            }
        }
    }
    None
}

#[test]
fn master_stdin_is_refused_on_every_allowlisted_file_command() {
    let tmp = tempfile::tempdir().unwrap();
    let pm = tmp.path().join("pm");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(state.join("home")).unwrap();
    let init = cadence_agent::reaper::output(
        bin()
            .args(["--state-dir", state.to_str().unwrap(), "issue", "init"])
            .env("CADENCE_PM_DIR", &pm)
            .env("HOME", state.join("home"))
            .env_remove("CADENCE_ALIAS"),
    )
    .unwrap();
    assert!(
        init.status.success(),
        "issue init: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let cases: &[&[&str]] = &[
        &["plan", "propose", "--project", "demo", "--file", "-"],
        &["plan", "propose", "--project", "demo"],
        &[
            "report", "file", "--task", "D-1", "--kind", "done", "--file", "-",
        ],
        &["report", "file", "--task", "D-1", "--kind", "done"],
        &[
            "report", "file", "--task", "D-1", "--kind", "verdict", "--file", "-",
        ],
        &["report", "file", "--task", "D-1", "--kind", "verdict"],
        &["report", "--file", "-"],
        &["report"],
        &["master", "escalate", "D-1", "q.md", "--file", "-"],
    ];
    for args in cases {
        let (ok, text) = run(&pm, &state, true, args);
        assert!(!ok, "{args:?} stored stdin: {text}");
        assert!(
            text.contains(cadence_agent::master::NO_STDIN),
            "{args:?}: {text}"
        );
        assert!(!text.contains(SECRET), "{args:?} echoed stdin: {text}");
    }
    assert!(
        contains_secret(&pm).is_none(),
        "secret stored in the tracker: {:?}",
        contains_secret(&pm)
    );
    assert!(
        contains_secret(&state).is_none(),
        "secret stored in state: {:?}",
        contains_secret(&state)
    );

    // A non-master still gets the stdin pipe. Refusing every caller
    // would turn this into the master's message.
    let (_ok, text) = run(
        &pm,
        &state,
        false,
        &["plan", "propose", "--project", "demo", "--file", "-"],
    );
    assert!(
        !text.contains(cadence_agent::master::NO_STDIN),
        "worker stdin was refused: {text}"
    );
}
