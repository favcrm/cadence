//! CAD-2207 topic Ask pre-commit acceptance — independent author.
//!
//! This module is a CHILD of `team_assistants_refusal` only to reuse its
//! already-real fixture (`Fx`: a real in-process daemon over a real store, a
//! real `operator` trust root and one loopback authoritative platform). The
//! guard, the store transactions, the topic verbs and every decision under
//! test are the existing production ones; no guard or store transition is
//! mirrored here.
//!
//! SCOPE — the one window the ticket names. `rpc_topic_request` writes the
//! generic-topic transcript post and only then queues the one request through
//! `send_with_human_expected`, whose queue transaction re-checks the expected
//! recipient uid and the member-assistant binding. The acceptance target is the
//! last moment before that queue commit:
//!
//!   ordinary subscribed target -> Ask starts -> the target's authority changes
//!   before the queue commit -> the Ask must refuse and leave nothing behind.
//!
//! The fixture drives a REAL, SUPPORTED hand-over inside that window, using
//! production entry points only (no SQL, no synthetic binding rows):
//!
//!   1. `Store::set_state_detached(alias, "stopped", None)` — the exact write
//!      `agent stop` commits;
//!   2. `Store::remove_agent(alias, force = false, by)` — the removal verb's
//!      store half, which deletes the agents row (a free alias again);
//!   3. `Store::assistant_bind(org, sub, assistant_id, alias, rev)` — now
//!      legal, because `assistant_bind` refuses to adopt an alias that is
//!      still registered as an ordinary agent;
//!   4. re-register the member's runtime under the now-private alias, then
//!      `Store::assistant_event_fence_bind_agent` — the one-time runtime-uid
//!      binding of the durable visibility fence.
//!
//! This is the real reachable sequence for the race. The in-flight Ask captured
//! the ORIGINAL uid before the park; after the hand-over the alias names a
//! different runtime and a member binding, so the queue transaction must refuse
//! rather than silently retarget. Both a moved-uid refusal
//! (`Recipient changed before the request was queued; nothing was sent`) and
//! the member-lifecycle refusal are valid answers here; the test accepts either
//! and pins neither as "the" private-binding string.
//!
//! Nothing may survive the refusal — that is the concrete unauthorized
//! observable:
//!   * no committed topic post (the pre-fix code commits it in a separate
//!     transaction, with `sender = "operator"` and `sender_uid = NULL`, which
//!     the generic visibility predicate publishes to every non-owner reader);
//!   * no `topic_deliveries` intent for the now-private target;
//!   * no `topic_post_request` binding, no request message row, no mailbox row;
//!   * a refusal whose text names neither alias, uid, body, post id nor request
//!     id, and an operator `topic_read` that shows only the ordinary PEER
//!     control Ask.
//!
//! PREREQUISITE — one deterministic pre-commit park. NO existing production
//! test seam brackets the queue commit, so this file is authored against the
//! minimal seam proposed to the manager and does NOT compile until it lands:
//!
//!   * `crate::test_seam::park_ask_before_queue_commit(is_ask: bool)` — called
//!     by `send_with_human_expected` immediately before its
//!     `enqueue_steered_for_with_conversation` call, with
//!     `request_post.is_some()`; inert unless the running thread armed a park,
//!     and compiled out of every release build.
//!   * `crate::test_seam::AskCommitPark` — the test-side handle:
//!     `new() -> Arc<Self>`, `arm(self: &Arc<Self>)` (installs the one-shot on
//!     the CALLING thread, i.e. the one that will run the Ask),
//!     `wait_entered(&self, Duration) -> bool`, `release(&self)`.
//!
//! The park is per-thread on purpose: no process-global env var and no
//! state-dir file, so fixtures running concurrently in this binary cannot see
//! each other's boundary. `pause_gate` is NOT usable here — it needs
//! `armed(state_dir)` plus a shared directory and a process-global env var.
//!
//! The boundary is a PRE-COMMIT boundary, not an after-post one: the old
//! two-transaction code has already committed the Ask's post when it parks,
//! while a correct atomic implementation will not have. This test therefore
//! only RECORDS whether a post was present (for the failure diagnostics); it
//! never requires one, and it never asserts anything about the post's
//! intermediate existence.
//!
//! Expected red today (post/queue split): the refused Ask leaves its committed
//! post row (and the target's pending intent) behind, so
//! `ask_race_handover_before_queue_commit_leaves_no_operator_visible_body`
//! fails on the surviving post and the operator `topic_read` body. It passes
//! only when the refusal leaves no post, intent, queue row or binding.
//!
//! UNPROVED here, stated narrowly: nothing about mixed-version nodes, CAD-2061
//! per-session/per-turn claim identity, CAD-2148 credential custody, the
//! member's own authorized Ask route, or OS-user separation. The test pins the
//! dispatcher's end state, not the mechanism of the fix (atomic insert+enqueue
//! and a compensating removal both satisfy it), and it does not claim that a
//! concurrent hand-over is the only reachable variant.
//!
//! Run just this module's acceptances (no checks were run while authoring):
//! `scripts/boat-build -- cargo test --locked --features test-seam --lib
//!  cad2207_topic_ask_race_acceptance -- --test-threads 1`

#![cfg(feature = "test-seam")]

use super::*;

/// The ordinary subscribed target handed to the member inside the window.
const TARGET: &str = "cad2207-ask-race-target";
/// A second ordinary subscriber that stays ordinary: the unchanged-head
/// control that proves the same topic and verb still serve an ordinary Ask.
const PEER: &str = "cad2207-ask-race-peer";
const ASK_BODY: &str = "cad2207-ask-race-body";
const ASK_ID: &str = "cad2207-ask-race-ask";
const CONTROL_BODY: &str = "cad2207-ask-race-control-body";
const CONTROL_ID: &str = "cad2207-ask-race-control";
const MEMBER: &str = "sub-alice";
const MEMBER_ASSISTANT: &str = "assist_alice";
/// The store's expected-recipient-uid refusal — the alias now names another
/// runtime, so the in-flight Ask must not retarget. Fixed text, no identifier.
const MOVED_TARGET: &str = "Recipient changed before the request was queued; nothing was sent";

/// Register a plain managed agent up front and return its durable uid. The
/// topic verbs resolve an alias through the registry, so an ordinary target
/// (and its uid for the delivery-intent assertions) must exist before the
/// topic is created; this is the registration the fixture's lazy worker gets.
fn registered_peer(fx: &Fx, alias: &str) -> String {
    let cwd = fx.dir.path().join(alias);
    std::fs::create_dir_all(&cwd).expect("the fixture cannot create the ordinary peer cwd");
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias,
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .expect("the fixture cannot register the ordinary topic peer");
    fx.shared.store.agent(alias).unwrap().uid
}

/// Create the topic as the operator and subscribe `aliases` to it.
fn fleet_topic(fx: &Fx, source: &str, aliases: &[&str]) -> String {
    let created = fx
        .call(
            Asserted::Operator,
            "topic_create",
            json!({"type": "fleet", "source": source}),
        )
        .expect("the operator could not create the race topic");
    let topic = created["topic"]
        .as_str()
        .expect("topic_create named no topic")
        .to_string();
    for alias in aliases {
        let subscribed = fx
            .call(
                Asserted::Operator,
                "topic_subscribe",
                json!({"topic": topic, "alias": alias}),
            )
            .unwrap_or_else(|e| panic!("the operator could not subscribe {alias}: {e}"));
        assert_eq!(subscribed["alias"].as_str(), Some(*alias), "{subscribed}");
    }
    topic
}

/// The real `agent stop` + `agent rm` pair, through the production store
/// writes those verbs commit. `set_state_detached` is the exact write
/// `agent stop` performs; `remove_agent` deletes the registry row (refusing a
/// live endpoint, non-terminal state or open work, none of which this fixture
/// has). No SQL is issued here, and the topic-subscription row is untouched —
/// `ordinary_topic_subscribers` joins `agents`, so a removed uid simply stops
/// being an eligible recipient.
fn stop_and_remove(fx: &Fx, alias: &str) {
    fx.shared
        .store
        .set_state_detached(alias, "stopped", None)
        .unwrap_or_else(|e| panic!("the fixture cannot stop '{alias}': {e}"));
    fx.shared
        .store
        .remove_agent(
            alias,
            false,
            &json!({"by": "operator", "by_kind": "operator"}),
        )
        .unwrap_or_else(|e| panic!("the fixture cannot remove '{alias}': {e}"));
}

/// Hand the now-free `alias` to `sub`'s personal assistant with the real
/// production writes: `assistant_bind` (legal only once the ordinary agents
/// row is gone) stamps the binding and its durable visibility fence, then the
/// member's runtime registers under the alias and the fence takes its one-time
/// runtime uid. Returns the new runtime uid.
fn hand_alias_to_member(fx: &Fx, sub: &str, alias: &str) -> String {
    fx.shared
        .store
        .assistant_bind(ORG, sub, MEMBER_ASSISTANT, alias, Some("rev0"))
        .unwrap_or_else(|e| panic!("the free alias '{alias}' could not be bound: {e}"));
    assert!(
        fx.shared
            .store
            .assistant_for_alias(alias)
            .unwrap()
            .is_some(),
        "the member binding did not land for the handed-over alias"
    );
    let new_uid = registered_peer(fx, alias);
    fx.shared
        .store
        .assistant_event_fence_bind_agent(ORG, sub, &new_uid)
        .unwrap_or_else(|e| panic!("the fence could not bind the runtime uid: {e}"));
    new_uid
}

/// The daemon-visible page bodies of `topic`, read as `who`.
fn topic_bodies(fx: &Fx, who: Asserted, topic: &str) -> Vec<String> {
    let page = fx
        .call(who, "topic_read", json!({"topic": topic, "limit": 50}))
        .unwrap_or_else(|e| panic!("topic_read was refused for the race fixture: {e}"));
    page["posts"]
        .as_array()
        .unwrap_or_else(|| panic!("topic_read returned no posts array: {page}"))
        .iter()
        .map(|post| {
            post["body"]
                .as_str()
                .unwrap_or_else(|| panic!("a topic post has no body: {post}"))
                .to_string()
        })
        .collect()
}

/// The exact refusal from an Ask (it must never be accepted, and it must name
/// no alias, uid, body, post id or request id).
fn refusal_text(answer: Result<Value>, case: &str, secrets: &[&str]) -> String {
    let error = match answer {
        Err(error) => error,
        Ok(value) => panic!("{case} was accepted: {value}"),
    };
    let text = error.to_string();
    for secret in secrets {
        assert!(
            !text.contains(*secret),
            "the refusal leaked '{secret}': {text}"
        );
    }
    text
}

/// One Ask driven on its own thread (the park is per-thread). The guard
/// releases the park and joins BEFORE a failed assertion can unwind past it,
/// so a red run cannot leave a parked daemon or a detached thread behind.
struct ParkedAsk {
    park: std::sync::Arc<crate::test_seam::AskCommitPark>,
    thread: Option<std::thread::JoinHandle<Result<Value>>>,
}

impl ParkedAsk {
    fn spawn(fx: &Fx, topic: &str) -> Self {
        let park = crate::test_seam::AskCommitPark::new();
        let ask_park = park.clone();
        let shared = fx.shared.clone();
        let ask_topic = topic.to_string();
        let thread = std::thread::spawn(move || {
            // The one-shot is installed on the Ask's own thread: no
            // process-global env or state-dir file, so concurrent fixtures
            // cannot observe this boundary.
            ask_park.arm();
            scoped(Asserted::Operator, || {
                shared.dispatch(
                    "topic_request",
                    &json!({
                        "topic": ask_topic,
                        "target": TARGET,
                        "text": ASK_BODY,
                        "id": ASK_ID
                    }),
                    std::process::id(),
                )
            })
        });
        Self {
            park,
            thread: Some(thread),
        }
    }

    /// Wait for the daemon's pre-commit park. The bound is a liveness cap
    /// only: the ordering never depends on elapsed time.
    fn wait_entered(&self, what: &str) {
        assert!(
            self.park.wait_entered(Duration::from_secs(30)),
            "the Ask never reached the pre-commit boundary: {what}"
        );
    }

    /// Let the parked Ask continue and take its answer.
    fn release_and_join(mut self) -> Result<Value> {
        self.park.release();
        let thread = self.thread.take().expect("the parked Ask thread vanished");
        thread.join().expect("the Ask thread panicked")
    }
}

impl Drop for ParkedAsk {
    fn drop(&mut self) {
        self.park.release();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// CAD-2207, positive control: the UNCHANGED head still serves an ordinary
/// `topic_request` and its idempotent retry. The post, the request message, the
/// request-post binding and the receipt all stay canonical, and the retry
/// mints nothing new.
#[test]
fn ordinary_ask_and_idempotent_retry_stay_served() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let target_uid = registered_peer(&fx, TARGET);
    let topic = fleet_topic(&fx, "cad2207-ask-race-control-topic", &[TARGET]);

    let first = fx
        .call(
            Asserted::Operator,
            "topic_request",
            json!({"topic": topic, "target": TARGET, "text": ASK_BODY, "id": ASK_ID}),
        )
        .expect("the operator lost the ordinary topic_request path");
    assert_eq!(first["duplicate"], false, "{first}");
    assert_eq!(first["target"]["alias"].as_str(), Some(TARGET), "{first}");
    assert_eq!(
        first["target"]["uid"].as_str(),
        Some(target_uid.as_str()),
        "{first}"
    );
    let post = first["message"]
        .as_str()
        .expect("the control Ask named no post")
        .to_string();
    let request = first["request"]
        .as_str()
        .expect("the control Ask named no request")
        .to_string();

    // The ordinary Ask is served end to end: transcript row, request-post
    // binding, queued request message and one operator-visible body.
    let stored = fx
        .shared
        .store
        .topic_post_by_id(&post)
        .unwrap()
        .expect("the control Ask wrote no topic post");
    assert_eq!(stored.sender, "operator", "{stored:?}");
    assert_eq!(stored.sender_uid, None, "{stored:?}");
    assert_eq!(stored.body, ASK_BODY, "{stored:?}");
    assert_eq!(
        fx.shared.store.topic_post_request(&post).unwrap(),
        Some(request.clone()),
        "the control Ask is not bound to its request"
    );
    let queued = fx
        .shared
        .store
        .message(&request)
        .unwrap()
        .expect("the control Ask queued no request message");
    assert_eq!(queued.alias, TARGET, "{queued:?}");
    assert!(
        queued.body.contains(ASK_BODY),
        "the control request does not carry the input: {queued:?}"
    );
    assert_eq!(
        fx.shared.store.messages(TARGET).unwrap().len(),
        1,
        "the control Ask queued more than one request"
    );
    assert_eq!(
        topic_bodies(&fx, Asserted::Operator, &topic),
        vec![ASK_BODY.to_string()],
        "the operator no longer reads an ordinary Ask body"
    );

    // ---- idempotent retry: the same envelope, nothing new -----------------
    let posts_after_first = fx
        .shared
        .store
        .topic_posts(&topic, None, None, 1000)
        .unwrap()
        .len();
    let mail_after_first: Vec<String> = fx
        .shared
        .store
        .messages(TARGET)
        .unwrap()
        .into_iter()
        .map(|message| message.id)
        .collect();
    let retried = fx
        .call(
            Asserted::Operator,
            "topic_request",
            json!({"topic": topic, "target": TARGET, "text": ASK_BODY, "id": ASK_ID}),
        )
        .expect("the ordinary idempotent retry was refused");
    assert_eq!(retried["duplicate"], true, "{retried}");
    assert_eq!(
        retried["message"].as_str(),
        Some(post.as_str()),
        "{retried}"
    );
    assert_eq!(
        retried["request"].as_str(),
        Some(request.as_str()),
        "{retried}"
    );
    assert_eq!(
        retried["target"]["uid"].as_str(),
        Some(target_uid.as_str()),
        "{retried}"
    );
    assert_eq!(
        fx.shared
            .store
            .topic_posts(&topic, None, None, 1000)
            .unwrap()
            .len(),
        posts_after_first,
        "the idempotent retry wrote another topic post"
    );
    let mail_after_retry: Vec<String> = fx
        .shared
        .store
        .messages(TARGET)
        .unwrap()
        .into_iter()
        .map(|message| message.id)
        .collect();
    assert_eq!(
        mail_after_retry, mail_after_first,
        "the idempotent retry queued another request"
    );
}

/// CAD-2207, the boundary acceptance. An ordinary subscribed target is
/// registered, subscribed and asked; the Ask parks at the pre-commit boundary;
/// the target is stopped, removed and handed to a member assistant with the
/// real production writes; the released Ask must refuse and leave no committed
/// post, delivery intent, queue row, binding or operator-visible body.
#[test]
fn ask_race_handover_before_queue_commit_leaves_no_operator_visible_body() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let original_uid = registered_peer(&fx, TARGET);
    registered_peer(&fx, PEER);
    let topic = fleet_topic(&fx, "cad2207-ask-race-topic", &[TARGET, PEER]);

    let post = crate::store::topic_post_id(&topic, ASK_ID);
    let request = crate::store::topic_request_id(&topic, ASK_ID);
    assert_eq!(
        fx.shared
            .store
            .topic_posts(&topic, None, None, 1000)
            .unwrap()
            .len(),
        0,
        "the race topic did not start empty"
    );

    // ---- start the Ask and park it before the queue commit ---------------
    let ask = ParkedAsk::spawn(&fx, &topic);
    ask.wait_entered(
        "without that park the hand-over cannot be ordered before the queue commit (CAD-2207)",
    );

    // The boundary is the last moment before the queue commit: the original
    // target is still the registered runtime, nothing has been queued or
    // bound yet. Whether the old two-transaction code has already committed
    // the post is RECORDED for diagnostics only — a correct atomic
    // implementation has no committed post here, and this acceptance never
    // requires one.
    assert_eq!(
        fx.shared.store.agent(TARGET).unwrap().uid,
        original_uid,
        "the target changed before the fixture handed it over — the park is not \
         the pre-commit boundary (CAD-2207)"
    );
    assert_eq!(
        fx.shared.store.topic_post_request(&post).unwrap(),
        None,
        "the Ask bound its request post before the pre-commit boundary (CAD-2207)"
    );
    assert!(
        fx.shared.store.message(&request).unwrap().is_none(),
        "the Ask queued its request before the pre-commit boundary (CAD-2207)"
    );
    let post_committed_at_boundary: bool =
        fx.shared.store.topic_post_by_id(&post).unwrap().is_some();

    // ---- the target leaves the fleet, and the alias becomes the member's --
    stop_and_remove(&fx, TARGET);
    assert!(
        fx.shared.store.agent_opt(TARGET).unwrap().is_none(),
        "the fixture left the ordinary target registered"
    );
    let new_uid = hand_alias_to_member(&fx, MEMBER, TARGET);
    assert_ne!(
        new_uid, original_uid,
        "the member runtime reused the removed runtime's uid"
    );

    // ---- let the Ask reach its queue commit; it must refuse --------------
    let text = refusal_text(
        ask.release_and_join(),
        "a topic_request whose target was handed to a member assistant before the queue commit",
        &[
            TARGET,
            original_uid.as_str(),
            new_uid.as_str(),
            ASK_BODY,
            post.as_str(),
            request.as_str(),
        ],
    );
    assert!(
        text == crate::store::RESTRICTED_LIFECYCLE || text == MOVED_TARGET,
        "the Ask was not refused by a target-authority check — both a moved-uid \
         and a member-lifecycle refusal are valid here, anything else is not: \
         {text}"
    );

    // ---- nothing operator-visible or queued survives the refusal ---------
    assert!(
        fx.shared.store.topic_post_by_id(&post).unwrap().is_none(),
        "the refused Ask left its topic post row behind — its body is written \
         with a NULL uid and the `operator` sender, so the generic visibility \
         predicate publishes it to every non-owner reader (CAD-2207; post \
         present at the boundary: {post_committed_at_boundary})"
    );
    assert!(
        fx.shared.store.topic_deliveries(&post).unwrap().is_empty(),
        "the refused Ask left a queued delivery intent for the now-private \
         target (CAD-2207; post present at the boundary: \
         {post_committed_at_boundary})"
    );
    assert_eq!(
        fx.shared.store.topic_post_request(&post).unwrap(),
        None,
        "the refused Ask left a request-post binding"
    );
    assert!(
        fx.shared.store.message(&request).unwrap().is_none(),
        "the refused Ask queued its request message"
    );
    assert!(
        fx.shared.store.messages(TARGET).unwrap().is_empty(),
        "the refused Ask put a row in the now-private target's mailbox"
    );

    // ---- unchanged-head positive control, same topic and same verb -------
    // The refusal must not poison the ordinary path or the operator's read.
    let control = fx
        .call(
            Asserted::Operator,
            "topic_request",
            json!({
                "topic": topic,
                "target": PEER,
                "text": CONTROL_BODY,
                "id": CONTROL_ID
            }),
        )
        .expect("the refused Ask poisoned the ordinary topic_request path");
    assert_eq!(control["duplicate"], false, "{control}");
    assert_eq!(control["target"]["alias"].as_str(), Some(PEER), "{control}");
    assert_eq!(
        topic_bodies(&fx, Asserted::Operator, &topic),
        vec![CONTROL_BODY.to_string()],
        "the operator transcript no longer shows exactly the ordinary control \
         Ask (CAD-2207; post present at the boundary: \
         {post_committed_at_boundary})"
    );
}

/// The same window without the hand-over: an ordinary Ask parked at the
/// pre-commit boundary and released unchanged is still served, so the
/// acceptance above cannot pass by making the boundary itself refuse every Ask.
#[test]
fn ask_race_park_without_handover_stays_served() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let target_uid = registered_peer(&fx, TARGET);
    let topic = fleet_topic(&fx, "cad2207-ask-race-unbound-topic", &[TARGET]);

    let ask = ParkedAsk::spawn(&fx, &topic);
    ask.wait_entered("the same window without a hand-over must still park the Ask");
    assert_eq!(
        fx.shared.store.agent(TARGET).unwrap().uid,
        target_uid,
        "the target changed while the Ask was parked without a hand-over"
    );

    let served = ask
        .release_and_join()
        .expect("an ordinary Ask parked at the pre-commit boundary was refused");
    assert_eq!(served["duplicate"], false, "{served}");
    assert_eq!(served["target"]["alias"].as_str(), Some(TARGET), "{served}");
    assert_eq!(
        served["target"]["uid"].as_str(),
        Some(target_uid.as_str()),
        "{served}"
    );
    let post = served["message"]
        .as_str()
        .expect("the parked ordinary Ask named no post")
        .to_string();
    let request = served["request"]
        .as_str()
        .expect("the parked ordinary Ask named no request")
        .to_string();
    assert_eq!(
        fx.shared.store.topic_post_request(&post).unwrap(),
        Some(request.clone()),
        "the parked ordinary Ask did not bind its request"
    );
    assert_eq!(
        fx.shared.store.message(&request).unwrap().map(|m| m.alias),
        Some(TARGET.to_string()),
        "the parked ordinary Ask did not queue its request"
    );
    assert_eq!(
        topic_bodies(&fx, Asserted::Operator, &topic),
        vec![ASK_BODY.to_string()],
        "the parked ordinary Ask is not visible to the operator"
    );
}
