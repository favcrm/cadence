//! CAD-2215: scoped monitoring projection stability over the real live stream.
//!
//! The scoped monitor-alert projection is randomized on every read: each alert
//! carries a fresh encrypted `ack` token and the page carries a fresh opaque
//! cursor. Those tokens are not activity. This child module of the real stream
//! fixture drives the signed board's `/api/stream` and proves:
//!
//! * a member-visible alert still produces a `monitoring` frame (the positive
//!   control: the suppression below is not vacuous), and
//! * an alert on a monitor the member cannot see (the private project's
//!   monitor) produces the shared daemon tick — the operator observes its
//!   `monitoring` frame — while the member's own unchanged scoped projection
//!   emits no `monitoring` frame, twice in a row, so repeatedly re-reading
//!   randomized cursor/ack tokens never reads as activity.
//!
//! Ordering is event-driven, never timed: the operator's frame is the barrier
//! proving the shared watcher published the tick, and a visible tracker
//! sentinel proves the member stream was alive past it. Absence of a member
//! frame is judged by frame CLASS (`monitoring`) and hidden identifiers before
//! that sentinel, never by a timeout. Monitors are parked (`freeze_monitors`)
//! so their own checks cannot inject unrelated monitoring churn.

use super::*;

const HIDDEN_ALERT_MARKER: &str = "cad2215-scoped-monitor-hidden-alert";

/// Insert one alert on the PRIVATE project's monitor through the real SQLite
/// store the daemon reads. This is the same fixture mechanism the sibling
/// stream acceptance uses for its visible positive control.
fn hidden_monitor_alert(rig: &Rig, seq: i64) {
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO monitor_alerts\
         (seq,monitor_id,task_id,event_seq,fingerprint,kind,payload,state,created,updated) \
         VALUES(?1,'cad2215-secret-monitor','cad2215-secret-task',?1,?2,'test',?3,'open',2,2)",
        rusqlite::params![
            seq,
            format!("cad2215-scoped-monitor-hidden-{seq}"),
            json!({"marker": HIDDEN_ALERT_MARKER}).to_string(),
        ],
    )
    .unwrap();
}

/// Insert one alert on the READABLE monitor: the member-visible positive
/// control. Distinct sequence from the sibling fixture's own control alert.
fn member_visible_monitor_alert(rig: &Rig, seq: i64) {
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO monitor_alerts\
         (seq,monitor_id,task_id,event_seq,fingerprint,kind,payload,state,created,updated) \
         VALUES(?1,'cad2215-visible-monitor','cad2215-visible-task',?1,?2,'test',?3,'open',2,2)",
        rusqlite::params![
            seq,
            format!("cad2215-scoped-monitor-visible-{seq}"),
            json!({"marker": "cad2215-scoped-monitor-visible"}).to_string(),
        ],
    )
    .unwrap();
}

/// The visible tracker tick that proves the member stream was alive past a
/// hidden daemon tick. It is an `issues`/`issue` frame, never `monitoring`.
fn tracker_sentinel(
    rig: &Rig,
    op: &mut MemberStream,
    entity: &mut MemberStream,
    legacy: &mut MemberStream,
    marker: &str,
) {
    write_issue(rig, "visible", "VIS-1", marker);
    assert!(
        op.through(marker).contains(marker),
        "operator lost the tracker sentinel"
    );
    assert!(
        entity.through(marker).contains(marker),
        "entity member lost the tracker sentinel"
    );
    assert!(
        legacy.through(marker).contains(marker),
        "legacy member lost the tracker sentinel"
    );
    // The seed also creates VIS-9 (the readable request issue). A tracker scan
    // can publish it after the streams open, and a queued VIS-9 frame would
    // then be the member's next frame — mistaken for the change under test.
    // Tick it with its own marker and consume it before the interval begins.
    let request_marker = format!("{marker}-9");
    write_issue(rig, "visible", "VIS-9", &request_marker);
    for (stream, who) in [
        (op, "operator"),
        (entity, "entity member"),
        (legacy, "legacy member"),
    ] {
        assert!(
            stream.through(&request_marker).contains(&request_marker),
            "{who} lost the VIS-9 drain"
        );
    }
}

#[test]
fn repeated_scoped_alert_projections_and_hidden_monitor_activity_emit_no_monitoring_frame() {
    let rig = start(&[]);
    seed(&rig);
    let _keys = initialize_queue_keyring(&rig);
    freeze_monitors(&rig);
    let member = login(&rig, "cad2215-scoped-monitor-member", "member");
    let operator = login(&rig, "cad2215-scoped-monitor-owner", "owner");
    let mut op = MemberStream::open(&rig, &operator, true, "monitor operator");
    let mut entity = MemberStream::open(&rig, &member, true, "monitor member entity");
    let mut legacy = MemberStream::open(&rig, &member, false, "monitor member legacy");

    // Start both member streams on an empty queue with a visible tracker tick,
    // the same drain the sibling monitoring control uses.
    tracker_sentinel(
        &rig,
        &mut op,
        &mut entity,
        &mut legacy,
        "CAD2215_SCOPED_MONITOR_START",
    );

    // Positive control: a readable monitor's new alert is a real member-visible
    // monitoring change and must reach both member client shapes.
    member_visible_monitor_alert(&rig, 221697);
    assert!(
        op.through("event: monitoring")
            .contains("event: monitoring"),
        "operator did not observe the member-visible monitoring change"
    );
    for (stream, who) in [(&mut entity, "entity"), (&mut legacy, "legacy")] {
        let frame = stream.next_data("the member-visible monitoring change");
        assert_eq!(
            event_name(&frame),
            Some("monitoring"),
            "{who} received a non-monitoring frame for a member-visible alert: {frame}"
        );
    }
    tracker_sentinel(
        &rig,
        &mut op,
        &mut entity,
        &mut legacy,
        "CAD2215_SCOPED_MONITOR_DRAIN",
    );

    // Two successive hidden-monitor ticks. Each re-reads the member's scoped
    // monitoring projection (unchanged alerts, fresh random cursor/ack tokens)
    // because the shared watcher published a monitoring tick for the operator's
    // view. Neither may produce a member `monitoring` frame.
    for (round, (alert_seq, checkpoint)) in [
        (221701, "CAD2215_SCOPED_MONITOR_HIDDEN_1"),
        (221702, "CAD2215_SCOPED_MONITOR_HIDDEN_2"),
    ]
    .into_iter()
    .enumerate()
    {
        hidden_monitor_alert(&rig, alert_seq);
        // The barrier: the shared watcher already published this daemon tick,
        // and the operator (who can see the private monitor's counts) observes
        // its monitoring frame.
        assert!(
            op.through("event: monitoring")
                .contains("event: monitoring"),
            "operator did not observe hidden monitoring tick {round}"
        );

        write_issue(&rig, "visible", "VIS-1", checkpoint);
        assert!(op.through(checkpoint).contains(checkpoint));
        for (stream, who) in [(&mut entity, "entity"), (&mut legacy, "legacy")] {
            let interval = stream.until_checkpoint_or_close(checkpoint);
            assert_no_frame_kind(
                &interval,
                &["monitoring"],
                &format!("hidden monitor alert round {round}"),
            );
            assert_no_hidden(
                &interval,
                &[
                    HIDDEN_ALERT_MARKER,
                    "cad2215-secret-monitor",
                    "cad2215-secret-task",
                ],
            );
            assert_sentinel_reached(
                &interval,
                checkpoint,
                &format!("hidden monitor alert round {round} ({who})"),
            );
        }
    }

    // The suppression still delivers a real member-visible monitoring change
    // after two silent rounds: the filter hides tokens, not activity.
    member_visible_monitor_alert(&rig, 221698);
    assert!(
        op.through("event: monitoring")
            .contains("event: monitoring"),
        "operator lost the second member-visible monitoring change"
    );
    for (stream, who) in [(&mut entity, "entity"), (&mut legacy, "legacy")] {
        let frame = stream.next_data("the second member-visible monitoring change");
        assert_eq!(
            event_name(&frame),
            Some("monitoring"),
            "{who} received a non-monitoring frame for the second member-visible alert: {frame}"
        );
    }
}
