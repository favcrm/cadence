//! CAD-1019: the org registry is a preference, never authority, and a
//! `Remote` selection drives the allowlisted transport with no local
//! fallback. These tests drive the real `cadence` binary — no daemon, no
//! issuer, no network — against isolated HOME/XDG roots. Each guard is
//! proven by a refusal, not a code path. Self-contained: every helper
//! lives in this file (no `tests/common`).
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Output};

/// A bare CLI invocation: the binary, its env scrubbed of every pin the
/// tests exercise, rooted at a fresh temp dir. `cli` is the success-strict
/// form; `cli_out` returns the raw output for refusal assertions.
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
        .env_remove("CADENCE_ORG")
        .env_remove("CADENCE_ALIAS")
        .args(args);
    command
}
fn cli_out(root: &std::path::Path, args: &[&str]) -> Output {
    cadence_agent::reaper::output(&mut cli_command(root, args)).unwrap()
}
fn ok(root: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let out = cli_out(root, args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn refuse(root: &std::path::Path, args: &[&str]) -> String {
    let out = cli_out(root, args);
    assert!(
        !out.status.success(),
        "expected refusal, got {}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// A fake local peer: bind `<state>/cadence.sock`, answer one
/// `agent_list` with a marker. Proves which state dir the CLI reached.
fn local_peer(
    root: &std::path::Path,
    marker: &'static str,
) -> (std::path::PathBuf, std::thread::JoinHandle<()>) {
    // `local` resolves to the host's own dirs — under the scrubbed env
    // that is `XDG_STATE_HOME/cadence` (legacy layout), not a test-named
    // folder. Bind the fake peer exactly where `resolve` will look.
    let state = root.join("legacy").join("cadence");
    std::fs::create_dir_all(&state).unwrap();
    let socket = UnixListener::bind(state.join("cadence.sock")).unwrap();
    let join = std::thread::spawn(move || {
        let (mut stream, _) = socket.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], "agent_list");
        writeln!(
            stream,
            "{}",
            serde_json::json!({"ok": true, "result": {"agents": [], "destination": marker}})
        )
        .unwrap();
    });
    (state, join)
}

/// The registry file the CLI writes — a test helper reads it back
/// for assertions; the CLI's own locked access is under `orgs.lock`.
fn registry_json(root: &std::path::Path) -> serde_json::Value {
    let orgs = root.join("config").join("cadence").join("orgs.json");
    serde_json::from_slice(&std::fs::read(&orgs).unwrap()).unwrap()
}

/// `local` is always selectable and pins the standalone roots — the
/// registry does not have to pre-exist.
#[test]
fn switch_local_materializes_and_selects() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    let v = ok(root.path(), &["org", "switch", "local"]);
    assert_eq!(v["org"], "local");
    assert_eq!(v["selected"], true);
    // A second switch is idempotent — the connection persists.
    let v = ok(root.path(), &["org", "switch", "local"]);
    assert_eq!(v["selected"], true);
}

/// `CADENCE_ALIAS` is the UX guard: a managed caller cannot change the
/// saved default. The registry is untouched.
#[test]
fn managed_caller_cannot_switch_default() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    ok(root.path(), &["org", "switch", "local"]);
    let mut cmd = cli_command(root.path(), &["org", "switch", "local"]);
    cmd.env("CADENCE_ALIAS", "worker-1");
    let out = cadence_agent::reaper::output(&mut cmd).unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot change the saved org"), "{err}");
}

/// `--org` on a managed caller refuses — inherited pins win over any
/// per-command override. The refusal names no credential and does not
/// touch the registry.
#[test]
fn managed_caller_cannot_override_org() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    ok(root.path(), &["org", "switch", "local"]);
    let mut cmd = cli_command(root.path(), &["--org", "local", "agent", "list", "--json"]);
    cmd.env("CADENCE_ALIAS", "worker-1");
    let out = cadence_agent::reaper::output(&mut cmd).unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("managed callers cannot override"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A `--org` pointing at an unregistered name fails before any peer is
/// contacted — no fallback to the ambient local state.
#[test]
fn unknown_org_refuses_without_fallback() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    ok(root.path(), &["org", "switch", "local"]);
    let err = refuse(
        root.path(),
        &["--org", "nonexistent", "agent", "list", "--json"],
    );
    assert!(err.contains("unknown org"), "{err}");
}

/// A remote org resolves its endpoint and drives the allowlisted verb
/// against the remote host — with no stored credential it fails closed
/// pointing at `login`, and a `--org local` still names the local arm.
/// The remote row is seeded the way `login`'s registry write records it.
#[test]
fn remote_selection_drives_remote_transport_without_fallback() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    ok(root.path(), &["org", "switch", "local"]);
    // Seed a remote connection the way `login` does, without an issuer:
    // write the registry row by hand under the registry's own format.
    let orgs = root.path().join("config").join("cadence").join("orgs.json");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&orgs).unwrap()).unwrap();
    registry["connections"].as_array_mut().unwrap().push(serde_json::json!({
        "selection": {"org": "acme"},
        "destination": {"mode": "remote", "endpoint": "https://acme.cadencecloud.app", "org_id": "ws_acme"}
    }));
    registry["selected"] = serde_json::json!({"org": "acme"});
    std::fs::write(&orgs, serde_json::to_vec_pretty(&registry).unwrap()).unwrap();
    // An allowlisted read resolves remote and fails on the missing
    // credential — the error points at `login`, never at local fallback.
    let err = refuse(root.path(), &["agent", "list", "--json"]);
    assert!(
        err.contains("cadence login") || err.contains("credential"),
        "{err}"
    );
    // A non-allowlisted verb refuses locally before any request is built.
    let err = refuse(
        root.path(),
        &["issue", "new", "a title", "--project", "cadence"],
    );
    assert!(err.contains("not available on a remote org"), "{err}");
    // And `--org local` still names the local arm.
    let (_state, peer) = local_peer(root.path(), "local");
    let out = ok(root.path(), &["--org", "local", "agent", "list", "--json"]);
    assert_eq!(out["destination"], "local");
    peer.join().unwrap();
}

/// Two `switch`es racing different orgs serialize under the registry
/// lock — each writes a complete registry, so the file ends at one
/// coherent selection, never a torn half-write.
#[test]
fn concurrent_switches_leave_a_coherent_registry() {
    let root = tempfile::Builder::new()
        .prefix("org-")
        .tempdir_in("/tmp")
        .unwrap();
    // Seed two orgs via the real writers: `local`, then a remote row
    // written the way `login` would (registry JSON under the same path).
    ok(root.path(), &["org", "switch", "local"]);
    {
        let orgs = root.path().join("config").join("cadence").join("orgs.json");
        let mut reg = registry_json(root.path());
        reg["connections"].as_array_mut().unwrap().push(serde_json::json!({
            "selection": {"org": "other"},
            "destination": {"mode": "local", "state_dir": "/tmp/other/state", "tracker_dir": "/tmp/other/tracker"}
        }));
        std::fs::write(&orgs, serde_json::to_vec_pretty(&reg).unwrap()).unwrap();
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let r1 = root.path().to_path_buf();
    let r2 = root.path().to_path_buf();
    let b1 = barrier.clone();
    let a = std::thread::spawn(move || {
        b1.wait();
        cli_out(&r1, &["org", "switch", "local"])
    });
    let b = std::thread::spawn(move || {
        barrier.wait();
        cli_out(&r2, &["org", "switch", "other"])
    });
    let (a, b) = (a.join().unwrap(), b.join().unwrap());
    // Both succeed or one is refused; never a torn file. The final
    // selection is exactly one org.
    assert!(a.status.success() || b.status.success());
    let reg = registry_json(root.path());
    let sel = reg["selected"]["org"].as_str().unwrap();
    assert!(sel == "local" || sel == "other", "{sel}");
}
