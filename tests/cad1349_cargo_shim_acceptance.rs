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
    sync::{Arc, Mutex},
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
    shim: PathBuf,
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
        shim,
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
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
    let out = Command::new(root.join("scripts/result-test-args"))
        .current_dir(root)
        .output()
        .expect("read active integration target selection");
    assert!(out.status.success(), "result-test-args failed");
    let lines: Vec<_> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(!lines.is_empty() && lines.len() % 2 == 0);
    lines
        .chunks_exact(2)
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
        "[package]\nname = \"cadence-agent\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\ntest-seam = []\n\n[lib]\npath = \"src/lib.rs\"\n\n[[bin]]\nname = \"cadence-fixture\"\npath = \"src/main.rs\"\n",
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

fn start_fixture_daemon(state: &Path, home: &Path, xdg_state: &Path) -> Output {
    let mut command = Command::new(BINARY);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args(["--state-dir", state.to_str().unwrap(), "daemon", "start"])
        .env("HOME", home)
        .env("XDG_STATE_HOME", xdg_state)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env_remove("CADENCE_STATE_DIR")
        .env_remove("CADENCE_PM_DIR");
    cadence_agent::reaper::output(&mut command).expect("start isolated fixture daemon")
}

fn run_runner(
    fixture: &Fixture,
    root: &Path,
    provider: &Path,
    target: &Path,
    markers: &Path,
    helper: &Path,
    with_daemon: bool,
) -> (Output, Option<String>) {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let runner_temp = root.join("runner-temp");
    let tmp = root.join("tmp");
    let driver_home = root.join("driver-home");
    fs::create_dir_all(&runner_temp).unwrap();
    fs::create_dir_all(&tmp).unwrap();
    fs::create_dir_all(&driver_home).unwrap();
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
    let path = format!(
        "{}:{}:{}",
        fixture.shim.parent().unwrap().display(),
        provider.display(),
        compiler_path.to_string_lossy()
    );
    let xdg_data = root.join("driver-xdg-data");
    let nextest_name = "cadence/tools/cadence-nextest-0.9.145/cargo-nextest";
    let source_data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    if let Some(source) = source_data
        .map(|data| data.join(nextest_name))
        .filter(|p| p.is_file())
    {
        let destination = xdg_data.join(nextest_name);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(source, destination).unwrap();
    }
    let mut command = Command::new("timeout");
    command
        .args(["--signal=TERM", "--kill-after=5s", "180s"])
        .arg(repo.join("scripts/run-result-tests"))
        .env_clear()
        .args([
            "--manifest-path",
            target.join("Cargo.toml").to_str().unwrap(),
        ])
        .current_dir(repo)
        .env("PATH", &path)
        .env("HOME", &driver_home)
        .env("XDG_DATA_HOME", &xdg_data)
        .env("CARGO_HOME", cargo_home)
        .env("RUSTUP_HOME", rustup_home)
        .env("TMPDIR", &tmp)
        .env("RUNNER_TEMP", &runner_temp)
        .env("CARGO_TARGET_DIR", target.join("target"))
        .env("CARGO_BUILD_JOBS", "2")
        .env("CADENCE_BUILD_SLOT", helper)
        .env("CADENCE_PM_DIR", root.join("production-looking-pm"))
        .env("CADENCE_STATE_DIR", root.join("production-looking-state"))
        .env("CADENCE_ALIAS", "cad1349-fixture-agent")
        .env("CAD1349_DRIVER_HOME", &driver_home)
        .env(
            "CAD1349_EXPECT_TARGET",
            target.join("target").to_str().unwrap(),
        )
        .env("CAD1349_MARKERS", markers)
        .env("CAD1349_PROVIDER_DIR", provider)
        .env_remove("CADENCE_SUITE_LOCK")
        .env("GIT_CONFIG_NOSYSTEM", "1");

    if !with_daemon {
        let out = cadence_agent::reaper::output(&mut command).expect("run missing-helper refusal");
        return (out, None);
    }

    // run-result-tests creates its XDG state directory under a random private
    // scratch directory. Point only that fixture state location at our owned
    // temporary daemon state so the generated absolute helper performs real
    // slot admission without consulting any host daemon.
    let state = root.join("admitted-state");
    fs::create_dir_all(&state).unwrap();
    let daemon_error = Arc::new(Mutex::new(None::<String>));
    let error_out = daemon_error.clone();
    let watch_root = runner_temp.clone();
    let watch_state = state.clone();
    let watch_home = root.join("daemon-home");
    fs::create_dir_all(&watch_home).unwrap();
    let watch_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread_stop = watch_stop.clone();
    let daemon_thread = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !thread_stop.load(std::sync::atomic::Ordering::Relaxed) {
            let Ok(entries) = fs::read_dir(&watch_root) else {
                thread::sleep(Duration::from_millis(2));
                continue;
            };
            for entry in entries.flatten() {
                let xdg = entry.path().join("xdg-state");
                if !xdg.is_dir() {
                    continue;
                }
                let state_link = xdg.join("cadence");
                if !state_link.exists() {
                    if let Err(error) = std::os::unix::fs::symlink(&watch_state, &state_link) {
                        *error_out.lock().unwrap() =
                            Some(format!("create isolated state link: {error}"));
                        return;
                    }
                }
                if !watch_state.join("cadence.sock").exists() {
                    let output = start_fixture_daemon(&watch_state, &watch_home, &xdg);
                    if !output.status.success() {
                        *error_out.lock().unwrap() = Some(format!(
                            "isolated daemon start failed: {}{}",
                            String::from_utf8_lossy(&output.stdout),
                            String::from_utf8_lossy(&output.stderr)
                        ));
                        return;
                    }
                }
                return;
            }
            thread::sleep(Duration::from_millis(2));
        }
        *error_out.lock().unwrap() = Some("runner scratch state was not observed".to_string());
    });

    let output = cadence_agent::reaper::output(&mut command).expect("run real result-test runner");
    watch_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    daemon_thread.join().expect("join isolated daemon starter");
    let start_error = daemon_error.lock().unwrap().clone();
    if state.join("cadence.sock").exists() {
        let mut stop = Command::new(BINARY);
        stop.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .args(["--state-dir", state.to_str().unwrap(), "daemon", "stop"])
            .env("HOME", root.join("daemon-home"))
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_PM_DIR");
        let stopped = cadence_agent::reaper::output(&mut stop).expect("stop owned fixture daemon");
        assert!(
            stopped.status.success(),
            "isolated daemon stop failed: {}",
            String::from_utf8_lossy(&stopped.stderr)
        );
    }
    (output, start_error)
}

#[test]
fn cad1349_real_generated_shim_admits_metadata_and_refuses_missing_helper() {
    let root = Builder::new().prefix("c1349-").tempdir_in("/tmp").unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let positive = fixture(root.path(), Path::new(BINARY), "positive-lane");
    // A stale lane shim is refreshed by the supported issue-start reuse path.
    fs::write(&positive.shim, "#!/bin/sh\nexit 77\n").unwrap();
    let mut refresh = Command::new(BINARY);
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
    assert_embedded_helper(&positive.shim, Path::new(BINARY));
    let targets = integration_targets();
    let positive_manifest = root.path().join("positive-fixture");
    write_fixture(&positive_manifest, &targets);
    let positive_markers = root.path().join("positive-markers");
    let (success, daemon_error) = run_runner(
        &positive,
        root.path(),
        &root.path().join("provider-bin"),
        &positive_manifest,
        &positive_markers,
        Path::new(BINARY),
        true,
    );
    assert!(
        daemon_error.is_none(),
        "isolated slot daemon setup failed: {}; runner output:\n{}\n{}",
        daemon_error.unwrap_or_default(),
        String::from_utf8_lossy(&success.stdout),
        String::from_utf8_lossy(&success.stderr),
    );
    assert!(
        success.status.success(),
        "supported runner recipe failed through the generated shim:\n{}\n{}",
        String::from_utf8_lossy(&success.stdout),
        String::from_utf8_lossy(&success.stderr)
    );
    let transcript = format!(
        "{}\n{}",
        String::from_utf8_lossy(&success.stdout),
        String::from_utf8_lossy(&success.stderr)
    );
    assert!(
        transcript.contains("acquired (build"),
        "Cargo metadata did not reach real build-slot admission:\n{transcript}"
    );
    assert!(
        transcript.contains("validated "),
        "runner did not validate the pinned JUnit coverage:\n{transcript}"
    );
    let observed: Vec<_> = fs::read_dir(&positive_markers).unwrap().flatten().collect();
    assert_eq!(
        observed.len(),
        targets.len() + 2,
        "not all fixture tests executed through the runner"
    );

    let missing_helper = root.path().join("cadence-helper-removed");
    fs::copy(BINARY, &missing_helper).unwrap();
    fs::set_permissions(&missing_helper, fs::Permissions::from_mode(0o755)).unwrap();
    let missing = fixture(root.path(), &missing_helper, "missing-helper-lane");
    fs::remove_file(&missing_helper).unwrap();
    let missing_manifest = root.path().join("missing-fixture");
    write_fixture(&missing_manifest, &targets);
    let missing_markers = root.path().join("missing-markers");
    let (refusal, _) = run_runner(
        &missing,
        root.path(),
        &root.path().join("missing-provider-bin"),
        &missing_manifest,
        &missing_markers,
        &missing_helper,
        false,
    );
    assert!(
        !refusal.status.success(),
        "runner accepted a generated shim with a missing admission helper"
    );
    assert!(
        fs::read_dir(&missing_markers).unwrap().next().is_none(),
        "tests ran after helper refusal"
    );
    assert!(
        !missing_manifest
            .join("target/nextest/cadence/junit.xml")
            .exists(),
        "runner produced a success report after helper refusal"
    );
}
