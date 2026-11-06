//! CAD-2207 — LOCAL share-revocation ordering against an in-flight notice turn.
//!
//! Independently authored negative acceptance, deliberately separate from the
//! implementation and from the shared `team_assistants_refusal` checks. This
//! module is a CHILD of that module only to reuse its already-real fixture
//! (`Fx`: a real in-process daemon, a real `operator/board-identity.json` trust
//! root and one loopback authoritative platform answering
//! `/__platform/assistant/revalidate`). The only fixture capability this module
//! asks for is that ONE parked pull is finished off the accept loop, so the
//! racing RPC's own pull can still be answered (see PREREQUISITE below); the
//! decisions, the store and every guard are the existing ones.
//!
//! SCOPE, stated narrowly so it cannot be over-read: this exercises the OWNER's
//! LOCAL `member_job_share {enabled:false}` operation — the public RPC, end to
//! end, including its own live-authority pull and its `shared:false` answer —
//! racing a DELIVERED private `assist_job_event` notice through the production
//! actor loop. It says NOTHING about remote membership/session/credential/grant
//! revocation: that authority lives in the platform service behind
//! `member_authority_live`, is re-derived per effect there, and cannot be
//! serialized by a local lock. Conflating the two would claim coverage this test
//! does not have.
//!
//! The window under test is the one the ticket names. `daemon/actor.rs` calls
//! `Shared::admit_app_submission` immediately before `adapter.run_turn_slotted`,
//! and for an `assist_job_event` that gate is the FINAL
//! `member_message_reprove`: it reads the owner's live share (`ownership` or
//! `assist_job_is_shared`), THEN performs its live-authority pull
//! (`assist_notice_reprove` → `member_authority_live`), and only after both does
//! the actor hand the notice to the provider. A revocation that COMPLETES inside
//! that window must never be followed by a provider submission of the notice; if
//! the revocation cannot complete, its answer must be an explicit
//! pending/refusal — never a plain success. A submission that provably precedes
//! the revocation is already in flight and is not undone by anything asserted
//! here.
//!
//! Determinism: the fixture's HTTP-side barrier on the revalidate pull
//! (`Authority::hold_next` / `held` / `released` — the seam
//! `revoke_during_inflight_dispatch_pull_refuses_kickoff` already uses). This
//! path pulls live authority exactly twice for the notice: once in the take gate
//! (`take_app_aware`), once in the final re-prove. The barrier is re-armed
//! between them, so the second parked pull is provably the final re-prove, whose
//! owner-share read has ALREADY happened. Nothing is ordered by a sleep; bounded
//! waits only observe a barrier hit and a settled row.
//!
//! PREREQUISITE — one concurrent fixture answer. The revocation is issued as the
//! REAL `member_job_share {enabled:false}` RPC, through the proven-operator
//! relay, because a serialization added in `rpc_member_job_share` (before or
//! after its own live-authority pull) is exactly the fix that must make this
//! test pass; substituting `Store::assist_job_share` would let such a fix fail
//! here while the public operation is safe. That RPC performs its own
//! `member_authority_live` pull, so the platform must answer ONE request while
//! the notice's final re-prove is parked. This test therefore turns on
//! `Fx::concurrent`, which serves each platform request on its own worker
//! (default: the existing single-threaded accept loop, untouched for every
//! other fixture). Without that capability the RPC's pull would time out and
//! the test fails at the `reached_platform` precondition below — a loud harness
//! failure, never a false pass. The control test needs no concurrency: it drives
//! the same RPC with nothing in flight.
//!
//! The submission is the production one: the bound assistant is served by the
//! in-process `ProviderAdapter` this repo ships for tests
//! (`endpoint_kind = "fake"`), so the real actor loop, the real `run_turn_slotted`
//! call and the real store transitions run unchanged. Only the launch surface
//! differs, and no provider echo text is asserted.
//!
//! Expected red today (CAD-2207, admitted-before-revoke / sent-after-revoke):
//! `cad2207_completed_share_revocation_is_never_followed_by_a_notice_submission`
//! fails on the assertion that a completed revocation was followed by a provider
//! submission — the notice reaches `completed` with a `turn_id` after the real
//! `member_job_share {enabled:false}` call returned `shared:false` and the share
//! row was gone. It passes only when the submission is ordered against the
//! owner's share mutation (the in-flight turn is refused before the provider, or
//! the revocation answers an explicit pending/refusal instead of success).
//!
//! Run just this module's acceptances:
//! `scripts/boat-build -- cargo test --locked --features test-seam --lib cad2207_ -- --test-threads 1`

use super::*;

/// Wait until the fixture platform has PARKED a `revalidate` pull. The parked
/// request is the witness that the daemon is inside `member_authority_live`,
/// i.e. past the local reads that precede it and before whatever follows it.
fn cad2207_pull_parked(fx: &Fx, what: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        {
            let guard = fx
                .authority
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if guard.held {
                return;
            }
            let (next, _) = fx
                .barrier
                .wait_timeout(guard, Duration::from_millis(25))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // The wait's guard is released here; the diagnostic below runs
            // outside the lock so a failed wait reports ONE failure instead of
            // panicking while holding (and poisoning) the fixture.
            drop(next);
        }
        assert!(std::time::Instant::now() < deadline, "{what}");
    }
}

/// Release the parked pull and park the NEXT one. The released pull answers from
/// the fixture's CURRENT state (ALLOW while the member is live); the caller
/// proceeds and its next pull parks instead of being answered. `held` is cleared
/// first so the second park is observable on its own — otherwise only the first
/// pull of this turn could be witnessed.
fn cad2207_release_and_park_next(fx: &Fx, what: &str) {
    let was_parked = {
        let mut guard = fx
            .authority
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let was_parked = guard.held;
        if was_parked {
            guard.held = false;
            guard.hold_next = true;
            guard.released = true;
            fx.barrier.notify_all();
        }
        was_parked
    };
    // Outside the lock: a failed wait must report one test failure, never
    // poison the fixture (CAD-2207).
    assert!(was_parked, "{what}: no pull was parked");
    cad2207_pull_parked(fx, what);
}

/// The notice row, waited until it reaches a terminal state. A turn left
/// silently `queued`/`submitting` is itself a failure: an inadmissible notice
/// must be refused explicitly and an admitted one must settle.
fn cad2207_settle_notice(fx: &Fx, id: &str) -> crate::store::Message {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let message = fx
            .shared
            .store
            .message(id)
            .unwrap()
            .expect("the delivered notice vanished");
        if matches!(
            message.state.as_str(),
            "completed" | "failed" | "cancelled" | "rejected" | "interrupted" | "unknown"
        ) {
            return message;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the notice never settled (state '{}'): a completed share revocation left it \
             silently pending instead of refusing it (CAD-2207)",
            message.state
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Did this notice reach the provider? `turn_id` is written by the adapter's own
/// `on_started`; `running`/`completed` mean the turn was submitted. The reply
/// text is never consulted.
fn cad2207_notice_submitted(message: &crate::store::Message) -> bool {
    message.turn_id.is_some() || matches!(message.state.as_str(), "running" | "completed")
}

/// The recorded refusal reason of a notice, if any.
fn cad2207_refusal_text(message: &crate::store::Message) -> Option<&str> {
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

/// Every delivered job notice on one alias (the only source this module counts).
fn cad2207_notices(fx: &Fx, alias: &str) -> Vec<crate::store::Message> {
    fx.shared
        .store
        .messages(alias)
        .unwrap()
        .into_iter()
        .filter(|message| message.source == "assist_job_event")
        .collect()
}

/// The status the STORE records for ONE assist intent — the same event row the
/// watch-loop router drains, so `delivered`/`refused`/`unresolved` are the
/// router's real outcomes, never a test mirror.
fn cad2207_intent_status(
    fx: &Fx,
    job: &str,
    dedupe: &str,
    target: &str,
    sub: Option<&str>,
) -> String {
    let events = fx.shared.store.job_events(job, 0, 500).unwrap();
    let intent = events
        .into_iter()
        .find(|event| {
            event.kind == "assist_intent"
                && event.payload.get("dedupe").and_then(Value::as_str) == Some(dedupe)
                && event.payload.get("target").and_then(Value::as_str) == Some(target)
                && event.payload.get("sub").and_then(Value::as_str) == sub
        })
        .unwrap_or_else(|| {
            panic!(
                "no assist intent for job {job} (dedupe '{dedupe}', target '{target}', sub {sub:?})"
            )
        });
    intent
        .payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the assist intent for '{dedupe}' carries no status"))
        .to_string()
}

/// Was this exact message ever CLAIMED by the actor? The claim is the store's own
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

/// Issue ONE real `member_job_share {enabled:false}` call and wait, bounded, for
/// its own answer. The call runs on its own thread so a serialization that waits
/// for an in-flight reproof of the same job cannot silently hang the whole lane;
/// the bound is a liveness guard only (a non-blocking implementation answers in
/// milliseconds) and on expiry the parked reproof is released before failing.
fn cad2207_disable_share_bounded(fx: &Fx, job: &str) -> Result<Value> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let shared = fx.shared.clone();
    let envelope = fx.member_envelope("sub-alice");
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
                "the owner's share disable neither completed nor answered an explicit refusal while \
                 a notice reproof for the same job was in flight: an in-flight reproof must not \
                 block the owner's own share change (CAD-2207)"
            );
        }
    }
}

/// Serve an already-bound member assistant on the shipped in-process provider
/// (`endpoint_kind = "fake"`). The binding, its principal cwd, its private
/// conversation and every member guard are untouched — only how its turns are
/// executed changes, and that is the same `ProviderAdapter` contract a managed
/// runtime follows (`FakeAdapter` drives queueing, admission and outcome
/// handling, never a model call).
fn cad2207_serve_on_fake_provider(fx: &Fx, alias: &str) {
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

/// Stop the fixture actor and wait for ownership to clear, so the torn-down
/// state dir is never touched by a live thread.
fn cad2207_quiet(fx: &Fx, alias: &str) {
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

/// Owner share live, durable subscriber, and ONE delivered private notice each —
/// produced by the REAL intent drain (`assist_request_notice` +
/// `route_assist_job_notifications`), which writes the delivery receipts the
/// re-prove requires. No notice is synthesized or hand-enqueued.
struct Cad2207SharedNotice {
    job: String,
    owner_alias: String,
    subscriber_alias: String,
    subscriber_notice: crate::store::Message,
    owner_notice: crate::store::Message,
}

fn cad2207_shared_notice(fx: &Fx) -> Cad2207SharedNotice {
    let owner = fx.member_runtime("sub-alice");
    let subscriber = fx.member_runtime("sub-ben");
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope("sub-alice"),
            "title": "cad2207 revocation ordering job",
            "request_key": "cad2207-revocation-ordering-job",
            "body": "cad2207 revocation ordering spec",
        }),
    );
    let job = request["job"]
        .as_str()
        .unwrap_or_else(|| panic!("member_job_request returned no job: {request}"))
        .to_string();
    let task = format!("{job}-t1");
    fx.op(
        "member_job_share",
        json!({"member_envelope": fx.member_envelope("sub-alice"), "job": job, "enabled": true}),
    );
    fx.op(
        "member_job_subscribe",
        json!({"member_envelope": fx.member_envelope("sub-ben"), "job": job}),
    );
    assert!(
        fx.shared.store.assist_job_is_shared(&job, ORG).unwrap(),
        "the fixture's owner share did not land"
    );
    assert!(
        fx.shared
            .store
            .job_is_subscriber(&job, ORG, "sub-ben")
            .unwrap(),
        "the fixture did not create the durable subscriber row"
    );
    fx.shared.store.assist_request_notice(&job).unwrap();
    fx.shared
        .route_assist_job_notifications()
        .unwrap_or_else(|e| panic!("the assist intent drain failed: {e}"));
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
                        .is_some()
            })
            .unwrap_or_else(|| panic!("no delivered assist notice for '{alias}'"))
    };
    let subscriber_notice = notice_for(&subscriber.alias);
    let owner_notice = notice_for(&owner.alias);
    assert_eq!(
        subscriber_notice.task_id.as_deref(),
        Some(task.as_str()),
        "the delivered notice does not carry the job's task"
    );
    Cad2207SharedNotice {
        job,
        owner_alias: owner.alias,
        subscriber_alias: subscriber.alias,
        subscriber_notice,
        owner_notice,
    }
}

/// CAD-2207 (negative acceptance): a share revocation that COMPLETES while the
/// subscriber's delivered notice is inside its FINAL live-authority re-prove
/// must never be followed by a provider submission of that notice, and the
/// notice must not be left silently pending either.
///
/// Today this is RED: the final re-prove reads the live share, parks in its own
/// authority pull, the revocation commits (the owner's `shared:false`), the pull
/// answers ALLOW, and `run_turn_slotted` runs — the notice completes after the
/// share was withdrawn.
#[test]
fn cad2207_completed_share_revocation_is_never_followed_by_a_notice_submission() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let notice = cad2207_shared_notice(&fx);
    cad2207_serve_on_fake_provider(&fx, &notice.subscriber_alias);
    // Serve each platform request off the accept loop, so the racing
    // `member_job_share` call's own live-authority pull is answered while the
    // notice's final re-prove is parked. Every other fixture keeps the
    // default single-threaded loop.
    fx.concurrent.store(true, Ordering::SeqCst);

    // Bring the subscriber's assistant up on the production actor loop. Its take
    // gate re-proves the notice and is this turn's FIRST live pull.
    {
        let mut guard = fx
            .authority
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.hold_next = true;
    }
    fx.shared
        .launch_actor(&notice.subscriber_alias)
        .unwrap_or_else(|e| panic!("the subscriber's assistant did not start: {e}"));
    cad2207_pull_parked(
        &fx,
        "the take-gate re-prove never reached the live authority pull",
    );

    // Release the take gate and park the FINAL re-prove
    // (`admit_app_submission` -> `member_message_reprove`). At this point the
    // owner-share read for this turn has already happened and no provider has
    // been called.
    cad2207_release_and_park_next(
        &fx,
        "the final re-prove never reached the live authority pull",
    );
    let claimed = fx
        .shared
        .store
        .message(&notice.subscriber_notice.id)
        .unwrap()
        .expect("the delivered notice vanished before its turn");
    assert_eq!(
        claimed.state, "submitting",
        "the notice was not claimed by the actor before the final re-prove: {claimed:?}"
    );
    assert!(
        claimed.turn_id.is_none(),
        "a provider turn was recorded before the final re-prove finished: {claimed:?}"
    );

    // The PUBLIC revocation: `member_job_share {enabled:false}` through the
    // proven-operator relay — the very operation the owner performs, including
    // its own live-authority pull and its `shared:false` answer. A serialization
    // added inside this handler (before or after its pull) is a legitimate fix
    // and must be able to make this test pass, so the store write is never
    // substituted for it. The platform's accept loop is free (the notice's final
    // re-prove is parked on its own thread), so this call's own pull is answered
    // independently while that one stays in flight.
    let revocation = cad2207_disable_share_bounded(&fx, &notice.job);
    // A transport outcome is NOT an explicit pending/refusal: if the racing RPC
    // never reached the platform, this fixture could not serve one concurrent
    // pull and the run would prove nothing. Fail loudly instead of passing.
    let reached_platform = revocation
        .as_ref()
        .err()
        .is_none_or(|error| !error.to_string().contains("unreachable"));
    assert!(
        reached_platform,
        "the racing member_job_share never reached the platform: this acceptance requires the \
         fixture to answer ONE concurrent revalidate request while the notice's final re-prove \
         is parked (CAD-2207)"
    );
    match &revocation {
        Ok(answer) => {
            assert_eq!(
                answer["shared"],
                json!(false),
                "member_job_share did not report the disable: {answer}"
            );
            assert!(
                !fx.shared
                    .store
                    .assist_job_is_shared(&notice.job, ORG)
                    .unwrap(),
                "member_job_share reported shared:false without withdrawing the share"
            );
            // Release the final re-prove. The platform still answers ALLOW (the
            // member is live), so only the completed share revocation can stop
            // the send.
            fx.release_barrier();
            let settled = cad2207_settle_notice(&fx, &notice.subscriber_notice.id);
            assert!(
                !cad2207_notice_submitted(&settled),
                "a COMPLETED member_job_share disable was followed by a provider submission of \
                 the previously authorized private shared notice (state '{}', turn {:?}): the \
                 final re-prove must be ordered against the owner's share mutation (CAD-2207)",
                settled.state,
                settled.turn_id
            );
            assert!(
                matches!(settled.state.as_str(), "failed" | "cancelled"),
                "the notice was neither submitted nor explicitly refused after the completed \
                 revocation: state '{}', row {settled:?} (CAD-2207)",
                settled.state
            );
            assert!(
                cad2207_refusal_text(&settled).is_some(),
                "the refused notice carries no recorded reason: {settled:?} (CAD-2207)"
            );
        }
        Err(error) => {
            // An explicit pending/refusal is an acceptable answer while a
            // submission is already in flight — and such a refusal must not
            // half-apply: the share stays VALID, so the in-flight submission is
            // legitimate and is not undone by anything here. The notice must
            // still settle, never be left silently pending.
            assert!(
                !error.to_string().trim().is_empty(),
                "the revocation refused without an explicit reason (CAD-2207)"
            );
            assert!(
                fx.shared
                    .store
                    .assist_job_is_shared(&notice.job, ORG)
                    .unwrap(),
                "a refused member_job_share withdrew the share anyway — a pending/refusal must \
                 not half-apply (CAD-2207)"
            );
            fx.release_barrier();
            let settled = cad2207_settle_notice(&fx, &notice.subscriber_notice.id);
            assert!(
                !matches!(settled.state.as_str(), "queued" | "submitting"),
                "the revocation refused explicitly, yet the notice is left silently pending \
                 (state '{}'): a pending answer must name the in-flight submission (CAD-2207)",
                settled.state
            );
        }
    }
    // The durable subscriber row survives either way, so a refusal cannot be
    // explained by a removed subscription.
    assert!(
        fx.shared
            .store
            .job_is_subscriber(&notice.job, ORG, "sub-ben")
            .unwrap(),
        "the durable subscriber row vanished — a refusal could then be the subscriber gate \
         instead of the withdrawn share"
    );

    // Revocation withdraws the SUBSCRIBER's ride only: the owner still owns the
    // job and the owner's own notice still re-proves. This is the authorized
    // positive inside the same fixture — a blanket refusal cannot satisfy it.
    assert!(
        fx.shared
            .store
            .assist_request_owns(ORG, "sub-alice", &notice.job)
            .unwrap(),
        "the owner lost ownership of their own job"
    );
    fx.shared
        .member_message_reprove(&notice.owner_notice)
        .unwrap_or_else(|e| {
            panic!(
                "the owner's own notice was refused after the subscriber's share was revoked: {e}"
            )
        });

    cad2207_quiet(&fx, &notice.subscriber_alias);
}

/// CAD-2207 control: the SAME two-park race with NO revocation. The notice must
/// reach the provider — the barrier dance and the in-process provider are not
/// refusing by themselves, so the negative test above can only be red because of
/// the share mutation. It then drives the RPC disable path with nothing in
/// flight and shows it reports `shared:false` from the very write the race
/// observed.
#[test]
fn cad2207_unrevoked_inflight_notice_reprove_still_reaches_the_provider() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let notice = cad2207_shared_notice(&fx);
    cad2207_serve_on_fake_provider(&fx, &notice.subscriber_alias);

    {
        let mut guard = fx
            .authority
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.hold_next = true;
    }
    fx.shared
        .launch_actor(&notice.subscriber_alias)
        .unwrap_or_else(|e| panic!("the subscriber's assistant did not start: {e}"));
    cad2207_pull_parked(
        &fx,
        "control: the take-gate re-prove never reached the live authority pull",
    );
    cad2207_release_and_park_next(
        &fx,
        "control: the final re-prove never reached the live authority pull",
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&notice.job, ORG)
            .unwrap(),
        "control: the owner's share was not live at the final re-prove"
    );
    fx.release_barrier();

    let settled = cad2207_settle_notice(&fx, &notice.subscriber_notice.id);
    assert_eq!(
        settled.state, "completed",
        "the unrevoked notice never reached the provider (control broken): {settled:?}"
    );
    assert!(
        settled.turn_id.is_some(),
        "the unrevoked notice recorded no provider turn: {settled:?}"
    );

    // The RPC's disable path, with nothing in flight: it answers `shared:false`
    // and its one effect is the store write the race above observed.
    let disabled = fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope("sub-alice"),
            "job": notice.job,
            "enabled": false,
        }),
    );
    assert_eq!(
        disabled["shared"],
        json!(false),
        "member_job_share did not report the disable: {disabled}"
    );
    assert!(
        !fx.shared
            .store
            .assist_job_is_shared(&notice.job, ORG)
            .unwrap(),
        "member_job_share reported shared:false without clearing the share row"
    );

    cad2207_quiet(&fx, &notice.subscriber_alias);
}

/// A second assist-bound job for the SAME owner in the SAME org, shared. The
/// fixture supports it: `member_job_request` keys the canonical job id on
/// `(org, sub, request_key)` and binds its own `assist_requests` ledger row, so
/// a distinct request key is a genuinely distinct owned job, and the platform's
/// revalidate allow decision pins `job_id` as a resource without consulting
/// ownership (that ACL is the daemon's). No subscription and no notice are
/// created for this job — only its share row is needed.
fn cad2207_second_shared_job(fx: &Fx) -> String {
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope("sub-alice"),
            "title": "cad2207 unrelated revocation ordering job",
            "request_key": "cad2207-revocation-ordering-job-b",
            "body": "cad2207 unrelated revocation ordering spec",
        }),
    );
    let job = request["job"]
        .as_str()
        .unwrap_or_else(|| panic!("the second member_job_request returned no job: {request}"))
        .to_string();
    assert!(
        fx.shared
            .store
            .assist_request_owns(ORG, "sub-alice", &job)
            .unwrap(),
        "the second job is not owned by the member"
    );
    fx.op(
        "member_job_share",
        json!({"member_envelope": fx.member_envelope("sub-alice"), "job": job, "enabled": true}),
    );
    assert!(
        fx.shared.store.assist_job_is_shared(&job, ORG).unwrap(),
        "the fixture's second owner share did not land"
    );
    job
}

/// CAD-2207 behavioural acceptance: an unshare of a DIFFERENT job must neither be
/// blocked by, nor disturb, an in-flight notice for another job of the same
/// owner.
///
/// While the subscriber's notice for job A is parked in its FINAL
/// live-authority re-prove, the owner disables the share of a distinct job B
/// (same org) through the real `member_job_share` RPC. That revocation must
/// succeed on its own terms: B's share is actually withdrawn, A's share stays
/// valid, and A's already-authorized in-flight notice still reaches the provider
/// and settles. An over-broad serialization (one that blocks every share change
/// behind any in-flight submission) or an over-broad refusal fails here, without
/// this test inspecting any lock or implementation detail.
#[test]
fn cad2207_unrelated_share_disable_completes_while_another_notice_is_in_flight() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let notice = cad2207_shared_notice(&fx);
    let job_b = cad2207_second_shared_job(&fx);
    assert_ne!(
        job_b, notice.job,
        "the fixture produced the same canonical job twice"
    );
    cad2207_serve_on_fake_provider(&fx, &notice.subscriber_alias);
    // The unrelated disable performs its own live-authority pull while job A's
    // final re-prove is parked, so this fixture must answer one request off the
    // accept loop (as in the race test above).
    fx.concurrent.store(true, Ordering::SeqCst);
    {
        let mut guard = fx
            .authority
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.hold_next = true;
    }
    fx.shared
        .launch_actor(&notice.subscriber_alias)
        .unwrap_or_else(|e| panic!("the subscriber's assistant did not start: {e}"));
    cad2207_pull_parked(
        &fx,
        "the take-gate re-prove never reached the live authority pull",
    );
    cad2207_release_and_park_next(
        &fx,
        "the final re-prove never reached the live authority pull",
    );

    // Job A's notice is parked at its final re-prove. Revoke the OTHER job.
    let revoked_b = fx
        .op_err(
            "member_job_share",
            json!({
                "member_envelope": fx.member_envelope("sub-alice"),
                "job": job_b,
                "enabled": false,
            }),
        )
        .unwrap_or_else(|e| {
            panic!("unsharing an unrelated job while job A's notice was in flight was refused: {e}")
        });
    assert_eq!(
        revoked_b["shared"],
        json!(false),
        "member_job_share did not report the unrelated disable: {revoked_b}"
    );
    assert!(
        !fx.shared.store.assist_job_is_shared(&job_b, ORG).unwrap(),
        "the unrelated job's share was not actually withdrawn"
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&notice.job, ORG)
            .unwrap(),
        "unsharing job B withdrew job A's share"
    );
    assert!(
        fx.shared
            .store
            .job_is_subscriber(&notice.job, ORG, "sub-ben")
            .unwrap(),
        "unsharing job B removed job A's durable subscriber row"
    );

    // Job A was authorized throughout: release its final re-prove and let its
    // notice run to a settled, submitted turn.
    fx.release_barrier();
    let settled = cad2207_settle_notice(&fx, &notice.subscriber_notice.id);
    assert_eq!(
        settled.state, "completed",
        "job A's notice did not settle after an unrelated job B was unshared: {settled:?}"
    );
    assert!(
        settled.turn_id.is_some(),
        "job A's notice recorded no provider turn: {settled:?}"
    );
    assert!(
        fx.shared
            .store
            .assist_request_owns(ORG, "sub-alice", &notice.job)
            .unwrap(),
        "the owner lost ownership of job A"
    );

    cad2207_quiet(&fx, &notice.subscriber_alias);
}

/// CAD-2207 (behavioural): a subscriber notification intent that was already
/// PENDING when the owner withdrew the share must be refused by the REAL
/// watch-loop router, and that refusal must not depend on reading the task or the
/// owner's request row first; nothing may be enqueued for the subscriber, while
/// the owner's own intent from the same transition still routes.
///
/// The probe is INPUT, not code: ONE subscriber intent is pointed at a task row
/// that does not exist, which makes any task lookup on that path visible through
/// that intent's terminal status — `unresolved` means the router read the task
/// before deciding, `refused` means the share decision came first (and the
/// request-row read, which follows the task read on that path, never ran).
///
/// RED under read-before-share ordering: the probed intent settles `unresolved`
/// instead of `refused`.
#[test]
fn cad2207_withdrawn_share_pending_subscriber_intent_is_refused_before_any_task_read() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let notice = cad2207_shared_notice(&fx);
    let task = format!("{}-t1", notice.job);
    let subscriber_notices_before = cad2207_notices(&fx, &notice.subscriber_alias).len();

    // Two REAL transitions, created while the share is still live; their
    // intents stay pending because nothing drains them until the router below.
    fx.shared
        .store
        .job_notice(
            &task,
            "blocked",
            "cad2207-pending-subscriber",
            "cad2207 pending notice",
        )
        .unwrap();
    fx.shared
        .store
        .job_notice(
            &task,
            "blocked",
            "cad2207-pending-subscriber-probe",
            "cad2207 pending notice probe",
        )
        .unwrap();
    assert_eq!(
        cad2207_intent_status(
            &fx,
            &notice.job,
            "cad2207-pending-subscriber",
            "subscriber",
            Some("sub-ben")
        ),
        "pending",
        "the fixture's transition did not leave a pending subscriber intent"
    );

    // The owner withdraws through the real RPC (nothing is in flight here).
    let revoked = fx
        .op_err(
            "member_job_share",
            json!({
                "member_envelope": fx.member_envelope("sub-alice"),
                "job": notice.job,
                "enabled": false,
            }),
        )
        .unwrap_or_else(|e| panic!("the owner's withdrawal was refused: {e}"));
    assert_eq!(
        revoked["shared"],
        json!(false),
        "member_job_share did not report the withdrawal: {revoked}"
    );
    assert!(
        !fx.shared
            .store
            .assist_job_is_shared(&notice.job, ORG)
            .unwrap(),
        "the withdrawal did not land"
    );

    // Input probe: ben's second-transition intent now names an absent task.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE events SET payload=json_set(payload,'$.task',?) \
                 WHERE kind='assist_intent' AND json_extract(payload,'$.job')=? \
                   AND json_extract(payload,'$.target')='subscriber' \
                   AND json_extract(payload,'$.sub')='sub-ben' \
                   AND json_extract(payload,'$.dedupe')=?",
                rusqlite::params![
                    "cad2207-absent-task",
                    notice.job,
                    "cad2207-pending-subscriber-probe"
                ],
            )?;
            Ok(())
        })
        .unwrap();

    fx.shared
        .route_assist_job_notifications()
        .unwrap_or_else(|e| panic!("the intent drain failed: {e}"));

    assert_eq!(
        cad2207_intent_status(
            &fx,
            &notice.job,
            "cad2207-pending-subscriber",
            "subscriber",
            Some("sub-ben")
        ),
        "refused",
        "a pending subscriber intent survived a completed withdrawal without being refused"
    );
    assert_eq!(
        cad2207_intent_status(
            &fx,
            &notice.job,
            "cad2207-pending-subscriber-probe",
            "subscriber",
            Some("sub-ben")
        ),
        "refused",
        "the router looked up the task before consulting the owner's share: the withdrawn \
         subscriber intent settled 'unresolved' (a task-lookup failure) instead of being refused \
         before any private read (CAD-2207)"
    );
    assert_eq!(
        cad2207_intent_status(
            &fx,
            &notice.job,
            "cad2207-pending-subscriber",
            "subscriber",
            Some("sub-alice")
        ),
        "delivered",
        "the owner lost the notice for their own job when the subscriber's intent was refused"
    );
    assert_eq!(
        cad2207_intent_status(
            &fx,
            &notice.job,
            "cad2207-pending-subscriber-probe",
            "subscriber",
            Some("sub-alice")
        ),
        "delivered",
        "the owner's intent was lost when the subscriber's probed intent was refused"
    );

    // Nothing reached the withdrawn subscriber.
    let subscriber_notices = cad2207_notices(&fx, &notice.subscriber_alias);
    assert_eq!(
        subscriber_notices.len(),
        subscriber_notices_before,
        "a withdrawn subscriber was enqueued a notice anyway"
    );
    assert!(
        subscriber_notices
            .iter()
            .all(|message| !message.body.contains("is now blocked")),
        "a withdrawn subscriber received notice content: {subscriber_notices:?}"
    );
    // The owner received both routed notices, each with its delivery receipt.
    let owner_notices: Vec<_> = cad2207_notices(&fx, &notice.owner_alias)
        .into_iter()
        .filter(|message| message.body.contains("is now blocked"))
        .collect();
    assert_eq!(
        owner_notices.len(),
        2,
        "the owner did not receive both routed notices: {owner_notices:?}"
    );
    for message in &owner_notices {
        assert!(
            fx.shared
                .store
                .assist_notice_delivery(&message.id)
                .unwrap()
                .is_some(),
            "the owner's routed notice carries no delivery receipt: {message:?}"
        );
    }
}

/// CAD-2207 control for the pending-intent case: with the share LIVE the real
/// router delivers the same intent to BOTH subscribers (the owner's own ledger
/// row and the second member), and the subscriber's notice carries the delivery
/// receipt its turn is later re-proved against.
#[test]
fn cad2207_live_share_pending_subscriber_intent_still_routes() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let notice = cad2207_shared_notice(&fx);
    let task = format!("{}-t1", notice.job);
    fx.shared
        .store
        .job_notice(
            &task,
            "blocked",
            "cad2207-live-subscriber",
            "cad2207 live notice",
        )
        .unwrap();
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&notice.job, ORG)
            .unwrap(),
        "the control lost the owner's share"
    );

    fx.shared
        .route_assist_job_notifications()
        .unwrap_or_else(|e| panic!("the intent drain failed: {e}"));

    for sub in ["sub-ben", "sub-alice"] {
        assert_eq!(
            cad2207_intent_status(
                &fx,
                &notice.job,
                "cad2207-live-subscriber",
                "subscriber",
                Some(sub)
            ),
            "delivered",
            "an authorized subscriber intent was not delivered for {sub}"
        );
    }
    let delivered = cad2207_notices(&fx, &notice.subscriber_alias)
        .into_iter()
        .find(|message| message.body.contains("is now blocked"))
        .unwrap_or_else(|| panic!("the subscriber received no notice for the live share"));
    assert!(
        fx.shared
            .store
            .assist_notice_delivery(&delivered.id)
            .unwrap()
            .is_some(),
        "the routed subscriber notice carries no delivery receipt: {delivered:?}"
    );
}

/// CAD-2207 (behavioural race): the owner's real unshare issued while the
/// subscriber's notice is parked in its FIRST queued re-proof — the take gate,
/// before any claim — must not be followed by that notice being claimed or
/// submitted. A completed withdrawal stops the notice before the claim, so the
/// actor's later prompt/context/provider work never starts; an explicit
/// same-job refusal must instead leave the share intact, in which case the
/// notice legitimately proceeds.
///
/// RED today: the take gate's re-proof read the live share before its authority
/// pull, so the completed withdrawal is followed by a CLAIM (and the actor's
/// prompt/context work) with the share already gone.
#[test]
fn cad2207_withdrawal_at_the_take_gate_stops_the_notice_before_claim() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let notice = cad2207_shared_notice(&fx);
    cad2207_serve_on_fake_provider(&fx, &notice.subscriber_alias);
    fx.concurrent.store(true, Ordering::SeqCst);
    {
        let mut guard = fx
            .authority
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.hold_next = true;
    }
    fx.shared
        .launch_actor(&notice.subscriber_alias)
        .unwrap_or_else(|e| panic!("the subscriber's assistant did not start: {e}"));
    cad2207_pull_parked(
        &fx,
        "the take-gate re-prove never reached the live authority pull",
    );
    let queued = fx
        .shared
        .store
        .message(&notice.subscriber_notice.id)
        .unwrap()
        .expect("the delivered notice vanished");
    assert_eq!(
        queued.state, "queued",
        "the notice was claimed before its take-gate re-prove finished: {queued:?}"
    );

    let revocation = cad2207_disable_share_bounded(&fx, &notice.job);
    match &revocation {
        Ok(answer) => {
            assert_eq!(
                answer["shared"],
                json!(false),
                "member_job_share did not report the withdrawal: {answer}"
            );
            assert!(
                !fx.shared
                    .store
                    .assist_job_is_shared(&notice.job, ORG)
                    .unwrap(),
                "the withdrawal was reported but the share row survived"
            );
            fx.release_barrier();
            let settled = cad2207_settle_notice(&fx, &notice.subscriber_notice.id);
            assert!(
                !cad2207_claimed(&fx, &notice.subscriber_alias, &notice.subscriber_notice.id),
                "the notice was CLAIMED after a completed withdrawal: the actor went on to build \
                 prompt/context state for a subscriber whose share its own in-flight re-prove had \
                 already outlived (CAD-2207)"
            );
            assert!(
                !cad2207_notice_submitted(&settled),
                "a provider submission followed a completed withdrawal: {settled:?}"
            );
            assert!(
                matches!(settled.state.as_str(), "failed" | "cancelled"),
                "the notice was neither submitted nor explicitly refused after the completed \
                 withdrawal: state '{}', row {settled:?} (CAD-2207)",
                settled.state
            );
            assert!(
                cad2207_refusal_text(&settled).is_some(),
                "the refused notice carries no recorded reason: {settled:?} (CAD-2207)"
            );
        }
        Err(error) => {
            assert!(
                !error.to_string().trim().is_empty(),
                "the withdrawal refused without an explicit reason (CAD-2207)"
            );
            assert!(
                fx.shared
                    .store
                    .assist_job_is_shared(&notice.job, ORG)
                    .unwrap(),
                "an explicit refusal withdrew the share anyway — the refusal must not half-apply"
            );
            fx.release_barrier();
            let settled = cad2207_settle_notice(&fx, &notice.subscriber_notice.id);
            assert!(
                cad2207_claimed(&fx, &notice.subscriber_alias, &notice.subscriber_notice.id),
                "the share refused the withdrawal yet the notice was never claimed: {settled:?}"
            );
            assert_eq!(
                settled.state, "completed",
                "the refusal left the share intact but the notice did not proceed: {settled:?}"
            );
        }
    }
    cad2207_quiet(&fx, &notice.subscriber_alias);
}

/// CAD-2207 control: the same take-gate park with NO withdrawal still claims the
/// notice and completes it, so the stop above cannot be a blanket refusal.
#[test]
fn cad2207_unrevoked_take_gate_still_claims_and_completes() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let notice = cad2207_shared_notice(&fx);
    cad2207_serve_on_fake_provider(&fx, &notice.subscriber_alias);
    fx.concurrent.store(true, Ordering::SeqCst);
    {
        let mut guard = fx
            .authority
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.hold_next = true;
    }
    fx.shared
        .launch_actor(&notice.subscriber_alias)
        .unwrap_or_else(|e| panic!("the subscriber's assistant did not start: {e}"));
    cad2207_pull_parked(
        &fx,
        "control: the take-gate re-prove never reached the live authority pull",
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&notice.job, ORG)
            .unwrap(),
        "control: the owner's share was not live"
    );
    fx.release_barrier();
    let settled = cad2207_settle_notice(&fx, &notice.subscriber_notice.id);
    assert!(
        cad2207_claimed(&fx, &notice.subscriber_alias, &notice.subscriber_notice.id),
        "control: the unrevoked notice was never claimed: {settled:?}"
    );
    assert_eq!(
        settled.state, "completed",
        "control: the unrevoked notice did not complete: {settled:?}"
    );
    assert!(
        settled.turn_id.is_some(),
        "control: no provider turn was recorded: {settled:?}"
    );
    cad2207_quiet(&fx, &notice.subscriber_alias);
}
