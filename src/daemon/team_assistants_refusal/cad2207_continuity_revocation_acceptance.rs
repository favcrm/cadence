//! CAD-2207 — after a LOCAL share withdrawal, a subscriber's bound assistant
//! must not keep serving the owner's formerly shared job notice through NEW
//! retained-context reads.
//!
//! Independently authored negative acceptance, deliberately separate from the
//! implementation and from every other `team_assistants_refusal` check. This
//! module is a CHILD of that module only to reuse its already-real fixture
//! (`Fx`: a real in-process daemon, a real `operator/board-identity.json` trust
//! root, one loopback authoritative platform answering
//! `/__platform/assistant/revalidate`, and the real `Store`). Every input under
//! test is produced by the REAL code path: the owner's `member_job_request`,
//! the real `member_job_share` / `member_job_subscribe` RPCs on signed member
//! envelopes, the real `assist_request_notice` + `route_assist_job_notifications`
//! intent drain, and the real `member_job_share {enabled:false}` withdrawal.
//! The notice body is the router's — this file never writes one.
//!
//! WHAT THE CONTRACT REQUIRES. `member_job_share {enabled:false}` withdraws
//! the owner's LIVE share and deliberately keeps the durable subscriber row
//! (the response says so). From that point on, the subscriber's assistant must
//! not disclose the revoked job notice through a NEW request:
//!   * its own `thread_read` (the bound assistant connection, the member
//!     surface in `daemon::members_rpc`) must not return the notice's
//!     job/task/body, its message id, or the `assist_job_event` origin marker,
//!     while the member's OWN ordinary conversation is still served;
//!   * a NEW continuity pack must not quote the notice (its job/task/body)
//!     into the next provider attempt. The pack is built by the production
//!     `continuity::assemble` — the exact function `Shared::continuity_parts`
//!     calls before a turn — and, second, by a real actor turn on the shipped
//!     in-process provider, whose `cadence/fake_pack` event records the pack
//!     the actor really put ahead of the provider call;
//!   * the `reply_seq` watermark `thread_read` publishes beside the page must
//!     follow VISIBLE history only: after the withdrawal it may not name the
//!     hidden notice turn's reply on the forward page or on either backward
//!     (tail / `before`) branch, and it must still name the subscriber's own
//!     unrelated pre-notice reply. The notice turn is run to a real nonempty
//!     provider reply while the share is live, and its seq is read back from
//!     that same public field.
//!
//! SCOPE, stated narrowly so it cannot be over-read. This is the OWNER's LOCAL
//! withdrawal only. Remote membership/session/credential/grant revocation lives
//! in the platform service behind `member_authority_live`, is re-derived per
//! effect there, and is NOT re-tested here. Content a caller already received,
//! a provider prompt already sent, and anything a peer already copied are
//! irrevocable and are NOT claimed to be erased: the pre-withdrawal read is
//! deliberately made and asserted, precisely so "gone after" cannot be
//! confused with "never there". No refusal wording is asserted; no lock, SQL or
//! private helper is inspected as the thing under test.
//!
//! BASELINE FAILURE (at `50c5f725`): unshare deleted only the share row.
//! The delivered notice, receipt and message-linked turn survived; neither
//! `rpc_thread_read` nor the continuity read checked the current share.
//! `rpc_thread_read` additionally derives its `reply_seq` watermark from
//! `Store::thread_reply_seq`, which scans the RAW thread for the newest agent
//! reply independently of the projected entries — so filtering the hidden
//! entry out of the page alone still publishes the withdrawn reply's sequence
//! (and its position) as the watermark on both the forward and backward
//! branches. The watermark must be derived from the same visible projection.
//!
//! NOTE ON THE WHOLE TURN. The notice turn is message-linked: its opening
//! entry, the provider's reply and the turn result all carry the notice's
//! message id. The fake provider echoes its prompt, so the reply repeats the
//! same job/task/body. That echo is part of what a pack would quote, so the
//! content assertions below are on the whole pack/response, not just the
//! opening entry — a fix that filters only the `assist_job_event` row and
//! leaves the reply still quotes the withdrawn notice.
//!
//! POSITIVE CONTROLS IN THE SAME FIXTURE (a blanket hide cannot pass any
//! case): the subscriber's own member message in its bound conversation stays
//! readable and stays in the pack; the OWNER's own assistant still serves the
//! same delivered notice after the same withdrawal (the owner never lost the
//! job); and each pack case first asserts the LIVE pack really does carry the
//! notice, so the capability of the builder is proved before it is required to
//! withhold.
//!
//! RUN (after the module is registered with
//! `mod cad2207_continuity_revocation_acceptance;` in
//! `src/daemon/team_assistants_refusal.rs`):
//! `scripts/boat-build -- cargo test --locked --features test-seam --lib
//! cad2207_continuity_revocation -- --test-threads 1`

use super::*;

/// The member who owns the job and grants/withdraws its share.
const CAD2207_OWNER: &str = "sub-alice";
/// The second member whose assistant follows the shared job.
const CAD2207_SUBSCRIBER: &str = "sub-ben";
/// The owner's canonical request key — the job id is derived from it, so the
/// delivered notice names an identifier that exists nowhere else.
const CAD2207_REQUEST_KEY: &str = "cad2207-continuity-revocation-job";
/// The owner's private job title.
const CAD2207_JOB_TITLE: &str = "cad2207 continuity revocation private job";
/// The assistant-origin marker a delivered job notice carries.
const CAD2207_NOTICE_SOURCE: &str = "assist_job_event";
/// The subscriber's own ordinary message — the control content that must
/// survive the withdrawal in the read AND in the pack.
const CAD2207_OWN_NOTE: &str = "cad2207 continuity revocation: the subscriber's own ordinary note";
/// A fresh member turn queued AFTER the withdrawal. It is the pack's `current`
/// message (never quoted by `assemble`) and its own delivery attempt is what
/// builds the pack in the actor case.
const CAD2207_NEXT_TURN: &str = "cad2207 continuity revocation: the next turn after the withdrawal";
/// Stable client-side message ids (the identifier grammar is lowercase
/// letters, digits and hyphens).
const CAD2207_OWN_MESSAGE: &str = "cad2207-continuity-own-note";
const CAD2207_CURRENT_MESSAGE: &str = "cad2207-continuity-current-turn";
const CAD2207_PROVIDER_MESSAGE: &str = "cad2207-continuity-provider-turn";

/// The real stored state one fixture seeds: the owner's job, its live share,
/// the durable subscriber, and the delivered notice in each bound thread.
struct Cad2207Continuity {
    job: String,
    task: String,
    owner_alias: String,
    owner_notice: crate::store::Message,
    subscriber_alias: String,
    subscriber_thread: String,
    subscriber_notice: crate::store::Message,
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

/// One full real scenario: two provisioned members, the owner's canonical job,
/// the owner's REAL share, the second member's REAL subscription, and ONE
/// notice per bound thread produced by the real intent drain (which writes the
/// receipts the turn re-prove requires). Nothing is synthesized.
fn cad2207_fixture(fx: &Fx) -> Cad2207Continuity {
    let owner = fx.member_runtime(CAD2207_OWNER);
    let subscriber = fx.member_runtime(CAD2207_SUBSCRIBER);

    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_OWNER),
            "title": CAD2207_JOB_TITLE,
            "request_key": CAD2207_REQUEST_KEY,
            "body": "cad2207 continuity revocation: the owner's private job spec",
            "acceptance": "a withdrawn share leaves no new retained-context leak",
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

    // The REAL intent drain writes both notices (and their delivery receipts).
    fx.shared
        .store
        .assist_request_notice(&job)
        .unwrap_or_else(|error| panic!("the assist notice intent did not commit: {error}"));
    fx.shared
        .route_assist_job_notifications()
        .unwrap_or_else(|error| panic!("the assist intent drain failed: {error}"));

    let notice = cad2207_notice(fx, &subscriber.alias, &job);
    let owner_notice = cad2207_notice(fx, &owner.alias, &job);
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

    Cad2207Continuity {
        job,
        task,
        owner_alias: owner.alias,
        owner_notice,
        subscriber_alias: subscriber.alias,
        subscriber_thread: subscriber.thread_id,
        subscriber_notice: notice,
    }
}

/// The owner's REAL withdrawal: it must complete, clear the share row, and
/// leave the durable subscriber row in place — otherwise "gone after" could be
/// explained by a removed subscription instead of the withdrawn share.
fn cad2207_unshare(fx: &Fx, job: &str) {
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
}

/// The subscriber's own ordinary conversation, through the REAL member send.
/// `message` is supplied by the test, so the entry/message id is known without
/// parsing the receipt. The caller decides whether an actor may run.
fn cad2207_send_own(fx: &Fx, text: &str, message: &str) {
    let receipt = fx.op(
        "member_assist_send",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_SUBSCRIBER),
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

/// Serve an already-bound member assistant on the shipped in-process provider
/// (`endpoint_kind = "fake"`). Binding, cwd, conversation and every member
/// guard are unchanged — only the launch surface differs, and that is the same
/// `ProviderAdapter` contract a managed runtime follows.
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
fn cad2207_quiet(fx: &Fx, alias: &str) {
    let _ = fx.shared.rpc_stop(&json!({"alias": alias}));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
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
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Wait, bounded, until one message reaches a terminal state.
fn cad2207_settle(fx: &Fx, id: &str) -> crate::store::Message {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
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
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The bound assistant's OWN `thread_read`, on the daemon-derived agent
/// identity the member surface admits (`Asserted::Agent` is the registered
/// caller proof — no envelope, no operator, no cleared identity).
fn cad2207_thread_read(fx: &Fx, alias: &str) -> Value {
    cad2207_thread_page(fx, alias, json!({}))
}

/// The same read with one extra paging field (`tail`/`before`), so the backward
/// branches are driven through the public page contract.
fn cad2207_thread_page(fx: &Fx, alias: &str, extra: Value) -> Value {
    let mut params = json!({"alias": alias, "limit": 200});
    if let Some(extra) = extra.as_object() {
        for (key, value) in extra {
            params[key.as_str()] = value.clone();
        }
    }
    fx.as_assistant(alias, "thread_read", params)
        .unwrap_or_else(|e| {
            panic!("the bound assistant '{alias}' could not read its own thread: {e}")
        })
}

/// The entry at `seq` is an agent reply/result for `message` with nonblank
/// text — the exact shape `thread_read`'s `reply_seq` watermark names, read
/// from the consumer's own page (never from SQL).
fn cad2207_assert_agent_reply(page: &Value, seq: i64, message: &str, what: &str) {
    let entry = cad2207_entries(page)
        .iter()
        .find(|entry| entry.get("seq").and_then(Value::as_i64) == Some(seq))
        .unwrap_or_else(|| panic!("{what}: no entry at reply_seq {seq} on the page: {page}"));
    assert_eq!(
        entry.get("role").and_then(Value::as_str),
        Some("agent"),
        "{what}: the reply_seq entry is not an agent entry: {entry}"
    );
    assert!(
        matches!(
            entry.get("kind").and_then(Value::as_str),
            Some("assistant_text") | Some("turn_result")
        ),
        "{what}: the reply_seq entry is not an assistant reply/result: {entry}"
    );
    assert_eq!(
        entry.get("message").and_then(Value::as_str),
        Some(message),
        "{what}: the reply_seq entry is linked to another message: {entry}"
    );
    assert!(
        !entry
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "{what}: the reply_seq entry has blank text: {entry}"
    );
}

/// A page after the withdrawal must withhold the withdrawn notice turn AND
/// report the correct visible reply watermark: the sequence of the member's
/// own pre-notice reply, never the hidden notice reply's sequence.
fn cad2207_assert_watermark(
    page: &Value,
    scenario: &Cad2207Continuity,
    hidden: i64,
    visible: i64,
    what: &str,
) {
    let reply_seq = page["reply_seq"]
        .as_i64()
        .unwrap_or_else(|| panic!("{what}: the page carries no reply_seq: {page}"));
    assert_ne!(
        reply_seq, hidden,
        "{what} still reports the hidden notice reply's sequence {hidden} as its reply \
         watermark: {page}"
    );
    assert_eq!(
        reply_seq, visible,
        "{what} did not report the visible own reply's sequence {visible} as its reply \
         watermark: {page}"
    );
    cad2207_assert_agent_reply(page, reply_seq, CAD2207_OWN_MESSAGE, what);
    let entries = cad2207_entries(page);
    assert!(
        !entries
            .iter()
            .any(|entry| entry.get("seq").and_then(Value::as_i64) == Some(hidden)),
        "{what} returned an entry at the hidden notice reply's sequence {hidden}: {page}"
    );
    assert!(
        !entries.iter().any(|entry| {
            entry.get("message").and_then(Value::as_str)
                == Some(scenario.subscriber_notice.id.as_str())
        }),
        "{what} returned an entry linked to the withdrawn notice: {page}"
    );
    cad2207_assert_withheld(what, &page.to_string(), scenario);
}

fn cad2207_entries(page: &Value) -> &[Value] {
    page["entries"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("thread_read returned no entries array: {page}"))
}

/// The production continuity builder for one bound thread. `current` is the
/// message the pack travels with — the same argument `Shared::continuity_parts`
/// passes, so the current turn is never quoted into its own pack.
fn cad2207_assemble(fx: &Fx, alias: &str, thread_id: &str, current: &str) -> String {
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

/// Every `cadence/fake_pack` event the actor's provider recorded for an alias.
fn cad2207_packs(fx: &Fx, alias: &str) -> Vec<String> {
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

/// Wait, bounded, for a pack the actor delivered AFTER the given baseline — the
/// actual prompt prefix the real actor handed the provider.
fn cad2207_wait_pack(fx: &Fx, alias: &str, baseline: usize) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let packs = cad2207_packs(fx, alias);
        if packs.len() > baseline {
            return packs[packs.len() - 1].clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the actor never delivered a continuity pack for '{alias}' after the withdrawal"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// No trace of the withdrawn notice in a serialized response / pack text: the
/// router's body, the job and task it names, and the notice's own message id.
fn cad2207_assert_withheld(what: &str, text: &str, scenario: &Cad2207Continuity) {
    for (label, secret) in [
        ("job id", scenario.job.as_str()),
        ("task id", scenario.task.as_str()),
        ("notice body", scenario.subscriber_notice.body.as_str()),
        ("notice message id", scenario.subscriber_notice.id.as_str()),
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
fn cad2207_assert_served(what: &str, text: &str, scenario: &Cad2207Continuity) {
    for (label, secret) in [
        ("job id", scenario.job.as_str()),
        ("task id", scenario.task.as_str()),
        ("notice body", scenario.subscriber_notice.body.as_str()),
    ] {
        assert!(
            text.contains(secret),
            "{what} did not carry the {label} '{secret}': {text}"
        );
    }
}

/// Build the job/share/subscription and the subscriber's own UNRELATED reply
/// BEFORE the notice exists, so the reply watermark has a visible control older
/// than the notice turn. Returns the scenario plus the watermark the REAL self
/// read reported while only that own reply existed.
///
/// The notice intents from `member_job_request` are still pending here — no
/// watch loop runs in the fixture and this file never drains them early — so
/// the own turn is delivered first and the notice turn second.
fn cad2207_preexisting_reply_fixture(fx: &Fx) -> (Cad2207Continuity, i64) {
    let owner = fx.member_runtime(CAD2207_OWNER);
    let subscriber = fx.member_runtime(CAD2207_SUBSCRIBER);

    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_OWNER),
            "title": CAD2207_JOB_TITLE,
            "request_key": CAD2207_REQUEST_KEY,
            "body": "cad2207 continuity revocation: the owner's private job spec",
            "acceptance": "a withdrawn share leaves no new retained-context leak",
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

    // The subscriber's own unrelated turn runs FIRST on the fake provider; its
    // reply is the watermark that must survive once the notice turn is withheld.
    cad2207_serve_on_fake(fx, &subscriber.alias);
    fx.shared
        .launch_actor(&subscriber.alias)
        .unwrap_or_else(|e| panic!("the subscriber's assistant did not start: {e}"));
    cad2207_send_own(fx, CAD2207_OWN_NOTE, CAD2207_OWN_MESSAGE);
    let own = cad2207_settle(fx, CAD2207_OWN_MESSAGE);
    assert_eq!(
        own.state, "completed",
        "the subscriber's own turn did not complete: {own:?}"
    );

    // Only the own reply exists yet: the real self read names it.
    let before_notice = cad2207_thread_read(fx, &subscriber.alias);
    let visible_reply_seq = before_notice["reply_seq"]
        .as_i64()
        .unwrap_or_else(|| panic!("the self read returned no reply_seq: {before_notice}"));
    cad2207_assert_agent_reply(
        &before_notice,
        visible_reply_seq,
        CAD2207_OWN_MESSAGE,
        "the pre-notice own reply",
    );

    // Let the REAL intent drain deliver the owner's pending intent and the
    // subscriber's new one, then run the notice to a real nonempty reply.
    fx.shared
        .store
        .assist_request_notice(&job)
        .unwrap_or_else(|error| panic!("the assist notice intent did not commit: {error}"));
    fx.shared
        .route_assist_job_notifications()
        .unwrap_or_else(|error| panic!("the assist intent drain failed: {error}"));

    let notice = cad2207_notice(fx, &subscriber.alias, &job);
    let owner_notice = cad2207_notice(fx, &owner.alias, &job);
    assert_eq!(
        notice.task_id.as_deref(),
        Some(task.as_str()),
        "the delivered notice does not carry the job's own task"
    );
    assert!(
        notice.body.contains(job.as_str()) && notice.body.contains(task.as_str()),
        "the delivered notice body does not name the job/task: {}",
        notice.body
    );
    let delivered = cad2207_settle(fx, &notice.id);
    assert_eq!(
        delivered.state, "completed",
        "the delivered notice did not run to a reply before the withdrawal: {delivered:?}"
    );

    (
        Cad2207Continuity {
            job,
            task,
            owner_alias: owner.alias,
            owner_notice,
            subscriber_alias: subscriber.alias,
            subscriber_thread: subscriber.thread_id,
            subscriber_notice: notice,
        },
        visible_reply_seq,
    )
}

/// CAD-2207 (negative acceptance): after a successful owner withdrawal, the
/// subscriber assistant's own `thread_read` must not return the revoked job
/// notice — not its body, job/task ids, message id or `assist_job_event`
/// origin — while the member's own ordinary conversation is still served.
///
/// At the baseline the retained entry survived unshare and was still returned.
#[test]
fn cad2207_continuity_withdrawn_share_hides_the_notice_from_own_thread_read() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let scenario = cad2207_fixture(&fx);

    // The subscriber's own ordinary conversation through the real member send.
    // The endpoint is operator-stopped FIRST, so the send commits the member's
    // own entry synchronously and performs no launch: the read below is the
    // only thing that runs.
    fx.shared
        .store
        .set_enabled(&scenario.subscriber_alias, false)
        .unwrap_or_else(|e| panic!("the fixture could not stop the subscriber assistant: {e}"));
    cad2207_send_own(&fx, CAD2207_OWN_NOTE, CAD2207_OWN_MESSAGE);
    assert!(
        !fx.shared
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .owned(&scenario.subscriber_alias),
        "the fixture launched the stopped subscriber assistant"
    );

    // Control BEFORE the withdrawal: the same read serves the delivered notice
    // and the member's own note, so the post-withdrawal absence is a change.
    let live = cad2207_thread_read(&fx, &scenario.subscriber_alias);
    let live_text = live.to_string();
    cad2207_assert_served("the live own thread_read", &live_text, &scenario);
    assert!(
        live_text.contains(CAD2207_OWN_NOTE),
        "the live own thread_read lost the member's own conversation: {live}"
    );

    cad2207_unshare(&fx, &scenario.job);

    let withdrawn = cad2207_thread_read(&fx, &scenario.subscriber_alias);
    let withdrawn_text = withdrawn.to_string();
    cad2207_assert_withheld(
        "the post-withdrawal own thread_read",
        &withdrawn_text,
        &scenario,
    );
    assert!(
        !withdrawn_text.contains(CAD2207_NOTICE_SOURCE),
        "the post-withdrawal own thread_read still carries the '{CAD2207_NOTICE_SOURCE}' \
         origin marker: {withdrawn}"
    );

    // Structural: no entry is linked to the revoked notice's message.
    let entries = cad2207_entries(&withdrawn);
    assert!(
        !entries.iter().any(|entry| {
            entry.get("message").and_then(Value::as_str)
                == Some(scenario.subscriber_notice.id.as_str())
        }),
        "the post-withdrawal own thread_read returned the revoked notice entry: {withdrawn}"
    );

    // Positive control in the SAME response: the member's own ordinary turn is
    // still served, linked to its own message — a blanket hide fails here.
    let own = entries
        .iter()
        .find(|entry| entry.get("message").and_then(Value::as_str) == Some(CAD2207_OWN_MESSAGE))
        .unwrap_or_else(|| {
            panic!(
                "the post-withdrawal own thread_read dropped the member's own message: \
                 {withdrawn}"
            )
        });
    assert_eq!(
        own.get("text").and_then(Value::as_str),
        Some(CAD2207_OWN_NOTE),
        "the surviving own entry carries another body: {own}"
    );
}

/// CAD-2207 (negative acceptance): a notice legitimately delivered (and run)
/// before the withdrawal must not be quoted into a NEW continuity pack built
/// after it, while the owner's own notice stays available.
///
/// The pack is the production `continuity::assemble` — the exact builder
/// `Shared::continuity_parts` hands to the provider prefix. The live pack is
/// asserted first, so the builder's capability is proved before it is required
/// to withhold.
///
/// At the baseline neither pack checked the current share.
#[test]
fn cad2207_continuity_withdrawn_share_is_not_quoted_into_a_new_pack() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let scenario = cad2207_fixture(&fx);

    // Bring the subscriber's assistant up on the shipped in-process provider so
    // the delivered notice runs BEFORE the withdrawal: that is what makes it a
    // legitimately delivered turn (and the only state `continuity_in` carries).
    cad2207_serve_on_fake(&fx, &scenario.subscriber_alias);
    fx.shared
        .launch_actor(&scenario.subscriber_alias)
        .unwrap_or_else(|e| panic!("the subscriber's assistant did not start: {e}"));
    let delivered = cad2207_settle(&fx, &scenario.subscriber_notice.id);
    assert_eq!(
        delivered.state, "completed",
        "the delivered notice did not run before the withdrawal: {delivered:?}"
    );

    // The subscriber's own ordinary completed turn — the pack's positive
    // control, delivered before the withdrawal like a real conversation.
    cad2207_send_own(&fx, CAD2207_OWN_NOTE, CAD2207_OWN_MESSAGE);
    let own = cad2207_settle(&fx, CAD2207_OWN_MESSAGE);
    assert_eq!(
        own.state, "completed",
        "the subscriber's own turn did not complete: {own:?}"
    );
    cad2207_quiet(&fx, &scenario.subscriber_alias);

    // A fresh member turn queued for AFTER the withdrawal. It is the pack's
    // `current`, so it is excluded from the pack by the builder itself; the
    // stopped endpoint keeps it queued (no actor runs it here).
    cad2207_send_own(&fx, CAD2207_NEXT_TURN, CAD2207_CURRENT_MESSAGE);
    let queued = fx
        .shared
        .store
        .message(CAD2207_CURRENT_MESSAGE)
        .unwrap()
        .expect("the current turn vanished");
    assert_eq!(
        queued.state, "queued",
        "the fixture's current turn was run instead of staying queued: {queued:?}"
    );

    // Control BEFORE the withdrawal: the production builder really carries the
    // notice turn, plus the member's own ordinary turn.
    let live = cad2207_assemble(
        &fx,
        &scenario.subscriber_alias,
        &scenario.subscriber_thread,
        CAD2207_CURRENT_MESSAGE,
    );
    cad2207_assert_served("the live continuity pack", &live, &scenario);
    assert!(
        live.contains(CAD2207_OWN_NOTE),
        "the live continuity pack lost the member's own conversation: {live}"
    );

    cad2207_unshare(&fx, &scenario.job);

    let withdrawn = cad2207_assemble(
        &fx,
        &scenario.subscriber_alias,
        &scenario.subscriber_thread,
        CAD2207_CURRENT_MESSAGE,
    );
    cad2207_assert_withheld("the post-withdrawal continuity pack", &withdrawn, &scenario);
    assert!(
        withdrawn.contains(CAD2207_OWN_NOTE),
        "the post-withdrawal continuity pack dropped the member's own conversation — the \
         builder must withhold the withdrawn notice, not the whole history: {withdrawn}"
    );

    // The owner's own assistant still serves the SAME delivered notice after
    // the subscriber's share was withdrawn: the owner never lost the job.
    let owner = cad2207_thread_read(&fx, &scenario.owner_alias);
    let owner_text = owner.to_string();
    assert!(
        owner_text.contains(scenario.owner_notice.body.as_str())
            && owner_text.contains(scenario.job.as_str())
            && owner_text.contains(scenario.task.as_str()),
        "the owner's own assistant lost its delivered notice after the subscriber's share was \
         withdrawn: {owner}"
    );
}

/// CAD-2207 (behavioural, provider boundary): after the withdrawal, the pack the
/// REAL actor loop actually hands the provider for a NEW turn must not quote
/// the previously delivered notice; the same pack still carries the member's
/// own ordinary conversation.
///
/// The witness is the shipped in-process provider's own `cadence/fake_pack`
/// event, emitted with the pack text it received — an actual prompt prefix from
/// a real `run_turn`, not a mirrored rendering. A fresh provider session is
/// forced the way the daemon itself records one that was not carried over:
/// `Store::clear_native_session` drops the stored thread/session id, so the next
/// open schedules `Reason::New` (the same branch a lost session takes).
///
/// At the baseline the actor's post-withdrawal pack still contained the notice.
#[test]
fn cad2207_continuity_new_actor_turn_after_withdrawal_carries_no_notice_in_the_prompt() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let scenario = cad2207_fixture(&fx);

    cad2207_serve_on_fake(&fx, &scenario.subscriber_alias);
    fx.shared
        .launch_actor(&scenario.subscriber_alias)
        .unwrap_or_else(|e| panic!("the subscriber's assistant did not start: {e}"));
    let delivered = cad2207_settle(&fx, &scenario.subscriber_notice.id);
    assert_eq!(
        delivered.state, "completed",
        "the delivered notice did not run before the withdrawal: {delivered:?}"
    );
    cad2207_send_own(&fx, CAD2207_OWN_NOTE, CAD2207_OWN_MESSAGE);
    let own = cad2207_settle(&fx, CAD2207_OWN_MESSAGE);
    assert_eq!(
        own.state, "completed",
        "the subscriber's own turn did not complete: {own:?}"
    );

    cad2207_quiet(&fx, &scenario.subscriber_alias);
    let baseline = cad2207_packs(&fx, &scenario.subscriber_alias).len();

    cad2207_unshare(&fx, &scenario.job);

    // The provider did not carry the session over: this is the store's own
    // record of that fact, and it is exactly what makes the next open schedule
    // a continuity pack.
    fx.shared
        .store
        .clear_native_session(&scenario.subscriber_alias)
        .unwrap_or_else(|e| panic!("the fixture could not clear the stored session: {e}"));
    fx.shared
        .store
        .set_enabled(&scenario.subscriber_alias, true)
        .unwrap();
    fx.shared
        .launch_actor(&scenario.subscriber_alias)
        .unwrap_or_else(|e| panic!("the subscriber's assistant did not restart: {e}"));

    // The first turn of the fresh session is the post-withdrawal member turn.
    cad2207_send_own(&fx, CAD2207_NEXT_TURN, CAD2207_PROVIDER_MESSAGE);
    let prompt = cad2207_wait_pack(&fx, &scenario.subscriber_alias, baseline);
    assert!(
        prompt.starts_with(crate::continuity::PACK_BEGIN),
        "the actor delivered something that is not a continuity pack: {prompt}"
    );
    cad2207_assert_withheld("the post-withdrawal provider prompt", &prompt, &scenario);
    assert!(
        prompt.contains(CAD2207_OWN_NOTE),
        "the post-withdrawal provider prompt dropped the member's own conversation: {prompt}"
    );
    let message = cad2207_settle(&fx, CAD2207_PROVIDER_MESSAGE);
    assert_eq!(
        message.state, "completed",
        "the post-withdrawal turn never ran to completion: {message:?}"
    );
}

/// CAD-2207 (negative acceptance, reply watermark): `thread_read` publishes a
/// `reply_seq` watermark beside the page. After the withdrawal that watermark
/// must follow the VISIBLE history — the seq of the subscriber's own pre-notice
/// reply — and must not name the hidden notice turn's reply, on the forward
/// page or on either backward branch.
///
/// The notice turn is run first to a real, nonempty provider reply while the
/// share is live, and its seq is read back from the SAME public `reply_seq`
/// field. The unrelated own reply was delivered (and its watermark recorded)
/// before the notice existed, so it is the positive control: a fix that zeroes
/// the watermark, or that leaves the hidden sequence in place, fails here.
///
/// Before the watermark fix, `Store::thread_reply_seq` scanned the raw thread
/// independently of the projected entries, and `Shared::rpc_thread_read`
/// returned the hidden reply's sequence on both branches — filtering the
/// entries alone still names the hidden reply and its exact position.
#[test]
fn cad2207_continuity_withdrawn_share_does_not_leak_the_hidden_reply_watermark() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let (scenario, visible_reply_seq) = cad2207_preexisting_reply_fixture(&fx);

    // Control BEFORE the withdrawal: the same public field names the notice
    // turn's own reply, and the older own reply is still present and named.
    let live = cad2207_thread_read(&fx, &scenario.subscriber_alias);
    let notice_reply_seq = live["reply_seq"]
        .as_i64()
        .unwrap_or_else(|| panic!("the live self read returned no reply_seq: {live}"));
    assert!(
        notice_reply_seq > visible_reply_seq,
        "the notice turn's reply ({notice_reply_seq}) is not newer than the own reply \
         ({visible_reply_seq}); the fixture did not order them as intended"
    );
    cad2207_assert_agent_reply(
        &live,
        notice_reply_seq,
        scenario.subscriber_notice.id.as_str(),
        "the live notice reply",
    );
    cad2207_assert_agent_reply(
        &live,
        visible_reply_seq,
        CAD2207_OWN_MESSAGE,
        "the live own reply",
    );

    cad2207_unshare(&fx, &scenario.job);

    // Forward page (the default read).
    let forward = cad2207_thread_read(&fx, &scenario.subscriber_alias);
    cad2207_assert_watermark(
        &forward,
        &scenario,
        notice_reply_seq,
        visible_reply_seq,
        "the forward page",
    );

    // Backward tail page.
    let tail = cad2207_thread_page(&fx, &scenario.subscriber_alias, json!({"tail": true}));
    cad2207_assert_watermark(
        &tail,
        &scenario,
        notice_reply_seq,
        visible_reply_seq,
        "the tail page",
    );

    // Backward page below a cursor placed just past the hidden notice reply, so
    // the whole notice turn is a CANDIDATE of that page.
    let before = cad2207_thread_page(
        &fx,
        &scenario.subscriber_alias,
        json!({"before": notice_reply_seq + 1}),
    );
    cad2207_assert_watermark(
        &before,
        &scenario,
        notice_reply_seq,
        visible_reply_seq,
        "the page below the hidden reply",
    );
}
