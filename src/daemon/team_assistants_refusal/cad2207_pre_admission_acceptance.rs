//! CAD-2207 — a withdrawn share must be refused BEFORE the revoked notice's
//! protected body is materialized by the queue take.
//!
//! Independently authored negative acceptance, deliberately separate from the
//! implementation and from every other `team_assistants_refusal` check. This
//! module is a CHILD of that module only to reuse its already-real fixture
//! (`Fx`: a real in-process daemon, a real `operator/board-identity.json` trust
//! root and one loopback authoritative platform answering
//! `/__platform/assistant/revalidate`). No guard, store transition or decision
//! is mirrored here.
//!
//! WHAT IS UNDER TEST. `Shared::take_app_aware` is the real queue-take entry
//! point the production actor loop calls (`daemon/actor.rs`). At baseline
//! `50c5f725`, it decoded the full selected row before refusing a revoked
//! share. The take must instead settle the revoked notice from identity and
//! its durable receipt without materializing the protected body first.
//!
//! THE OBSERVABLE BOUNDARY:
//!   * the returned take is `Take::Empty` with no notice admission (never a
//!     claimed `Take::Message`, never a body-decode error);
//!   * the notice row is settled `failed` with a recorded refusal reason;
//!   * no `submitting` event names the notice (it was never claimed);
//!   * the delivery receipt survives and the stored body is untouched;
//!   * a member turn queued afterwards is taken and admitted, so the refusal
//!     is not a dead queue;
//!   * the owner's own notice for the same job is still taken and admitted —
//!     the guard is the withdrawn share, not a blanket refusal;
//!   * the refusal still leaves the SAME consumer-visible `turn_result` thread
//!     entry and `turn_finished` event the old `Store::finish` path wrote, and
//!     the subscriber's signed `member_assist_history` shows them after the
//!     owner re-shares (test 4).
//!
//! HOW "NOT MATERIALIZED" IS PROVEN, WITHOUT MIRRORING THE FIX:
//!   1. The repository's own native test seam counts full message-row decodes
//!      (`crate::store::take_decoded_messages`, thread-local, used by
//!      `cad627_board_poll_does_not_decode_terminal_history`). A refusal of a
//!      revoked notice must complete with ZERO full-row decodes. At the
//!      baseline the value was 2 (preview and settlement); narrowing only the
//!      preview would still fail because settlement is part of the read.
//!   2. A SYNTHETIC adversarial fixture (explicitly NOT an ordinary-flow
//!      state) replaces the queued notice's body with invalid UTF-8 after its
//!      real receipt exists. If any full-row read is attempted, it fails with a
//!      decode error; the test requires `take_app_aware` to still return the
//!      authorization refusal/settle, and requires the settle to leave the
//!      stored bytes exactly as they were. The corruption is a canary for a
//!      read, not a claim that production stores unreadable bodies.
//!
//! LIMIT OF THIS EVIDENCE, stated so it cannot be over-read: the counter counts
//! the repository's full-row message decoder (`row_message`), which is the only
//! path that decodes a `messages` row body into a Rust `String`; the corruption
//! canary catches ANY `String` decode attempt, including a bespoke
//! `SELECT body`. A METADATA-ONLY projection of non-body columns (id, source,
//! state, alias, turn_id) is not a body read, is not counted, and is exactly
//! what the safe preview/settle/app-proof should use. A fix that issued its own
//! byte-level `SELECT body` and never decoded it before the authorization
//! decision would evade both; that shape needs code review, not this black-box
//! test. Nothing here says a body may never be read after the admission
//! succeeds — the live-share control proves it still is.
//!
//! `finish` semantics remain legitimate: the terminal write is keyed by the
//! message id and writes `state/result/error/completed` plus the thread note
//! and events; it needs no body. At the baseline `app_completion_proof`
//! nevertheless read the full message. The safe fix is NOT to
//! trust the passed `Message`'s source and skip: the LIVE DB row is
//! authoritative, so the app proof must still be decided by a metadata-only
//! live-row query of that message's `source`/`state`/`alias`/`turn_id` (never
//! its body). A stale or forged in-memory `Message` therefore still cannot
//! bypass the app proof, and no body is decoded. That is why test 2 can demand
//! a successful settle of an unreadable revoked notice rather than tolerate a
//! decode error — and why the ordinary live-share control (test 3) proves the
//! take still materializes and admits a notice when the share IS live, so this
//! cannot be satisfied by never reading bodies at all.
//!
//! BASELINE FAILURE at `50c5f725`:
//!   * `cad2207_revoked_share_notice_take_refuses_without_materializing_the_body`
//!     — `decoded == 0` fails with 2 full-row decodes (preview + settle).
//!   * `cad2207_revoked_share_notice_with_unreadable_body_refuses_and_settles`
//!     — `take_app_aware` surfaces the invalid-UTF-8 decode error instead of
//!     `Take::Empty`.
//!   * `cad2207_live_share_notice_take_still_materializes_and_admits` was the
//!     passing ordinary-flow control.
//!   * `cad2207_refusal_receipt_still_reaches_reauthorized_member_history`
//!     guards the zero-decode requirement and would also fail a body-free
//!     refusal that dropped the existing thread `turn_result` or the
//!     `turn_finished` event. A genuine `assist_job_event` notice is enqueued
//!     by `commit_assist_intent` with `reply_to = NULL`, so `route_result` is
//!     a no-op for it: no real routed-result witness exists, and fabricating a
//!     `reply_to` would be a synthetic state. The test asserts that fact and
//!     the absence of any invented `result_routed`.
//!
//!   * `cad2207_stall_watch_tracks_revoked_notice_without_reading_its_body`
//!     and `cad2207_checkup_never_materializes_a_revoked_notice_body` are RED
//!     today for the same defect at the two secondary read sites
//!     (`watch.rs::stall_check` and `checkup.rs::checkup_agent`): each decodes
//!     the queued notice's full row. The watcher canary proves it by requiring
//!     the head to be tracked with an unreadable body; the checkup test
//!     measures `decoded == 0` and requires its recorded outcome. Controls:
//!     `cad2207_stall_watch_tracks_revoked_notice_with_a_valid_body` and
//!     `cad2207_stall_watch_ordinary_stalled_alert_still_fires`.
//!
//! Run just these (after the module is registered):
//! `scripts/boat-build -- cargo test --locked --features test-seam --lib
//! cad2207_revoked_share_notice -- --test-threads 1`,
//! `... --lib cad2207_live_share_notice -- --test-threads 1`,
//! `... --lib cad2207_refusal_receipt -- --test-threads 1`,
//! `... --lib cad2207_stall_watch -- --test-threads 1` and
//! `... --lib cad2207_checkup -- --test-threads 1`
//! (or the whole slice with `cad2207_`).

use super::*;
use std::time::Instant;

/// The owner of the shared job whose notice becomes unauthorized.
const CAD2207_PRE_OWNER: &str = "sub-alice";
/// The durable subscriber whose assistant holds the queued private notice.
const CAD2207_PRE_SUBSCRIBER: &str = "sub-ben";

/// The bytes injected by the synthetic corruption fixture, as SQLite's
/// uppercase `hex()` output. `x'fffe'` is not valid UTF-8, so a full-row
/// `String` decode of the body fails.
const CAD2207_CORRUPT_BODY_HEX: &str = "FFFE";

/// One owner-shared job with durable subscribers and ONE delivered private
/// `assist_job_event` notice each — produced by the REAL intent drain
/// (`assist_request_notice` + `route_assist_job_notifications`), which writes
/// the `assist_notice_deliveries` receipt the re-prove requires. Nothing is
/// synthesized or hand-enqueued, and no actor is launched: the take under test
/// is driven directly, exactly as `cad2207_shared_notice` does for the ordering
/// cases.
struct Cad2207NoticeFixture {
    job: String,
    subscriber_alias: String,
    owner_alias: String,
    /// The subscriber's queued notice and the body it carried when delivered.
    subscriber_notice: crate::store::Message,
    subscriber_body: String,
    /// The owner's queued notice for the SAME job — the owner control.
    owner_notice: crate::store::Message,
}

fn cad2207_delivered_notices(fx: &Fx) -> Cad2207NoticeFixture {
    let owner = fx.member_runtime(CAD2207_PRE_OWNER);
    let subscriber = fx.member_runtime(CAD2207_PRE_SUBSCRIBER);
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_PRE_OWNER),
            "title": "cad2207 pre-admission job",
            "request_key": "cad2207-pre-admission-job",
            "body": "cad2207 pre-admission spec",
        }),
    );
    let job = request["job"]
        .as_str()
        .unwrap_or_else(|| panic!("member_job_request returned no job: {request}"))
        .to_string();
    fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_PRE_OWNER),
            "job": job,
            "enabled": true,
        }),
    );
    fx.op(
        "member_job_subscribe",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_PRE_SUBSCRIBER),
            "job": job,
        }),
    );
    assert!(
        fx.shared.store.assist_job_is_shared(&job, ORG).unwrap(),
        "the fixture's owner share for {job} did not land"
    );
    assert!(
        fx.shared
            .store
            .job_is_subscriber(&job, ORG, CAD2207_PRE_SUBSCRIBER)
            .unwrap(),
        "the fixture did not create the durable subscriber row for {job}"
    );
    fx.shared.store.assist_request_notice(&job).unwrap();
    fx.shared
        .route_assist_job_notifications()
        .unwrap_or_else(|error| panic!("the assist intent drain failed: {error}"));
    let notice_for = |alias: &str| {
        fx.shared
            .store
            .messages(alias)
            .unwrap()
            .into_iter()
            .find(|message| {
                message.source == "assist_job_event"
                    && fx
                        .shared
                        .store
                        .assist_notice_delivery(&message.id)
                        .unwrap()
                        .is_some_and(|delivery| delivery.job == job)
            })
            .unwrap_or_else(|| panic!("no delivered assist notice for '{alias}'"))
    };
    let subscriber_notice = notice_for(&subscriber.alias);
    let owner_notice = notice_for(&owner.alias);
    assert_eq!(
        subscriber_notice.state, "queued",
        "the delivered subscriber notice is not the queued head this take will select: \
         {subscriber_notice:?}"
    );
    assert_eq!(
        owner_notice.state, "queued",
        "the delivered owner notice is not queued: {owner_notice:?}"
    );
    assert_ne!(
        subscriber_notice.id, owner_notice.id,
        "both notices share one message id"
    );
    let subscriber_body = subscriber_notice.body.clone();
    Cad2207NoticeFixture {
        job,
        subscriber_alias: subscriber.alias,
        owner_alias: owner.alias,
        subscriber_notice,
        subscriber_body,
        owner_notice,
    }
}

/// The owner's REAL `member_job_share {enabled:false}` on the proven-operator
/// relay, then the local store state it must have produced.
fn cad2207_withdraw_share(fx: &Fx, job: &str) {
    let withdrawn = fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_PRE_OWNER),
            "job": job,
            "enabled": false,
        }),
    );
    assert_eq!(
        withdrawn["shared"].as_bool(),
        Some(false),
        "the share withdrawal did not report the disabled state: {withdrawn}"
    );
    assert!(
        !fx.shared.store.assist_job_is_shared(job, ORG).unwrap(),
        "member_job_share reported shared:false without withdrawing the share"
    );
    assert!(
        fx.shared
            .store
            .job_is_subscriber(job, ORG, CAD2207_PRE_SUBSCRIBER)
            .unwrap(),
        "the durable subscriber row vanished — a refusal could then be the subscriber gate \
         instead of the withdrawn share"
    );
}

/// One real `take_app_aware`, with the native full-row decode count measured
/// across exactly that call on this thread.
fn cad2207_take(fx: &Fx, alias: &str) -> (crate::store::Take, bool, usize) {
    crate::store::take_decoded_messages();
    let (take, admission) = fx
        .shared
        .take_app_aware(alias)
        .unwrap_or_else(|error| panic!("take_app_aware('{alias}') failed: {error}"));
    let admitted = admission.is_some();
    drop(admission);
    let decoded = crate::store::take_decoded_messages();
    (take, admitted, decoded)
}

/// Was this exact message ever CLAIMED? The claim is the store's own
/// `submitting` event, written in the take transaction before any prompt,
/// context or provider work.
fn cad2207_claimed(fx: &Fx, alias: &str, message: &str) -> bool {
    fx.shared
        .store
        .events(alias, 0, 1000)
        .unwrap()
        .into_iter()
        .any(|event| {
            event.kind == "submitting"
                && event.payload.get("message").and_then(Value::as_str) == Some(message)
        })
}

/// Claim and admit a real member turn queued AFTER the refusal, proving the
/// refusal settled rather than wedging the assistant. The row is then closed
/// through the real store, the same fixture closure the sibling acceptances
/// use, so it stops being the queue head.
fn cad2207_ordinary_turn(fx: &Fx, alias: &str, text: &str) {
    let sent = fx.op(
        "member_assist_send",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_PRE_SUBSCRIBER),
            "text": text,
        }),
    );
    let id = sent["message"]
        .as_str()
        .unwrap_or_else(|| panic!("member_assist_send returned no message: {sent}"))
        .to_string();
    let (take, _, _) = cad2207_take(fx, alias);
    let crate::store::Take::Message(turn) = take else {
        panic!(
            "the ordinary member turn queued after the revoked-notice refusal was not taken \
             (the refusal wedged the assistant's queue)"
        )
    };
    assert_eq!(
        turn.id, id,
        "the take returned a different message than the member turn just sent"
    );
    fx.shared
        .admit_app_submission(&turn)
        .unwrap_or_else(|error| {
            panic!("the ordinary member turn was refused at admission: {error}")
        });
    fx.shared
        .store
        .finish(
            &turn,
            "failed",
            &json!({
                "status": "failed",
                "text": "",
                "error": "fixture closure after the pre-admission refusal control",
            }),
            Some("fixture closure"),
        )
        .expect("the fixture could not close its ordinary-turn control row");
}

/// The refused notice's settlement, read through the ordinary accessor (its
/// body is still valid here).
fn cad2207_settled_notice(fx: &Fx, id: &str) -> crate::store::Message {
    fx.shared
        .store
        .message(id)
        .unwrap()
        .unwrap_or_else(|| panic!("the revoked notice '{id}' vanished instead of settling"))
}

/// An authorization refusal never carries the protected body into its own
/// recorded outcome.
fn cad2207_assert_refusal_carried_no_body(settled: &crate::store::Message, body: &str) {
    assert!(
        !settled.error.as_deref().unwrap_or_default().contains(body),
        "the revocation refusal copied the notice body into its recorded reason: {:?}",
        settled.error
    );
    assert!(
        !settled.result.as_ref().is_some_and(|result| result
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains(body))),
        "the revocation refusal copied the notice body into its result text: {:?}",
        settled.result
    );
}

/// CONTRACT: with the owner's share withdrawn, the real `take_app_aware` must
/// refuse the queued subscriber notice WITHOUT decoding its full row, settle
/// it, and leave the assistant usable. The owner's own notice for the same job
/// stays admissible.
///
/// At the baseline, preview and settlement each decoded the selected notice.
#[test]
fn cad2207_revoked_share_notice_take_refuses_without_materializing_the_body() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let notice = cad2207_delivered_notices(&fx);
    cad2207_withdraw_share(&fx, &notice.job);

    // The notice is still durably queued; the fixture state is the real
    // post-withdrawal one (unshare deletes the share row only).
    let queued = fx
        .shared
        .store
        .message(&notice.subscriber_notice.id)
        .unwrap()
        .expect("the revoked notice was dropped before the take");
    assert_eq!(
        queued.state, "queued",
        "the revoked notice is not the queued head this take selects: {queued:?}"
    );

    let (take, admitted, decoded) = cad2207_take(&fx, &notice.subscriber_alias);
    assert!(
        matches!(take, crate::store::Take::Empty),
        "a revoked share's notice was claimed or handed back as runnable: the take must \
         refuse it (CAD-2207)"
    );
    assert!(!admitted, "the refused notice still carried an admission");
    assert_eq!(
        decoded, 0,
        "take_app_aware decoded {decoded} full message row(s) while refusing a withdrawn \
         share's notice: the revoked notice body was materialized before/while the admission \
         refused it. Both the selection preview and the refusal settle must use the message \
         identity + durable receipt and must not read the protected body (CAD-2207)."
    );

    let settled = cad2207_settled_notice(&fx, &notice.subscriber_notice.id);
    assert_eq!(
        settled.state, "failed",
        "the revoked notice was left '{}' instead of being explicitly refused: {settled:?}",
        settled.state
    );
    assert!(
        settled
            .error
            .as_deref()
            .is_some_and(|reason| !reason.trim().is_empty()),
        "the refused notice carries no recorded reason: {settled:?}"
    );
    assert!(
        settled.turn_id.is_none(),
        "the refused notice records a provider turn: {settled:?}"
    );
    assert_eq!(
        settled.body, notice.subscriber_body,
        "the refusal rewrote the notice body"
    );
    cad2207_assert_refusal_carried_no_body(&settled, &notice.subscriber_body);
    assert!(
        fx.shared
            .store
            .assist_notice_delivery(&notice.subscriber_notice.id)
            .unwrap()
            .is_some(),
        "the refusal dropped the durable delivery receipt the settle must be based on"
    );
    assert!(
        !cad2207_claimed(&fx, &notice.subscriber_alias, &notice.subscriber_notice.id),
        "the revoked notice was CLAIMED (a submitting event names it) — refusal must \
         happen before the claim (CAD-2207)"
    );
    assert!(
        fx.shared
            .store
            .events(&notice.subscriber_alias, 0, 1000)
            .unwrap()
            .iter()
            .any(|event| {
                event.kind == "member_turn_refused"
                    && event.payload.get("message").and_then(Value::as_str)
                        == Some(notice.subscriber_notice.id.as_str())
            }),
        "no truthful member_turn_refused event names the refused notice"
    );

    // Subsequent ordinary work on the SAME assistant is still taken and
    // admitted: the refusal settled the queue instead of wedging it.
    cad2207_ordinary_turn(&fx, &notice.subscriber_alias, "post-refusal ordinary turn");

    // The owner still owns the job; withdrawing a subscriber's ride must not
    // revoke the owner's own delivered notice.
    let (take, _, _) = cad2207_take(&fx, &notice.owner_alias);
    let crate::store::Take::Message(taken) = take else {
        panic!(
            "the owner's notice for the same job was not taken after the subscriber's share \
             was withdrawn — the refusal is broader than the withdrawn share (CAD-2207)"
        )
    };
    assert_eq!(
        taken.id, notice.owner_notice.id,
        "the owner's take returned a different message"
    );
    assert_eq!(
        taken.body, notice.owner_notice.body,
        "the owner's admissible notice body changed"
    );
    fx.shared
        .admit_app_submission(&taken)
        .expect("the owner's own notice was refused at admission after the share withdrawal");
    fx.shared
        .store
        .finish(
            &taken,
            "failed",
            &json!({
                "status": "failed",
                "text": "",
                "error": "fixture closure after the owner-positive control",
            }),
            Some("fixture closure"),
        )
        .expect("the fixture could not close its owner-positive control row");
}

/// SYNTHETIC ADVERSARIAL FIXTURE (never an ordinary production state): after
/// the real receipt exists and the real share is withdrawn, the queued
/// notice's body is replaced with invalid UTF-8. That makes any attempted
/// full-row body decode fail observably, so the test can distinguish "the take
/// refused without reading the body" from "the take tried to read it".
///
/// Requirements:
///   * `take_app_aware` returns `Take::Empty` (an authorization refusal), never
///     the decode error, and never a claimed message;
///   * the settle still succeeds and leaves the stored bytes exactly as the
///     fixture wrote them — `finish` is identity-keyed and needs no body;
///   * zero full-row decodes are measured;
///   * the assistant still runs ordinary work afterwards.
///
/// At the baseline, preview returned a decode error instead of the refusal.
#[test]
fn cad2207_revoked_share_notice_with_unreadable_body_refuses_and_settles() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let notice = cad2207_delivered_notices(&fx);
    cad2207_withdraw_share(&fx, &notice.job);

    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE messages SET body=CAST(x'fffe' AS TEXT) WHERE id=?",
                rusqlite::params![notice.subscriber_notice.id],
            )?;
            Ok(())
        })
        .expect("the fixture could not make the queued notice body unreadable");

    // Precondition: the corruption is real. The ordinary full-row reader now
    // fails, so any take that still reads the row will fail the same way.
    assert!(
        fx.shared
            .store
            .message(&notice.subscriber_notice.id)
            .is_err(),
        "the fixture's body corruption is not observable: the ordinary full-row reader still \
         decoded it, so this test could not detect a read"
    );

    crate::store::take_decoded_messages();
    let outcome = fx.shared.take_app_aware(&notice.subscriber_alias);
    let decoded = crate::store::take_decoded_messages();
    let (take, admission) = outcome.unwrap_or_else(|error| {
        panic!(
            "take_app_aware surfaced a body-decode error instead of an authorization \
             refusal/settle for a revoked notice whose body the take must not read: {error} \
             (CAD-2207)"
        )
    });
    assert!(
        matches!(take, crate::store::Take::Empty),
        "the revoked notice with an unreadable body was claimed or handed back as runnable"
    );
    assert!(
        admission.is_none(),
        "the refused notice still carried an admission"
    );
    drop(admission);
    assert_eq!(
        decoded, 0,
        "take_app_aware decoded {decoded} full message row(s) while refusing a withdrawn \
         share's notice: the revoked notice body was read (the corruption only makes that \
         read fail). The refusal must be decided from identity + receipt (CAD-2207)."
    );

    // Narrow read of the settled row: the ordinary accessor cannot decode this
    // body, and the settle must not have rewritten or "repaired" it. `hex` and
    // `typeof` are on the store's read allowlist; both run under the disarmed
    // read authorizer, and neither materializes a Rust `String`.
    let (state, error, turn_id, body_type, body_hex) = fx
        .shared
        .store
        .read_tx(|tx| {
            tx.query_row(
                "SELECT state,error,turn_id,typeof(body),hex(body) FROM messages WHERE id=?",
                rusqlite::params![notice.subscriber_notice.id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
        })
        .expect("the fixture could not read the settled notice row");
    assert_eq!(
        state, "failed",
        "the revoked notice with an unreadable body was left '{state}' instead of being \
         explicitly refused"
    );
    assert!(
        error.is_some_and(|reason| !reason.trim().is_empty()),
        "the refused notice carries no recorded reason"
    );
    assert!(
        turn_id.is_none(),
        "the refused notice records a provider turn"
    );
    assert_eq!(
        body_type, "text",
        "the fixture's body is not stored as text; the unreadable-body canary is not the \
         intended invalid-UTF-8 shape"
    );
    assert_eq!(
        body_hex, CAD2207_CORRUPT_BODY_HEX,
        "the refusal settle rewrote the unreadable body instead of leaving the stored bytes \
         untouched"
    );
    assert!(
        fx.shared
            .store
            .assist_notice_delivery(&notice.subscriber_notice.id)
            .unwrap()
            .is_some(),
        "the refusal dropped the durable delivery receipt the settle must be based on"
    );
    assert!(
        !cad2207_claimed(&fx, &notice.subscriber_alias, &notice.subscriber_notice.id),
        "the revoked notice was CLAIMED (a submitting event names it) — refusal must \
         happen before the claim (CAD-2207)"
    );

    // The unreadable private row did not wedge ordinary work queued after it.
    cad2207_ordinary_turn(
        &fx,
        &notice.subscriber_alias,
        "ordinary turn after unreadable notice",
    );
}

/// ORDINARY-FLOW CONTROL: with the owner share LIVE, the very same take path
/// must still materialize the notice body and admit it. This is what prevents
/// the two negative tests above from being satisfied by refusing every notice
/// or by never reading a body at all: the body equality and the admission
/// result below are the positive half of the contract.
#[test]
fn cad2207_live_share_notice_take_still_materializes_and_admits() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let notice = cad2207_delivered_notices(&fx);
    // No withdrawal: the share is live and the subscriber rides it.

    let (take, admitted, _decoded) = cad2207_take(&fx, &notice.subscriber_alias);
    let crate::store::Take::Message(taken) = take else {
        panic!(
            "a LIVE shared notice was not taken — the refusal above could be a blanket \
             refusal rather than a withdrawn-share gate (CAD-2207)"
        )
    };
    assert_eq!(
        taken.id, notice.subscriber_notice.id,
        "the live-share take returned a different message"
    );
    assert_eq!(
        taken.body, notice.subscriber_body,
        "the live-share take did not materialize the delivered notice body"
    );
    assert!(
        admitted,
        "the live shared notice was taken without a notice admission — the subscriber's ride \
         was not held across the claim"
    );
    fx.shared
        .admit_app_submission(&taken)
        .expect("the live shared notice was refused at admission");
    fx.shared
        .store
        .finish(
            &taken,
            "failed",
            &json!({
                "status": "failed",
                "text": "",
                "error": "fixture closure after the live-share control",
            }),
            Some("fixture closure"),
        )
        .expect("the fixture could not close its live-share control row");
}

/// CONSUMER-VISIBLE REFUSAL RECEIPT — regression acceptance for a body-free
/// refusal implementation.
///
/// The first body-free refusal settled the revoked notice without the
/// `Store::finish` receipt surfaces: the thread `turn_result` entry (and the
/// `turn_finished` event) the old path wrote. A subscriber therefore lost the
/// truthful "failed" turn result that used to appear in their own history.
///
/// Flow, all through real entry points: owner
/// `member_job_share {enabled:false}` → `take_app_aware` refusal with ZERO
/// full-row decodes (protected body unread) → owner
/// `member_job_share {enabled:true}` re-share → the SUBSCRIBER's signed
/// `member_assist_history`.
///
/// Required shapes, exactly as `Store::finish` used to write them:
///   * one `role=agent`, `kind=turn_result` entry for the notice's message id,
///     `text=""` — the old path copies `result["text"]`, which is empty, so no
///     invented text and never the notice body;
///   * payload `{status:"failed", error:<a recorded refusal reason>}`. The old
///     path runs the reason through the store's secret scan/redaction
///     (`clean_payload`) before it lands in the thread, so the acceptance pins
///     the failed status and a nonempty recorded reason, not byte equality
///     with the unredacted message-row error;
///   * a `turn_finished` event for the same message with the same
///     failed/empty-text result;
///   * no routing: `commit_assist_intent` enqueues a genuine
///     `assist_job_event` with `reply_to = NULL`, so `route_result` is a no-op
///     and nothing may appear as a routed delivery. A real routed-result
///     witness cannot be produced for this message without fabricating a
///     `reply_to` the producer never writes, so it is deliberately not
///     asserted; the generic refusal path must still preserve `finish`'s
///     routing for rows that do carry one.
#[test]
fn cad2207_refusal_receipt_still_reaches_reauthorized_member_history() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let notice = cad2207_delivered_notices(&fx);
    cad2207_withdraw_share(&fx, &notice.job);

    // The refusal itself must not read the protected body.
    let (take, admitted, decoded) = cad2207_take(&fx, &notice.subscriber_alias);
    assert!(
        matches!(take, crate::store::Take::Empty),
        "the revoked notice was claimed or handed back as runnable"
    );
    assert!(!admitted, "the refused notice still carried an admission");
    assert_eq!(
        decoded, 0,
        "the refusal materialized the revoked notice body; the receipt assertions below require \
         the body-free path (CAD-2207)"
    );
    let settled = cad2207_settled_notice(&fx, &notice.subscriber_notice.id);
    assert_eq!(
        settled.state, "failed",
        "the revoked notice was not settled failed: {settled:?}"
    );
    let result = settled
        .result
        .as_ref()
        .unwrap_or_else(|| panic!("the refused notice recorded no result json: {settled:?}"));
    assert_eq!(
        result["status"], "failed",
        "the settled result is not the old finish shape: {result}"
    );
    assert_eq!(
        result["text"], "",
        "the settled result invented text: {result}"
    );
    assert!(
        result["error"]
            .as_str()
            .is_some_and(|reason| !reason.trim().is_empty()),
        "the refused notice recorded no refusal reason: {settled:?}"
    );

    // Authorized re-share through the REAL owner RPC.
    let reshared = fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_PRE_OWNER),
            "job": notice.job,
            "enabled": true,
        }),
    );
    assert_eq!(
        reshared["shared"].as_bool(),
        Some(true),
        "the owner's re-share did not report the enabled state: {reshared}"
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&notice.job, ORG)
            .unwrap(),
        "member_job_share reported shared:true without restoring the share"
    );

    // The SUBSCRIBER's own signed history of their bound conversation.
    let history = fx.op(
        "member_assist_history",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_PRE_SUBSCRIBER),
            "tail": true,
            "limit": 200,
        }),
    );
    let entries = history["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("member_assist_history returned no entries: {history}"));
    let results: Vec<&Value> = entries
        .iter()
        .filter(|entry| {
            let entry = *entry;
            entry["message"].as_str() == Some(notice.subscriber_notice.id.as_str())
                && entry["kind"].as_str() == Some("turn_result")
        })
        .collect();
    assert_eq!(
        results.len(),
        1,
        "the subscriber's signed history does not show exactly one turn_result receipt for the \
         refused notice (the body-free refusal must preserve the old Store::finish thread note, \
         CAD-2207): {history}"
    );
    let entry = results[0];
    assert_eq!(
        entry["role"].as_str(),
        Some("agent"),
        "the turn_result receipt is not role=agent: {entry}"
    );
    assert_eq!(
        entry["text"].as_str(),
        Some(""),
        "the refusal invented turn-result text; the old Store::finish path writes \
         result[\"text\"], which is empty: {entry}"
    );
    assert_eq!(
        entry["text"], result["text"],
        "the history turn-result text does not match the settled result text"
    );
    assert_eq!(
        entry["payload"]["status"], result["status"],
        "the history receipt status does not match the settled result status"
    );
    // The thread note is written through the store's secret scan/redaction
    // (`clean_payload`), which is itself part of the old `Store::finish` path.
    // So the acceptance pins the consumer-visible failed receipt + a recorded
    // reason, not byte equality with the unredacted message-row error.
    assert!(
        entry["payload"]["error"]
            .as_str()
            .is_some_and(|reason| !reason.trim().is_empty()),
        "the history receipt carries no refusal reason: {entry}"
    );
    assert!(
        !entry["text"]
            .as_str()
            .unwrap_or_default()
            .contains(&notice.subscriber_body),
        "the turn-result receipt copied the protected notice body: {entry}"
    );
    assert!(
        !entry["payload"]
            .to_string()
            .contains(&notice.subscriber_body),
        "the turn-result payload copied the protected notice body: {entry}"
    );

    // The same finish also records the `turn_finished` event.
    let events = fx
        .shared
        .store
        .events(&notice.subscriber_alias, 0, 1000)
        .unwrap();
    let finished = events
        .iter()
        .find(|event| {
            event.kind == "turn_finished"
                && event.payload["message"].as_str() == Some(notice.subscriber_notice.id.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "the refusal wrote no turn_finished event for the notice — the old \
                 Store::finish receipt was dropped (CAD-2207)"
            )
        });
    assert_eq!(
        finished.payload["result"]["status"].as_str(),
        Some("failed"),
        "the turn_finished event does not carry the failed status: {finished:?}"
    );
    assert_eq!(
        finished.payload["result"]["text"].as_str(),
        Some(""),
        "the turn_finished event invented result text: {finished:?}"
    );
    assert!(
        !finished
            .payload
            .to_string()
            .contains(&notice.subscriber_body),
        "the turn_finished event copied the protected notice body: {finished:?}"
    );

    // A genuine assist notice carries no reply_to (`commit_assist_intent`
    // passes `None` to `enqueue_tx`), so there is no real routed result to
    // preserve for THIS message. Assert the by-construction fact and that no
    // routing was invented.
    assert!(
        settled.reply_to.is_none(),
        "a genuine assist_job_event notice unexpectedly carries reply_to {:?}; this acceptance \
         relies on the real producer's no-routing shape",
        settled.reply_to
    );
    assert!(
        !events.iter().any(|event| {
            event.kind == "result_routed"
                && event.payload["message"].as_str() == Some(notice.subscriber_notice.id.as_str())
        }),
        "the refusal invented a routed result for a notice that has no reply_to"
    );
}

// ============================================================================
// CAD-2207 independent acceptance (secondary body-read sites) — the stall
// watcher (`watch.rs::stall_check`) and the PM checkup
// (`checkup.rs::checkup_agent`) learn a queued head's identity without
// materializing a withdrawn notice's protected body. Both are exercised
// through their real entry points (`run_stall_watch` / `checkup_tick`), with
// the genuine owner/subscriber notice produced by the real intent drain.
// ============================================================================

/// A member assistant alias WITHOUT `member_runtime`'s ordinary-worker and
/// foreign-message seeding, so a checkup decode measurement sees only the
/// messages this acceptance created.
fn cad2207_member_alias(fx: &Fx, sub: &str) -> String {
    fx.provision(sub);
    let state = fx.op(
        "member_assist_state",
        json!({"member_envelope": fx.member_envelope(sub)}),
    );
    state["assistant"]["alias"]
        .as_str()
        .unwrap_or_else(|| panic!("member_assist_state bound no alias: {state}"))
        .to_string()
}

/// The delivered `assist_job_event` notice for `job` on `alias`, by its real
/// delivery receipt.
fn cad2207_notice_on(fx: &Fx, alias: &str, job: &str) -> crate::store::Message {
    fx.shared
        .store
        .messages(alias)
        .unwrap()
        .into_iter()
        .find(|message| {
            message.source == "assist_job_event"
                && fx
                    .shared
                    .store
                    .assist_notice_delivery(&message.id)
                    .unwrap()
                    .is_some_and(|delivery| delivery.job == job)
        })
        .unwrap_or_else(|| panic!("no delivered assist notice for '{alias}'"))
}

/// Minimal owner/subscriber pair for the checkup measurement: only the two
/// member assistants exist, so `checkup_tick`'s queued-head reads are the
/// subscriber's notice alone. The owner's own notice is settled through the
/// real store before the measurement, leaving no second queued head.
struct Cad2207MinimalNotices {
    job: String,
    subscriber_alias: String,
    subscriber_notice: crate::store::Message,
    subscriber_body: String,
}

fn cad2207_minimal_notices(fx: &Fx) -> Cad2207MinimalNotices {
    let owner_alias = cad2207_member_alias(fx, CAD2207_PRE_OWNER);
    let subscriber_alias = cad2207_member_alias(fx, CAD2207_PRE_SUBSCRIBER);
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_PRE_OWNER),
            "title": "cad2207 checkup job",
            "request_key": "cad2207-checkup-job",
            "body": "cad2207 checkup spec",
        }),
    );
    let job = request["job"]
        .as_str()
        .unwrap_or_else(|| panic!("member_job_request returned no job: {request}"))
        .to_string();
    fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_PRE_OWNER),
            "job": job,
            "enabled": true,
        }),
    );
    fx.op(
        "member_job_subscribe",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_PRE_SUBSCRIBER),
            "job": job,
        }),
    );
    fx.shared.store.assist_request_notice(&job).unwrap();
    fx.shared
        .route_assist_job_notifications()
        .unwrap_or_else(|error| panic!("the assist intent drain failed: {error}"));
    let subscriber_notice = cad2207_notice_on(fx, &subscriber_alias, &job);
    let owner_notice = cad2207_notice_on(fx, &owner_alias, &job);
    // Settle the owner's own notice before the measurement: checkup only
    // reads the *queued* head, so this leaves the subscriber's notice as the
    // sole queued row (and it is the only row whose decode we measure).
    fx.shared
        .store
        .finish(
            &owner_notice,
            "failed",
            &json!({
                "status": "failed",
                "text": "",
                "error": "fixture closure before the checkup measurement",
            }),
            Some("fixture closure"),
        )
        .expect("the fixture could not close the owner's notice");
    let subscriber_body = subscriber_notice.body.clone();
    Cad2207MinimalNotices {
        job,
        subscriber_alias,
        subscriber_notice,
        subscriber_body,
    }
}

/// The real stall tick only visits aliases with a control surface. Install
/// one without launching an actor — the same fixture shape the existing
/// checkup/next-action unit tests use. The queue, the agent row and every
/// production guard remain the real ones.
fn cad2207_inject_stall_ctl(fx: &Fx, alias: &str) {
    let ctl = Arc::new(crate::daemon::AgentCtl {
        adapter: Mutex::new(None),
        wake: crate::daemon::Notify::new(),
        thread: Mutex::new(None),
        stall: Mutex::new(crate::daemon::StallWatch::default()),
        cloud_held: AtomicBool::new(false),
    });
    fx.shared
        .lifecycle
        .lock()
        .unwrap()
        .agents
        .insert(alias.to_string(), ctl);
}

/// Run the REAL `run_stall_watch` loop on its own thread until `done` observes
/// the expected state, then stop it. On timeout the loop is still stopped
/// before the failure is reported, so a red run cannot leak a watcher thread
/// into the rest of the suite.
fn cad2207_watch_until(fx: &Fx, what: &str, done: impl Fn(&Fx) -> bool) {
    let shared = fx.shared.clone();
    let handle = std::thread::spawn(move || shared.run_stall_watch());
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut observed = false;
    while Instant::now() < deadline {
        if done(fx) {
            observed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    fx.shared.closing.store(true, Ordering::SeqCst);
    let joined = handle.join();
    fx.shared.closing.store(false, Ordering::SeqCst);
    assert!(joined.is_ok(), "the stall-watch loop panicked: {what}");
    assert!(observed, "{what}");
}

/// Narrow row state/body evidence (never decodes the body into a `String`).
fn cad2207_notice_storage(fx: &Fx, id: &str) -> (String, String, String) {
    fx.shared
        .store
        .read_tx(|tx| {
            tx.query_row(
                "SELECT state,typeof(body),hex(body) FROM messages WHERE id=?",
                rusqlite::params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
        })
        .expect("the fixture could not read the notice row")
}

/// NEGATIVE (stall watcher): the periodic watchdog must learn the queued
/// head's identity WITHOUT materializing a withdrawn notice's body.
///
/// The genuine owner/subscriber notice is delivered by the real intent drain,
/// then the owner's local share is withdrawn. The subscriber notice's body is
/// replaced with invalid UTF-8 (a synthetic canary: ANY `String` decode of it
/// fails). The real `run_stall_watch` loop runs with the subscriber and an
/// ordinary queued control both given control surfaces.
///
/// Required: the watchdog still tracks the revoked notice's queued head
/// (`last_activity_epoch` is the real watch state consumed by the views) —
/// which is impossible if it decoded the body, because that decode fails.
/// The ordinary control head must still be tracked, no stall/menu alert may be
/// invented for the revoked notice, and the row/body must be untouched.
/// Before the metadata-only watcher, `stall_check` decoded the full row,
/// failed on the canary, and silently dropped the tracked head.
#[test]
fn cad2207_stall_watch_tracks_revoked_notice_without_reading_its_body() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let notice = cad2207_delivered_notices(&fx);
    cad2207_withdraw_share(&fx, &notice.job);

    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE messages SET body=CAST(x'fffe' AS TEXT) WHERE id=?",
                rusqlite::params![notice.subscriber_notice.id],
            )?;
            Ok(())
        })
        .expect("the fixture could not make the queued notice body unreadable");

    cad2207_inject_stall_ctl(&fx, &notice.subscriber_alias);
    cad2207_inject_stall_ctl(&fx, "ordinary-worker");
    cad2207_watch_until(
        &fx,
        "the watchdog never tracked the ordinary queued control head (its tick did not run)",
        |fx| fx.shared.last_activity_epoch("ordinary-worker").is_some(),
    );

    assert!(
        fx.shared
            .last_activity_epoch(&notice.subscriber_alias)
            .is_some(),
        "the stall watcher did not track the withdrawn notice's queued head: it attempted to \
         materialize the protected body instead of reading the identity (CAD-2207)"
    );
    assert!(
        fx.shared.last_activity_epoch("ordinary-worker").is_some(),
        "the ordinary queued control head is no longer tracked by the watchdog"
    );
    assert!(
        fx.shared.stall_view(&notice.subscriber_alias).is_none(),
        "the watchdog invented a stall/menu alert for a revoked notice with no probe or menu"
    );
    let events = fx
        .shared
        .store
        .events(&notice.subscriber_alias, 0, 1000)
        .unwrap();
    assert!(
        !events.iter().any(|event| {
            matches!(
                event.kind.as_str(),
                "turn_stalled" | "delivery_stalled" | "approval_menu"
            ) && event.payload["message"].as_str() == Some(notice.subscriber_notice.id.as_str())
        }),
        "the watchdog emitted an alert naming the revoked queued notice"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.payload.to_string().contains(&notice.subscriber_body)),
        "the watchdog surfaced the protected notice body in an event"
    );
    // The watchdog only observes: the row is still queued and its unreadable
    // stored bytes are unchanged.
    let (state, body_type, body_hex) = cad2207_notice_storage(&fx, &notice.subscriber_notice.id);
    assert_eq!(
        state, "queued",
        "the watchdog changed the revoked notice's queue state: {state}"
    );
    assert_eq!(
        body_type, "text",
        "the fixture's unreadable-body shape changed: {body_type}"
    );
    assert_eq!(
        body_hex, CAD2207_CORRUPT_BODY_HEX,
        "the watchdog rewrote the unreadable notice body"
    );
}

/// POSITIVE control for the watcher: with the SAME real revoked notice but a
/// readable body, the watchdog tracks the queued head and the ordinary
/// control head, and invents no alert. This is the ordinary-flow half the
/// canary above must stay consistent with (it passes today and must keep
/// passing after the body-free fix).
#[test]
fn cad2207_stall_watch_tracks_revoked_notice_with_a_valid_body() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let notice = cad2207_delivered_notices(&fx);
    cad2207_withdraw_share(&fx, &notice.job);

    cad2207_inject_stall_ctl(&fx, &notice.subscriber_alias);
    cad2207_inject_stall_ctl(&fx, "ordinary-worker");
    cad2207_watch_until(
        &fx,
        "the watchdog never tracked the ordinary queued control head (its tick did not run)",
        |fx| fx.shared.last_activity_epoch("ordinary-worker").is_some(),
    );

    assert!(
        fx.shared
            .last_activity_epoch(&notice.subscriber_alias)
            .is_some(),
        "the watchdog stopped tracking the withdrawn notice's queued head with a readable body"
    );
    assert!(
        fx.shared.last_activity_epoch("ordinary-worker").is_some(),
        "the ordinary queued control head is no longer tracked"
    );
    assert!(
        fx.shared.stall_view(&notice.subscriber_alias).is_none(),
        "the watchdog invented a stall/menu alert for a revoked notice with no probe or menu"
    );
}

/// ORDINARY ALERT CONTROL: the watchdog's real stall path still fires for
/// ordinary work — a running turn past its `stall_secs` budget produces one
/// `turn_stalled` event and the view marks the turn stalled. This proves the
/// negative above is about the revoked body, not a watcher that stopped
/// working. (The test clock offset advances the budget without a wall wait.)
#[test]
fn cad2207_stall_watch_ordinary_stalled_alert_still_fires() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let _notice = cad2207_delivered_notices(&fx);
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET params=? WHERE alias='ordinary-worker'",
                rusqlite::params![json!({"stall_secs": 1}).to_string()],
            )?;
            Ok(())
        })
        .expect("the fixture could not set the control turn's stall budget");
    fx.shared
        .store
        .enqueue(
            "ordinary-worker",
            "ordinary control turn",
            None,
            "cad2207-stall-alert-control",
            "test",
        )
        .unwrap();
    fx.shared
        .store
        .mark_running(
            "cad2207-stall-alert-control",
            "turn-cad2207-stall-alert-control",
        )
        .unwrap();
    cad2207_inject_stall_ctl(&fx, "ordinary-worker");

    cad2207_watch_until(
        &fx,
        "the ordinary control turn never produced a turn_stalled alert",
        |fx| {
            if fx.shared.last_activity_epoch("ordinary-worker").is_none() {
                return false;
            }
            fx.shared.stall_clock_offset.store(10, Ordering::SeqCst);
            fx.shared
                .store
                .events("ordinary-worker", 0, 1000)
                .unwrap()
                .iter()
                .any(|event| {
                    event.kind == "turn_stalled"
                        && event.payload["message"].as_str() == Some("cad2207-stall-alert-control")
                })
        },
    );

    let events = fx.shared.store.events("ordinary-worker", 0, 1000).unwrap();
    assert!(
        events.iter().any(|event| {
            event.kind == "turn_stalled"
                && event.payload["message"].as_str() == Some("cad2207-stall-alert-control")
        }),
        "the watchdog no longer fires turn_stalled for an ordinary running turn"
    );
    let view = fx
        .shared
        .stall_view("ordinary-worker")
        .expect("the stalled ordinary control turn has no stall view");
    assert!(
        view.stalled,
        "the stall view does not mark the ordinary control turn stalled"
    );
}

/// NEGATIVE (PM checkup): `checkup_agent` must read the closed notice's
/// queued identity without materializing its body.
///
/// The genuine owner/subscriber notice is delivered, the owner's share is
/// withdrawn, the subscriber's notice body is the invalid-UTF-8 canary, and
/// the owner's own notice is already settled — so the subscriber's notice is
/// the only queued head the real `checkup_tick` can read. Required: the tick
/// decodes ZERO full message rows, still records the checkup outcome naming
/// the notice id, and leaves the queued row and its unreadable bytes alone.
/// Before the metadata-only checkup, full-row decoding failed on the canary
/// and the tick recorded no outcome for the subscriber.
///
/// POSITIVE control in the same fixture: after the measurement, a dedicated
/// ordinary worker with one valid queued message still gets its `checkup`
/// outcome naming that message.
#[test]
fn cad2207_checkup_never_materializes_a_revoked_notice_body() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let notices = cad2207_minimal_notices(&fx);
    cad2207_withdraw_share(&fx, &notices.job);

    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE messages SET body=CAST(x'fffe' AS TEXT) WHERE id=?",
                rusqlite::params![notices.subscriber_notice.id],
            )?;
            Ok(())
        })
        .expect("the fixture could not make the queued notice body unreadable");

    crate::store::take_decoded_messages();
    fx.shared.checkup_tick();
    let decoded = crate::store::take_decoded_messages();
    assert_eq!(
        decoded, 0,
        "the checkup pass decoded {decoded} full message row(s): it materialized the withdrawn \
         notice's protected body instead of reading its queued identity (CAD-2207)"
    );

    let events = fx
        .shared
        .store
        .events(&notices.subscriber_alias, 0, 1000)
        .unwrap();
    let outcome = events
        .iter()
        .find(|event| {
            event.kind == "checkup"
                && event.payload["message"].as_str() == Some(notices.subscriber_notice.id.as_str())
        })
        .unwrap_or_else(|| {
            panic!(
                "the checkup pass recorded no outcome naming the withdrawn notice: it read the \
                 skipped/failed body instead of the queued id (CAD-2207)"
            )
        });
    assert!(
        outcome.payload["outcome"]
            .as_str()
            .is_some_and(|value| !value.trim().is_empty()),
        "the checkup outcome is empty: {:?}",
        outcome.payload
    );
    assert!(
        !outcome
            .payload
            .to_string()
            .contains(&notices.subscriber_body),
        "the checkup outcome surfaced the protected notice body: {:?}",
        outcome.payload
    );

    let (state, body_type, body_hex) = cad2207_notice_storage(&fx, &notices.subscriber_notice.id);
    assert_eq!(
        state, "queued",
        "the checkup pass changed the withdrawn notice's queue state: {state}"
    );
    assert_eq!(
        body_type, "text",
        "the fixture's unreadable-body shape changed: {body_type}"
    );
    assert_eq!(
        body_hex, CAD2207_CORRUPT_BODY_HEX,
        "the checkup pass rewrote the unreadable notice body"
    );

    // Ordinary queued work still gets a checkup outcome.
    let cwd = fx.dir.path().join("cad2207-checkup-control-cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "cad2207-checkup-control",
            provider: "fake",
            endpoint_kind: "managed",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .expect("the fixture could not register its ordinary checkup control");
    fx.shared
        .store
        .enqueue(
            "cad2207-checkup-control",
            "ordinary control turn",
            None,
            "cad2207-checkup-control-msg",
            "test",
        )
        .unwrap();
    fx.shared.checkup_tick();
    let control_events = fx
        .shared
        .store
        .events("cad2207-checkup-control", 0, 1000)
        .unwrap();
    assert!(
        control_events.iter().any(|event| {
            event.kind == "checkup"
                && event.payload["message"].as_str() == Some("cad2207-checkup-control-msg")
        }),
        "the checkup pass stopped recording ordinary queued work"
    );
}
