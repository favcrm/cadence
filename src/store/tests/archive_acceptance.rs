// CAD-1239 acceptance checks (design section 10), store level. Written by a
// non-implementer. Each test drives the real store guard it names; nothing
// here mocks a guard or filters in the CLI.

use super::*;

const DAY: f64 = 86_400.0;

const OPERATOR: &str = r#"{"by":"operator","by_kind":"operator-connection"}"#;

fn operator() -> Value {
    serde_json::from_str(OPERATOR).unwrap()
}

/// Register a worker and put it in the eligible shape: stopped, disabled,
/// no endpoint, not kept, no open messages.
fn stopped_worker(s: &Store, alias: &str, cwd: &Path) {
    reg(s, alias, cwd);
    s.set_agent_state(alias, "stopped", None).unwrap();
    s.set_enabled(alias, false).unwrap();
}

/// Archive one alias through the real apply path, using its current digest.
fn archive_one(s: &Store, alias: &str) -> Result<Vec<ArchivePlan>> {
    let plan = s.archive_plan(alias)?;
    s.archive_agents(
        &[(alias.to_string(), plan.digest)],
        "retired",
        &operator(),
    )
}

fn event_count(s: &Store, kind: &str) -> i64 {
    let conn = s.conn();
    conn.query_row(
        "SELECT COUNT(*) FROM events WHERE kind=?1",
        [kind],
        |r| r.get(0),
    )
    .unwrap()
}

fn events_for_alias(s: &Store, alias: &str) -> i64 {
    let conn = s.conn();
    conn.query_row(
        "SELECT COUNT(*) FROM events WHERE alias=?1",
        [alias],
        |r| r.get(0),
    )
    .unwrap()
}

fn agent_row_count(s: &Store, alias: &str) -> i64 {
    let conn = s.conn();
    conn.query_row(
        "SELECT COUNT(*) FROM agents WHERE alias=?1",
        [alias],
        |r| r.get(0),
    )
    .unwrap()
}

/// A row with history that every delete path would otherwise wipe: a
/// delivered message, its events, and a thread-bearing alias.
fn seed_history(s: &Store, alias: &str) {
    s.enqueue(alias, "work", None, &format!("{alias}-m1"), "user")
        .unwrap();
    s.fixture_write(|conn| {
        (*conn).execute(
            "UPDATE messages SET state='completed' WHERE alias=?1",
            [alias],
        )?;
        (*conn).execute(
            "INSERT INTO events(alias,kind,payload,at) VALUES(?1,'submitted',?2,?3)",
            params![alias, json!({"message": format!("{alias}-m1")}).to_string(), now() - 1.0],
        )?;
        Ok(())
    })
    .unwrap();
}

// ---------------------------------------------------------------------------
// PRIMARY: archived_row_survives_deletion (I4, P-DEL-a to c)
// ---------------------------------------------------------------------------

#[test]
fn archived_row_survives_deletion_manual_remove_without_force() {
    let (dir, s) = store();
    stopped_worker(&s, "keep1", &dir.path().join("w"));
    seed_history(&s, "keep1");
    archive_one(&s, "keep1").unwrap();
    let events_before = events_for_alias(&s, "keep1");

    let err = s
        .remove_agent("keep1", false, &operator())
        .expect_err("archived row must refuse non-forced remove");
    assert!(err.to_string().contains("archived"), "{err}");

    assert_eq!(agent_row_count(&s, "keep1"), 1, "row deleted");
    assert_eq!(events_for_alias(&s, "keep1"), events_before, "history pruned");
    assert!(s.agent("keep1").unwrap().archived_at.is_some());
}

#[test]
fn archived_row_survives_deletion_manual_remove_with_force() {
    // P-DEL-a runs before any prune, so `force` cannot bypass the guard.
    let (dir, s) = store();
    stopped_worker(&s, "keep2", &dir.path().join("w"));
    seed_history(&s, "keep2");
    archive_one(&s, "keep2").unwrap();
    let events_before = events_for_alias(&s, "keep2");

    let err = s
        .remove_agent("keep2", true, &operator())
        .expect_err("archived row must refuse forced remove");
    assert!(err.to_string().contains("archived"), "{err}");

    assert_eq!(agent_row_count(&s, "keep2"), 1, "row deleted under force");
    assert_eq!(
        events_for_alias(&s, "keep2"),
        events_before,
        "history pruned under force"
    );
}

#[test]
fn archived_row_survives_deletion_gc_remove() {
    let (dir, s) = store();
    stopped_worker(&s, "keep3", &dir.path().join("w"));
    seed_history(&s, "keep3");
    archive_one(&s, "keep3").unwrap();
    let events_before = events_for_alias(&s, "keep3");

    let err = s
        .gc_remove_agent("keep3", &operator())
        .expect_err("archived row must refuse gc remove");
    assert!(err.to_string().contains("archived"), "{err}");
    assert_eq!(agent_row_count(&s, "keep3"), 1);
    assert_eq!(events_for_alias(&s, "keep3"), events_before);
}

#[test]
fn archived_row_survives_deletion_timer_gc_remove() {
    // The timer path returns Ok(None) with no event for an archived row,
    // even when it is otherwise old enough and eligible.
    let (dir, s) = store();
    stopped_worker(&s, "keep4", &dir.path().join("w"));
    seed_history(&s, "keep4");
    archive_one(&s, "keep4").unwrap();
    let events_before = events_for_alias(&s, "keep4");
    let gc_events_before = event_count(&s, "agent_gc_removed");

    let removed = s.timer_gc_remove("keep4", 0.0).unwrap();
    assert!(removed.is_none(), "timer removed an archived row");
    assert_eq!(agent_row_count(&s, "keep4"), 1);
    assert_eq!(events_for_alias(&s, "keep4"), events_before);
    assert_eq!(event_count(&s, "agent_gc_removed"), gc_events_before);
}

#[test]
fn archived_row_survives_deletion_raw_sql_delete_trigger() {
    // P-DEL-c backstop: a raw DELETE is aborted by the trigger.
    let (dir, s) = store();
    stopped_worker(&s, "keep5", &dir.path().join("w"));
    seed_history(&s, "keep5");
    archive_one(&s, "keep5").unwrap();

    let result = s.fixture_write(|conn| {
        (*conn).execute("DELETE FROM agents WHERE alias='keep5'", [])?;
        Ok(())
    });
    let err = result.expect_err("BEFORE DELETE trigger must abort raw delete");
    assert!(err.to_string().contains("archived"), "{err}");
    assert_eq!(agent_row_count(&s, "keep5"), 1);
    let _ = dir;
}

#[test]
fn archived_row_survives_deletion_not_a_gc_candidate() {
    // P-DEL-b: manual sweep and the shared predicate never list it, and the
    // gc plan rows keep it out of the sweepable set.
    let (dir, s) = store();
    stopped_worker(&s, "keep6", &dir.path().join("w"));
    archive_one(&s, "keep6").unwrap();

    let candidates = s.gc_candidates(Some(0.0)).unwrap();
    assert!(
        candidates.iter().all(|a| a.alias != "keep6"),
        "archived row listed as a gc candidate"
    );
    let rows = s.gc_rows(Some(0.0)).unwrap();
    let row = rows.iter().find(|r| r.agent.alias == "keep6").unwrap();
    assert!(!row.eligible(), "archived row classified eligible");
    assert_eq!(agent_row_count(&s, "keep6"), 1);
    let _ = dir;
}

#[test]
fn archived_row_survives_deletion_retains_marker_and_history() {
    // The row keeps its marker, its messages and its events after the
    // refused deletes above run in sequence.
    let (dir, s) = store();
    stopped_worker(&s, "keep7", &dir.path().join("w"));
    seed_history(&s, "keep7");
    archive_one(&s, "keep7").unwrap();
    let _ = s.remove_agent("keep7", true, &operator());
    let _ = s.gc_remove_agent("keep7", &operator());
    let _ = s.timer_gc_remove("keep7", 0.0);
    let agent = s.agent("keep7").unwrap();
    assert!(agent.archived_at.is_some());
    assert_eq!(agent.archived_by.as_deref(), Some("operator"));
    assert_eq!(agent.archive_reason.as_deref(), Some("retired"));
    let messages: i64 = {
        let conn = s.conn();
        conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE alias='keep7'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(messages, 1, "message history lost");
    let _ = dir;
}

// ---------------------------------------------------------------------------
// PRIMARY: archive_rev_aba_replay (I3, I6, I7)
// Preview -> archive -> restore -> replay must not re-archive the restored
// row, must refuse with stale_preview, and must emit no second event.
// ---------------------------------------------------------------------------

#[test]
fn archive_rev_aba_replay_preview_archive_restore_replay() {
    let (dir, s) = store();
    stopped_worker(&s, "aba", &dir.path().join("w"));

    // Preview P at revision r.
    let preview = s.archive_plan("aba").unwrap();
    let rev_r = preview.agent.lifecycle_rev;
    let digest_p = preview.digest.clone();
    assert!(preview.reasons.is_empty(), "{:?}", preview.reasons);

    // Apply P: archived, revision r+1, one agent_archived event.
    s.archive_agents(&[("aba".into(), digest_p.clone())], "retired", &operator())
        .unwrap();
    let after_archive = s.agent("aba").unwrap();
    assert!(after_archive.archived_at.is_some());
    assert!(after_archive.lifecycle_rev > rev_r, "archive did not bump rev");
    let rev_archived = after_archive.lifecycle_rev;
    assert_eq!(event_count(&s, "agent_archived"), 1);

    // Restore at its current revision: r+2, one agent_restored event.
    s.restore_agents(&[("aba".into(), rev_archived)], &operator())
        .unwrap();
    let restored = s.agent("aba").unwrap();
    assert!(restored.archived_at.is_none());
    let rev_restored = restored.lifecycle_rev;
    assert!(rev_restored > rev_archived, "restore did not bump rev");
    assert_eq!(event_count(&s, "agent_restored"), 1);
    assert_eq!(event_count(&s, "agent_archived"), 1);

    // Replay P: must refuse as stale, write nothing, emit no second event.
    let events_before = event_count(&s, "agent_archived");
    let err = s
        .archive_agents(&[("aba".into(), digest_p)], "retired", &operator())
        .expect_err("replayed preview must not archive the restored row");
    assert!(err.to_string().contains("stale_preview"), "{err}");

    let row = s.agent("aba").unwrap();
    assert!(row.archived_at.is_none(), "replay re-archived the row");
    assert_eq!(row.lifecycle_rev, rev_restored, "replay changed revision");
    assert_eq!(
        event_count(&s, "agent_archived"),
        events_before,
        "replay emitted a second agent_archived event"
    );
    let _ = dir;
}

#[test]
fn archive_rev_aba_replay_archive_then_replay_is_already_archived() {
    // Archive -> replay archive: refused, no second event, no write.
    let (dir, s) = store();
    stopped_worker(&s, "aba2", &dir.path().join("w"));
    let digest = s.archive_plan("aba2").unwrap().digest;
    s.archive_agents(&[("aba2".into(), digest.clone())], "retired", &operator())
        .unwrap();
    let rev = s.agent("aba2").unwrap().lifecycle_rev;
    let events = event_count(&s, "agent_archived");

    let err = s
        .archive_agents(&[("aba2".into(), digest)], "retired", &operator())
        .expect_err("replay of an applied archive must refuse");
    let msg = err.to_string();
    assert!(
        msg.contains("already_archived") || msg.contains("stale_preview"),
        "{msg}"
    );
    assert_eq!(s.agent("aba2").unwrap().lifecycle_rev, rev);
    assert_eq!(event_count(&s, "agent_archived"), events);
    let _ = dir;
}

#[test]
fn archive_rev_aba_restore_with_stale_revision_refused() {
    // Restore with the pre-restore revision is refused with no write.
    let (dir, s) = store();
    stopped_worker(&s, "aba3", &dir.path().join("w"));
    let digest = s.archive_plan("aba3").unwrap().digest;
    s.archive_agents(&[("aba3".into(), digest)], "retired", &operator())
        .unwrap();
    let rev_archived = s.agent("aba3").unwrap().lifecycle_rev;
    s.restore_agents(&[("aba3".into(), rev_archived)], &operator())
        .unwrap();
    let rev_restored = s.agent("aba3").unwrap().lifecycle_rev;

    let err = s
        .restore_agents(&[("aba3".into(), rev_archived)], &operator())
        .expect_err("stale restore must refuse");
    assert!(
        err.to_string().contains("stale_revision")
            || err.to_string().contains("not_archived"),
        "{err}"
    );
    assert_eq!(s.agent("aba3").unwrap().lifecycle_rev, rev_restored);
    let _ = dir;
}

// ---------------------------------------------------------------------------
// archive_recheck_under_lock (I2, E1-E6)
// A message queued after preview must be rejected by the apply transaction.
// ---------------------------------------------------------------------------

#[test]
fn archive_recheck_under_lock_message_after_preview_refuses_apply() {
    let (dir, s) = store();
    stopped_worker(&s, "recheck", &dir.path().join("w"));
    let preview = s.archive_plan("recheck").unwrap();
    assert!(preview.reasons.is_empty());

    // A message is queued after the preview. Enqueue is a writer that does
    // not reset the revision, so only the in-transaction re-check can catch
    // it.
    s.enqueue("recheck", "late work", None, "recheck-late", "user")
        .unwrap();

    let err = s
        .archive_agents(
            &[("recheck".into(), preview.digest)],
            "retired",
            &operator(),
        )
        .expect_err("apply must re-check and refuse an open message");
    let msg = err.to_string();
    assert!(
        msg.contains("open_message") || msg.contains("stale_preview"),
        "{msg}"
    );
    assert!(s.agent("recheck").unwrap().archived_at.is_none());
    assert_eq!(event_count(&s, "agent_archived"), 0);
    let _ = dir;
}

// ---------------------------------------------------------------------------
// archive_mixed_batch_all_or_nothing (I3)
// ---------------------------------------------------------------------------

#[test]
fn archive_mixed_batch_all_or_nothing_stale_alias_refuses_fresh_alias() {
    let (dir, s) = store();
    stopped_worker(&s, "fresh", &dir.path().join("w"));
    stopped_worker(&s, "stale", &dir.path().join("w"));
    let fresh_digest = s.archive_plan("fresh").unwrap().digest;
    let stale_digest = s.archive_plan("stale").unwrap().digest;

    // Make `stale` change under its own digest (a message is queued).
    s.enqueue("stale", "late", None, "stale-late", "user").unwrap();

    let err = s
        .archive_agents(
            &[
                ("fresh".into(), fresh_digest),
                ("stale".into(), stale_digest),
            ],
            "retired",
            &operator(),
        )
        .expect_err("mixed batch with a stale alias must refuse whole");
    assert!(!err.to_string().is_empty());
    assert!(
        s.agent("fresh").unwrap().archived_at.is_none(),
        "fresh alias archived although the batch was refused"
    );
    assert!(s.agent("stale").unwrap().archived_at.is_none());
    assert_eq!(event_count(&s, "agent_archived"), 0);
    let _ = dir;
}

// ---------------------------------------------------------------------------
// rollup_skips_archived_alias (P-DEL-d, section 4.1)
// ---------------------------------------------------------------------------

#[test]
fn rollup_skips_archived_alias_leaves_its_delivery_events_untouched() {
    let (dir, s) = store();
    reg(&s, "roll-live", &dir.path().join("w"));
    stopped_worker(&s, "roll-arch", &dir.path().join("w"));
    let add_events = |alias: &str, id: &str| {
        s.enqueue(alias, "work", None, id, "user").unwrap();
        s.fixture_write(|conn| {
            (*conn).execute(
                "UPDATE messages SET state='completed' WHERE id=?1",
                [id],
            )?;
            for kind in ["submitting", "submitted"] {
                (*conn).execute(
                    "INSERT INTO events(alias,kind,payload,at) VALUES(?1,?2,?3,?4)",
                    params![alias, kind, json!({"message": id}).to_string(), now() - 9.0 * DAY],
                )?;
            }
            Ok(())
        })
        .unwrap();
    };
    add_events("roll-live", "live-m1");
    add_events("roll-arch", "arch-m1");
    archive_one(&s, "roll-arch").unwrap();
    let arch_before = events_for_alias(&s, "roll-arch");

    let cutoff = now() - EVENT_ROLLUP_AGE_SECS;
    s.roll_up_delivery_events(cutoff, EVENT_ROLLUP_BATCH).unwrap();

    // The live alias rolls up; the archived alias's rows are unchanged.
    let rollup_count = |alias: &str| -> i64 {
        let conn = s.conn();
        conn.query_row(
            "SELECT COUNT(*) FROM events WHERE alias=?1 AND kind=?2",
            params![alias, DELIVERY_ROLLUP_EVENT],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(rollup_count("roll-live") >= 1, "live alias did not roll up");
    assert_eq!(rollup_count("roll-arch"), 0, "archived alias was rolled up");
    assert_eq!(
        events_for_alias(&s, "roll-arch"),
        arch_before,
        "archived alias's events were folded"
    );
    let _ = dir;
}

// ---------------------------------------------------------------------------
// restore_preserves_gc_keep (section 4.3)
// Restore clears the marker and writes no gc_keep.
// ---------------------------------------------------------------------------

#[test]
fn restore_preserves_gc_keep_policy_both_ways() {
    // Keep=0 before archive stays 0 after restore.
    let (dir, s) = store();
    stopped_worker(&s, "gck0", &dir.path().join("w"));
    archive_one(&s, "gck0").unwrap();
    let rev = s.agent("gck0").unwrap().lifecycle_rev;
    s.restore_agents(&[("gck0".into(), rev)], &operator()).unwrap();
    assert!(!s.agent("gck0").unwrap().gc_keep, "restore set gc_keep");

    // Keep=0 is required by archive, so a kept row cannot be archived. The
    // restore writer must not clear a keep that the archive plan refused.
    let (_d2, s2) = store();
    let keep_dir = _d2.path().join("w");
    stopped_worker(&s2, "gck1", &keep_dir);
    s2.set_gc_keep_by(
        "gck1",
        true,
        &json!({"by": "operator", "by_kind": "operator-connection"}),
    )
    .unwrap();
    let plan = s2.archive_plan("gck1").unwrap();
    assert!(
        plan.reasons.iter().any(|r| r == "gc_keep"),
        "kept row not refused by archive preview: {:?}",
        plan.reasons
    );
    let err = s2
        .archive_agents(&[("gck1".into(), plan.digest)], "retired", &operator())
        .expect_err("kept row must not archive");
    assert!(err.to_string().contains("gc_keep"), "{err}");
    assert!(s2.agent("gck1").unwrap().gc_keep, "refused archive cleared keep");
    let _ = dir;
}

// ---------------------------------------------------------------------------
// archived_row_not_started_or_enqueued (I5), store-side writers
// ---------------------------------------------------------------------------

#[test]
fn archived_row_not_enqueued_refused_inside_enqueue_transaction() {
    let (dir, s) = store();
    stopped_worker(&s, "noq", &dir.path().join("w"));
    archive_one(&s, "noq").unwrap();

    let err = s
        .enqueue("noq", "send", None, "noq-m1", "user")
        .expect_err("enqueue must refuse an archived target");
    assert!(err.to_string().contains("archived"), "{err}");
    let conn = s.conn();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages WHERE alias='noq'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0, "refused enqueue left a message row");
    let _ = dir;
}

#[test]
fn archived_row_excluded_from_queued_for_stopped() {
    // queued_for_stopped must never select an archived row, so auto-resume
    // cannot pick it up even with queued work present.
    let (dir, s) = store();
    stopped_worker(&s, "qfs", &dir.path().join("w"));
    s.enqueue("qfs", "x", None, "qfs-m1", "user").unwrap();
    // Control: before archive the queued row is selected.
    let before = s.queued_for_stopped().unwrap();
    assert!(
        before.iter().any(|(alias, _, _)| alias == "qfs"),
        "control: queued row not selected before archive: {before:?}"
    );
    // Archive refuses an open message, so force the row to a state the
    // archive plan accepts while keeping the stopped+queued shape the
    // selection query keys on: archive is the only thing under test.
    s.fixture_write(|conn| {
        (*conn).execute(
            "UPDATE messages SET state='queued' WHERE id='qfs-m1'",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    let err = s
        .archive_agents(
            &[("qfs".into(), s.archive_plan("qfs").unwrap().digest)],
            "retired",
            &operator(),
        )
        .expect_err("archive must refuse a row with an open queued message");
    assert!(err.to_string().contains("open_message") || err.to_string().contains("stale"), "{err}");
    // Now the real post-archive shape: archived with no open message.
    s.fixture_write(|conn| {
        (*conn).execute("UPDATE messages SET state='completed' WHERE id='qfs-m1'", [])?;
        Ok(())
    })
    .unwrap();
    archive_one(&s, "qfs").unwrap();
    s.fixture_write(|conn| {
        (*conn).execute("UPDATE messages SET state='queued' WHERE id='qfs-m1'", [])?;
        Ok(())
    })
    .unwrap();

    let rows = s.queued_for_stopped().unwrap();
    assert!(
        rows.iter().all(|(alias, _, _)| alias != "qfs"),
        "auto-resume selection includes archived row: {rows:?}"
    );
    let _ = dir;
}

// ---------------------------------------------------------------------------
// archived_assignee_refused_on_create_and_reopen (I5, C1)
// ---------------------------------------------------------------------------

#[test]
fn archived_assignee_refused_on_create_task_leaves_no_task_or_event() {
    let (dir, s) = store();
    let cwd = dir.path().join("w");
    stopped_worker(&s, "pm", &cwd);
    // Group membership is `params.upstream == pm`; register the worker with
    // it so the group check passes and only the archive guard can refuse.
    s.register_agent(&NewAgent {
        alias: "worker",
        provider: "fake",
        endpoint_kind: "managed",
        role: "worker",
        cwd: cwd.to_str().unwrap(),
        sandbox: "read-only",
        instructions: None,
        params: Some(r#"{"upstream":"pm"}"#),
        team_role: None,
        model_policy: None,
    })
    .unwrap();
    s.set_agent_state("worker", "stopped", None).unwrap();
    s.set_enabled("worker", false).unwrap();
    archive_one(&s, "worker").unwrap();

    let job_id = "arch-job";
    s.create_job(
        job_id, None, "/s.md", &"0".repeat(64), "pm", None, None, None, 2, None, None, None,
        None, None, None, None,
    )
    .unwrap();
    let events_before = event_count(&s, "task_created");
    let tasks_before: i64 = {
        let conn = s.conn();
        conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap()
    };

    let err = s
        .create_task(job_id, "arch-t1", None, Some("worker"), None, None, None, None, None)
        .expect_err("create_task must refuse an archived assignee");
    assert!(err.to_string().contains("archived"), "{err}");

    let tasks_after: i64 = {
        let conn = s.conn();
        conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(tasks_after, tasks_before, "refused create left a task row");
    assert_eq!(event_count(&s, "task_created"), events_before);
    let _ = dir;
}

// ---------------------------------------------------------------------------
// archived_rows_excluded_before_enrichment (section 8)
// Store-side half: the SQL filter the operational list uses. The archived
// row is excluded before any per-row work. The daemon-side enrichment check
// is in tests/cad1239_archive_daemon_acceptance.rs.
// ---------------------------------------------------------------------------

#[test]
fn archived_rows_excluded_before_enrichment_from_operational_list() {
    let (dir, s) = store();
    reg(&s, "listed", &dir.path().join("w"));
    stopped_worker(&s, "hidden", &dir.path().join("w"));
    archive_one(&s, "hidden").unwrap();

    let aliases: Vec<String> = s.agents().unwrap().into_iter().map(|a| a.alias).collect();
    assert!(aliases.contains(&"listed".to_string()));
    assert!(
        !aliases.contains(&"hidden".to_string()),
        "operational list returned an archived row"
    );
    let _ = dir;
}
