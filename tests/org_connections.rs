//! CAD-657: destination isolation is observable at the actual Unix RPC peer.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Output};

fn cli_command(root: &std::path::Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cadence"));
    command
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("legacy"))
        .env_remove("CADENCE_HOME")
        .env_remove("CADENCE_STATE_DIR")
        .env_remove("CADENCE_PM_DIR")
        .env_remove("CADENCE_PROFILE")
        .env_remove("CADENCE_ALIAS")
        .args(args);
    command
}
fn cli(root: &std::path::Path, args: &[&str]) -> Output {
    cadence_agent::reaper::output(&mut cli_command(root, args)).unwrap()
}
fn ok(root: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let out = cli(root, args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn add(root: &std::path::Path, name: &str) {
    let state = root.join(name).join("state");
    let tracker = root.join(name).join("tracker");
    ok(
        root,
        &[
            "org",
            "add",
            name,
            "--local-state-dir",
            state.to_str().unwrap(),
            "--tracker-dir",
            tracker.to_str().unwrap(),
        ],
    );
}
fn peer(root: &std::path::Path, name: &str) -> std::thread::JoinHandle<()> {
    peer_at(&root.join(name).join("state"), name)
}
fn peer_at(state: &std::path::Path, destination: &str) -> std::thread::JoinHandle<()> {
    std::fs::create_dir_all(state).unwrap();
    let socket = UnixListener::bind(state.join("cadence.sock")).unwrap();
    let destination = destination.to_owned();
    std::thread::spawn(move || {
        let (mut stream, _) = socket.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], "agent_list");
        writeln!(
            stream,
            "{}",
            serde_json::json!({"ok": true, "result": {"agents": [], "destination": destination}})
        )
        .unwrap();
    })
}
#[test]
fn local_selection_and_explicit_override_reach_only_selected_peer() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    add(root.path(), "first");
    add(root.path(), "second");
    ok(root.path(), &["org", "switch", "first"]);
    let first = peer(root.path(), "first");
    assert_eq!(
        ok(root.path(), &["agent", "list", "--json"])["destination"],
        "first"
    );
    first.join().unwrap();
    let second = peer(root.path(), "second");
    assert_eq!(
        ok(root.path(), &["--org", "second", "agent", "list", "--json"])["destination"],
        "second"
    );
    second.join().unwrap();
    assert_eq!(ok(root.path(), &["org", "inspect"])["org"], "first");
}
#[test]
fn remote_selection_and_missing_local_peer_never_fall_back() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    add(root.path(), "local");
    ok(
        root.path(),
        &[
            "org",
            "add",
            "acme",
            "--connection",
            "cloud",
            "--endpoint",
            "https://example.com",
            "--org-id",
            "company-acme",
        ],
    );
    ok(
        root.path(),
        &["org", "switch", "acme", "--connection", "cloud"],
    );
    let out = cli(root.path(), &["agent", "list", "--json"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("remote transport is not implemented"));
    let out = cli(root.path(), &["--org", "local", "agent", "list", "--json"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("/local/state/cadence.sock"));
}
#[test]
fn managed_state_pin_survives_default_switch_and_reconnect() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    add(root.path(), "original");
    add(root.path(), "new");
    ok(root.path(), &["org", "switch", "new"]);
    let original = peer(root.path(), "original");
    let out = cadence_agent::reaper::output(
        Command::new(env!("CARGO_BIN_EXE_cadence"))
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("CADENCE_STATE_DIR", root.path().join("original/state"))
            .env("CADENCE_PM_DIR", root.path().join("original/tracker"))
            .env_remove("CADENCE_HOME")
            .args(["agent", "list", "--json"]),
    )
    .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["destination"],
        "original"
    );
    original.join().unwrap();
}

#[test]
fn agent_and_detached_child_cannot_change_defaults_even_with_forged_state() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    add(root.path(), "first");
    ok(root.path(), &["org", "switch", "first"]);
    for detached in [false, true] {
        let mut command = if detached {
            let mut c = Command::new("setsid");
            c.arg(env!("CARGO_BIN_EXE_cadence"));
            c
        } else {
            Command::new(env!("CARGO_BIN_EXE_cadence"))
        };
        let out = cadence_agent::reaper::output(
            command
                .env("HOME", root.path())
                .env("XDG_CONFIG_HOME", root.path().join("config"))
                .env("CADENCE_ALIAS", "worker")
                .env_remove("CADENCE_HOME")
                .env_remove("CADENCE_PM_DIR")
                .args([
                    "--state-dir",
                    root.path().join("forged").to_str().unwrap(),
                    "org",
                    "switch",
                    "first",
                ]),
        )
        .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("operator action"));
    }
}

#[test]
fn concurrent_adds_preserve_both_connections() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    std::thread::scope(|scope| {
        scope.spawn(|| add(root.path(), "first"));
        scope.spawn(|| add(root.path(), "second"));
    });
    assert_eq!(
        ok(root.path(), &["org", "list"])["connections"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn selected_tracker_is_isolated_and_switch_does_not_move_inflight_rpc() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    add(root.path(), "first");
    add(root.path(), "second");
    ok(root.path(), &["org", "switch", "first"]);
    assert_eq!(
        ok(root.path(), &["issue", "init"])["pm_dir"],
        root.path().join("first/tracker").to_str().unwrap()
    );
    assert!(!root.path().join("second/tracker").exists());
    let state = root.path().join("first/state");
    std::fs::create_dir_all(&state).unwrap();
    let listener = UnixListener::bind(state.join("cadence.sock")).unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let peer = scope.spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            ready_tx.send(()).unwrap();
            finish_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            writeln!(
                stream,
                "{{\"ok\":true,\"result\":{{\"agents\":[],\"destination\":\"first\"}}}}"
            )
            .unwrap();
        });
        let command = scope.spawn(|| ok(root.path(), &["agent", "list", "--json"]));
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        ok(root.path(), &["org", "switch", "second"]);
        finish_tx.send(()).unwrap();
        assert_eq!(command.join().unwrap()["destination"], "first");
        peer.join().unwrap();
    });
    assert_eq!(
        ok(root.path(), &["issue", "init"])["pm_dir"],
        root.path().join("second/tracker").to_str().unwrap()
    );
}

#[test]
fn explicit_org_conflicts_with_pinned_env_and_registry_rejects_unsafe_paths() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    add(root.path(), "first");
    let out = cadence_agent::reaper::output(
        Command::new(env!("CARGO_BIN_EXE_cadence"))
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("CADENCE_STATE_DIR", root.path().join("pinned/state"))
            .args(["--org", "first", "agent", "list", "--json"]),
    )
    .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("conflict"));
    let out = cli(
        root.path(),
        &[
            "org",
            "add",
            "unsafe",
            "--local-state-dir",
            "/tmp/../other",
            "--tracker-dir",
            "/tmp/tracker",
        ],
    );
    assert!(!out.status.success());
    assert_eq!(
        ok(root.path(), &["org", "list"])["connections"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn explicit_state_dir_works_without_home_or_config_registry() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    let state = root.path().join("explicit/state");
    let out = cadence_agent::reaper::output(
        Command::new(env!("CARGO_BIN_EXE_cadence"))
            .env_remove("HOME")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("CADENCE_HOME")
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_PM_DIR")
            .env_remove("CADENCE_PROFILE")
            .env_remove("CADENCE_ALIAS")
            .args([
                "--state-dir",
                state.to_str().unwrap(),
                "agent",
                "list",
                "--json",
            ]),
    )
    .unwrap();
    assert!(!out.status.success());
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(
        error.contains(state.join("cadence.sock").to_str().unwrap()),
        "{error}"
    );
    assert!(!error.contains("HOME"));
}

#[test]
fn registry_refuses_symlink_authority_and_oversized_records() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    add(root.path(), "first");
    let registry = root.path().join("config/cadence/orgs.json");
    let target = root.path().join("external.json");
    std::fs::rename(&registry, &target).unwrap();
    std::os::unix::fs::symlink(&target, &registry).unwrap();
    assert!(!cli(root.path(), &["org", "list"]).status.success());
    std::fs::remove_file(&registry).unwrap();
    std::fs::write(&registry, vec![b' '; 65537]).unwrap();
    assert!(!cli(root.path(), &["org", "list"]).status.success());
    std::fs::rename(&target, &registry).unwrap();
    let lock = root.path().join("config/cadence/orgs.lock");
    std::fs::remove_file(&lock).unwrap();
    std::os::unix::fs::symlink(root.path().join("foreign.lock"), &lock).unwrap();
    assert!(!cli(root.path(), &["org", "list"]).status.success());
    assert!(!root.path().join("foreign.lock").exists());
}

#[test]
fn fifo_registry_and_lock_fail_promptly_without_waiting_for_a_peer() {
    use std::os::unix::ffi::OsStrExt;
    for filename in ["orgs.json", "orgs.lock"] {
        let root = tempfile::Builder::new()
            .prefix("org-")
            .tempdir_in("/tmp")
            .unwrap();
        add(root.path(), "first");
        let path = root.path().join("config/cadence").join(filename);
        std::fs::remove_file(&path).unwrap();
        let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: a terminated path under this fixture's owned directory.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let mut command = cli_command(root.path(), &["org", "list"]);
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cadence_agent::reaper::spawn(&mut command).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() > deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("{filename} blocked instead of refusing a FIFO");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let out = child.wait_with_output().unwrap();
        assert!(!out.status.success());
    }
}

#[test]
fn managed_explicit_state_without_state_env_ignores_selected_org_and_preserves_tracker() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    add(root.path(), "operator-org");
    ok(root.path(), &["org", "switch", "operator-org"]);
    let original = peer(root.path(), "original");
    let state = root.path().join("original/state");
    let mut command = cli_command(
        root.path(),
        &[
            "--state-dir",
            state.to_str().unwrap(),
            "agent",
            "list",
            "--all",
            "--json",
        ],
    );
    command.env("CADENCE_ALIAS", "worker");
    let out = cadence_agent::reaper::output(&mut command).unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["destination"],
        "original"
    );
    original.join().unwrap();
    let mut command = cli_command(
        root.path(),
        &["--state-dir", state.to_str().unwrap(), "issue", "init"],
    );
    command.env("CADENCE_ALIAS", "worker");
    let out = cadence_agent::reaper::output(&mut command).unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["pm_dir"],
        root.path().join("pm").to_str().unwrap()
    );
    assert!(!root.path().join("operator-org/tracker").exists());
}

#[test]
fn managed_legacy_home_xdg_without_state_env_ignores_selected_org_and_overrides() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    add(root.path(), "operator-org");
    ok(root.path(), &["org", "switch", "operator-org"]);
    let original = peer_at(&root.path().join("legacy/cadence"), "legacy");
    let mut command = cli_command(root.path(), &["agent", "list", "--all", "--json"]);
    command.env("CADENCE_ALIAS", "worker");
    let out = cadence_agent::reaper::output(&mut command).unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["destination"],
        "legacy"
    );
    original.join().unwrap();
    let mut command = cli_command(root.path(), &["issue", "init"]);
    command.env("CADENCE_ALIAS", "worker");
    let out = cadence_agent::reaper::output(&mut command).unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["pm_dir"],
        root.path().join("pm").to_str().unwrap()
    );
    assert!(!root.path().join("operator-org/tracker").exists());
    let mut command = cli_command(
        root.path(),
        &["--org", "operator-org", "agent", "list", "--all", "--json"],
    );
    command.env("CADENCE_ALIAS", "worker");
    let out = cadence_agent::reaper::output(&mut command).unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("managed callers cannot override"));
}
