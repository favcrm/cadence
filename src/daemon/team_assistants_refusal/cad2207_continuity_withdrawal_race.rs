//! CAD-2207 — a LOCAL share withdrawal must not be followed by a provider
//! submission whose continuity pack still quotes the withdrawn private notice.
//!
//! Independently authored negative acceptance, deliberately separate from the
//! implementation and from every other `team_assistants_refusal` check. This
//! module is a CHILD of that module only to reuse its already-real fixture
//! (`Fx`: a real in-process daemon, a real `operator/board-identity.json` trust
//! root, one loopback authoritative platform answering
//! `/__platform/assistant/revalidate`, and the real `Store`). Every input is
//! produced by the REAL code path: the owner's `member_job_request`, the real
//! `member_job_share` / `member_job_subscribe` RPCs on signed member envelopes,
//! the real `assist_request_notice` + `route_assist_job_notifications` intent
//! drain, the real `member_assist_send` member turn, and the real
//! `member_job_share {enabled:false}` withdrawal. The notice body is the
//! router's — this file never writes one.
//!
//! THE WINDOW UNDER TEST. This is a DIFFERENT window from the sibling
//! `cad2207_share_revocation_ordering` acceptance: there the turn IS the
//! `assist_job_event` notice (its own admission is reserved by the take and the
//! final re-prove). Here the turn is an ORDINARY member message. Its continuity
//! pack — assembled in `Shared::continuity_parts`, i.e. from
//! `crate::continuity::assemble` — quotes a notice that was delivered and run
//! while the owner's share was live, and the turn itself holds NO admission for
//! that notice's job. `daemon/actor.rs` then calls `admit_app_submission`
//! immediately before `adapter.run_turn_slotted`; for an ordinary member turn
//! that gate is `member_message_reprove` → `member_authority_live`, i.e. the
//! FINAL member-authority revalidation. The prompt (pack included) is already
//! built at that point. A withdrawal that COMPLETES inside this window must not
//! be followed by a submission of the withdrawn notice; if instead the
//! withdrawal cannot complete, its answer must be an explicit pending/refusal,
//! never a plain success. That is the ticket's contract, verbatim:
//!   * `member_job_share {enabled:false}` is refused while a notice-bearing
//!     submission is in flight (and then the share stays live, so the in-flight
//!     submission is legitimate), OR
//!   * the withdrawal succeeds before the resumed provider submission, and the
//!     provider never receives the withdrawn notice.
//!
//! TIMING (channel, never a sleep). The fixture's HTTP-side barrier
//! (`Authority::hold_next` / `held` / `released`) holds ONE `revalidate` pull
//! until the test releases it. An ordinary member turn pulls live authority
//! twice: once in the take gate (`take_app_aware`) and once in the final
//! re-prove (`admit_app_submission`). The test parks the first pull, then
//! re-arms the barrier before releasing it (the same re-arm the sibling race
//! acceptance uses), so the SECOND parked pull is provably the final re-prove —
//! pull #1 cannot even be answered until the release, and no other `revalidate`
//! request can consume the re-armed hold while pull #1 is parked. At the second
//! park the test also asserts the store row is `submitting` with no `turn_id`
//! (no provider turn started) and that the continuity `PACK_EVENT` for this
//! exact message was already recorded (the pack was assembled). Only then does
//! it issue the owner's real withdrawal.
//!
//! The provider is the shipped in-process `ProviderAdapter` (`endpoint_kind =
//! "fake"`), so the real actor loop and the real `run_turn_slotted` call run
//! unchanged, and the witness is that provider's own `cadence/fake_pack` event
//! — the ACTUAL pack prefix it received, not a mirrored rendering, a source
//! string or a lock counter.
//!
//! CONTROL IN THE SAME FIXTURE. While the same pull is parked, the owner also
//! withdraws an UNRELATED shared job that no notice for this thread exists for;
//! that withdrawal must succeed. This proves the boundary is the notice-bearing
//! submission, not a blanket "any unshare while a turn runs is refused". A
//! control before the race builds the production pack for the same turn and
//! asserts it really carries the notice and the member's own conversation, so
//! "absent after" is a change, not a builder that never carried it. When the
//! withdrawal succeeds and the provider still receives a pack, that pack must
//! also keep the member's own ordinary conversation: the ticket authorizes
//! withholding the withdrawn notice, not blanking the member's own history (the
//! sibling continuity acceptance requires the same preservation after a
//! withdrawal). A fix that refuses the whole turn before submission carries no
//! pack and is accepted on that branch.
//!
//! SCOPE, stated narrowly. This is the OWNER's LOCAL withdrawal only. Remote
//! membership/session/credential/grant revocation lives in the platform service
//! behind `member_authority_live`, is re-derived per effect there, and is NOT
//! re-tested here. Content already delivered, a prompt already sent, and any
//! copy a peer already made are irrevocable and are NOT claimed to be erased.
//! No refusal wording is asserted: a refusal is accepted on its OBSERVABLE
//! effect (the share stays live and the submission is genuinely notice-bearing);
//! the control refusal path only rejects a transport failure ("unreachable") so
//! a fixture that could not answer the racing pull fails loudly instead of
//! passing. CAD-2061 session claims, CAD-2148 custody, derived output taint and
//! genuine cross-harness acceptance are explicitly NOT claimed.
//!
//! EXPECTED BASELINE FAILURE (at `1f8026f15`). Admissions are reserved only for
//! `assist_job_event` turns (`admit_assist_notice` / `pre_admit_assist_notice`
//! return `None` for an ordinary member message), so `rpc_member_job_share`
//! sees no pending admission for the notice's job and returns `shared:false`
//! while the final re-prove is parked. After the release the platform answers
//! ALLOW and `run_turn_slotted` receives the already-built prompt whose pack
//! still quotes the withdrawn notice — the `cadence/fake_pack` event names the
//! notice body, job, task and message id. The test fails on
//! `cad2207_race_assert_withheld`.
//!
//! RUN (the module must first be registered with
//! `mod cad2207_continuity_withdrawal_race;` beside the other
//! `cad2207_*` children of `team_assistants_refusal.rs`):
//! `scripts/boat-build -- cargo test --locked --features test-seam --lib
//! cad2207_continuity_withdrawal_race -- --test-threads 1`

use super::*;

/// The member who owns the job and grants/withdraws its share.
const CAD2207_RACE_OWNER: &str = "sub-alice";
/// The second member whose ordinary assistant turn carries the continuity pack.
const CAD2207_RACE_SUBSCRIBER: &str = "sub-ben";
/// The owner's canonical request key — the job id is derived from it, so the
/// delivered notice names an identifier that exists nowhere else.
const CAD2207_RACE_JOB_KEY: &str = "cad2207-continuity-withdrawal-race-job";
const CAD2207_RACE_JOB_TITLE: &str = "cad2207 continuity withdrawal race private job";
/// A second, UNRELATED canonical job of the same owner. It is created after the
/// notice drain and its intent is never drained, so no notice for it exists in
/// the subscriber's thread — the boundary control.
const CAD2207_RACE_CONTROL_KEY: &str = "cad2207-continuity-withdrawal-race-control-job";
const CAD2207_RACE_CONTROL_TITLE: &str = "cad2207 continuity withdrawal race unrelated job";
/// The assistant-origin marker a delivered job notice carries.
const CAD2207_RACE_NOTICE: &str = "assist_job_event";
/// The subscriber's own ordinary message — the control content the pack must
/// keep carrying.
const CAD2207_RACE_OWN_NOTE: &str =
    "cad2207 continuity withdrawal race: the subscriber's own ordinary note";
/// The ordinary member turn that races the withdrawal. It is NOT a job notice:
/// its pack quotes the previously delivered notice, and it holds no admission
/// for that job.
const CAD2207_RACE_TURN: &str =
    "cad2207 continuity withdrawal race: the ordinary turn that races the withdrawal";
/// Stable client-side message ids (lowercase letters, digits, hyphens).
const CAD2207_RACE_OWN_MESSAGE: &str = "cad2207-race-own-note";
const CAD2207_RACE_CURRENT: &str = "cad2207-race-current-turn";

/// One fully realized scenario: the owner's job with a LIVE share, the durable
/// subscriber, one notice delivered and RUN while the share was live, the
/// subscriber's own completed turn, and an unrelated shared job with no notice.
struct Cad2207RaceScenario {
    job: String,
    task: String,
    notice: crate::store::Message,
    subscriber_alias: String,
    subscriber_thread: String,
    control_job: String,
}

/// The delivered `assist_job_event` notice for ONE job on ONE alias — found by
/// the durable `assist_notice_deliveries` receipt, never by body text.
fn cad2207_race_notice(fx: &Fx, alias: &str, job: &str) -> crate::store::Message {
    fx.shared
        .store
        .messages(alias)
        .unwrap_or_else(|error| panic!("could not read '{alias}' messages: {error}"))
        .into_iter()
        .find(|message| {
            message.source == CAD2207_RACE_NOTICE
                && fx
                    .shared
                    .store
                    .assist_notice_delivery(&message.id)
                    .unwrap()
                    .is_some_and(|delivery| delivery.job == job)
        })
        .unwrap_or_else(|| panic!("no delivered assist notice for '{job}' on '{alias}'"))
}

/// Serve an already-bound member assistant on the shipped in-process provider
/// (`endpoint_kind = "fake"`). Binding, cwd, conversation and every member
/// guard are unchanged — only the launch surface differs, and that is the same
/// `ProviderAdapter` contract a managed runtime follows.
fn cad2207_race_serve_on_fake(fx: &Fx, alias: &str) {
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
            panic!("the fixture could not serve '{alias}' on the fake provider: {e}")
        });
    fx.shared
        .store
        .set_enabled(alias, true)
        .unwrap_or_else(|e| panic!("the fixture could not enable '{alias}': {e}"));
}

/// Stop the fixture actor and wait for ownership to clear, so a restart (or a
/// torn-down state dir) never races a live thread.
fn cad2207_race_quiet(fx: &Fx, alias: &str) {
    let _ = fx.shared.rpc_stop(&json!({"alias": alias}));
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while fx
        .shared
        .lifecycle
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .owned(alias)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the fixture actor for '{alias}' did not stop"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait, bounded, until one message reaches a terminal state.
fn cad2207_race_settle(fx: &Fx, id: &str) -> crate::store::Message {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let message = fx
            .shared
            .store
            .message(id)
            .unwrap_or_else(|e| panic!("message '{id}' could not be read: {e}"))
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

/// The subscriber's own ordinary conversation, through the REAL member send.
/// `message` is supplied by the test, so the entry/message id is known without
/// parsing the receipt.
fn cad2207_race_send(fx: &Fx, text: &str, message: &str) {
    let receipt = fx.op(
        "member_assist_send",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_RACE_SUBSCRIBER),
            "text": text,
            "message": message,
        }),
    );
    let stored = fx
        .shared
        .store
        .message(message)
        .unwrap_or_else(|error| panic!("the member send '{message}' could not be read: {error}"))
        .unwrap_or_else(|| panic!("the member send '{message}' was not queued: {receipt}"));
    assert_eq!(
        stored.body, text,
        "the queued member send carries another body: {stored:?}"
    );
}

/// The provider's actual observed continuity pack, from `cadence/fake_pack`
/// events — the prefix a real `run_turn` received.
fn cad2207_race_packs(fx: &Fx, alias: &str) -> Vec<String> {
    fx.shared
        .store
        .events(alias, 0, 5000)
        .unwrap_or_else(|e| panic!("could not read '{alias}' events: {e}"))
        .into_iter()
        .filter(|event| event.kind == "fake_pack")
        .filter_map(|event| {
            event
                .payload
                .get("pack")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// Was a continuity `PACK_EVENT` recorded for THIS message? `record_continuity`
/// runs in the actor after `continuity_parts` assembled the pack and before
/// `admit_app_submission` performs the final re-prove — so this is the durable
/// witness that the pack was already assembled when the second pull parked.
fn cad2207_race_pack_recorded(fx: &Fx, alias: &str, message: &str) -> bool {
    fx.shared
        .store
        .events(alias, 0, 5000)
        .unwrap_or_else(|e| panic!("could not read '{alias}' events: {e}"))
        .into_iter()
        .any(|event| {
            event.kind == crate::continuity::PACK_EVENT
                && event.payload.get("message").and_then(Value::as_str) == Some(message)
        })
}

/// The production continuity builder for one bound thread, with `current` the
/// message the pack travels with — the same argument
/// `Shared::continuity_parts` passes, so the current turn is never quoted into
/// its own pack.
fn cad2207_race_live_pack(fx: &Fx, alias: &str, thread_id: &str, current: &str) -> String {
    let thread = fx
        .shared
        .store
        .thread_by_id(thread_id)
        .unwrap_or_else(|e| panic!("thread '{thread_id}' could not be read: {e}"))
        .unwrap_or_else(|| panic!("the bound thread '{thread_id}' vanished"));
    let pm = fx.pm();
    let pack = crate::continuity::assemble(
        &fx.shared.store,
        Some(pm.as_path()),
        alias,
        crate::continuity::Reason::New,
        current,
        Some(&thread),
    )
    .unwrap_or_else(|e| panic!("the production continuity builder failed for '{alias}': {e}"))
    .unwrap_or_else(|| {
        panic!(
            "the production continuity builder carried nothing for '{alias}' — the fixture \
             never reached the prompt path it is meant to test"
        )
    });
    pack.text
}

/// Wait until the fixture platform has PARKED a `revalidate` pull.
fn cad2207_race_pull_parked(fx: &Fx, what: &str) {
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
            // Release the lock here: a failed wait must report ONE failure
            // instead of panicking while holding (and poisoning) the fixture.
            drop(next);
        }
        assert!(std::time::Instant::now() < deadline, "{what}");
    }
}

/// Release the parked pull and park the NEXT one. The released pull answers
/// from the fixture's CURRENT state (ALLOW while the member is live); the caller
/// proceeds and its next pull parks instead of being answered. `held` is cleared
/// first so the second park is observable on its own. The re-armed hold cannot
/// be consumed by another request: it is written BEFORE the parked pull is
/// released, so no request can arrive in between, and the test issues no RPC
/// until the second park is observed — the only revalidate request that can
/// follow the release is the actor's own final re-prove.
fn cad2207_race_release_and_park_next(fx: &Fx, what: &str) {
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
    // poison the fixture.
    assert!(was_parked, "{what}: no pull was parked");
    cad2207_race_pull_parked(fx, what);
}

/// Issue ONE real `member_job_share {enabled:false}` and wait, bounded, for its
/// own answer. The call runs on its own thread so it really is concurrent with
/// the parked submission and its own live-authority pull; the bound is a
/// liveness guard only, and on expiry the parked pull is released before
/// failing so the fixture never hangs.
fn cad2207_race_unshare_bounded(fx: &Fx, job: &str) -> Result<Value> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let shared = fx.shared.clone();
    let envelope = fx.member_envelope(CAD2207_RACE_OWNER);
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
                "the owner's withdrawal neither completed nor answered an explicit refusal \
                 while an ordinary notice-bearing submission was parked in its final \
                 member-authority revalidation"
            );
        }
    }
}

/// The recorded refusal reason of a settled turn, if any.
fn cad2207_race_refusal_text(message: &crate::store::Message) -> Option<&str> {
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

/// No trace of the withdrawn notice in a provider prompt / response: the
/// router's body, the job and task it names, and the notice's own message id
/// (the fake provider's reply echoes the delivered prompt, and the pack quotes
/// the same body, so the whole observed text is checked).
fn cad2207_race_assert_withheld(what: &str, text: &str, scenario: &Cad2207RaceScenario) {
    for (label, secret) in [
        ("job id", scenario.job.as_str()),
        ("task id", scenario.task.as_str()),
        ("notice body", scenario.notice.body.as_str()),
        ("notice message id", scenario.notice.id.as_str()),
    ] {
        assert!(!secret.is_empty(), "{what}: the {label} is empty");
        assert!(
            !text.contains(secret),
            "{what} still discloses the {label} '{secret}': {text}"
        );
    }
}

/// The withdrawn notice IS served while the share is live — the control that
/// the surface really carries it, so "absent after" is a change, not a gap.
fn cad2207_race_assert_served(what: &str, text: &str, scenario: &Cad2207RaceScenario) {
    for (label, secret) in [
        ("job id", scenario.job.as_str()),
        ("task id", scenario.task.as_str()),
        ("notice body", scenario.notice.body.as_str()),
    ] {
        assert!(
            text.contains(secret),
            "{what} did not carry the {label} '{secret}': {text}"
        );
    }
}

/// One full real scenario, nothing synthesized: two provisioned members, the
/// owner's canonical job, the owner's REAL share, the subscriber's REAL
/// subscription, ONE notice delivered by the real intent drain AND RUN to a
/// real provider reply while the share is live, the subscriber's own completed
/// turn, and an UNRELATED shared job (created after the drain, never drained)
/// that carries no notice.
fn cad2207_race_fixture(fx: &Fx) -> Cad2207RaceScenario {
    fx.member_runtime(CAD2207_RACE_OWNER);
    let subscriber = fx.member_runtime(CAD2207_RACE_SUBSCRIBER);

    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_RACE_OWNER),
            "title": CAD2207_RACE_JOB_TITLE,
            "request_key": CAD2207_RACE_JOB_KEY,
            "body": "cad2207 continuity withdrawal race: the owner's private job spec",
            "acceptance": "a withdrawn share never reaches a resumed provider submission",
        }),
    );
    let job = request
        .get("job")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("member_job_request returned no job: {request}"))
        .to_string();
    let task = format!("{job}-t1");
    fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_RACE_OWNER),
            "job": job,
            "enabled": true,
        }),
    );
    fx.op(
        "member_job_subscribe",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_RACE_SUBSCRIBER),
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
            .job_is_subscriber(&job, ORG, CAD2207_RACE_SUBSCRIBER)
            .unwrap(),
        "the fixture did not create the durable subscriber row"
    );

    // The REAL intent drain writes the notices (and their delivery receipts).
    fx.shared
        .store
        .assist_request_notice(&job)
        .unwrap_or_else(|error| panic!("the assist notice intent did not commit: {error}"));
    fx.shared
        .route_assist_job_notifications()
        .unwrap_or_else(|error| panic!("the assist intent drain failed: {error}"));

    let notice = cad2207_race_notice(fx, &subscriber.alias, &job);
    assert_eq!(
        notice.task_id.as_deref(),
        Some(task.as_str()),
        "the delivered notice does not carry the job's own task"
    );
    // The secrets the post-withdrawal surfaces must withhold really are in the
    // authorized notice, the router's own body — nothing is invented here.
    for (label, secret) in [
        ("job id", job.as_str()),
        ("task id", task.as_str()),
        ("state", "awaiting_approval"),
    ] {
        assert!(
            notice.body.contains(secret),
            "the delivered notice body does not name the {label} '{secret}': {}",
            notice.body
        );
    }

    // Deliver AND RUN the notice while the share is live: the pack can only
    // quote a delivered turn, not a queued intent.
    cad2207_race_serve_on_fake(fx, &subscriber.alias);
    fx.shared
        .launch_actor(&subscriber.alias)
        .unwrap_or_else(|e| panic!("the subscriber's assistant did not start: {e}"));
    let delivered = cad2207_race_settle(fx, &notice.id);
    assert_eq!(
        delivered.state, "completed",
        "the delivered notice did not run to a reply while the share was live: {delivered:?}"
    );

    // The member's own completed ordinary turn — the pack's positive control.
    cad2207_race_send(fx, CAD2207_RACE_OWN_NOTE, CAD2207_RACE_OWN_MESSAGE);
    let own = cad2207_race_settle(fx, CAD2207_RACE_OWN_MESSAGE);
    assert_eq!(
        own.state, "completed",
        "the subscriber's own turn did not complete: {own:?}"
    );

    cad2207_race_quiet(fx, &subscriber.alias);

    // The boundary control: a second shared job whose intent is NEVER drained,
    // so no notice for it can be quoted into this thread.
    let control = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_RACE_OWNER),
            "title": CAD2207_RACE_CONTROL_TITLE,
            "request_key": CAD2207_RACE_CONTROL_KEY,
            "body": "cad2207 continuity withdrawal race: the unrelated job spec",
            "acceptance": "an unrelated share is never blocked by another job's submission",
        }),
    );
    let control_job = control
        .get("job")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("member_job_request returned no control job: {control}"))
        .to_string();
    fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_RACE_OWNER),
            "job": control_job,
            "enabled": true,
        }),
    );
    fx.op(
        "member_job_subscribe",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_RACE_SUBSCRIBER),
            "job": control_job,
        }),
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&control_job, ORG)
            .unwrap(),
        "the fixture's control share did not land"
    );
    assert!(
        fx.shared
            .store
            .job_is_subscriber(&control_job, ORG, CAD2207_RACE_SUBSCRIBER)
            .unwrap(),
        "the fixture did not create the control subscriber row"
    );
    let control_notice = fx
        .shared
        .store
        .messages(&subscriber.alias)
        .unwrap_or_else(|error| panic!("could not read the subscriber's messages: {error}"))
        .into_iter()
        .any(|message| {
            message.source == CAD2207_RACE_NOTICE
                && fx
                    .shared
                    .store
                    .assist_notice_delivery(&message.id)
                    .unwrap()
                    .is_some_and(|delivery| delivery.job == control_job)
        });
    assert!(
        !control_notice,
        "the unrelated control job acquired a notice — it would not isolate the boundary"
    );

    Cad2207RaceScenario {
        job,
        task,
        notice,
        subscriber_alias: subscriber.alias,
        subscriber_thread: subscriber.thread_id,
        control_job,
    }
}

/// CAD-2207 (negative acceptance, provider boundary): an ordinary member turn
/// whose continuity pack already quotes a delivered private notice must not
/// reach the provider after the owner's LOCAL withdrawal completes, and a
/// withdrawal that cannot complete must be an explicit pending/refusal.
///
/// The turn is parked in its FINAL member-authority revalidation, after
/// `continuity_parts` assembled the pack (the `PACK_EVENT` is already recorded)
/// and before `run_turn_slotted`. While it is parked, the owner's real
/// `member_job_share {enabled:false}` is issued on its own thread (so its own
/// live-authority pull is answered concurrently). The witness is the shipped
/// provider's `cadence/fake_pack` event: the actual pack prefix the provider
/// received.
///
/// At the baseline (`1f8026f15`) admissions are reserved only for
/// `assist_job_event` turns, so the withdrawal returns `shared:false` and the
/// already-built pack — notice included — is submitted. This fails on
/// `cad2207_race_assert_withheld`.
#[test]
fn cad2207_continuity_withdrawal_races_the_final_member_revalidation() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let scenario = cad2207_race_fixture(&fx);
    // Serve each platform request on its own worker, so the racing
    // `member_job_share` call's own live-authority pull is answered while the
    // final re-prove is parked. Every other fixture keeps the default
    // single-threaded accept loop.
    fx.concurrent.store(true, Ordering::SeqCst);

    // Force a fresh provider session the way the daemon itself records one that
    // was not carried over: `clear_native_session` drops the stored
    // thread/session id, so the next open schedules `Reason::New` and the next
    // turn rebuilds a pack. Then stop the endpoint so the member send below only
    // queues (its own live-authority pull is answered before any barrier is
    // armed), and the actor claims the row only after the barrier is armed.
    fx.shared
        .store
        .clear_native_session(&scenario.subscriber_alias)
        .unwrap_or_else(|error| panic!("the fixture could not clear the stored session: {error}"));
    fx.shared
        .store
        .set_enabled(&scenario.subscriber_alias, false)
        .unwrap_or_else(|e| panic!("the fixture could not stop the subscriber assistant: {e}"));

    cad2207_race_send(&fx, CAD2207_RACE_TURN, CAD2207_RACE_CURRENT);
    let queued = fx
        .shared
        .store
        .message(CAD2207_RACE_CURRENT)
        .unwrap_or_else(|error| panic!("the raced turn could not be read: {error}"))
        .unwrap_or_else(|| panic!("the fixture's current turn was not queued"));
    assert_eq!(
        queued.state, "queued",
        "the fixture's current turn was run before the race: {queued:?}"
    );

    let baseline = cad2207_race_packs(&fx, &scenario.subscriber_alias).len();

    // Control BEFORE the race: the production builder really carries the
    // delivered notice plus the member's own ordinary turn for exactly this
    // queued message. A fix cannot pass by never building the pack.
    let live = cad2207_race_live_pack(
        &fx,
        &scenario.subscriber_alias,
        &scenario.subscriber_thread,
        CAD2207_RACE_CURRENT,
    );
    cad2207_race_assert_served("the live pre-race continuity pack", &live, &scenario);
    assert!(
        live.contains(CAD2207_RACE_OWN_NOTE),
        "the live pre-race continuity pack lost the member's own conversation: {live}"
    );

    // Arm the barrier for the actor's FIRST pull (the take-gate re-prove), then
    // let the actor claim the queued ordinary turn.
    {
        let mut guard = fx
            .authority
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            !guard.hold_next,
            "the fixture barrier was already armed before the race"
        );
        guard.hold_next = true;
    }
    fx.shared
        .store
        .set_enabled(&scenario.subscriber_alias, true)
        .unwrap_or_else(|e| {
            panic!("the fixture could not re-enable the subscriber assistant: {e}")
        });
    fx.shared
        .launch_actor(&scenario.subscriber_alias)
        .unwrap_or_else(|e| panic!("the subscriber's assistant did not restart: {e}"));
    cad2207_race_pull_parked(
        &fx,
        "the ordinary turn's take-gate re-prove never reached the live authority pull",
    );
    // Release the take gate and park the FINAL re-prove
    // (`admit_app_submission` -> `member_message_reprove`). The pack has already
    // been assembled into the prompt at this point, and no provider turn exists.
    cad2207_race_release_and_park_next(
        &fx,
        "the final member-authority revalidation never reached the live authority pull",
    );

    let mid = fx
        .shared
        .store
        .message(CAD2207_RACE_CURRENT)
        .unwrap_or_else(|error| panic!("the raced turn could not be read: {error}"))
        .expect("the raced ordinary turn vanished before its final re-prove");
    assert_eq!(
        mid.state, "submitting",
        "the ordinary turn was not claimed before its final re-prove: {mid:?}"
    );
    assert!(
        mid.turn_id.is_none(),
        "a provider turn was recorded before the final re-prove finished: {mid:?}"
    );
    assert!(
        cad2207_race_pack_recorded(&fx, &scenario.subscriber_alias, CAD2207_RACE_CURRENT),
        "the ordinary turn reached its final member-authority revalidation before its continuity \
         pack was assembled — the race would not be testing an assembled notice-bearing prompt"
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&scenario.job, ORG)
            .unwrap(),
        "the owner's share was not live at the start of the race"
    );

    // Boundary control, under the SAME parked pull: the owner withdraws an
    // unrelated shared job whose notice is nowhere in this thread. It must
    // succeed, so a refusal of the notice's job cannot be blanket behavior.
    let control =
        cad2207_race_unshare_bounded(&fx, &scenario.control_job).unwrap_or_else(|error| {
            panic!(
            "the unrelated job's withdrawal was refused while the notice-bearing submission was \
             parked — the pending boundary is too broad: {error}"
        )
        });
    assert_eq!(
        control["shared"],
        json!(false),
        "the unrelated job's withdrawal did not report shared:false: {control}"
    );
    assert!(
        !fx.shared
            .store
            .assist_job_is_shared(&scenario.control_job, ORG)
            .unwrap(),
        "the unrelated job's withdrawal reported shared:false without clearing its share"
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&scenario.job, ORG)
            .unwrap(),
        "the unrelated control withdrawal changed the notice job's share"
    );

    // The race: the owner withdraws the very job whose notice the assembled pack
    // quotes, while the final re-prove is still parked (before submission).
    let withdrawal = cad2207_race_unshare_bounded(&fx, &scenario.job);
    match withdrawal {
        Ok(answer) => {
            // Path B of the contract: the withdrawal completed BEFORE the
            // resumed provider submission (the pull is still parked right now).
            assert_eq!(
                answer["shared"],
                json!(false),
                "member_job_share did not report the withdrawal: {answer}"
            );
            assert!(
                !fx.shared
                    .store
                    .assist_job_is_shared(&scenario.job, ORG)
                    .unwrap(),
                "member_job_share reported shared:false without clearing the share row"
            );

            fx.release_barrier();
            let settled = cad2207_race_settle(&fx, CAD2207_RACE_CURRENT);
            let observed = cad2207_race_packs(&fx, &scenario.subscriber_alias);
            let observed = &observed[baseline..];
            for pack in observed {
                cad2207_race_assert_withheld(
                    "the provider's observed prompt after the completed withdrawal",
                    pack,
                    &scenario,
                );
            }
            assert!(
                !matches!(settled.state.as_str(), "queued" | "submitting"),
                "the withdrawal completed and the turn was left silently pending (state '{}'): \
                 an assembled notice-bearing submission must be either refused or submitted \
                 without the withdrawn notice: {settled:?}",
                settled.state
            );
            if !observed.is_empty() {
                let last = observed.last().unwrap();
                assert!(
                    last.contains(CAD2207_RACE_OWN_NOTE),
                    "the provider prompt after the completed withdrawal dropped the member's own \
                     conversation — the fix must withhold the withdrawn notice, not blanket-hide \
                     the pack: {last}"
                );
            } else {
                // The provider observed nothing, so it never received the
                // notice. Acceptable only if the turn was explicitly refused
                // before submission rather than failing for no stated reason.
                assert!(
                    matches!(
                        settled.state.as_str(),
                        "failed" | "cancelled" | "rejected" | "interrupted"
                    ),
                    "the provider observed no pack after the completed withdrawal, yet the turn \
                     was not explicitly refused before submission: state '{}', {settled:?}",
                    settled.state
                );
                assert!(
                    cad2207_race_refusal_text(&settled).is_some(),
                    "the refused turn carries no recorded reason: {settled:?}"
                );
            }
        }
        Err(error) => {
            // Path A of the contract: an explicit pending/refusal while a
            // notice-bearing submission is in flight. It must be the STRUCTURED
            // reservation gate, not an unrelated failure: a remote-authority or
            // transport error (kind 'rejected', no code) would otherwise
            // masquerade as the boundary this test claims to prove.
            assert_eq!(
                error.kind(),
                "gate",
                "the withdrawal refused with kind '{}' and code {:?}, not the structured pending \
                 gate `share_withdrawal_pending`; an unrelated refusal cannot stand in for the \
                 notice-bearing reservation boundary: {error}",
                error.kind(),
                error.code()
            );
            assert_eq!(
                error.code(),
                Some("share_withdrawal_pending"),
                "the withdrawal was refused by a gate that is not `share_withdrawal_pending`; an \
                 unrelated refusal cannot stand in for the notice-bearing reservation boundary: \
                 {error}"
            );
            assert!(
                fx.shared
                    .store
                    .assist_job_is_shared(&scenario.job, ORG)
                    .unwrap(),
                "a refused member_job_share withdrew the share anyway — a pending/refusal must \
                 not half-apply"
            );

            fx.release_barrier();
            let settled = cad2207_race_settle(&fx, CAD2207_RACE_CURRENT);
            assert_eq!(
                settled.state, "completed",
                "the withdrawal was refused (the share stayed live), yet the notice-bearing \
                 submission did not complete: {settled:?}"
            );
            let observed = cad2207_race_packs(&fx, &scenario.subscriber_alias);
            let observed = &observed[baseline..];
            let submitted = observed
                .last()
                .unwrap_or_else(|| panic!("the completed turn never reached the provider"));
            // The submission really was notice-bearing — that is exactly why the
            // withdrawal had to be refused rather than applied under it.
            cad2207_race_assert_served(
                "the provider's observed prompt under the refusal",
                submitted,
                &scenario,
            );
        }
    }

    // The durable subscriber row survives either way, so a refusal cannot be
    // explained by a removed subscription.
    assert!(
        fx.shared
            .store
            .job_is_subscriber(&scenario.job, ORG, CAD2207_RACE_SUBSCRIBER)
            .unwrap(),
        "the durable subscriber row vanished — a refusal could then be the subscriber gate \
         instead of the withdrawn share"
    );

    cad2207_race_quiet(&fx, &scenario.subscriber_alias);
}

// ============================================================================
// Rendered-budget boundary: a share whose notice is NOT in the assembled prompt
// must not be held. The fix reserves the jobs whose receipt-linked turns the
// rendered pack actually carries; an older notice pushed outside BOTH the
// verbatim window and the summary listing is not carried and must not block the
// owner's own withdrawal of that job.
// ============================================================================

/// The OLDER job whose notice the rendered pack drops: it is behind enough
/// ordinary turns that it is outside `LAST_TURNS` verbatim and outside the
/// `SUMMARY_MAX` one-line listing.
const CAD2207_BUDGET_OMITTED_KEY: &str = "cad2207-continuity-budget-omitted-job";
const CAD2207_BUDGET_OMITTED_TITLE: &str = "cad2207 continuity budget omitted private job";
/// The RECENT job whose notice the rendered pack carries verbatim.
const CAD2207_BUDGET_INCLUDED_KEY: &str = "cad2207-continuity-budget-included-job";
const CAD2207_BUDGET_INCLUDED_TITLE: &str = "cad2207 continuity budget included private job";
/// Ordinary member turns between the two notices. Each rendered summary line is
/// ~330 bytes against `SUMMARY_MAX` (3_500), so 26 of them leave the older
/// notice well outside the summary listing as well as the 8-turn verbatim
/// window — verified by the pre-race production-pack control, never assumed.
const CAD2207_BUDGET_FILLERS: usize = 26;
const CAD2207_BUDGET_FILLER_TEXT: &str = "cad2207 continuity summary budget filler turn: the \
     ordinary member text here is deliberately long so that one rendered summary line consumes \
     its share of the pack's fixed summary byte budget";
/// The ordinary member turn that races the withdrawals with a pack assembled
/// from the current thread.
const CAD2207_BUDGET_CURRENT: &str = "cad2207-budget-current-turn";

/// One realized scenario: an older shared job whose notice the rendered pack
/// omits, a recent shared job whose notice the pack carries, and the ordinary
/// filler turns that separate them.
struct Cad2207BudgetScenario {
    omitted_job: String,
    omitted_task: String,
    omitted_notice: crate::store::Message,
    included_job: String,
    included_task: String,
    included_notice: crate::store::Message,
    subscriber_alias: String,
    subscriber_thread: String,
}

/// The rendered pack carries this job's notice (control: the fixture kept it
/// inside the verbatim window).
fn cad2207_budget_assert_included(what: &str, text: &str, scenario: &Cad2207BudgetScenario) {
    for (label, secret) in [
        ("job id", scenario.included_job.as_str()),
        ("task id", scenario.included_task.as_str()),
        ("notice body", scenario.included_notice.body.as_str()),
    ] {
        assert!(
            text.contains(secret),
            "{what} did not carry the included notice's {label} '{secret}': {text}"
        );
    }
}

/// The rendered pack does NOT carry this job's notice: neither the router body
/// nor the job/task/message identifiers appear. A hit here is a FIXTURE failure
/// (the filler turns did not push the older notice outside the rendered
/// summary/turns), never a product assertion.
fn cad2207_budget_assert_omitted(what: &str, text: &str, scenario: &Cad2207BudgetScenario) {
    for (label, secret) in [
        ("job id", scenario.omitted_job.as_str()),
        ("task id", scenario.omitted_task.as_str()),
        ("notice body", scenario.omitted_notice.body.as_str()),
        ("notice message id", scenario.omitted_notice.id.as_str()),
    ] {
        assert!(
            !text.contains(secret),
            "{what} still carries the omitted notice's {label} '{secret}'. The fixture did not \
             push that turn outside the rendered summary/turns (increase the filler count or \
             length); this is a fixture failure, not a product assertion: {text}"
        );
    }
}

/// Both notices are inside the ACTUAL `SOURCE_ENTRIES` gather window used to
/// build the pack, by message id. This is what makes the fixture reproduce
/// "gathered but not rendered" rather than "beyond the source window": a notice
/// dropped by `SOURCE_ENTRIES` would never be a reservation candidate at all,
/// from either implementation.
fn cad2207_budget_assert_gathered(fx: &Fx, scenario: &Cad2207BudgetScenario, current: &str) {
    let thread = fx
        .shared
        .store
        .thread_by_id(&scenario.subscriber_thread)
        .unwrap_or_else(|error| panic!("the bound thread could not be read: {error}"))
        .unwrap_or_else(|| panic!("the bound thread '{}' vanished", scenario.subscriber_thread));
    let pm = fx.pm();
    let sources = crate::continuity::gather_for(
        &fx.shared.store,
        Some(pm.as_path()),
        &scenario.subscriber_alias,
        current,
        Some(&thread),
    )
    .unwrap_or_else(|error| panic!("the continuity gather failed: {error}"));
    let gathered = |id: &str| {
        sources.entries.iter().any(|entry| {
            entry.message_id.as_deref() == Some(id)
                || entry
                    .payload
                    .as_ref()
                    .and_then(|payload| payload.get("message"))
                    .and_then(Value::as_str)
                    == Some(id)
        })
    };
    for (label, notice) in [
        ("omitted", &scenario.omitted_notice),
        ("included", &scenario.included_notice),
    ] {
        assert!(
            gathered(notice.id.as_str()),
            "the {label} notice is NOT in the gathered source window — it is beyond \
             SOURCE_ENTRIES, so an entry dropped there would not reproduce over-reservation \
             of a gathered-but-unrendered notice"
        );
    }
    assert_eq!(
        sources.older_entries, 0,
        "the fixture's thread already exceeds SOURCE_ENTRIES; the omitted notice would then be \
         outside the gather window rather than merely outside the rendered summary/turns"
    );
}

/// Build both jobs, run the older notice, fill the rendered budget with real
/// ordinary member turns, then run the recent notice. Nothing is synthesized:
/// both notices come from the real intent drain and both run on the shipped
/// provider; every filler turn is a real `member_assist_send` turn.
fn cad2207_budget_fixture(fx: &Fx) -> Cad2207BudgetScenario {
    fx.member_runtime(CAD2207_RACE_OWNER);
    let subscriber = fx.member_runtime(CAD2207_RACE_SUBSCRIBER);

    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_RACE_OWNER),
            "title": CAD2207_BUDGET_OMITTED_TITLE,
            "request_key": CAD2207_BUDGET_OMITTED_KEY,
            "body": "cad2207 budget omission: the older private job spec",
            "acceptance": "an omitted continuity turn never blocks the owner's withdrawal",
        }),
    );
    let omitted_job = request
        .get("job")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("member_job_request returned no omitted job: {request}"))
        .to_string();
    let omitted_task = format!("{omitted_job}-t1");
    fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_RACE_OWNER),
            "job": omitted_job,
            "enabled": true,
        }),
    );
    fx.op(
        "member_job_subscribe",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_RACE_SUBSCRIBER),
            "job": omitted_job,
        }),
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&omitted_job, ORG)
            .unwrap(),
        "the fixture's omitted-job share did not land"
    );
    fx.shared
        .store
        .assist_request_notice(&omitted_job)
        .unwrap_or_else(|error| panic!("the omitted notice intent did not commit: {error}"));
    fx.shared
        .route_assist_job_notifications()
        .unwrap_or_else(|error| panic!("the assist intent drain failed: {error}"));
    let omitted_notice = cad2207_race_notice(fx, &subscriber.alias, &omitted_job);
    assert_eq!(
        omitted_notice.task_id.as_deref(),
        Some(omitted_task.as_str()),
        "the omitted notice does not carry the job's own task"
    );

    // Run the older notice, then fill the rendered budget with ordinary turns.
    cad2207_race_serve_on_fake(fx, &subscriber.alias);
    fx.shared
        .launch_actor(&subscriber.alias)
        .unwrap_or_else(|e| panic!("the subscriber's assistant did not start: {e}"));
    let delivered = cad2207_race_settle(fx, &omitted_notice.id);
    assert_eq!(
        delivered.state, "completed",
        "the older notice did not run while its share was live: {delivered:?}"
    );
    for index in 0..CAD2207_BUDGET_FILLERS {
        let message = format!("cad2207-budget-filler-{index:02}");
        cad2207_race_send(fx, CAD2207_BUDGET_FILLER_TEXT, &message);
        let settled = cad2207_race_settle(fx, &message);
        assert_eq!(
            settled.state, "completed",
            "the fixture's ordinary turn '{message}' did not complete: {settled:?}"
        );
    }

    // The recent shared job: its notice is the newest turn, so the rendered
    // pack carries it verbatim (the positive control).
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_RACE_OWNER),
            "title": CAD2207_BUDGET_INCLUDED_TITLE,
            "request_key": CAD2207_BUDGET_INCLUDED_KEY,
            "body": "cad2207 budget omission: the recent private job spec",
            "acceptance": "a rendered continuity turn keeps its share held through submission",
        }),
    );
    let included_job = request
        .get("job")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("member_job_request returned no included job: {request}"))
        .to_string();
    let included_task = format!("{included_job}-t1");
    fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_RACE_OWNER),
            "job": included_job,
            "enabled": true,
        }),
    );
    fx.op(
        "member_job_subscribe",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_RACE_SUBSCRIBER),
            "job": included_job,
        }),
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&included_job, ORG)
            .unwrap(),
        "the fixture's included-job share did not land"
    );
    fx.shared
        .store
        .assist_request_notice(&included_job)
        .unwrap_or_else(|error| panic!("the included notice intent did not commit: {error}"));
    fx.shared
        .route_assist_job_notifications()
        .unwrap_or_else(|error| panic!("the assist intent drain failed: {error}"));
    let included_notice = cad2207_race_notice(fx, &subscriber.alias, &included_job);
    assert_eq!(
        included_notice.task_id.as_deref(),
        Some(included_task.as_str()),
        "the included notice does not carry the job's own task"
    );
    let delivered = cad2207_race_settle(fx, &included_notice.id);
    assert_eq!(
        delivered.state, "completed",
        "the recent notice did not run while its share was live: {delivered:?}"
    );

    cad2207_race_quiet(fx, &subscriber.alias);

    Cad2207BudgetScenario {
        omitted_job,
        omitted_task,
        omitted_notice,
        included_job,
        included_task,
        included_notice,
        subscriber_alias: subscriber.alias,
        subscriber_thread: subscriber.thread_id,
    }
}

/// CAD-2207 (negative acceptance, rendered-budget boundary): the local share
/// admission for a member continuity pack must cover only the notice sources
/// the pack ACTUALLY RENDERS. An older receipt-linked shared notice that the
/// assembled prompt omits (outside both the verbatim window and the summary
/// listing) must not keep its job's share held: while the ordinary member turn
/// whose prompt excludes it is parked in its final member-authority
/// revalidation, the OWNER's `member_job_share {enabled:false}` for that job
/// must succeed. A job whose notice IS rendered keeps the existing protection
/// (refused pending, or withdrawn before submission with the notice never
/// reaching the provider).
///
/// The control before the race builds the production pack for the same queued
/// message and asserts the rendered text really carries the recent notice and
/// really omits the older one — so the omission is proved, never assumed. After
/// the release the test reads the provider's own `cadence/fake_pack` event: the
/// actual prompt must omit the withdrawn notice. At the baseline this is red
/// (nothing holds the included job); on a fix that reserves gathered rather
/// than rendered sources it is red at the omitted job's withdrawal.
#[test]
fn cad2207_budget_omitted_notice_does_not_block_the_owners_unshare() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let scenario = cad2207_budget_fixture(&fx);
    // Serve each platform request on its own worker so both racing
    // `member_job_share` calls' own live-authority pulls are answered while the
    // final re-prove is parked.
    fx.concurrent.store(true, Ordering::SeqCst);

    fx.shared
        .store
        .clear_native_session(&scenario.subscriber_alias)
        .unwrap_or_else(|error| panic!("the fixture could not clear the stored session: {error}"));
    fx.shared
        .store
        .set_enabled(&scenario.subscriber_alias, false)
        .unwrap_or_else(|e| panic!("the fixture could not stop the subscriber assistant: {e}"));

    cad2207_race_send(&fx, CAD2207_RACE_TURN, CAD2207_BUDGET_CURRENT);
    let queued = fx
        .shared
        .store
        .message(CAD2207_BUDGET_CURRENT)
        .unwrap_or_else(|error| panic!("the raced turn could not be read: {error}"))
        .unwrap_or_else(|| panic!("the fixture's current turn was not queued"));
    assert_eq!(
        queued.state, "queued",
        "the fixture's current turn was run before the race: {queued:?}"
    );

    let baseline = cad2207_race_packs(&fx, &scenario.subscriber_alias).len();

    // Control BEFORE the race: both notices are inside the REAL `SOURCE_ENTRIES`
    // gather window (by message id), and the production builder's actual
    // rendered pack carries the recent notice and omits the older one. This
    // proves the fixture reached the rendered-budget boundary and that the
    // omission is not merely an entry dropped beyond SOURCE_ENTRIES.
    cad2207_budget_assert_gathered(&fx, &scenario, CAD2207_BUDGET_CURRENT);
    let live = cad2207_race_live_pack(
        &fx,
        &scenario.subscriber_alias,
        &scenario.subscriber_thread,
        CAD2207_BUDGET_CURRENT,
    );
    cad2207_budget_assert_included("the live pre-race continuity pack", &live, &scenario);
    cad2207_budget_assert_omitted("the live pre-race continuity pack", &live, &scenario);

    // Park the ordinary turn in its FINAL member-authority revalidation, after
    // the pack was assembled and before any provider submission.
    {
        let mut guard = fx
            .authority
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            !guard.hold_next,
            "the fixture barrier was already armed before the race"
        );
        guard.hold_next = true;
    }
    fx.shared
        .store
        .set_enabled(&scenario.subscriber_alias, true)
        .unwrap_or_else(|e| {
            panic!("the fixture could not re-enable the subscriber assistant: {e}")
        });
    fx.shared
        .launch_actor(&scenario.subscriber_alias)
        .unwrap_or_else(|e| panic!("the subscriber's assistant did not restart: {e}"));
    cad2207_race_pull_parked(
        &fx,
        "the ordinary turn's take-gate re-prove never reached the live authority pull",
    );
    cad2207_race_release_and_park_next(
        &fx,
        "the final member-authority revalidation never reached the live authority pull",
    );

    let mid = fx
        .shared
        .store
        .message(CAD2207_BUDGET_CURRENT)
        .unwrap_or_else(|error| panic!("the raced turn could not be read: {error}"))
        .expect("the raced ordinary turn vanished before its final re-prove");
    assert_eq!(
        mid.state, "submitting",
        "the ordinary turn was not claimed before its final re-prove: {mid:?}"
    );
    assert!(
        mid.turn_id.is_none(),
        "a provider turn was recorded before the final re-prove finished: {mid:?}"
    );
    assert!(
        cad2207_race_pack_recorded(&fx, &scenario.subscriber_alias, CAD2207_BUDGET_CURRENT),
        "the ordinary turn reached its final member-authority revalidation before its continuity \
         pack was assembled — the race would not be testing an assembled prompt"
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&scenario.omitted_job, ORG)
            .unwrap()
            && fx
                .shared
                .store
                .assist_job_is_shared(&scenario.included_job, ORG)
                .unwrap(),
        "both owner shares must be live at the start of the race"
    );

    // The OMITTED notice must not hold its job. A refusal here is the
    // over-reservation regression: the assembled prompt carries no trace of it.
    let omitted =
        cad2207_race_unshare_bounded(&fx, &scenario.omitted_job).unwrap_or_else(|error| {
            panic!(
            "the owner could not withdraw a job whose notice the assembled prompt does not carry \
             — the continuity admission over-reserved a gathered-but-unrendered source: {error}"
        )
        });
    assert_eq!(
        omitted["shared"],
        json!(false),
        "the omitted job's withdrawal did not report shared:false: {omitted}"
    );
    assert!(
        !fx.shared
            .store
            .assist_job_is_shared(&scenario.omitted_job, ORG)
            .unwrap(),
        "the omitted job's withdrawal reported shared:false without clearing its share"
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&scenario.included_job, ORG)
            .unwrap(),
        "withdrawing the omitted job changed the included job's share"
    );

    // The INCLUDED notice keeps the existing protection: refused pending, or
    // withdrawn before submission with the notice never reaching the provider
    // (the same contract the sibling race acceptance asserts).
    let included = cad2207_race_unshare_bounded(&fx, &scenario.included_job);

    fx.release_barrier();
    let settled = cad2207_race_settle(&fx, CAD2207_BUDGET_CURRENT);
    let observed = cad2207_race_packs(&fx, &scenario.subscriber_alias);
    let observed = &observed[baseline..];

    match included {
        Ok(answer) => {
            // The withdrawal succeeded before the resumed submission, so the
            // provider must never receive the included notice either.
            assert_eq!(
                answer["shared"],
                json!(false),
                "member_job_share did not report the included withdrawal: {answer}"
            );
            assert!(
                !fx.shared
                    .store
                    .assist_job_is_shared(&scenario.included_job, ORG)
                    .unwrap(),
                "member_job_share reported shared:false without clearing the share row"
            );
            for pack in observed {
                cad2207_budget_assert_omitted(
                    "the provider's observed prompt after the completed withdrawals",
                    pack,
                    &scenario,
                );
                // The included job was withdrawn successfully before the
                // resumed submission: its notice must not reach the provider.
                assert!(
                    !pack.contains(scenario.included_notice.body.as_str())
                        && !pack.contains(scenario.included_job.as_str()),
                    "the provider received a notice whose share the owner withdrew before the \
                     resumed submission: {pack}"
                );
            }
            assert!(
                !matches!(settled.state.as_str(), "queued" | "submitting"),
                "the withdrawals completed and the turn was left silently pending: {settled:?}"
            );
            if observed.is_empty() {
                assert!(
                    matches!(
                        settled.state.as_str(),
                        "failed" | "cancelled" | "rejected" | "interrupted"
                    ),
                    "the provider observed no pack although the included notice was withdrawn \
                     before submission, yet the turn was not explicitly refused: {settled:?}"
                );
                assert!(
                    cad2207_race_refusal_text(&settled).is_some(),
                    "the refused turn carries no recorded reason: {settled:?}"
                );
            }
        }
        Err(error) => {
            // The included notice is rendered, so its job's share stays held
            // through the provider attempt — and the already-built prompt is
            // submitted legitimately with the included notice, without the
            // withdrawn omitted one. The refusal must be the STRUCTURED
            // reservation gate: an unrelated nonempty error (remote authority,
            // dispatch, transport) cannot stand in for the boundary.
            assert_eq!(
                error.kind(),
                "gate",
                "the included withdrawal refused with kind '{}' and code {:?}, not the structured \
                 pending gate `share_withdrawal_pending`; an unrelated refusal cannot stand in \
                 for the rendered-notice reservation boundary: {error}",
                error.kind(),
                error.code()
            );
            assert_eq!(
                error.code(),
                Some("share_withdrawal_pending"),
                "the included withdrawal was refused by a gate that is not \
                 `share_withdrawal_pending`; an unrelated refusal cannot stand in for the \
                 rendered-notice reservation boundary: {error}"
            );
            assert!(
                fx.shared
                    .store
                    .assist_job_is_shared(&scenario.included_job, ORG)
                    .unwrap(),
                "a refused member_job_share withdrew the included job's share anyway"
            );
            assert_eq!(
                settled.state, "completed",
                "the included notice's share stayed live, yet its submission did not complete: \
                 {settled:?}"
            );
            let submitted = observed
                .last()
                .unwrap_or_else(|| panic!("the completed turn never reached the provider"));
            cad2207_budget_assert_included(
                "the provider's observed prompt under the pending refusal",
                submitted,
                &scenario,
            );
            cad2207_budget_assert_omitted(
                "the provider's observed prompt under the pending refusal",
                submitted,
                &scenario,
            );
        }
    }

    // Both durable subscriber rows survive, so no outcome can be explained by a
    // removed subscription.
    for job in [&scenario.omitted_job, &scenario.included_job] {
        assert!(
            fx.shared
                .store
                .job_is_subscriber(job.as_str(), ORG, CAD2207_RACE_SUBSCRIBER)
                .unwrap(),
            "the durable subscriber row for '{job}' vanished"
        );
    }

    cad2207_race_quiet(&fx, &scenario.subscriber_alias);
}
