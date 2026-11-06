//! CAD-2207 topic privacy acceptance — independent author.
//!
//! One MIXED fleet topic (an ordinary peer plus a member's bound personal
//! assistant) pins the dispatcher boundary for the legacy topic verbs:
//!
//! - a generic operator (non-owner by construction) cannot subscribe or
//!   unsubscribe the bound assistant, and the refusal names neither the alias
//!   nor its uid;
//! - the bound assistant's own process cannot publish into the shared topic —
//!   no post, no delivery intent, and no topic notice in either agent's
//!   mailbox;
//! - a non-owner read of the topic does not carry the assistant's
//!   post-binding post: body, post id, author row or its delivery receipt;
//! - the operator's group list (`work_conversation_list`) does not move its
//!   `posts` count or `last_activity` when a private post appears, so a
//!   private-only update cannot be inferred from the aggregate;
//! - provenance decides, not time: the operator-authored shared post (a
//!   durable ordinary identity) stays readable, while a null-UID alias post is
//!   AMBIGUOUS legacy history — CAD-2207 withholds it rather than trusting a
//!   `created` ordering — and an ordinary peer's read, pagination and
//!   membership management keep working;
//! - a REMOVED ordinary agent's post stays readable through its stored durable
//!   sender uid, and the work-list `posts` count is the VISIBLE post count —
//!   not a sequence moved by hidden posts or by another topic's intervening
//!   posts;
//! - `topic_request {target}` addressed to the bound assistant is refused with
//!   no post, request, receipt, queue entry or notice, while the same request
//!   to an ordinary subscriber succeeds (a bind racing in AFTER the precheck is
//!   NOT simulated here: no genuine interleaving seam exists without a
//!   production transaction hook);
//! - the STORE itself refuses a stale-snapshot caller that reaches it after the
//!   bind: `topic_publish` as the bound sender and `topic_subscribe` /
//!   `topic_unsubscribe` for its uid change no post, delivery or membership,
//!   while ordinary store calls keep working. The live concurrent bind race is
//!   not simulated for the same missing-barrier reason;
//! - an AMBIGUOUS Ask receipt (the bound request message is missing its
//!   recipient uid OR its recipient label) is withheld from every reader: the
//!   ordinary post body stays visible but its request handle and receipt/state
//!   do not, while a fully identified ordinary receipt remains readable;
//! - a duplicate `topic_request` retry keeps its stable duplicate for an
//!   ordinary target and refuses, naming no alias/uid/handle and mutating
//!   nothing, once that same target is bound (stable binding only; no live
//!   check/receipt race is simulated);
//! - a real job transition's topic fan-out notifies an ordinary subscriber but
//!   skips a bound assistant's legacy subscription (no delivery, no mailbox
//!   notice), while the PM `job_event` queue is unchanged;
//! - the STORE queue transaction rechecks a generic-topic `request_post`: a
//!   target that became bound after the caller's precheck is refused with no
//!   message row and no post/request binding, while an ordinary target still
//!   queues and binds. This pins the stale-target end state; it does NOT claim
//!   the separate post-insert/queue split is atomic.
//!
//! The fixture writes an ambiguous legacy post before the binding exists (no
//! durable sender uid, and a stored sender that is now the binding alias),
//! which the guard must withhold even though it predates the binding; the
//! post-binding private post follows it. The pre-binding subscription is a
//! backdated row: a uid does not exist before the agent is registered, so no
//! public verb can create it. Those rows are written through the store's
//! test-only `fixture_write` facade, never through a production writer, and no
//! publish runs while the assistant is subscribed — so fixture setup never
//! queues a private post into an ordinary agent's mailbox.
//!
//! Scope, stated narrowly: this is the LOCAL topic dispatcher boundary. It
//! makes no claim about OS-user custody or a signed owner-authorized topic
//! route, and it does not invent such consent: every private post here is
//! post-binding context that no member shared.

use super::*;

const ORDINARY: &str = "cad2207-topic-peer";
const SHARED_BODY: &str = "cad2207-shared-update";
const OLD_SHARED_BODY: &str = "cad2207-old-shared-post";
const PRIVATE_BODY: &str = "cad2207-private-topic-body";
const PRIVATE_ATTEMPT: &str = "cad2207-assistant-publish-body";
const PRIVATE_POST_ID: &str = "cad2207-topic-private-post";

/// Register a plain managed agent up front and return its durable uid. The
/// topic verbs resolve an alias through the registry, so the ordinary peer
/// must exist before the topic is created/subscribed; this is the same
/// registration the fixture's lazy worker gets, just not deferred.
fn ordinary_peer(fx: &Fx, alias: &str) -> String {
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

/// The group row for `topic` in the operator's work-conversation list.
fn group_row<'a>(list: &'a Value, topic: &str) -> &'a Value {
    list["conversations"]
        .as_array()
        .unwrap_or_else(|| panic!("work_conversation_list is not a list: {list}"))
        .iter()
        .find(|row| row["kind"] == "group" && row["id"].as_str() == Some(topic))
        .unwrap_or_else(|| panic!("the mixed topic has no group row: {list}"))
}

/// The decoded bodies of one `topic_read` page, oldest first.
fn bodies(page: &Value) -> Vec<String> {
    page["posts"]
        .as_array()
        .unwrap_or_else(|| panic!("topic_read has no posts: {page}"))
        .iter()
        .map(|post| {
            post["body"]
                .as_str()
                .unwrap_or_else(|| panic!("post has no body: {post}"))
                .to_string()
        })
        .collect()
}

/// Every post body reachable by paging the filtered topic, and whether any
/// page carried the private post.
fn paged_bodies(fx: &Fx, topic: &str, as_agent: Option<&str>) -> Vec<String> {
    let read = |params: Value| match as_agent {
        Some(alias) => fx.call(Asserted::Agent(alias.to_string()), "topic_read", params),
        None => fx.call(Asserted::Operator, "topic_read", params),
    };
    let mut before: Option<i64> = None;
    let mut out = Vec::new();
    for _ in 0..4 {
        let mut params = json!({"topic": topic, "limit": 1});
        if let Some(before) = before {
            params["before"] = json!(before);
        }
        let page = read(params).expect("a topic page was refused");
        let mut page_bodies = bodies(&page);
        out.append(&mut page_bodies);
        if page["more_before"].as_bool() != Some(true) {
            return out;
        }
        let cursor = page["cursor"].as_i64().expect("a page has no cursor");
        assert!(cursor > 0, "a page with more_before had no positive cursor");
        before = Some(cursor);
    }
    panic!("pagination did not terminate over the visible topic history");
}

#[test]
fn mixed_topic_hides_the_bound_assistants_private_context_from_operators() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    // The ordinary peer must exist BEFORE the topic verbs resolve it — the
    // fixture's own `ordinary-worker` is registered lazily by
    // `member_runtime`, which runs after the ambiguous post must be seeded.
    let ordinary_uid = ordinary_peer(&fx, ORDINARY);

    // ---- the shared topic and its ordinary peer --------------------------
    let created = fx
        .call(
            Asserted::Operator,
            "topic_create",
            json!({"type": "fleet", "source": "cad2207-mixed-topic"}),
        )
        .expect("the operator could not create the mixed topic");
    let topic = created["topic"]
        .as_str()
        .expect("topic_create named no topic")
        .to_string();
    let subscribed = fx
        .call(
            Asserted::Operator,
            "topic_subscribe",
            json!({"topic": topic, "alias": ORDINARY}),
        )
        .expect("the operator could not subscribe the ordinary peer");
    assert_eq!(subscribed["alias"], ORDINARY, "{subscribed}");
    assert_eq!(
        subscribed["uid"].as_str(),
        Some(ordinary_uid.as_str()),
        "the ordinary subscription returned another uid: {subscribed}"
    );
    let shared = fx
        .call(
            Asserted::Operator,
            "topic_publish",
            json!({"topic": topic, "text": SHARED_BODY, "id": "cad2207-shared-update"}),
        )
        .expect("the operator could not publish the shared update");
    assert_eq!(shared["duplicate"], false, "{shared}");
    let shared_post = shared["message"]
        .as_str()
        .expect("the shared publish named no post")
        .to_string();

    // ---- ambiguous legacy history: no durable sender uid -----------------
    // The alias has no agents row yet, so the sender uid is NULL and the
    // stored sender is the future binding alias. CAD-2207 treats that as
    // UNPROVED provenance (alias reuse is possible), not as ordinary
    // authorship, so the guard must withhold it regardless of `created`. No
    // subscription exists yet, so this writes no delivery and no mailbox
    // notice.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO topic_posts(id,topic_id,sender_uid,sender,body,created) \
                 VALUES(?,?,NULL,?,?,?)",
                rusqlite::params![
                    "cad2207-topic-old-post",
                    topic,
                    "assist-agent-alice",
                    OLD_SHARED_BODY,
                    crate::issue::time::now_epoch() as f64
                ],
            )?;
            Ok(())
        })
        .expect("the fixture cannot seed the ambiguous legacy post");

    // ---- the binding ------------------------------------------------------
    let alice = fx.member_runtime("sub-alice");
    let binding = fx
        .shared
        .store
        .assistant_binding(ORG, "sub-alice")
        .unwrap()
        .expect("the fixture binding vanished");
    let assistant_uid = fx.shared.store.agent(&alice.alias).unwrap().uid;

    // The pre-binding subscription: a uid cannot exist before registration,
    // so the row is placed with `added` before the binding's own creation.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO topic_subscriptions(topic_id,uid,added) VALUES(?,?,?)",
                rusqlite::params![topic, assistant_uid, binding.created - 60.0],
            )?;
            Ok(())
        })
        .expect("the fixture cannot seed the pre-binding subscription");

    // The operator's aggregate BEFORE the private post: the already-shared
    // view the privacy guard must not move.
    let list_before = fx
        .call(Asserted::Operator, "work_conversation_list", json!({}))
        .expect("the operator's work list was refused");
    let row_before = group_row(&list_before, &topic).clone();

    // ---- post-binding private context -------------------------------------
    // One post by the bound assistant, plus the delivery intent the vulnerable
    // publish would have written for the ordinary peer. The intent carries no
    // message id: setup must not queue a private notice into any mailbox.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO topic_posts(id,topic_id,sender_uid,sender,body,created) \
                 VALUES(?,?,?,?,?,?)",
                rusqlite::params![
                    PRIVATE_POST_ID,
                    topic,
                    assistant_uid,
                    alice.alias,
                    PRIVATE_BODY,
                    crate::issue::time::now_epoch() as f64
                ],
            )?;
            tx.execute(
                "INSERT INTO topic_deliveries(post_id,uid,alias,message_id,state) \
                 VALUES(?,?,?,NULL,'pending')",
                rusqlite::params![PRIVATE_POST_ID, ordinary_uid, ORDINARY],
            )?;
            Ok(())
        })
        .expect("the fixture cannot seed the post-binding private post");

    let posts_before = fx
        .shared
        .store
        .topic_posts(&topic, None, None, 1000)
        .unwrap()
        .len();
    let members_before = fx.shared.store.topic_subscribers(&topic).unwrap();
    let ordinary_mail_before: Vec<String> = fx
        .shared
        .store
        .messages(ORDINARY)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    let assistant_mail_before: Vec<String> = fx
        .shared
        .store
        .messages(&alice.alias)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    let tree_before = fx.tree();

    // A generic operator cannot mutate the private assistant's membership,
    // and the refusal names neither the alias nor the uid.
    for subscribe in [true, false] {
        let verb = if subscribe {
            "topic_subscribe"
        } else {
            "topic_unsubscribe"
        };
        let error = fx
            .call(
                Asserted::Operator,
                verb,
                json!({"topic": topic, "alias": alice.alias}),
            )
            .expect_err("a generic operator mutated a member assistant's topic membership");
        let text = error.to_string();
        assert!(
            !text.contains(alice.alias.as_str()) && !text.contains(assistant_uid.as_str()),
            "{verb} named the private assistant in its refusal: {text}"
        );
    }
    assert_eq!(
        fx.shared.store.topic_subscribers(&topic).unwrap(),
        members_before,
        "a refused membership change moved the topic's subscribers"
    );

    // The bound assistant's own process cannot publish into the mixed topic:
    // no post, no delivery, no notice in either mailbox.
    let error = fx
        .call(
            Asserted::Agent(alice.alias.clone()),
            "topic_publish",
            json!({"topic": topic, "text": PRIVATE_ATTEMPT, "id": "cad2207-assistant-publish"}),
        )
        .expect_err("a bound assistant published into the shared topic");
    let text = error.to_string();
    assert!(
        !text.contains(alice.alias.as_str())
            && !text.contains(assistant_uid.as_str())
            && !text.contains(PRIVATE_ATTEMPT),
        "the publish refusal leaked a private identifier or the input: {text}"
    );
    assert_eq!(
        fx.shared
            .store
            .topic_posts(&topic, None, None, 1000)
            .unwrap()
            .len(),
        posts_before,
        "a refused assistant publish wrote a topic post"
    );
    assert_eq!(
        fx.shared
            .store
            .topic_deliveries(PRIVATE_POST_ID)
            .unwrap()
            .len(),
        1,
        "a refused assistant publish wrote another delivery"
    );
    let ordinary_mail_after: Vec<String> = fx
        .shared
        .store
        .messages(ORDINARY)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    let assistant_mail_after: Vec<String> = fx
        .shared
        .store
        .messages(&alice.alias)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(
        ordinary_mail_after, ordinary_mail_before,
        "a refused assistant publish queued a topic notice in the ordinary peer's mailbox"
    );
    assert_eq!(
        assistant_mail_after, assistant_mail_before,
        "a refused assistant publish queued a notice in the assistant's mailbox"
    );
    assert_eq!(
        fx.tree(),
        tree_before,
        "a refused topic mutation changed the tracker"
    );

    // ---- the operator read: shared history visible, private context not --
    let read = fx
        .call(Asserted::Operator, "topic_read", json!({"topic": topic}))
        .expect("the operator's topic read was refused");
    let shown = bodies(&read);
    assert!(
        shown.iter().any(|body| body == SHARED_BODY),
        "the operator lost the proven-ordinary shared post: {read}"
    );
    assert!(
        !shown.iter().any(|body| body == OLD_SHARED_BODY),
        "the operator read an UNPROVED legacy post (null sender uid) as ordinary history: {read}"
    );
    assert!(
        !read.to_string().contains(OLD_SHARED_BODY),
        "the operator read surfaced the ambiguous legacy body: {read}"
    );
    assert!(
        !shown.iter().any(|body| body == PRIVATE_BODY),
        "the operator read the post-binding private body: {read}"
    );
    assert!(
        !read.to_string().contains(PRIVATE_POST_ID) && !read.to_string().contains(PRIVATE_BODY),
        "the operator read named the private post or its body: {read}"
    );
    for post in read["posts"].as_array().unwrap() {
        for receipt in post["deliveries"].as_array().unwrap_or(&Vec::new()) {
            assert!(
                !(receipt["alias"].as_str() == Some(ORDINARY)
                    && receipt["state"].as_str() == Some("pending")),
                "the operator read a private delivery receipt: {post}"
            );
        }
    }

    // The ambiguous legacy post and the private post must not leak through
    // pagination either.
    let paged = paged_bodies(&fx, &topic, None);
    assert!(
        paged.iter().any(|body| body == SHARED_BODY)
            && !paged.iter().any(|body| body == OLD_SHARED_BODY)
            && !paged.iter().any(|body| body == PRIVATE_BODY),
        "the operator's pages did not match the proven-ordinary history: {paged:?}"
    );

    // The ordinary peer keeps the same filtered view.
    let peer = fx
        .call(
            Asserted::Agent(ORDINARY.to_string()),
            "topic_read",
            json!({"topic": topic}),
        )
        .expect("the subscribed ordinary peer lost its topic read");
    let peer_bodies = bodies(&peer);
    assert!(
        peer_bodies.iter().any(|body| body == SHARED_BODY)
            && !peer_bodies.iter().any(|body| body == OLD_SHARED_BODY)
            && !peer_bodies.iter().any(|body| body == PRIVATE_BODY),
        "the ordinary peer's view is wrong: {peer}"
    );

    // ---- the aggregate: a private-only update is not inferable ----------
    let list_after = fx
        .call(Asserted::Operator, "work_conversation_list", json!({}))
        .expect("the operator's work list was refused");
    let row_after = group_row(&list_after, &topic);
    assert_eq!(
        row_after["posts"], row_before["posts"],
        "the group list's post count moved on a private-only update: {row_after}"
    );
    assert_eq!(
        row_after["last_activity"], row_before["last_activity"],
        "the group list's last_activity moved on a private-only update: {row_after}"
    );
    assert!(
        !row_after["participants"]
            .to_string()
            .contains(assistant_uid.as_str())
            && !row_after["participants"]
                .to_string()
                .contains(alice.alias.as_str()),
        "the group row listed the private assistant as a member: {row_after}"
    );
    assert!(
        !list_after.to_string().contains(PRIVATE_BODY),
        "the work list carried the private body: {list_after}"
    );

    // ---- ordinary controls: membership and shared posts keep working ----
    let off = fx
        .call(
            Asserted::Operator,
            "topic_unsubscribe",
            json!({"topic": topic, "alias": ORDINARY}),
        )
        .expect("the operator lost the ordinary unsubscribe path");
    assert_eq!(off["subscribed"], false, "{off}");
    assert_eq!(off["alias"], ORDINARY, "{off}");
    assert_eq!(off["uid"].as_str(), Some(ordinary_uid.as_str()), "{off}");
    let on = fx
        .call(
            Asserted::Operator,
            "topic_subscribe",
            json!({"topic": topic, "alias": ORDINARY}),
        )
        .expect("the operator lost the ordinary subscribe path");
    assert_eq!(on["subscribed"], true, "{on}");
    assert_eq!(on["uid"].as_str(), Some(ordinary_uid.as_str()), "{on}");
    let shared_read = fx
        .call(
            Asserted::Operator,
            "topic_read",
            json!({"topic": topic, "after": 0, "limit": 10}),
        )
        .expect("the shared topic read was refused");
    assert!(
        shared_read.to_string().contains(shared_post.as_str())
            || shared_read["posts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|post| post["id"].as_str() == Some(shared_post.as_str())),
        "the shared post is missing from the topic: {shared_read}"
    );
}

/// CAD-2207: an operator's ORDINARY update on a topic that also has a
/// previously subscribed personal assistant must reach only the ordinary
/// peer. The private assistant gets no delivery intent, no queued notice and
/// no mailbox message, and the publish answer names neither its alias nor its
/// uid. The assistant's subscription is seeded directly (the operator is not
/// authorized to create it); this is the "already subscribed" case, and it
/// does not depend on the first test's baseline timing.
#[test]
fn ordinary_publish_skips_the_subscribed_private_assistant() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let ordinary_uid = ordinary_peer(&fx, ORDINARY);
    const UPDATE: &str = "cad2207-ordinary-topic-update";

    let created = fx
        .call(
            Asserted::Operator,
            "topic_create",
            json!({"type": "fleet", "source": "cad2207-ordinary-publish"}),
        )
        .expect("the operator could not create the topic");
    let topic = created["topic"]
        .as_str()
        .expect("topic_create named no topic")
        .to_string();
    fx.call(
        Asserted::Operator,
        "topic_subscribe",
        json!({"topic": topic, "alias": ORDINARY}),
    )
    .expect("the operator could not subscribe the ordinary peer");

    let alice = fx.member_runtime("sub-alice");
    let binding = fx
        .shared
        .store
        .assistant_binding(ORG, "sub-alice")
        .unwrap()
        .expect("the fixture binding vanished");
    let assistant_uid = fx.shared.store.agent(&alice.alias).unwrap().uid;
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO topic_subscriptions(topic_id,uid,added) VALUES(?,?,?)",
                rusqlite::params![topic, assistant_uid, binding.created - 60.0],
            )?;
            Ok(())
        })
        .expect("the fixture cannot seed the assistant's subscription");

    let mailbox = |alias: &str| -> Vec<String> {
        fx.shared
            .store
            .messages(alias)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect()
    };
    let ordinary_mail = mailbox(ORDINARY);
    let assistant_mail = mailbox(&alice.alias);

    let published = fx
        .call(
            Asserted::Operator,
            "topic_publish",
            json!({"topic": topic, "text": UPDATE, "id": "cad2207-ordinary-update"}),
        )
        .expect("the operator could not publish the ordinary update");
    let post = published["message"]
        .as_str()
        .expect("the publish named no post")
        .to_string();
    let text = published.to_string();
    assert!(
        !text.contains(alice.alias.as_str()),
        "the publish answer named the private assistant: {published}"
    );
    assert!(
        !text.contains(assistant_uid.as_str()),
        "the publish answer exposed the private assistant's uid: {published}"
    );

    let deliveries = published["deliveries"]
        .as_array()
        .expect("the publish named no deliveries");
    assert_eq!(
        deliveries.len(),
        1,
        "the ordinary publish delivered to more than the ordinary peer: {published}"
    );
    assert_eq!(
        deliveries[0]["alias"].as_str(),
        Some(ORDINARY),
        "{published}"
    );
    assert_eq!(
        deliveries[0]["uid"].as_str(),
        Some(ordinary_uid.as_str()),
        "{published}"
    );

    let stored = fx.shared.store.topic_deliveries(&post).unwrap();
    assert!(
        stored.iter().any(|d| d.uid == ordinary_uid),
        "the ordinary peer got no delivery intent: {stored:?}"
    );
    assert!(
        !stored.iter().any(|d| d.uid == assistant_uid),
        "the bound assistant got a delivery intent for an ordinary publish: {stored:?}"
    );

    let ordinary_after = mailbox(ORDINARY);
    let assistant_after = mailbox(&alice.alias);
    let new_ordinary: Vec<&String> = ordinary_after
        .iter()
        .filter(|id| !ordinary_mail.contains(*id))
        .collect();
    assert_eq!(
        new_ordinary.len(),
        1,
        "the ordinary peer did not receive exactly one notice: {new_ordinary:?}"
    );
    let notice = fx
        .shared
        .store
        .message(new_ordinary[0])
        .unwrap()
        .expect("the ordinary notice vanished");
    assert_eq!(
        notice.source,
        crate::proto::TOPIC_NOTICE_SOURCE,
        "{notice:?}"
    );
    assert!(
        notice.body.contains(UPDATE) && notice.body.contains(topic.as_str()),
        "the ordinary notice is not this topic's update: {notice:?}"
    );
    assert_eq!(
        assistant_after, assistant_mail,
        "the bound assistant received a queued notice for an ordinary publish"
    );
}

/// CAD-2207: a genuine ordinary agent's post keeps its stored durable sender
/// uid after the agent row is REMOVED through the real lifecycle (`agent
/// remove`), so the operator can still read it; the bound assistant's and the
/// ambiguous null-UID posts remain withheld. The work-list group count is the
/// VISIBLE post count: posts on another topic sit between them in the global
/// sequence and must not move it, and neither may the hidden posts.
#[test]
fn removed_ordinary_author_stays_readable_and_visible_counts_ignore_other_topics() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let author = "cad2207-topic-author";
    let author_uid = ordinary_peer(&fx, author);
    const AUTHORED: &str = "cad2207-authored-before-removal";
    const OTHER_BODY: &str = "cad2207-other-topic-body";

    let created = fx
        .call(
            Asserted::Operator,
            "topic_create",
            json!({"type": "fleet", "source": "cad2207-author-topic"}),
        )
        .expect("the operator could not create the authored topic");
    let topic = created["topic"]
        .as_str()
        .expect("topic_create named no topic")
        .to_string();
    fx.call(
        Asserted::Operator,
        "topic_subscribe",
        json!({"topic": topic, "alias": author}),
    )
    .expect("the operator could not subscribe the author");
    let authored = fx
        .call(
            Asserted::Agent(author.to_string()),
            "topic_publish",
            json!({"topic": topic, "text": AUTHORED, "id": "cad2207-authored-post"}),
        )
        .expect("the ordinary author could not publish");
    assert_eq!(authored["duplicate"], false, "{authored}");

    // Remove the author through the real lifecycle. The actor-capable gate
    // wants a stopped, endpoint-less row first (fixture pre-state); the
    // removal itself is the dispatcher verb.
    fx.shared.store.set_enabled(author, false).unwrap();
    fx.shared
        .store
        .set_agent_state(author, "stopped", None)
        .unwrap();
    fx.call(
        Asserted::Operator,
        "agent_remove",
        json!({"alias": author, "force": true}),
    )
    .expect("the operator could not remove the ordinary author");
    assert!(
        fx.shared.store.agent_opt(author).unwrap().is_none(),
        "the author row survived removal"
    );

    // A visible peer keeps topic A's group row alive; a post on ANOTHER topic
    // sits between topic A's post and its hidden rows in the global seq.
    let _viewing_uid = ordinary_peer(&fx, ORDINARY);
    fx.call(
        Asserted::Operator,
        "topic_subscribe",
        json!({"topic": topic, "alias": ORDINARY}),
    )
    .expect("the operator could not subscribe the viewing peer");
    let other = fx
        .call(
            Asserted::Operator,
            "topic_create",
            json!({"type": "fleet", "source": "cad2207-other-topic"}),
        )
        .expect("the operator could not create the other topic");
    let other_topic = other["topic"]
        .as_str()
        .expect("topic_create named no other topic")
        .to_string();
    fx.call(
        Asserted::Operator,
        "topic_publish",
        json!({"topic": other_topic, "text": OTHER_BODY, "id": "cad2207-other-post"}),
    )
    .expect("the operator could not publish on the other topic");

    // The bound assistant's private post and an ambiguous null-UID alias post
    // join topic A after the removal; both are withheld.
    let alice = fx.member_runtime("sub-alice");
    let assistant_uid = fx.shared.store.agent(&alice.alias).unwrap().uid;
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO topic_posts(id,topic_id,sender_uid,sender,body,created) \
                 VALUES(?,?,?,?,?,?)",
                rusqlite::params![
                    "cad2207-removed-private-post",
                    topic,
                    assistant_uid,
                    alice.alias,
                    PRIVATE_BODY,
                    crate::issue::time::now_epoch() as f64
                ],
            )?;
            tx.execute(
                "INSERT INTO topic_posts(id,topic_id,sender_uid,sender,body,created) \
                 VALUES(?,?,NULL,?,?,?)",
                rusqlite::params![
                    "cad2207-removed-ambiguous-post",
                    topic,
                    "assist-agent-alice",
                    OLD_SHARED_BODY,
                    crate::issue::time::now_epoch() as f64
                ],
            )?;
            Ok(())
        })
        .expect("the fixture cannot seed the withheld posts");

    let read = fx
        .call(Asserted::Operator, "topic_read", json!({"topic": topic}))
        .expect("the operator's topic read was refused");
    let shown = bodies(&read);
    assert!(
        shown.iter().any(|body| body == AUTHORED),
        "the removed author's post is no longer readable: {read}"
    );
    assert!(
        !shown.iter().any(|body| body == PRIVATE_BODY)
            && !shown.iter().any(|body| body == OLD_SHARED_BODY),
        "a private or ambiguous post surfaced after the author's removal: {read}"
    );
    let authored_row = read["posts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|post| post["body"].as_str() == Some(AUTHORED))
        .expect("the authored post row vanished");
    assert_eq!(
        authored_row["sender"]["uid"].as_str(),
        Some(author_uid.as_str()),
        "the removed author's stored durable uid was lost: {authored_row}"
    );
    assert_eq!(
        authored_row["sender"]["alias"].as_str(),
        Some(author),
        "{authored_row}"
    );
    let authored_created = authored_row["created"]
        .as_f64()
        .expect("the authored post has no created stamp");

    // The work-list group count is the visible post count, and the hidden
    // posts plus the OTHER topic's intervening post cannot move it.
    let list = fx
        .call(Asserted::Operator, "work_conversation_list", json!({}))
        .expect("the operator's work list was refused");
    let row = group_row(&list, &topic);
    assert_eq!(
        row["posts"].as_u64(),
        Some(1),
        "the group count did not reflect only the visible posts: {row}"
    );
    assert_eq!(
        row["last_activity"].as_f64(),
        Some(authored_created),
        "the group last_activity moved on hidden posts: {row}"
    );
    assert!(
        !row["participants"]
            .to_string()
            .contains(assistant_uid.as_str()),
        "the group row listed the private assistant as a member: {row}"
    );
    assert!(
        !list.to_string().contains(PRIVATE_BODY) && !list.to_string().contains(OLD_SHARED_BODY),
        "the work list carried a withheld body: {list}"
    );
    let other_row = group_row(&list, &other_topic);
    assert!(
        other_row["posts"].as_u64() != Some(0),
        "the other topic's own post is missing from its group row: {other_row}"
    );
}

/// CAD-2207: `topic_request` takes `target` (not `alias`) and enqueues through
/// `send_with_human_expected` directly — the retained-subscriber bypass. An
/// operator request addressed to a previously subscribed bound assistant must
/// be refused BEFORE the post write / request enqueue, naming neither alias,
/// uid nor the input, and leaving no post, receipt, queue entry or notice. The
/// same request to an ordinary subscriber succeeds.
///
/// A bind racing in AFTER the target precheck would need a real transaction
/// fence. This fixture has no genuine seam to interleave `assistant_bind`
/// inside the dispatcher call (no test hook pauses between the check and the
/// enqueue), so no race is simulated here rather than faked.
#[test]
fn operator_topic_request_refuses_a_subscribed_private_assistant() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let ordinary_uid = ordinary_peer(&fx, ORDINARY);
    const PRIVATE_REQUEST: &str = "cad2207-private-request-input";
    const ORDINARY_REQUEST: &str = "cad2207-ordinary-request-input";

    let created = fx
        .call(
            Asserted::Operator,
            "topic_create",
            json!({"type": "fleet", "source": "cad2207-request-topic"}),
        )
        .expect("the operator could not create the request topic");
    let topic = created["topic"]
        .as_str()
        .expect("topic_create named no topic")
        .to_string();
    fx.call(
        Asserted::Operator,
        "topic_subscribe",
        json!({"topic": topic, "alias": ORDINARY}),
    )
    .expect("the operator could not subscribe the ordinary target");

    let alice = fx.member_runtime("sub-alice");
    let binding = fx
        .shared
        .store
        .assistant_binding(ORG, "sub-alice")
        .unwrap()
        .expect("the fixture binding vanished");
    let assistant_uid = fx.shared.store.agent(&alice.alias).unwrap().uid;
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO topic_subscriptions(topic_id,uid,added) VALUES(?,?,?)",
                rusqlite::params![topic, assistant_uid, binding.created - 60.0],
            )?;
            Ok(())
        })
        .expect("the fixture cannot seed the assistant's subscription");

    let mailbox = |alias: &str| -> Vec<String> {
        fx.shared
            .store
            .messages(alias)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect()
    };
    let posts_before = fx
        .shared
        .store
        .topic_posts(&topic, None, None, 1000)
        .unwrap()
        .len();
    let assistant_mail_before = mailbox(&alice.alias);
    let ordinary_mail_before = mailbox(ORDINARY);
    let private_post = crate::store::topic_post_id(&topic, "cad2207-private-request");
    let private_request = crate::store::topic_request_id(&topic, "cad2207-private-request");

    let error = fx
        .call(
            Asserted::Operator,
            "topic_request",
            json!({
                "topic": topic,
                "target": alice.alias,
                "text": PRIVATE_REQUEST,
                "id": "cad2207-private-request"
            }),
        )
        .expect_err("an operator request reached a subscribed private assistant");
    let text = error.to_string();
    assert!(
        !text.contains(alice.alias.as_str())
            && !text.contains(assistant_uid.as_str())
            && !text.contains(PRIVATE_REQUEST),
        "the topic_request refusal named a private identifier or the input: {text}"
    );
    assert_eq!(
        fx.shared
            .store
            .topic_posts(&topic, None, None, 1000)
            .unwrap()
            .len(),
        posts_before,
        "a refused topic_request wrote a topic post"
    );
    assert!(
        fx.shared
            .store
            .topic_post_by_id(&private_post)
            .unwrap()
            .is_none(),
        "a refused topic_request left its post row"
    );
    assert!(
        fx.shared
            .store
            .topic_post_request(&private_post)
            .unwrap()
            .is_none(),
        "a refused topic_request bound a request receipt"
    );
    assert!(
        fx.shared
            .store
            .topic_deliveries(&private_post)
            .unwrap()
            .is_empty(),
        "a refused topic_request wrote a delivery intent"
    );
    assert!(
        fx.shared.store.message(&private_request).unwrap().is_none(),
        "a refused topic_request queued its request message"
    );
    assert_eq!(
        mailbox(&alice.alias),
        assistant_mail_before,
        "a refused topic_request queued a notice in the assistant's mailbox"
    );
    assert_eq!(
        mailbox(ORDINARY),
        ordinary_mail_before,
        "a refused topic_request reached the ordinary mailbox"
    );

    // ---- ordinary-target control: the same verb still works --------------
    let granted = fx
        .call(
            Asserted::Operator,
            "topic_request",
            json!({
                "topic": topic,
                "target": ORDINARY,
                "text": ORDINARY_REQUEST,
                "id": "cad2207-ordinary-request"
            }),
        )
        .expect("the operator lost the ordinary topic_request path");
    assert_eq!(granted["duplicate"], false, "{granted}");
    assert_eq!(
        granted["target"]["alias"].as_str(),
        Some(ORDINARY),
        "{granted}"
    );
    assert_eq!(
        granted["target"]["uid"].as_str(),
        Some(ordinary_uid.as_str()),
        "{granted}"
    );
    let granted_post = granted["message"]
        .as_str()
        .expect("the request named no post")
        .to_string();
    let granted_request = granted["request"]
        .as_str()
        .expect("the request named no request")
        .to_string();
    let stored_post = fx
        .shared
        .store
        .topic_post_by_id(&granted_post)
        .unwrap()
        .expect("the request's topic post vanished");
    assert_eq!(stored_post.sender, "operator", "{stored_post:?}");
    assert_eq!(stored_post.body, ORDINARY_REQUEST, "{stored_post:?}");
    assert_eq!(
        fx.shared.store.topic_post_request(&granted_post).unwrap(),
        Some(granted_request.clone()),
        "the request's topic post is not bound to its receipt"
    );
    let request_msg = fx
        .shared
        .store
        .message(&granted_request)
        .unwrap()
        .expect("the request message vanished");
    assert_eq!(request_msg.alias, ORDINARY, "{request_msg:?}");
    assert!(
        request_msg.body.contains(ORDINARY_REQUEST),
        "the request message does not carry the input: {request_msg:?}"
    );
    assert_eq!(
        mailbox(ORDINARY).len(),
        ordinary_mail_before.len() + 1,
        "the ordinary target did not receive exactly one request"
    );
    assert_eq!(
        mailbox(&alice.alias),
        assistant_mail_before,
        "the ordinary topic_request leaked a notice to the private assistant"
    );
}

/// CAD-2207: the check/bind race at the STORE level. Once the alias/uid is
/// bound, a caller holding a stale pre-bind snapshot must not make the store
/// publish as that sender or change its membership: the store transaction
/// itself must refuse before any post, delivery or membership write. Ordinary
/// store calls keep working.
///
/// This is the stale-snapshot END STATE, not a live concurrent bind race:
/// there is no genuine transaction-barrier seam that interleaves
/// `assistant_bind` between the store's check and its write, so no live race is
/// simulated here rather than faked.
#[test]
fn store_topic_writes_refuse_a_bound_uid_from_a_stale_snapshot() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let ordinary_uid = ordinary_peer(&fx, ORDINARY);
    const PRIVATE_BODY: &str = "cad2207-store-private-body";
    const ORDINARY_BODY: &str = "cad2207-store-ordinary-body";

    let created = fx
        .call(
            Asserted::Operator,
            "topic_create",
            json!({"type": "fleet", "source": "cad2207-store-topic"}),
        )
        .expect("the operator could not create the store topic");
    let topic = created["topic"]
        .as_str()
        .expect("topic_create named no topic")
        .to_string();
    fx.call(
        Asserted::Operator,
        "topic_subscribe",
        json!({"topic": topic, "alias": ORDINARY}),
    )
    .expect("the operator could not subscribe the ordinary peer");

    let alice = fx.member_runtime("sub-alice");
    let assistant_uid = fx.shared.store.agent(&alice.alias).unwrap().uid;

    let mailbox = |alias: &str| -> Vec<String> {
        fx.shared
            .store
            .messages(alias)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect()
    };
    let posts_before = fx
        .shared
        .store
        .topic_posts(&topic, None, None, 1000)
        .unwrap()
        .len();
    let members_before = fx.shared.store.topic_subscribers(&topic).unwrap();
    let ordinary_mail_before = mailbox(ORDINARY);
    let assistant_mail_before = mailbox(&alice.alias);
    let private_client = "cad2207-store-private-publish";
    let private_post = crate::store::topic_post_id(&topic, private_client);

    let error = fx
        .shared
        .store
        .topic_publish(
            &topic,
            private_client,
            Some(assistant_uid.as_str()),
            &alice.alias,
            PRIVATE_BODY,
        )
        .expect_err("the store published as a bound assistant from a stale snapshot");
    let text = error.to_string();
    assert!(
        !text.contains(alice.alias.as_str())
            && !text.contains(assistant_uid.as_str())
            && !text.contains(PRIVATE_BODY),
        "the store's stale publish refusal named a private identifier or the input: {text}"
    );
    assert!(
        fx.shared
            .store
            .topic_post_by_id(&private_post)
            .unwrap()
            .is_none(),
        "a refused stale store publish wrote its post row"
    );
    assert!(
        fx.shared
            .store
            .topic_deliveries(&private_post)
            .unwrap()
            .is_empty(),
        "a refused stale store publish wrote a delivery intent"
    );
    assert_eq!(
        fx.shared
            .store
            .topic_posts(&topic, None, None, 1000)
            .unwrap()
            .len(),
        posts_before,
        "a refused stale store publish changed the topic posts"
    );
    assert_eq!(
        mailbox(ORDINARY),
        ordinary_mail_before,
        "a refused stale store publish reached the ordinary mailbox"
    );
    assert_eq!(
        mailbox(&alice.alias),
        assistant_mail_before,
        "a refused stale store publish queued a notice for the assistant"
    );

    let error = fx
        .shared
        .store
        .topic_subscribe(&topic, &assistant_uid)
        .expect_err("the store subscribed a bound assistant's uid from a stale snapshot");
    let text = error.to_string();
    assert!(
        !text.contains(alice.alias.as_str()) && !text.contains(assistant_uid.as_str()),
        "the stale subscribe refusal named the private assistant: {text}"
    );
    let error = fx
        .shared
        .store
        .topic_unsubscribe(&topic, &assistant_uid)
        .expect_err("the store unsubscribed a bound assistant's uid from a stale snapshot");
    let text = error.to_string();
    assert!(
        !text.contains(alice.alias.as_str()) && !text.contains(assistant_uid.as_str()),
        "the stale unsubscribe refusal named the private assistant: {text}"
    );
    assert_eq!(
        fx.shared.store.topic_subscribers(&topic).unwrap(),
        members_before,
        "a refused stale membership call moved the topic's subscribers"
    );

    // ---- ordinary store controls -----------------------------------------
    fx.shared
        .store
        .topic_publish(
            &topic,
            "cad2207-store-ordinary-publish",
            Some(ordinary_uid.as_str()),
            ORDINARY,
            ORDINARY_BODY,
        )
        .expect("an ordinary store publish was refused");
    let ordinary_post = crate::store::topic_post_id(&topic, "cad2207-store-ordinary-publish");
    let stored = fx
        .shared
        .store
        .topic_post_by_id(&ordinary_post)
        .unwrap()
        .expect("the ordinary store post vanished");
    assert_eq!(
        stored.sender_uid.as_deref(),
        Some(ordinary_uid.as_str()),
        "{stored:?}"
    );
    assert_eq!(stored.sender, ORDINARY, "{stored:?}");
    assert!(
        fx.shared
            .store
            .topic_deliveries(&ordinary_post)
            .unwrap()
            .iter()
            .any(|d| d.uid == ordinary_uid),
        "the ordinary store publish wrote no delivery"
    );
    fx.shared
        .store
        .topic_unsubscribe(&topic, &ordinary_uid)
        .expect("the ordinary store unsubscribe was refused");
    assert!(
        !fx.shared
            .store
            .topic_is_subscriber(&topic, &ordinary_uid)
            .unwrap(),
        "the ordinary store unsubscribe did not remove the row"
    );
    fx.shared
        .store
        .topic_subscribe(&topic, &ordinary_uid)
        .expect("the ordinary store subscribe was refused");
    assert!(
        fx.shared
            .store
            .topic_is_subscriber(&topic, &ordinary_uid)
            .unwrap(),
        "the ordinary store subscribe did not add the row"
    );
}

/// CAD-2207: an Ask receipt whose bound request message has only HALF its
/// identity (recipient uid without the recipient label, or the label without
/// the uid) is ambiguous provenance. `topic_read` must keep the ordinary post
/// body but omit the request handle, the uid/alias receipt and its state, for
/// BOTH the operator and an ordinary subscribed peer — while an independently
/// valid ordinary Ask keeps its full receipt. Both missing halves are seeded.
#[test]
fn topic_read_withholds_ambiguous_ask_receipts_but_keeps_valid_ordinary_ones() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let ordinary_uid = ordinary_peer(&fx, ORDINARY);
    const VALID_BODY: &str = "cad2207-valid-ask-body";
    const UID_ONLY_BODY: &str = "cad2207-ambiguous-uid-only-ask";
    const LABEL_ONLY_BODY: &str = "cad2207-ambiguous-label-only-ask";
    const UID_ONLY_REQUEST: &str = "cad2207-ambiguous-request-uid-only";
    const LABEL_ONLY_REQUEST: &str = "cad2207-ambiguous-request-label-only";
    const UID_ONLY_POST: &str = "cad2207-ambiguous-post-uid-only";
    const LABEL_ONLY_POST: &str = "cad2207-ambiguous-post-label-only";

    let created = fx
        .call(
            Asserted::Operator,
            "topic_create",
            json!({"type": "fleet", "source": "cad2207-ask-topic"}),
        )
        .expect("the operator could not create the Ask topic");
    let topic = created["topic"]
        .as_str()
        .expect("topic_create named no topic")
        .to_string();
    fx.call(
        Asserted::Operator,
        "topic_subscribe",
        json!({"topic": topic, "alias": ORDINARY}),
    )
    .expect("the operator could not subscribe the ordinary peer");

    // The independently valid ordinary Ask: the store's enqueue sets BOTH the
    // recipient uid and the recipient label from the target row.
    let granted = fx
        .call(
            Asserted::Operator,
            "topic_request",
            json!({
                "topic": topic,
                "target": ORDINARY,
                "text": VALID_BODY,
                "id": "cad2207-valid-ask"
            }),
        )
        .expect("the operator lost the ordinary Ask path");
    let valid_request = granted["request"]
        .as_str()
        .expect("the valid Ask named no request")
        .to_string();
    let valid_state = fx
        .shared
        .store
        .message(&valid_request)
        .unwrap()
        .expect("the valid request message vanished")
        .state;

    // Two visible ordinary posts, each bound to a request message missing ONE
    // identity half. Synthetic fixture rows: no fiddling with production
    // writers, and the ordinary author label keeps both posts visible.
    let now = crate::issue::time::now_epoch() as f64;
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO messages(id,alias,body,source,state,created,recipient_uid,recipient) \
                 VALUES(?,?,?,'operator','queued',?,?,NULL)",
                rusqlite::params![
                    UID_ONLY_REQUEST,
                    ORDINARY,
                    "[fleet cad2207-ask-topic] uid-only",
                    now,
                    ordinary_uid
                ],
            )?;
            tx.execute(
                "INSERT INTO messages(id,alias,body,source,state,created,recipient_uid,recipient) \
                 VALUES(?,?,?,'operator','queued',?,NULL,?)",
                rusqlite::params![
                    LABEL_ONLY_REQUEST,
                    ORDINARY,
                    "[fleet cad2207-ask-topic] label-only",
                    now,
                    ORDINARY
                ],
            )?;
            tx.execute(
                "INSERT INTO topic_posts(id,topic_id,sender_uid,sender,body,created,request_message) \
                 VALUES(?,?,NULL,'operator',?,?,?)",
                rusqlite::params![UID_ONLY_POST, topic, UID_ONLY_BODY, now, UID_ONLY_REQUEST],
            )?;
            tx.execute(
                "INSERT INTO topic_posts(id,topic_id,sender_uid,sender,body,created,request_message) \
                 VALUES(?,?,NULL,'operator',?,?,?)",
                rusqlite::params![
                    LABEL_ONLY_POST,
                    topic,
                    LABEL_ONLY_BODY,
                    now,
                    LABEL_ONLY_REQUEST
                ],
            )?;
            Ok(())
        })
        .expect("the fixture cannot seed the ambiguous Ask rows");

    let check = |read: &Value| {
        let text = read.to_string();
        for request in [UID_ONLY_REQUEST, LABEL_ONLY_REQUEST] {
            assert!(
                !text.contains(request),
                "the read exposed an ambiguous request handle: {read}"
            );
        }
        let posts = read["posts"]
            .as_array()
            .unwrap_or_else(|| panic!("topic_read has no posts: {read}"));
        for body in [UID_ONLY_BODY, LABEL_ONLY_BODY] {
            let row = posts
                .iter()
                .find(|post| post["body"].as_str() == Some(body))
                .unwrap_or_else(|| panic!("the ordinary post '{body}' vanished: {read}"));
            assert!(
                row["request"].is_null(),
                "the ambiguous request handle was emitted with the visible post: {row}"
            );
            assert!(
                row["deliveries"]
                    .as_array()
                    .is_none_or(|receipts| receipts.is_empty()),
                "the read emitted an ambiguous uid/alias receipt or state: {row}"
            );
        }
        let valid = posts
            .iter()
            .find(|post| post["body"].as_str() == Some(VALID_BODY))
            .unwrap_or_else(|| panic!("the valid Ask post vanished: {read}"));
        assert_eq!(
            valid["request"].as_str(),
            Some(valid_request.as_str()),
            "the valid Ask lost its request handle: {valid}"
        );
        let receipts = valid["deliveries"]
            .as_array()
            .expect("the valid receipt is not a list");
        assert_eq!(receipts.len(), 1, "the valid Ask lost its receipt: {valid}");
        assert_eq!(
            receipts[0]["uid"].as_str(),
            Some(ordinary_uid.as_str()),
            "{valid}"
        );
        assert_eq!(receipts[0]["alias"].as_str(), Some(ORDINARY), "{valid}");
        assert_eq!(
            receipts[0]["message"].as_str(),
            Some(valid_request.as_str()),
            "{valid}"
        );
        assert_eq!(
            receipts[0]["state"].as_str(),
            Some(valid_state.as_str()),
            "{valid}"
        );
    };

    let operator_read = fx
        .call(Asserted::Operator, "topic_read", json!({"topic": topic}))
        .expect("the operator's topic read was refused");
    check(&operator_read);

    let peer_read = fx
        .call(
            Asserted::Agent(ORDINARY.to_string()),
            "topic_read",
            json!({"topic": topic}),
        )
        .expect("the ordinary peer's topic read was refused");
    check(&peer_read);
}

/// CAD-2207: the IN-TRANSACTION recheck in
/// `enqueue_steered_for_with_conversation`. A caller that resolved a generic
/// (non-room) topic post and an ordinary target, then lost the race to a
/// binding, must be refused INSIDE the queue transaction: no message row and no
/// post/request binding, with a generic text naming neither alias nor uid. The
/// ordinary-target queue + binding still works.
///
/// This pins the stale-target END STATE only. It does NOT claim the separate
/// post-insert/queue split is atomic: the post row exists before this call, and
/// a concurrent bind is only observed by the recheck, not eliminated.
#[test]
fn store_enqueue_rechecks_a_bound_target_for_a_generic_topic_post() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let ordinary_uid = ordinary_peer(&fx, ORDINARY);
    let alice = fx.member_runtime("sub-alice");
    let assistant_uid = fx.shared.store.agent(&alice.alias).unwrap().uid;

    let created = fx
        .call(
            Asserted::Operator,
            "topic_create",
            json!({"type": "fleet", "source": "cad2207-enqueue-topic"}),
        )
        .expect("the operator could not create the enqueue topic");
    let topic = created["topic"]
        .as_str()
        .expect("topic_create named no topic")
        .to_string();
    let (bound_post, _) = fx
        .shared
        .store
        .topic_post_insert(
            &topic,
            "cad2207-bound-request",
            None,
            "operator",
            "cad2207-bound-request-body",
        )
        .expect("the fixture cannot insert the bound-request post");
    let (ordinary_post, _) = fx
        .shared
        .store
        .topic_post_insert(
            &topic,
            "cad2207-ordinary-request",
            None,
            "operator",
            "cad2207-ordinary-request-body",
        )
        .expect("the fixture cannot insert the ordinary-request post");

    // The target is now bound even though the caller's precheck saw it
    // ordinary; the queue transaction must refuse the generic-topic request.
    let bound_message = "cad2207-enqueue-bound-message";
    let error = fx
        .shared
        .store
        .enqueue_steered_for_with_conversation(
            &alice.alias,
            Some(assistant_uid.as_str()),
            "cad2207-enqueue-bound-body",
            None,
            bound_message,
            "operator",
            None,
            None,
            None,
            &crate::store::Sender::OperatorChat,
            &crate::store::Steer::NONE,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(bound_post.as_str()),
        )
        .expect_err("a stale bound target was queued into a generic topic request");
    assert_eq!(
        error.to_string(),
        crate::store::RESTRICTED_LIFECYCLE,
        "the queue transaction did not answer the generic lifecycle refusal: {error}"
    );
    assert!(
        !error.to_string().contains(alice.alias.as_str())
            && !error.to_string().contains(assistant_uid.as_str()),
        "the queue refusal named the private assistant: {error}"
    );
    assert!(
        fx.shared.store.message(bound_message).unwrap().is_none(),
        "the refused queue wrote the request message row"
    );
    assert!(
        fx.shared
            .store
            .topic_post_request(&bound_post)
            .unwrap()
            .is_none(),
        "the refused queue bound the request post"
    );
    let stored = fx
        .shared
        .store
        .topic_post_by_id(&bound_post)
        .unwrap()
        .expect("the pre-existing request post vanished");
    assert_eq!(stored.body, "cad2207-bound-request-body", "{stored:?}");

    // ---- ordinary-target positive: queue, then binding -------------------
    let ordinary_message = "cad2207-enqueue-ordinary-message";
    let (fresh, state) = fx
        .shared
        .store
        .enqueue_steered_for_with_conversation(
            ORDINARY,
            Some(ordinary_uid.as_str()),
            "cad2207-enqueue-ordinary-body",
            None,
            ordinary_message,
            "operator",
            None,
            None,
            None,
            &crate::store::Sender::OperatorChat,
            &crate::store::Steer::NONE,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(ordinary_post.as_str()),
        )
        .expect("an ordinary target's request-enqueue was refused");
    assert!(
        !fresh,
        "a fresh ordinary request was reported as a duplicate: {state}"
    );
    let queued = fx
        .shared
        .store
        .message(ordinary_message)
        .unwrap()
        .expect("the ordinary request message vanished");
    assert_eq!(queued.alias, ORDINARY, "{queued:?}");
    // The enqueue binds the request post in the same transaction.
    assert_eq!(
        fx.shared.store.topic_post_request(&ordinary_post).unwrap(),
        Some(ordinary_message.to_string()),
        "the ordinary request post is not bound to its queued message"
    );
}

/// CAD-2207: the duplicate-`topic_request` leak. An ordinary Ask's retry must
/// keep returning its stable duplicate (same post/request/target, no new post
/// or message). A malformed pre-existing receipt whose recipient uid matches
/// but whose recipient label or message alias does not must refuse with no
/// handle or mutation. Once that SAME alias/uid is bound, the retry must
/// refuse generically too — no alias, uid, post id or request handle in the
/// text — and change nothing (no post, no message, no binding mutation).
///
/// This pins the STABLE binding end state: the binding exists before the retry.
/// The alias-check/receipt-read race the reviewer described still needs a real
/// transaction barrier (none exists here), and the post/queue split is
/// separately blocked; neither is simulated or claimed.
#[test]
fn operator_topic_request_duplicate_retry_refuses_once_the_target_is_bound() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let target = "cad2207-duplicate-target";
    let target_uid = ordinary_peer(&fx, target);
    const BODY: &str = "cad2207-duplicate-ask-body";
    const REQUEST_ID: &str = "cad2207-duplicate-ask";
    let now = crate::issue::time::now_epoch() as f64;

    let created = fx
        .call(
            Asserted::Operator,
            "topic_create",
            json!({"type": "fleet", "source": "cad2207-duplicate-topic"}),
        )
        .expect("the operator could not create the duplicate topic");
    let topic = created["topic"]
        .as_str()
        .expect("topic_create named no topic")
        .to_string();
    fx.call(
        Asserted::Operator,
        "topic_subscribe",
        json!({"topic": topic, "alias": target}),
    )
    .expect("the operator could not subscribe the duplicate target");

    let first = fx
        .call(
            Asserted::Operator,
            "topic_request",
            json!({"topic": topic, "target": target, "text": BODY, "id": REQUEST_ID}),
        )
        .expect("the operator could not send the ordinary Ask");
    assert_eq!(first["duplicate"], false, "{first}");
    let post = first["message"]
        .as_str()
        .expect("the Ask named no post")
        .to_string();
    let request = first["request"]
        .as_str()
        .expect("the Ask named no request")
        .to_string();
    let posts_after_first = fx
        .shared
        .store
        .topic_posts(&topic, None, None, 1000)
        .unwrap()
        .len();
    let mail_after_first = fx.shared.store.messages(target).unwrap().len();

    // ---- ordinary duplicate-retry positive --------------------------------
    let retried = fx
        .call(
            Asserted::Operator,
            "topic_request",
            json!({"topic": topic, "target": target, "text": BODY, "id": REQUEST_ID}),
        )
        .expect("the ordinary duplicate retry was refused");
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
        retried["target"]["alias"].as_str(),
        Some(target),
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
        "the duplicate retry wrote another topic post"
    );
    assert_eq!(
        fx.shared.store.messages(target).unwrap().len(),
        mail_after_first,
        "the duplicate retry queued another request message"
    );

    // ---- malformed pre-existing receipts: uid matches, identity does not --
    // Each valid Ask is created, then its stored request message is damaged so
    // the recipient UID still matches the target while the recipient label (or
    // the message alias) does not. The duplicate path must refuse with no
    // handle in the text and no new post, message or binding rewrite.
    let corrupt = |request: &str, sql: &str| {
        fx.shared
            .store
            .fixture_write(|tx| {
                tx.execute(sql, rusqlite::params![request])?;
                Ok(())
            })
            .expect("the fixture cannot corrupt the request receipt");
    };
    let duplicate_params =
        |id: &str| json!({"topic": topic, "target": target, "text": BODY, "id": id});
    for (client_id, sql) in [
        (
            "cad2207-malformed-label-null",
            "UPDATE messages SET recipient=NULL WHERE id=?",
        ),
        (
            "cad2207-malformed-label-wrong",
            "UPDATE messages SET recipient='cad2207-wrong-label' WHERE id=?",
        ),
        (
            "cad2207-malformed-alias-wrong",
            "UPDATE messages SET alias='cad2207-wrong-alias' WHERE id=?",
        ),
    ] {
        let made = fx
            .call(
                Asserted::Operator,
                "topic_request",
                duplicate_params(client_id),
            )
            .expect("the fixture could not create the malformed receipt's Ask");
        let malformed_post = made["message"]
            .as_str()
            .expect("the malformed Ask named no post")
            .to_string();
        let malformed_request = made["request"]
            .as_str()
            .expect("the malformed Ask named no request")
            .to_string();
        corrupt(&malformed_request, sql);
        let posts_before = fx
            .shared
            .store
            .topic_posts(&topic, None, None, 1000)
            .unwrap()
            .len();
        let mail_before = fx.shared.store.messages(target).unwrap().len();
        let error = fx
            .call(
                Asserted::Operator,
                "topic_request",
                duplicate_params(client_id),
            )
            .expect_err("a malformed duplicate receipt was served");
        let text = error.to_string();
        for secret in [
            target,
            target_uid.as_str(),
            malformed_post.as_str(),
            malformed_request.as_str(),
        ] {
            assert!(
                !text.contains(secret),
                "the malformed duplicate refusal leaked '{secret}': {text}"
            );
        }
        assert_eq!(
            fx.shared
                .store
                .topic_posts(&topic, None, None, 1000)
                .unwrap()
                .len(),
            posts_before,
            "the malformed duplicate refusal wrote a topic post"
        );
        assert_eq!(
            fx.shared.store.messages(target).unwrap().len(),
            mail_before,
            "the malformed duplicate refusal queued a request message"
        );
        assert_eq!(
            fx.shared.store.topic_post_request(&malformed_post).unwrap(),
            Some(malformed_request.clone()),
            "the malformed duplicate refusal changed the request binding"
        );
    }

    // ---- the target becomes bound -----------------------------------------
    // A synthetic assistants row for the SAME alias keeps the ordinary agent
    // row and uid intact, so the duplicate path still resolves the target and
    // matches the stored receipt — exactly the state the reviewer's race can
    // produce. The binding's genuine durable visibility fence is stamped too
    // (the same same-transaction cutoffs `assistant_bind` writes); a binding
    // without one fails every message read closed, which is the refusal the
    // fixture must not mistake for its assertion.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO assistants(org,sub,assistant_id,alias,state,authority_rev,created,updated) \
                 VALUES(?,?,?,?,'active',?,?,?)",
                rusqlite::params![ORG, "sub-alice", "assist_alice", target, "rev0", now, now],
            )?;
            let message_cutoff: i64 =
                tx.query_row("SELECT COALESCE(MAX(seq),0) FROM messages", [], |row| {
                    row.get(0)
                })?;
            let event_cutoff: i64 =
                tx.query_row("SELECT COALESCE(MAX(seq),0) FROM events", [], |row| {
                    row.get(0)
                })?;
            tx.execute(
                "INSERT INTO assistant_event_fences(org,sub,assistant_id,alias,cutoff_seq,message_cutoff_seq,agent_uid,created) \
                 VALUES(?,?,?,?,?,?,?,?)",
                rusqlite::params![
                    ORG,
                    "sub-alice",
                    "assist_alice",
                    target,
                    event_cutoff,
                    message_cutoff,
                    target_uid,
                    now
                ],
            )?;
            Ok(())
        })
        .expect("the fixture cannot bind the duplicate target with its fence");
    assert!(
        fx.shared
            .store
            .assistant_for_alias(target)
            .unwrap()
            .is_some(),
        "the synthetic binding was not stored"
    );

    let posts_before_retry = fx
        .shared
        .store
        .topic_posts(&topic, None, None, 1000)
        .unwrap()
        .len();
    let mail_before_retry: Vec<String> = fx
        .shared
        .store
        .messages(target)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    let binding_before = fx
        .shared
        .store
        .assistant_binding(ORG, "sub-alice")
        .unwrap()
        .expect("the synthetic binding vanished")
        .to_json();

    let error = fx
        .call(
            Asserted::Operator,
            "topic_request",
            json!({"topic": topic, "target": target, "text": BODY, "id": REQUEST_ID}),
        )
        .expect_err("a bound target's duplicate retry returned a private handle");
    let text = error.to_string();
    for secret in [target, target_uid.as_str(), post.as_str(), request.as_str()] {
        assert!(
            !text.contains(secret),
            "the duplicate refusal leaked '{secret}': {text}"
        );
    }
    assert_eq!(
        fx.shared
            .store
            .topic_posts(&topic, None, None, 1000)
            .unwrap()
            .len(),
        posts_before_retry,
        "the refused duplicate retry wrote another topic post"
    );
    let stored = fx
        .shared
        .store
        .topic_post_by_id(&post)
        .unwrap()
        .expect("the original request post vanished");
    assert_eq!(stored.body, BODY, "{stored:?}");
    assert_eq!(
        fx.shared.store.topic_post_request(&post).unwrap(),
        Some(request.clone()),
        "the refused duplicate retry changed the request binding"
    );
    let mail_after_retry: Vec<String> = fx
        .shared
        .store
        .messages(target)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(
        mail_after_retry, mail_before_retry,
        "the refused duplicate retry queued a message"
    );
    assert_eq!(
        fx.shared
            .store
            .assistant_binding(ORG, "sub-alice")
            .unwrap()
            .expect("the synthetic binding vanished")
            .to_json(),
        binding_before,
        "the refused duplicate retry changed the binding"
    );
}

/// CAD-2207: a REAL job transition's topic fan-out must use the same
/// private-recipient exclusion as the generic publisher. The job topic has an
/// ordinary subscriber (also the job PM) plus a seeded legacy subscription for
/// a now-bound personal assistant. The real stall-watch transition notice
/// (`Store::job_notice`, the public `route_job_event` path) must write the
/// fan-out post and deliver the ordinary notice, while creating NO delivery or
/// mailbox notice for the bound assistant. The PM `job_event` queue is
/// unchanged.
///
/// Seam report: there is no public "job" topic-create verb (`topic_create`
/// refuses the `job` kind), so the fixture provisions the job topic with the
/// same `Store::topic_ensure` the daemon uses internally; `create_job` creates
/// the `{job}-t1` task the transition names. The transition itself is the real
/// public store path, not a forged outcome.
#[test]
fn job_topic_fanout_skips_a_bound_assistants_legacy_subscription() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let pm = "cad2207-job-pm";
    let pm_uid = ordinary_peer(&fx, pm);
    let alice = fx.member_runtime("sub-alice");
    let assistant_uid = fx.shared.store.agent(&alice.alias).unwrap().uid;

    let spec = fx.dir.path().join("cad2207-job-spec.md");
    std::fs::write(&spec, "# job\n").expect("the fixture cannot write the spec");
    let sha = super::super::sha256_file(&spec).expect("the fixture cannot hash the spec");
    let (_fresh, job) = fx
        .shared
        .store
        .create_job(
            "job-cad2207-fanout",
            Some("cad2207"),
            spec.to_str().unwrap(),
            &sha,
            pm,
            None,
            None,
            None,
            2,
            None,
            Some("cad2207 fanout task"),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("the fixture cannot create the job");
    let task = format!("{}-t1", job.id);
    let (topic, _) = fx
        .shared
        .store
        .topic_ensure("job", &job.id)
        .expect("the fixture cannot ensure the job topic");
    fx.shared
        .store
        .topic_subscribe(&topic.id, &pm_uid)
        .expect("the fixture cannot subscribe the ordinary PM");
    let binding = fx
        .shared
        .store
        .assistant_binding(ORG, "sub-alice")
        .unwrap()
        .expect("the fixture binding vanished");
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO topic_subscriptions(topic_id,uid,added) VALUES(?,?,?)",
                rusqlite::params![topic.id, assistant_uid, binding.created - 60.0],
            )?;
            Ok(())
        })
        .expect("the fixture cannot seed the legacy private subscription");

    let mailbox = |alias: &str| -> Vec<String> {
        fx.shared
            .store
            .messages(alias)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect()
    };
    let pm_mail_before = mailbox(pm);
    let assistant_mail_before = mailbox(&alice.alias);

    fx.shared
        .store
        .job_notice(
            &task,
            "dispatched",
            "cad2207-fanout",
            "cad2207 job fanout note",
        )
        .expect("the real job transition notice was refused");

    let posts = fx
        .shared
        .store
        .topic_posts(&topic.id, None, None, 10)
        .unwrap();
    let post = posts
        .iter()
        .find(|post| post.sender == "daemon")
        .expect("the job topic got no daemon fanout post");
    assert!(
        post.body.contains("cad2207 job fanout note"),
        "the fanout post does not carry the transition body: {post:?}"
    );
    let deliveries = fx.shared.store.topic_deliveries(&post.id).unwrap();
    assert!(
        deliveries.iter().any(|d| d.uid == pm_uid),
        "the ordinary subscriber got no fanout delivery: {deliveries:?}"
    );
    assert!(
        !deliveries.iter().any(|d| d.uid == assistant_uid),
        "the bound assistant got a job-topic delivery: {deliveries:?}"
    );

    let pm_mail_after = mailbox(pm);
    let new_pm: Vec<&String> = pm_mail_after
        .iter()
        .filter(|id| !pm_mail_before.contains(*id))
        .collect();
    assert_eq!(
        new_pm.len(),
        2,
        "the PM did not receive the job_event and its topic notice: {new_pm:?}"
    );
    let mut saw_event = false;
    let mut saw_notice = false;
    for id in &new_pm {
        let message = fx
            .shared
            .store
            .message(id)
            .unwrap()
            .expect("a new PM message vanished");
        if message.source == "job_event" {
            saw_event = true;
        }
        if message.source == crate::proto::TOPIC_NOTICE_SOURCE {
            saw_notice = true;
            assert!(
                message.body.contains("cad2207 job fanout note"),
                "the ordinary topic notice lost the transition body: {message:?}"
            );
        }
    }
    assert!(
        saw_event && saw_notice,
        "the real transition path changed shape: {new_pm:?}"
    );
    assert_eq!(
        mailbox(&alice.alias),
        assistant_mail_before,
        "the bound assistant received a job-topic notice"
    );
}
