//! CAD-2259 acceptance, written from the ticket by someone other than
//! the implementer. It drives `Shared::send_hosted_kickoff`, the send
//! the daemon's hosted workspace worker makes for every kickoff to its
//! bound default agent, against a real store.
//!
//! A default agent the operator must look at first (`attention`, or
//! holding an `unknown` message that fences it) refuses the kickoff with
//! `default_agent_unavailable`, names the alias, and queues nothing. A
//! default that is not fenced, including one whose only `unknown` row is
//! a nudge, still gets the kickoff queued like any plain send.

use super::*;

/// `alias` registered and opened to a live endpoint, so it reads
/// `idle` the way the daemon leaves it after a successful open.
fn idle_default(shared: &Arc<Shared>, dir: &Path, alias: &str, generation: &str) {
    register(shared, dir, alias);
    open_endpoint(shared, alias, generation);
    assert_eq!(shared.store.agent(alias).unwrap().state, "idle");
}

fn open_endpoint(shared: &Arc<Shared>, alias: &str, generation: &str) {
    shared
        .store
        .set_identity(
            alias,
            &adapter::Identity {
                thread_id: format!("thread-{alias}"),
                session_id: format!("session-{alias}"),
                model: None,
                effort: None,
                pid: 4242,
                endpoint: Some(format!("native://{alias}")),
                generation: Some(generation.to_string()),
                attach: None,
            },
        )
        .unwrap();
}

/// A kickoff the agent has started: queued, claimed, and running under
/// `turn`.
fn running_kickoff(shared: &Arc<Shared>, alias: &str, id: &str, turn: &str) {
    shared
        .store
        .enqueue(alias, "earlier work", None, id, "user")
        .unwrap();
    let Take::Message(message) = shared.store.take_queued(alias).unwrap() else {
        panic!("{alias}: the earlier kickoff must be claimed");
    };
    assert_eq!(message.id, id);
    shared.store.mark_running(id, turn).unwrap();
}

/// The kickoff exactly as the workspace worker hands it to the daemon.
fn kickoff(alias: &str, id: &str) -> Value {
    json!({
        "alias": alias,
        "text": kickoff_text(id),
        "message": id,
    })
}

fn kickoff_text(id: &str) -> String {
    format!("Hosted assignment {id}: start the ticket in your lane.")
}

/// Every row the alias holds, by id and state.
fn rows(shared: &Arc<Shared>, alias: &str) -> Vec<(String, String)> {
    shared
        .store
        .messages(alias)
        .unwrap()
        .into_iter()
        .map(|m| (m.id, m.state))
        .collect()
}

fn assert_refused_and_nothing_queued(shared: &Arc<Shared>, alias: &str, id: &str) {
    let before = rows(shared, alias);
    let queued = shared.store.queued_count(alias).unwrap();

    let error = shared
        .send_hosted_kickoff(&kickoff(alias, id))
        .expect_err("a kickoff to a fenced default agent must be refused");
    assert_eq!(error.code(), Some("default_agent_unavailable"), "{error}");
    assert_eq!(error.kind(), "rejected", "{error}");
    assert!(error.to_string().contains(alias), "{error}");

    assert!(
        shared.store.message(id).unwrap().is_none(),
        "the refused kickoff must not be stored"
    );
    assert_eq!(rows(shared, alias), before);
    assert_eq!(shared.store.queued_count(alias).unwrap(), queued);
}

fn assert_queued_for(shared: &Arc<Shared>, alias: &str, id: &str) {
    let queued = shared.store.queued_count(alias).unwrap();

    let receipt = shared
        .send_hosted_kickoff(&kickoff(alias, id))
        .unwrap_or_else(|e| panic!("a kickoff to a usable default must be queued: {e}"));
    assert_eq!(receipt["message"], id, "{receipt}");
    assert_eq!(receipt["duplicate"], false, "{receipt}");

    let stored = shared
        .store
        .message(id)
        .unwrap()
        .expect("the kickoff must be stored");
    assert_eq!(stored.alias, alias);
    assert_eq!(stored.state, "queued");
    assert_eq!(stored.body, kickoff_text(id));
    assert_eq!(shared.store.queued_count(alias).unwrap(), queued + 1);
}

#[test]
fn cad2259_acceptance_an_attention_default_refuses_the_kickoff_and_queues_nothing() {
    let (dir, shared) = shared();
    let alias = "roster-fenced-a";
    idle_default(&shared, dir.path(), alias, "gen-1");
    // Work already waiting, as on the operator's host: it must stay as it is.
    shared
        .store
        .enqueue(alias, "older kickoff", None, "older-1", "user")
        .unwrap();
    shared
        .store
        .set_state_detached(alias, "attention", Some("Provider connection is closed"))
        .unwrap();
    assert_eq!(shared.store.agent(alias).unwrap().state, "attention");
    assert!(!shared.store.has_unknown(alias).unwrap());
    assert_eq!(shared.store.queued_count(alias).unwrap(), 1);

    assert_refused_and_nothing_queued(&shared, alias, "hla-kickoff-attention");
}

#[test]
fn cad2259_acceptance_an_idle_default_fenced_by_an_unknown_turn_refuses_the_kickoff() {
    let (dir, shared) = shared();
    let alias = "roster-fenced-b";
    idle_default(&shared, dir.path(), alias, "gen-1");
    running_kickoff(&shared, alias, "earlier-b", "turn-b");
    // The endpoint is reopened mid-turn: the in-flight turn becomes
    // `unknown` and the agent reads `idle` again.
    open_endpoint(&shared, alias, "gen-2");
    assert_eq!(shared.store.agent(alias).unwrap().state, "idle");
    assert_eq!(
        shared.store.message("earlier-b").unwrap().unwrap().state,
        "unknown"
    );
    assert!(shared.store.has_unknown(alias).unwrap());

    assert_refused_and_nothing_queued(&shared, alias, "hla-kickoff-unknown");
}

#[test]
fn cad2259_acceptance_an_idle_default_with_only_an_unknown_nudge_still_takes_the_kickoff() {
    let (dir, shared) = shared();
    let alias = "roster-live-c";
    idle_default(&shared, dir.path(), alias, "gen-1");
    running_kickoff(&shared, alias, "earlier-c", "turn-c");
    let agent = shared.store.agent(alias).unwrap();
    let (duplicate, state, bound) = shared
        .store
        .begin_native_nudge(
            &agent,
            "steer",
            "nudge-c",
            &store::Sender::Unattributed,
            &store::Steer::NONE,
        )
        .unwrap();
    assert!(!duplicate);
    assert_eq!(state, "submitting");
    assert_eq!(bound.as_deref(), Some("turn-c"));
    shared
        .store
        .finish_native_nudge("nudge-c", "unknown", Some("the provider did not answer"))
        .unwrap();
    let earlier = shared.store.message("earlier-c").unwrap().unwrap();
    shared
        .store
        .finish(&earlier, "completed", &json!({"text": "done"}), None)
        .unwrap();
    let nudge = shared.store.message("nudge-c").unwrap().unwrap();
    assert_eq!(
        (nudge.state.as_str(), nudge.source.as_str()),
        ("unknown", "nudge")
    );
    assert_eq!(shared.store.agent(alias).unwrap().state, "idle");
    assert!(!shared.store.has_unknown(alias).unwrap());

    assert_queued_for(&shared, alias, "hla-kickoff-nudge");
}

#[test]
fn cad2259_acceptance_a_healthy_idle_default_takes_the_kickoff() {
    let (dir, shared) = shared();
    let alias = "roster-live-d";
    idle_default(&shared, dir.path(), alias, "gen-1");

    assert_queued_for(&shared, alias, "hla-kickoff-healthy");
}
