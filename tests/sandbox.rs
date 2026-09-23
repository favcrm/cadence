//! CAD-310: `cadence sandbox` — isolated dev instances on a host that
//! also runs the production daemon.
//!
//! Every test fakes `HOME` with a temp dir, so the "production
//! defaults" these tests refuse (`~/.local/state/cadence`, `~/pm`) are
//! temp paths too; the real production state is never named. Each test
//! scrubs the cadence env it inherits and stops what it started from a
//! drop guard, so a failed assertion never leaks a daemon or a UI.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant, SystemTime};

use cadence_agent::client;
use serde_json::{json, Value};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_cadence");

/// Env a test inherits from its shell and must never forward: any of
/// these could point a sandbox command at a real instance.
const SCRUB: &[&str] = &[
    "CADENCE_STATE_DIR",
    "CADENCE_PM_DIR",
    "CADENCE_PROFILE",
    "CADENCE_SANDBOX_ROOT",
    "CADENCE_ALIAS",
    "CADENCE_DAEMON_ID",
    "CADENCE_ROLLOUT_AS",
    "XDG_STATE_HOME",
    "XDG_DATA_HOME",
];

/// One fake home plus a short sandbox base under it — unix socket
/// paths stay far below the 108-byte limit.
struct Host {
    tmp: TempDir,
    home: PathBuf,
    base: PathBuf,
    started: Vec<String>,
}

impl Host {
    fn new() -> Self {
        let tmp = tempfile::Builder::new().prefix("sb").tempdir().unwrap();
        // `$HOME` is `<tmp>/home` so a test can aim a sandbox base at
        // its parent and name `home` as a (refused) sandbox.
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let base = home.join("sb");
        Self {
            tmp,
            home,
            base,
            started: Vec::new(),
        }
    }

    fn home(&self) -> &Path {
        &self.home
    }

    fn prod_state(&self) -> PathBuf {
        self.home().join(".local/state/cadence")
    }

    fn prod_pm(&self) -> PathBuf {
        self.home().join("pm")
    }

    /// `cadence <args>` under the fake home, sandboxes rooted at `base`.
    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &Path)]) -> Output {
        let mut cmd = Command::new(BIN);
        for key in SCRUB {
            cmd.env_remove(key);
        }
        cmd.env("HOME", self.home())
            .env("CADENCE_SANDBOX_ROOT", &self.base)
            .args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    /// `run_env` for a command that would serve in the foreground if
    /// its guard regressed: killed after 20s so the test fails instead
    /// of hanging.
    fn run_bounded(&self, args: &[&str], env: &[(&str, &Path)]) -> Output {
        let mut cmd = Command::new(BIN);
        for key in SCRUB {
            cmd.env_remove(key);
        }
        cmd.env("HOME", self.home())
            .env("CADENCE_SANDBOX_ROOT", &self.base)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        child.wait_with_output().unwrap()
    }

    /// `sandbox up <name>`, asserting success; the drop guard downs it.
    fn up(&mut self, name: &str) -> Value {
        self.up_with(name, &[])
    }

    fn up_with(&mut self, name: &str, extra: &[&str]) -> Value {
        self.started.push(name.to_string());
        let mut args = vec!["sandbox", "up", name];
        args.extend(extra);
        let out = self.run(&args);
        assert!(
            out.status.success(),
            "sandbox up {name} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn down(&self, name: &str) -> Output {
        self.run(&["sandbox", "down", name])
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        for name in self.started.clone() {
            let _ = self.down(name.as_str());
        }
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn refused(out: &Output, needle: &str) {
    assert!(!out.status.success(), "expected a refusal: {}", text(out));
    assert!(
        text(out).contains(needle),
        "refusal should mention {needle:?}: {}",
        text(out)
    );
}

/// Blocking `GET /api/health` on the loopback UI — `None` when nothing
/// answers.
fn ui_health(port: u16) -> Option<Value> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(
        stream,
        "GET /api/health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .ok()?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).ok()?;
    serde_json::from_str(buf.split("\r\n\r\n").nth(1)?).ok()
}

/// Does anything on `port` still serve `pm`? A freed port may be taken
/// by a sibling test's sandbox at once, so "nothing answers" is too
/// strong under a parallel run.
fn serves(port: u16, pm: &Path) -> bool {
    ui_health(port).is_some_and(|h| h["pm_dir"].as_str() == Some(&*pm.to_string_lossy()))
}

fn ui_pid(state: &Path) -> i32 {
    std::fs::read_to_string(state.join("ui.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn wait_dead(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while pid_alive(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    !pid_alive(pid)
}

fn environ(pid: i64) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/environ"))
        .unwrap()
        .split(|b| *b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

fn path_of(v: &Value, key: &str) -> PathBuf {
    PathBuf::from(v[key].as_str().unwrap_or_else(|| panic!("{key} in {v}")))
}

/// `up` builds a marked root with its own state dir, PM dir and a UI
/// port from 3110, runs daemon + UI from this binary under the sandbox
/// profile, writes nothing to the (fake) production defaults or to
/// `$HOME`'s skill dirs; `env` and `ls` describe it; `down` stops both;
/// a second `up` reuses the root and its recorded port.
///
/// The port is pinned high in the range: sibling tests pick the lowest
/// free port, and one of them must not take this one while it is down.
#[test]
fn up_isolates_state_pm_and_port_and_down_stops_both() {
    let mut host = Host::new();
    let pinned = (3150..3200)
        .rev()
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .unwrap();
    let up = host.up_with("iso", &["--port", &pinned.to_string()]);
    let root = host.base.join("iso");
    assert_eq!(path_of(&up, "root"), root);
    assert!(root.join("cadence-sandbox.json").is_file(), "marker: {up}");
    let state = path_of(&up, "state_dir");
    let pm = path_of(&up, "pm_dir");
    assert!(state.starts_with(&root) && pm.starts_with(&root), "{up}");
    assert_eq!(path_of(&up, "socket"), state.join("cadence.sock"));
    assert!(pm.join("pm.yaml").is_file(), "PM dir initialised");
    let port = up["port"].as_u64().unwrap() as u16;
    assert_eq!(port, pinned);

    // Daemon: answers on the sandbox socket, runs this binary with the
    // sandbox profile and the sandbox's own dirs.
    let health = client::rpc(&state, "health", json!({})).unwrap();
    let daemon_pid = health["pid"].as_i64().unwrap();
    let env = environ(daemon_pid);
    assert!(env.contains(&"CADENCE_PROFILE=sandbox:iso".to_string()));
    assert!(env.contains(&format!("CADENCE_PM_DIR={}", pm.display())));
    assert!(env.contains(&format!("CADENCE_STATE_DIR={}", state.display())));
    let exe = std::fs::read_link(format!("/proc/{daemon_pid}/exe")).unwrap();
    assert_eq!(exe, std::fs::canonicalize(BIN).unwrap(), "invoking binary");

    // UI: serves this sandbox's PM dir on the recorded port.
    let ui = ui_health(port).expect("sandbox UI answers");
    assert_eq!(PathBuf::from(ui["pm_dir"].as_str().unwrap()), pm);
    assert_eq!(ui["daemon"], "reachable");

    // Nothing reached the production defaults or $HOME's skill dirs.
    for leaked in [
        host.prod_state(),
        host.prod_pm(),
        host.home().join(".agents"),
        host.home().join(".claude/skills"),
    ] {
        assert!(!leaked.exists(), "{} must not exist", leaked.display());
    }

    // env prints the exports; ls lists it as running.
    let env_out = host.run(&["sandbox", "env", "iso"]);
    assert!(env_out.status.success(), "{}", text(&env_out));
    let exports = String::from_utf8_lossy(&env_out.stdout);
    assert!(exports.contains("export CADENCE_PROFILE='sandbox:iso'"));
    assert!(exports.contains(&format!("export CADENCE_STATE_DIR='{}'", state.display())));
    assert!(exports.contains(&format!("export CADENCE_PM_DIR='{}'", pm.display())));
    let ls = host.run(&["sandbox", "ls"]);
    assert!(ls.status.success(), "{}", text(&ls));
    let ls: Value = serde_json::from_slice(&ls.stdout).unwrap();
    let row = ls["sandboxes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "iso")
        .cloned()
        .unwrap_or_else(|| panic!("iso listed: {ls}"));
    assert_eq!(row["port"], port);
    assert_eq!(row["daemon"], "running");
    assert_eq!(row["ui"], "running");

    // A second up while running is idempotent.
    let again = host.up("iso");
    assert_eq!(again["port"], port);
    assert_eq!(again["daemon"]["state"], "already_running", "{again}");

    // down stops daemon and UI.
    let ui_pid = ui_pid(&state);
    let down = host.down("iso");
    assert!(down.status.success(), "{}", text(&down));
    assert!(wait_dead(daemon_pid as i32), "daemon exited");
    assert!(wait_dead(ui_pid), "ui exited");
    assert!(client::rpc(&state, "health", json!({})).is_err());
    assert!(!serves(port, &pm), "ui port released");
    assert!(
        root.join("cadence-sandbox.json").is_file(),
        "down keeps data"
    );

    // up again reuses the root, its data and its recorded port.
    let reused = host.up("iso");
    assert_eq!(reused["port"], port, "recorded port reused: {reused}");
    assert_eq!(reused["daemon"]["state"], "started");
    assert!(serves(port, &pm), "reused UI serves this sandbox");
}

/// Two sandboxes never share a port, a socket or a PM dir.
#[test]
fn two_sandboxes_get_distinct_ports_and_dirs() {
    let mut host = Host::new();
    let a = host.up("aa");
    let b = host.up("bb");
    assert_ne!(a["port"], b["port"]);
    assert_ne!(a["socket"], b["socket"]);
    assert_ne!(a["pm_dir"], b["pm_dir"]);
    let pa = a["port"].as_u64().unwrap() as u16;
    let pb = b["port"].as_u64().unwrap() as u16;
    assert_eq!(ui_health(pa).unwrap()["pm_dir"], a["pm_dir"]);
    assert_eq!(ui_health(pb).unwrap()["pm_dir"], b["pm_dir"]);
}

/// A port some other process already holds is skipped, and `--port`
/// naming it is refused rather than racing the owner.
#[test]
fn up_skips_a_busy_port() {
    let mut host = Host::new();
    // Hold the first free port in range so `up` must look past it.
    let held = (3110..3200)
        .find_map(|p| std::net::TcpListener::bind(("127.0.0.1", p)).ok())
        .unwrap();
    let held_port = held.local_addr().unwrap().port();
    let up = host.up("busy");
    assert_ne!(up["port"], held_port);
    let out = host.run(&["sandbox", "up", "pinned", "--port", &held_port.to_string()]);
    refused(&out, "in use");
    assert!(
        !host.base.join("pinned").exists(),
        "refused before creating"
    );
}

/// `up` refuses when the sandbox would land on the production state
/// dir, socket or PM dir, or port 3010 — before anything is written.
#[test]
fn up_refuses_production_defaults_and_port_3010() {
    let host = Host::new();
    let prod_state = host.prod_state();
    let prod_pm = host.prod_pm();

    // Root == the production state dir.
    let out = host.run_env(
        &["sandbox", "up", "cadence"],
        &[("CADENCE_SANDBOX_ROOT", &host.home().join(".local/state"))],
    );
    refused(&out, "production");
    assert!(!prod_state.exists(), "nothing created under production");

    // Root == the production PM dir.
    let out = host.run_env(
        &["sandbox", "up", "pm"],
        &[("CADENCE_SANDBOX_ROOT", host.home())],
    );
    refused(&out, "production");
    assert!(!prod_pm.exists());

    // Root == an ancestor of the production state dir.
    let out = host.run_env(
        &["sandbox", "up", "state"],
        &[("CADENCE_SANDBOX_ROOT", &host.home().join(".local"))],
    );
    refused(&out, "production");

    // A symlinked root resolving to the production state dir.
    std::fs::create_dir_all(&prod_state).unwrap();
    std::fs::create_dir_all(&host.base).unwrap();
    std::os::unix::fs::symlink(&prod_state, host.base.join("evil")).unwrap();
    let out = host.run(&["sandbox", "up", "evil"]);
    refused(&out, "symlink");
    assert_eq!(std::fs::read_dir(&prod_state).unwrap().count(), 0);

    // Port 3010 is the production board.
    let out = host.run(&["sandbox", "up", "p3010", "--port", "3010"]);
    refused(&out, "3010");
    assert!(!host.base.join("p3010").exists(), "refused before creating");

    // Names are one path segment, lower-case.
    for bad in ["../x", "a/b", "UP", "", "-x"] {
        let out = host.run(&["sandbox", "up", bad]);
        assert!(!out.status.success(), "{bad:?} accepted: {}", text(&out));
    }
    assert!(!host.home().join("x").exists());
}

/// Under `CADENCE_PROFILE=sandbox:*` the daemon itself refuses the
/// production defaults — its state dir or its PM dir — so a mis-set
/// env fails closed. (The UI's port-3010 refusal is a unit test in
/// `src/sandbox.rs`: an integration test of it would, on a regression,
/// bind the real board port.)
#[test]
fn sandbox_profile_daemon_refuses_production_dirs() {
    let host = Host::new();
    let own = host.home().join("own");
    let prod_state = host.prod_state();
    let profile = Path::new("sandbox:t");

    // daemon run on the production state dir.
    let out = host.run_bounded(
        &["--state-dir", prod_state.to_str().unwrap(), "daemon", "run"],
        &[("CADENCE_PROFILE", profile), ("CADENCE_PM_DIR", &own)],
    );
    refused(&out, "production");
    assert!(!prod_state.exists(), "refused before touching the dir");

    // daemon run whose PM dir resolves to the production default.
    let st = own.join("st");
    let out = host.run_bounded(
        &["--state-dir", st.to_str().unwrap(), "daemon", "run"],
        &[("CADENCE_PROFILE", profile)],
    );
    refused(&out, "production");
    assert!(!st.exists(), "refused before touching the dir");
}

/// Under the sandbox profile `ui tailscale` and `--tailscale` are
/// refused before tailscale is ever invoked, and `skill install` does
/// not write into `$HOME`.
#[test]
fn sandbox_profile_refuses_tailscale_and_skill_install() {
    let host = Host::new();
    let st = host.home().join("st");
    let pm = host.home().join("own-pm");
    // A PATH holding only a tailscale that records being called.
    let bin = host.home().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let called = host.home().join("tailscale-called");
    let fake = bin.join("tailscale");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", called.display()),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let path = PathBuf::from(path);
    let env: &[(&str, &Path)] = &[
        ("CADENCE_PROFILE", Path::new("sandbox:t")),
        ("CADENCE_PM_DIR", &pm),
        ("PATH", &path),
    ];
    let st_arg = st.to_str().unwrap();
    for args in [
        vec!["--state-dir", st_arg, "ui", "tailscale", "start"],
        vec!["--state-dir", st_arg, "ui", "tailscale", "stop"],
        vec!["--state-dir", st_arg, "ui", "tailscale", "status"],
        vec![
            "--state-dir",
            st_arg,
            "ui",
            "start",
            "--port",
            "3111",
            "--tailscale",
        ],
        vec!["--state-dir", st_arg, "ui", "stop", "--tailscale-off"],
    ] {
        let out = host.run_env(&args, env);
        refused(&out, "sandbox");
    }
    assert!(!called.exists(), "tailscale must never be invoked");

    let out = host.run_env(&["skill", "install"], env);
    refused(&out, "sandbox");
    assert!(!host.home().join(".agents").exists());
    assert!(!host.home().join(".claude").exists());
}

/// A provider store over the WAL limit is only *reported* by a sandbox
/// daemon (`wal_checkpoint_pending`, dry run) — never checkpointed,
/// even with `wal_dry_run` unset.
#[test]
fn sandbox_daemon_wal_watcher_is_observe_only() {
    let mut host = Host::new();
    // A claude project store with a WAL well past the limit, quiet for
    // two hours; the writer stays open so nothing else checkpoints it.
    let dir = host.home().join(".claude/projects/p");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("store.sqlite");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0i64)
        .unwrap();
    conn.execute_batch("CREATE TABLE t(x BLOB); INSERT INTO t VALUES (randomblob(262144));")
        .unwrap();
    let wal = dir.join("store.sqlite-wal");
    let old = SystemTime::now() - Duration::from_secs(7200);
    std::fs::File::options()
        .write(true)
        .open(&wal)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(old)
                .set_accessed(old),
        )
        .unwrap();
    let before = std::fs::metadata(&wal).unwrap().len();
    assert!(before > 4096);

    // First up creates the PM dir; lower its WAL limit, then restart so
    // the watcher's first tick sees it.
    let up = host.up("wal");
    let pm = path_of(&up, "pm_dir");
    let state = path_of(&up, "state_dir");
    assert!(host.down("wal").status.success());
    let yaml = std::fs::read_to_string(pm.join("pm.yaml")).unwrap();
    std::fs::write(
        pm.join("pm.yaml"),
        format!("{yaml}host:\n  wal_max_bytes: 4096\n"),
    )
    .unwrap();
    host.up("wal");

    let deadline = Instant::now() + Duration::from_secs(20);
    let pending = loop {
        let page = client::rpc(
            &state,
            "agent_events",
            json!({"alias": "daemon", "after": 0}),
        )
        .unwrap();
        let events = page["events"].as_array().cloned().unwrap_or_default();
        assert!(
            !events.iter().any(|e| e["kind"] == "wal_checkpointed"),
            "sandbox daemon checkpointed a provider store: {page}"
        );
        if let Some(e) = events
            .iter()
            .find(|e| e["kind"] == "wal_checkpoint_pending")
        {
            break e.clone();
        }
        assert!(Instant::now() < deadline, "no WAL watch event: {page}");
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(pending["payload"]["dry_run"], true, "{pending}");
    assert_eq!(
        std::fs::metadata(&wal).unwrap().len(),
        before,
        "WAL untouched"
    );
    drop(conn);
}

/// `reset` deletes a sandbox root only when it holds a matching marker
/// and is neither a symlink, `$HOME`, `/`, nor a production dir; the
/// real one is stopped first and then gone.
#[test]
fn reset_deletes_only_a_marked_sandbox() {
    let mut host = Host::new();
    std::fs::create_dir_all(&host.base).unwrap();

    // No such sandbox.
    refused(&host.run(&["sandbox", "reset", "nope"]), "no sandbox");

    // An unmarked directory is left alone — by reset and by down.
    let plain = host.base.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("keep"), "x").unwrap();
    refused(&host.run(&["sandbox", "reset", "plain"]), "marker");
    refused(&host.run(&["sandbox", "down", "plain"]), "marker");
    assert!(plain.join("keep").is_file());

    // A real sandbox to borrow a marker from.
    let up = host.up("real");
    let real = path_of(&up, "root");
    let marker = std::fs::read(real.join("cadence-sandbox.json")).unwrap();

    // Marker copied under another name → name mismatch.
    let copy = host.base.join("copy");
    std::fs::create_dir_all(&copy).unwrap();
    std::fs::write(copy.join("cadence-sandbox.json"), &marker).unwrap();
    refused(&host.run(&["sandbox", "reset", "copy"]), "marker");
    assert!(copy.join("cadence-sandbox.json").is_file());

    // Marker file is a symlink to a real marker.
    let linked = host.base.join("linked");
    std::fs::create_dir_all(&linked).unwrap();
    std::os::unix::fs::symlink(
        real.join("cadence-sandbox.json"),
        linked.join("cadence-sandbox.json"),
    )
    .unwrap();
    refused(&host.run(&["sandbox", "reset", "linked"]), "symlink");
    assert!(linked.is_dir());

    // Root is a symlink to the real sandbox.
    std::os::unix::fs::symlink(&real, host.base.join("alias")).unwrap();
    refused(&host.run(&["sandbox", "reset", "alias"]), "symlink");
    assert!(real.join("cadence-sandbox.json").is_file());

    // A forged marker naming $HOME, and one naming the production
    // state dir: both refused, both intact.
    let home = host.home().to_path_buf();
    let home_name = "home".to_string();
    let forged = |root: &Path, name: &str| {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(
            root.join("cadence-sandbox.json"),
            serde_json::to_vec(&json!({
                "kind": "cadence-sandbox", "version": 1,
                "name": name, "root": root, "port": 3150,
            }))
            .unwrap(),
        )
        .unwrap();
    };
    forged(&home, &home_name);
    let out = host.run_env(
        &["sandbox", "reset", &home_name],
        &[("CADENCE_SANDBOX_ROOT", host.tmp.path())],
    );
    refused(&out, "refus");
    assert!(
        home.is_dir() && host.base.join("real").is_dir(),
        "HOME intact"
    );
    std::fs::remove_file(home.join("cadence-sandbox.json")).unwrap();

    let prod_state = host.prod_state();
    forged(&prod_state, "cadence");
    std::fs::write(prod_state.join("cadence.sqlite3"), "live").unwrap();
    let out = host.run_env(
        &["sandbox", "reset", "cadence"],
        &[("CADENCE_SANDBOX_ROOT", &host.home().join(".local/state"))],
    );
    refused(&out, "production");
    assert!(
        prod_state.join("cadence.sqlite3").is_file(),
        "production intact"
    );

    // The real one: reset stops its daemon and UI, then deletes it.
    let state = path_of(&up, "state_dir");
    let pm = path_of(&up, "pm_dir");
    let port = up["port"].as_u64().unwrap() as u16;
    let daemon_pid = client::rpc(&state, "health", json!({})).unwrap()["pid"]
        .as_i64()
        .unwrap();
    let ui_pid = ui_pid(&state);
    let out = host.run(&["sandbox", "reset", "real"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!real.exists(), "sandbox root deleted");
    assert!(wait_dead(daemon_pid as i32), "daemon stopped before delete");
    assert!(wait_dead(ui_pid), "ui stopped before delete");
    assert!(!serves(port, &pm), "ui port released");
    host.started.retain(|n| n != "real");
    // Everything else under the base survived.
    assert!(plain.join("keep").is_file() && copy.is_dir() && linked.is_dir());
}
