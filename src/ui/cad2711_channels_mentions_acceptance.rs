//! CAD-2711 acceptance tests: unified `@mention` routing, personal assistant
//! isolation, creator/owner channel membership + cutoff audit events, silent
//! `/note` mode, non-escalation authority preservation, and Human-Turn Brake.

use super::*;
use crate::test_seam::{scoped, Asserted};

fn register_shareable_agent(rig: &Rig, alias: &str) -> String {
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias,
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: rig._dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    room_fixture_db(rig)
        .query_row("SELECT uid FROM agents WHERE alias=?1", [alias], |r| {
            r.get(0)
        })
        .unwrap()
}

fn agent_topic_publish(
    rig: &Rig,
    alias: &str,
    topic: &str,
    id: &str,
    text: &str,
) -> crate::Result<Value> {
    scoped(Asserted::Agent(alias.to_string()), || {
        crate::client::rpc(
            rig._dir.path(),
            "topic_publish",
            json!({ "topic": topic, "text": text, "id": id }),
        )
    })
}

#[test]
fn cad2711_non_member_mention_is_blocked_acl_with_zero_queued_or_bell_intents() {
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer, _events) = queue_observer();
    let rig = grant_rig(
        Arc::new(AtomicBool::new(true)),
        roster.clone(),
        observer,
        None,
        None,
    );
    let _keys = initialize_queue_keyring(&rig);
    let ana = login(&rig, "ana", "member");
    let ben = login(&rig, "ben", "member");
    roster.lock().unwrap().allowed_cookies = vec![ana.clone(), ben];
    let _member_worker = register_shareable_agent(&rig, "member-worker");
    let _outside_worker = register_shareable_agent(&rig, "outside-worker");
    let ben_actor =
        crate::store::human_threads::VerifiedHuman::verified(&rig.issuer, ORG, "ben").unwrap();

    // Only Ana is a member of this channel; Ben and outside-worker are not members.
    let group = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "human_group",
            "title": "ACL Isolation",
            "client_id": "acl-isolation-room",
            "humans": [],
            "agents": []
        })),
    );
    let room = group["room"].as_str().unwrap();
    let posted = room_post(
        &rig,
        &ana,
        room,
        "acl-post-1",
        "Checking @ben and @outside-worker plus @member-worker",
    );
    assert_room_post(&posted["post"]);
    assert_eq!(posted["post"]["silent"], false);
    let outcomes = posted["post"]["outcomes"].as_array().unwrap();
    assert_eq!(
        outcomes.len(),
        3,
        "three unresolved non-member handles: {outcomes:?}"
    );
    for expected in ["@ben", "@outside-worker", "@member-worker"] {
        let hit = outcomes
            .iter()
            .find(|o| {
                o["handle"]
                    .as_str()
                    .is_some_and(|h| h.eq_ignore_ascii_case(expected))
            })
            .unwrap_or_else(|| panic!("missing outcome for {expected}: {outcomes:?}"));
        assert_eq!(hit["kind"], "blocked_acl", "expected blocked_acl: {hit}");
    }

    let db = room_fixture_db(&rig);
    let msg_count: i64 = db
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(msg_count, 0, "non-member @agent mention must queue nothing");
    let intent_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM human_notification_intents WHERE room=?1 AND recipient=?2",
            rusqlite::params![room, &ben_actor.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        intent_count, 0,
        "non-member @human mention must emit no bell/push intent"
    );
}

#[test]
fn cad2711_active_human_mention_notifies_bell_and_keeps_assistant_isolated() {
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer, _events) = queue_observer();
    let rig = grant_rig(
        Arc::new(AtomicBool::new(true)),
        roster.clone(),
        observer,
        None,
        None,
    );
    let _keys = initialize_queue_keyring(&rig);
    let ana = login(&rig, "ana", "member");
    let ben = login(&rig, "ben", "member");
    roster.lock().unwrap().allowed_cookies = vec![ana.clone(), ben];
    // Register Ben's bound assistant alias in agents too so isolation is proven even if queried.
    let _ben_assistant = register_shareable_agent(&rig, "assist-agent-ben");
    let ben_actor =
        crate::store::human_threads::VerifiedHuman::verified(&rig.issuer, ORG, "ben").unwrap();

    let group = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "human_group",
            "title": "Human Mention Isolation",
            "client_id": "human-mention-iso",
            "humans": ["ben"],
            "agents": []
        })),
    );
    let room = group["room"].as_str().unwrap();
    let posted = room_post(&rig, &ana, room, "ping-ben", "Hey @ben please review");
    let outcomes = posted["post"]["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert_eq!(outcomes[0]["kind"], "notified");
    assert_eq!(outcomes[0]["label"], "Ben");

    let db = room_fixture_db(&rig);
    let reason: String = db
        .query_row(
            "SELECT reason FROM human_notification_intents WHERE room=?1 AND recipient=?2",
            rusqlite::params![room, &ben_actor.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reason, "mention");
    let assistant_msgs: i64 = db
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        assistant_msgs, 0,
        "human @mention must never wake or queue personal assistant"
    );
}

#[test]
fn cad2711_creator_and_owner_manage_members_auto_upgrade_and_scope_audit_events() {
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer, _events) = queue_observer();
    let rig = grant_rig(
        Arc::new(AtomicBool::new(true)),
        roster.clone(),
        observer,
        None,
        None,
    );
    let _keys = initialize_queue_keyring(&rig);
    let owner = login(&rig, "grant-owner", "owner");
    let ana = login(&rig, "ana", "member");
    let ben = login(&rig, "ben", "member");
    {
        let mut r = roster.lock().unwrap();
        r.owner_cookie = owner.clone();
        r.allowed_cookies = vec![ana.clone(), ben.clone()];
    }
    let worker_uid = register_shareable_agent(&rig, "ops-worker");
    let ben_actor =
        crate::store::human_threads::VerifiedHuman::verified(&rig.issuer, ORG, "ben").unwrap();

    // Ana creates a human-only channel with Owner and Ben.
    let group = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "human_group",
            "title": "Ops Channel",
            "client_id": "ops-channel-1",
            "humans": ["grant-owner", "ben"],
            "agents": []
        })),
    );
    let room = group["room"].as_str().unwrap();
    let path = format!("/api/human-threads/rooms/{room}");

    // Creator & Owner have can_manage=true; ordinary member Ben has can_manage=false.
    assert_eq!(room_ok(&rig, &ana, "GET", &path, None)["can_manage"], true);
    assert_eq!(
        room_ok(&rig, &owner, "GET", &path, None)["can_manage"],
        true
    );
    assert_eq!(room_ok(&rig, &ben, "GET", &path, None)["can_manage"], false);

    // Non-creator member Ben is refused HTTP 403 not_room_manager on add & remove.
    let (add_status, add_err) = room_http(
        &rig,
        &ben,
        "POST",
        &format!("{path}/members"),
        Some(json!({ "agent_uid": worker_uid })),
    );
    assert_eq!(add_status, 403, "{add_err}");
    assert_eq!(add_err["code"], "not_room_manager");

    let (del_status, del_err) = room_http(
        &rig,
        &ben,
        "DELETE",
        &format!("{path}/members/human/{}", ben_actor.id),
        None,
    );
    assert_eq!(del_status, 403, "{del_err}");
    assert_eq!(del_err["code"], "not_room_manager");

    // Creator removes Ben; event is logged while Ben is removed.
    room_ok(
        &rig,
        &ana,
        "DELETE",
        &format!("{path}/members/human/{}", ben_actor.id),
        None,
    );

    // Workspace owner (not creator) adds a shared agent to the human_group -> auto-upgrades to mixed_group.
    room_ok(
        &rig,
        &owner,
        "POST",
        &format!("{path}/members"),
        Some(json!({ "agent_uid": worker_uid })),
    );
    let db = room_fixture_db(&rig);
    let kind: String = db
        .query_row("SELECT kind FROM human_rooms WHERE id=?1", [room], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        kind, "mixed_group",
        "adding a shared agent must upgrade human_group -> mixed_group"
    );

    // Owner re-invites Ben after the earlier removal + agent add.
    room_ok(
        &rig,
        &owner,
        "POST",
        &format!("{path}/members"),
        Some(json!({ "human_subject": "ben" })),
    );

    // Owner sees full audit trail; rejoined Ben sees only events from their rejoin cutoff forward.
    let owner_page = room_ok(&rig, &owner, "GET", &path, None);
    let owner_events = owner_page["events"].as_array().unwrap();
    assert_eq!(
        owner_events.len(),
        3,
        "owner sees remove + agent add + rejoin add: {owner_events:?}"
    );
    let ben_page = room_ok(&rig, &ben, "GET", &path, None);
    let ben_events = ben_page["events"].as_array().unwrap();
    assert_eq!(
        ben_events.len(),
        1,
        "rejoined member must not see pre-rejoin audit events: {ben_events:?}"
    );
    assert!(
        ben_events[0]["text"]
            .as_str()
            .unwrap()
            .contains("added Ben"),
        "{ben_events:?}"
    );
    assert_eq!(
        ben_page["posts"].as_array().unwrap().len(),
        0,
        "audit events must not pollute topic_posts"
    );
}

#[test]
fn cad2711_personal_assistant_excluded_from_catalog_create_and_invite() {
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer, _events) = queue_observer();
    let rig = grant_rig(
        Arc::new(AtomicBool::new(true)),
        roster.clone(),
        observer,
        None,
        None,
    );
    let _keys = initialize_queue_keyring(&rig);
    let ana = login(&rig, "ana", "member");
    roster.lock().unwrap().allowed_cookies = vec![ana.clone()];

    // assist-agent-ben is bound in `assistants`; shareable-worker is not.
    let assist_ben_uid = register_shareable_agent(&rig, "assist-agent-ben");
    let shareable_uid = register_shareable_agent(&rig, "shareable-worker");

    let catalog = room_ok(&rig, &ana, "GET", "/api/human-threads/agents", None);
    let agents = catalog["agents"].as_array().unwrap();
    assert!(
        agents
            .iter()
            .any(|a| a["uid"] == shareable_uid && a["alias"] == "shareable-worker"),
        "{agents:?}"
    );
    assert!(
        !agents.iter().any(|a| a["alias"] == "assist-agent-ben"),
        "personal assistant must never appear in shared channel agent catalog: {agents:?}"
    );

    let (create_status, create_err) = room_http(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "mixed_group",
            "title": "Blocked Assistant",
            "client_id": "bad-asst-create",
            "humans": [],
            "agents": [assist_ben_uid]
        })),
    );
    assert_eq!(create_status, 400, "{create_err}");
    assert_eq!(create_err["code"], "personal_assistant_excluded");

    let group = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "human_group",
            "title": "Good Channel",
            "client_id": "good-asst-invite-base",
            "humans": [],
            "agents": []
        })),
    );
    let room = group["room"].as_str().unwrap();
    let (inv_status, inv_err) = room_http(
        &rig,
        &ana,
        "POST",
        &format!("/api/human-threads/rooms/{room}/members"),
        Some(json!({ "agent_uid": assist_ben_uid })),
    );
    assert_eq!(inv_status, 400, "{inv_err}");
    assert_eq!(inv_err["code"], "personal_assistant_excluded");
}

#[test]
fn cad2711_dm_refuses_agents_and_mentions_return_blocked_dm() {
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer, _events) = queue_observer();
    let rig = grant_rig(
        Arc::new(AtomicBool::new(true)),
        roster.clone(),
        observer,
        None,
        None,
    );
    let _keys = initialize_queue_keyring(&rig);
    let ana = login(&rig, "ana", "member");
    let ben = login(&rig, "ben", "member");
    roster.lock().unwrap().allowed_cookies = vec![ana.clone(), ben];
    let dm_worker_uid = register_shareable_agent(&rig, "dm-worker");

    let dm = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/dm",
        Some(json!({ "peer_subject": "ben" })),
    );
    let room = dm["room"].as_str().unwrap();

    let (add_status, add_err) = room_http(
        &rig,
        &ana,
        "POST",
        &format!("/api/human-threads/rooms/{room}/members"),
        Some(json!({ "agent_uid": dm_worker_uid })),
    );
    assert_eq!(add_status, 400, "{add_err}");
    assert_eq!(add_err["code"], "dm_agents_refused");

    let posted = room_post(
        &rig,
        &ana,
        room,
        "dm-mention-agent",
        "Hey @dm-worker join us",
    );
    let outcomes = posted["post"]["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert_eq!(outcomes[0]["kind"], "blocked_dm");
    let msgs: i64 = room_fixture_db(&rig)
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(msgs, 0, "1:1 DM must never queue a shared agent");
}

#[test]
fn cad2711_silent_note_and_flag_suppress_agent_queue_and_bell_notifications() {
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer, _events) = queue_observer();
    let rig = grant_rig(
        Arc::new(AtomicBool::new(true)),
        roster.clone(),
        observer,
        None,
        None,
    );
    let _keys = initialize_queue_keyring(&rig);
    let ana = login(&rig, "ana", "member");
    let ben = login(&rig, "ben", "member");
    roster.lock().unwrap().allowed_cookies = vec![ana.clone(), ben];
    let worker_uid = register_shareable_agent(&rig, "silent-worker");

    let group = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "mixed_group",
            "title": "Silent Notes",
            "client_id": "silent-notes-room",
            "humans": ["ben"],
            "agents": [worker_uid]
        })),
    );
    let room = group["room"].as_str().unwrap();
    let path = format!("/api/human-threads/rooms/{room}/posts");

    let note_cmd = room_ok(
        &rig,
        &ana,
        "POST",
        &path,
        Some(json!({
            "client_id": "note-slash",
            "body": "/note @silent-worker @ben quiet heads up"
        })),
    );
    assert_eq!(note_cmd["post"]["silent"], true);
    for o in note_cmd["post"]["outcomes"].as_array().unwrap() {
        assert_eq!(o["kind"], "silent", "{o}");
    }

    let note_flag = room_ok(
        &rig,
        &ana,
        "POST",
        &path,
        Some(json!({
            "client_id": "note-flag",
            "body": "@silent-worker @ben explicit flag",
            "silent": true
        })),
    );
    assert_eq!(note_flag["post"]["silent"], true);
    for o in note_flag["post"]["outcomes"].as_array().unwrap() {
        assert_eq!(o["kind"], "silent", "{o}");
    }

    let db = room_fixture_db(&rig);
    let msgs: i64 = db
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    let intents: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM human_notification_intents WHERE room=?1",
            [room],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(msgs, 0, "silent posts must queue zero agent turns");
    assert_eq!(
        intents, 0,
        "silent posts must create zero bell/push intents"
    );
}

#[test]
fn cad2711_channel_mention_preserves_caller_authority_never_escalating_member() {
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer, _events) = queue_observer();
    let rig = grant_rig(
        Arc::new(AtomicBool::new(true)),
        roster.clone(),
        observer,
        None,
        None,
    );
    let _keys = initialize_queue_keyring(&rig);
    let owner = login(&rig, "grant-owner", "owner");
    let ana = login(&rig, "ana", "member");
    {
        let mut r = roster.lock().unwrap();
        r.owner_cookie = owner.clone();
        r.allowed_cookies = vec![ana.clone()];
    }
    let worker_uid = register_shareable_agent(&rig, "auth-worker");
    let ana_actor =
        crate::store::human_threads::VerifiedHuman::verified(&rig.issuer, ORG, "ana").unwrap();
    let owner_actor =
        crate::store::human_threads::VerifiedHuman::verified(&rig.issuer, ORG, "grant-owner")
            .unwrap();

    let group = room_ok(
        &rig,
        &owner,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "mixed_group",
            "title": "Authority Separation",
            "client_id": "auth-sep-room",
            "humans": ["ana"],
            "agents": [worker_uid]
        })),
    );
    let room = group["room"].as_str().unwrap();

    let mem_post = room_post(&rig, &ana, room, "mem-ask-1", "Member ask @auth-worker");
    assert_eq!(mem_post["post"]["outcomes"][0]["kind"], "queued");
    let owner_post = room_post(&rig, &owner, room, "own-ask-1", "Owner ask @auth-worker");
    assert_eq!(owner_post["post"]["outcomes"][0]["kind"], "queued");

    let db = room_fixture_db(&rig);
    let (mem_msg_id, mem_src, mem_sender, mem_actor, mem_mode): (
        String,
        String,
        String,
        String,
        String,
    ) = db
        .query_row(
            "SELECT m.id, m.source, COALESCE(m.sender,''), pr.origin_actor, pr.authority_mode
               FROM human_room_post_requests pr
               JOIN messages m ON m.id=pr.message_id
              WHERE pr.post_id=?1",
            [mem_post["post"]["id"].as_str().unwrap()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(mem_src, "member");
    assert_eq!(mem_sender, "member");
    assert_eq!(mem_actor, ana_actor.id);
    assert_eq!(mem_mode, "member");

    let mem_operator_entries: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM thread_entries WHERE message_id=?1 AND role='operator'",
            [mem_msg_id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        mem_operator_entries, 0,
        "member @mention must never record an operator thread entry"
    );

    let (own_msg_id, own_src, own_sender, own_actor, own_mode): (
        String,
        String,
        String,
        String,
        String,
    ) = db
        .query_row(
            "SELECT m.id, m.source, COALESCE(m.sender,''), pr.origin_actor, pr.authority_mode
               FROM human_room_post_requests pr
               JOIN messages m ON m.id=pr.message_id
              WHERE pr.post_id=?1",
            [owner_post["post"]["id"].as_str().unwrap()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(own_src, "operator");
    assert_eq!(own_sender, "operator");
    assert_eq!(own_actor, owner_actor.id);
    assert_eq!(own_mode, "owner");

    let own_entry_role: String = db
        .query_row(
            "SELECT role FROM thread_entries WHERE message_id=?1",
            [own_msg_id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        own_entry_role, "operator",
        "owner @mention must record operator role"
    );
}

#[test]
fn cad2711_human_turn_brake_blocks_third_agent_hop_until_human_resets() {
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer, _events) = queue_observer();
    let rig = grant_rig(
        Arc::new(AtomicBool::new(true)),
        roster.clone(),
        observer,
        None,
        None,
    );
    let _keys = initialize_queue_keyring(&rig);
    let ana = login(&rig, "ana", "member");
    roster.lock().unwrap().allowed_cookies = vec![ana.clone()];
    let hop_a_uid = register_shareable_agent(&rig, "hop-a");
    let hop_b_uid = register_shareable_agent(&rig, "hop-b");

    let group = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "mixed_group",
            "title": "Brake Test",
            "client_id": "brake-room-1",
            "humans": [],
            "agents": [hop_a_uid, hop_b_uid]
        })),
    );
    let room = group["room"].as_str().unwrap();
    let path = format!("/api/human-threads/rooms/{room}");
    let topic: String = room_fixture_db(&rig)
        .query_row(
            "SELECT topic_id FROM human_rooms WHERE id=?1",
            [room],
            |r| r.get(0),
        )
        .unwrap();

    let page0 = room_ok(&rig, &ana, "GET", &path, None);
    assert_eq!(page0["agent_chain_count"], 0);
    assert_eq!(page0["agent_chain_limit"], 2);

    // Hop 1: hop-a mentions @hop-b -> queued, chain_count=1.
    let hop1 = agent_topic_publish(&rig, "hop-a", &topic, "hop-1", "Over to you @hop-b")
        .expect("first agent turn must succeed");
    assert_eq!(hop1["chain_count"], 1);
    assert_eq!(hop1["chain_limit"], 2);
    assert_eq!(hop1["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(hop1["deliveries"][0]["alias"], "hop-b");

    // Hop 2: hop-b mentions @hop-a -> chain_count=2, @hop-a blocked_chain, 0 deliveries.
    let hop2 = agent_topic_publish(&rig, "hop-b", &topic, "hop-2", "Handing back @hop-a")
        .expect("second agent turn posts then engages brake for further @mentions");
    assert_eq!(hop2["chain_count"], 2);
    assert_eq!(hop2["deliveries"].as_array().unwrap().len(), 0);
    assert_eq!(hop2["outcomes"][0]["kind"], "blocked_chain");

    // Hop 3: 3rd agent turn before a human posts is refused with waiting_for_person.
    let hop3 = agent_topic_publish(&rig, "hop-a", &topic, "hop-3", "Trying past brake")
        .expect_err("3rd consecutive agent turn must be refused");
    assert_eq!(hop3.code(), Some("waiting_for_person"), "{hop3:?}");

    let paused = room_ok(&rig, &ana, "GET", &path, None);
    assert_eq!(paused["agent_chain_count"], 2);
    assert_eq!(paused["agent_chain_limit"], 2);

    // Human post resets the brake counter to 0.
    let reset = room_post(
        &rig,
        &ana,
        room,
        "human-reset-1",
        "Good work @hop-a continue",
    );
    assert_eq!(reset["post"]["outcomes"][0]["kind"], "queued");
    let resumed = room_ok(&rig, &ana, "GET", &path, None);
    assert_eq!(resumed["agent_chain_count"], 0);

    // Agent can publish again now that a human took a turn.
    let hop4 = agent_topic_publish(&rig, "hop-a", &topic, "hop-4", "Back on track")
        .expect("agent turn must resume after human post");
    assert_eq!(hop4["chain_count"], 1);
}
