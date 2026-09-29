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
    // sees holder and epoch differ and trips the fence.
    let body: Value =
        serde_json::from_str(&std::fs::read_to_string(lease_file(dir.path())).unwrap()).unwrap();
    let stolen = json!({"holder": "intruder",
                        "epoch": body["epoch"].as_u64().unwrap() + 5,
                        "expires_unix": now_unix() + 600.0});
    std::fs::write(lease_file(dir.path()), stolen.to_string()).unwrap();

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
    // trips the fence. This poll must happen BEFORE the stop below:
    // once `closing` is set serve stops accepting connections, so any
    // RPC issued while the tail is parked at the gate would hang on
    // the 700s client timeout and deadlock the rendezvous.
    let body: Value =
        serde_json::from_str(&std::fs::read_to_string(lease_file(dir.path())).unwrap()).unwrap();
    let stolen = json!({"holder": "intruder",
                        "epoch": body["epoch"].as_u64().unwrap() + 5,
                        "expires_unix": now_unix() + 600.0});
    std::fs::write(lease_file(dir.path()), stolen.to_string()).unwrap();
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
    let mut guard = GateGuard::armed(&gate);
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

/// Releases the shutdown gate if the test unwinds after asking serve
/// to stop but before reaching the gate itself — otherwise serve parks
/// at the gate forever and the shutdown join hangs. Armed only after
/// `stop` is set (serve then always reaches the gate); disarmed once
/// the test rendezvoused normally.
struct GateGuard {
    gate: Option<Arc<Barrier>>,
}

impl GateGuard {
    fn armed(gate: &Arc<Barrier>) -> Self {
        Self {
            gate: Some(Arc::clone(gate)),
        }
    }

    fn disarm(&mut self) {
        self.gate.take();
    }
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        if let Some(gate) = self.gate.take() {
            gate.wait();
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
