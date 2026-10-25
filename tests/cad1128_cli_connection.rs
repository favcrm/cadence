#![cfg(all(unix, feature = "test-seam"))]
// Real CLI lifecycle + exact-ID/revision CAS — UNCOMPILED/UNEXECUTED.
// Mount as a new integration file `tests/cad1128_cli_connection.rs`.
//
// REV — the writer's flagged defects are fixed:
//   * `.env_remove("CADENCE_ALIAS")` REMOVED — the spawned CLI keeps the
//     native identity seam (`CADENCE_TEST_SEAM`+`CADENCE_TEST_AS`) and is a
//     seam-asserted native caller; nothing clears an identity var. The
//     daemon itself is seam-armed (`ServeOptions.test_seam`), so the child's
//     asserted frame is honoured — this is REAL CLI subprocess behaviour,
//     not a fake identity.
//   * `Daemon` now owns `JoinHandle<Result<()>>`, joins through a deadline
//     (`join_before`), preserves the join error (no unconditional Ok), and
//     has a `Drop` that stops+joins — mirroring `Rig`'s `Owner`; no
//     foreign kills, no leaked thread on panic.
//   * The child resolves the daemon via `--state-dir` (the real CLI flag)
//     + `HOME`/`XDG_STATE_HOME`/`CADENCE_PM_DIR`/`current_dir`, NOT a
//     socket-path override — the native path the CLI uses in production.
//   * Encrypted path is covered: `custody initialize`/`unlock`/`lock`/
//     `status` against a `Rig`-provisioned encrypted record + restart-
//     locked positive.
//   * An explicit stale-expected control runs over the same socket the CLI
//     used (the daemon's `expected_connection_id`/`expected_revision` CAS),
//     complementary to the CLI `expected_identity` preflight.
//   * Secrets go on STDIN only (`--secret-stdin` / `--token-stdin`).
//
// The CLI is the REAL `env!("CARGO_BIN_EXE_cadence")` binary; the daemon is
// a REAL `serve_with` seam-armed fixture — no in-process assertions stand
// in for the subprocess behaviour.

use cadence_agent::daemon::{self, ServeOptions};
use cadence_agent::platform;
use serde_json::json;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const A: &str = "cli-lifecycle-synthetic-A-a1b2c3d4";
const B: &str = "cli-lifecycle-synthetic-B-e5f6a7b8";
const CLEANUP: Duration = Duration::from_secs(30);

/// Join a fixture thread through a deadline, preserving its `Result` —
/// the `Rig`/`Owner` contract: a late thread is an error, a panicked one
/// is an error, and no `Ok(())` is reported for a join that failed.
fn join_before(
    handle: JoinHandle<cadence_agent::Result<()>>,
    deadline: Instant,
) -> cadence_agent::Result<()> {
    while !handle.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    if !handle.is_finished() {
        return Err(cadence_agent::Error::internal(
            "cli fixture daemon cleanup deadline exceeded",
        ));
    }
    handle
        .join()
        .map_err(|_| cadence_agent::Error::internal("cli fixture daemon panicked"))?
}

/// A managed seam-armed daemon on a fixed state dir: `spawn`/`start`/
/// `stop` so a real restart shares one durable store. `Drop` stops+joins.
struct Daemon {
    state: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<cadence_agent::Result<()>>>>,
}

impl Daemon {
    fn spawn(state: &Path) -> Arc<Self> {
        let d = Arc::new(Self {
            state: state.to_path_buf(),
            stop: Arc::new(AtomicBool::new(false)),
            thread: Mutex::new(None),
        });
        d.start();
        d
    }

    fn start(&self) {
        self.stop.store(false, Ordering::SeqCst);
        let mut opts = ServeOptions {
            stop: Some(self.stop.clone()),
            test_seam: true,
            report_router: Some(0),
            checkup: Some(0),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            ..Default::default()
        };
        opts.provider_env.set(
            "CADENCE_PM_DIR",
            self.state.parent().unwrap().join("pm").to_str().unwrap(),
        );
        // No `encrypted_custody` field: the daemon's shared config adopts
        // `<state>/custody-encrypted.json` when present (the private owner
        // record), so the fixture only writes that record — never a
        // `pub(crate)` config type this external test cannot name.
        platform::local::register(&self.state, &mut opts);
        platform::smtp::attach(&mut opts);
        let state = self.state.clone();
        *self.thread.lock().unwrap() =
            Some(thread::spawn(move || daemon::serve_with(&state, opts)));
        let deadline = Instant::now() + CLEANUP;
        while cadence_agent::client::rpc_timeout(
            &self.state,
            "health",
            json!({}),
            Duration::from_secs(2),
        )
        .is_err()
            || cadence_agent::test_seam::Seam::token_at(&self.state).is_none()
        {
            assert!(Instant::now() < deadline, "daemon never started");
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Stop and join through the deadline, preserving the join `Result`.
    /// Callers that must not discard a failed join use `finish`; the Drop
    /// path keeps a fixed diagnostic, never a swallowed `Err`.
    fn stop(&self) -> cadence_agent::Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        let mut outcome = Ok(());
        if let Some(h) = self.thread.lock().unwrap().take() {
            if let Err(e) = join_before(h, Instant::now() + CLEANUP) {
                outcome = Err(e);
            }
        }
        outcome
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.thread.get_mut().unwrap().take() {
            if join_before(h, Instant::now() + CLEANUP).is_err() {
                eprintln!("CAD-1128 cli fixture daemon cleanup did not complete");
            }
        }
    }
}

/// Run the real `cadence` CLI as a seam-asserted native caller. The daemon
// resolves via `--state-dir`; identity is asserted via the test seam, never
// cleared. `CADENCE_ALIAS` is NOT touched — the seam carries the identity.
fn cli(
    state: &Path,
    root: &Path,
    assert_as: &str,
    args: &[&str],
    secret: Option<&str>,
) -> (bool, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cadence"));
    cmd.arg("--state-dir")
        .arg(state)
        .args(args)
        .current_dir(root)
        .env("HOME", root.join("home"))
        .env("XDG_STATE_HOME", root.join("home"))
        .env("CADENCE_PM_DIR", root.join("pm"))
        .env("CADENCE_TEST_SEAM", "1")
        .env("CADENCE_TEST_AS", assert_as);
    // CAD-308: stdio is configured on the Command before the reaper spawns —
    // `reaper::spawn` is the only permitted child-spawn seam.
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cadence_agent::reaper::spawn(&mut cmd).expect("cadence binary must exist");
    if let Some(s) = secret {
        child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// A matching RPC on the daemon's socket — the explicit stale-expected
/// control that complements the CLI preflight (the CLI reads identity once
/// and forwards expected fields; this drives the same CAS directly).
fn op_rpc(
    state: &Path,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    cadence_agent::test_seam::scoped(cadence_agent::test_seam::Asserted::Operator, || {
        cadence_agent::client::rpc_timeout(state, method, params, Duration::from_secs(15))
    })
    .map_err(|e| e.to_string())
}

// =====================================================================
// Encrypted path: config/init/unlock/enroll/lock/restart-locked.
// =====================================================================
#[test]
fn cli_encrypted_lifecycle_and_locked_restart() {
    let root = TempDir::new().unwrap();
    // Config admission requires an owner-only root and record — TempDir's
    // default 0755/0664 fails `check()`.
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let state = root.path().join("state");
    let key_dir = root.path().join("keys");
    let key_path = key_dir.join("key");
    for d in [
        &state,
        &key_dir,
        &root.path().join("pm"),
        &root.path().join("home"),
    ] {
        std::fs::create_dir_all(d).unwrap();
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    // A real 32-byte key at the configured path; the record binds it.
    std::fs::write(&key_path, [0xabu8; 32]).unwrap();
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(
        state.join("custody-encrypted.json"),
        serde_json::to_vec(&json!({"key_file": key_path, "vault_dir": state.join("vault")}))
            .unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(
        state.join("custody-encrypted.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();

    let daemon = Daemon::spawn(&state);
    let rt = root.path();

    // `custody status` is the only read — before init it reports not-ready.
    let (ok, out, _) = cli(
        &state,
        rt,
        "operator",
        &["connection", "custody", "status"],
        None,
    );
    assert!(ok, "custody status must succeed: {out}");

    // initialize + unlock WITHOUT the ack flag refuse at the CLI parse
    // layer (the arg is required) — before any socket call.
    for verb in ["initialize", "unlock"] {
        let (ok, _, _) = cli(
            &state,
            rt,
            "operator",
            &["connection", "custody", verb],
            None,
        );
        assert!(
            !ok,
            "custody {verb} without --accept-same-uid-risk must refuse"
        );
    }
    // initialize + unlock WITH the ack succeed (real session open).
    let (ok, out, err) = cli(
        &state,
        rt,
        "operator",
        &[
            "connection",
            "custody",
            "initialize",
            "--accept-same-uid-risk",
        ],
        None,
    );
    assert!(ok, "custody initialize failed: {err} {out}");
    let (ok, out, err) = cli(
        &state,
        rt,
        "operator",
        &["connection", "custody", "unlock", "--accept-same-uid-risk"],
        None,
    );
    assert!(ok, "custody unlock failed: {err} {out}");

    // Enroll an encrypted record while unlocked — STDIN secret only.
    let (ok, out, err) = cli(
        &state,
        rt,
        "operator",
        &[
            "connection",
            "create-smtp",
            "smtp",
            "--account",
            "enc-acct",
            "--scope",
            "email:send",
            "--host",
            "localhost",
            "--port",
            "1",
            "--tls-mode",
            "implicit",
            "--username",
            "u",
            "--sender",
            "sender@example.test",
            "--secret-stdin",
            "--accept-same-uid-risk",
        ],
        Some(A),
    );
    assert!(ok, "encrypted create-smtp failed: {err}");
    let id = serde_json::from_str::<serde_json::Value>(&out).unwrap()["connection"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // lock the session; enroll/rotate now refuse (no unlocked manager).
    let (ok, _, _) = cli(
        &state,
        rt,
        "operator",
        &["connection", "custody", "lock"],
        None,
    );
    assert!(ok, "custody lock failed");
    let (ok, _, _) = cli(
        &state,
        rt,
        "operator",
        &[
            "connection",
            "rotate-smtp",
            &id,
            "--secret-stdin",
            "--accept-same-uid-risk",
        ],
        Some(B),
    );
    assert!(!ok, "rotate on a locked encrypted custody must refuse");

    // Real restart — daemon stops, respawns on the same encrypted state;
    // custody returns locked and the record persists.
    daemon.stop().expect("cli fixture daemon stop failed");
    daemon.start();
    let (ok, out, _) = cli(&state, rt, "operator", &["connection", "show", &id], None);
    assert!(ok, "post-restart show failed: {out}");
    let (ok, out, _) = cli(
        &state,
        rt,
        "operator",
        &["connection", "custody", "status"],
        None,
    );
    assert!(ok);
    // Real locked assertion against the status schema: a restart returns
    // `unlocked:false` until `unlock` is re-run (never assumed — the guard).
    let status = serde_json::from_str::<serde_json::Value>(&out).unwrap();
    // `unlocked` MUST be present in the status schema — a missing key is a
    // schema failure, never a masked "locked". The real response is
    // {"custody":{"configured","unlocked","fenced","isolation","protected"}}.
    let unlocked = status["custody"]["unlocked"]
        .as_bool()
        .expect("custody status carries no `unlocked` flag");
    assert!(!unlocked, "post-restart custody must report locked: {out}");
    let (ok, _, _) = cli(
        &state,
        rt,
        "operator",
        &[
            "connection",
            "rotate-smtp",
            &id,
            "--secret-stdin",
            "--accept-same-uid-risk",
        ],
        Some(B),
    );
    assert!(!ok, "a restarted locked custody must still refuse rotate");

    daemon.stop().expect("cli fixture daemon stop failed");
}

// =====================================================================
// Legacy record + CLI preflight vs explicit stale-RPC control.
// =====================================================================
#[test]
fn cli_preflight_and_stale_expected_rpc_control() {
    let root = TempDir::new().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let state = root.path().join("state");
    let key_dir = root.path().join("keys");
    for d in [
        &state,
        &key_dir,
        &root.path().join("pm"),
        &root.path().join("home"),
    ] {
        std::fs::create_dir_all(d).unwrap();
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let daemon = Daemon::spawn(&state);
    let rt = root.path();

    // Legacy custody:"file" record enrolled through the real CLI.
    let (ok, out, err) = cli(
        &state,
        rt,
        "operator",
        &[
            "connection",
            "create-smtp",
            "smtp",
            "--account",
            "leg-acct",
            "--scope",
            "email:send",
            "--host",
            "smtp.example",
            "--port",
            "465",
            "--tls-mode",
            "implicit",
            "--username",
            "u",
            "--sender",
            "sender@example.test",
            "--secret-stdin",
            "--accept-same-uid-risk",
        ],
        Some(A),
    );
    assert!(ok, "legacy create-smtp failed: {err}");
    let id = serde_json::from_str::<serde_json::Value>(&out).unwrap()["connection"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // The CLI `show` returns the exact identity the preflight reads.
    let (ok, out, _) = cli(&state, rt, "operator", &["connection", "show", &id], None);
    assert!(ok);
    let rev = serde_json::from_str::<serde_json::Value>(&out).unwrap()["connection"]["revision"]
        .as_u64()
        .unwrap();
    assert!(
        rev > 0,
        "a fresh legacy enrollment must be at a positive revision"
    );

    // A matching CLI revoke succeeds — the preflight forwards the live pair.
    let (ok, out, _) = cli(&state, rt, "operator", &["connection", "revoke", &id], None);
    assert!(ok, "matching CLI revoke failed");
    assert!(out.contains("revoked"), "{out}");

    // Re-enroll a fresh incarnation.
    let (ok, out, _) = cli(
        &state,
        rt,
        "operator",
        &[
            "connection",
            "create-smtp",
            "smtp",
            "--account",
            "leg-acct",
            "--scope",
            "email:send",
            "--host",
            "smtp.example",
            "--port",
            "465",
            "--tls-mode",
            "implicit",
            "--username",
            "u",
            "--sender",
            "sender@example.test",
            "--secret-stdin",
            "--accept-same-uid-risk",
        ],
        Some(B),
    );
    assert!(ok);
    let id2 = serde_json::from_str::<serde_json::Value>(&out).unwrap()["connection"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // The explicit stale-expected control over the same socket the CLI used:
    // the daemon's CAS refuses a wrong `expected_revision`, the same gate
    // the CLI `expected_identity` forwards into. A NEW incarnation restarts
    // `credential_revision` at 1 — read id2's live revision, never assume.
    let live2 = op_rpc(&state, "connection_show", json!({"connection_id": id2})).unwrap();
    let rev2 = live2["connection"]["revision"].as_u64().unwrap();
    let stale = op_rpc(
        &state,
        "connection_revoke",
        json!({"connection_id": id2, "expected_connection_id": id2,
               "expected_revision": rev2 + 99}),
    );
    assert!(
        stale.is_err(),
        "a forged expected_revision must refuse: {stale:?}"
    );
    let still = op_rpc(&state, "connection_show", json!({"connection_id": id2})).unwrap();
    assert_eq!(
        still["connection"]["revision"].as_u64().unwrap(),
        rev2,
        "a refused revoke must not retire or bump"
    );

    // Non-operator CLI caller is refused at the daemon gate.
    let (ok, _, _) = cli(
        &state,
        rt,
        "agent:writer",
        &["connection", "revoke", &id2],
        None,
    );
    assert!(!ok, "agent revoke must be refused by the daemon gate");

    daemon.stop().expect("cli fixture daemon stop failed");
}
