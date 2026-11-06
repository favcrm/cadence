//! CAD-2207 — a revoked shared job's private notice must not survive in a
//! subscriber's assistant HISTORY.
//!
//! Independently authored negative acceptance, deliberately separate from the
//! implementation and from every other `team_assistants_refusal` check. This
//! module is a CHILD of that module only to reuse its already-real fixture
//! (`Fx`: a real in-process daemon, a real `operator/board-identity.json` trust
//! root, one loopback authoritative platform answering
//! `/__platform/assistant/revalidate`, and the real `Store`). Every value under
//! test is produced by the REAL code path: the owner's `member_job_request`,
//! the real `member_job_share`/`member_job_subscribe` RPCs on signed member
//! envelopes, the real `assist_request_notice` +
//! `route_assist_job_notifications` intent drain that enqueues the notice into
//! the bound assistant's own thread, and the real `member_assist_history`
//! read. No notice body is hand-written or hand-enqueued: the test reads the
//! delivered message back from the store and uses ITS body, id and task as the
//! secrets that must not survive a revocation. The delivered body is the
//! router's — it names the synthetic job id derived from the owner's
//! `request_key`, so the withheld content is genuinely job-specific rather
//! than a generic placeholder.
//!
//! WHAT THE CONTRACT REQUIRES. After a successful LOCAL owner unshare
//! (`member_job_share {enabled:false}`), a subscriber's NEWLY requested
//! `member_assist_history` page must not disclose the revoked job notice — not
//! its body, its request/job/task identifiers, its message id, or the
//! assistant-origin `source` metadata that marks it as a delivered job event.
//! The scope is FUTURE READS of this daemon's history verb only. A response a
//! caller already received, a provider request already sent, and material a
//! peer already copied are NOT claimed to be erased: the fixture never
//! pretends an earlier response is retracted, and the pre-withdrawal read is
//! deliberately made and asserted.
//!
//! WHAT IS EXERCISED (five independent cases, one fixture each):
//!   * forward paging (`after: 0`, the default page): the notice is visible
//!     while the owner's share is live and gone after the REAL unshare;
//!   * tail / `before` paging (`tail: true`, then an explicit `before` cursor
//!     placed just past the notice's own entry so the revoked entry is a
//!     CANDIDATE of that page): both withhold the notice after the same
//!     unshare;
//!   * re-share: the still-durable subscriber's page shows the notice again
//!     once the OWNER re-enables the share;
//!   * a DELIVERED continuity note: the daemon's own `continuity_pack`/
//!     `delivered` thread row has `message_id` NULL but its payload names the
//!     triggering notice. It is produced by real actor turns on the shipped
//!     in-process provider: one ordinary completed member turn, then the
//!     store's own record of a session the provider did not carry over
//!     (`clear_native_session`), so the next open schedules a pack and the
//!     live-share notice turn builds it. A fresh signed history after the
//!     unshare must not return the note or its notice id;
//!   * a SKIPPED (non-delivered) continuity note: the daemon's own
//!     `continuity_settle` row (`outcome: "skipped"`) names the notice the
//!     same way. Reaching the production `Ok(None)` branch for a notice turn
//!     needs no carryable prior turn, so the due reason is seeded on the
//!     daemon's own due map (`Reason::Compacted`); the NOTE itself is written
//!     by the production `continuity_settle`, never constructed here. A real
//!     provider-compaction turn cannot reach this branch because its own
//!     delivered turn is carryable (and the compaction marker itself is
//!     filtered), which is why the due map is seeded.
//!
//! POSITIVE CONTROLS IN THE SAME FIXTURE (a blanket hide cannot pass any
//! case): the subscriber's OWN member message in its bound conversation stays
//! readable in every post-withdrawal page, and the OWNER's own history still
//! contains the SAME delivered notice body (the owner never lost the job). The
//! pre-withdrawal read is itself the control that the notice really is in the
//! subscriber's thread, with the exact delivered body, before the share is
//! withdrawn — so "absent after" cannot be confused with "never there". No
//! check asserts refusal wording, and no ASSERTION reads a lock or a private
//! helper: the skipped case's due seed is fixture setup (explained above), and
//! every withheld/served claim is made from the public signed RPC and the real
//! thread rows.
//!
//! BASELINE FAILURE (at `50c5f725`): `Shared::rpc_member_assist_history`
//! resolved the bound thread and returned
//! `Store::thread_entries_of` / `thread_entries_before_of` with only
//! `operator_only_entries_hidden` applied (an artifact/hire-proposal KIND
//! filter, `src/daemon/members_rpc.rs`). `Store::thread_note_enqueued` wrote
//! the notice body, its `message_id` and `{"source":"assist_job_event"}` at
//! enqueue time, and `Store::assist_job_share(.., enabled=false)` deletes only
//! the `assist_job_shares` row — the durable subscriber row, the message and
//! its `assist_notice_deliveries` receipt all survive. The actor's own
//! continuity notes (`Shared::record_continuity` / `continuity_settle`) are
//! `KIND_MESSAGE` thread rows with `message_id` NULL whose payload carries
//! `"message": <notice id>`, so the same read returned those too. At that
//! baseline a NEW history read still returned the revoked notice — and the
//! note naming it — in every paging mode; all five cases failed on the
//! withheld-notice assertions.
//!
//! NOT COVERED (reported, never simulated): remote membership/session/
//! credential revocation (CAD-2148 custody and CAD-2061 claims are separate
//! dependencies); the bound assistant's own generic `thread_read` path and a
//! continuity pack's assembled provider prompt (independent retained-read
//! surfaces owned by other checks); a copy already sent to a provider; and
//! unknown-provenance entries (a notice-shaped row with no delivery receipt)
//! or store-error handling. Among the non-delivered continuity outcomes,
//! `failed` (needs `continuity::assemble` to error — a store/scrub failure no
//! honest fixture induces) and `omitted` (needs a mandatory task context,
//! which an `assist_job_event` notice has no pin for, plus an over-cap combined
//! prompt) are NOT reachable for a notice turn in this fixture; `skipped` is
//! exercised with its due reason seeded on the daemon's own map as explained
//! above. Re-sharing is asserted only for the same durable subscriber whose
//! row the unshare deliberately does not remove — it is not claimed for a
//! subscription created after the withdrawal, for another member's envelope,
//! or for a design that records a permanent revocation tombstone instead of
//! following the live share.
//!
//! Run just this module's acceptances (after the module is registered):
//! `scripts/boat-build -- cargo test --locked --features test-seam --lib
//! cad2207_ -- --test-threads 1`

use super::*;

/// The member who owns the job and grants/withdraws its share.
const CAD2207_OWNER: &str = "sub-alice";
/// The second member who subscribes to the shared job.
const CAD2207_SUBSCRIBER: &str = "sub-ben";
/// The owner's canonical request key. The job id is derived from it, so the
/// delivered notice carries a job identifier that exists nowhere else.
const CAD2207_REQUEST_KEY: &str = "cad2207-history-revocation-job";
/// The owner's synthetic job title — the request's own private content.
const CAD2207_JOB_TITLE: &str = "cad2207 history revocation private job";
/// The subscriber's own ordinary assistant message — the control content that
/// must remain readable across the revocation.
const CAD2207_OWN_TEXT: &str = "cad2207 history revocation: the subscriber's own private note";
/// The assistant-origin marker a delivered job notice's thread entry carries.
const CAD2207_NOTICE_SOURCE: &str = "assist_job_event";
/// Client-side message ids for the subscriber's ordinary turn (the identifier
/// grammar is lowercase letters, digits and hyphens).
const CAD2207_OWN_MESSAGE: &str = "cad2207-history-own-note";

/// The real stored state one fixture seeds, plus the exact secrets a revoked
/// subscriber page must withhold.
struct Cad2207History {
    subscriber_alias: String,
    subscriber_thread: String,
    owner_alias: String,
    job: String,
    task: String,
    notice: crate::store::Message,
    owner_notice: crate::store::Message,
}

/// The delivered `assist_job_event` notice for ONE job on ONE alias — found by
/// the durable `assist_notice_deliveries` receipt, never by body text.
fn cad2207_notice(fx: &Fx, alias: &str, job: &str) -> crate::store::Message {
    fx.shared
        .store
        .messages(alias)
        .unwrap_or_else(|error| panic!("could not read '{alias}' messages: {error}"))
        .into_iter()
        .find(|message| {
            message.source == CAD2207_NOTICE_SOURCE
                && fx
                    .shared
                    .store
                    .assist_notice_delivery(&message.id)
                    .unwrap()
                    .is_some_and(|delivery| delivery.job == job)
        })
        .unwrap_or_else(|| panic!("no delivered assist notice for {job} on '{alias}'"))
}

/// One REAL `member_assist_history` call on a fresh signed envelope. `extra`
/// supplies only the paging fields the handler accepts.
fn cad2207_history(fx: &Fx, sub: &str, extra: Value) -> Value {
    let mut params = json!({"member_envelope": fx.member_envelope(sub)});
    if let Some(extra) = extra.as_object() {
        for (key, value) in extra {
            params[key.as_str()] = value.clone();
        }
    }
    fx.op("member_assist_history", params)
}

fn cad2207_entries(history: &Value) -> &[Value] {
    history["entries"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("member_assist_history returned no entries array: {history}"))
}

fn cad2207_has_text(history: &Value, text: &str) -> bool {
    cad2207_entries(history)
        .iter()
        .any(|entry| entry.get("text").and_then(Value::as_str) == Some(text))
}

fn cad2207_has_message(history: &Value, id: &str) -> bool {
    cad2207_entries(history)
        .iter()
        .any(|entry| entry.get("message").and_then(Value::as_str) == Some(id))
}

fn cad2207_entry_seq(history: &Value, id: &str) -> i64 {
    cad2207_entries(history)
        .iter()
        .find(|entry| entry.get("message").and_then(Value::as_str) == Some(id))
        .and_then(|entry| entry.get("seq").and_then(Value::as_i64))
        .unwrap_or_else(|| panic!("no history entry for message {id}: {history}"))
}

/// Serve an already-bound member assistant on the shipped in-process provider
/// (`endpoint_kind = "fake"`). Binding, principal cwd, private conversation and
/// every member guard are unchanged — only the launch surface differs, exactly
/// as the sibling share-revocation fixture does.
fn cad2207_serve_on_fake(fx: &Fx, alias: &str) {
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET provider='fake', endpoint_kind='fake' WHERE alias=?",
                rusqlite::params![alias],
            )?;
            Ok(())
        })
        .unwrap_or_else(|error| panic!("could not serve '{alias}' on the fake provider: {error}"));
    fx.shared
        .store
        .set_enabled(alias, true)
        .unwrap_or_else(|error| panic!("could not enable '{alias}': {error}"));
}

/// Stop the fixture actor and wait for ownership to clear, so no turn runs
/// while the post-withdrawal reads are made.
fn cad2207_quiet(fx: &Fx, alias: &str) {
    let _ = fx.shared.rpc_stop(&json!({"alias": alias}));
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let owned = fx
            .shared
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .owned(alias);
        if !owned {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the fixture actor for '{alias}' did not stop"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait, bounded, until a queued message reaches a terminal state.
fn cad2207_settle(fx: &Fx, id: &str) -> crate::store::Message {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let message = fx
            .shared
            .store
            .message(id)
            .unwrap_or_else(|error| panic!("message '{id}' could not be read: {error}"))
            .unwrap_or_else(|| panic!("message '{id}' vanished"));
        if matches!(
            message.state.as_str(),
            "completed" | "failed" | "cancelled" | "rejected" | "interrupted" | "unknown"
        ) {
            return message;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "message '{id}' never settled (state '{}')",
            message.state
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One REAL member send with a client message id, so the test knows the queued
/// message's id without parsing the receipt.
fn cad2207_send_member(fx: &Fx, text: &str, id: &str) {
    fx.op(
        "member_assist_send",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_SUBSCRIBER),
            "text": text,
            "message": id,
        }),
    );
    let stored = fx
        .shared
        .store
        .message(id)
        .unwrap_or_else(|error| panic!("the member send '{id}' could not be read: {error}"))
        .unwrap_or_else(|| panic!("the member send '{id}' was not queued"));
    assert_eq!(
        stored.body, text,
        "the queued member send carries another body"
    );
}

/// The REAL continuity note the daemon wrote for `notice`: a bookkeeping
/// thread row (`message_id` NULL) whose payload names the triggering notice.
/// `outcome` selects the production writer's branch (`delivered`, `skipped`,
/// `failed`, `omitted`). Never a test-constructed payload.
fn cad2207_note_entry(
    fx: &Fx,
    thread_id: &str,
    notice_id: &str,
    outcome: &str,
) -> crate::store::ThreadEntry {
    fx.shared
        .store
        .thread_entries_of(thread_id, 0, 1000)
        .unwrap_or_else(|error| panic!("thread '{thread_id}' could not be read: {error}"))
        .into_iter()
        .find(|entry| {
            let event = entry
                .payload
                .as_ref()
                .and_then(|payload| payload.get("event"))
                .and_then(Value::as_str);
            let message = entry
                .payload
                .as_ref()
                .and_then(|payload| payload.get("message"))
                .and_then(Value::as_str);
            let recorded = entry
                .payload
                .as_ref()
                .and_then(|payload| payload.get("outcome"))
                .and_then(Value::as_str);
            entry.message_id.is_none()
                && event == Some(crate::continuity::PACK_EVENT)
                && message == Some(notice_id)
                && recorded == Some(outcome)
        })
        .unwrap_or_else(|| {
            panic!("no '{outcome}' continuity note references notice {notice_id} in '{thread_id}'")
        })
}

/// Replace the actor's opening `Reason::New` due-ness with the `Compacted`
/// reason the production `continuity_parts` branch reads. The opening write is
/// the daemon's own in-memory map; removing it first cannot race the actor
/// (the queue is empty), and the NOTE itself is written later by the real
/// `continuity_settle` — this test never constructs the payload.
fn cad2207_seed_compacted_due(fx: &Fx, alias: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let taken = fx
            .shared
            .continuity_due
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(alias)
            .is_some();
        if taken {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the fixture actor never scheduled its opening continuity pack"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    fx.shared
        .continuity_due
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(alias.to_string(), crate::continuity::Reason::Compacted);
}

/// The DELIVERED continuity-note scenario. The subscriber's assistant runs on
/// the shipped in-process provider for one ordinary completed member turn, so
/// the thread has a carryable turn. The actor is then stopped and its stored
/// native session cleared — the store's own record of a session the provider
/// did not carry over — so the next open schedules `Reason::New`. The next
/// turn is the live-share notice, so the production `continuity_parts` builds
/// a real pack and `record_continuity` writes the `continuity_pack`/
/// `delivered` note naming the notice. Nothing here writes the note.
fn cad2207_delivered_note_fixture(fx: &Fx) -> Cad2207History {
    let owner = fx.member_runtime(CAD2207_OWNER);
    let subscriber = fx.member_runtime(CAD2207_SUBSCRIBER);
    cad2207_serve_on_fake(fx, &subscriber.alias);
    // Start the fake actor explicitly, exactly as the sibling continuity
    // fixture does. Relying on `member_assist_send`'s lazy launch left the
    // message `queued` in this fixture: the send is admitted either way, and
    // only the launch attempt can report the endpoint did not start. With the
    // actor already owned, the send is queued for it and the turn drains.
    fx.shared
        .launch_actor(&subscriber.alias)
        .unwrap_or_else(|error| panic!("the subscriber's assistant did not start: {error}"));

    cad2207_send_member(fx, CAD2207_OWN_TEXT, CAD2207_OWN_MESSAGE);
    assert_eq!(
        cad2207_settle(fx, CAD2207_OWN_MESSAGE).state,
        "completed",
        "the subscriber's own turn did not complete"
    );
    cad2207_quiet(fx, &subscriber.alias);

    // The notice arrives while the share is live, with no actor running.
    let (job, task) = cad2207_shared_job(fx);
    let notice = cad2207_notice(fx, &subscriber.alias, &job);
    let owner_notice = cad2207_notice(fx, &owner.alias, &job);
    assert_eq!(
        notice.task_id.as_deref(),
        Some(task.as_str()),
        "the delivered notice does not carry the job's own task"
    );

    // A fresh provider session is recorded the way the daemon records one it
    // cannot resume: `clear_native_session` drops the stored thread/session id,
    // so the next open schedules the pack. The notice is the first turn.
    fx.shared
        .store
        .clear_native_session(&subscriber.alias)
        .unwrap_or_else(|error| panic!("could not clear the stored session: {error}"));
    fx.shared
        .store
        .set_enabled(&subscriber.alias, true)
        .unwrap_or_else(|error| panic!("could not re-enable the subscriber: {error}"));
    fx.shared
        .launch_actor(&subscriber.alias)
        .unwrap_or_else(|error| panic!("the subscriber's assistant did not restart: {error}"));

    assert_eq!(
        cad2207_settle(fx, &notice.id).state,
        "completed",
        "the notice turn did not complete under the live share"
    );
    cad2207_quiet(fx, &subscriber.alias);

    Cad2207History {
        subscriber_alias: subscriber.alias,
        subscriber_thread: subscriber.thread_id,
        owner_alias: owner.alias,
        job,
        task,
        notice,
        owner_notice,
    }
}

/// The SKIPPED continuity-note scenario (`outcome: "skipped"`). There is no
/// carryable prior turn, so the production `continuity::assemble` returns
/// nothing to carry and `continuity_settle` records the non-delivered note
/// naming the notice. The due reason is seeded on the daemon's own map because
/// a real provider-compaction turn would leave a carryable turn behind (the
/// marker note itself is filtered) — the note writer is still production.
fn cad2207_skipped_note_fixture(fx: &Fx) -> Cad2207History {
    let owner = fx.member_runtime(CAD2207_OWNER);
    let subscriber = fx.member_runtime(CAD2207_SUBSCRIBER);
    cad2207_serve_on_fake(fx, &subscriber.alias);
    fx.shared
        .launch_actor(&subscriber.alias)
        .unwrap_or_else(|error| panic!("the subscriber's assistant did not start: {error}"));
    cad2207_seed_compacted_due(fx, &subscriber.alias);

    let (job, task) = cad2207_shared_job(fx);
    let notice = cad2207_notice(fx, &subscriber.alias, &job);
    let owner_notice = cad2207_notice(fx, &owner.alias, &job);
    assert_eq!(
        notice.task_id.as_deref(),
        Some(task.as_str()),
        "the delivered notice does not carry the job's own task"
    );
    assert_eq!(
        cad2207_settle(fx, &notice.id).state,
        "completed",
        "the notice turn did not complete under the live share"
    );

    // The member's own ordinary turn is queued AFTER the notice, so it was not
    // carryable when the pack assembled, and it is now part of the history.
    cad2207_send_member(fx, CAD2207_OWN_TEXT, CAD2207_OWN_MESSAGE);
    assert_eq!(
        cad2207_settle(fx, CAD2207_OWN_MESSAGE).state,
        "completed",
        "the subscriber's own turn did not complete"
    );
    cad2207_quiet(fx, &subscriber.alias);

    Cad2207History {
        subscriber_alias: subscriber.alias,
        subscriber_thread: subscriber.thread_id,
        owner_alias: owner.alias,
        job,
        task,
        notice,
        owner_notice,
    }
}

/// Every identifiable trace of the revoked notice must be gone from the page:
/// its body, its message id, the job/task it names, and the assistant-origin
/// source marker. The check runs over the serialized WHOLE response (so
/// conversation metadata, entry payloads and cursors are all covered) and
/// again per entry, so a redacted-but-still-identified entry cannot pass.
fn cad2207_assert_withheld(what: &str, history: &Value, scenario: &Cad2207History) {
    let text = history.to_string();
    for (label, secret) in [
        ("notice body", scenario.notice.body.as_str()),
        ("notice message id", scenario.notice.id.as_str()),
        ("job id", scenario.job.as_str()),
        ("task id", scenario.task.as_str()),
    ] {
        assert!(!secret.is_empty(), "{what}: the {label} is empty");
        assert!(
            !text.contains(secret),
            "{what} disclosed the {label} '{secret}': {history}"
        );
    }
    assert!(
        !text.contains(CAD2207_NOTICE_SOURCE),
        "{what} disclosed the assistant-origin source '{CAD2207_NOTICE_SOURCE}': {history}"
    );
    for entry in cad2207_entries(history) {
        assert_ne!(
            entry.get("message").and_then(Value::as_str),
            Some(scenario.notice.id.as_str()),
            "{what} returned the revoked notice entry: {entry}"
        );
        let source = entry
            .get("payload")
            .and_then(|payload| payload.get("source"))
            .and_then(Value::as_str);
        assert_ne!(
            source,
            Some(CAD2207_NOTICE_SOURCE),
            "{what} returned an assistant-origin entry: {entry}"
        );
    }
}

/// The owner's canonical job + REAL share + the second member's REAL
/// subscription + ONE notice per bound thread from the real intent drain.
/// Returns the canonical job id and its task id (the delivered notice names
/// both). Nothing is synthesized or hand-enqueued.
fn cad2207_shared_job(fx: &Fx) -> (String, String) {
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_OWNER),
            "title": CAD2207_JOB_TITLE,
            "request_key": CAD2207_REQUEST_KEY,
            "body": "cad2207 history revocation: the owner's private job spec",
            "acceptance": "no revoked notice survives a subscriber's new history read",
        }),
    );
    let job = request
        .get("job")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("member_job_request returned no job: {request}"))
        .to_string();
    fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_OWNER),
            "job": job,
            "enabled": true,
        }),
    );
    fx.op(
        "member_job_subscribe",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_SUBSCRIBER),
            "job": job,
        }),
    );
    assert!(
        fx.shared.store.assist_job_is_shared(&job, ORG).unwrap(),
        "the fixture's owner share did not land"
    );
    assert!(
        fx.shared
            .store
            .job_is_subscriber(&job, ORG, CAD2207_SUBSCRIBER)
            .unwrap(),
        "the fixture did not create the durable subscriber row"
    );
    // The REAL intent drain writes the notice (and its delivery receipt) into
    // both bound threads. The body is the router's, never hand-written here.
    fx.shared
        .store
        .assist_request_notice(&job)
        .unwrap_or_else(|error| panic!("the assist notice intent did not commit: {error}"));
    fx.shared
        .route_assist_job_notifications()
        .unwrap_or_else(|error| panic!("the assist intent drain failed: {error}"));
    (job.clone(), format!("{job}-t1"))
}

/// Build one full real scenario: two provisioned members, the subscriber's own
/// ordinary message, the owner's canonical job, the owner's REAL share, the
/// second member's REAL subscription, and ONE delivered notice in each bound
/// thread produced by the real intent drain (which writes the receipts the
/// turn re-prove requires). Nothing is synthesized or hand-enqueued.
fn cad2207_history_fixture(fx: &Fx) -> Cad2207History {
    let owner = fx.member_runtime(CAD2207_OWNER);
    let subscriber = fx.member_runtime(CAD2207_SUBSCRIBER);

    // The subscriber's own ordinary conversation. `member_assist_send` commits
    // the member's thread entry BEFORE it tries to launch the endpoint, so the
    // entry is synchronous. The fixture marks the endpoint operator-stopped
    // first, so the launch is truthfully refused and NO actor is started: the
    // send stays queued in the subscriber's own thread and nothing else drains
    // the queue while this test reads.
    //
    // The stop is a STORE write, not the generic `agent_stop` verb: CAD-2207
    // requires an unbound operator lifecycle verb to refuse a bound member
    // assistant (its own acceptance), so this fixture sets the same durable
    // `enabled=0` state directly, exactly as the sibling
    // `cad2207_serve_on_fake_provider` sets `enabled=1` — the member send's
    // admission and thread write are unchanged either way.
    fx.shared
        .store
        .set_enabled(&subscriber.alias, false)
        .unwrap_or_else(|error| panic!("could not stop the subscriber's assistant: {error}"));
    let sent = fx.op(
        "member_assist_send",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_SUBSCRIBER),
            "text": CAD2207_OWN_TEXT,
        }),
    );
    assert!(
        sent.get("message").and_then(Value::as_str).is_some()
            || sent.get("id").and_then(Value::as_str).is_some()
            || sent.get("queued").and_then(Value::as_bool) == Some(true),
        "the subscriber's own send is not a send receipt: {sent}"
    );
    assert!(
        !fx.shared
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .owned(&subscriber.alias),
        "the fixture launched the stopped subscriber assistant; the own-conversation control \
         is then no longer a quiet thread"
    );

    // The owner's canonical job + REAL share + the second member's REAL
    // subscription and ONE delivered notice per bound thread, all through the
    // signed member verbs and the real intent drain.
    let (job, task) = cad2207_shared_job(fx);

    let notice = cad2207_notice(fx, &subscriber.alias, &job);
    let owner_notice = cad2207_notice(fx, &owner.alias, &job);
    assert_eq!(
        notice.task_id.as_deref(),
        Some(task.as_str()),
        "the delivered notice does not carry the job's own task"
    );
    // The delivered body really names the synthetic job/task. The secrets the
    // post-withdrawal pages must withhold are therefore present in the
    // authorized notice, not invented by this test.
    assert!(
        notice.body.contains(job.as_str()),
        "the delivered notice body does not name the job: {}",
        notice.body
    );
    assert!(
        notice.body.contains(task.as_str()),
        "the delivered notice body does not name the task: {}",
        notice.body
    );
    assert!(
        notice.body.contains("awaiting_approval"),
        "the delivered body is not the request notice: {}",
        notice.body
    );

    Cad2207History {
        subscriber_alias: subscriber.alias,
        subscriber_thread: subscriber.thread_id,
        owner_alias: owner.alias,
        job,
        task,
        notice,
        owner_notice,
    }
}

/// The owner's REAL withdrawal. It must complete (not refuse), actually clear
/// the share row, and leave the durable subscriber row in place — otherwise
/// the "hidden after" assertions could be explained by a removed subscription
/// instead of the withdrawn share.
fn cad2207_unshare(fx: &Fx, job: &str) -> Value {
    let answer = fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_OWNER),
            "job": job,
            "enabled": false,
        }),
    );
    assert_eq!(
        answer["shared"],
        json!(false),
        "member_job_share did not report the withdrawal: {answer}"
    );
    assert!(
        !fx.shared.store.assist_job_is_shared(job, ORG).unwrap(),
        "member_job_share reported shared:false without clearing the share row"
    );
    assert!(
        fx.shared
            .store
            .job_is_subscriber(job, ORG, CAD2207_SUBSCRIBER)
            .unwrap(),
        "the durable subscriber row vanished — the withdrawal must not retro-remove it"
    );
    answer
}

/// CAD-2207 (negative acceptance): the DEFAULT forward page of a subscriber's
/// `member_assist_history` must stop disclosing a job notice the owner has
/// unshared, while the subscriber's own message and the owner's own notice
/// stay readable.
#[test]
fn cad2207_revoked_share_is_withheld_from_forward_subscriber_history() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let scenario = cad2207_history_fixture(&fx);

    // Control BEFORE the withdrawal: the notice really is in the subscriber's
    // own bound thread, with the exact delivered body.
    let shared = cad2207_history(&fx, CAD2207_SUBSCRIBER, json!({"after": 0, "limit": 100}));
    assert_eq!(
        shared["conversation"]["id"],
        json!(scenario.subscriber_thread),
        "the subscriber's history left its own bound conversation: {shared}"
    );
    assert_eq!(
        shared["conversation"]["alias"],
        json!(scenario.subscriber_alias),
        "the subscriber's history served another alias's conversation: {shared}"
    );
    assert!(
        cad2207_has_message(&shared, &scenario.notice.id),
        "the shared notice is not in the subscriber's history before the withdrawal: {shared}"
    );
    assert!(
        cad2207_has_text(&shared, &scenario.notice.body),
        "the shared notice body is not in the subscriber's history before the withdrawal: \
         {shared}"
    );
    assert!(
        cad2207_has_text(&shared, CAD2207_OWN_TEXT),
        "the subscriber's own message was not in its history even before the withdrawal: \
         {shared}"
    );

    cad2207_unshare(&fx, &scenario.job);

    // A NEW forward request after the completed withdrawal.
    let revoked = cad2207_history(&fx, CAD2207_SUBSCRIBER, json!({"after": 0, "limit": 100}));
    assert_eq!(
        revoked["conversation"]["id"],
        json!(scenario.subscriber_thread),
        "the post-withdrawal read no longer serves the subscriber's own thread: {revoked}"
    );
    assert_eq!(
        revoked["conversation"]["alias"],
        json!(scenario.subscriber_alias),
        "the post-withdrawal read served another alias's conversation: {revoked}"
    );
    cad2207_assert_withheld(
        "the subscriber's forward history after the withdrawal",
        &revoked,
        &scenario,
    );
    assert!(
        cad2207_has_text(&revoked, CAD2207_OWN_TEXT),
        "the subscriber's own ordinary message disappeared with the revocation: {revoked}"
    );

    // The owner's own notice is unaffected: the owner never lost the job.
    let owner = cad2207_history(&fx, CAD2207_OWNER, json!({"after": 0, "limit": 100}));
    assert_eq!(
        owner["conversation"]["alias"],
        json!(scenario.owner_alias),
        "the owner's history served another alias's conversation: {owner}"
    );
    assert!(
        cad2207_has_message(&owner, &scenario.owner_notice.id),
        "the owner's own notice disappeared after the subscriber's withdrawal: {owner}"
    );
    assert!(
        cad2207_has_text(&owner, &scenario.owner_notice.body),
        "the owner's own notice body disappeared after the subscriber's withdrawal: {owner}"
    );
}

/// CAD-2207 (negative acceptance): the TAIL and `before` pages of the same
/// subscriber history must withhold the revoked notice too. The `before`
/// cursor is taken from the notice's own entry, so the revoked entry is a real
/// candidate of that page rather than excluded by the cursor.
#[test]
fn cad2207_revoked_share_is_withheld_from_tail_and_before_pages() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let scenario = cad2207_history_fixture(&fx);

    let shared_tail = cad2207_history(&fx, CAD2207_SUBSCRIBER, json!({"tail": true, "limit": 100}));
    assert!(
        cad2207_has_message(&shared_tail, &scenario.notice.id),
        "the shared notice is not in the subscriber's tail page before the withdrawal: \
         {shared_tail}"
    );
    let notice_seq = cad2207_entry_seq(&shared_tail, &scenario.notice.id);
    assert!(
        cad2207_has_text(&shared_tail, CAD2207_OWN_TEXT),
        "the subscriber's own message was not in its tail page even before the withdrawal: \
         {shared_tail}"
    );

    cad2207_unshare(&fx, &scenario.job);

    let tail = cad2207_history(&fx, CAD2207_SUBSCRIBER, json!({"tail": true, "limit": 100}));
    cad2207_assert_withheld(
        "the subscriber's tail history after the withdrawal",
        &tail,
        &scenario,
    );
    assert!(
        cad2207_has_text(&tail, CAD2207_OWN_TEXT),
        "the subscriber's own ordinary message disappeared from the post-withdrawal tail page: \
         {tail}"
    );

    let before = cad2207_history(
        &fx,
        CAD2207_SUBSCRIBER,
        json!({"before": notice_seq + 1, "limit": 100}),
    );
    cad2207_assert_withheld(
        "the subscriber's `before` history page after the withdrawal",
        &before,
        &scenario,
    );
    assert!(
        cad2207_has_text(&before, CAD2207_OWN_TEXT),
        "the subscriber's own ordinary message disappeared from the post-withdrawal `before` \
         page: {before}"
    );

    // The owner's tail page still carries the owner's own notice.
    let owner = cad2207_history(&fx, CAD2207_OWNER, json!({"tail": true, "limit": 100}));
    assert_eq!(
        owner["conversation"]["alias"],
        json!(scenario.owner_alias),
        "the owner's tail page served another alias's conversation: {owner}"
    );
    assert!(
        cad2207_has_text(&owner, &scenario.owner_notice.body),
        "the owner's own notice body disappeared from the owner's tail page: {owner}"
    );
}

/// CAD-2207 control: the OWNER can re-grant the share for the still-durable
/// subscriber, and the subscriber's next history page shows the notice again —
/// the same authorized job notice, in the subscriber's OWN bound conversation,
/// with the subscriber's own message. This encodes the live-share reading the
/// withdrawal path documents ("existing subscribers are not retro-removed");
/// it deliberately claims nothing about a subscription created after the
/// withdrawal, another member's envelope, or a tombstone design.
#[test]
fn cad2207_reshare_restores_the_durable_subscriber_authorized_notice() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let scenario = cad2207_history_fixture(&fx);

    let shared = cad2207_history(&fx, CAD2207_SUBSCRIBER, json!({"tail": true, "limit": 100}));
    assert!(
        cad2207_has_message(&shared, &scenario.notice.id),
        "the shared notice is not in the subscriber's history before the withdrawal: {shared}"
    );

    cad2207_unshare(&fx, &scenario.job);
    let revoked = cad2207_history(&fx, CAD2207_SUBSCRIBER, json!({"tail": true, "limit": 100}));
    cad2207_assert_withheld(
        "the subscriber's history after the withdrawal",
        &revoked,
        &scenario,
    );
    assert!(
        cad2207_has_text(&revoked, CAD2207_OWN_TEXT),
        "the subscriber's own ordinary message disappeared with the withdrawal: {revoked}"
    );

    let again = fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_OWNER),
            "job": scenario.job,
            "enabled": true,
        }),
    );
    assert_eq!(
        again["shared"],
        json!(true),
        "member_job_share did not report the new grant: {again}"
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&scenario.job, ORG)
            .unwrap(),
        "member_job_share reported shared:true without writing the share row"
    );

    let restored = cad2207_history(&fx, CAD2207_SUBSCRIBER, json!({"tail": true, "limit": 100}));
    assert_eq!(
        restored["conversation"]["id"],
        json!(scenario.subscriber_thread),
        "the re-shared read no longer serves the subscriber's own thread: {restored}"
    );
    assert_eq!(
        restored["conversation"]["alias"],
        json!(scenario.subscriber_alias),
        "the re-shared read served another alias's conversation: {restored}"
    );
    assert!(
        cad2207_has_message(&restored, &scenario.notice.id),
        "re-sharing did not restore the subscriber's authorized job notice: {restored}"
    );
    assert!(
        cad2207_has_text(&restored, &scenario.notice.body),
        "re-sharing did not restore the subscriber's authorized notice body: {restored}"
    );
    assert!(
        cad2207_has_text(&restored, CAD2207_OWN_TEXT),
        "the subscriber's own ordinary message disappeared across the re-share: {restored}"
    );

    // The owner's page remains the owner's own notice, unchanged by the grant.
    let owner = cad2207_history(&fx, CAD2207_OWNER, json!({"tail": true, "limit": 100}));
    assert!(
        cad2207_has_text(&owner, &scenario.owner_notice.body),
        "the owner's own notice body disappeared after the re-share: {owner}"
    );
}

/// The shared assertions for a continuity-note case. `outcome` is the
/// production writer's branch (`delivered`, `skipped`, …). The REAL note must
/// exist with `message_id` NULL and its payload naming the notice; the LIVE
/// signed history must serve it; after the withdrawal no fresh signed history
/// page may name the notice (in the note payload or any turn entry) while the
/// member's own message and the owner's own notice stay readable.
fn cad2207_assert_note_withheld(fx: &Fx, scenario: &Cad2207History, outcome: &str, test: &str) {
    let note = cad2207_note_entry(
        fx,
        &scenario.subscriber_thread,
        &scenario.notice.id,
        outcome,
    );
    assert_eq!(
        note.message_id, None,
        "{test}: the continuity note is message-linked: {note:?}"
    );
    assert_eq!(
        note.payload
            .as_ref()
            .and_then(|payload| payload.get("outcome"))
            .and_then(Value::as_str),
        Some(outcome),
        "{test}: the note is not the '{outcome}' outcome: {note:?}"
    );

    // Control BEFORE the withdrawal: the signed history serves the note and
    // names the notice — the very surface that must stop after the unshare.
    let live = cad2207_history(fx, CAD2207_SUBSCRIBER, json!({"tail": true, "limit": 200}));
    assert!(
        live.to_string().contains(&scenario.notice.id),
        "{test}: the live signed history never served the notice id (control broken): {live}"
    );
    assert!(
        cad2207_entries(&live).iter().any(|entry| {
            entry.get("message").is_none_or(Value::is_null)
                && entry
                    .get("payload")
                    .and_then(|payload| payload.get("event"))
                    .and_then(Value::as_str)
                    == Some(crate::continuity::PACK_EVENT)
                && entry
                    .get("payload")
                    .and_then(|payload| payload.get("outcome"))
                    .and_then(Value::as_str)
                    == Some(outcome)
                && entry
                    .get("payload")
                    .and_then(|payload| payload.get("message"))
                    .and_then(Value::as_str)
                    == Some(scenario.notice.id.as_str())
        }),
        "{test}: the live signed history did not serve the '{outcome}' continuity note: {live}"
    );
    assert!(
        cad2207_has_text(&live, CAD2207_OWN_TEXT),
        "{test}: the live signed history lost the subscriber's own message: {live}"
    );

    cad2207_unshare(fx, &scenario.job);

    let revoked = cad2207_history(fx, CAD2207_SUBSCRIBER, json!({"tail": true, "limit": 200}));
    cad2207_assert_withheld(
        &format!("{test}: the subscriber's history after the withdrawal"),
        &revoked,
        scenario,
    );
    assert!(
        !cad2207_entries(&revoked).iter().any(|entry| {
            entry
                .get("payload")
                .and_then(|payload| payload.get("message"))
                .and_then(Value::as_str)
                == Some(scenario.notice.id.as_str())
        }),
        "{test}: a fresh signed history entry still names the revoked notice in its payload: \
         {revoked}"
    );
    assert!(
        cad2207_has_text(&revoked, CAD2207_OWN_TEXT),
        "{test}: the subscriber's own message disappeared with the withdrawal: {revoked}"
    );
    assert_eq!(
        revoked["conversation"]["alias"],
        json!(scenario.subscriber_alias),
        "{test}: the post-withdrawal read served another alias's conversation: {revoked}"
    );

    let owner = cad2207_history(fx, CAD2207_OWNER, json!({"tail": true, "limit": 200}));
    assert_eq!(
        owner["conversation"]["alias"],
        json!(scenario.owner_alias),
        "{test}: the owner's history served another alias's conversation: {owner}"
    );
    assert!(
        cad2207_has_text(&owner, &scenario.owner_notice.body),
        "{test}: the owner's own notice disappeared after the subscriber's withdrawal: {owner}"
    );
}

/// CAD-2207 (negative acceptance, security review): the daemon's DELIVERED
/// continuity note is a bookkeeping thread row with `message_id` NULL whose
/// payload names the triggering notice. A subscriber's fresh `member_assist_
/// history` after the unshare must not return it (or any other trace of the
/// notice); the member's own message and the owner's own notice stay.
#[test]
fn cad2207_revoked_share_is_withheld_from_a_delivered_continuity_note() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let scenario = cad2207_delivered_note_fixture(&fx);
    cad2207_assert_note_withheld(&fx, &scenario, "delivered", "delivered continuity note");
}

/// CAD-2207 (negative acceptance, security review): the same bookkeeping row
/// for a NON-DELIVERED pack (`outcome: "skipped"`, written by
/// `continuity_settle`) also names the triggering notice. A fresh signed
/// history after the unshare must not return it; the member's own message and
/// the owner's own notice stay.
#[test]
fn cad2207_revoked_share_is_withheld_from_a_skipped_continuity_note() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let scenario = cad2207_skipped_note_fixture(&fx);
    cad2207_assert_note_withheld(&fx, &scenario, "skipped", "skipped continuity note");
}
