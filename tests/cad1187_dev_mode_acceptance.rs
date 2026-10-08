//! CAD-1187 acceptance A4, written from the ticket by someone other than the
//! implementer. Black box: drives the real `cadence` binary through its CLI
//! only. The implementer may not edit or weaken this file.
//!
//! The rule under test: lease-free / unattested "dev" operation is allowed
//! only when the target store carries the dev marker AND its state dir is
//! under the sandbox base. `dev reload` anywhere else is refused with a
//! reason, and nothing is stopped or started. A build change on a non-dev
//! store still demands the rollout lease.

use std::collections::BTreeMap;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use tempfile::{Builder, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");

struct Host {
    root: TempDir,
}

impl Host {
    fn new() -> Self {
        // Short /tmp root: unix socket paths are limited to 107 bytes.
        let root = Builder::new().prefix("c1187-").tempdir_in("/tmp").unwrap();
        for dir in ["home", "xdg", "tmp", "boxes", "locks", "bin", "elsewhere"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
        }
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// Production's default state dir as the binary computes it under this
    /// isolated HOME/XDG (legacy layout: `$XDG_STATE_HOME/cadence`). It is a
    /// temp stand-in: the real ~/.local/state/cadence is never referenced.
    fn fake_production(&self) -> PathBuf {
        let prod = self.path("xdg/cadence");
        std::fs::create_dir_all(&prod).unwrap();
        std::fs::write(prod.join("canary"), "prod\n").unwrap();
        prod
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BINARY);
        cmd.env("HOME", self.path("home"))
            .env("XDG_STATE_HOME", self.path("xdg"))
            .env("TMPDIR", self.path("tmp"))
            .env("CADENCE_SANDBOX_ROOT", self.path("boxes"))
            .env("CADENCE_TEST_PORT_LOCK_DIR", self.path("locks"))
            .env("CADENCE_SUITE_LOCK", self.path("locks/suite.lock"))
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("CADENCE_PM_DIR")
            .env_remove("CADENCE_HOME")
            .env_remove("CADENCE_PROFILE")
            .env_remove("CADENCE_ALIAS")
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
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = self.command(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let child = cadence_agent::reaper::spawn(&mut cmd).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait_with_output());
        });
        rx.recv_timeout(Duration::from_secs(60))
            .expect("cadence did not finish in 60s")
            .unwrap()
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
    .to_lowercase()
}

/// Pids whose argv or environment names `needle` (a state dir), scanned from
/// /proc (pgrep -f would match this very test).
fn procs_naming(needle: &Path) -> Vec<u32> {
    let needle = needle.to_string_lossy().into_owned();
    let me = std::process::id();
    let mut pids = vec![];
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
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
        let hit = ["cmdline", "environ"].iter().any(|f| {
            std::fs::read(entry.path().join(f))
                .map(|b| String::from_utf8_lossy(&b).contains(&needle))
                .unwrap_or(false)
        });
        if hit {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids
}

fn exe_of(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

/// Relative path -> (kind, length) for a whole tree, symlinks not followed.
fn snapshot(dir: &Path) -> BTreeMap<String, String> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let meta = std::fs::symlink_metadata(e.path()).unwrap();
            let rel = e.path().strip_prefix(base).unwrap().display().to_string();
            if meta.is_dir() {
                out.insert(rel, "dir".into());
                walk(base, &e.path(), out);
            } else {
                out.insert(rel, format!("{:?}:{}", meta.file_type(), meta.len()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Copy directories and regular files only (a live socket cannot be copied,
/// and a copied store has none).
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let meta = std::fs::symlink_metadata(e.path()).unwrap();
        let dest = to.join(e.file_name());
        if meta.is_dir() {
            copy_tree(&e.path(), &dest);
        } else if meta.is_file() {
            std::fs::copy(e.path(), &dest).unwrap();
        }
    }
}

/// A refusal is a rejected-class exit (3, or 4 for a gate) carrying a reason
/// sentence. A clap usage error (2, "unrecognized subcommand"), a crash
/// (101/signal) or a bare non-zero never counts.
fn assert_refused(out: &Output, what: &str, reasons: &[&str]) {
    let all = text(out);
    let code = out.status.code();
    assert!(
        matches!(code, Some(3 | 4)),
        "{what}: expected a rejection exit (3/4), got {code:?}\n{all}"
    );
    assert!(
        !all.contains("unrecognized subcommand")
            && !all.contains("usage:")
            && !all.contains("panicked"),
        "{what}: this is a usage error or crash, not a refusal\n{all}"
    );
    assert!(
        reasons.iter().any(|r| all.contains(r)),
        "{what}: refusal does not name its reason (wanted one of {reasons:?})\n{all}"
    );
}

fn tracked_pids_alive(pids: &[u32]) -> bool {
    pids.iter()
        .all(|p| Path::new(&format!("/proc/{p}")).exists())
}

/// Stops anything this test started, whatever the assertions did.
struct Cleanup<'a> {
    host: &'a Host,
    sandbox_names: Vec<&'static str>,
    plain_states: Vec<PathBuf>,
    watched: Vec<PathBuf>,
}

/// Pids running from, or naming, anything under the temp root: exe path, argv
/// or environment (a board started by a reloaded build carries the root in its
/// exe and in HOME).
fn procs_under(root: &Path) -> Vec<u32> {
    let needle = root.to_string_lossy().into_owned();
    let me = std::process::id();
    let mut pids = procs_naming(root);
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != me && exe_of(pid).is_some_and(|e| e.to_string_lossy().starts_with(&needle)) {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// SIGTERM, then SIGKILL, every process under the root; wait for each to go.
fn reap_under(root: &Path) {
    for signal in [libc::SIGTERM, libc::SIGKILL] {
        let pids = procs_under(root);
        if pids.is_empty() {
            return;
        }
        for pid in &pids {
            unsafe { libc::kill(*pid as i32, signal) };
        }
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < until && !procs_under(root).is_empty() {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn assert_nothing_runs_from(host: &Host) {
    let left = procs_under(host.root.path());
    assert!(
        left.is_empty(),
        "processes left running from the temp root: {left:?}"
    );
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        for n in &self.sandbox_names {
            let _ = self.host.run(&["sandbox", "down", n]);
            let _ = self.host.run(&["sandbox", "reset", n]);
        }
        for s in &self.plain_states {
            let _ = self
                .host
                .run(&["--state-dir", s.to_str().unwrap(), "daemon", "stop"]);
        }
        for w in &self.watched {
            for pid in procs_naming(w) {
                unsafe { libc::kill(pid as i32, libc::SIGTERM) };
            }
        }
        reap_under(self.host.root.path());
    }
}

fn new_build(host: &Host) -> PathBuf {
    let bin = host.path("bin/cadence-new");
    std::fs::copy(BINARY, &bin).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn start_plain_daemon(host: &Host, state: &Path) -> Vec<u32> {
    std::fs::create_dir_all(state).unwrap();
    std::fs::set_permissions(state, std::fs::Permissions::from_mode(0o700)).unwrap();
    let out = host.run(&["--state-dir", state.to_str().unwrap(), "daemon", "start"]);
    assert!(out.status.success(), "plain daemon start: {}", text(&out));
    let pids = procs_naming(state);
    assert!(!pids.is_empty(), "no daemon process found for {state:?}");
    pids
}

// --- (a) plain temp --state-dir, no marker --------------------------------
//
// Fails without the guard: a bare `dev reload --state-dir X` would stop the
// live daemon in X and start the supplied binary (the daemon pids would
// change / die), or would exit 0.
#[test]
fn a4_a_reload_refused_on_unmarked_plain_state_dir() {
    let host = Host::new();
    let state = host.path("plain/state");
    let build = new_build(&host);
    let mut c = Cleanup {
        host: &host,
        sandbox_names: vec![],
        plain_states: vec![state.clone()],
        watched: vec![state.clone()],
    };
    let before = start_plain_daemon(&host, &state);
    let sock_before = state.join("cadence.sock").exists();

    let out = host.run(&[
        "--state-dir",
        state.to_str().unwrap(),
        "dev",
        "reload",
        "--build",
        build.to_str().unwrap(),
    ]);
    assert_refused(
        &out,
        "(a) unmarked store",
        &["marker", "not a dev", "dev store", "not a dev store"],
    );

    assert!(
        tracked_pids_alive(&before),
        "(a) the plain daemon was stopped"
    );
    assert_eq!(
        procs_naming(&state),
        before,
        "(a) a daemon was started or replaced"
    );
    assert_eq!(
        state.join("cadence.sock").exists(),
        sock_before,
        "(a) socket changed"
    );
    for pid in &before {
        assert_ne!(
            exe_of(*pid).as_deref(),
            Some(build.as_path()),
            "(a) new build was started"
        );
    }
    c.watched.clear();
    drop(c);
    assert_nothing_runs_from(&host);
}

// --- (b) real `dev up` store, copied outside the sandbox base ---------------
// --- (c) path resolving to the production state dir -------------------------
//
// Both start from one genuine dev store so the marker is whatever the real
// `dev up` writes (its format is not assumed here). A positive control first
// proves `dev reload` is not simply refusing everything.
//
// Fails without the guard: (b) the copied marker would be honoured (the
// pre-change `owner_of` only checks `<root>/state` beside a marker, never the
// base) and the reload would start a daemon in the copy; (c) a marker beside a
// state dir that is a symlink into production would reach production's store.
#[test]
fn a4_b_c_reload_refused_for_copied_and_production_resolving_stores() {
    let host = Host::new();
    let build = new_build(&host);
    let base = host.path("boxes");
    let root = base.join("dv");
    let state = root.join("state");
    let mut c = Cleanup {
        host: &host,
        sandbox_names: vec!["dv"],
        plain_states: vec![],
        watched: vec![],
    };

    let up = host.run(&["dev", "up", "--name", "dv"]);
    assert!(up.status.success(), "dev up: {}", text(&up));

    // Positive control: the genuine store accepts a reload with a local build.
    let ok = host.run(&[
        "--state-dir",
        state.to_str().unwrap(),
        "dev",
        "reload",
        "--build",
        build.to_str().unwrap(),
    ]);
    assert!(
        ok.status.success(),
        "control: reload on the real dev store must work: {}",
        text(&ok)
    );
    assert!(
        procs_naming(&state)
            .iter()
            .any(|p| exe_of(*p).as_deref() == Some(build.as_path())),
        "control: no process for the dev store runs the given build"
    );

    let down = host.run(&["sandbox", "down", "dv"]);
    assert!(down.status.success(), "sandbox down: {}", text(&down));

    // (b) copy the whole store, marker included, outside the sandbox base.
    let copy_root = host.path("elsewhere/dv");
    copy_tree(&root, &copy_root);
    let copy_state = copy_root.join("state");
    c.watched.push(copy_root.clone());
    let snap = snapshot(&copy_root);
    let out = host.run(&[
        "--state-dir",
        copy_state.to_str().unwrap(),
        "dev",
        "reload",
        "--build",
        build.to_str().unwrap(),
    ]);
    assert_refused(
        &out,
        "(b) copied store",
        &["sandbox base", "outside", "not under"],
    );
    assert_eq!(
        snapshot(&copy_root),
        snap,
        "(b) refused reload modified the copied store"
    );
    assert!(
        procs_naming(&copy_root).is_empty(),
        "(b) a process was started for the copy"
    );
    assert!(
        !copy_state.join("cadence.sock").exists(),
        "(b) a daemon socket appeared"
    );
    c.watched.clear();

    // (c1) a symlink to the (fake) production state dir.
    let prod = host.fake_production();
    let prod_snap = snapshot(&prod);
    let link = host.path("elsewhere/prod-link");
    symlink(&prod, &link).unwrap();
    for target in [&link, &prod] {
        let out = host.run(&[
            "--state-dir",
            target.to_str().unwrap(),
            "dev",
            "reload",
            "--build",
            build.to_str().unwrap(),
        ]);
        assert_refused(
            &out,
            "(c) production dir",
            &[
                "production",
                "marker",
                "not a dev",
                "symlink",
                "overlap",
                "not the root's own",
                "sandbox base",
                "outside",
            ],
        );
        assert_eq!(
            snapshot(&prod),
            prod_snap,
            "(c) production stand-in was modified via {target:?}"
        );
        assert!(
            procs_naming(&prod).is_empty(),
            "(c) a process was started for production"
        );
    }

    // (c2) a genuine marker whose `state` was swapped for a symlink into
    // production: the marker must not exempt it.
    std::fs::rename(&state, root.join("state.real")).unwrap();
    symlink(&prod, &state).unwrap();
    let out = host.run(&[
        "--state-dir",
        state.to_str().unwrap(),
        "dev",
        "reload",
        "--build",
        build.to_str().unwrap(),
    ]);
    assert_refused(
        &out,
        "(c2) marker + state symlinked into production",
        &[
            "production",
            "symlink",
            "overlap",
            "not the root's own",
            "unusable",
            "not a dev",
        ],
    );
    assert_eq!(
        snapshot(&prod),
        prod_snap,
        "(c2) production stand-in was modified"
    );
    assert!(
        procs_naming(&prod).is_empty(),
        "(c2) a process was started for production"
    );
    std::fs::remove_file(&state).unwrap();
    std::fs::rename(root.join("state.real"), &state).unwrap();
    drop(c);
    assert_nothing_runs_from(&host);
}

// --- (d) unmarked store + CADENCE_PROFILE=sandbox:x exported ---------------
//
// Fails without the guard: if the exported profile alone unlocks dev mode the
// live daemon would be replaced (or the command would exit 0).
#[test]
fn a4_d_hand_set_sandbox_profile_unlocks_nothing() {
    let host = Host::new();
    let state = host.path("plain/state");
    let build = new_build(&host);
    let mut c = Cleanup {
        host: &host,
        sandbox_names: vec![],
        plain_states: vec![state.clone()],
        watched: vec![state.clone()],
    };
    let before = start_plain_daemon(&host, &state);

    let out = host.run_env(
        &[
            "--state-dir",
            state.to_str().unwrap(),
            "dev",
            "reload",
            "--build",
            build.to_str().unwrap(),
        ],
        &[("CADENCE_PROFILE", "sandbox:x")],
    );
    assert_refused(
        &out,
        "(d) exported profile, unmarked store",
        &["marker", "not a dev", "dev store"],
    );
    assert!(tracked_pids_alive(&before), "(d) the daemon was stopped");
    assert_eq!(
        procs_naming(&state),
        before,
        "(d) a daemon was started or replaced"
    );
    for pid in &before {
        assert_ne!(
            exe_of(*pid).as_deref(),
            Some(build.as_path()),
            "(d) new build was started"
        );
    }
    c.watched.clear();
    drop(c);
    assert_nothing_runs_from(&host);
}

// --- non-dev store: a build change still demands the rollout lease ---------
//
// The store records a different build than this binary. `daemon start`
// (the same spawn gate `dev reload` must not bypass) is refused for lack of a
// lease, exactly as before, with and without a hand-set sandbox profile.
// Fails if the dev relaxation leaks to non-dev stores.
#[test]
fn a4_build_change_on_non_dev_store_still_requires_the_lease() {
    let host = Host::new();
    let state = host.path("plain/state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    let recorded = "0123456789abcdef0123456789abcdef01234567";
    {
        let conn = rusqlite::Connection::open(cadence_agent::rollout::db_file(&state)).unwrap();
        cadence_agent::rollout::upsert_daemon_build(&conn, recorded, 1.0).unwrap();
    }
    let c = Cleanup {
        host: &host,
        sandbox_names: vec![],
        plain_states: vec![],
        watched: vec![state.clone()],
    };
    let snap = snapshot(&state);

    for env in [vec![], vec![("CADENCE_PROFILE", "sandbox:x")]] {
        let out = host.run_env(
            &[
                "--state-dir",
                state.to_str().unwrap(),
                "daemon",
                "start",
                "--as",
                "operator:acc",
            ],
            &env,
        );
        assert_eq!(
            out.status.code(),
            Some(3),
            "start must be rejected: {}",
            text(&out)
        );
        assert!(
            text(&out).contains("no rollout lease is held"),
            "wrong refusal: {}",
            text(&out)
        );
        assert!(
            procs_naming(&state).is_empty(),
            "a daemon started without a lease"
        );
    }

    // The dev verb must not be a way around it.
    let build = new_build(&host);
    let out = host.run(&[
        "--state-dir",
        state.to_str().unwrap(),
        "dev",
        "reload",
        "--build",
        build.to_str().unwrap(),
    ]);
    assert_refused(
        &out,
        "dev reload on build-changed non-dev store",
        &["marker", "not a dev", "dev store", "lease"],
    );
    assert!(
        procs_naming(&state).is_empty(),
        "dev reload started a daemon"
    );

    // The recorded build is untouched.
    let conn = rusqlite::Connection::open(cadence_agent::rollout::db_file(&state)).unwrap();
    let now: String = conn
        .query_row("SELECT commit_sha FROM daemon_build WHERE id=1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(now, recorded);
    drop(conn);
    let _ = snap;
    drop(c);
    assert_nothing_runs_from(&host);
}
