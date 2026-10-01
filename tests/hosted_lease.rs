//! CAD-538: hosted lifecycle — the start-time lease acquire, heartbeat
//! renewal, self-fence on lease loss, and the SIGTERM flush, run end to
//! end against the `file:` provider. The file provider is the local
//! double for the company-DO lease (AOS-55): mutual exclusion and
//! expiry takeover are real, only the transport is fake.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use cadence_agent::daemon;
use cadence_agent::issue::Pm;
use cadence_agent::lease::Hosted;
use cadence_agent::platform::deployments::DeploymentMetadata;
use serde_json::json;
use serde_json::Value;
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use tempfile::TempDir;

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

fn lease_file(dir: &Path) -> PathBuf {
    dir.join("lease.json")
}

/// Overwrite the lease file the way a real contender's critical
/// section does: under `flock(LOCK_EX)` on `lease.json.lock`, then
/// tmp + fsync + rename. A bare `fs::write` races the heartbeat — a
/// renewal already inside its lock can overwrite the steal, so the
/// fence the test waits for never trips and the teardown parks
/// (CAD-991). The provider's renew also takes this lock, so the steal
/// can only land between critical sections — never interleaved with
/// one.
fn steal_lease(dir: &Path) {
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("lease.json.lock"))
        .unwrap();
    // LOCK_NB retried under a deadline — a wedged holder must fail
    // this test, not hang it (the provider's own renew bounds its
    // wait the same way).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "lease.json.lock stayed held past the steal deadline"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let path = lease_file(dir);
    let body: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let stolen = json!({"holder": "intruder",
                        "epoch": body["epoch"].as_u64().unwrap() + 5,
                        "expires_unix": now_unix() + 600.0});
    let tmp = dir.join(format!(".lease-steal.tmp.{}", std::process::id()));
    std::fs::write(&tmp, stolen.to_string()).unwrap();
    File::open(&tmp).unwrap().sync_all().unwrap();
    std::fs::rename(&tmp, &path).unwrap();
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
}

/// A `file:` lease under `dir` — `renew` 1s keeps the heartbeat fast so
/// a stolen lease is detected inside the test's poll budget.
fn lease_spec(dir: &Path, ttl_secs: u64) -> Hosted {
    Hosted {
        lease: Some(format!("file:{}", lease_file(dir).display())),
        lease_ttl_secs: Some(ttl_secs),
        lease_renew_secs: Some(1),
        flush_timeout_secs: Some(15),
    }
}

fn leased_opts(dir: &Path, ttl_secs: u64) -> daemon::ServeOptions {
    let mut opts = daemon_opts();
    opts.lease = Some(lease_spec(dir, ttl_secs));
    opts
}

/// A `Pm::init`'d tracker whose pm.yaml carries the `hosted:` table —
/// the config a real `daemon run` process resolves its lease from.
fn hosted_pm(dir: &Path, ttl_secs: u64, renew_secs: u64) -> PathBuf {
    let pm_dir = dir.join("pm");
    let pm = Pm::init(&pm_dir).unwrap();
    let yaml = pm_dir.join("pm.yaml");
    let mut text = std::fs::read_to_string(&yaml).unwrap();
    text.push_str(&format!(
        "\nhosted:\n  lease: file:{}\n  lease_ttl_secs: {ttl_secs}\n  \
         lease_renew_secs: {renew_secs}\n  flush_timeout_secs: 15\n",
        lease_file(dir).display()
    ));
    std::fs::write(&yaml, &text).unwrap();
    // Committed — the stop-time flush deliberately never sweeps
    // worktree dirt, so the fixture must not leave any.
    pm.commit(&[yaml], "hosted config\n\nActor: test\n")
        .unwrap();
    pm_dir
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Two daemons, one lease: while `a` holds it `b` refuses at acquire —
/// before its store opens. After `a` releases on a clean stop, `b`
/// takes the lease and writes.
#[test]
fn cad538_second_daemon_refused_while_lease_held() {
    let dir = TempDir::new().unwrap();
    let a = TestDaemon::start_opts(leased_opts(dir.path(), 30));
    a.register("w1");
    let health = a.rpc("health", json!({})).unwrap();
    assert_eq!(health["lease"]["epoch"], 1, "{health}");
    assert!(
        health["lease"]["provider"]
            .as_str()
            .unwrap_or_default()
            .starts_with("file:"),
        "{health}"
    );
    assert!(health["lease"]["fenced"].is_null(), "{health}");

    // The contender's `serve` fails at the lease, before its store
    // opens: not even an empty cadence.sqlite3 appears.
    let state_b = TempDir::new().unwrap();
    let err = daemon::serve_with(state_b.path(), leased_opts(dir.path(), 30)).unwrap_err();
    assert!(err.to_string().contains("held by"), "{err}");
    assert!(!state_b.path().join("cadence.sqlite3").exists());

    // A clean stop releases; the next daemon takes the lease and writes.
    drop(a);
    let b = TestDaemon::start_on_opts(state_b.path().to_path_buf(), leased_opts(dir.path(), 30));
    b.register("w2");
}

/// Losing the lease mid-run trips the fence: the next store write and
/// the next tracker write are refused, nothing partially lands, reads
/// still answer.
#[test]
fn cad538_lease_loss_fences_every_write() {
    let dir = TempDir::new().unwrap();
    let pm_dir = dir.path().join("pm");
    Pm::init(&pm_dir).unwrap();
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start_opts(leased_opts(dir.path(), 30));
    d.register("w1");
    d.wait_agent("w1", "idle", 15);

    // While the lease holds, a tracker write lands with the lease epoch
    // stamped on its commit.
    let out = d
        .operator_rpc(
            "agent_file_write",
            json!({"agent": "w1", "file": "SOUL.md", "text": "leased\n"}),
        )
        .unwrap();
    assert_eq!(out["changed"], true, "{out}");
    let log = git(&pm_dir, &["log", "-1", "--format=%B"]);
    assert!(log.contains("Lease-Epoch: 1"), "{log}");

    // A foreign holder steals the file — the heartbeat's next renewal
    // sees holder and epoch differ and trips the fence. The steal
    // takes the lease's own flock so it cannot race a renewal already
    // inside its critical section (CAD-991).
    steal_lease(dir.path());

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let h = d.rpc("health", json!({})).unwrap();
        if h["lease"]["fenced"].is_string() {
            break;
        }
        assert!(Instant::now() < deadline, "never fenced: {h}");
        thread::sleep(Duration::from_millis(100));
    }
    // The forensic record lands in the state dir — the daemon's own
    // file, since every leased store write is refused from here on.
    let fact: Value =
        serde_json::from_str(&std::fs::read_to_string(d.state.join("lease-fence.json")).unwrap())
            .unwrap();
    assert!(
        fact["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("renewal failed"),
        "{fact}"
    );

    // Store writes refuse — the proven operator, so the refusal can
    // only be the fence.
    let e = d
        .operator_rpc(
            "agent_send",
            json!({"alias": "w1", "text": "stolen", "message": "m-x"}),
        )
        .unwrap_err();
    assert!(e.to_string().contains("lease"), "{e}");
    let e = d
        .operator_rpc(
            "agent_register",
            json!({"alias": "w2", "provider": "fake",
                   "endpoint_kind": "fake", "cwd": "/tmp"}),
        )
        .unwrap_err();
    assert!(e.to_string().contains("lease"), "{e}");

    // Tracker writes refuse the same way — `agent_file_write` takes the
    // tracker lock first, so the refusal lands before a byte moves.
    let e = d
        .operator_rpc(
            "agent_file_write",
            json!({"agent": "w1", "file": "SOUL.md", "text": "stolen\n"}),
        )
        .unwrap_err();
    assert!(e.to_string().contains("lease"), "{e}");

    // No partial writes: w1's log never saw m-x, w2 never registered,
    // and the tracker's worktree/index are untouched.
    let msgs = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone();
    assert!(!msgs.iter().any(|m| m["id"] == "m-x"), "{msgs:?}");
    let agents = d.rpc("agent_list", json!({})).unwrap()["agents"]
        .as_array()
        .unwrap()
        .clone();
    assert!(!agents.iter().any(|a| a["alias"] == "w2"), "{agents:?}");
    assert_eq!(
        std::fs::read_to_string(pm_dir.join("agents/w1/SOUL.md")).unwrap(),
        "leased\n"
    );
    assert_eq!(git(&pm_dir, &["status", "--porcelain"]), "");

    // Reads still answer — a fenced daemon stays diagnosable.
    assert!(d.rpc("agent_list", json!({})).is_ok());
    assert!(d.rpc("agent_show", json!({"alias": "w1"})).is_ok());
}

/// A SIGKILLed holder's lease lapses; the replacement daemon takes over
/// at the next epoch, and the dead daemon's own restart is refused.
/// Real `daemon run` processes — the production shape.
#[test]
fn cad538_takeover_after_holder_death() {
    let dir = TempDir::new().unwrap();
    let pm_dir = hosted_pm(dir.path(), 2, 1);
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());

    let mut a = TestDaemon::start_process_in(TempDir::new().unwrap());
    let holder_a = a.rpc("health", json!({})).unwrap()["lease"]["holder"]
        .as_str()
        .unwrap()
        .to_string();
    a.register("w1");

    // Dead without release — the record outlives it until the TTL.
    a.process.as_mut().unwrap().kill().unwrap();
    a.process.as_mut().unwrap().wait().unwrap();
    thread::sleep(Duration::from_secs(4));

    // The replacement takes over at epoch + 1.
    let b = TestDaemon::start_process_in(TempDir::new().unwrap());
    let hb = b.rpc("health", json!({})).unwrap();
    assert_eq!(hb["lease"]["epoch"], 2, "{hb}");
    assert_ne!(hb["lease"]["holder"].as_str().unwrap(), holder_a);
    b.register("w2");

    // The dead daemon restarted on its own state dir is refused at the
    // lease — it can never write again.
    let revive = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .arg("--state-dir")
        .arg(&a.state)
        .args(["daemon", "run"])
        .env("HOME", a.dir.path().join("home"))
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_ROLLOUT_AS")
        .envs(test_env().vars())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        !revive.status.success(),
        "revived daemon started: {}",
        String::from_utf8_lossy(&revive.stderr)
    );
    assert!(
        String::from_utf8_lossy(&revive.stderr).contains("held by"),
        "{}",
        String::from_utf8_lossy(&revive.stderr)
    );
}

/// SIGTERM is the clean stop: the WAL folds back into the db, the
/// tracker's staged index commits under `cadence flush on stop` with
/// the lease epoch stamped, and the lease releases last — all inside
/// the flush budget.
#[test]
fn cad538_sigterm_flushes_store_and_tracker() {
    let dir = TempDir::new().unwrap();
    let pm_dir = hosted_pm(dir.path(), 30, 1);
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let mut d = TestDaemon::start_process_in(TempDir::new().unwrap());
    d.register("w1");
    d.wait_agent("w1", "idle", 15);

    // The tracker's half of the flush: an index-staged but uncommitted
    // file — the residue of a write interrupted mid-flight.
    std::fs::write(pm_dir.join("pending.md"), "pending\n").unwrap();
    git(&pm_dir, &["add", "pending.md"]);

    let wal = d.state.join("cadence.sqlite3-wal");
    let pid = d.process.as_ref().unwrap().id();
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(40);
    let status = loop {
        if let Some(s) = d.process.as_mut().unwrap().try_wait().unwrap() {
            break s;
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not exit within the flush budget: {}",
            std::fs::read_to_string(d.dir.path().join("daemon-process.log")).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(100));
    };
    assert!(status.success(), "SIGTERM exit: {status}");

    // The WAL folded back — nothing durable lives only in the wal.
    let wal_len = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    assert_eq!(wal_len, 0, "WAL was not folded on SIGTERM");

    // The staged tracker file committed with the lease epoch stamped —
    // proof the daemon's `pm` handles carry the lease end to end.
    let log = git(&pm_dir, &["log", "-1", "--format=%B"]);
    assert!(log.contains("cadence flush on stop"), "{log}");
    assert!(log.contains("Lease-Epoch: 1"), "{log}");
    assert_eq!(git(&pm_dir, &["status", "--porcelain"]), "");
    let shown = git(&pm_dir, &["show", "--name-only", "--format=", "HEAD"]);
    assert!(shown.lines().any(|l| l == "pending.md"), "{shown}");

    // The lease is released — an expired marker the successor takes
    // over at once (epochs stay monotone across holders).
    let body: Value =
        serde_json::from_str(&std::fs::read_to_string(lease_file(dir.path())).unwrap()).unwrap();
    assert!(
        body["expires_unix"].as_f64().unwrap_or(1.0) <= now_unix(),
        "{body}"
    );

    // Nothing written before the signal was lost: a fresh daemon on the
    // same state still sees the agent — and re-acquires at epoch 2 (the
    // recorded hint keeps epochs monotone even after a clean release).
    let d2 = TestDaemon::start_on_opts(d.state.clone(), leased_opts(dir.path(), 30));
    let agents = d2.rpc("agent_list", json!({})).unwrap()["agents"]
        .as_array()
        .unwrap()
        .clone();
    assert!(agents.iter().any(|a| a["alias"] == "w1"), "{agents:?}");
    let h2 = d2.rpc("health", json!({})).unwrap();
    assert_eq!(h2["lease"]["epoch"], 2, "{h2}");
}

/// CAD-702 changed the premise: the heartbeat no longer dies at
/// `closing` — it renews through the flush — so a parked tail cannot
/// outrun the TTL by waiting anymore. Lease loss mid-shutdown is now a
/// stolen lease: the still-live heartbeat's next renewal sees holder
/// and epoch differ and trips the fence, and the tracker flush refuses
/// exactly as before — a successor may already hold the lease. (Renamed
/// from `cad538_expired_lease_refuses_the_shutdown_flush`, whose
/// wait-past-the-TTL shape the new ordering deliberately closes.)
#[test]
fn cad538_stolen_lease_refuses_the_shutdown_flush() {
    let dir = TempDir::new().unwrap();
    let pm_dir = dir.path().join("pm");
    Pm::init(&pm_dir).unwrap();
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(Barrier::new(2));
    let mut opts = leased_opts(dir.path(), 30);
    opts.stop = Some(stop.clone());
    opts.release_shutdown_snapshot = Some(gate.clone());
    opts.provider_deployments =
        Some(DeploymentMetadata::parse(br#"{"schema":1,"providers":[]}"#).unwrap());
    let d = TestDaemon::start_opts(opts);
    d.register("w1");

    // Work a flush would commit — index-staged, uncommitted.
    std::fs::write(pm_dir.join("pending.md"), "pending\n").unwrap();
    git(&pm_dir, &["add", "pending.md"]);

    // A foreign holder steals the file while the daemon runs — the
    // live heartbeat's next renewal sees holder and epoch differ and
    // trips the fence. The steal takes the lease's own flock so it
    // cannot race a renewal already inside its critical section
    // (CAD-991). The guard is armed before the steal: it drops before
    // `d` (locals drop in reverse declaration order), so on a panic
    // in the steal or the poll it signals `stop` itself — the
    // daemon's drop has not run yet — and releases the snapshot
    // barrier from a detached thread, so teardown can never wait on a
    // partnerless gate (CAD-991: an 18-minute hang seen in CI). The
    // poll must happen BEFORE the explicit stop below: once `closing`
    // is set serve stops accepting connections, so an RPC issued
    // while the tail is parked at the gate would hang on the 700s
    // client timeout and deadlock the rendezvous.
    let mut guard = GateGuard::armed(&gate, &stop);
    steal_lease(dir.path());
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let h = d.rpc("health", json!({})).unwrap();
        if h["lease"]["fenced"].is_string() {
            break;
        }
        assert!(Instant::now() < deadline, "never fenced: {h}");
        thread::sleep(Duration::from_millis(100));
    }

    // Stop; the fenced tail parks at the gate, the test releases it,
    // and the tracker flush must refuse: a successor may already hold
    // the lease.
    stop.store(true, Ordering::SeqCst);
    gate.wait();
    guard.disarm();

    // Clean exit — but no commit landed past the lease's loss.
    drop(d);
    let log = git(&pm_dir, &["log", "--format=%B", "-3"]);
    assert!(
        !log.contains("cadence flush on stop"),
        "flush committed after the lease was stolen: {log}"
    );
    // The staged file was never claimed — still staged for the
    // successor's operator to judge, not swept into our epoch.
    assert_eq!(git(&pm_dir, &["status", "--porcelain"]), "A  pending.md");
}

/// A wedged lease lock — a SIGSTOPed contender or a hung filesystem —
/// cannot park shutdown: `flock` retries boundedly, the failed renewal
/// fences the daemon, and SIGTERM still exits inside the budget.
#[test]
fn cad538_wedged_lease_lock_bounds_shutdown() {
    let dir = TempDir::new().unwrap();
    let pm_dir = hosted_pm(dir.path(), 30, 1);
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let mut d = TestDaemon::start_process_in(TempDir::new().unwrap());
    d.register("w1");
    d.wait_agent("w1", "idle", 15);

    std::fs::write(pm_dir.join("pending.md"), "pending\n").unwrap();
    git(&pm_dir, &["add", "pending.md"]);

    // Hold the flock on a separate open file description — exactly
    // what a wedged holder looks like to the daemon's renew.
    let wedged = File::options()
        .read(true)
        .write(true)
        .open(dir.path().join("lease.json.lock"))
        .unwrap();
    assert_eq!(unsafe { libc::flock(wedged.as_raw_fd(), libc::LOCK_EX) }, 0);

    // The heartbeat's next renew (≤1s) burns its bounded retry (~2s)
    // then fails closed — the daemon fences itself.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let h = d.rpc("health", json!({})).unwrap();
        if h["lease"]["fenced"].is_string() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "never fenced under the wedge: {h}"
        );
        thread::sleep(Duration::from_millis(100));
    }
    assert!(d.rpc("health", json!({})).unwrap()["lease"]["fenced"]
        .as_str()
        .unwrap()
        .contains("renewal failed"));

    // SIGTERM while the lock stays wedged: join is bounded by the
    // flock budget, flush refuses on the tripped fence, release fails
    // closed — the process still exits cleanly.
    let pid = d.process.as_ref().unwrap().id();
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(s) = d.process.as_mut().unwrap().try_wait().unwrap() {
            break s;
        }
        assert!(
            Instant::now() < deadline,
            "daemon hung on the wedged lease lock: {}",
            std::fs::read_to_string(d.dir.path().join("daemon-process.log")).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(100));
    };
    assert!(status.success(), "SIGTERM under wedge: {status}");
    drop(wedged);

    // Fenced before the signal — the flush refused, the staged file
    // was never committed into an epoch the daemon no longer owned.
    let log = git(&pm_dir, &["log", "--format=%B", "-3"]);
    assert!(!log.contains("cadence flush on stop"), "{log}");
    assert_eq!(git(&pm_dir, &["status", "--porcelain"]), "A  pending.md");
}

/// CAD-694 (review B3): a start that fails AFTER acquiring the lease —
/// here an injected relaunch fault on the exact flagged path — must not
/// release the lease while the shutdown flush is still unproven: a
/// successor taking over would share the live writer. Park the flush at
/// the gate, let the tail's injected budget lapse, and the lease must
/// stay held; the parked worker then completes into our own still-valid
/// TTL window, never a successor's epoch.
#[test]
fn cad694_failed_start_withholds_lease_while_flush_unproven() {
    suite_slot();
    let dir = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let gate = Arc::new(Barrier::new(2));
    let mut opts = leased_opts(dir.path(), 30);
    opts.relaunch_fault_for_test = Some(Arc::new(AtomicBool::new(true)));
    opts.flush_gate_for_test = Some({
        let gate = gate.clone();
        Arc::new(move || {
            gate.wait();
        })
    });
    opts.flush_budget_for_test = Some(Duration::from_millis(50));
    // The worker's completion receipt: the test owns the late
    // completion instead of racing it, and keeps its fixture dirs alive
    // until the worker is done touching them.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let done_tx = Mutex::new(done_tx);
    opts.flush_done_for_test = Some(Arc::new(move || {
        let _ = done_tx.lock().unwrap().send(());
    }));
    // Same-shape fixture daemon as the cad702 test: a hosted lease
    // triggers the deployment lookup, which must stay hermetic.
    opts.provider_deployments =
        Some(DeploymentMetadata::parse(br#"{"schema":1,"providers":[]}"#).unwrap());
    let state_dir = state.path().to_path_buf();
    let handle = thread::spawn(move || daemon::serve_with(&state_dir, opts));
    let exit = handle.join().unwrap();
    let err = exit.expect_err("the injected relaunch fault exits serve");
    assert!(
        err.to_string().contains("injected relaunch fault"),
        "serve must fail on the injected relaunch fault, not another error: {err}"
    );
    // The flush never completed inside the (tiny) budget — the tail
    // withheld release, so the file lease is still ours until TTL.
    let body: Value =
        serde_json::from_str(&std::fs::read_to_string(lease_file(dir.path())).unwrap()).unwrap();
    assert!(
        body["expires_unix"].as_f64().unwrap_or(0.0) > now_unix(),
        "released under a live flush: {body}"
    );
    // Let the parked worker finish — release stays withheld; the lease
    // transfers by TTL, the fence-bounded window a late write is
    // confined to.
    gate.wait();
    done_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the parked flush worker never completed");
    let body: Value =
        serde_json::from_str(&std::fs::read_to_string(lease_file(dir.path())).unwrap()).unwrap();
    assert!(
        body["expires_unix"].as_f64().unwrap_or(0.0) > now_unix(),
        "a late flush completion must not release retroactively: {body}"
    );
}

/// True while this state dir's heartbeat thread (`lh-` + the last 12
/// chars of the dir name — see `LeaseHeartbeat::thread_name`) lives.
fn heartbeat_thread_alive(state_dir: &Path) -> bool {
    let leaf = state_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let tail: String = leaf
        .chars()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let want = format!("lh-{tail}");
    std::fs::read_dir("/proc/self/task")
        .into_iter()
        .flatten()
        .flatten()
        .any(|task| {
            std::fs::read_to_string(task.path().join("comm")).is_ok_and(|name| name.trim() == want)
        })
}

/// CAD-694 x CAD-947: the CAD-947 heartbeat runs from just after acquire,
/// so a start that fails after acquiring the lease (relaunch fault) has a
/// live renewal poster. The exit tail must flush, then stop AND JOIN the
/// poster, and only then release — never a renewal racing the release
/// (CAD-702: it would rewrite a removed lease or trip a spurious fence).
/// The poster sleeps in 100ms steps, so a tail that releases first leaves
/// it alive for up to 100ms after the release is visible; a 1ms watcher
/// catches that. Passing no heartbeat to the tail fails this test.
#[test]
fn cad694_failed_start_stops_heartbeat_before_release() {
    suite_slot();
    let dir = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let mut opts = leased_opts(dir.path(), 30);
    opts.relaunch_fault_for_test = Some(Arc::new(AtomicBool::new(true)));
    opts.provider_deployments =
        Some(DeploymentMetadata::parse(br#"{"schema":1,"providers":[]}"#).unwrap());
    let state_dir = state.path().to_path_buf();
    let lease_path = lease_file(dir.path());
    let watched_state = state.path().to_path_buf();
    let done = Arc::new(AtomicBool::new(false));
    // Records, at the first instant the release is visible (expiry
    // zeroed), whether the poster was still alive. `None` = never seen.
    let watcher = {
        let done = Arc::clone(&done);
        thread::spawn(move || {
            let mut seen_alive = false;
            let mut released = false;
            while !done.load(Ordering::SeqCst) {
                if let Ok(text) = std::fs::read_to_string(&lease_path) {
                    if let Ok(body) = serde_json::from_str::<Value>(&text) {
                        if body["expires_unix"].as_f64() == Some(0.0) {
                            released = true;
                            seen_alive |= heartbeat_thread_alive(&watched_state);
                        }
                    }
                }
                thread::sleep(Duration::from_millis(1));
            }
            (released, seen_alive)
        })
    };
    let handle = thread::spawn(move || daemon::serve_with(&state_dir, opts));
    let err = handle
        .join()
        .unwrap()
        .expect_err("the injected relaunch fault exits serve");
    assert!(
        err.to_string().contains("injected relaunch fault"),
        "serve must fail on the injected relaunch fault: {err}"
    );
    // Outlive two renew periods: a surviving poster would rewrite the
    // lease or trip the fence by now.
    thread::sleep(Duration::from_millis(2500));
    done.store(true, Ordering::SeqCst);
    let (released, seen_alive) = watcher.join().unwrap();
    assert!(released, "the failed start never released the lease");
    assert!(
        !seen_alive,
        "the heartbeat was still alive when the lease released: a renewal could race the release"
    );
    assert!(
        lease_expiry(dir.path()) == 0.0,
        "a renewal rewrote the released lease"
    );
    assert!(
        !state.path().join("lease-fence.json").exists(),
        "a late renewal tripped a spurious fence after release"
    );
}

/// Installs a pre-commit hook that parks the flush commit INSIDE git —
/// reached only after the fence admitted the write. `trap_term` makes it
/// ignore SIGTERM, like a hook that outlives its commit. Returns the
/// directory holding its `entered`, `pid` and `release` marks.
fn park_commit_hook(pm_dir: &Path, trap_term: bool) -> TempDir {
    use std::os::unix::fs::PermissionsExt;
    let marks = TempDir::new().unwrap();
    let hook = pm_dir.join(".git/hooks/pre-commit");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\n{trap}echo $$ > {m}/pid\n: > {m}/entered\n\
             n=0\nwhile [ ! -f {m}/release ] && [ $n -lt 600 ]; do sleep 0.05; n=$((n+1)); done\n",
            m = marks.path().display(),
            trap = if trap_term { "trap '' TERM\n" } else { "" },
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    marks
}

fn wait_entered(marks: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !marks.join("entered").exists() {
        assert!(
            Instant::now() < deadline,
            "the flush never reached its commit — the test parked nothing"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn pid_alive(marks: &Path) -> bool {
    let pid: i32 = std::fs::read_to_string(marks.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal 0 only probes for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// CAD-694 (r4 B3): withholding the lease release is not enough — a
/// `git commit` admitted before the budget lapsed must not outlive it,
/// or it lands after the lease transfers by TTL, into a successor's
/// epoch. The writer is parked INSIDE the commit; the budget lapses;
/// serve must return with that commit terminated and reaped, and
/// releasing the hook afterwards must land nothing.
#[test]
fn cad694_budget_lapse_terminates_a_parked_flush_commit() {
    suite_slot();
    let dir = TempDir::new().unwrap();
    let pm_dir = hosted_pm(dir.path(), 30, 1);
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let marks = park_commit_hook(&pm_dir, false);
    let stop = Arc::new(AtomicBool::new(false));
    let mut opts = leased_opts(dir.path(), 30);
    opts.stop = Some(stop.clone());
    opts.flush_budget_for_test = Some(Duration::from_millis(2000));
    opts.provider_deployments =
        Some(DeploymentMetadata::parse(br#"{"schema":1,"providers":[]}"#).unwrap());
    let d = TestDaemon::start_opts(opts);
    std::fs::write(pm_dir.join("pending.md"), "pending\n").unwrap();
    git(&pm_dir, &["add", "pending.md"]);
    stop.store(true, Ordering::SeqCst);
    drop(d); // serve returns after the flush tail: budget lapse, cancel
    wait_entered(marks.path());
    assert!(
        !pid_alive(marks.path()),
        "the parked commit's hook outlived the budget lapse"
    );
    std::fs::write(marks.path().join("release"), "").unwrap();
    thread::sleep(Duration::from_millis(500));
    let log = git(&pm_dir, &["log", "--format=%B", "-3"]);
    assert!(!log.contains("cadence flush on stop"), "{log}");
    assert_eq!(git(&pm_dir, &["status", "--porcelain"]), "A  pending.md");
}

/// CAD-694 (r4 B3, review 2): a hook that traps SIGTERM survives git
/// and holds the pipes. The cancel must still kill the whole group and
/// let the worker drop the tracker lock — a stale `.write.lock` after
/// the daemon exits blocks every later tracker writer (CAD-852).
#[test]
fn cad694_cancel_kills_a_term_trapping_hook_and_frees_the_tracker_lock() {
    suite_slot();
    let dir = TempDir::new().unwrap();
    let pm_dir = hosted_pm(dir.path(), 30, 1);
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let marks = park_commit_hook(&pm_dir, true);
    let stop = Arc::new(AtomicBool::new(false));
    let mut opts = leased_opts(dir.path(), 30);
    opts.stop = Some(stop.clone());
    opts.flush_budget_for_test = Some(Duration::from_millis(2000));
    opts.provider_deployments =
        Some(DeploymentMetadata::parse(br#"{"schema":1,"providers":[]}"#).unwrap());
    let d = TestDaemon::start_opts(opts);
    std::fs::write(pm_dir.join("pending.md"), "pending\n").unwrap();
    git(&pm_dir, &["add", "pending.md"]);
    stop.store(true, Ordering::SeqCst);
    drop(d);
    wait_entered(marks.path());
    assert!(
        !pid_alive(marks.path()),
        "a TERM-trapping hook outlived the cancel"
    );
    assert!(
        !pm_dir.join(".write.lock").exists(),
        "the cancelled flush left the tracker lock behind"
    );
}

/// CAD-694 (r4 B3, review 2): the contract's proof. The commit is
/// admitted under a live lease and parks inside git; renewal then fails
/// closed, the TTL lapses and a REAL successor acquires epoch 2. Neither
/// the flush budget (held far open here) nor an explicit cancel fires —
/// the commit's own owner must notice the lease slipping and end it, or
/// the old daemon's `Lease-Epoch: 1` commit lands in the successor's
/// epoch once the hook is released.
#[test]
fn cad694_flush_commit_cannot_land_after_a_successor_acquires() {
    use cadence_agent::lease::{FileProvider, Provider};
    suite_slot();
    let dir = TempDir::new().unwrap();
    // TTL 10 / renew 1: validity stays near 10s, far above the 2s commit
    // floor even when renewals lag on a loaded host.
    let pm_dir = hosted_pm(dir.path(), 10, 1);
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let marks = park_commit_hook(&pm_dir, false);
    let stop = Arc::new(AtomicBool::new(false));
    let mut opts = leased_opts(dir.path(), 10);
    opts.stop = Some(stop.clone());
    opts.flush_budget_for_test = Some(Duration::from_secs(120));
    opts.provider_deployments =
        Some(DeploymentMetadata::parse(br#"{"schema":1,"providers":[]}"#).unwrap());
    let d = TestDaemon::start_opts(opts);
    std::fs::write(pm_dir.join("pending.md"), "pending\n").unwrap();
    git(&pm_dir, &["add", "pending.md"]);
    // Request the stop only while the lease has ample validity, so the
    // commit is admitted before any floor could matter.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let body: Value =
            serde_json::from_str(&std::fs::read_to_string(lease_file(dir.path())).unwrap())
                .unwrap();
        if body["expires_unix"].as_f64().unwrap_or(0.0) - now_unix() > 3.5 {
            break;
        }
        assert!(Instant::now() < deadline, "the lease never showed headroom");
        thread::sleep(Duration::from_millis(10));
    }
    stop.store(true, Ordering::SeqCst);
    wait_entered(marks.path());
    // Wedge renewal: the heartbeat fails closed and the lease lapses.
    let wedged = File::options()
        .read(true)
        .write(true)
        .open(dir.path().join("lease.json.lock"))
        .unwrap();
    assert_eq!(unsafe { libc::flock(wedged.as_raw_fd(), libc::LOCK_EX) }, 0);
    thread::sleep(Duration::from_secs(11));
    drop(wedged);
    let successor = FileProvider::new(lease_file(dir.path()), Duration::from_secs(10))
        .acquire("successor", 0)
        .expect("the lapsed lease is takeable");
    assert!(successor.epoch.unwrap_or(0) >= 2, "{successor:?}");
    // The parked commit is released only now — after the takeover.
    std::fs::write(marks.path().join("release"), "").unwrap();
    drop(d);
    thread::sleep(Duration::from_millis(300));
    let log = git(&pm_dir, &["log", "--format=%B", "-3"]);
    assert!(
        !log.contains("cadence flush on stop"),
        "a flush commit landed after the successor took the lease: {log}"
    );
    assert!(!pid_alive(marks.path()), "the commit's hook is still alive");
    assert_eq!(git(&pm_dir, &["status", "--porcelain"]), "A  pending.md");
}

/// CAD-694: a lease with a short MARGIN (TTL 6s, renew 5s: one missed
/// beat leaves 1s) must still get its stop flush. The commit floor
/// derives from that margin (500ms); a fixed 2s floor would refuse every
/// commit made in the last 2s before a renewal (CAD-538 regression).
/// The TTL leaves startup room (the heartbeat starts after recovery);
/// the stop is requested only once the lease is inside that last 2s
/// window, so the short-margin floor path is what decides the commit.
#[test]
fn cad694_short_margin_lease_still_flushes_on_stop() {
    suite_slot();
    let dir = TempDir::new().unwrap();
    let pm_dir = hosted_pm(dir.path(), 6, 5);
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let mut opts = daemon_opts();
    opts.lease = Some(Hosted {
        lease: Some(format!("file:{}", lease_file(dir.path()).display())),
        lease_ttl_secs: Some(6),
        lease_renew_secs: Some(5),
        flush_timeout_secs: Some(15),
    });
    opts.stop = Some(stop.clone());
    opts.provider_deployments =
        Some(DeploymentMetadata::parse(br#"{"schema":1,"providers":[]}"#).unwrap());
    let d = TestDaemon::start_opts(opts);
    std::fs::write(pm_dir.join("pending.md"), "pending\n").unwrap();
    git(&pm_dir, &["add", "pending.md"]);
    // Wait for the window where validity is under the fixed 2s floor but
    // still well above the derived 500ms one.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let body: Value =
            serde_json::from_str(&std::fs::read_to_string(lease_file(dir.path())).unwrap())
                .unwrap();
        let left = body["expires_unix"].as_f64().unwrap_or(0.0) - now_unix();
        if (1.2..1.8).contains(&left) {
            break;
        }
        assert!(Instant::now() < deadline, "never saw the short window");
        thread::sleep(Duration::from_millis(10));
    }
    stop.store(true, Ordering::SeqCst);
    drop(d);
    let log = git(&pm_dir, &["log", "-1", "--format=%B"]);
    assert!(log.contains("cadence flush on stop"), "{log}");
    assert_eq!(git(&pm_dir, &["status", "--porcelain"]), "");
}

/// CAD-694: the crash path. A daemon SIGKILLed mid-flush leaves its
/// `git commit` child (own process group, out of reach of group
/// signals) running; it would land `cadence flush on stop` with the OLD
/// lease epoch long after the write was declared interrupted. The child
/// must die with its parent: SIGKILL the daemon while the commit is
/// parked in a hook, release the hook, and nothing may land.
#[test]
fn cad694_sigkilled_daemon_does_not_leave_an_orphan_commit() {
    let dir = TempDir::new().unwrap();
    let pm_dir = hosted_pm(dir.path(), 30, 1);
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let marks = park_commit_hook(&pm_dir, false);
    let mut d = TestDaemon::start_process_in(TempDir::new().unwrap());
    d.register("w1");
    d.wait_agent("w1", "idle", 15);
    std::fs::write(pm_dir.join("pending.md"), "pending\n").unwrap();
    git(&pm_dir, &["add", "pending.md"]);
    let pid = d.process.as_ref().unwrap().id() as i32;
    unsafe { libc::kill(pid, libc::SIGTERM) };
    wait_entered(marks.path());
    unsafe { libc::kill(pid, libc::SIGKILL) };
    let _ = d.process.as_mut().unwrap().wait();
    // The hook is released only after the daemon is dead.
    std::fs::write(marks.path().join("release"), "").unwrap();
    thread::sleep(Duration::from_secs(2));
    let log = git(&pm_dir, &["log", "--format=%B", "-3"]);
    assert!(
        !log.contains("cadence flush on stop"),
        "an orphaned flush commit landed after its daemon was killed: {log}"
    );
    assert_eq!(git(&pm_dir, &["status", "--porcelain"]), "A  pending.md");
}

/// CAD-694 (r4 B5): the exit decision itself. A real, non-retryable
/// drain fault followed by the lease lapsing (renewal wedged past the
/// TTL) before the verdict is still a fault: serve must exit `Err`. A
/// carve-out that re-reads the fence after the fact would call it the
/// fence's own consequence and exit clean. (The CAD-538 control — a
/// drain the already-tripped fence refuses at the write exits clean —
/// is `cad538_wedged_lease_lock_bounds_shutdown`.)
#[test]
fn cad694_real_drain_fault_exits_err_even_when_the_lease_lapses_after() {
    suite_slot();
    let dir = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let entered_tx = Mutex::new(entered_tx);
    let go_rx = Mutex::new(go_rx);
    // A roomy lease (TTL 6s, renew 2s): it cannot lapse on its own
    // before shutdown reaches the drain, even on a loaded host.
    let mut opts = daemon_opts();
    opts.lease = Some(Hosted {
        lease: Some(format!("file:{}", lease_file(dir.path()).display())),
        lease_ttl_secs: Some(6),
        lease_renew_secs: Some(2),
        flush_timeout_secs: Some(15),
    });
    opts.stop = Some(Arc::new(AtomicBool::new(true)));
    opts.shutdown_entries_hook = Some(Arc::new(move |_conn| {
        let _ = entered_tx.lock().unwrap().send(());
        let _ = go_rx.lock().unwrap().recv_timeout(Duration::from_secs(60));
        Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
            Some("injected disk full".to_string()),
        ))
    }));
    opts.provider_deployments =
        Some(DeploymentMetadata::parse(br#"{"schema":1,"providers":[]}"#).unwrap());
    let state_dir = state.path().to_path_buf();
    let handle = thread::spawn(move || daemon::serve_with(&state_dir, opts));
    match entered_rx.recv_timeout(Duration::from_secs(60)) {
        Ok(()) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("serve ended before the drain ran: {:?}", handle.join())
        }
        Err(e) => panic!("the drain never ran: {e}"),
    }
    // The drain's write already passed the fence. Wedge renewal, then
    // wait — explicitly — until the lease on disk has lapsed.
    let wedged = File::options()
        .read(true)
        .write(true)
        .open(dir.path().join("lease.json.lock"))
        .unwrap();
    assert_eq!(unsafe { libc::flock(wedged.as_raw_fd(), libc::LOCK_EX) }, 0);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let body: Value =
            serde_json::from_str(&std::fs::read_to_string(lease_file(dir.path())).unwrap())
                .unwrap();
        if body["expires_unix"].as_f64().unwrap_or(f64::MAX) < now_unix() {
            break;
        }
        assert!(Instant::now() < deadline, "the wedged lease never lapsed");
        thread::sleep(Duration::from_millis(100));
    }
    thread::sleep(Duration::from_millis(200));
    go_tx.send(()).unwrap();
    let exit = handle.join().unwrap();
    drop(wedged);
    let err = exit.expect_err("a real drain fault must not exit clean");
    assert!(err.to_string().contains("shutdown entries failed"), "{err}");
}

/// `hosted.lease` off leaves startup untouched: no lease file, no
/// `lease` in health. `daemon_opts` pins `Hosted::default()` —
/// explicitly off, so a real `hosted:` table on this host cannot leak
/// in.
#[test]
fn cad538_unleased_daemon_unchanged() {
    let dir = TempDir::new().unwrap();
    let d = TestDaemon::start();
    d.register("w1");
    assert!(d.rpc("health", json!({})).unwrap()["lease"].is_null());
    assert!(!lease_file(dir.path()).exists());
}

/// Same, through the config path: `opts.lease = None` + no `hosted:`
/// table in pm.yaml resolves to off.
#[test]
fn cad538_hosted_config_off_by_default() {
    let dir = TempDir::new().unwrap();
    let pm_dir = dir.path().join("pm");
    Pm::init(&pm_dir).unwrap();
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let mut opts = daemon_opts();
    opts.lease = None; // resolve from pm.yaml — which has no `hosted:`
    let d = TestDaemon::start_opts(opts);
    assert!(d.rpc("health", json!({})).unwrap()["lease"].is_null());
    // A malformed `hosted` table fails closed — never unleased on a typo.
    std::fs::write(
        pm_dir.join("pm.yaml"),
        "schema: 1\nhosted:\n  lease: bogouscheme://x\n",
    )
    .unwrap();
    let state = TempDir::new().unwrap();
    let mut opts = daemon_opts();
    opts.lease = None;
    let err = daemon::serve_with(state.path(), opts).unwrap_err();
    assert!(err.to_string().contains("bogouscheme"), "{err}");
}

/// Releases the shutdown gate if the test unwinds before reaching the
/// gate itself — otherwise serve parks at the gate forever and the
/// shutdown join hangs (CAD-991). `GateGuard` must be declared AFTER
/// the `TestDaemon` it protects, so on an unwind it drops first
/// (locals drop in reverse declaration order): it signals `stop`
/// itself — the daemon's own drop has not run yet — then releases
/// the barrier from a detached thread, so the guard itself can never
/// park the test even if serve is wedged before the gate. Disarmed
/// once the test rendezvoused normally.
struct GateGuard {
    gate: Option<Arc<Barrier>>,
    stop: Arc<AtomicBool>,
}

impl GateGuard {
    fn armed(gate: &Arc<Barrier>, stop: &Arc<AtomicBool>) -> Self {
        Self {
            gate: Some(Arc::clone(gate)),
            stop: Arc::clone(stop),
        }
    }

    fn disarm(&mut self) {
        self.gate.take();
    }
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        if let Some(gate) = self.gate.take() {
            // The daemon's own stop flag is still unset — serve would
            // never reach the barrier. Ask for the stop here; the
            // releaser thread then rendezvouses whenever the tail
            // arrives, while the daemon's own drop (the bounded
            // `stop_and_join`) is what reports a wedge.
            self.stop.store(true, Ordering::SeqCst);
            thread::spawn(move || {
                gate.wait();
            });
        }
    }
}

/// An always-204 stub host that records every renewal POST's arrival
/// time — the witness for CAD-702's shutdown ordering.
struct StubHost {
    url: String,
    times: Arc<Mutex<Vec<Instant>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl StubHost {
    fn new() -> Self {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/renew", listener.local_addr().unwrap());
        let times = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&times);
        let stop = Arc::new(AtomicBool::new(false));
        let halted = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !halted.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut request = Vec::new();
                        let mut byte = [0];
                        while socket.read(&mut byte).unwrap_or(0) == 1 {
                            request.push(byte[0]);
                            if request.ends_with(b"\r\n\r\n") {
                                break;
                            }
                            assert!(request.len() < 8192);
                        }
                        let text = String::from_utf8(request).unwrap();
                        assert!(text.starts_with("POST /renew HTTP/1.1\r\n"), "{text}");
                        seen.lock().unwrap().push(Instant::now());
                        let _ = socket.write_all(
                            b"HTTP/1.1 204 No Content\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                        );
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("stub accept: {e}"),
                }
            }
        });
        Self {
            url,
            times,
            stop,
            thread: Some(thread),
        }
    }

    fn posts(&self) -> usize {
        self.times.lock().unwrap().len()
    }

    fn times(&self) -> Vec<Instant> {
        self.times.lock().unwrap().clone()
    }
}

impl Drop for StubHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// CAD-702: `daemon stop` keeps exactly one renewal poster through the
/// WAL checkpoint and tracker flush — renewal stops only after the
/// flush completes. The stub records POST times across a deliberately
/// slow flush: renewals must span the whole shutdown window, then go
/// silent once the daemon is gone (no zero-poster gap, no second
/// poster, no afterlife).
#[test]
fn cad702_http_renewal_continues_through_slow_flush() {
    let stub = StubHost::new();
    let stop = Arc::new(AtomicBool::new(false));
    let mut opts = daemon_opts();
    opts.lease = Some(Hosted {
        lease: Some("http://lease.internal".into()),
        lease_ttl_secs: Some(6),
        lease_renew_secs: Some(1),
        flush_timeout_secs: Some(20),
    });
    // Test-only, in-process: the configured endpoint stays
    // `lease.internal` (restrictions enforced); only the transport
    // dials the loopback stub.
    opts.lease_http_endpoint_override = Some(stub.url.clone());
    // A deliberately slow flush: three seconds at a one-second
    // heartbeat must contain renewals iff the poster outlives `closing`.
    opts.flush_delay_for_test = Some(Duration::from_secs(3));
    opts.stop = Some(stop.clone());
    // Hermetic AgenticOS attach: a hosted lease triggers the deployment
    // lookup, which must not depend on the image file here — an empty
    // composition parses and leaves the adapter gated; we never publish.
    opts.provider_deployments =
        Some(DeploymentMetadata::parse(br#"{"schema":1,"providers":[]}"#).unwrap());
    let d = TestDaemon::start_opts(opts);
    // Admission plus at least one heartbeat renewal before shutdown —
    // so every POST after `stop` is proof of renewal during shutdown.
    let deadline = Instant::now() + Duration::from_secs(15);
    while stub.posts() < 2 {
        assert!(
            Instant::now() < deadline,
            "heartbeat never renewed before stop"
        );
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        d.rpc("health", json!({})).unwrap()["lease"]["fenced"].is_null(),
        "daemon fenced before shutdown"
    );
    let stopping_at = Instant::now();
    stop.store(true, Ordering::SeqCst);
    drop(d); // serve returns only after flush, heartbeat stop, release
    let during = stub
        .times()
        .into_iter()
        .filter(|t| *t >= stopping_at)
        .count();
    assert!(
        during >= 2,
        "renewal did not continue through the slow flush: {during} POSTs after stop"
    );
    // And the poster stopped with the daemon: silence afterwards.
    let total = stub.posts();
    thread::sleep(Duration::from_millis(2500));
    assert_eq!(stub.posts(), total, "renewal poster outlived the flush");
}

fn lease_expiry(dir: &Path) -> f64 {
    let body: Value =
        serde_json::from_str(&std::fs::read_to_string(lease_file(dir)).unwrap()).unwrap();
    body["expires_unix"].as_f64().unwrap()
}

fn wait_for_lease_file(dir: &Path) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !lease_file(dir).exists() {
        assert!(Instant::now() < deadline, "lease never acquired");
        thread::sleep(Duration::from_millis(20));
    }
}

/// CAD-947: a recovery that outlives the lease TTL. The heartbeat must
/// already be renewing when startup begins, so the daemon's own first
/// startup write is not refused by its own expired lease, and it keeps
/// the lease afterwards. RED on main: the heartbeat started only after
/// startup, so the write at the end of a 4s dwell hit a 2s lease and
/// the daemon exited with "store write refused — the daemon's hosted
/// lease is lost: lease expired".
#[test]
fn cad947_startup_longer_than_ttl_keeps_the_lease() {
    let dir = TempDir::new().unwrap();
    let mut opts = leased_opts(dir.path(), 2);
    opts.startup_delay_for_test = Some(Duration::from_secs(4));
    let d = TestDaemon::start_opts(opts);
    let h = d.rpc("health", json!({})).unwrap();
    assert_eq!(h["lease"]["epoch"], 1, "{h}");
    assert!(h["lease"]["fenced"].is_null(), "{h}");
    // Still held and still writable well past another TTL.
    thread::sleep(Duration::from_secs(3));
    d.register("w1");
    let h = d.rpc("health", json!({})).unwrap();
    assert!(h["lease"]["fenced"].is_null(), "{h}");
    assert!(lease_expiry(dir.path()) > now_unix(), "lease lapsed: {h}");
}

/// CAD-947: startup that fails after the lease was taken and the
/// heartbeat started must stop the heartbeat. The lease is then left to
/// expire (the existing rule for an unclean exit), so a successor takes
/// over after one TTL and no renewal poster outlives the failed start.
#[test]
fn cad947_failed_startup_stops_the_heartbeat_and_lets_the_lease_expire() {
    let dir = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let mut opts = leased_opts(dir.path(), 3);
    // A shared socket without a configured agent UID is refused by
    // startup after the lease, the store and the heartbeat exist.
    opts.shared_socket = Some((state.path().join("shared.sock"), 0));
    let err = daemon::serve_with(state.path(), opts).unwrap_err();
    assert!(err.to_string().contains("shared socket"), "{err}");
    let at_exit = lease_expiry(dir.path());
    // Two renew periods: a surviving heartbeat would have extended it.
    thread::sleep(Duration::from_millis(2500));
    assert_eq!(
        lease_expiry(dir.path()),
        at_exit,
        "the heartbeat outlived the failed startup"
    );
    // Left to expire, not leaked: a successor holds it after the TTL.
    while lease_expiry(dir.path()) > now_unix() {
        thread::sleep(Duration::from_millis(100));
    }
    let successor = TempDir::new().unwrap();
    let d = TestDaemon::start_on_opts(successor.path().to_path_buf(), leased_opts(dir.path(), 3));
    let h = d.rpc("health", json!({})).unwrap();
    assert_eq!(h["lease"]["epoch"], 2, "{h}");
}

/// CAD-947: a renewal failure during startup trips the fence like any
/// other: the foreign holder takes the lease while recovery dwells, the
/// early heartbeat sees it, and the daemon's startup write is refused
/// rather than landing under a lease it no longer holds.
#[test]
fn cad947_renewal_failure_during_startup_trips_the_fence() {
    let dir = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let mut opts = leased_opts(dir.path(), 30);
    opts.startup_delay_for_test = Some(Duration::from_secs(4));
    opts.stop = Some(stop.clone());
    let owned = state.path().to_path_buf();
    let handle = thread::spawn(move || daemon::serve_with(&owned, opts));
    wait_for_lease_file(dir.path());
    // The steal takes the lease's own flock — a renewal already inside
    // its critical section would otherwise overwrite it and the
    // startup write would land under a lease the daemon lost (CAD-991).
    steal_lease(dir.path());
    // The dwell is 4s; give startup a generous margin to finish.
    let deadline = Instant::now() + Duration::from_secs(12);
    while !handle.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    let finished = handle.is_finished();
    stop.store(true, Ordering::SeqCst);
    let result = handle.join().unwrap();
    assert!(finished, "daemon came up under a lease it had lost");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("lease"), "{err}");
    let fact: Value = serde_json::from_str(
        &std::fs::read_to_string(state.path().join("lease-fence.json")).unwrap(),
    )
    .unwrap();
    assert!(
        fact["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("renewal failed"),
        "{fact}"
    );
}
