//! CAD-1024 / contract PR-4: `cadence staging refresh` — the delegate
//! round-trip an agent actually runs. These are integration tests over a
//! real `daemon run` process on a registered staging state dir: the
//! delegate `w1` (a planted `LaneShell` pane) runs
//! `staging refresh --as delegate:w1`, which claims the rollout lease,
//! stops and restarts the daemon, and releases — every step admitted only
//! by `w1`'s live `staging_grants` row, never by operator proof.
//!
//! The refuses are the contract's adversarial list: an unregistered dir,
//! a forged `--as` whose env alias does not match the pane's, a revoked
//! grant, and an expired grant.

mod common;

use serde_json::json;
use std::path::Path;

use common::{daemon_opts, plant_member_pane, test_port, LaneShell, TestDaemon};

/// `staging refresh --as delegate:<alias>` run inside `lane`'s pane with
/// `CADENCE_ALIAS=<alias>` — the real delegate call site. The cadence
/// subprocess's ancestry is the lane's bash pid, so `caller_chain` finds
/// the planted pane endpoint.
fn delegate_refresh(lane: &mut LaneShell, state: &Path, args: &str) -> (i64, String) {
    delegate_refresh_as(lane, "w1", state, args)
}

/// The delegate refresh run inside `lane`'s pane with `CADENCE_ALIAS` set
/// to `env_alias` — a real agent's invocation carries its own alias on the
/// pane env; a forged `--as` names a different one. `env -u` first so the
/// suite host's `CADENCE_ALIAS` never leaks a false match.
fn delegate_refresh_as(
    lane: &mut LaneShell,
    env_alias: &str,
    state: &Path,
    args: &str,
) -> (i64, String) {
    lane.run(&format!(
        "env -u CADENCE_ALIAS CADENCE_ALIAS={env_alias} {} --state-dir {} staging refresh {args}",
        env!("CARGO_BIN_EXE_cadence"),
        state.display()
    ))
}

/// Plant `w1` in `d` with `lane`'s bash pid as its endpoint, register the
/// dir staging on `board_port`, and grant `w1` every staging op.
fn granted_delegate(d: &TestDaemon, lane: &LaneShell, board_port: u16) {
    plant_member_pane(d, "w1", "claude", Some("pm"), lane.pid());
    d.operator_rpc("staging_register", json!({"board_port": board_port}))
        .unwrap();
    d.operator_rpc(
        "staging_delegate",
        json!({"agent": "w1",
               "ops": ["rollout_claim","daemon_start","daemon_stop","ui_start","ui_stop"],
               "ttl_secs": 3600,
               "reason": "staging refresh test"}),
    )
    .unwrap();
}

#[test]
fn delegate_refresh_round_trip_on_a_registered_staging_dir() {
    let d = TestDaemon::start_opts(daemon_opts());
    let lanes_home = tempfile::TempDir::new().unwrap();
    let mut w1 = LaneShell::spawn(lanes_home.path());
    let port = test_port();
    granted_delegate(&d, &w1, port.port);

    // `daemon stop` stops the fixture daemon; `daemon start` respawns a
    // `daemon run` subprocess bound to the same dir. `--no-ui` keeps the
    // test off a real board bind — the ui ops are gated the same way.
    let (rc, out) = delegate_refresh(&mut w1, &d.state, "--as delegate:w1 --no-ui");
    assert_eq!(rc, 0, "delegate refresh under a live grant: {out}");
    let v: serde_json::Value = serde_json::from_str(out.trim().lines().last().unwrap_or("{}"))
        .unwrap_or_else(|_| panic!("refresh output is not json: {out}"));
    assert_eq!(v["delegate"], "delegate:w1", "{v}");
    assert_eq!(v["release"], json!(true), "{v}");
    // A daemon answers again after the refresh, and the lease is free.
    assert!(cadence_agent::client::rpc(&d.state, "health", json!({})).is_ok());
    let lease = cadence_agent::rollout::status(&d.state).unwrap();
    assert_ne!(lease["held"], json!(true), "lease must be free: {lease}");
    let _ = cadence_agent::client::rpc(&d.state, "shutdown", json!({}));
}

#[test]
fn delegate_refresh_on_an_unregistered_dir_is_refused() {
    let d = TestDaemon::start_opts(daemon_opts());
    let lanes_home = tempfile::TempDir::new().unwrap();
    let mut w1 = LaneShell::spawn(lanes_home.path());
    // A planted pane, but the dir was never registered staging and has no
    // grant — `require_delegate_grant` refuses before any step runs.
    plant_member_pane(&d, "w1", "claude", Some("pm"), w1.pid());
    let (rc, out) = delegate_refresh(&mut w1, &d.state, "--as delegate:w1 --no-ui");
    assert_ne!(rc, 0, "unregistered dir must refuse: {out}");
    assert!(
        out.contains("registered staging") || out.contains("no live grant"),
        "{out}"
    );
}

#[test]
fn delegate_refresh_is_refused_after_revoke() {
    let d = TestDaemon::start_opts(daemon_opts());
    let lanes_home = tempfile::TempDir::new().unwrap();
    let mut w1 = LaneShell::spawn(lanes_home.path());
    granted_delegate(&d, &w1, test_port().port);
    d.operator_rpc("staging_revoke", json!({"agent": "w1"}))
        .unwrap();
    let (rc, out) = delegate_refresh(&mut w1, &d.state, "--as delegate:w1 --no-ui");
    assert_ne!(rc, 0, "revoked grant must refuse: {out}");
    assert!(out.contains("no live grant"), "{out}");
}

#[test]
fn delegate_refresh_is_refused_after_expiry() {
    let d = TestDaemon::start_opts(daemon_opts());
    let lanes_home = tempfile::TempDir::new().unwrap();
    let mut w1 = LaneShell::spawn(lanes_home.path());
    plant_member_pane(&d, "w1", "claude", Some("pm"), w1.pid());
    d.operator_rpc("staging_register", json!({"board_port": test_port().port}))
        .unwrap();
    d.operator_rpc(
        "staging_delegate",
        json!({"agent": "w1", "ops": ["rollout_claim","daemon_start","daemon_stop","ui_start","ui_stop"], "ttl_secs": 60}),
    )
    .unwrap();
    // Backdate the grant's expiry so it is dead when the delegate calls.
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE staging_grants SET expires_at=?1 WHERE alias='w1'",
        rusqlite::params![cadence_agent::rollout::unix_now() - 1.0],
    )
    .unwrap();
    let (rc, out) = delegate_refresh(&mut w1, &d.state, "--as delegate:w1 --no-ui");
    assert_ne!(rc, 0, "expired grant must refuse: {out}");
    assert!(out.contains("no live grant"), "{out}");
}

#[test]
fn delegate_refresh_for_a_mismatched_env_alias_is_refused() {
    // `CADENCE_ALIAS=w2` but `--as delegate:w1` — the env alias must equal
    // the literal alias the grant names, or the gate refuses even with a
    // live grant for w1.
    let d = TestDaemon::start_opts(daemon_opts());
    let lanes_home = tempfile::TempDir::new().unwrap();
    let mut w2 = LaneShell::spawn(lanes_home.path());
    granted_delegate(&d, &w2, test_port().port); // grants w1, w2's pid planted
    plant_member_pane(&d, "w2", "claude", Some("pm"), w2.pid());
    let (rc, out) = delegate_refresh_as(&mut w2, "w2", &d.state, "--as delegate:w1 --no-ui");
    assert_ne!(rc, 0, "env alias w2 vs grant w1 must refuse: {out}");
    assert!(
        out.contains("env alias") || out.contains("CADENCE_ALIAS"),
        "{out}"
    );
}
