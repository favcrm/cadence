//! CAD-1268 independent refusal acceptance tests.
//!
//! These drive the public scheduler API with the current test process as a
//! stable live holder and zero resource floors. They do not start Cargo,
//! providers, a daemon, or disposable holder processes.

use cadence_agent::slots::{SlotCaller, SlotClock, SlotConfig, SlotKind, Slots};
use serde_json::{json, Value};
use tempfile::Builder;

fn budget(total_slots: usize) -> SlotConfig {
    // Keep every individual pool roomy so these assertions isolate the
    // aggregate budget rather than the legacy per-pool limits.
    SlotConfig {
        total_slots,
        build_slots: 8,
        suite_slots: 8,
        check_slots: 8,
        slot_mem_min_available_bytes: 0,
        slot_mem_min_available_check_bytes: 0,
        slot_disk_min_free_bytes: 0,
        ..SlotConfig::default()
    }
}

fn acquire(
    slots: &mut Slots,
    kind: SlotKind,
    lane: &str,
    request: &str,
    probe: bool,
    at: f64,
) -> Value {
    slots
        .acquire(
            kind,
            lane,
            std::process::id(),
            request,
            probe,
            SlotClock::at(at, at),
        )
        .unwrap()
        .0
}

fn release(slots: &mut Slots, token: &str, lane: &str, at: f64) {
    slots.release(token, lane, std::process::id(), at).unwrap();
}

#[test]
fn default_aggregate_budget_and_jobs_are_two() {
    let config = SlotConfig::default();
    assert_eq!(config.total_slots, 2, "aggregate grants default to two");
    assert_eq!(config.jobs_per_lane, 2, "default compiler jobs per lane");
}

#[test]
fn configured_zero_aggregate_cap_is_clamped_to_one() {
    let mut slots = Slots::new(budget(0));
    let first = acquire(&mut slots, SlotKind::Build, "lane-a", "build-a", false, 1.0);
    assert_eq!(first["granted"], true, "zero must clamp to one");
    let second = acquire(&mut slots, SlotKind::Suite, "lane-b", "suite-b", false, 2.0);
    assert_eq!(
        second["granted"], false,
        "clamped cap must still be enforced"
    );
}

#[test]
fn every_pool_and_probe_obey_the_combined_cap_until_release() {
    let mut slots = Slots::new(budget(1));
    let holder = acquire(&mut slots, SlotKind::Build, "lane-a", "build-a", false, 1.0);
    assert_eq!(holder["granted"], true);
    let token = holder["token"].as_str().unwrap().to_owned();

    // A probe may report that check cannot run, but cannot take a second
    // grant or leave a waiter behind. Suite is independently queued on B.
    let check = acquire(&mut slots, SlotKind::Check, "lane-b", "check-b", true, 2.0);
    assert_eq!(check["granted"], false, "probe exceeded aggregate capacity");
    let suite = acquire(&mut slots, SlotKind::Suite, "lane-b", "suite-b", false, 2.0);
    assert_eq!(suite["granted"], false, "suite exceeded aggregate capacity");

    release(&mut slots, &token, "lane-a", 3.0);
    let suite = acquire(&mut slots, SlotKind::Suite, "lane-b", "suite-b", false, 4.0);
    assert_eq!(
        suite["granted"], true,
        "queued suite should run after release"
    );
}

#[test]
fn same_lane_process_cannot_hold_and_wait_but_exact_grant_is_idempotent() {
    let mut slots = Slots::new(budget(1));
    let first = acquire(
        &mut slots,
        SlotKind::Build,
        "lane-a",
        "original",
        false,
        1.0,
    );
    assert_eq!(first["granted"], true);
    let first_token = first["token"].as_str().unwrap();

    let duplicate = acquire(
        &mut slots,
        SlotKind::Build,
        "lane-a",
        "original",
        false,
        2.0,
    );
    assert_eq!(duplicate["granted"], true);
    assert_eq!(
        duplicate["token"], first_token,
        "exact request must be idempotent"
    );

    let error = slots
        .acquire(
            SlotKind::Build,
            "lane-a",
            std::process::id(),
            "second-request",
            false,
            SlotClock::at(3.0, 3.0),
        )
        .expect_err("same holder must not queue while aggregate capacity is full");
    assert!(error.to_string().contains("deadlock guard"), "{error}");

    // A genuine distinct caller is waitable; the refused second request
    // did not occupy a queue position or change the original grant.
    let (status, _) = slots.status(
        SlotCaller {
            lane: "lane-a",
            pids: &[std::process::id()],
        },
        4.0,
    );
    assert!(status["waiting"]
        .as_array()
        .unwrap()
        .iter()
        .all(|waiter| waiter["request_id"] != "second-request"));
    let other = acquire(&mut slots, SlotKind::Suite, "lane-b", "waiter", false, 4.0);
    assert_eq!(other["granted"], false);
}

#[test]
fn cross_pool_priority_is_not_blocked_by_an_earlier_ordinary_waiter() {
    let mut config = budget(1);
    config.priority_lanes = vec!["priority".to_owned()];
    let mut slots = Slots::new(config);
    let holder = acquire(
        &mut slots,
        SlotKind::Build,
        "holder",
        "build-holder",
        false,
        1.0,
    );
    let token = holder["token"].as_str().unwrap().to_owned();

    // The ordinary build wait arrives first; the later priority suite
    // waiter must still win once the sole aggregate grant is released.
    assert_eq!(
        acquire(
            &mut slots,
            SlotKind::Build,
            "ordinary",
            "build-wait",
            false,
            2.0
        )["granted"],
        false
    );
    assert_eq!(
        acquire(
            &mut slots,
            SlotKind::Suite,
            "priority",
            "suite-priority",
            false,
            3.0
        )["granted"],
        false
    );
    release(&mut slots, &token, "holder", 4.0);
    assert_eq!(
        acquire(
            &mut slots,
            SlotKind::Build,
            "ordinary",
            "build-wait",
            false,
            5.0
        )["granted"],
        false,
        "ordinary waiter bypassed eligible priority suite waiter"
    );
    assert_eq!(
        acquire(
            &mut slots,
            SlotKind::Suite,
            "priority",
            "suite-priority",
            false,
            5.0
        )["granted"],
        true
    );
}

#[test]
fn restored_live_holds_over_a_reduced_cap_are_retained_and_drain() {
    let root = Builder::new().prefix("c1268-").tempdir_in("/tmp").unwrap();
    let state = root.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let file = state.join("slots.json");
    let pid = std::process::id();
    std::fs::write(
        &file,
        json!({
            "holds": [
                {"token":"slot-build-live", "request_id":"build-live", "kind":"build", "lane":"restored-build", "pid":pid, "pid_start":null, "acquired_epoch":10.0},
                {"token":"slot-suite-live", "request_id":"suite-live", "kind":"suite", "lane":"restored-suite", "pid":pid, "pid_start":null, "acquired_epoch":10.0}
            ]
        })
        .to_string(),
    )
    .unwrap();

    let mut slots = Slots::new(budget(1));
    slots.persist_to(file);
    slots.restore(SlotClock::at(20.0, 20.0));
    let waiting = acquire(
        &mut slots,
        SlotKind::Check,
        "new-lane",
        "new-work",
        false,
        21.0,
    );
    assert_eq!(
        waiting["granted"], false,
        "restore revoked or ignored live over-cap holds"
    );

    release(&mut slots, "slot-build-live", "restored-build", 22.0);
    let still_waiting = acquire(
        &mut slots,
        SlotKind::Check,
        "new-lane",
        "new-work",
        false,
        23.0,
    );
    assert_eq!(
        still_waiting["granted"], false,
        "remaining restored hold did not drain"
    );
    release(&mut slots, "slot-suite-live", "restored-suite", 24.0);
    let granted = acquire(
        &mut slots,
        SlotKind::Check,
        "new-lane",
        "new-work",
        false,
        25.0,
    );
    assert_eq!(
        granted["granted"], true,
        "new work did not proceed after restored holds drained"
    );
}
