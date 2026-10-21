use super::*;
use crate::store::NewAgent;

const HOUR: f64 = 3600.0;

fn pinned(dir: &Path, setting: AutoStopSetting) -> Arc<Shared> {
    let opts = ServeOptions {
        auto_stop: Some(setting),
        ..ServeOptions::default()
    };
    Shared::new(dir, &opts).unwrap()
}

/// Register `alias` and make its row look like a live, idle actor
/// with a saved thread — no actor runs; the verdict reads rows.
fn idle_row(
    shared: &Shared,
    dir: &Path,
    alias: &str,
    provider: &str,
    kind: &str,
    role: &str,
    params: Value,
) -> Agent {
    let params = params.to_string();
    shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider,
            endpoint_kind: kind,
            role,
            cwd: dir.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: Some(&params),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    rusqlite::Connection::open(dir.join("cadence.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE agents SET state='idle', enabled=1, thread_id=? WHERE alias=?",
            rusqlite::params![format!("t-{alias}"), alias],
        )
        .unwrap();
    shared.store.agent(alias).unwrap()
}

fn worker(shared: &Shared, dir: &Path, alias: &str, params: Value) -> Agent {
    let mut p = json!({"upstream": "pm"});
    if let (Some(p), Some(extra)) = (p.as_object_mut(), params.as_object()) {
        p.extend(extra.clone());
    }
    idle_row(shared, dir, alias, "fake", "fake", "worker", p)
}

fn verdict(shared: &Shared, setting: &AutoStopSetting, agent: &Agent, at: f64) -> AutoStopVerdict {
    let agents = shared.store.agents().unwrap();
    let activity = shared
        .store
        .auto_stop_activity(AUTO_STOP_PASSIVE_KINDS)
        .unwrap();
    let agent = shared.store.agent(&agent.alias).unwrap();
    shared.auto_stop_verdict(
        &agent,
        setting,
        at,
        &upstream_roots(&agents),
        activity.get(&agent.alias),
    )
}

fn kept(v: &AutoStopVerdict) -> &str {
    match v {
        AutoStopVerdict::Keep(reason) => reason,
        other => panic!("expected keep, got {other:?}"),
    }
}

#[test]
fn setting_defaults_on_at_an_hour_with_provider_override_and_floor() {
    let dir = tempfile::tempdir().unwrap();
    let pm = dir.path();
    // No pm.yaml / no key: ON at the built-in hour, no warning.
    let on = AutoStopSetting::from_pm_dir(Some(pm));
    assert_eq!(on, AutoStopSetting::default());
    assert_eq!(on.default_bound(), Some(AUTO_STOP_DEFAULT_SECS));
    assert_eq!(on.warning(), None);
    assert_eq!(
        AutoStopSetting::from_pm_dir(None).default_bound(),
        Some(3600)
    );
    std::fs::write(
        pm.join("pm.yaml"),
        "schema: 1\nhost:\n  auto_stop_idle_secs: 0\n  \
             auto_stop_idle_secs_by_provider:\n    claude: 7200\n    codex: 60\n",
    )
    .unwrap();
    let s = AutoStopSetting::from_pm_dir(Some(pm));
    assert_eq!(s.default_bound(), None, "0 turns the host default off");
    assert_eq!(s.by_provider.get("claude"), Some(&7200));
    let w = s.warning().unwrap();
    assert!(
        w.contains("auto_stop_idle_secs_by_provider.codex 60"),
        "{w}"
    );
    assert!(w.contains("600s floor"), "{w}");
    // An unusable [host] table stops nothing, and says why.
    std::fs::write(
        pm.join("pm.yaml"),
        "schema: 1\nhost:\n  auto_stop_idle_secs: \"1h\"\n",
    )
    .unwrap();
    let bad = AutoStopSetting::from_pm_dir(Some(pm));
    assert_eq!(bad.default_bound(), None);
    assert!(
        bad.warning().unwrap().contains("idle auto-stop off"),
        "{bad:?}"
    );
}

#[test]
fn bound_precedence_agent_then_provider_then_host() {
    let dir = tempfile::tempdir().unwrap();
    let mut setting = AutoStopSetting::idle_after(5400);
    setting.by_provider.insert("fake".into(), 7200);
    let shared = pinned(dir.path(), setting.clone());
    let plain = worker(&shared, dir.path(), "plain", json!({}));
    assert_eq!(
        setting.bound_for(&plain),
        (
            Some(7200),
            "[host] auto_stop_idle_secs_by_provider.fake".into()
        )
    );
    let own = worker(
        &shared,
        dir.path(),
        "own",
        json!({"auto_stop_idle_secs": "900"}),
    );
    assert_eq!(setting.bound_for(&own).0, Some(900));
    let low = worker(
        &shared,
        dir.path(),
        "low",
        json!({"auto_stop_idle_secs": 5}),
    );
    assert_eq!(setting.bound_for(&low).0, Some(AUTO_STOP_FLOOR_SECS));
    let zero = worker(
        &shared,
        dir.path(),
        "zero",
        json!({"auto_stop_idle_secs": 0}),
    );
    assert_eq!(setting.bound_for(&zero).0, None);
    let off = worker(
        &shared,
        dir.path(),
        "off",
        json!({"auto_stop": "off", "auto_stop_idle_secs": 900}),
    );
    assert_eq!(
        setting.bound_for(&off),
        (None, "agent auto_stop=off".into())
    );
    // Another provider falls through to the host default.
    let host = idle_row(
        &shared,
        dir.path(),
        "host",
        "claude",
        "managed",
        "worker",
        json!({"upstream": "pm"}),
    );
    assert_eq!(setting.bound_for(&host).0, Some(5400));
    assert_eq!(AutoStopSetting::off().bound_for(&host).0, None);
    assert_eq!(AutoStopSetting::default().bound_for(&host).0, Some(3600));
}

#[test]
fn idle_worker_is_due_only_after_the_bound() {
    let dir = tempfile::tempdir().unwrap();
    let setting = AutoStopSetting::default();
    let shared = pinned(dir.path(), setting.clone());
    let w = worker(&shared, dir.path(), "w1", json!({}));
    let now = epoch_secs();
    // Registration is its newest activity: 59 minutes on, kept.
    let young = verdict(&shared, &setting, &w, now + 59.0 * 60.0);
    assert!(kept(&young).contains("of 3600s"), "{young:?}");
    // Pre-age its whole stream by 72 minutes: due now.
    let age = |secs: f64| {
        rusqlite::Connection::open(dir.path().join("cadence.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE events SET at=at-? WHERE alias='w1'",
                rusqlite::params![secs],
            )
            .unwrap();
    };
    age(72.0 * 60.0);
    match verdict(&shared, &setting, &w, epoch_secs()) {
        AutoStopVerdict::Stop {
            idle_secs,
            bound_secs,
            source,
            ..
        } => {
            assert!(idle_secs >= 71.0 * 60.0, "{idle_secs}");
            assert_eq!(bound_secs, 3600);
            assert_eq!(source, "default");
        }
        other => panic!("expected stop, got {other:?}"),
    }
    // Bookkeeping events never reset the idle clock; turn work does.
    for passive in [
        "quota_updated",
        "params_updated",
        "stop_requested",
        "gc_keep_updated",
    ] {
        shared.store.event_public("w1", passive, json!({})).unwrap();
    }
    assert!(matches!(
        verdict(&shared, &setting, &w, epoch_secs()),
        AutoStopVerdict::Stop { .. }
    ));
    shared
        .store
        .event_public("w1", "turn_finished", json!({}))
        .unwrap();
    let fresh = verdict(&shared, &setting, &w, epoch_secs());
    assert!(kept(&fresh).starts_with("idle"), "{fresh:?}");
    // A resume's `ready` restarts the clock the same way.
    age(72.0 * 60.0);
    shared.store.event_public("w1", "ready", json!({})).unwrap();
    let resumed = verdict(&shared, &setting, &w, epoch_secs());
    assert!(kept(&resumed).starts_with("idle"), "{resumed:?}");
}

#[test]
fn pm_roots_inbox_opted_out_and_unresumable_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let setting = AutoStopSetting::default();
    let shared = pinned(dir.path(), setting.clone());
    let at = epoch_secs() + 10.0 * HOUR;
    let pm = idle_row(&shared, dir.path(), "pm", "fake", "fake", "pm", json!({}));
    assert_eq!(
        kept(&verdict(&shared, &setting, &pm, at)),
        "group root (role pm)"
    );
    // No upstream: its own group root, like `group_root` everywhere.
    let solo = idle_row(
        &shared,
        dir.path(),
        "solo",
        "fake",
        "fake",
        "worker",
        json!({}),
    );
    assert_eq!(
        kept(&verdict(&shared, &setting, &solo, at)),
        "group root (no upstream)"
    );
    // A worker others report to is a sub-PM.
    let lead = worker(&shared, dir.path(), "lead", json!({}));
    worker(&shared, dir.path(), "member", json!({"upstream": "lead"}));
    assert_eq!(
        kept(&verdict(&shared, &setting, &lead, at)),
        "group root (has members)"
    );
    let inbox = idle_row(
        &shared,
        dir.path(),
        "box",
        "inbox",
        "inbox",
        "worker",
        json!({"upstream": "pm"}),
    );
    assert_eq!(kept(&verdict(&shared, &setting, &inbox, at)), "inbox");
    let off = worker(&shared, dir.path(), "off", json!({"auto_stop": "off"}));
    assert!(kept(&verdict(&shared, &setting, &off, at)).contains("auto_stop=off"));
    let nothread = worker(&shared, dir.path(), "nothread", json!({}));
    rusqlite::Connection::open(dir.path().join("cadence.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE agents SET thread_id=NULL WHERE alias='nothread'",
            [],
        )
        .unwrap();
    assert!(kept(&verdict(&shared, &setting, &nothread, at)).contains("not resumable"));
    // The control: an ordinary member idle as long is due.
    let member = shared.store.agent("member").unwrap();
    assert!(matches!(
        verdict(&shared, &setting, &member, at),
        AutoStopVerdict::Stop { .. }
    ));
}

/// Park `alias` the way the timer leaves it — `stopped`, disabled,
/// newest marker `agent_auto_stopped`, one message queued — and
/// return the marker the sweep reads.
fn auto_stopped_with_mail(shared: &Arc<Shared>, dir: &Path, alias: &str) -> store::Event {
    worker(shared, dir, alias, json!({}));
    shared.store.set_enabled(alias, false).unwrap();
    shared
        .store
        .set_state_detached(alias, "stopped", None)
        .unwrap();
    shared
        .store
        .event_public(alias, AUTO_STOP_EVENT, json!({"idle_secs": 7200.0}))
        .unwrap();
    shared
        .store
        .enqueue(alias, "work", None, &format!("m-{alias}"), "user")
        .unwrap();
    shared
        .store
        .last_event_of(alias, AUTO_STOP_MARKER_KINDS)
        .unwrap()
        .unwrap()
}

/// Still parked: stopped, disabled, no actor, and no auto-resume
/// record of either kind — nothing for a needs-me row to show.
fn assert_parked_quietly(shared: &Arc<Shared>, alias: &str) {
    let agent = shared.store.agent(alias).unwrap();
    assert_eq!(agent.state, "stopped", "{alias}");
    assert!(!agent.enabled, "{alias}");
    assert!(!shared.lifecycle.lock().unwrap().owned(alias), "{alias}");
    let resume = shared
        .store
        .last_event_of(alias, &[AUTO_RESUME_EVENT, AUTO_RESUME_FAILED_EVENT])
        .unwrap();
    assert!(resume.is_none(), "{alias}: {resume:?}");
}

/// CAD-413 (qa-1): an operator stop landing between the sweep's
/// marker read and the resume wins — finished or still in flight —
/// and leaves no `agent_auto_resume_failed` behind. The control
/// proves the same call starts an agent whose marker is unchanged.
#[test]
fn auto_resume_loses_to_an_operator_stop_between_check_and_start() {
    let dir = tempfile::tempdir().unwrap();
    let shared = pinned(dir.path(), AutoStopSetting::off());

    // The stop finished after the sweep read `agent_auto_stopped`.
    let seen = auto_stopped_with_mail(&shared, dir.path(), "w-done");
    shared.rpc_stop(&json!({"alias": "w-done"})).unwrap();
    shared.auto_resume("w-done", "m-w-done", 1, &seen);
    assert_parked_quietly(&shared, "w-done");

    // The stop is still in flight: its reservation holds the alias.
    let seen = auto_stopped_with_mail(&shared, dir.path(), "w-flight");
    shared
        .lifecycle
        .lock()
        .unwrap()
        .stopping
        .insert("w-flight".to_string());
    shared.auto_resume("w-flight", "m-w-flight", 1, &seen);
    shared.lifecycle.lock().unwrap().stopping.remove("w-flight");
    assert_parked_quietly(&shared, "w-flight");

    // Control: nothing raced, so the resume records and starts.
    let seen = auto_stopped_with_mail(&shared, dir.path(), "w-ctl");
    shared.auto_resume("w-ctl", "m-w-ctl", 1, &seen);
    assert!(shared.lifecycle.lock().unwrap().owned("w-ctl"));
    let marker = shared
        .store
        .last_event_of("w-ctl", AUTO_STOP_MARKER_KINDS)
        .unwrap()
        .unwrap();
    assert_ne!(marker.kind, AUTO_STOP_EVENT, "{marker:?}");
    assert!(shared
        .store
        .last_event_of("w-ctl", &[AUTO_RESUME_EVENT])
        .unwrap()
        .is_some());
    shared.rpc_stop(&json!({"alias": "w-ctl"})).unwrap();
}

/// A resume recorded but never started (the daemon died between the
/// two) is surfaced as failed on the next sweep, not left waiting
/// silently — and only once.
#[test]
fn auto_resume_recorded_but_never_started_is_reported_failed() {
    let dir = tempfile::tempdir().unwrap();
    let shared = pinned(dir.path(), AutoStopSetting::off());
    auto_stopped_with_mail(&shared, dir.path(), "w1");
    shared
        .store
        .event_public(
            "w1",
            AUTO_RESUME_EVENT,
            json!({"message": "m-w1", "queued": 1}),
        )
        .unwrap();
    shared.auto_resume_tick();
    let failed = shared
        .store
        .last_event_of("w1", AUTO_STOP_MARKER_KINDS)
        .unwrap()
        .unwrap();
    assert_eq!(failed.kind, AUTO_RESUME_FAILED_EVENT, "{failed:?}");
    assert_eq!(failed.payload["message"], "m-w1", "{failed:?}");
    assert!(!shared.lifecycle.lock().unwrap().owned("w1"));
    shared.auto_resume_tick();
    let after = shared
        .store
        .last_event_of("w1", AUTO_STOP_MARKER_KINDS)
        .unwrap()
        .unwrap();
    assert_eq!(after.seq, failed.seq, "reported once: {after:?}");
}

#[test]
fn every_non_terminal_message_state_keeps_the_agent() {
    let dir = tempfile::tempdir().unwrap();
    let setting = AutoStopSetting::default();
    let shared = pinned(dir.path(), setting.clone());
    let at = epoch_secs() + 10.0 * HOUR;
    let conn = rusqlite::Connection::open(dir.path().join("cadence.sqlite3")).unwrap();
    for state in [
        "queued",
        "submitting",
        "running",
        "submitted",
        "awaiting_report",
        "unknown",
        "some_future_state",
    ] {
        let alias = format!("busy-{}", state.replace('_', "-"));
        let w = worker(&shared, dir.path(), &alias, json!({}));
        let id = format!("m-{alias}");
        shared
            .store
            .enqueue(&alias, "work", None, &id, "user")
            .unwrap();
        conn.execute(
            "UPDATE messages SET state=?, created=created-36000 WHERE id=?",
            rusqlite::params![state, id],
        )
        .unwrap();
        let v = verdict(&shared, &setting, &w, at);
        assert!(kept(&v).starts_with("busy: 1 message"), "{state}: {v:?}");
    }
    // Terminal states do not hold an agent.
    for state in ["completed", "failed", "interrupted", "cancelled"] {
        let alias = format!("done-{state}");
        let w = worker(&shared, dir.path(), &alias, json!({}));
        let id = format!("m-{state}");
        shared
            .store
            .enqueue(&alias, "work", None, &id, "user")
            .unwrap();
        conn.execute(
            "UPDATE messages SET state=? WHERE id=?",
            rusqlite::params![state, id],
        )
        .unwrap();
        assert!(
            matches!(
                verdict(&shared, &setting, &w, at),
                AutoStopVerdict::Stop { .. }
            ),
            "{state}"
        );
    }
}

#[test]
fn per_provider_override_is_respected() {
    let dir = tempfile::tempdir().unwrap();
    let mut setting = AutoStopSetting::default();
    setting.by_provider.insert("fake".into(), 3 * 3600);
    setting.by_provider.insert("claude".into(), 0);
    let shared = pinned(dir.path(), setting.clone());
    let now = epoch_secs();
    let w = worker(&shared, dir.path(), "w1", json!({}));
    let early = verdict(&shared, &setting, &w, now + 2.0 * HOUR);
    assert!(kept(&early).contains("by_provider.fake"), "{early:?}");
    assert!(matches!(
        verdict(&shared, &setting, &w, now + 3.5 * HOUR),
        AutoStopVerdict::Stop {
            bound_secs: 10800,
            ..
        }
    ));
    let c = idle_row(
        &shared,
        dir.path(),
        "c1",
        "claude",
        "managed",
        "worker",
        json!({"upstream": "pm"}),
    );
    let off = verdict(&shared, &setting, &c, now + 30.0 * HOUR);
    assert!(kept(&off).contains("auto-stop off"), "{off:?}");
}

#[test]
fn label_and_view_name_the_auto_stop_until_superseded() {
    assert_eq!(auto_stop_label(72.0 * 60.0), "stopped (auto, idle 72m)");
    assert_eq!(auto_stop_label(3.0 * HOUR), "stopped (auto, idle 3h)");
    assert_eq!(
        auto_stop_label(5.0 * HOUR + 1200.0),
        "stopped (auto, idle 5h20m)"
    );
    let dir = tempfile::tempdir().unwrap();
    let shared = pinned(dir.path(), AutoStopSetting::off());
    worker(&shared, dir.path(), "w1", json!({}));
    shared
        .store
        .set_state_detached("w1", "stopped", None)
        .unwrap();
    shared
        .store
        .event_public("w1", "stop_requested", json!({}))
        .unwrap();
    shared
        .store
        .event_public(
            "w1",
            AUTO_STOP_EVENT,
            json!({"idle_secs": 4320.0, "bound_secs": 3600}),
        )
        .unwrap();
    let agent = shared.store.agent("w1").unwrap();
    let markers = shared
        .store
        .last_events_of_all(AUTO_STOP_MARKER_KINDS)
        .unwrap();
    let view = auto_stop_view(&agent, markers.get("w1")).unwrap();
    assert_eq!(view["label"], "stopped (auto, idle 72m)");
    assert_eq!(view["resume"], "cadence agent resume w1");
    // A later manual stop supersedes the marker.
    shared
        .store
        .event_public("w1", "stop_requested", json!({}))
        .unwrap();
    let marker = shared
        .store
        .last_event_of("w1", AUTO_STOP_MARKER_KINDS)
        .unwrap();
    assert!(auto_stop_view(&agent, marker.as_ref()).is_none());
}
