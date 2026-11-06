//! CAD-2207 — the QUEUE HEAD that a take admitted is not the message it claims.
//!
//! Independently authored negative acceptance, deliberately separate from the
//! implementation and from every other `team_assistants_refusal` check. This
//! module is a CHILD of that module only to reuse its already-real fixture
//! (`Fx`: a real in-process daemon, a real `operator/board-identity.json` trust
//! root and one loopback authoritative platform answering
//! `/__platform/assistant/revalidate`). No guard, store transition or decision
//! is mirrored here; only the fixture's existing HTTP barrier and concurrency
//! switch are used.
//!
//! SCOPE, stated narrowly so it cannot be over-read. `Shared::take_app_aware`
//! reads the queue head, admits THAT message (local receipt read, job/share
//! re-read and the live-authority pull), and only then runs the atomic
//! `take_queued`, which claims whatever is the head at that later moment. This
//! module proves one concrete consequence of that window: when the head it
//! admitted stops being the head while the take is parked in the admitted
//! message's own live-authority revalidation, the take must NOT return or claim
//! the NEW head. The new head was never admitted by this take — its
//! authorization was withdrawn while the take was parked — so it must stay
//! queued and unconsumed until an invocation actually admits it, at which point
//! that invocation must refuse it for its own (withdrawn) authorization.
//!
//! What this does NOT claim: per-session/per-turn claim identity (CAD-2061) and
//! credential custody (CAD-2148) remain dependencies, and nothing here says
//! anything about remote membership/session/credential revocation, about the
//! actor's post-claim prompt/context work, or about `take_queued`'s own
//! transaction. The unit under test is the RETURNED TAKE and the store state of
//! the two rows — a consumer-observable outcome, never a lock, a field or a
//! private helper.
//!
//! Determinism. The fixture's HTTP-side barrier on the revalidate pull
//! (`Authority::hold_next` / `held` / `released` — the seam the sibling
//! `cad2207_share_revocation_ordering` acceptances already use) parks the
//! admitted head's live-authority pull. `held` is the witness that the daemon
//! is inside `member_authority_live`, i.e. AFTER `queued_head` + the local
//! receipt/share reads, and BEFORE `take_queued`. Nothing is ordered by a
//! sleep: bounded waits only observe the barrier hit and a settled row, and the
//! released pull answers from current fixture state.
//!
//! The head change and why it is a STORE write. Both candidate rows are
//! delivered `assist_job_event` notices on the same member assistant, so both
//! are task-bound. The public `message_cancel` RPC deliberately refuses a member
//! assistant's turn (CAD-2207 privacy), and `Store::cancel` itself refuses a
//! task-bound row, so neither can retire the head. The head is therefore closed
//! with `Store::finish(.., "failed", ..)` — the exact terminal write
//! `take_app_aware`'s own refusal path and the actor loop use for a queued
//! notice, and the same fixture closure the CAD-1380 acceptance uses to stop a
//! row being the queue head. It changes only the head; no share, admission or
//! take semantics are touched, and the admitted job's share is re-asserted
//! afterwards to prove the change was queue-only.
//!
//! The new head's authorization is withdrawn through the REAL
//! `member_job_share {enabled:false}` RPC on the proven-operator relay (with the
//! owner's envelope), not through a store poke: a serialization added inside
//! that RPC is one of the fixes that must keep this test honest, so substituting
//! the store write would let such a fix fail here while the public operation is
//! safe. That RPC performs its own `member_authority_live` pull, so the fixture
//! platform must answer ONE request while the take's pull is parked: this test
//! turns on `Fx::concurrent`, exactly as the sibling race acceptance does.
//! Without it the RPC's pull times out and the test fails loudly at the
//! `reached_platform` precondition — never a false pass.
//!
//! Expected red today (CAD-2207, admitted-head-A / claimed-head-B), both of:
//!   * `cad2207_changed_queue_head_is_never_claimed_under_the_old_admissions_revalidation`
//!     — `take_queued` returns the replacement head B, stores it `submitting` and
//!     hands it back even though B's share was withdrawn before the take
//!     resumed;
//!   * `cad2207_changed_queue_head_waits_for_its_own_admission_then_runs` — the
//!     same claim with B fully authorized, so the only possible justification is
//!     the changed head itself.
//!
//! Both pass only when a take whose head changed refuses to consume the new head
//! and leaves it queued for its own (later) admission.
//!
//! Controls in the same fixture:
//!   * unchanged head — the same barrier, the same real withdrawal of the OTHER
//!     (non-head) job's share, and NOT retiring the head: the take must return
//!     and claim exactly the notice it admitted, with its notice admission
//!     carried through, and must leave the withdrawn non-head row queued;
//!   * the third case above also proves the end of the deferral: after the
//!     changed-head take declines, a fresh take independently admits B (live
//!     share, nothing parked) and claims it, so the guard cannot be a permanent
//!     refusal or a dead queue.
//!
//! Actor liveness (separate from the head-identity cases above):
//! `cad2207_actor_claims_backlog_promptly_after_internal_topic_notice_progress`
//! proves that a take which finishes by consuming an internal-only row — a real
//! CAD-810 topic notice, made `available` inside the claim and never handed to
//! the actor — does not leave already-queued eligible work waiting for the
//! actor's empty-queue poll: the message behind the notice must be CLAIMED inside
//! a bound strictly shorter than `Shared::idle_poll` (5s at the fixture default;
//! the test reads the fixture value and asserts it is that long, so the poll
//! cannot be what delivered the claim). RED against the current admitted-head
//! claim: that take answers `Take::Empty`, the actor waits the poll, and the
//! backlog is still `queued` at the bound. The case names no fix shape —
//! claiming in the same take or re-taking immediately in the actor both pass.
//! The genuinely-empty-queue half (the actor must still WAIT rather than spin)
//! is deliberately not asserted here: it would need an un-woken queue write plus
//! a scheduling assumption instead of a consumer-visible contract, so it stays
//! with the ordinary backlog/empty coverage and the real daemon smoke.
//!
//! Run these acceptances (after the module is registered):
//! `scripts/boat-build -- cargo test --locked --features test-seam --lib
//! cad2207_ -- --test-threads 1`

use super::*;

/// Owner of both jobs; the member whose share the new head's withdrawal targets.
const CAD2207_QUEUE_OWNER: &str = "sub-alice";
/// The member assistant whose queue both notices land in (the subscriber).
const CAD2207_QUEUE_SUBSCRIBER: &str = "sub-ben";

/// A take driven on its own thread, parked in the admitted head's live-authority
/// pull, with cleanup that RUNS ON ASSERTION FAILURE: dropping the handle
/// releases the fixture barrier first and then joins the take, so a failed
/// assertion cannot leave `Fx::drop` joining a platform worker that is still
/// parked on a pull the test never released.
struct Cad2207ParkedTake<'a> {
    fx: &'a Fx,
    handle: Option<std::thread::JoinHandle<()>>,
    outcome: std::sync::mpsc::Receiver<Result<(Option<Box<crate::store::Message>>, bool)>>,
}

impl<'a> Cad2207ParkedTake<'a> {
    /// Start `take_app_aware` on the alias. The thread returns only the
    /// consumer-visible outcome: the claimed message (if any) and whether the
    /// take handed back a notice admission. The admission itself borrows
    /// `Shared`, so it is observed and dropped inside the thread.
    fn spawn(fx: &'a Fx, alias: &str) -> Self {
        let (sender, outcome) = std::sync::mpsc::channel();
        let shared = fx.shared.clone();
        let alias = alias.to_string();
        let handle = std::thread::spawn(move || {
            let taken = shared.take_app_aware(&alias).map(|(take, admission)| {
                let admitted = admission.is_some();
                match take {
                    crate::store::Take::Message(message) => (Some(message), admitted),
                    _ => (None, false),
                }
            });
            let _ = sender.send(taken);
        });
        Self {
            fx,
            handle: Some(handle),
            outcome,
        }
    }

    /// Wait until the fixture platform has PARKED a revalidate pull. The parked
    /// request is the witness that the take is inside `member_authority_live`
    /// for the head it read, before `take_queued` runs.
    fn wait_parked(&self, what: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            {
                let guard = self
                    .fx
                    .authority
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if guard.held {
                    return;
                }
                let (next, _) = self
                    .fx
                    .barrier
                    .wait_timeout(guard, Duration::from_millis(25))
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                // Release the lock here: a failed wait must report ONE failure
                // instead of panicking while holding (and poisoning) the fixture.
                drop(next);
            }
            assert!(std::time::Instant::now() < deadline, "{what}");
        }
    }

    fn release(&self) {
        self.fx.release_barrier();
    }

    /// Release the parked pull, take the outcome (bounded) and join the thread.
    fn result(mut self, what: &str) -> Result<(Option<Box<crate::store::Message>>, bool)> {
        self.release();
        let received = self.outcome.recv_timeout(Duration::from_secs(60));
        if let Some(handle) = self.handle.take() {
            assert!(handle.join().is_ok(), "the take thread panicked: {what}");
        }
        match received {
            Ok(outcome) => outcome,
            Err(_) => panic!("{what}: the take neither claimed nor refused within the bound"),
        }
    }
}

impl<'a> Drop for Cad2207ParkedTake<'a> {
    fn drop(&mut self) {
        self.release();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct Cad2207QueuedNotice {
    job: String,
    notice: crate::store::Message,
}

/// One owner-shared job with a durable subscriber and ONE delivered private
/// notice on the subscriber's assistant — produced by the REAL intent drain
/// (`assist_request_notice` + `route_assist_job_notifications`), which writes
/// the delivery receipt the re-prove requires. Nothing is synthesized or
/// hand-enqueued.
fn cad2207_queued_notice(
    fx: &Fx,
    alias: &str,
    request_key: &str,
    title: &str,
) -> Cad2207QueuedNotice {
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_QUEUE_OWNER),
            "title": title,
            "request_key": request_key,
            "body": format!("{title} spec"),
        }),
    );
    let job = request["job"]
        .as_str()
        .unwrap_or_else(|| panic!("member_job_request returned no job: {request}"))
        .to_string();
    fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_QUEUE_OWNER),
            "job": job,
            "enabled": true,
        }),
    );
    fx.op(
        "member_job_subscribe",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_QUEUE_SUBSCRIBER),
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
            .job_is_subscriber(&job, ORG, CAD2207_QUEUE_SUBSCRIBER)
            .unwrap(),
        "the fixture did not create the durable subscriber row for {job}"
    );
    fx.shared.store.assist_request_notice(&job).unwrap();
    fx.shared
        .route_assist_job_notifications()
        .unwrap_or_else(|e| panic!("the assist intent drain failed: {e}"));
    let notice = fx
        .shared
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
        .unwrap_or_else(|| panic!("no delivered assist notice for {job} on '{alias}'"));
    let task = format!("{job}-t1");
    assert_eq!(
        notice.task_id.as_deref(),
        Some(task.as_str()),
        "the delivered notice does not carry the job's task"
    );
    Cad2207QueuedNotice { job, notice }
}

/// Issue ONE real `member_job_share {enabled:false}` on the owner's envelope and
/// wait, bounded, for its own answer. Runs on its own thread so a serialization
/// that waits for an in-flight reproof cannot silently hang the lane; the bound
/// is a liveness guard only, and on expiry the parked reproof is released before
/// failing.
fn cad2207_withdraw_share_bounded(fx: &Fx, job: &str) -> Result<Value> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let shared = fx.shared.clone();
    let envelope = fx.member_envelope(CAD2207_QUEUE_OWNER);
    let job = job.to_string();
    std::thread::spawn(move || {
        let answer = scoped(Asserted::Operator, || {
            shared.dispatch(
                "member_job_share",
                &json!({"member_envelope": envelope, "job": job, "enabled": false}),
                std::process::id(),
            )
        });
        let _ = sender.send(answer);
    });
    match receiver.recv_timeout(Duration::from_secs(20)) {
        Ok(answer) => answer,
        Err(_) => {
            fx.release_barrier();
            panic!(
                "the owner's share withdrawal neither completed nor answered an explicit refusal \
                 while a take for another message was parked: an in-flight revalidation must not \
                 block the owner's own share change (CAD-2207)"
            );
        }
    }
}

/// Did this exact message ever get CLAIMED? The claim is the store's own
/// `submitting` event, written inside `take_queued`'s transaction before any
/// prompt, context or provider work.
fn cad2207_claim_event(fx: &Fx, alias: &str, message: &str) -> bool {
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

fn cad2207_refusal_reason(message: &crate::store::Message) -> Option<&str> {
    message
        .error
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .or_else(|| {
            message
                .result
                .as_ref()
                .and_then(|result| result.get("error"))
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
        })
}

/// CAD-2207 (negative acceptance, red-capable): a take that admitted head A must
/// not consume the head that replaced A while that admission was still being
/// revalidated — and the replacement's authorization was withdrawn before the
/// take resumed, so consuming it is an unauthorized claim.
///
/// Setup: two owner-shared jobs, both subscribed by the same member assistant.
/// Job A's notice is delivered first (it is the queue head); job B's notice is
/// delivered second. The take reads A, enters A's live-authority pull and parks
/// there. While parked, A is retired (it stops being the head) and B's share is
/// withdrawn through the real RPC. On release the take's atomic `take_queued`
/// sees B as the head.
///
/// RED today: the take returns B, persists it `submitting` and hands it to the
/// caller — a message this take never admitted and whose authorization was
/// already gone. Expected green: nothing is claimed, B stays `queued`, and a
/// later take that actually admits B refuses it for its own withdrawn share.
#[test]
fn cad2207_changed_queue_head_is_never_claimed_under_the_old_admissions_revalidation() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let subscriber = fx.member_runtime(CAD2207_QUEUE_SUBSCRIBER);
    let _owner = fx.member_runtime(CAD2207_QUEUE_OWNER);
    let alias = subscriber.alias.clone();

    let first = cad2207_queued_notice(
        &fx,
        &alias,
        "cad2207-queue-head-race-job-a",
        "cad2207 queue head race job a",
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&first.job, ORG)
            .unwrap(),
        "the admitted head's job is not shared: the head is not authorized"
    );
    let second = cad2207_queued_notice(
        &fx,
        &alias,
        "cad2207-queue-head-race-job-b",
        "cad2207 queue head race job b",
    );
    assert_ne!(
        first.job, second.job,
        "the fixture produced the same canonical job twice"
    );

    // Precondition, loud if the fixture ever reorders: the take's `queued_head`
    // read is genuinely the first notice, and the second is behind it.
    let head = fx
        .shared
        .store
        .queued_head(&alias)
        .unwrap()
        .unwrap_or_else(|| panic!("nothing is queued on '{alias}'"));
    assert_eq!(
        head.id, first.notice.id,
        "the fixture's queue head is not the notice the take will admit"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&second.notice.id)
            .unwrap()
            .expect("the second notice vanished")
            .state,
        "queued",
        "the second notice is not queued behind the head"
    );
    // Precondition: the admitted head's own re-prove is possible in this
    // fixture (binding, delivery receipt, live share, reachable authority), so
    // a park timeout below can only mean the take never reached it.
    fx.shared
        .member_message_reprove(&first.notice)
        .unwrap_or_else(|e| panic!("the fixture's admitted head cannot re-prove: {e}"));

    // The withdrawal RPC below performs its own live-authority pull while the
    // take's pull is parked: the fixture platform must answer it off the accept
    // loop.
    fx.concurrent.store(true, Ordering::SeqCst);
    {
        let mut guard = fx
            .authority
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.hold_next = true;
    }

    let take = Cad2207ParkedTake::spawn(&fx, &alias);
    take.wait_parked("the take never reached the live-authority revalidation of the head it read");

    // Witness: the take is parked BEFORE any claim, on the admitted head.
    let parked = fx
        .shared
        .store
        .message(&first.notice.id)
        .unwrap()
        .expect("the admitted head vanished while parked");
    assert_eq!(
        parked.state, "queued",
        "the head was claimed before its own revalidation finished: {parked:?}"
    );
    assert!(
        !cad2207_claim_event(&fx, &alias, &first.notice.id),
        "the head was claimed before its own revalidation finished"
    );

    // HEAD CHANGE: retire A while the take is parked in A's revalidation. This
    // is the store's own terminal write for a queued notice (the RPC cancel is
    // deliberately closed for a member assistant's private turn, and
    // `Store::cancel` refuses a task-bound row); see the module header.
    let retired = json!({
        "status": "failed",
        "text": "",
        "error": "cad2207 queue-head race: the head was retired while its take revalidated it",
    });
    fx.shared
        .store
        .finish(
            &first.notice,
            "failed",
            &retired,
            Some("cad2207 queue-head race: the head was retired while its take revalidated it"),
        )
        .unwrap_or_else(|e| panic!("the fixture could not retire the head: {e}"));
    assert_eq!(
        fx.shared
            .store
            .message(&first.notice.id)
            .unwrap()
            .expect("the retired head vanished")
            .state,
        "failed",
        "the head did not actually stop being the head"
    );
    let changed = fx
        .shared
        .store
        .queued_head(&alias)
        .unwrap()
        .expect("no queue head after the admitted head was retired");
    assert_eq!(
        changed.id, second.notice.id,
        "the queue head did not change to the second job's notice"
    );

    // The replacement's authorization is withdrawn BEFORE the take resumes, so
    // any claim of it by this take is unauthorized.
    let withdrawn = cad2207_withdraw_share_bounded(&fx, &second.job)
        .unwrap_or_else(|e| panic!("the owner's share withdrawal was refused: {e}"));
    assert_eq!(
        withdrawn["shared"],
        json!(false),
        "member_job_share did not report the withdrawal: {withdrawn}"
    );
    assert!(
        !fx.shared
            .store
            .assist_job_is_shared(&second.job, ORG)
            .unwrap(),
        "the withdrawal was reported but the share row survived"
    );

    let (taken, admitted) = take
        .result("the take never returned after its admission was released")
        .unwrap_or_else(|e| panic!("the take failed instead of answering: {e}"));

    assert!(
        taken.is_none(),
        "the take revalidated head A, A stopped being the head while that revalidation was \
         parked, and the take then CLAIMED '{}' (state '{}', admission={admitted}) — a message \
         this take never admitted, whose share was withdrawn before the take resumed. A changed \
         head must stay queued until its own admission (CAD-2207)",
        taken
            .as_ref()
            .map(|message| message.id.as_str())
            .unwrap_or("<none>"),
        taken
            .as_ref()
            .map(|message| message.state.as_str())
            .unwrap_or("<none>"),
    );

    let replacement = fx
        .shared
        .store
        .message(&second.notice.id)
        .unwrap()
        .expect("the replacement head vanished");
    assert_eq!(
        replacement.state, "queued",
        "the changed head was not left queued (state '{}', error {:?}) — the take consumed a \
         message whose authorization it never proved (CAD-2207)",
        replacement.state, replacement.error
    );
    assert!(
        replacement.turn_id.is_none(),
        "the changed head reached a provider turn: {replacement:?}"
    );
    assert!(
        !cad2207_claim_event(&fx, &alias, &second.notice.id),
        "the changed head was CLAIMED (`submitting`) under the retired head's admission \
         (CAD-2207)"
    );

    // The head change was queue-only: the admitted job's share was never touched.
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&first.job, ORG)
            .unwrap(),
        "retiring the head also withdrew its job's share — the fixture changed the wrong axis"
    );

    // The deferral is not a dead queue: the next take admits B on its own terms
    // and refuses it for its own withdrawn share, with a recorded reason.
    let (independent, _) = fx
        .shared
        .take_app_aware(&alias)
        .unwrap_or_else(|e| panic!("the independent take failed: {e}"));
    // `Take` carries no Debug, so the observable is spelled out instead.
    let independently_claimed = match &independent {
        crate::store::Take::Message(message) => Some(message.id.clone()),
        crate::store::Take::Stop | crate::store::Take::Empty => None,
    };
    assert!(
        independently_claimed.is_none(),
        "a later take claimed the withdrawn notice {independently_claimed:?} instead of \
         refusing it on its own admission"
    );
    let refused = fx
        .shared
        .store
        .message(&second.notice.id)
        .unwrap()
        .expect("the refused notice vanished");
    assert_eq!(
        refused.state, "failed",
        "the withdrawn notice was not explicitly refused by its own admission: {refused:?}"
    );
    assert!(
        cad2207_refusal_reason(&refused).is_some(),
        "the independently refused notice carries no recorded reason: {refused:?}"
    );
    assert!(
        !cad2207_claim_event(&fx, &alias, &second.notice.id),
        "the withdrawn notice was claimed (`submitting`) even by its own admission (CAD-2207)"
    );
    assert!(
        fx.shared
            .store
            .events(&alias, 0, 1000)
            .unwrap()
            .into_iter()
            .any(|event| event.kind == "member_turn_refused"
                && event.payload.get("message").and_then(Value::as_str)
                    == Some(second.notice.id.as_str())),
        "no member_turn_refused event names the withdrawn notice: the refusal was not durable"
    );
}

/// CAD-2207 control: the SAME barrier, the SAME real withdrawal of the other
/// job's share, and NO head change. The take must claim exactly the notice it
/// admitted, carry that notice's admission through, and leave the withdrawn
/// non-head row untouched — so the negative above can only be red because the
/// head changed under the take, not because a park or a withdrawal refuses
/// everything.
#[test]
fn cad2207_unchanged_queue_head_still_claims_its_own_admitted_notice() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let subscriber = fx.member_runtime(CAD2207_QUEUE_SUBSCRIBER);
    let _owner = fx.member_runtime(CAD2207_QUEUE_OWNER);
    let alias = subscriber.alias.clone();

    let admitted = cad2207_queued_notice(
        &fx,
        &alias,
        "cad2207-queue-head-control-job-a",
        "cad2207 queue head control job a",
    );
    let other = cad2207_queued_notice(
        &fx,
        &alias,
        "cad2207-queue-head-control-job-b",
        "cad2207 queue head control job b",
    );
    assert_ne!(admitted.job, other.job);
    let head = fx
        .shared
        .store
        .queued_head(&alias)
        .unwrap()
        .unwrap_or_else(|| panic!("nothing is queued on '{alias}'"));
    assert_eq!(
        head.id, admitted.notice.id,
        "control: the admitted notice is not the queue head"
    );
    // Precondition, exactly as in the negative: the head's own re-prove works
    // in this fixture, so a park timeout below is a harness failure and never a
    // false pass.
    fx.shared
        .member_message_reprove(&admitted.notice)
        .unwrap_or_else(|e| panic!("control: the fixture's head cannot re-prove: {e}"));

    fx.concurrent.store(true, Ordering::SeqCst);
    {
        let mut guard = fx
            .authority
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.hold_next = true;
    }

    let take = Cad2207ParkedTake::spawn(&fx, &alias);
    take.wait_parked("control: the take never reached the head's live-authority revalidation");

    // Mirror of the negative's withdrawal, on the row that is NOT the head.
    let withdrawn = cad2207_withdraw_share_bounded(&fx, &other.job)
        .unwrap_or_else(|e| panic!("control: the owner's share withdrawal was refused: {e}"));
    assert_eq!(
        withdrawn["shared"],
        json!(false),
        "control: member_job_share did not report the withdrawal: {withdrawn}"
    );
    assert!(
        !fx.shared
            .store
            .assist_job_is_shared(&other.job, ORG)
            .unwrap(),
        "control: the other job's share was not actually withdrawn"
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&admitted.job, ORG)
            .unwrap(),
        "control: withdrawing the non-head job also withdrew the head's share"
    );

    let (taken, carried) = take
        .result("control: the take never returned after its admission was released")
        .unwrap_or_else(|e| panic!("control: the take failed instead of answering: {e}"));
    let message = taken.unwrap_or_else(|| {
        panic!(
            "control: the take claimed nothing even though the head it admitted was unchanged \
             and authorized"
        )
    });
    assert_eq!(
        message.id, admitted.notice.id,
        "control: the take claimed a different message than the head it admitted"
    );
    assert!(
        carried,
        "control: the admitted notice's admission was not carried through the take"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&admitted.notice.id)
            .unwrap()
            .expect("control: the claimed notice vanished")
            .state,
        "submitting",
        "control: the admitted unchanged head was not claimed"
    );
    assert!(
        cad2207_claim_event(&fx, &alias, &admitted.notice.id),
        "control: no submitting event names the claimed head"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&other.notice.id)
            .unwrap()
            .expect("control: the withdrawn non-head notice vanished")
            .state,
        "queued",
        "control: the take consumed the withdrawn notice that was not its head"
    );
    assert!(
        !cad2207_claim_event(&fx, &alias, &other.notice.id),
        "control: the non-head withdrawn notice was claimed by the head's take"
    );
}

/// CAD-2207 (behavioural, authorization-independent): the changed-head compare
/// is the race, not the replacement's authorization. The replacement head B is
/// fully authorized here — its share is live throughout and nobody withdraws
/// anything — so nothing except "this take admitted A, not B" can justify
/// refusing to consume it. The take that admitted A must leave B queued, and the
/// very next take, which DOES admit B, must claim it. That is the "until
/// independently admitted" half, proven on a live share.
#[test]
fn cad2207_changed_queue_head_waits_for_its_own_admission_then_runs() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let subscriber = fx.member_runtime(CAD2207_QUEUE_SUBSCRIBER);
    let _owner = fx.member_runtime(CAD2207_QUEUE_OWNER);
    let alias = subscriber.alias.clone();

    let first = cad2207_queued_notice(
        &fx,
        &alias,
        "cad2207-queue-head-live-job-a",
        "cad2207 queue head live job a",
    );
    let second = cad2207_queued_notice(
        &fx,
        &alias,
        "cad2207-queue-head-live-job-b",
        "cad2207 queue head live job b",
    );
    assert_ne!(first.job, second.job);
    let head = fx
        .shared
        .store
        .queued_head(&alias)
        .unwrap()
        .unwrap_or_else(|| panic!("nothing is queued on '{alias}'"));
    assert_eq!(
        head.id, first.notice.id,
        "the fixture's queue head is not the notice the take will admit"
    );
    fx.shared
        .member_message_reprove(&first.notice)
        .unwrap_or_else(|e| panic!("the fixture's admitted head cannot re-prove: {e}"));

    {
        let mut guard = fx
            .authority
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.hold_next = true;
    }
    let take = Cad2207ParkedTake::spawn(&fx, &alias);
    take.wait_parked("the take never reached the live-authority revalidation of the head it read");

    // Only the head changes: B keeps its live share and its authorization.
    fx.shared
        .store
        .finish(
            &first.notice,
            "failed",
            &json!({
                "status": "failed",
                "text": "",
                "error": "cad2207 queue-head race: the head was retired while its take revalidated it",
            }),
            Some("cad2207 queue-head race: the head was retired while its take revalidated it"),
        )
        .unwrap_or_else(|e| panic!("the fixture could not retire the head: {e}"));
    let changed = fx
        .shared
        .store
        .queued_head(&alias)
        .unwrap()
        .expect("no queue head after the admitted head was retired");
    assert_eq!(
        changed.id, second.notice.id,
        "the queue head did not change to the second job's notice"
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&second.job, ORG)
            .unwrap(),
        "the replacement head's authorization was not live"
    );

    let (taken, _) = take
        .result("the take never returned after its admission was released")
        .unwrap_or_else(|e| panic!("the take failed instead of answering: {e}"));
    assert!(
        taken.is_none(),
        "the take admitted head A and then CLAIMED the replacement head '{}' even though that \
         replacement was never admitted by this invocation: a changed head must wait for its own \
         admission (CAD-2207)",
        taken
            .as_ref()
            .map(|message| message.id.as_str())
            .unwrap_or("<none>"),
    );
    let waiting = fx
        .shared
        .store
        .message(&second.notice.id)
        .unwrap()
        .expect("the replacement head vanished");
    assert_eq!(
        waiting.state, "queued",
        "the changed head was not left queued (state '{}', error {:?}) — the take consumed a \
         message it never admitted (CAD-2207)",
        waiting.state, waiting.error
    );
    assert!(
        !cad2207_claim_event(&fx, &alias, &second.notice.id),
        "the changed head was CLAIMED (`submitting`) under the retired head's admission \
         (CAD-2207)"
    );

    // Independent admission: a fresh take, nothing parked, B's own live share.
    let (independent, carried) = fx
        .shared
        .take_app_aware(&alias)
        .unwrap_or_else(|e| panic!("the independent take failed: {e}"));
    let claimed = match independent {
        crate::store::Take::Message(message) => *message,
        crate::store::Take::Stop | crate::store::Take::Empty => panic!(
            "the authorized replacement head was never admitted by its own take — the changed-head \
             guard refused it permanently"
        ),
    };
    assert_eq!(
        claimed.id, second.notice.id,
        "the independent take claimed a different message than the head it admitted"
    );
    assert!(
        carried.is_some(),
        "the replacement head's notice admission was not carried through its own take"
    );
    assert_eq!(
        fx.shared
            .store
            .message(&second.notice.id)
            .unwrap()
            .expect("the independently claimed notice vanished")
            .state,
        "submitting",
        "the independently admitted replacement head was not claimed"
    );
    assert!(
        cad2207_claim_event(&fx, &alias, &second.notice.id),
        "no submitting event names the independently claimed replacement head"
    );
}

// ============================================================================
// CAD-2207 liveness: INTERNAL progress inside one take must not park the actor
// on its empty-queue poll while real backlog is already queued.
//
// `take_app_aware` can finish a take without returning a message even though an
// eligible message is already queued behind the row it consumed: a CAD-810
// topic notice is made `available` and never reaches the actor, an unresolved
// routed row is failed, an inactive nudge is skipped. The actor then sees
// `Take::Empty` and waits `idle_poll` (5s in this fixture) before its next take,
// so that backlog is delayed by a poll interval per internally-consumed row.
//
// This acceptance is behaviour-level and does not name a fix shape: it observes
// (a) the internal-only head actually being consumed, then (b) the eligible
// message behind it being CLAIMED within a bound strictly shorter than the
// configured poll. A correct fix may claim it in the same take or re-take
// immediately in the actor; either passes. The genuinely-empty-queue half (the
// actor must still WAIT rather than spin) is deliberately not asserted here: it
// would need an un-woken queue write plus a scheduling assumption instead of a
// consumer-visible contract, so it stays with the ordinary backlog/empty
// coverage and the real daemon smoke.
// ============================================================================

/// An ordinary (non-member) runtime: no assistant binding, so the re-prove is a
/// no-op and the test isolates the queue/actor liveness question.
fn cad2207_liveness_agent(fx: &Fx, alias: &str) {
    let cwd = fx.dir.path().join("cad2207-liveness-cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias,
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "workspace-write",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap_or_else(|e| panic!("the fixture could not register '{alias}': {e}"));
}

/// Serve an already-registered runtime on the shipped in-process provider
/// (`endpoint_kind = "fake"`). The binding, its principal cwd and every guard are
/// untouched — only how its turns are executed changes, and that is the same
/// `ProviderAdapter` contract a managed runtime follows. Local to this module:
/// the sibling share-revocation acceptance keeps its own copy private, so this
/// file cannot borrow it.
fn cad2207_liveness_provider(fx: &Fx, alias: &str) {
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET provider='fake', endpoint_kind='fake' WHERE alias=?",
                rusqlite::params![alias],
            )?;
            Ok(())
        })
        .unwrap_or_else(|e| {
            panic!("the fixture could not serve '{alias}' on the in-process provider: {e}")
        });
    fx.shared
        .store
        .set_enabled(alias, true)
        .unwrap_or_else(|e| panic!("the fixture could not enable '{alias}': {e}"));
}

/// Stop one fixture runtime and wait, bounded, for ownership to clear so the
/// torn-down state dir is never touched by a live thread. Local to this module:
/// the sibling share-revocation acceptance keeps its own copy private.
fn cad2207_liveness_stop(fx: &Fx, alias: &str) {
    let _ = fx.shared.rpc_stop(&json!({"alias": alias}));
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while fx.shared.lifecycle.lock().unwrap().owned(alias) {
        assert!(
            std::time::Instant::now() < deadline,
            "the fixture actor for '{alias}' did not stop"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The actor's empty-queue poll, read from the fixture (the actor loop is
/// `Shared`, so this IS the value `run_actor` waits on). Requiring it to stay
/// well above the observations below keeps a pass meaningful: a claim inside the
/// bound cannot have come from the poll.
fn cad2207_actor_poll(fx: &Fx) -> Duration {
    let poll = fx.shared.idle_poll;
    assert!(
        poll >= Duration::from_secs(5),
        "the fixture's empty-queue poll is {poll:?}: this acceptance needs it well above its \
         bounded observations"
    );
    poll
}

/// Stops the fixture actor on the way out — including on an assertion failure —
/// so a red acceptance cannot leave a live thread polling a removed state dir.
struct Cad2207ActorStop<'a> {
    fx: &'a Fx,
    alias: &'a str,
}

impl<'a> Drop for Cad2207ActorStop<'a> {
    fn drop(&mut self) {
        // Idempotent: the success path has already stopped it, and a stop that
        // finds nothing running is not an error worth reporting twice.
        let _ = self.fx.shared.rpc_stop(&json!({"alias": self.alias}));
    }
}

/// Wait, bounded, for the topic notice to be made `available` — the
/// store-observable witness that the take consumed it internally.
fn cad2207_await_available(
    fx: &Fx,
    id: &str,
    bound: Duration,
    what: &str,
) -> crate::store::Message {
    let deadline = std::time::Instant::now() + bound;
    loop {
        let message = fx
            .shared
            .store
            .message(id)
            .unwrap()
            .unwrap_or_else(|| panic!("{what}: the notice vanished"));
        // The state is copied so the "available" arm can move the row out
        // without the match's scrutinee borrow still being live.
        let state = message.state.clone();
        match state.as_str() {
            "available" => return message,
            "queued" => {}
            other => panic!(
                "{what}: the topic notice settled '{other}' instead of being made available \
                 ({message:?}) — the fixture's delivery provenance is wrong, not the queue"
            ),
        }
        assert!(std::time::Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Wait, bounded, for `id` to leave `queued` (claimed or otherwise settled).
fn cad2207_await_not_queued(
    fx: &Fx,
    id: &str,
    bound: Duration,
    what: &str,
) -> crate::store::Message {
    let deadline = std::time::Instant::now() + bound;
    loop {
        let message = fx
            .shared
            .store
            .message(id)
            .unwrap()
            .unwrap_or_else(|| panic!("{what}: the message vanished"));
        if message.state != "queued" {
            return message;
        }
        assert!(std::time::Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// CAD-2207 (liveness, expected RED against the current admitted-head claim): an
/// actor that internally consumed the admitted head (a real CAD-810 topic
/// notice) must promptly claim the ELIGIBLE message already queued behind it,
/// not wait out its empty-queue poll first.
///
/// Setup, all through supported fixture operations: one ordinary runtime
/// subscribed to a real `fleet` topic; a real `topic_publish` queues its
/// `topic_notice` as the head; an ordinary message is queued behind it. The
/// actor is then launched with both rows present, so its FIRST take admits the
/// notice. The notice becoming `available` proves that take internal-progressed.
/// From that moment the eligible message must be claimed within a bound
/// strictly shorter than `Shared::idle_poll` (5s), so the poll cannot be the
/// thing that delivers it.
///
/// RED today: after consuming the notice the take answers `Take::Empty`, the
/// actor waits the whole poll, and the backlog is still `queued` at the bound.
#[test]
fn cad2207_actor_claims_backlog_promptly_after_internal_topic_notice_progress() {
    let fx = Fx::new();
    let alias = "cad2207-liveness-worker";
    cad2207_liveness_agent(&fx, alias);
    cad2207_liveness_provider(&fx, alias);

    // A real fleet topic with this ordinary runtime as a subscriber, then a real
    // publish: its queued `topic_notice` is the internal-only head.
    let created = fx.op(
        "topic_create",
        json!({"type": "fleet", "source": "cad2207-liveness-topic"}),
    );
    let topic = created["topic"]
        .as_str()
        .unwrap_or_else(|| panic!("topic_create named no topic: {created}"))
        .to_string();
    fx.op("topic_subscribe", json!({"topic": topic, "alias": alias}));
    let published = fx.op(
        "topic_publish",
        json!({
            "topic": topic,
            "text": "cad2207 liveness topic update",
            "id": "cad2207-liveness-post",
        }),
    );
    assert!(
        published["deliveries"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["alias"].as_str() == Some(alias))),
        "the publish delivered no notice to the subscribed runtime: {published}"
    );
    let notices: Vec<crate::store::Message> = fx
        .shared
        .store
        .messages(alias)
        .unwrap()
        .into_iter()
        .filter(|message| message.source == crate::proto::TOPIC_NOTICE_SOURCE)
        .collect();
    assert_eq!(
        notices.len(),
        1,
        "the publish queued {} topic notices on '{alias}'",
        notices.len()
    );
    let notice = notices.into_iter().next().unwrap();

    // The eligible ordinary message sits BEHIND the internal-only head.
    let backlog = "cad2207-liveness-backlog";
    fx.shared
        .store
        .enqueue(alias, "cad2207 liveness backlog", None, backlog, "user")
        .unwrap_or_else(|e| panic!("the backlog message was refused: {e}"));
    let head = fx
        .shared
        .store
        .queued_head(alias)
        .unwrap()
        .unwrap_or_else(|| panic!("nothing is queued on '{alias}'"));
    assert_eq!(
        head.id, notice.id,
        "the topic notice is not the head the take will admit"
    );

    let poll = cad2207_actor_poll(&fx);
    let bound = Duration::from_secs(2);
    assert!(
        bound < poll,
        "the observation bound {bound:?} must sit inside the empty-queue poll {poll:?}"
    );

    // Stop the actor even if an assertion below fails, so a red run cannot leak
    // a live thread polling a removed state dir.
    let _stop = Cad2207ActorStop { fx: &fx, alias };
    fx.shared
        .launch_actor(alias)
        .unwrap_or_else(|e| panic!("the runtime's actor did not start: {e}"));
    let consumed = cad2207_await_available(
        &fx,
        &notice.id,
        Duration::from_secs(10),
        "the actor never consumed the queued topic notice",
    );
    assert_eq!(
        consumed.source,
        crate::proto::TOPIC_NOTICE_SOURCE,
        "the consumed head was not the topic notice"
    );

    // The advertised observable: the backlog behind the consumed head is claimed
    // well inside the poll.
    let claimed = cad2207_await_not_queued(
        &fx,
        backlog,
        bound,
        "the actor did not claim the eligible message already queued behind the topic notice \
         within the observation bound: after consuming the notice the take answered 'empty' and \
         the actor went back to its idle poll, so real backlog waits a full poll interval behind \
         internal-only work (CAD-2207)",
    );
    assert!(
        matches!(
            claimed.state.as_str(),
            "submitting" | "running" | "completed"
        ),
        "the backlog settled '{}' instead of being claimed: {claimed:?}",
        claimed.state
    );
    assert!(
        cad2207_claim_event(&fx, alias, backlog),
        "the backlog left `queued` without a `submitting` claim event: {claimed:?}"
    );

    cad2207_liveness_stop(&fx, alias);
}
