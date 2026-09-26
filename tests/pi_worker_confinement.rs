//! CAD-556 — Landlock confinement for pi WORKERS (`join <pm> pi
//! --confine`, or `pm.yaml [host] confine_pi_workers: true`). The
//! worker's provider is exec'd through `cadence confine` under a
//! per-worker policy: its worktree + shared git dir + declared caches
//! writable, the toolchain readable, `$HOME` and every other agent's
//! dir denied — the same launcher machinery as the master (CAD-439),
//! never a second sandbox.
//!
//! Policy-shape tests run anywhere; the legs that apply the policy for
//! real (`cadence confine -- …`) skip where the kernel has no
//! Landlock. `agent_show` reports `confined` plus the emitted policy.

#![allow(clippy::disallowed_methods)]

mod common;

use std::path::{Path, PathBuf};

use cadence_agent::adapter::pi::PiAdapter;
use cadence_agent::adapter::{AdapterHooks, ProviderAdapter, ProviderEnv};
use cadence_agent::store::Agent;
use common::*;
use serde_json::{json, Value};

fn fake_pi(mode: &str) -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/fake-pi.py");
    format!("python3 {} {mode}", script.display())
}

fn worker(alias: &str, cwd: &Path, params: Value) -> Agent {
    let mut params = params;
    // CAD-559: a pi agent opens only on an explicit allowlisted model —
    // tests that want another value set the key themselves.
    if let Some(p) = params.as_object_mut() {
        p.entry("model".to_string())
            .or_insert_with(|| json!("fake/model-1"));
    }
    Agent {
        alias: alias.into(),
        provider: "pi".into(),
        endpoint_kind: "managed".into(),
        role: "worker".into(),
        team_role: None,
        cwd: cwd.to_string_lossy().into(),
        sandbox: "read-only".into(),
        instructions: None,
        thread_id: None,
        session_id: None,
        model: None,
        effort: None,
        pid: None,
        pid_start: None,
        endpoint: None,
        params: Some(params),
        model_selection: None,
        quota: None,
        generation: None,
        state: "starting".into(),
        enabled: true,
        error: None,
        created: 0.0,
        updated: 0.0,
    }
}

/// An adapter whose state dir is `state` — the log's grandparent is
/// what `PiAdapter` derives `state_dir` from.
fn adapter(mode: &str, state: &Path, own: &[(&str, String)]) -> PiAdapter {
    let env = ProviderEnv::default();
    env.set("CADENCE_PI_COMMAND", fake_pi(mode));
    // CAD-559: pi opens only under an operator `[pi]` policy — `own`
    // can still repoint CADENCE_PM_DIR at a test's own pm.yaml.
    let pm = state.join("pm");
    pi_policy_pm(&pm);
    env.set("CADENCE_PM_DIR", pm.to_string_lossy().to_string());
    for (k, v) in own {
        env.set(k, v.clone());
    }
    std::fs::create_dir_all(state.join("agents")).unwrap();
    PiAdapter::new(
        AdapterHooks {
            on_event: Box::new(|_, _| {}),
            on_request: Box::new(|_| {}),
        },
        &state.join("agents").join("w.provider.log"),
        &env,
    )
}

/// Is `path` reachable under this grant set? A grant covers itself
/// and everything beneath it — a granted FILE does not open its
/// parent dir, and a granted child does not open its ancestors.
fn covers(set: &[PathBuf], path: &Path) -> bool {
    set.iter().any(|grant| path.starts_with(grant))
}

/// `git` on PATH — the probes run real `git`/`cargo` subprocesses.
fn git(args: &[&str], cwd: &Path) -> std::process::Output {
    std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

/// Daemon-env wiring so a `confine:true` worker launched by a
/// TestDaemon really opens confined: the real `cadence` bin as the
/// confine exe (the test binary has no `confine` subcommand) and the
/// e2e dir on the worker extra-read seam so confined fake-pi is
/// readable. [`ConfineEnv`]'s drop removes both.
struct ConfineEnv {
    _private: (),
}

fn confine_env() -> ConfineEnv {
    test_env().set(
        "CADENCE_CONFINE_COMMAND",
        env!("CARGO_BIN_EXE_cadence").to_string(),
    );
    let e2e = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e");
    test_env().set(
        cadence_agent::adapter::pi::CONFINE_WORKER_EXTRA_READ_ENV,
        e2e.to_string_lossy().to_string(),
    );
    ConfineEnv { _private: () }
}

impl Drop for ConfineEnv {
    fn drop(&mut self) {
        test_env().remove("CADENCE_CONFINE_COMMAND");
        test_env().remove(cadence_agent::adapter::pi::CONFINE_WORKER_EXTRA_READ_ENV);
    }
}

/// A repo with one commit, a linked worktree `wt` (so the policy must
/// grant the MAIN checkout's shared `.git`), a bare `remote.git`, and
/// a sibling checkout `other/` the confined worker must not see.
struct Repo {
    wt: PathBuf,
    common_git: PathBuf,
    remote: PathBuf,
    sibling: PathBuf,
}

fn fixture_repo(root: &Path) -> Repo {
    let main = root.join("repo-main");
    std::fs::create_dir_all(&main).unwrap();
    assert!(git(&["init", "-b", "main"], &main).status.success());
    std::fs::write(main.join("tracked.txt"), "v1\n").unwrap();
    assert!(git(&["add", "-A"], &main).status.success());
    assert!(git(
        &[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@t",
            "commit",
            "-m",
            "init"
        ],
        &main
    )
    .status
    .success());
    let wt = root.join("wt");
    assert!(git(&["worktree", "add", wt.to_str().unwrap()], &main)
        .status
        .success());
    let remote = root.join("remote.git");
    assert!(git(&["init", "--bare", remote.to_str().unwrap()], root)
        .status
        .success());
    let sibling = root.join("other");
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(sibling.join("secret.txt"), "not yours\n").unwrap();
    Repo {
        common_git: main.join(".git"),
        wt,
        remote,
        sibling,
    }
}

// ---- policy shape: no Landlock needed ----

/// The emitted worker policy — what `open` feeds `cadence confine`.
/// Write: worktree, shared git dir, worker dir, pm dir, cargo caches,
/// sccache. Read: toolchain + the two non-secret ssh files. Never:
/// `$HOME` itself, `~/.ssh` keys, `~/.gitconfig`, `~/.pi`/`~/.claude`,
/// `credentials.toml`, other agents' dirs, the master dir, the daemon
/// store root. A mutation widening any of those fails here.
#[test]
fn emitted_worker_policy_is_the_worktree_plus_declared_caches() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let state = root.join("state");
    let home = root.join("home");
    let pm = root.join("pm");
    let repo = fixture_repo(root);
    for d in [
        home.join(".ssh"),
        home.join(".pi"),
        home.join(".claude"),
        home.join(".cargo"),
        home.join(".rustup"),
        home.join(".cache/sccache"),
        pm.as_path().to_path_buf(),
        state.join("agents").join("other"),
        state.join("master").join("pi"),
    ] {
        std::fs::create_dir_all(&d).unwrap();
    }
    std::fs::write(home.join(".ssh/id_ed25519"), "KEY\n").unwrap();
    std::fs::write(home.join(".ssh/config"), "Host *\n").unwrap();
    std::fs::write(home.join(".ssh/known_hosts"), "h ssh-ed25519 k\n").unwrap();
    std::fs::write(home.join(".gitconfig"), "[user]\n\tname = Op\n").unwrap();
    std::fs::write(home.join(".cargo/config.toml"), "[build]\njobs = 2\n").unwrap();
    std::fs::write(
        home.join(".cargo/credentials.toml"),
        "[registry]\ntoken = \"x\"\n",
    )
    .unwrap();

    let env = ProviderEnv::default();
    env.set("CADENCE_PI_COMMAND", fake_pi("normal"));
    env.set("HOME", home.to_string_lossy().to_string());
    env.set("CADENCE_PM_DIR", pm.to_string_lossy().to_string());
    // The toolchain vars are read from the daemon env first — set
    // them to the fake home's so the whole policy is deterministic
    // (the real process env's CARGO_HOME would win otherwise).
    env.set(
        "CARGO_HOME",
        home.join(".cargo").to_string_lossy().to_string(),
    );
    env.set(
        "RUSTUP_HOME",
        home.join(".rustup").to_string_lossy().to_string(),
    );
    env.set(
        "SCCACHE_DIR",
        home.join(".cache/sccache").to_string_lossy().to_string(),
    );
    let agent = worker("wc", &repo.wt, json!({"confine": true}));
    let (_exe, policy) = cadence_agent::adapter::pi::pi_worker_confinement(&env, &state, &agent);

    let worker_dir = state.join("agents").join("wc");
    for want in [
        repo.wt.clone(),
        repo.common_git.clone(),
        worker_dir.clone(),
        // CAD-570: the worker's own XDG_CACHE_HOME — where pi-devin
        // writes its model catalog — named in the write set itself,
        // not only covered by the worker-dir grant.
        worker_dir.join("pi/cache"),
        pm.clone(),
        home.join(".cargo/registry"),
        home.join(".cargo/git"),
        home.join(".cache/sccache"),
    ] {
        assert!(
            covers(&policy.write, &want),
            "write lacks {}",
            want.display()
        );
    }
    // And never the operator's own cache — the un-fixed posture this
    // ticket removes.
    assert!(
        !covers(&policy.write, &home.join(".cache/pi-devin"))
            && !covers(&policy.read, &home.join(".cache/pi-devin")),
        "the operator's ~/.cache must stay out of the policy"
    );
    for want in [
        home.join(".cargo/bin"),
        home.join(".cargo/config.toml"),
        home.join(".rustup"),
        home.join(".ssh/config"),
        home.join(".ssh/known_hosts"),
        PathBuf::from("/usr"),
    ] {
        assert!(covers(&policy.read, &want), "read lacks {}", want.display());
    }
    // The deny surface — asserted as ABSENCE, so a widening mutation
    // (e.g. `write.push(home)`) trips the test, not a runtime probe.
    // Inside $HOME the check is airtight: EVERY granted path under it
    // must be one of the enumerated allows — a new `home.join("x")`
    // grant anywhere in the policy fails this loop.
    let home_allows: Vec<PathBuf> = vec![
        home.join(".ssh/config"),
        home.join(".ssh/known_hosts"),
        home.join(".cargo/bin"),
        home.join(".cargo/config.toml"),
        home.join(".cargo/registry"),
        home.join(".cargo/git"),
        home.join(".cargo/.global-cache"),
        home.join(".cargo/.package-cache"),
        home.join(".rustup"),
        home.join(".cache/sccache"),
        home.join(".config/sccache"),
    ];
    for entry in policy.read.iter().chain(policy.write.iter()) {
        if entry.starts_with(&home) {
            assert!(
                home_allows.iter().any(|a| entry.starts_with(a)),
                "policy reaches {} — outside the declared home set",
                entry.display()
            );
        }
    }
    for denied in [
        home.join(".ssh"),
        home.join(".ssh/id_ed25519"),
        home.join(".gitconfig"),
        home.join(".pi"),
        home.join(".claude"),
        home.join(".cargo/credentials.toml"),
        repo.sibling.clone(),
        state.join("agents").join("other"),
        state.join("master"),
    ] {
        assert!(
            !covers(&policy.write, &denied) && !covers(&policy.read, &denied),
            "policy reaches {}",
            denied.display()
        );
    }
    // …and a granted child never drags its parent along: `agents/wc`
    // is writable but `agents/` itself must not be.
    assert!(covers(&policy.write, &worker_dir));
    assert!(
        !covers(&policy.write, &state.join("agents")),
        "agents/ itself is not a grant"
    );
    assert!(
        !covers(&policy.write, &state),
        "state dir root is not a grant"
    );
    assert!(
        !covers(&policy.write, &home.join(".cargo")),
        "cargo root — only named caches"
    );
}

/// Without `confine` the worker argv is the plain CAD-544 one — no
/// wrapper, no confinement log. Default-off is the shipped posture.
#[test]
fn unconfined_worker_launch_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    for params in [json!({}), json!({"confine": false})] {
        let ad = adapter("normal", dir.path(), &[]);
        let agent = worker("wu", dir.path(), params);
        ad.open(&agent).unwrap();
        ad.close();
        let rec: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("agents/pi-record-wu.json")).unwrap(),
        )
        .unwrap();
        let argv: Vec<String> = serde_json::from_value(rec["argv"].clone()).unwrap();
        assert!(argv.windows(2).any(|w| w[0] == "--session"), "{argv:?}");
        assert!(!argv.iter().any(|a| a == "confine"), "{argv:?}");
        let log = std::fs::read_to_string(dir.path().join("agents/w.provider.log")).unwrap();
        assert!(!log.contains("worker confinement:"), "{log}");
    }
}

/// `--confine` on a host without Landlock is refused, never silently
/// unconfined (the TEST_NO_LANDLOCK seam simulates the kernel saying
/// no).
#[test]
fn confine_refuses_when_the_host_cannot_confine() {
    let dir = tempfile::tempdir().unwrap();
    let ad = adapter(
        "normal",
        dir.path(),
        &[(cadence_agent::master::TEST_NO_LANDLOCK, "1".into())],
    );
    let agent = worker("wn", dir.path(), json!({"confine": true}));
    let err = match ad.open(&agent) {
        Err(e) => e,
        Ok(_) => panic!("--confine without Landlock must refuse"),
    };
    assert!(
        err.to_string().contains("Landlock"),
        "--confine without Landlock must name the miss: {err}"
    );
    assert!(
        !dir.path().join("agents/pi-record-wn.json").exists(),
        "a refused launch ran the provider anyway"
    );
}

// ---- the confined process for real (Landlock-gated) ----

/// A confined worker really is exec'd through `cadence confine`: the
/// fake runs under the policy (record lands in its granted dir —
/// `agents/pi-record-<alias>.json`, a sibling write, is DENIED and the
/// fake falls back to `agents/<alias>/pi-record.json`), a turn
/// completes, TMPDIR/GIT_CONFIG_GLOBAL were redirected, and the
/// provider log carries the emitted policy.
#[test]
fn confined_worker_runs_fake_pi_under_the_policy() {
    if cadence_agent::confine::available().is_err() {
        eprintln!("no Landlock on this host — confined leg skipped");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // cwd and the state dir must be DISJOINT — nesting state inside
    // the worktree would make `agents/` writable by the cwd grant and
    // the record assertions meaningless.
    let state = dir.path().join("state");
    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let e2e = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e");
    let ad = adapter(
        "normal",
        &state,
        &[
            (
                "CADENCE_CONFINE_COMMAND",
                env!("CARGO_BIN_EXE_cadence").to_string(),
            ),
            // fake-pi.py lives outside every grant — the worker
            // extra-read seam exposes it, like the master's test does.
            (
                cadence_agent::adapter::pi::CONFINE_WORKER_EXTRA_READ_ENV,
                e2e.to_string_lossy().to_string(),
            ),
        ],
    );
    let agent = worker("wc", &cwd, json!({"confine": true}));
    ad.open(&agent).unwrap();
    let turn = ad.run_turn("confined hello", "m1", &|_| {}).unwrap();
    assert_eq!(turn.status, "completed", "{}", turn.status);
    ad.close();

    // The sibling record path is outside the grant — the fake's write
    // failed, so the record proves confinement by WHERE it landed.
    assert!(
        !state.join("agents/pi-record-wc.json").exists(),
        "a confined worker wrote a sibling file under agents/"
    );
    let rec: Value = serde_json::from_str(
        &std::fs::read_to_string(state.join("agents/wc/pi-record.json")).unwrap(),
    )
    .unwrap();
    let env: Vec<String> = serde_json::from_value(rec["env"].clone()).unwrap();
    for name in [
        "TMPDIR",
        "GIT_CONFIG_GLOBAL",
        "PI_CODING_AGENT_DIR",
        "XDG_CACHE_HOME",
    ] {
        assert!(env.iter().any(|n| n == name), "{name} missing: {env:?}");
    }
    // CAD-570: the pi-devin catalog cache landed in the worker's own
    // XDG_CACHE_HOME under the policy — before the fix this write hit
    // the denied `~/.cache/pi-devin` and logged EACCES.
    assert!(
        state
            .join("agents/wc/pi/cache/pi-devin/models.json")
            .is_file(),
        "confined worker: the catalog cache did not land in agents/wc/pi/cache"
    );
    let log = std::fs::read_to_string(state.join("agents/w.provider.log")).unwrap();
    assert!(log.contains("worker confinement:"), "{log}");
    assert!(
        !log.contains("EACCES"),
        "confined worker logged a cache EACCES: {log}"
    );
}

/// The emitted policy applied for real via `cadence confine`: inside
/// it, a process edits the worktree, runs a cargo build, commits and
/// pushes — and EACCES on the sibling checkout, `~/.ssh`, the master
/// dir, another worker's dir, the daemon store, and any write outside
/// the granted roots.
#[test]
fn confined_policy_allows_work_and_denies_the_rest() {
    if cadence_agent::confine::available().is_err() {
        eprintln!("no Landlock on this host — confined leg skipped");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let state = root.join("state");
    let home = root.join("home");
    let pm = root.join("pm");
    let repo = fixture_repo(root);
    // The toolchain dirs are the host's real ones (cargo/rustup only
    // exist there); the DENY targets are the fake home's so the test
    // never touches operator content.
    let cargo_home = PathBuf::from(
        std::env::var("CARGO_HOME")
            .unwrap_or_else(|_| format!("{}/.cargo", std::env::var("HOME").unwrap())),
    );
    let rustup_home = PathBuf::from(
        std::env::var("RUSTUP_HOME")
            .unwrap_or_else(|_| format!("{}/.rustup", std::env::var("HOME").unwrap())),
    );
    let sccache = PathBuf::from(
        std::env::var("SCCACHE_DIR")
            .unwrap_or_else(|_| format!("{}/.cache/sccache", std::env::var("HOME").unwrap())),
    );
    for d in [
        home.join(".ssh"),
        home.join(".pi"),
        home.join(".claude"),
        pm.as_path().to_path_buf(),
        state.join("agents").join("wc"),
        state.join("agents").join("other").join("pi"),
        state.join("master").join("pi"),
    ] {
        std::fs::create_dir_all(&d).unwrap();
    }
    std::fs::write(home.join(".ssh/id_ed25519"), "KEY\n").unwrap();
    std::fs::write(home.join(".ssh/config"), "Host *\n").unwrap();
    std::fs::write(home.join(".gitconfig"), "[user]\n").unwrap();
    std::fs::write(state.join("agents/other/pi/auth.json"), "{}\n").unwrap();
    std::fs::write(state.join("master/pi/auth.json"), "{}\n").unwrap();
    std::fs::write(state.join("store.db"), "x\n").unwrap();
    // A dependency-free crate — `cargo build --offline` exercises the
    // toolchain + target-dir + cache grants without the network.
    let krate = repo.wt.join("hello");
    std::fs::create_dir_all(krate.join("src")).unwrap();
    std::fs::write(
        krate.join("Cargo.toml"),
        "[package]\nname = \"hello\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(krate.join("src/main.rs"), "fn main() {}\n").unwrap();
    // The worker's private gitconfig (what open() would seed) — git is
    // pointed at it so an unreadable ~/.gitconfig cannot fatal.
    let worker_dir = state.join("agents").join("wc");
    let gitconfig = worker_dir.join("gitconfig");
    std::fs::write(&gitconfig, "[user]\n\tname = W\n\temail = w@w\n").unwrap();
    let tmp = worker_dir.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();

    let env = ProviderEnv::default();
    env.set("CADENCE_PI_COMMAND", fake_pi("normal"));
    env.set("HOME", home.to_string_lossy().to_string());
    env.set("CADENCE_PM_DIR", pm.to_string_lossy().to_string());
    // Without this the emitted confine exe is the TEST binary (its
    // libtest parser answers `--read` with "Unrecognized option").
    env.set(
        "CADENCE_CONFINE_COMMAND",
        env!("CARGO_BIN_EXE_cadence").to_string(),
    );
    env.set("CARGO_HOME", cargo_home.to_string_lossy().to_string());
    env.set("RUSTUP_HOME", rustup_home.to_string_lossy().to_string());
    env.set("SCCACHE_DIR", sccache.to_string_lossy().to_string());
    // The push target: a file remote is a path, so it rides the
    // declared extra-write seam — ssh/https remotes are network and
    // Landlock does not gate them.
    env.set(
        cadence_agent::adapter::pi::CONFINE_WORKER_EXTRA_WRITE_ENV,
        repo.remote.to_string_lossy().to_string(),
    );
    let agent = worker("wc", &repo.wt, json!({"confine": true}));
    let (confine, policy) = cadence_agent::adapter::pi::pi_worker_confinement(&env, &state, &agent);
    // The bare repo gets the worktree's refs pushed to it.
    assert!(git(
        &["remote", "add", "origin", repo.remote.to_str().unwrap()],
        &repo.wt
    )
    .status
    .success());

    // The probe runs inside the policy: argv-only code (no script file
    // to grant), manifest as JSON on argv, report on stdout.
    let spec = json!({
        "read_ok": [repo.wt.join("tracked.txt"), home.join(".ssh/config")],
        "write_ok": [repo.wt.join("made.txt"), worker_dir.join("note.txt")],
        "read_deny": [
            repo.sibling.join("secret.txt"),
            home.join(".ssh/id_ed25519"),
            home.join(".gitconfig"),
            home.join(".pi"),
            state.join("agents/other/pi/auth.json"),
            state.join("master/pi/auth.json"),
            state.join("store.db"),
        ],
        "write_deny": [
            repo.sibling.join("evil.txt"),
            home.join("pwned"),
            state.join("master/pi/evil"),
            state.join("agents/other/pi-record.json"),
            root.join("outside.txt"),
            PathBuf::from("/tmp").join(format!("cad556-{}", std::process::id())),
        ],
        "run_ok": [
            ["git", "-C", repo.wt.to_str().unwrap(), "add", "-A"],
            ["git", "-C", repo.wt.to_str().unwrap(), "commit", "-m", "work"],
            ["git", "-C", repo.wt.to_str().unwrap(), "push", "origin", "HEAD:refs/heads/wt"],
            ["cargo", "build", "--offline", "--manifest-path", krate.join("Cargo.toml").to_str().unwrap()],
        ],
    });
    let out = std::process::Command::new(&confine)
        .arg("confine")
        .args(policy.to_args())
        .arg("--")
        .arg("python3")
        .arg("-c")
        .arg(PROBE)
        .arg(spec.to_string())
        .env("HOME", &home)
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("TMPDIR", &tmp)
        .env("CARGO_HOME", &cargo_home)
        .env("RUSTUP_HOME", &rustup_home)
        .env("SCCACHE_DIR", &sccache)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report: Value = serde_json::from_str(stdout.trim().lines().last().unwrap_or("{}"))
        .unwrap_or_else(|e| {
            panic!(
                "probe output not json: {e}\nstdout={stdout}\nstderr={}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    assert!(
        out.status.success() && report["ok"] == true,
        "confined probes: {}\nstderr: {}",
        serde_json::to_string_pretty(&report).unwrap(),
        String::from_utf8_lossy(&out.stderr)
    );
    // And the work landed for real.
    assert!(git(&["log", "--format=%s", "-1"], &repo.wt)
        .stdout
        .starts_with(b"work"));
    assert!(repo.wt.join("made.txt").exists());
}

/// The probe the policy test runs: every check reports into the JSON
/// on stdout — denied-expected passes only on EACCES/ENOENT, a SUCCESS
/// means the policy leaked.
const PROBE: &str = r#"
import json, os, subprocess, sys
spec = json.loads(sys.argv[1])
checks = []
def rec(name, ok, detail=""):
    checks.append({"check": name, "ok": bool(ok), "detail": str(detail)[:400]})
def can_read(p):
    try:
        open(p).read(1)
        return True
    except OSError as e:
        return e
def can_write(p):
    try:
        open(p, "w").write("x")
        return True
    except OSError as e:
        return e
for p in spec["read_ok"]:
    r = can_read(p); rec("read:" + p, r is True, r)
for p in spec["write_ok"]:
    r = can_write(p); rec("write:" + p, r is True, r)
for p in spec["read_deny"]:
    r = can_read(p); rec("deny-read:" + p, r is not True, "LEAKED" if r is True else r)
for p in spec["write_deny"]:
    r = can_write(p); rec("deny-write:" + p, r is not True, "LEAKED" if r is True else r)
for argv in spec.get("run_ok", []):
    r = subprocess.run(argv, capture_output=True, text=True, timeout=300)
    rec("run:" + " ".join(argv[:3]), r.returncode == 0,
        r.stdout[-300:] + r.stderr[-300:] if r.returncode else "")
ok = all(c["ok"] for c in checks)
print(json.dumps({"ok": ok, "fail": [c for c in checks if not c["ok"]]}))
"#;

// ---- the register/show/default wiring (daemon-level) ----

/// `agent show` answers "is it confined" on every pi/managed row:
/// `confined` mirrors the param, and a confined row carries the
/// emitted `confinement` policy (the same vectors `open` feeds
/// `cadence confine`).
#[test]
fn agent_show_reports_the_worker_policy() {
    let _env = confine_env();
    let d = TestDaemon::start();
    let _pi = d.mock_pi("normal");
    d.register_pi("wshow", json!({}));
    let show = d.rpc("agent_show", json!({"alias": "wshow"})).unwrap();
    assert_eq!(show["agent"]["confined"], false, "{show}");
    assert!(show["agent"]["confinement"].is_null(), "{show}");

    d.register_pi("wshow2", json!({"confine": true}));
    let show = d.rpc("agent_show", json!({"alias": "wshow2"})).unwrap();
    assert_eq!(show["agent"]["confined"], true, "{show}");
    let policy = &show["agent"]["confinement"];
    let write: Vec<String> = serde_json::from_value(policy["write"].clone()).unwrap();
    let worker_dir = d.state.join("agents").join("wshow2");
    assert!(
        write.iter().any(|p| p == &worker_dir.to_string_lossy()),
        "emitted policy lacks the worker dir: {write:?}"
    );
    // The master's read-mostly confinement fields are unchanged.
    assert!(!write.iter().any(|p| p == "/usr"), "{write:?}");
}

/// `pm.yaml [host] confine_pi_workers` defaults a paramless pi join to
/// confined; an explicit `confine` param — `join --no-confine` — wins.
#[test]
fn pm_config_defaults_confinement_and_explicit_params_win() {
    let pm_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        pm_dir.path().join("pm.yaml"),
        "schema: 1\nhost:\n  confine_pi_workers: true\n",
    )
    .unwrap();
    test_env().set(
        "CADENCE_PM_DIR",
        pm_dir.path().to_string_lossy().to_string(),
    );
    let _env = confine_env();
    let d = TestDaemon::start();
    let _pi = d.mock_pi("normal");

    // No `confine` in params → the host default inserts it.
    d.register_pi("wdef", json!({}));
    let show = d.rpc("agent_show", json!({"alias": "wdef"})).unwrap();
    assert_eq!(show["agent"]["params"]["confine"], true, "{show}");
    assert_eq!(show["agent"]["confined"], true, "{show}");

    // An explicit false is the `--no-confine` shape — the default
    // never overrides what the caller said.
    d.register_pi("wopt", json!({"confine": false}));
    let show = d.rpc("agent_show", json!({"alias": "wopt"})).unwrap();
    assert_eq!(show["agent"]["params"]["confine"], false, "{show}");
    assert_eq!(show["agent"]["confined"], false, "{show}");
    test_env().remove("CADENCE_PM_DIR");
}

/// `agent set` cannot shed the boundary: `confine` is a posture param
/// — an agent caller gets refused; only operator/PM may change it, and
/// only for the next launch.
#[test]
fn confine_is_posture_not_self_service() {
    assert_eq!(
        cadence_agent::adapter::registry::param_class("confine"),
        cadence_agent::adapter::registry::ParamClass::Posture
    );
    // …and it is never a live mutation — Landlock applies at exec.
    let d = TestDaemon::start();
    let _pi = d.mock_pi("normal");
    d.register_pi("wpos", json!({}));
    let err = d
        .fixture_rpc(
            "agent_set",
            json!({"alias": "wpos", "patch": {"confine": true}}),
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("confine"),
        "live confine set must be refused: {err}"
    );
}

/// `join … pi --confine` end to end: the flag lands on the stored
/// params, `agent show` reports the worker confined, and on a Landlock
/// host the fake provider really launches through `cadence confine`
/// (its record proves it — the sibling write is denied, the fallback
/// inside its own dir lands). `join <pm> claude --confine` is refused
/// as a pi-only flag.
#[test]
fn join_pi_confine_end_to_end() {
    let _env = confine_env();
    let d = TestDaemon::start();
    let _pi = d.mock_pi("normal");
    d.register_inbox("pm");
    let bin = env!("CARGO_BIN_EXE_cadence");
    let run = |args: &[&str]| {
        std::process::Command::new(bin)
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .operator_output()
            .unwrap()
    };

    // Non-pi providers refuse the flag, not drop it.
    let out = run(&[
        "join",
        "pm",
        "claude",
        "--alias",
        "wx",
        "--confine",
        "--detach",
    ]);
    assert!(
        !out.status.success(),
        "claude --confine must refuse: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // The worker's cwd must not be the state dir (where `agents/`
    // legitimately lives) — give it a disjoint checkout so the denied
    // sibling-write leg below is real.
    let cwd = d.dir.path().join("checkout");
    std::fs::create_dir_all(&cwd).unwrap();
    let landlocked = cadence_agent::confine::available().is_ok();
    let out = run(&[
        "join",
        "pm",
        "pi",
        "--alias",
        "wj",
        "--confine",
        "--detach",
        "--no-bootstrap",
        "--cwd",
        cwd.to_str().unwrap(),
    ]);
    if landlocked {
        assert!(
            out.status.success(),
            "join --confine: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        d.wait_agent("wj", "idle", 20);
        let show = d.rpc("agent_show", json!({"alias": "wj"})).unwrap();
        assert_eq!(show["agent"]["params"]["confine"], true, "{show}");
        assert_eq!(show["agent"]["confined"], true, "{show}");
        assert!(
            d.state.join("agents/wj/pi-record.json").exists(),
            "confined fake-pi's record never landed"
        );
        assert!(
            !d.state.join("agents/pi-record-wj.json").exists(),
            "the sibling record write should have been denied"
        );
    } else {
        // No Landlock: the opt-in refuses up front, never launches
        // unconfined.
        assert!(!out.status.success(), "--confine must refuse here");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("confine"), "{err}");
    }
}
