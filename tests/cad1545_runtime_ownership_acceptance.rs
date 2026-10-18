//! CAD-1545 independent acceptance for local `dev` ownership and hosted
//! lifecycle config. Drives the real `cadence` binary and
//! `lease::read_hosted`. The implementer does not own this file.
//!
//! Saved remote selection must not retarget `cadence dev ls` or rewrite the
//! org registry. An explicit remote `--org` or `CADENCE_ORG` refuses before
//! credentials or transport. Local state/tracker pins and the managed-caller
//! selector refusal stay in force.
//!
//! `read_hosted` fails closed on a non-NotFound read, bad YAML, and a hosted
//! table that is not the hosted record, including `hosted: null` and a bare
//! `hosted:`. A missing file or table is off.
//! A foreground `daemon run` must surface that unusable config itself.
//!
//! `dev up --build` talks only to an owned `cadence-fixture` child. `issue
//! init` may exit 0 with no output. `daemon start` must report one JSON
//! object whose `state` is a nonempty string; anything else refuses and
//! must not reach `ui start`, including a root array such as `["started"]`.
//! A state object with extra fields may pass, including one space, tab,
//! CR, and LF before `{`.
//! A child stderr frame whose `kind` is already `ui_bind_in_use` still
//! follows the parent's existing retry classification. That does not
//! prove own-child provenance. The fixture is not a service and not a
//! remote API.
//!
//! The operator's saved org may be a remote workspace. These fixtures never
//! switch it and never send a tracker command at that selection. Every
//! spawned cadence and the owned fixture clear `CADENCE_ORG` themselves;
//! a case that needs the variable sets it on that command alone. A
//! foreground daemon whose `pm.yaml` is a directory must exit before it
//! binds its Unix socket. If it stays up, the test SIGTERMs that process
//! group, then SIGKILLs it, and the temp root is removed either way.

use std::fs::{self, File};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::Builder;

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
const REMOTE_ORG: &str = "hosted";
const REMOTE_ENDPOINT: &str = "https://example.invalid";
const REMOTE_ORG_ID: &str = "ws_cad1545";

struct Iso {
    root: tempfile::TempDir,
    registry: PathBuf,
    registry_bytes: Vec<u8>,
    sentinel: PathBuf,
    sentinel_log: PathBuf,
}

impl Iso {
    fn new() -> Self {
        let root = Builder::new().prefix("c1545-").tempdir_in("/tmp").unwrap();
        for name in [
            "home",
            "config",
            "data",
            "state",
            "tmp",
            "boxes",
            "locks",
            "bin",
            "local-state",
            "local-pm",
            "pin-state",
            "pin-pm",
        ] {
            fs::create_dir_all(root.path().join(name)).unwrap();
        }
        let sentinel = root.path().join("bin");
        let sentinel_log = root.path().join("external.log");
        for name in ["gh", "curl", "wget", "ssh", "nc", "false"] {
            let path = sentinel.join(name);
            fs::write(
                &path,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"{bin} $*\" >> \"{log}\"\nexit 97\n",
                    bin = name,
                    log = sentinel_log.display()
                ),
            )
            .unwrap();
            let mut perms = fs::metadata(&path).unwrap().permissions();
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms).unwrap();
        }
        let registry_dir = root.path().join("config/cadence");
        fs::create_dir_all(&registry_dir).unwrap();
        let registry = registry_dir.join("orgs.json");
        let body = serde_json::to_vec_pretty(&json!({
            "selected": {"org": REMOTE_ORG},
            "connections": [
                {
                    "selection": {"org": REMOTE_ORG},
                    "destination": {
                        "mode": "remote",
                        "endpoint": REMOTE_ENDPOINT,
                        "org_id": REMOTE_ORG_ID
                    }
                },
                {
                    "selection": {"org": "local"},
                    "destination": {
                        "mode": "local",
                        "state_dir": root.path().join("local-state"),
                        "tracker_dir": root.path().join("local-pm")
                    }
                }
            ]
        }))
        .unwrap();
        fs::write(&registry, &body).unwrap();
        Self {
            root,
            registry,
            registry_bytes: body,
            sentinel,
            sentinel_log,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BINARY);
        cmd.env_clear();
        isolate_org(&mut cmd);
        let path = format!("{}:/usr/bin:/bin", self.sentinel.display());
        cmd.env("PATH", path)
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("TMPDIR", self.path("tmp"))
            .env("CADENCE_SANDBOX_ROOT", self.path("boxes"))
            .env("CADENCE_TEST_PORT_LOCK_DIR", self.path("locks"));
        for (name, value) in [("CARGO_HOME", "CARGO_HOME"), ("RUSTUP_HOME", "RUSTUP_HOME")] {
            if let Some(value) = std::env::var_os(value) {
                cmd.env(name, value);
            }
        }
        for name in cadence_agent::adapter::PROVIDER_COMMAND_VARS {
            cmd.env(name, self.sentinel.join("false"));
        }
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = self.command(args);
        for (key, value) in env {
            cmd.env(key, value);
        }
        // The spawned CLI is its own process group. A timeout signals only
        // that pid's group, never a process found by scanning /proc.
        cmd.process_group(0);
        let child = cadence_agent::reaper::spawn(&mut cmd).expect("spawn cadence");
        let pid = child.id();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait_with_output());
        });
        match rx.recv_timeout(Duration::from_secs(45)) {
            Ok(output) => output.expect("wait cadence"),
            Err(_) => {
                graceful_end(pid);
                let _ = rx.recv_timeout(Duration::from_secs(3));
                panic!(
                    "cadence did not finish within 45s; SIGTERM then SIGKILL of spawned group {pid}"
                );
            }
        }
    }

    fn registry_untouched(&self) {
        let now = fs::read(&self.registry).unwrap();
        assert_eq!(
            now,
            self.registry_bytes,
            "org registry bytes changed\nbefore: {}\nafter: {}",
            String::from_utf8_lossy(&self.registry_bytes),
            String::from_utf8_lossy(&now)
        );
        let cadence_config = self.path("config/cadence");
        let mut extra = Vec::new();
        for entry in fs::read_dir(&cadence_config).unwrap().flatten() {
            if entry.file_name() != "orgs.json" {
                extra.push(entry.file_name());
            }
        }
        assert!(extra.is_empty(), "org config gained files: {extra:?}");
        assert!(
            !self.sentinel_log.exists(),
            "external command ran: {}",
            fs::read_to_string(&self.sentinel_log).unwrap_or_default()
        );
        assert!(
            !self.path("config/cadence/remote-auth").exists(),
            "remote credential directory was created"
        );
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn assert_local_ls(out: &Output, what: &str) {
    let all = text(out);
    assert!(
        out.status.success(),
        "{what}: `cadence dev ls` should succeed locally, got {:?}\n{all}",
        out.status.code()
    );
    assert!(
        !all.contains(REMOTE_ENDPOINT) && !all.contains("remote="),
        "{what}: local dev ls mentioned the saved remote endpoint\n{all}"
    );
    let body: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{what}: stdout is not JSON ({e})\n{all}"));
    assert!(
        body.get("sandboxes").and_then(Value::as_array).is_some(),
        "{what}: expected a sandbox list\n{all}"
    );
}

fn assert_rejected(out: &Output, what: &str, needles: &[&str]) {
    let all = text(out);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{what}: expected rejection exit 3, got {:?}\n{all}",
        out.status.code()
    );
    assert!(
        all.contains("\"kind\": \"rejected\"") || all.contains("\"kind\":\"rejected\""),
        "{what}: refusal is not a rejected error\n{all}"
    );
    for needle in needles {
        assert!(
            all.contains(needle),
            "{what}: refusal missing {needle}\n{all}"
        );
    }
    let lower = all.to_lowercase();
    for leaked in [
        "credential",
        "timed out",
        "connection refused",
        "name or service not known",
        "temporary failure in name resolution",
        "wake",
        REMOTE_ENDPOINT,
    ] {
        assert!(
            !lower.contains(leaked),
            "{what}: refusal reached credentials or transport ({leaked})\n{all}"
        );
    }
}

#[test]
fn saved_remote_org_keeps_dev_local_and_refuses_explicit_remote_selectors() {
    let iso = Iso::new();
    let listed = iso.run(&["dev", "ls"], &[]);
    assert_local_ls(&listed, "saved remote default");
    iso.registry_untouched();

    let by_flag = iso.run(&["--org", REMOTE_ORG, "dev", "ls"], &[]);
    assert_rejected(
        &by_flag,
        "--org remote",
        &["cadence dev", REMOTE_ORG, "remote"],
    );
    iso.registry_untouched();

    let by_env = iso.run(&["dev", "ls"], &[("CADENCE_ORG", REMOTE_ORG)]);
    assert_rejected(
        &by_env,
        "CADENCE_ORG remote",
        &["cadence dev", REMOTE_ORG, "remote"],
    );
    iso.registry_untouched();

    let local_flag = iso.run(&["--org", "local", "dev", "ls"], &[]);
    assert_local_ls(&local_flag, "--org local");
    iso.registry_untouched();

    let local_env = iso.run(&["dev", "ls"], &[("CADENCE_ORG", "local")]);
    assert_local_ls(&local_env, "CADENCE_ORG=local");
    iso.registry_untouched();

    let pin_state = iso.path("pin-state");
    let pin_pm = iso.path("pin-pm");
    let pinned = iso.run(
        &["--state-dir", pin_state.to_str().unwrap(), "dev", "ls"],
        &[("CADENCE_PM_DIR", pin_pm.to_str().unwrap())],
    );
    assert_local_ls(&pinned, "state and tracker pins");
    iso.registry_untouched();

    let pin_and_org = iso.run(
        &[
            "--state-dir",
            pin_state.to_str().unwrap(),
            "--org",
            REMOTE_ORG,
            "dev",
            "ls",
        ],
        &[("CADENCE_PM_DIR", pin_pm.to_str().unwrap())],
    );
    assert_rejected(
        &pin_and_org,
        "--org beside local pins",
        &["--org", "conflicts"],
    );
    iso.registry_untouched();

    let pin_and_env = iso.run(
        &["--state-dir", pin_state.to_str().unwrap(), "dev", "ls"],
        &[
            ("CADENCE_PM_DIR", pin_pm.to_str().unwrap()),
            ("CADENCE_ORG", REMOTE_ORG),
        ],
    );
    assert_rejected(
        &pin_and_env,
        "CADENCE_ORG beside local pins",
        &["cadence dev", REMOTE_ORG, "remote"],
    );
    iso.registry_untouched();

    let managed = iso.run(&["dev", "ls"], &[("CADENCE_ALIAS", "worker-1")]);
    assert_local_ls(&managed, "managed caller without a selector");
    iso.registry_untouched();

    let managed_org = iso.run(
        &["--org", REMOTE_ORG, "dev", "ls"],
        &[
            ("CADENCE_ALIAS", "worker-1"),
            ("CADENCE_STATE_DIR", pin_state.to_str().unwrap()),
            ("CADENCE_PM_DIR", pin_pm.to_str().unwrap()),
        ],
    );
    assert_rejected(
        &managed_org,
        "managed --org",
        &["managed callers cannot override"],
    );
    iso.registry_untouched();

    let managed_env = iso.run(
        &["dev", "ls"],
        &[("CADENCE_ALIAS", "worker-1"), ("CADENCE_ORG", REMOTE_ORG)],
    );
    assert_rejected(
        &managed_env,
        "managed CADENCE_ORG",
        &["cadence dev", REMOTE_ORG, "remote"],
    );
    iso.registry_untouched();
}

#[test]
fn read_hosted_rejects_unusable_config_and_absent_table_is_off() {
    let root = Builder::new().prefix("c1545h-").tempdir_in("/tmp").unwrap();
    let pm = root.path().join("pm");
    fs::create_dir_all(&pm).unwrap();

    assert!(cadence_agent::lease::read_hosted(&pm)
        .expect("missing pm.yaml")
        .is_none());
    assert!(
        cadence_agent::lease::read_hosted(&root.path().join("missing-dir"))
            .expect("missing tracker dir")
            .is_none()
    );

    fs::write(pm.join("pm.yaml"), "host:\n  auto_stop_idle_secs: 1\n").unwrap();
    assert!(cadence_agent::lease::read_hosted(&pm)
        .expect("yaml without hosted")
        .is_none());

    fs::write(
        pm.join("pm.yaml"),
        "other: true\nhosted:\n  lease: off\n  lease_ttl_secs: 9\n",
    )
    .unwrap();
    let hosted = cadence_agent::lease::read_hosted(&pm)
        .expect("valid hosted table")
        .expect("present table");
    assert_eq!(hosted.lease.as_deref(), Some("off"));
    assert_eq!(hosted.lease_ttl_secs, Some(9));
    assert_eq!(hosted.lease_renew_secs, None);
    assert_eq!(hosted.flush_timeout_secs, None);

    let cases: [(&str, Option<&str>, &[&str]); 9] = [
        ("directory", None, &["pm.yaml", "cannot read", "refusing"]),
        ("malformed", Some("hosted: [\n"), &["pm.yaml", "YAML"]),
        ("scalar", Some("hosted: off\n"), &["pm.yaml", "hosted"]),
        ("list", Some("hosted:\n  - lease\n"), &["pm.yaml", "hosted"]),
        (
            "bad-type",
            Some("hosted:\n  lease: 4\n"),
            &["pm.yaml", "hosted", "lease"],
        ),
        (
            "unknown",
            Some("hosted:\n  lease: off\n  nope: 1\n"),
            &["pm.yaml", "hosted", "nope"],
        ),
        (
            "blank-key",
            Some("hosted:\n  lease_ttl_secs: later\n"),
            &["pm.yaml", "hosted", "lease_ttl_secs"],
        ),
        ("null", Some("hosted: null\n"), &["pm.yaml", "hosted"]),
        ("empty", Some("hosted:\n"), &["pm.yaml", "hosted"]),
    ];
    for (name, body, needles) in cases {
        let _ = fs::remove_dir_all(pm.join("pm.yaml"));
        let _ = fs::remove_file(pm.join("pm.yaml"));
        if name == "directory" {
            fs::create_dir(pm.join("pm.yaml")).unwrap();
        } else {
            fs::write(pm.join("pm.yaml"), body.unwrap()).unwrap();
        }
        let err = cadence_agent::lease::read_hosted(&pm)
            .err()
            .unwrap_or_else(|| panic!("{name}: unusable config returned Ok"));
        let msg = err.to_string();
        for needle in needles {
            assert!(msg.contains(needle), "{name}: {msg}");
        }
    }
}

#[test]
fn foreground_daemon_refuses_unreadable_hosted_config() {
    let root = Builder::new().prefix("c1545d-").tempdir_in("/tmp").unwrap();
    for name in ["home", "config", "data", "xdg", "tmp", "state", "pm", "bin"] {
        fs::create_dir_all(root.path().join(name)).unwrap();
    }
    let pm = root.path().join("pm");
    fs::create_dir(pm.join("pm.yaml")).unwrap();
    let provider_log = root.path().join("provider.log");
    let provider = root.path().join("bin/provider");
    fs::write(
        &provider,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\nexit 97\n",
            provider_log.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&provider).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&provider, perms).unwrap();

    let stdout_path = root.path().join("stdout");
    let stderr_path = root.path().join("stderr");
    let socket = root.path().join("state/cadence.sock");
    let mut cmd = Command::new(BINARY);
    cmd.env_clear();
    isolate_org(&mut cmd);
    cmd.env(
        "PATH",
        format!("{}:/usr/bin:/bin", root.path().join("bin").display()),
    )
    .env("HOME", root.path().join("home"))
    .env("XDG_CONFIG_HOME", root.path().join("config"))
    .env("XDG_DATA_HOME", root.path().join("data"))
    .env("XDG_STATE_HOME", root.path().join("xdg"))
    .env("TMPDIR", root.path().join("tmp"))
    .env("CADENCE_PM_DIR", &pm)
    .args([
        "--state-dir",
        root.path().join("state").to_str().unwrap(),
        "daemon",
        "run",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::from(File::create(&stdout_path).unwrap()))
    .stderr(Stdio::from(File::create(&stderr_path).unwrap()))
    .process_group(0);
    for name in cadence_agent::adapter::PROVIDER_COMMAND_VARS {
        cmd.env(name, &provider);
    }
    let mut child = cadence_agent::reaper::spawn(&mut cmd).expect("spawn daemon");
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait daemon") {
            break status;
        }
        if socket.exists() || Instant::now() >= deadline {
            let opened = socket.exists();
            graceful_end(pid);
            let _ = child.wait();
            let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
            panic!(
                "foreground daemon stayed up with pm.yaml as a directory \
                 (socket opened: {opened}); SIGTERM then SIGKILL of {pid}\n{stderr}"
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
    let stdout = fs::read_to_string(&stdout_path).unwrap_or_default();
    assert_eq!(
        status.code(),
        Some(3),
        "daemon run should reject unreadable lifecycle config, got {status:?}\n{stderr}\n{stdout}"
    );
    let lower = stderr.to_lowercase();
    assert!(
        !lower.contains("operator identity"),
        "operator-identity refusal is not proof of the hosted-config guard\n{stderr}"
    );
    assert!(
        stderr.contains("pm.yaml") && stderr.contains("cannot read") && stderr.contains("refusing"),
        "daemon refusal did not name the unreadable lifecycle config\n{stderr}"
    );
    assert!(
        stderr.contains("\"kind\": \"rejected\"") || stderr.contains("\"kind\":\"rejected\""),
        "daemon refusal kind\n{stderr}"
    );
    let provider_hit = fs::read_to_string(&provider_log).unwrap_or_default();
    assert!(
        provider_hit.is_empty(),
        "daemon launched a provider: {provider_hit}"
    );
    assert!(
        procs_naming(&root.path().join("state")).is_empty(),
        "a process still names the state dir; this scan is not used to signal anyone"
    );
    assert!(
        !socket.exists(),
        "unreadable hosted config still created {}",
        socket.display()
    );
}

/// Drop `CADENCE_ORG` (and a token that would otherwise select a remote
/// workspace) on this command. Callers that need a value set it afterwards.
fn isolate_org(cmd: &mut Command) {
    cmd.env_remove("CADENCE_ORG");
    cmd.env_remove("CADENCE_TOKEN");
}

fn graceful_end(pid: u32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    unsafe {
        kill(-pid, 15);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if unsafe { kill(pid, 0) } != 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    unsafe {
        kill(-pid, 9);
    }
}

/// Assertion aid only. A path match is not ownership and is never signaled.
fn procs_naming(needle: &Path) -> Vec<u32> {
    let needle = needle.to_string_lossy().into_owned();
    let me = std::process::id();
    let mut pids = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return pids;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == me {
            continue;
        }
        let hit = ["cmdline", "environ"].iter().any(|name| {
            fs::read(entry.path().join(name))
                .map(|bytes| String::from_utf8_lossy(&bytes).contains(&needle))
                .unwrap_or(false)
        });
        if hit {
            pids.push(pid);
        }
    }
    pids
}

#[test]
fn dev_up_fixture_requires_startup_state_and_keeps_bind_retries() {
    let iso = Iso::new();
    let fixture = iso.path("bin/cadence-fixture");
    fs::write(&fixture, FIXTURE).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&fixture).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&fixture, perms).unwrap();
    let log = iso.path("fixture.log");
    let ui_count = iso.path("ui.count");

    let bad = [
        ("malformed", "jmal", "invalid startup state"),
        ("missing", "jmiss", "invalid startup state"),
        ("nonstring", "jtype", "invalid startup state"),
        ("blank", "jblank", "empty startup state"),
        ("array", "jarray", "invalid startup state"),
    ];
    for (mode, name, needle) in bad {
        let _ = fs::remove_file(&log);
        let _ = fs::remove_file(&ui_count);
        let out = iso.run(
            &["dev", "up", name, "--build", fixture.to_str().unwrap()],
            &[
                ("CADENCE_FIXTURE_LOG", log.to_str().unwrap()),
                ("CADENCE_FIXTURE_MODE", mode),
                ("CADENCE_FIXTURE_UI_COUNT", ui_count.to_str().unwrap()),
            ],
        );
        let all = text(&out);
        assert_eq!(
            out.status.code(),
            Some(3),
            "{mode}: expected refusal, got {:?}\n{all}",
            out.status.code()
        );
        assert!(
            all.contains(needle),
            "{mode}: refusal missing {needle}\n{all}"
        );
        let invocations = fixture_verbs(&log);
        assert!(
            invocations.iter().any(|v| v == "init"),
            "{mode}: issue init was not run: {invocations:?}\n{all}"
        );
        assert!(
            invocations.iter().any(|v| v == "daemon"),
            "{mode}: daemon start was not run: {invocations:?}\n{all}"
        );
        assert!(
            !invocations.iter().any(|v| v == "ui"),
            "{mode}: ui start ran after a bad daemon state: {invocations:?}\n{all}"
        );
        assert!(!ui_count.exists(), "{mode}: ui counter exists");
        assert!(
            !all.to_lowercase().contains("operator identity"),
            "{mode}: operator-identity refusal is not the startup-state proof\n{all}"
        );
        assert!(
            procs_naming(&iso.path("boxes")).is_empty(),
            "{mode}: fixture left a process naming the sandbox; it starts no service"
        );
    }

    let _ = fs::remove_file(&log);
    let _ = fs::remove_file(&ui_count);
    let out = iso.run(
        &["dev", "up", "jretry", "--build", fixture.to_str().unwrap()],
        &[
            ("CADENCE_FIXTURE_LOG", log.to_str().unwrap()),
            ("CADENCE_FIXTURE_MODE", "retry"),
            ("CADENCE_FIXTURE_UI_COUNT", ui_count.to_str().unwrap()),
        ],
    );
    let all = text(&out);
    assert!(
        out.status.success(),
        "extra fields plus the parent's existing ui_bind_in_use classification should still come up; this is not own-child provenance\n{all}"
    );
    let invocations = fixture_verbs(&log);
    assert_eq!(
        invocations.iter().filter(|v| v.as_str() == "init").count(),
        1,
        "init count {invocations:?}"
    );
    assert_eq!(
        invocations
            .iter()
            .filter(|v| v.as_str() == "daemon")
            .count(),
        1,
        "daemon start must not be replayed when the parent retries on its existing classification {invocations:?}"
    );
    assert_eq!(
        invocations.iter().filter(|v| v.as_str() == "ui").count(),
        2,
        "parent's existing ui_bind_in_use classification should retry ui start once; not own-child provenance {invocations:?}\n{all}"
    );
    assert_eq!(fs::read_to_string(&ui_count).unwrap().trim(), "2");
    let body: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("up stdout is not JSON ({e})\n{all}"));
    assert_eq!(body["daemon"], "started");
    assert_eq!(body["ui"], "listening");
    assert!(
        !iso.sentinel_log.exists(),
        "fixture path called an external command: {}",
        fs::read_to_string(&iso.sentinel_log).unwrap_or_default()
    );
    assert!(
        procs_naming(&iso.path("boxes")).is_empty(),
        "fixture left a process naming the sandbox; classification retry starts no service"
    );
}

fn fixture_verbs(log: &Path) -> Vec<String> {
    let text = fs::read_to_string(log).unwrap_or_default();
    let mut verbs = Vec::new();
    let mut args: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(arg) = line.strip_prefix("ARG ") {
            args.push(arg.to_string());
        } else if line == "END" {
            verbs.push(verb_of(&args));
            args.clear();
        }
    }
    verbs
}

fn verb_of(args: &[String]) -> String {
    args.windows(2)
        .find_map(|pair| match (pair[0].as_str(), pair[1].as_str()) {
            ("issue", "init") => Some("init".to_string()),
            ("daemon", "start") => Some("daemon".to_string()),
            ("ui", "start") => Some("ui".to_string()),
            _ => None,
        })
        .unwrap_or_else(|| format!("other:{args:?}"))
}

const FIXTURE: &str = r#"#!/bin/sh
# Child protocol only. Never execs cadence, a provider, or a network client.
unset CADENCE_ORG CADENCE_TOKEN
log=${CADENCE_FIXTURE_LOG:?}
mode=${CADENCE_FIXTURE_MODE:?}
printf 'BEGIN\n' >> "$log"
for arg in "$@"; do
    printf 'ARG %s\n' "$arg" >> "$log"
done
printf 'END\n' >> "$log"
prev=
verb=
for arg in "$@"; do
    if [ "$prev" = issue ] && [ "$arg" = init ]; then verb=init; fi
    if [ "$prev" = daemon ] && [ "$arg" = start ]; then verb=daemon; fi
    if [ "$prev" = ui ] && [ "$arg" = start ]; then verb=ui; fi
    prev=$arg
done
case "$verb" in
init)
    exit 0
    ;;
daemon)
    case "$mode" in
    malformed) printf '%s' '{'; exit 0 ;;
    missing) printf '%s' '{"ready":true}'; exit 0 ;;
    nonstring) printf '%s' '{"state":false,"extra":1}'; exit 0 ;;
    blank) printf '%s' '{"state":"  "}'; exit 0 ;;
    array) printf '%s' '["started"]'; exit 0 ;;
    # Leading space/tab/CR/LF is valid JSON whitespace before the object.
    retry) printf ' \t\r\n%s' '{"state":"started","note":"kept","pid":0}'; exit 0 ;;
    *) printf '%s' '{"error":"unknown fixture mode"}' >&2; exit 97 ;;
    esac
    ;;
ui)
    count=${CADENCE_FIXTURE_UI_COUNT:?}
    n=0
    if [ -f "$count" ]; then n=$(cat "$count"); fi
    n=$((n + 1))
    printf '%s' "$n" > "$count"
    if [ "$mode" = retry ] && [ "$n" = 1 ]; then
        # Parent already classifies this kind as a retry. Not own-child provenance.
        printf '%s' '{"kind":"ui_bind_in_use","error":"bind"}' >&2
        exit 1
    fi
    printf '%s' '{"state":"listening","note":"kept"}'
    exit 0
    ;;
*)
    printf '%s' '{"error":"unexpected fixture command"}' >&2
    exit 97
    ;;
esac
"#;
