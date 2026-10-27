use super::*;
use crate::store::NewAgent;

const DAY: f64 = 86_400.0;

fn pinned(dir: &Path, setting: AgentGcSetting) -> Arc<Shared> {
    let opts = ServeOptions {
        agent_gc: Some(setting),
        ..ServeOptions::default()
    };
    Shared::new(dir, &opts).unwrap()
}

/// Register a stopped, disabled fake agent last updated `age_days`
/// ago — pre-aged through a side connection, no clock or sleep.
fn stopped_row(shared: &Shared, dir: &Path, alias: &str, age_days: f64) {
    shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: dir.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    rusqlite::Connection::open(dir.join("cadence.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE agents SET state='stopped', enabled=0, endpoint=NULL,
                 updated=? WHERE alias=?",
            rusqlite::params![epoch_secs() - age_days * DAY, alias],
        )
        .unwrap();
}

fn removed_events(shared: &Shared) -> Vec<Value> {
    shared
        .store
        .events(Store::DAEMON_STREAM, 0, 100)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "agent_gc_removed")
        .map(|e| e.payload)
        .collect()
}

#[test]
fn setting_is_off_unless_host_sets_it_and_floors_at_seven_days() {
    let dir = tempfile::tempdir().unwrap();
    let pm = dir.path();
    // No pm.yaml, and a [host] table without the key: off.
    assert_eq!(
        AgentGcSetting::from_pm_dir(Some(pm)),
        AgentGcSetting::default()
    );
    assert_eq!(AgentGcSetting::from_pm_dir(None).effective_secs(), None);
    std::fs::write(
        pm.join("pm.yaml"),
        "schema: 1\nhost:\n  wal_max_bytes: 4096\n",
    )
    .unwrap();
    let off = AgentGcSetting::from_pm_dir(Some(pm));
    assert_eq!(off.effective_secs(), None);
    assert_eq!(off.warning(), None);
    // Set at or above the floor: used verbatim, no warning.
    std::fs::write(
        pm.join("pm.yaml"),
        "schema: 1\nhost:\n  agent_gc_older_than_secs: 1209600\n",
    )
    .unwrap();
    let on = AgentGcSetting::from_pm_dir(Some(pm));
    assert_eq!(on.effective_secs(), Some(1_209_600));
    assert_eq!(on.warning(), None);
    // Below the floor: raised to 7 days, with a warning.
    std::fs::write(
        pm.join("pm.yaml"),
        "schema: 1\nhost:\n  agent_gc_older_than_secs: 3600\n",
    )
    .unwrap();
    let low = AgentGcSetting::from_pm_dir(Some(pm));
    assert_eq!(low.configured_secs, Some(3600));
    assert_eq!(low.effective_secs(), Some(AGENT_GC_FLOOR_SECS));
    assert!(low.warning().unwrap().contains("below the 7-day floor"));
    // An unusable [host] table fails closed: off, with the error.
    std::fs::write(
        pm.join("pm.yaml"),
        "schema: 1\nhost:\n  agent_gc_older_than_secs: \"7d\"\n",
    )
    .unwrap();
    let bad = AgentGcSetting::from_pm_dir(Some(pm));
    assert_eq!(bad.effective_secs(), None);
    assert!(
        bad.warning().unwrap().contains("agent_gc_older_than_secs"),
        "{bad:?}"
    );
}

#[test]
fn unconfigured_tick_never_removes() {
    let dir = tempfile::tempdir().unwrap();
    let shared = pinned(dir.path(), AgentGcSetting::default());
    stopped_row(&shared, dir.path(), "ancient", 365.0);
    shared.agent_gc_tick();
    assert!(shared.store.agent_opt("ancient").unwrap().is_some());
    assert!(removed_events(&shared).is_empty());
    let status = shared.agent_gc.status();
    assert_eq!(status["enabled"], false, "{status}");
    assert!(status["last_check_at"].is_f64(), "{status}");
    assert!(status["last_sweep_at"].is_null(), "{status}");
}

#[test]
fn configured_tick_sweeps_eligible_rows_at_most_hourly() {
    let dir = tempfile::tempdir().unwrap();
    // One hour configured: below the floor, so the timer uses 7 days.
    let shared = pinned(dir.path(), AgentGcSetting::older_than(3600));
    stopped_row(&shared, dir.path(), "old", 30.0);
    stopped_row(&shared, dir.path(), "under-floor", 2.0);
    stopped_row(&shared, dir.path(), "unknown", 30.0);
    shared
        .store
        .enqueue("unknown", "work", None, "m-u", "user")
        .unwrap();
    rusqlite::Connection::open(dir.path().join("cadence.sqlite3"))
        .unwrap()
        .execute("UPDATE messages SET state='unknown' WHERE id='m-u'", [])
        .unwrap();

    shared.agent_gc_tick();
    assert!(shared.store.agent_opt("old").unwrap().is_none());
    assert!(shared.store.agent_opt("under-floor").unwrap().is_some());
    assert!(shared.store.agent_opt("unknown").unwrap().is_some());
    let events = removed_events(&shared);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["alias"], "old");
    assert_eq!(events[0]["older_than_secs"], AGENT_GC_FLOOR_SECS as f64);
    let status = shared.agent_gc.status();
    assert_eq!(status["enabled"], true, "{status}");
    assert_eq!(status["older_than_secs"], AGENT_GC_FLOOR_SECS, "{status}");
    assert_eq!(status["configured_secs"], 3600, "{status}");
    assert!(status["warning"].as_str().unwrap().contains("floor"));
    assert_eq!(status["last_removed"], 1, "{status}");
    assert!(status["note"]
        .as_str()
        .unwrap()
        .contains("frees no memory and no disk"));

    // A fresh eligible row within the hour waits: a forced re-check
    // reads the setting but does not sweep again.
    stopped_row(&shared, dir.path(), "later", 30.0);
    shared.agent_gc.state.lock().unwrap().next_check = None;
    shared.agent_gc_tick();
    assert!(shared.store.agent_opt("later").unwrap().is_some());
    // An hour after the last sweep, the next tick takes it.
    {
        let mut st = shared.agent_gc.state.lock().unwrap();
        st.next_check = None;
        st.last_sweep = Instant::now().checked_sub(AGENT_GC_EVERY);
    }
    shared.agent_gc_tick();
    assert!(shared.store.agent_opt("later").unwrap().is_none());
    assert_eq!(removed_events(&shared).len(), 2);
}

#[test]
fn gc_keep_survives_the_sweep_that_removes_a_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let shared = pinned(dir.path(), AgentGcSetting::older_than(AGENT_GC_FLOOR_SECS));
    stopped_row(&shared, dir.path(), "keep", 30.0);
    stopped_row(&shared, dir.path(), "gone", 30.0);
    shared
        .store
        .set_gc_keep_by("keep", true, &Value::Null)
        .unwrap();
    shared.agent_gc_tick();
    assert!(shared.store.agent_opt("keep").unwrap().is_some());
    assert!(shared.store.agent_opt("gone").unwrap().is_none());
    let events = removed_events(&shared);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["alias"], "gone");
}

#[test]
fn gc_floor_refuses_below_seven_days_without_force() {
    assert!(super::timers::require_agent_gc_floor(Some(3600.0), false).is_err());
    assert!(super::timers::require_agent_gc_floor(Some(3600.0), true).is_ok());
    assert!(super::timers::require_agent_gc_floor(Some(AGENT_GC_FLOOR_SECS as f64), false).is_ok());
    assert!(super::timers::require_agent_gc_floor(None, false).is_ok());
}

#[test]
fn gc_keep_set_after_candidate_snapshot_blocks_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let shared = pinned(dir.path(), AgentGcSetting::default());
    stopped_row(&shared, dir.path(), "kept", 30.0);
    let _ = shared.store.gc_candidates(Some(0.0)).unwrap();
    shared
        .store
        .set_gc_keep_by("kept", true, &Value::Null)
        .unwrap();
    let err = shared
        .store
        .gc_remove_agent("kept", &Value::Null)
        .expect_err("a keep set after the snapshot must block the delete");
    assert!(
        err.to_string().contains("no longer a gc candidate"),
        "{err}"
    );
    assert!(shared.store.agent_opt("kept").unwrap().is_some());
}

#[test]
fn gc_timer_removes_the_provider_log_with_the_row() {
    let dir = tempfile::tempdir().unwrap();
    let shared = pinned(dir.path(), AgentGcSetting::older_than(AGENT_GC_FLOOR_SECS));
    stopped_row(&shared, dir.path(), "gone", 30.0);
    let log = shared.provider_log_dir.join("gone.provider.log");
    std::fs::write(&log, b"provider chatter").unwrap();
    shared.agent_gc_tick();
    assert!(shared.store.agent_opt("gone").unwrap().is_none());
    assert!(!log.exists());
}
