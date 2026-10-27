// Independently authored CAD-1085 acceptance for atomic room Ask queue binding.
use crate::error::Result;
use crate::store::human_threads::{RoomMember, VerifiedHuman};
use crate::store::{NewAgent, Sender, Steer, Store};

fn register(store: &Store, alias: &str, cwd: &std::path::Path) {
    store
        .register_agent(&NewAgent {
            alias,
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
        .unwrap();
}

fn queue_ask(
    store: &Store,
    alias: &str,
    expected_uid: &str,
    request: &str,
    post: &str,
    body: &str,
) -> Result<(bool, String)> {
    store.enqueue_steered_for_with_conversation(
        alias,
        Some(expected_uid),
        body,
        None,
        request,
        "operator",
        None,
        None,
        None,
        &Sender::OperatorChat,
        &Steer::NONE,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(post),
    )
}

fn queued_events(store: &Store, alias: &str, request: &str) -> usize {
    store
        .events(alias, 0, 200)
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.kind == "queued" && event.payload["message"].as_str() == Some(request)
        })
        .count()
}

#[test]
fn room_ask_binding_commits_with_queue_and_refusals_preserve_retry_identity() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("room-queue-acceptance.sqlite3")).unwrap();
    register(&store, "ask-worker", root.path());
    register(&store, "ask-other", root.path());
    let worker_uid = store.agent("ask-worker").unwrap().uid;
    let other_uid = store.agent("ask-other").unwrap().uid;
    let human =
        VerifiedHuman::verified("acceptance-issuer", "acceptance-org", "acceptance-human").unwrap();
    let room = store
        .human_group_create(
            &human,
            "mixed_group",
            "Atomic Ask acceptance",
            "atomic-ask-acceptance",
            &[
                RoomMember::Agent("ask-worker".to_string()),
                RoomMember::Agent("ask-other".to_string()),
            ],
        )
        .unwrap();
    let (topic, alias) = store
        .human_room_ask_target(&human, &room, &worker_uid)
        .unwrap();
    assert_eq!(alias, "ask-worker");

    let client = "ask-positive";
    let (post, fresh) = store
        .human_room_ask_post(&human, &room, &worker_uid, client, "Please read this room.")
        .unwrap();
    assert!(fresh);
    let request = crate::store::topic_request_id(&topic, client);
    let body = "Read the metadata-only Ask in its private room.";
    assert!(store.topic_post_request(&post.id).unwrap().is_none());
    assert!(store.message(&request).unwrap().is_none());

    let first = queue_ask(&store, "ask-worker", &worker_uid, &request, &post.id, body).unwrap();
    assert!(!first.0);
    assert_eq!(first.1, "queued");
    assert_eq!(
        store.topic_post_request(&post.id).unwrap().as_deref(),
        Some(request.as_str())
    );
    assert_eq!(
        store.human_room_for_request(&request).unwrap().as_deref(),
        Some(room.as_str())
    );
    assert_eq!(
        store
            .message(&request)
            .unwrap()
            .unwrap()
            .recipient_uid
            .as_deref(),
        Some(worker_uid.as_str())
    );
    assert_eq!(queued_events(&store, "ask-worker", &request), 1);

    let repeated = queue_ask(&store, "ask-worker", &worker_uid, &request, &post.id, body).unwrap();
    assert!(repeated.0);
    assert_eq!(repeated.1, first.1);
    assert_eq!(queued_events(&store, "ask-worker", &request), 1);
    assert_eq!(
        store.human_room_for_request(&request).unwrap().as_deref(),
        Some(room.as_str())
    );

    // Existing-envelope retries cannot change either content or recipient.
    assert!(queue_ask(
        &store,
        "ask-worker",
        &worker_uid,
        &request,
        &post.id,
        "Changed envelope"
    )
    .is_err());
    assert!(queue_ask(&store, "ask-other", &other_uid, &request, &post.id, body).is_err());
    assert_eq!(
        store
            .message(&request)
            .unwrap()
            .unwrap()
            .recipient_uid
            .as_deref(),
        Some(worker_uid.as_str())
    );
    assert_eq!(
        store.topic_post_request(&post.id).unwrap().as_deref(),
        Some(request.as_str())
    );
    assert_eq!(queued_events(&store, "ask-worker", &request), 1);
    assert_eq!(queued_events(&store, "ask-other", &request), 0);

    // A real pre-existing conflicting post binding must roll back the newly
    // enqueued message AND its event, not fail after committing the queue.
    let conflict_client = "ask-conflicting-binding";
    let (conflict_post, _) = store
        .human_room_ask_post(
            &human,
            &room,
            &worker_uid,
            conflict_client,
            "Keep the existing binding.",
        )
        .unwrap();
    store
        .topic_post_bind_request(&conflict_post.id, &request)
        .unwrap();
    let conflict_request = crate::store::topic_request_id(&topic, conflict_client);
    assert!(queue_ask(
        &store,
        "ask-worker",
        &worker_uid,
        &conflict_request,
        &conflict_post.id,
        body,
    )
    .is_err());
    assert!(store.message(&conflict_request).unwrap().is_none());
    assert_eq!(queued_events(&store, "ask-worker", &conflict_request), 0);
    assert_eq!(
        store
            .topic_post_request(&conflict_post.id)
            .unwrap()
            .as_deref(),
        Some(request.as_str())
    );
    assert!(store
        .human_room_for_request(&conflict_request)
        .unwrap()
        .is_none());
    assert!(store.topic_post_by_id(&conflict_post.id).unwrap().is_some());

    // The selected UID is a trusted enqueue precondition. A mismatched UID
    // leaves the original post unbound and retryable under the SAME request ID.
    let retry_client = "ask-retry-after-recipient-refusal";
    let retry_body = "Retry this same Ask only after the recipient is current.";
    let (retry_post, _) = store
        .human_room_ask_post(&human, &room, &worker_uid, retry_client, retry_body)
        .unwrap();
    let retry_request = crate::store::topic_request_id(&topic, retry_client);
    assert!(queue_ask(
        &store,
        "ask-worker",
        &other_uid,
        &retry_request,
        &retry_post.id,
        body,
    )
    .is_err());
    assert!(store.message(&retry_request).unwrap().is_none());
    assert_eq!(queued_events(&store, "ask-worker", &retry_request), 0);
    assert!(store.topic_post_request(&retry_post.id).unwrap().is_none());
    assert!(store
        .human_room_for_request(&retry_request)
        .unwrap()
        .is_none());
    let (same_post, fresh) = store
        .human_room_ask_post(&human, &room, &worker_uid, retry_client, retry_body)
        .unwrap();
    assert!(!fresh);
    assert_eq!(same_post.id, retry_post.id);
    assert_eq!(
        crate::store::topic_request_id(&topic, retry_client),
        retry_request
    );

    let recovered = queue_ask(
        &store,
        "ask-worker",
        &worker_uid,
        &retry_request,
        &same_post.id,
        body,
    )
    .unwrap();
    assert!(!recovered.0);
    assert_eq!(
        store.topic_post_request(&same_post.id).unwrap().as_deref(),
        Some(retry_request.as_str())
    );
    assert_eq!(
        store
            .human_room_for_request(&retry_request)
            .unwrap()
            .as_deref(),
        Some(room.as_str())
    );
    let duplicate = queue_ask(
        &store,
        "ask-worker",
        &worker_uid,
        &retry_request,
        &same_post.id,
        body,
    )
    .unwrap();
    assert!(duplicate.0);
    assert_eq!(duplicate.1, recovered.1);
    assert_eq!(queued_events(&store, "ask-worker", &retry_request), 1);
    assert_eq!(
        store
            .message(&retry_request)
            .unwrap()
            .unwrap()
            .recipient_uid
            .as_deref(),
        Some(worker_uid.as_str())
    );
}
