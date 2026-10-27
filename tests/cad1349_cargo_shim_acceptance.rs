//! Independent CAD-1349 acceptance: generate the lane cargo shim through the
//! real non-installed cadence binary, then exercise it through the restricted
//! result-test runner with isolated build-slot state.

use cadence_agent::issue::{write, Pm};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::Builder;

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
const PROJECT: &str = "cad1349";
const ISSUE: &str = "C1349-1";

struct Fixture {
    home: PathBuf,
    pm: Pm,
    repo: PathBuf,
    name: String,
    lane: PathBuf,
    shim: PathBuf,
}

struct RegisteredFixtureCleanup {
    state: PathBuf,
    home: PathBuf,
    xdg_state: PathBuf,
    driver_stop: PathBuf,
    tmux_state: PathBuf,
    armed: bool,
}

impl RegisteredFixtureCleanup {
    fn close(&mut self) -> Result<(), String> {
        let stop_file = fs::write(&self.driver_stop, "stop\n")
            .map_err(|error| format!("write fixture driver stop file: {error}"));
        let state = self.state.to_string_lossy();
        let mut stop_daemon = Command::new(BINARY);
        stop_daemon
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .args(["--state-dir", state.as_ref(), "daemon", "stop"])
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", &self.xdg_state);
        let stopped = cadence_agent::reaper::output(&mut stop_daemon)
            .map_err(|error| format!("run fixture daemon stop: {error}"));

        let mut kill_tmux = Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/fake_tmux.py"
        ));
        kill_tmux
            .args(["-L", "isolated", "kill-server"])
            .env("CADENCE_FAKE_TMUX_STATE", &self.tmux_state);
        let killed = cadence_agent::reaper::output(&mut kill_tmux);

        stop_file?;
        match stopped {
            Err(error) => Err(error),
            Ok(output) if !output.status.success() => Err(format!(
                "stop fixture daemon: {}",
                String::from_utf8_lossy(&output.stderr)
            )),
            Ok(_) => match killed {
                Err(error) => Err(format!("kill fixture tmux: {error}")),
                Ok(output) if !output.status.success() => Err(format!(
                    "kill fixture tmux: {}",
                    String::from_utf8_lossy(&output.stderr)
                )),
                Ok(_) => {
                    self.armed = false;
                    Ok(())
                }
            },
        }
    }
}

impl Drop for RegisteredFixtureCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.close();
        }
    }
}

fn fixture(root: &Path, helper: &Path, name: &str) -> Fixture {
    let home = root.join(format!("{name}-home"));
    let repo = root.join(format!("{name}-repo"));
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&repo).unwrap();
    git(&home, &repo, &["init", "--quiet", "--initial-branch=main"]);
    fs::write(repo.join(".gitignore"), "target/\n.cadence/\n").unwrap();
    fs::write(repo.join("tracked.txt"), "isolated fixture\n").unwrap();
    git(&home, &repo, &["add", ".gitignore", "tracked.txt"]);
    git(&home, &repo, &["commit", "--quiet", "-m", "fixture base"]);
    let origin = root.join(format!("{name}-origin.git"));
    fs::create_dir_all(&origin).unwrap();
    git(
        &home,
        &origin,
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    git(
        &home,
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(
        &home,
        &repo,
        &["push", "--quiet", "--set-upstream", "origin", "main"],
    );
    git(&home, &repo, &["remote", "set-head", "origin", "main"]);

    let pm = Pm::init(&root.join(format!("{name}-pm"))).expect("isolated PM");
    write::project_add(
        &pm,
        PROJECT,
        "C1349",
        &[repo.display().to_string()],
        &[],
        &[],
        None,
    )
    .expect("isolated project");
    write::new_issue(
        &pm,
        &repo,
        Some(PROJECT),
        "cargo shim runner acceptance",
        Some("P2"),
        None,
        &[],
        None,
        None,
        &[],
        Some(ISSUE),
        Some("Private CAD-1349 shim fixture."),
        "cad1349-fixture",
    )
    .expect("isolated issue");

    let mut start = Command::new(helper);
    start
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args([
            "issue",
            "start",
            ISSUE,
            "--repo",
            repo.to_str().unwrap(),
            "--name",
            name,
            "--owner",
            "cad1349-fixture",
            "--by",
            "cad1349-fixture",
        ])
        .current_dir(&repo)
        .env("CADENCE_PM_DIR", &pm.dir)
        .env("HOME", &home)
        .env("XDG_STATE_HOME", root.join(format!("{name}-state")))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env_remove("CADENCE_STATE_DIR")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = cadence_agent::reaper::output(&mut start).expect("run isolated issue start");
    assert!(
        output.status.success(),
        "real issue start failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let started: Value = serde_json::from_slice(&output.stdout).expect("issue start JSON");
    let lane = PathBuf::from(started["worktree"].as_str().expect("lane path"));
    let git_dir = git_output(&home, &lane, &["rev-parse", "--absolute-git-dir"]);
    let shim = PathBuf::from(git_dir).join("cadence-hooks/bin/cargo");
    assert!(
        shim.is_file(),
        "worktree setup did not generate its cargo shim"
    );
    assert_embedded_helper(&shim, helper);

    Fixture {
        home,
        pm,
        repo,
        name: name.to_string(),
        lane,
        shim,
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn assert_generated_slot_env(env_file: &Path, helper: &Path) {
    let expected = fs::canonicalize(helper).expect("canonical helper executable");
    let generated = fs::read_to_string(env_file).expect("generated lane .env");
    let exports: Vec<_> = generated
        .lines()
        .filter(|line| line.starts_with("export CADENCE_BUILD_SLOT="))
        .collect();
    assert_eq!(
        exports.len(),
        1,
        "expected exactly one helper export: {generated:?}"
    );
    assert_eq!(
        exports[0],
        format!("export CADENCE_BUILD_SLOT={}", shell_quote(&expected))
    );
    assert!(
        !generated
            .lines()
            .any(|line| line.starts_with("CADENCE_BUILD_SLOT=")),
        "stale unexported helper assignment remains in {}: {generated:?}",
        env_file.display()
    );

    let mut source = Command::new("sh");
    source
        .args([
            "-c",
            ". \"$1\"; sh -c 'printf %s \"$CADENCE_BUILD_SLOT\"'",
            "cad1349-env",
        ])
        .arg(env_file)
        .env_remove("CADENCE_BUILD_SLOT");
    let sourced = cadence_agent::reaper::output(&mut source)
        .expect("source generated lane .env in child shell");
    assert!(
        sourced.status.success(),
        "sourcing generated .env failed: {}",
        String::from_utf8_lossy(&sourced.stderr)
    );
    assert_eq!(
        String::from_utf8(sourced.stdout).unwrap(),
        expected.display().to_string()
    );
}

fn assert_embedded_helper(shim: &Path, input: &Path) {
    let generated = fs::read_to_string(shim).unwrap();
    // current_exe() resolves the executable identity. Compare against its
    // canonical on-disk path, not the possibly aliased argv path.
    let expected_helper = fs::canonicalize(input).expect("canonical helper executable");
    let expected_assignment = format!("cad={}", shell_quote(&expected_helper));
    let observed_assignments: Vec<_> = generated
        .lines()
        .filter(|line| line.starts_with("cad="))
        .collect();
    assert!(
        observed_assignments.contains(&expected_assignment.as_str()),
        "generated shim helper mismatch: expected {expected_assignment:?} from input {input:?} (canonical {expected_helper:?}); observed {observed_assignments:?}",
    );
}

fn git(home: &Path, cwd: &Path, args: &[&str]) {
    let out = git_command(home, cwd, args);
    assert!(
        out.status.success(),
        "git {} failed:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_output(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let out = git_command(home, cwd, args);
    assert!(out.status.success(), "git {} failed", args.join(" "));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git_command(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args([
            "-c",
            "user.name=CAD-1349 fixture",
            "-c",
            "user.email=fixture@invalid",
        ])
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    cadence_agent::reaper::output(&mut command).expect("run fixture git")
}

fn integration_targets() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut command = Command::new(root.join("scripts/result-test-args"));
    command.current_dir(root);
    let out = cadence_agent::reaper::output(&mut command)
        .expect("read active integration target selection");
    assert!(out.status.success(), "result-test-args failed");
    let lines: Vec<_> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let (pairs, remainder) = lines.as_slice().as_chunks::<2>();
    assert!(!lines.is_empty() && remainder.is_empty());
    pairs
        .iter()
        .map(|pair| {
            assert_eq!(pair[0], "--test");
            pair[1].clone()
        })
        .collect()
}

fn rust_test_body(marker: &str) -> String {
    format!(
        r#"#[test]
fn cad1349_runner_keeps_its_execution_boundary() {{
    use std::{{env, fs, path::{{Path, PathBuf}}, process::Command}};
    let runner_temp = PathBuf::from(env::var("RUNNER_TEMP").unwrap());
    let home = PathBuf::from(env::var("HOME").unwrap());
    assert_ne!(home, PathBuf::from(env::var("CAD1349_DRIVER_HOME").unwrap()));
    assert!(home.starts_with(&runner_temp), "runner HOME escaped its temp root: {{home:?}}");
    for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMPDIR"] {{
        let value = PathBuf::from(env::var(key).unwrap());
        assert!(value.starts_with(&runner_temp), "{{key}} escaped runner temp root: {{value:?}}");
    }}
    assert!(env::var_os("CARGO_TARGET_DIR").is_none(), "ambient target dir reached test");
    assert_eq!(env::var("CARGO_BUILD_TARGET_DIR").unwrap(), env::var("CAD1349_EXPECT_TARGET").unwrap());
    let cadence_vars: Vec<_> = env::vars().filter(|(key, _)| key.starts_with("CADENCE_")).map(|(key, _)| key).collect();
    assert!(cadence_vars.iter().all(|key| matches!(key.as_str(), "CADENCE_SUITE_LOCK" | "CADENCE_NEXTTEST_BIN" | "CADENCE_REVIEW_SUITE_LOCK_HELD" | "CADENCE_REVIEW_PR" | "CADENCE_REVIEW_HEAD")), "unexpected CADENCE_* variables: {{cadence_vars:?}}");
    for key in ["CADENCE_PM_DIR", "CADENCE_STATE_DIR", "CADENCE_ALIAS", "CADENCE_BUILD_SLOT"] {{
        assert!(env::var_os(key).is_none(), "caller routing variable {{key}} reached test");
    }}
    let path: Vec<_> = env::split_paths(&env::var_os("PATH").unwrap()).collect();
    assert!(path[0].ends_with("cadence-hooks/bin"), "generated shim not first on PATH: {{path:?}}");
    assert!(path[1].ends_with("cargo-toolchain"), "compiler dir not second on PATH: {{path:?}}");
    let rustc = path[1].join("rustc");
    assert!(rustc.is_file(), "matching rustc missing from private compiler PATH: {{rustc:?}}");
    let rustc_output = Command::new("rustc").arg("--version").output().expect("run rustc from private compiler PATH");
    assert!(rustc_output.status.success(), "private rustc failed: {{}}", String::from_utf8_lossy(&rustc_output.stderr));
    assert!(!path.iter().any(|part| part == Path::new(&env::var("CAD1349_PROVIDER_DIR").unwrap())),
        "host provider CLI directory leaked into test PATH");
    assert!(Command::new("claude").arg("--version").output().is_err(),
        "provider CLI resolved from test PATH");
    fs::write(Path::new(&env::var("CAD1349_MARKERS").unwrap()).join("{marker}"), b"ran").unwrap();
}}
"#
    )
}

fn write_fixture(manifest_dir: &Path, targets: &[String]) {
    fs::create_dir_all(manifest_dir.join("src")).unwrap();
    fs::create_dir_all(manifest_dir.join("tests")).unwrap();
    let mut manifest = String::from(
        "[package]\nname = \"cadence-agent\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\ntest-seam = []\n\n[lib]\npath = \"src/lib.rs\"\n\n[[bin]]\nname = \"cadence-fixture\"\npath = \"src/main.rs\"\n\n[profile.test]\ndebug = false\n",
    );
    fs::write(manifest_dir.join("src/lib.rs"), rust_test_body("lib")).unwrap();
    fs::write(
        manifest_dir.join("src/main.rs"),
        "fn main() {}\n\n".to_owned() + &rust_test_body("bin"),
    )
    .unwrap();
    for name in targets {
        let path = manifest_dir.join("tests").join(format!("{name}.rs"));
        fs::write(&path, rust_test_body(&format!("integration-{name}"))).unwrap();
        manifest.push_str(&format!(
            "\n[[test]]\nname = \"{name}\"\npath = \"tests/{name}.rs\"\n"
        ));
    }
    fs::write(manifest_dir.join("Cargo.toml"), manifest).unwrap();
    fs::write(
        manifest_dir.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"cadence-agent\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
}

fn start_registered_fixture_daemon(
    state: &Path,
    home: &Path,
    xdg_state: &Path,
    root: &Path,
    driver: &Path,
    tmux_command: &Path,
) -> Output {
    let mut command = Command::new(BINARY);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args(["--state-dir", state.to_str().unwrap(), "daemon", "start"])
        .env("HOME", home)
        .env("XDG_STATE_HOME", xdg_state)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", root.join("caller-xdg-data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("CADENCE_PM_DIR", root.join("fixture-pm"))
        .env("CADENCE_TMUX_COMMAND", tmux_command)
        .env("CADENCE_FAKE_TMUX_STATE", root.join("tmux/state.json"))
        .env("CADENCE_STUB_LOCKS", root.join("stub-locks"))
        .env(
            "CADENCE_STUB_COMMAND",
            format!("sh {}", shell_quote(driver)),
        )
        .env_remove("CADENCE_STATE_DIR");
    cadence_agent::reaper::output(&mut command).expect("start registered fixture daemon")
}

fn fixture_cli(state: &Path, home: &Path, xdg_state: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(BINARY);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args(["--state-dir", state.to_str().unwrap()])
        .args(args)
        .env("HOME", home)
        .env("XDG_STATE_HOME", xdg_state)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("CADENCE_PM_DIR", home.join("pm"))
        .env_remove("CADENCE_STATE_DIR");
    cadence_agent::reaper::output(&mut command).expect("run isolated registered-lane CLI")
}

fn wait_for_file(path: &Path, description: &str) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while !path.is_file() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn run_registered_runner_cases(
    lane: &Fixture,
    root: &Path,
    provider: &Path,
    target: &Path,
    markers: &Path,
    helper: &Path,
) -> (String, String, String, String) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let state = root.join("registered-state");
    let home = root.join("registered-home");
    let xdg_state = root.join("registered-xdg-state");
    let driver_home = root.join("registered-driver-home");
    let runner_temp = root.join("registered-runner-temp");
    let tmp = root.join("registered-tmp");
    let xdg_data = root.join("caller-xdg-data");
    let stub_locks = root.join("stub-locks");
    let session = "cad1349-registered";
    let alias = "positive-lane";
    let go = root.join("registered-go");
    let stop = root.join("registered-stop");
    let result_dir = root.join("registered-results");
    let tmux_wrapper = root.join("registered-tmux.py");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&xdg_state).unwrap();
    fs::create_dir_all(&driver_home).unwrap();
    fs::create_dir_all(&runner_temp).unwrap();
    fs::create_dir_all(&tmp).unwrap();
    fs::create_dir_all(&xdg_data).unwrap();
    fs::create_dir_all(&stub_locks).unwrap();
    fs::create_dir_all(&result_dir).unwrap();
    std::os::unix::fs::symlink(&state, xdg_state.join("cadence")).unwrap();
    fs::create_dir_all(provider).unwrap();
    fs::create_dir_all(markers).unwrap();
    let provider_cli = provider.join("claude");
    fs::write(&provider_cli, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&provider_cli, fs::Permissions::from_mode(0o755)).unwrap();

    let compiler_path = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .filter(|path| !path.ends_with("cadence-hooks/bin"))
        .collect::<Vec<_>>();
    let compiler_path = std::env::join_paths(compiler_path).unwrap();
    let original_home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original_home.join(".cargo"));
    let rustup_home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| original_home.join(".rustup"));
    let runner_path = format!(
        "{}:{}:{}",
        lane.shim.parent().unwrap().display(),
        provider.display(),
        compiler_path.to_string_lossy()
    );
    let nextest_name = "cadence/tools/cadence-nextest-0.9.150/cargo-nextest";
    let source_data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    if let Some(source) = source_data
        .map(|data| data.join(nextest_name))
        .filter(|path| path.is_file())
    {
        let destination = xdg_data.join(nextest_name);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(source, destination).unwrap();
    }

    let driver = root.join("registered-driver.sh");
    let driver_body = format!(
        r##"#!/bin/bash
set -u
exec 9>{lock}
flock -x 9
while [ ! -e {go} ]; do
  if IFS= read -r -t 0.05 _; then :; else sleep 0.01; fi
done
export PATH={path}
export HOME={driver_home} CARGO_HOME={cargo_home} RUSTUP_HOME={rustup_home}
export XDG_DATA_HOME={xdg_data} TMPDIR={tmp} RUNNER_TEMP={runner_temp}
export CARGO_TARGET_DIR={target_dir} CARGO_BUILD_JOBS=2
export CADENCE_BUILD_SLOT={helper} CADENCE_PM_DIR={pm_dir}
export CADENCE_STATE_DIR={state} CADENCE_SUITE_LOCK={suite_lock}
export CAD1349_DRIVER_HOME={driver_home} CAD1349_EXPECT_TARGET={target_dir}
export CAD1349_MARKERS={markers} CAD1349_PROVIDER_DIR={provider}
runner={runner}
manifest={manifest}
run_case() {{
  name=$1
  shift
  if timeout --signal=TERM --kill-after=5s 180s "$@" >{log_dir}/"$name.log" 2>&1; then
    rc=0
  else
    rc=$?
  fi
  printf '%s\n' "$rc" >{log_dir}/"$name.status"
}}
run_case suite "$runner" --manifest-path "$manifest"
python3 - {log_dir}/suite.log {log_dir}/suite.token <<'PYCODE'
import re, sys
text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
match = re.search(r"slot ([^ ]+) acquired \(suite,", text)
if not match:
    raise SystemExit("real suite acquisition token missing from runner output")
open(sys.argv[2], "w").write(match.group(1))
PYCODE
run_case wrong-kind {helper} build-slot run build -- "$runner" --manifest-path "$manifest"
run_case incomplete env CADENCE_BUILD_SLOT_TOKEN=forged-incomplete-token "$runner" --manifest-path "$manifest"
run_case replay bash -c 'export CADENCE_BUILD_SLOT_PID=$$ CADENCE_BUILD_SLOT_LANE="$CADENCE_ALIAS" CADENCE_BUILD_SLOT_TOKEN="$(<"$1")"; exec "$2" --manifest-path "$3"' _ {log_dir}/suite.token "$runner" "$manifest"
while [ ! -e {stop} ]; do sleep 0.05; done
"##,
        lock = shell_quote(&stub_locks.join(format!("{session}.lock"))),
        go = shell_quote(&go),
        path = shell_quote(Path::new(&runner_path)),
        driver_home = shell_quote(&driver_home),
        cargo_home = shell_quote(&cargo_home),
        rustup_home = shell_quote(&rustup_home),
        xdg_data = shell_quote(&xdg_data),
        tmp = shell_quote(&tmp),
        runner_temp = shell_quote(&runner_temp),
        target_dir = shell_quote(&target.join("target")),
        helper = shell_quote(helper),
        pm_dir = shell_quote(&root.join("fixture-pm")),
        state = shell_quote(&state),
        suite_lock = shell_quote(&root.join("registered-suite.lock")),
        markers = shell_quote(markers),
        provider = shell_quote(provider),
        runner = shell_quote(&repo.join("scripts/run-result-tests")),
        manifest = shell_quote(&target.join("Cargo.toml")),
        log_dir = shell_quote(&result_dir),
        stop = shell_quote(&stop),
    );
    fs::write(&driver, driver_body).unwrap();
    fs::set_permissions(&driver, fs::Permissions::from_mode(0o755)).unwrap();
    Pm::init(&root.join("fixture-pm")).expect("isolated pty agent PM");
    let original_tmux = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_tmux.py");
    fs::write(
        &tmux_wrapper,
        format!(
            r#"#!/usr/bin/env python3
import os, sys
env = os.environ.copy()
args = sys.argv[1:]
for i, arg in enumerate(args[:-1]):
    if arg == '-e' and '=' in args[i + 1]:
        key, value = args[i + 1].split('=', 1)
        env[key] = value
os.execvpe(sys.executable, [sys.executable, {}, *args], env)
"#,
            shell_quote(&original_tmux)
        ),
    )
    .unwrap();
    fs::set_permissions(&tmux_wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let mut cleanup = RegisteredFixtureCleanup {
        state: state.clone(),
        home: home.clone(),
        xdg_state: xdg_state.clone(),
        driver_stop: stop.clone(),
        tmux_state: root.join("tmux/state.json"),
        armed: true,
    };
    let started =
        start_registered_fixture_daemon(&state, &home, &xdg_state, root, &driver, &tmux_wrapper);
    assert!(
        started.status.success(),
        "registered fixture daemon start failed: {}{}",
        String::from_utf8_lossy(&started.stdout),
        String::from_utf8_lossy(&started.stderr)
    );
    let registered = fixture_cli(
        &state,
        &home,
        &xdg_state,
        &[
            "agent",
            "register",
            alias,
            "--provider",
            "tui-stub",
            "--endpoint",
            "pty",
            "--cwd",
            root.to_str().unwrap(),
            "--param",
            &format!("session={session}"),
        ],
    );
    assert!(
        registered.status.success(),
        "register temporary pty agent: {}{}",
        String::from_utf8_lossy(&registered.stdout),
        String::from_utf8_lossy(&registered.stderr)
    );
    let sent = fixture_cli(
        &state,
        &home,
        &xdg_state,
        &["send", alias, "--text", "start acceptance runner"],
    );
    assert!(
        sent.status.success(),
        "start temporary pty agent: {}{}",
        String::from_utf8_lossy(&sent.stdout),
        String::from_utf8_lossy(&sent.stderr)
    );
    let unregistered_suite = fixture_cli(
        &state,
        &home,
        &xdg_state,
        &["build-slot", "run", "suite", "--", "true"],
    );
    let unregistered_output = format!(
        "{}{}",
        String::from_utf8_lossy(&unregistered_suite.stdout),
        String::from_utf8_lossy(&unregistered_suite.stderr)
    );
    assert!(
        !unregistered_suite.status.success()
            && unregistered_output.contains("only a build or test slot"),
        "unregistered suite admission was not refused: {unregistered_output}"
    );

    let store = cadence_agent::store::Store::open_side(&state.join("cadence.sqlite3")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let agent = loop {
        let row = store.agent(alias).unwrap();
        let session_state = root.join("tmux/state.json");
        if row.endpoint_kind == "pty" && row.generation.is_some() && session_state.is_file() {
            if let Some(pid) = row.pid {
                let pane: Value =
                    serde_json::from_slice(&fs::read(session_state).unwrap()).unwrap();
                assert_eq!(pid as u32, pane["pid"].as_u64().unwrap() as u32);
                break row;
            }
        }
        assert!(
            Instant::now() < deadline,
            "registered pty pane did not start"
        );
        thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(agent.alias, alias);
    fs::write(&go, "run\n").unwrap();
    for case in ["suite", "wrong-kind", "incomplete", "replay"] {
        wait_for_file(&result_dir.join(format!("{case}.status")), case);
    }
    let read_case = |case: &str| {
        let status = fs::read_to_string(result_dir.join(format!("{case}.status"))).unwrap();
        let log = fs::read_to_string(result_dir.join(format!("{case}.log"))).unwrap_or_default();
        (status.trim().parse::<i32>().unwrap(), log)
    };
    let (suite_rc, suite_log) = read_case("suite");
    let (wrong_rc, wrong_log) = read_case("wrong-kind");
    let (incomplete_rc, incomplete_log) = read_case("incomplete");
    let (replay_rc, replay_log) = read_case("replay");
    cleanup
        .close()
        .unwrap_or_else(|error| panic!("fixture cleanup failed: {error}"));
    assert_eq!(suite_rc, 0, "registered suite runner failed: {suite_log}");
    assert!(
        suite_log.contains("acquired (suite"),
        "suite acquisition missing: {suite_log}"
    );
    assert!(
        suite_log.contains("validated "),
        "JUnit validation missing: {suite_log}"
    );
    assert_eq!(wrong_rc, 1, "wrong-kind slot was not refused: {wrong_log}");
    assert!(
        wrong_log.contains("did not validate the inherited build-slot identity"),
        "wrong-kind refusal missing: {wrong_log}"
    );
    assert_eq!(
        incomplete_rc, 1,
        "incomplete identity was not refused: {incomplete_log}"
    );
    assert!(
        incomplete_log.contains("incomplete or mismatched inherited build-slot identity"),
        "incomplete refusal missing: {incomplete_log}"
    );
    assert_eq!(replay_rc, 1, "replayed token was not refused: {replay_log}");
    assert!(
        replay_log.contains("did not validate the inherited build-slot identity"),
        "replay refusal missing: {replay_log}"
    );
    assert_eq!(
        fs::read_dir(markers).unwrap().count(),
        integration_targets().len() + 2
    );
    (suite_log, wrong_log, incomplete_log, replay_log)
}

fn run_helper_case(
    fixture: &Fixture,
    root: &Path,
    provider: &Path,
    target: &Path,
    markers: &Path,
    helper_env: Option<&Path>,
    prefix: &str,
) -> Output {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let runner_temp = root.join(format!("{prefix}-runner-temp"));
    let tmp = root.join(format!("{prefix}-tmp"));
    fs::create_dir_all(&runner_temp).unwrap();
    fs::create_dir_all(&tmp).unwrap();
    fs::create_dir_all(provider).unwrap();
    let shim_dir = fixture.shim.parent().unwrap();
    let path = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .filter(|part| !part.ends_with("cadence-hooks/bin"))
        .collect::<Vec<_>>();
    let path = std::env::join_paths(std::iter::once(shim_dir.to_path_buf()).chain(path)).unwrap();
    let mut command = Command::new("timeout");
    command
        .args(["--signal=TERM", "--kill-after=5s", "180s"])
        .arg(repo.join("scripts/run-result-tests"))
        .args([
            "--manifest-path",
            target.join("Cargo.toml").to_str().unwrap(),
        ])
        .current_dir(repo)
        .env_clear()
        .env("PATH", path)
        .env("HOME", root.join("missing-home"))
        .env("TMPDIR", &tmp)
        .env("RUNNER_TEMP", &runner_temp)
        .env("CAD1349_PROVIDER_DIR", provider)
        .env("CAD1349_MARKERS", markers)
        .env("CADENCE_STATE_DIR", root.join("owned-unstarted-state"))
        .env(
            "CADENCE_SUITE_LOCK",
            root.join(format!("{prefix}-suite.lock")),
        )
        .env("GIT_CONFIG_NOSYSTEM", "1");
    if let Some(helper) = helper_env {
        command.env("CADENCE_BUILD_SLOT", helper);
    } else {
        command.env_remove("CADENCE_BUILD_SLOT");
    }
    cadence_agent::reaper::output(&mut command).expect("run generated-shim refusal")
}

#[test]
fn cad1349_real_generated_shim_admits_metadata_and_refuses_missing_helper() {
    let root = Builder::new().prefix("c1349-").tempdir_in("/tmp").unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let helper = root.path().join("cadence helper ' ; literal");
    fs::copy(BINARY, &helper).unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    let positive = fixture(root.path(), &helper, "positive-lane");
    let env_file = positive.lane.join(".env");
    assert_generated_slot_env(&env_file, &helper);
    // The supported issue-start reuse path removes a pre-CAD-1349 assignment
    // and leaves exactly one exported helper at its canonical path.
    let mut env_text = fs::read_to_string(&env_file).unwrap();
    env_text.push_str("CADENCE_BUILD_SLOT=/stale/unexported-helper\n");
    fs::write(&env_file, env_text).unwrap();
    // A stale lane shim is refreshed by the supported issue-start reuse path.
    fs::write(&positive.shim, "#!/bin/sh\nexit 77\n").unwrap();
    let mut refresh = Command::new(&helper);
    refresh
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args([
            "issue",
            "start",
            ISSUE,
            "--repo",
            positive.repo.to_str().unwrap(),
            "--name",
            &positive.name,
            "--owner",
            "cad1349-fixture",
            "--by",
            "cad1349-fixture",
        ])
        .current_dir(&positive.repo)
        .env("CADENCE_PM_DIR", &positive.pm.dir)
        .env("HOME", &positive.home)
        .env("XDG_STATE_HOME", root.path().join("positive-lane-state"))
        .env("XDG_CONFIG_HOME", positive.home.join("config"))
        .env("XDG_DATA_HOME", positive.home.join("data"))
        .env("XDG_CACHE_HOME", positive.home.join("cache"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", positive.home.join("gitconfig"))
        .env_remove("CADENCE_STATE_DIR")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    let refreshed = cadence_agent::reaper::output(&mut refresh).expect("refresh stale lane shim");
    assert!(
        refreshed.status.success(),
        "issue-start reuse failed to refresh shim: {}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    assert_embedded_helper(&positive.shim, &helper);
    assert_generated_slot_env(&env_file, &helper);
    let targets = integration_targets();
    let positive_manifest = root.path().join("positive-fixture");
    write_fixture(&positive_manifest, &targets);
    let positive_markers = root.path().join("positive-markers");
    let _registered_results = run_registered_runner_cases(
        &positive,
        root.path(),
        &root.path().join("provider-bin"),
        &positive_manifest,
        &positive_markers,
        &helper,
    );

    let unexported_manifest = root.path().join("unexported-helper-fixture");
    write_fixture(&unexported_manifest, &targets);
    let unexported_markers = root.path().join("unexported-helper-markers");
    fs::create_dir_all(&unexported_markers).unwrap();
    let unexported = run_helper_case(
        &positive,
        root.path(),
        &root.path().join("unexported-helper-provider-bin"),
        &unexported_manifest,
        &unexported_markers,
        None,
        "unexported-helper",
    );
    let unexported_output = format!(
        "{}{}",
        String::from_utf8_lossy(&unexported.stdout),
        String::from_utf8_lossy(&unexported.stderr)
    );
    assert!(
        !unexported.status.success(),
        "runner accepted a generated shim without its helper export: {unexported_output}"
    );
    assert!(
        unexported_output.contains("generated cargo shim")
            && unexported_output.contains("CADENCE_BUILD_SLOT")
            && unexported_output.contains("export"),
        "missing generated-shim helper-export diagnostic: {unexported_output}"
    );
    assert!(
        !unexported_output.lines().any(|line| {
            line.starts_with("validated ")
                || line.starts_with("running ")
                || line.starts_with("test result:")
        }),
        "runner emitted JUnit/test-execution output before refusing: {unexported_output}"
    );
    assert_eq!(
        fs::read_dir(&unexported_markers).unwrap().count(),
        0,
        "tests executed without the generated shim helper export"
    );
    assert!(
        !unexported_manifest
            .join("target/nextest/cadence/junit.xml")
            .exists(),
        "runner produced a JUnit report without the generated shim helper export"
    );

    let missing_helper = root.path().join("cadence-helper-removed");
    fs::copy(BINARY, &missing_helper).unwrap();
    fs::set_permissions(&missing_helper, fs::Permissions::from_mode(0o755)).unwrap();
    let missing = fixture(root.path(), &missing_helper, "missing-helper-lane");
    fs::remove_file(&missing_helper).unwrap();
    let missing_manifest = root.path().join("missing-fixture");
    write_fixture(&missing_manifest, &targets);
    let missing_markers = root.path().join("missing-markers");
    fs::create_dir_all(&missing_markers).unwrap();
    let refusal = run_helper_case(
        &missing,
        root.path(),
        &root.path().join("missing-provider-bin"),
        &missing_manifest,
        &missing_markers,
        Some(&missing_helper),
        "missing",
    );
    assert!(
        !refusal.status.success(),
        "runner accepted a generated shim with a missing admission helper"
    );
    assert!(
        !missing_manifest
            .join("target/nextest/cadence/junit.xml")
            .exists(),
        "runner produced a success report after helper refusal"
    );
}
