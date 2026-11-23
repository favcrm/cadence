//! Independent gate acceptance tests for CAD-2711.
//!
//! Written independently from the implementation to exercise the real HTTP routes,
//! daemon RPC gates (`caller_gate`, `topics_rpc`, `human_threads_rpc`, `write_path`),
//! and durable SQLite/filesystem state in the board+daemon rig across all seven
//! gate and refusal surfaces.

use super::*;
use crate::test_seam::{scoped, Asserted};

fn indep_register_agent(rig: &Rig, alias: &str) -> String {
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

fn indep_agent_rpc(rig: &Rig, alias: &str, method: &str, params: Value) -> crate::Result<Value> {
    scoped(Asserted::Agent(alias.to_string()), || {
        crate::client::rpc(rig._dir.path(), method, params)
    })
}

fn indep_topic_publish(
    rig: &Rig,
    alias: &str,
    topic: &str,
    id: &str,
    text: &str,
) -> crate::Result<Value> {
    indep_agent_rpc(
        rig,
        alias,
        "topic_publish",
        json!({ "topic": topic, "text": text, "id": id }),
    )
}

fn indep_topic_read(rig: &Rig, alias: &str, topic: &str) -> crate::Result<Value> {
    indep_agent_rpc(
        rig,
        alias,
        "topic_read",
        json!({ "topic": topic, "cursor_format": "opaque-v1" }),
    )
}

fn indep_room_topic(rig: &Rig, room: &str) -> String {
    room_fixture_db(rig)
        .query_row(
            "SELECT topic_id FROM human_rooms WHERE id=?1",
            [room],
            |r| r.get(0),
        )
        .unwrap()
}

fn indep_latest_comment_mention_event(rig: &Rig) -> Value {
    let raw: String = room_fixture_db(rig)
        .query_row(
            "SELECT payload FROM events
              WHERE kind='issue_comment_mentions'
              ORDER BY seq DESC
              LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn cad2711_indep_member_turn_blocks_cross_channel_and_revoked_human_topic_ops() {
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
        let mut guard = roster.lock().unwrap();
        guard.owner_cookie = owner.clone();
        guard.allowed_cookies = vec![owner.clone(), ana.clone(), ben.clone()];
    }

    let worker_uid = indep_register_agent(&rig, "indep-scope-worker");
    let ana_actor =
        crate::store::human_threads::VerifiedHuman::verified(&rig.issuer, ORG, "ana").unwrap();

    // Workspace owner creates Channel A (Ana + worker) and Channel B (Ben + same worker).
    // Because Owner is creator of Channel A, Ana can later be removed cleanly.
    let group_a = room_ok(
        &rig,
        &owner,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "mixed_group",
            "title": "Scope Channel A",
            "client_id": "indep-scope-room-a",
            "humans": ["ana"],
            "agents": [worker_uid.clone()]
        })),
    );
    let group_b = room_ok(
        &rig,
        &owner,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "mixed_group",
            "title": "Scope Channel B",
            "client_id": "indep-scope-room-b",
            "humans": ["ben"],
            "agents": [worker_uid]
        })),
    );
    let room_a = group_a["room"].as_str().unwrap();
    let room_b = group_b["room"].as_str().unwrap();
    let topic_a = indep_room_topic(&rig, room_a);
    let topic_b = indep_room_topic(&rig, room_b);

    let post_a = room_post(
        &rig,
        &ana,
        room_a,
        "scope-mem-post-1",
        "Member ask @indep-scope-worker in A",
    );
    assert_room_post(&post_a["post"]);
    assert_eq!(post_a["post"]["outcomes"][0]["kind"], "queued");

    let db = room_fixture_db(&rig);
    let (msg_id, authority_mode): (String, String) = db
        .query_row(
            "SELECT m.id, pr.authority_mode
               FROM human_room_post_requests pr
               JOIN messages m ON m.id=pr.message_id
              WHERE pr.post_id=?1",
            [post_a["post"]["id"].as_str().unwrap()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(authority_mode, "member");

    db.execute(
        "UPDATE messages SET state='running' WHERE id=?1",
        [msg_id.as_str()],
    )
    .unwrap();

    // Positive control: originating channel A is readable & publishable while Ana is active.
    let read_a = indep_topic_read(&rig, "indep-scope-worker", &topic_a)
        .expect("topic_read on originating channel A must succeed while Ana is active");
    assert!(
        read_a["posts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["body"] == "Member ask @indep-scope-worker in A"),
        "expected Channel A post in topic_read: {read_a}"
    );
    indep_topic_publish(
        &rig,
        "indep-scope-worker",
        &topic_a,
        "scope-a-reply-1",
        "Working in Channel A",
    )
    .expect("topic_publish on originating channel A must succeed while Ana is active");

    // Cross-channel scoping refusal: even though worker IS an active member of Channel B,
    // both topic_read and topic_publish on topic_b must fail with member_authority_refused.
    let read_b_err = indep_topic_read(&rig, "indep-scope-worker", &topic_b)
        .expect_err("member turn from A must refuse topic_read on B");
    assert_eq!(
        read_b_err.code(),
        Some("member_authority_refused"),
        "{read_b_err:?}"
    );

    let pub_b_err = indep_topic_publish(
        &rig,
        "indep-scope-worker",
        &topic_b,
        "scope-b-escape-1",
        "Cross-channel escape attempt",
    )
    .expect_err("member turn from A must refuse topic_publish on B");
    assert_eq!(
        pub_b_err.code(),
        Some("member_authority_refused"),
        "{pub_b_err:?}"
    );

    let posts_in_b: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM topic_posts WHERE topic_id=?1",
            [topic_b.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(posts_in_b, 0, "refused cross-channel publish wrote to B");

    // Owner removes originating member Ana from Channel A while worker's turn is still running.
    room_ok(
        &rig,
        &owner,
        "DELETE",
        &format!(
            "/api/human-threads/rooms/{room_a}/members/human/{}",
            ana_actor.id
        ),
        None,
    );

    let revoked_read = indep_topic_read(&rig, "indep-scope-worker", &topic_a)
        .expect_err("topic_read on A must refuse once originating human is removed");
    assert_eq!(
        revoked_read.code(),
        Some("member_authority_refused"),
        "{revoked_read:?}"
    );

    let revoked_pub = indep_topic_publish(
        &rig,
        "indep-scope-worker",
        &topic_a,
        "scope-a-revoked-reply",
        "Reply after Ana removal",
    )
    .expect_err("topic_publish on A must refuse once originating human is removed");
    assert_eq!(
        revoked_pub.code(),
        Some("member_authority_refused"),
        "{revoked_pub:?}"
    );

    let leaked_reply: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM topic_posts
              WHERE topic_id=?1 AND body='Reply after Ana removal'",
            [topic_a.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leaked_reply, 0, "revoked member turn published into A");
}

#[test]
fn cad2711_indep_member_turn_blocks_privileged_rpcs_directly_and_across_hop() {
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

    let agent1_uid = indep_register_agent(&rig, "indep-hop-one");
    let agent2_uid = indep_register_agent(&rig, "indep-hop-two");
    let _bystander_uid = indep_register_agent(&rig, "indep-bystander");
    let ana_actor =
        crate::store::human_threads::VerifiedHuman::verified(&rig.issuer, ORG, "ana").unwrap();

    let group = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "mixed_group",
            "title": "Hop Taint Check",
            "client_id": "indep-hop-taint-room",
            "humans": [],
            "agents": [agent1_uid, agent2_uid]
        })),
    );
    let room = group["room"].as_str().unwrap();
    let topic = indep_room_topic(&rig, room);

    let first_post = room_post(&rig, &ana, room, "hop-taint-post-1", "Start @indep-hop-one");
    assert_room_post(&first_post["post"]);
    assert_eq!(first_post["post"]["outcomes"][0]["kind"], "queued");

    let db = room_fixture_db(&rig);
    let (msg1_id, mode1): (String, String) = db
        .query_row(
            "SELECT m.id, pr.authority_mode
               FROM human_room_post_requests pr
               JOIN messages m ON m.id=pr.message_id
              WHERE pr.post_id=?1",
            [first_post["post"]["id"].as_str().unwrap()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(mode1, "member");

    db.execute(
        "UPDATE messages SET state='running' WHERE id=?1",
        [msg1_id.as_str()],
    )
    .unwrap();

    // Direct member turn must refuse privileged send/answer/register RPCs.
    for (method, params) in [
        (
            "agent_answer",
            json!({ "alias": "indep-bystander", "choice": "1" }),
        ),
        (
            "agent_send",
            json!({ "alias": "indep-bystander", "text": "escalate", "message": "indep-priv-1" }),
        ),
        (
            "cli_message_send",
            json!({ "alias": "master", "text": "escalate", "message": "indep-cli-1" }),
        ),
    ] {
        let err = indep_agent_rpc(&rig, "indep-hop-one", method, params)
            .expect_err("direct member turn must refuse privileged RPC");
        assert_eq!(
            err.code(),
            Some("member_authority_refused"),
            "{method} returned unexpected error: {err:?}"
        );
    }

    // Allowed topic_publish hops to @indep-hop-two while preserving member taint.
    let hop = indep_topic_publish(
        &rig,
        "indep-hop-one",
        &topic,
        "indep-hop-msg-1",
        "Handing over @indep-hop-two",
    )
    .expect("topic_publish hop within originating channel must succeed");
    assert_eq!(hop["deliveries"].as_array().unwrap().len(), 1);
    let msg2_id = hop["deliveries"][0]["message"].as_str().unwrap();

    let (hop_source, hop_actor, hop_mode): (String, String, String) = db
        .query_row(
            "SELECT m.source, pr.origin_actor, pr.authority_mode
               FROM human_room_post_requests pr
               JOIN messages m ON m.id=pr.message_id
              WHERE pr.message_id=?1",
            [msg2_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(hop_source, "member");
    assert_eq!(hop_actor, ana_actor.id);
    assert_eq!(hop_mode, "member");

    db.execute(
        "UPDATE messages SET state='completed' WHERE id=?1",
        [msg1_id.as_str()],
    )
    .unwrap();
    db.execute("UPDATE messages SET state='running' WHERE id=?1", [msg2_id])
        .unwrap();

    // Agent 2 running the hopped turn inherits member taint and refuses the same RPCs.
    for (method, params) in [
        (
            "agent_answer",
            json!({ "alias": "indep-bystander", "choice": "1" }),
        ),
        (
            "agent_send",
            json!({ "alias": "indep-bystander", "text": "hop escalate", "message": "indep-priv-2" }),
        ),
        (
            "cli_message_send",
            json!({ "alias": "master", "text": "hop escalate", "message": "indep-cli-2" }),
        ),
    ] {
        let err = indep_agent_rpc(&rig, "indep-hop-two", method, params)
            .expect_err("hopped member turn must also refuse privileged RPC");
        assert_eq!(
            err.code(),
            Some("member_authority_refused"),
            "hop {method} returned unexpected error: {err:?}"
        );
    }
}

#[test]
fn cad2711_indep_personal_assistant_and_master_excluded_from_catalog_create_and_add() {
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
    roster.lock().unwrap().allowed_cookies = vec![ana.clone(), ben.clone()];

    // grant_rig already bound assist-agent-ana and assist-agent-ben in `assistants`.
    let shareable_uid = indep_register_agent(&rig, "indep-public-worker");
    let assist_ana_uid = indep_register_agent(&rig, "assist-agent-ana");
    let assist_ben_uid = indep_register_agent(&rig, "assist-agent-ben");
    let master_uid = indep_register_agent(&rig, "master");

    for cookie in [&ana, &ben] {
        let catalog = room_ok(&rig, cookie, "GET", "/api/human-threads/agents", None);
        let agents = catalog["agents"].as_array().unwrap();
        assert!(
            agents
                .iter()
                .any(|a| a["uid"] == shareable_uid && a["alias"] == "indep-public-worker"),
            "shareable agent missing from catalog: {agents:?}"
        );
        for excluded in ["assist-agent-ana", "assist-agent-ben", "master"] {
            assert!(
                !agents.iter().any(|a| a["alias"] == excluded),
                "excluded agent {excluded} leaked into catalog: {agents:?}"
            );
        }
    }

    let db = room_fixture_db(&rig);
    let before_rooms: i64 = db
        .query_row("SELECT COUNT(*) FROM human_rooms", [], |r| r.get(0))
        .unwrap();

    for (label, blocked_uid) in [
        ("assist-ana", &assist_ana_uid),
        ("assist-ben", &assist_ben_uid),
        ("master", &master_uid),
    ] {
        let (status, err) = room_http(
            &rig,
            &ana,
            "POST",
            "/api/human-threads/groups",
            Some(json!({
                "kind": "mixed_group",
                "title": "Blocked Assistant Group",
                "client_id": format!("indep-blocked-create-{label}"),
                "humans": [],
                "agents": [blocked_uid]
            })),
        );
        assert_eq!(status, 400, "{label} create status {status}: {err}");
        assert_eq!(err["code"], "personal_assistant_excluded", "{err}");
    }

    let after_refused_creates: i64 = db
        .query_row("SELECT COUNT(*) FROM human_rooms", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after_refused_creates, before_rooms);

    let room = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "human_group",
            "title": "Clean Channel",
            "client_id": "indep-clean-group-for-add",
            "humans": ["ben"],
            "agents": []
        })),
    )["room"]
        .as_str()
        .unwrap()
        .to_string();

    for (label, blocked_uid) in [
        ("assist-ana", &assist_ana_uid),
        ("assist-ben", &assist_ben_uid),
        ("master", &master_uid),
    ] {
        let (status, err) = room_http(
            &rig,
            &ana,
            "POST",
            &format!("/api/human-threads/rooms/{room}/members"),
            Some(json!({ "agent_uid": blocked_uid })),
        );
        assert_eq!(status, 400, "{label} add status {status}: {err}");
        assert_eq!(err["code"], "personal_assistant_excluded", "{err}");
    }

    let agent_members: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM human_room_members WHERE room=?1 AND kind='agent'",
            [room.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        agent_members, 0,
        "excluded assistant must not be added to room"
    );

    // Positive control: shareable agent can be added.
    room_ok(
        &rig,
        &ana,
        "POST",
        &format!("/api/human-threads/rooms/{room}/members"),
        Some(json!({ "agent_uid": shareable_uid })),
    );
}

#[test]
fn cad2711_indep_non_creator_non_owner_member_add_and_remove_refused_403() {
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
        let mut guard = roster.lock().unwrap();
        guard.owner_cookie = owner.clone();
        guard.allowed_cookies = vec![owner.clone(), ana.clone(), ben.clone()];
    }

    let worker_uid = indep_register_agent(&rig, "indep-mgr-worker");
    let ana_actor =
        crate::store::human_threads::VerifiedHuman::verified(&rig.issuer, ORG, "ana").unwrap();
    let owner_actor =
        crate::store::human_threads::VerifiedHuman::verified(&rig.issuer, ORG, "grant-owner")
            .unwrap();

    // Creator Ana creates channel with ordinary member Ben and workspace owner.
    let room = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "human_group",
            "title": "Manager Gate Room",
            "client_id": "indep-mgr-gate-room",
            "humans": ["ben", "grant-owner"],
            "agents": []
        })),
    )["room"]
        .as_str()
        .unwrap()
        .to_string();

    let path = format!("/api/human-threads/rooms/{room}");
    assert_eq!(room_ok(&rig, &ben, "GET", &path, None)["can_manage"], false);
    assert_eq!(room_ok(&rig, &ana, "GET", &path, None)["can_manage"], true);
    assert_eq!(
        room_ok(&rig, &owner, "GET", &path, None)["can_manage"],
        true
    );

    let (add_agent_status, add_agent_err) = room_http(
        &rig,
        &ben,
        "POST",
        &format!("{path}/members"),
        Some(json!({ "agent_uid": worker_uid })),
    );
    assert_eq!(add_agent_status, 403, "{add_agent_err}");
    assert_eq!(add_agent_err["code"], "not_room_manager");

    let (add_human_status, add_human_err) = room_http(
        &rig,
        &ben,
        "POST",
        &format!("{path}/members"),
        Some(json!({ "human_subject": "ben" })),
    );
    assert_eq!(add_human_status, 403, "{add_human_err}");
    assert_eq!(add_human_err["code"], "not_room_manager");

    for target_id in [&owner_actor.id, &ana_actor.id] {
        let (del_status, del_err) = room_http(
            &rig,
            &ben,
            "DELETE",
            &format!("{path}/members/human/{target_id}"),
            None,
        );
        assert_eq!(del_status, 403, "{del_err}");
        assert_eq!(del_err["code"], "not_room_manager");
    }

    let db = room_fixture_db(&rig);
    let active_members: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM human_room_members WHERE room=?1 AND removed IS NULL",
            [room.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(active_members, 3);
    let events: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM human_room_events WHERE room=?1",
            [room.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(events, 0);
}

#[test]
fn cad2711_indep_dm_agent_mention_blocked_and_replay_silent_mismatch_refused() {
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
    roster.lock().unwrap().allowed_cookies = vec![ana.clone(), ben.clone()];
    let worker_uid = indep_register_agent(&rig, "indep-dm-worker");

    let dm_room = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/dm",
        Some(json!({ "peer_subject": "ben" })),
    )["room"]
        .as_str()
        .unwrap()
        .to_string();

    let dm_post = room_post(
        &rig,
        &ana,
        &dm_room,
        "indep-dm-mention-1",
        "Hey @indep-dm-worker join our DM",
    );
    assert_room_post(&dm_post["post"]);
    let dm_outcomes = dm_post["post"]["outcomes"].as_array().unwrap();
    assert_eq!(dm_outcomes.len(), 1, "{dm_outcomes:?}");
    assert_eq!(dm_outcomes[0]["kind"], "blocked_dm");

    let db = room_fixture_db(&rig);
    let queued_after_dm: i64 = db
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(queued_after_dm, 0, "1:1 DM mention must queue 0 messages");

    // Create a channel to verify client_id replay silent-flag contradiction refusal.
    let grp_room = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "mixed_group",
            "title": "Replay Silent Guard",
            "client_id": "indep-replay-guard-room",
            "humans": ["ben"],
            "agents": [worker_uid]
        })),
    )["room"]
        .as_str()
        .unwrap()
        .to_string();
    let posts_url = format!("/api/human-threads/rooms/{grp_room}/posts");

    // Case 1: non-silent first -> contradictory silent: true or /note replay refused.
    let first = room_ok(
        &rig,
        &ana,
        "POST",
        &posts_url,
        Some(json!({
            "client_id": "indep-replay-1",
            "body": "@indep-dm-worker check replay",
            "silent": false
        })),
    );
    let post1_id = first["post"]["id"].as_str().unwrap();
    assert_eq!(first["post"]["silent"], false);
    assert_eq!(first["post"]["outcomes"][0]["kind"], "queued");

    let (meta1_silent, meta1_outcomes): (i64, String) = db
        .query_row(
            "SELECT silent, outcomes_json FROM human_room_post_meta WHERE post_id=?1",
            [post1_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(meta1_silent, 0);

    let (replay_status, replay_err) = room_http(
        &rig,
        &ana,
        "POST",
        &posts_url,
        Some(json!({
            "client_id": "indep-replay-1",
            "body": "@indep-dm-worker check replay",
            "silent": true
        })),
    );
    assert!(
        (400..500).contains(&replay_status),
        "contradictory silent replay must return 4xx: {replay_status} {replay_err}"
    );

    let (after_silent, after_outcomes): (i64, String) = db
        .query_row(
            "SELECT silent, outcomes_json FROM human_room_post_meta WHERE post_id=?1",
            [post1_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(after_silent, 0, "replay must not mutate meta silent flag");
    assert_eq!(
        after_outcomes, meta1_outcomes,
        "replay must not mutate meta outcomes_json"
    );

    // Case 2: silent first -> contradictory silent: false replay refused without mutation.
    let second = room_ok(
        &rig,
        &ana,
        "POST",
        &posts_url,
        Some(json!({
            "client_id": "indep-replay-2",
            "body": "Quiet initial message",
            "silent": true
        })),
    );
    let post2_id = second["post"]["id"].as_str().unwrap();
    assert_eq!(second["post"]["silent"], true);

    let (rev_status, rev_err) = room_http(
        &rig,
        &ana,
        "POST",
        &posts_url,
        Some(json!({
            "client_id": "indep-replay-2",
            "body": "Quiet initial message",
            "silent": false
        })),
    );
    assert!(
        (400..500).contains(&rev_status),
        "silent->non-silent replay must return 4xx: {rev_status} {rev_err}"
    );
    let meta2_silent: i64 = db
        .query_row(
            "SELECT silent FROM human_room_post_meta WHERE post_id=?1",
            [post2_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(meta2_silent, 1, "silent flag mutated after refused replay");
}

#[test]
fn cad2711_indep_human_turn_brake_enforces_limit_and_resets_on_human_post() {
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

    let uid1 = indep_register_agent(&rig, "indep-brake-1");
    let uid2 = indep_register_agent(&rig, "indep-brake-2");

    let room = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "mixed_group",
            "title": "Human Turn Brake",
            "client_id": "indep-brake-room",
            "humans": [],
            "agents": [uid1, uid2]
        })),
    )["room"]
        .as_str()
        .unwrap()
        .to_string();
    let room_url = format!("/api/human-threads/rooms/{room}");
    let topic = indep_room_topic(&rig, &room);

    // Originating human post starts the turn at chain count 0.
    let origin = room_post(&rig, &ana, &room, "brake-origin-1", "Begin @indep-brake-1");
    assert_eq!(origin["post"]["outcomes"][0]["kind"], "queued");
    let state0 = room_ok(&rig, &ana, "GET", &room_url, None);
    assert_eq!(state0["agent_chain_count"], 0);
    assert_eq!(state0["agent_chain_limit"], 2);

    // Agent hop 1 (chain_count = 1 < 2): @indep-brake-2 is queued.
    let hop1 = indep_topic_publish(
        &rig,
        "indep-brake-1",
        &topic,
        "indep-brake-hop-1",
        "Tagging @indep-brake-2",
    )
    .expect("hop 1 must succeed");
    assert_eq!(hop1["chain_count"], 1);
    assert_eq!(hop1["chain_limit"], 2);
    assert_eq!(hop1["outcomes"][0]["kind"], "queued");
    assert_eq!(hop1["deliveries"].as_array().unwrap().len(), 1);

    // Agent hop 2 (chain_count reaches 2 == limit): post lands, @indep-brake-1 is blocked_chain.
    let hop2 = indep_topic_publish(
        &rig,
        "indep-brake-2",
        &topic,
        "indep-brake-hop-2",
        "Returning @indep-brake-1",
    )
    .expect("hop 2 posts with blocked_chain outcome");
    assert_eq!(hop2["chain_count"], 2);
    assert_eq!(hop2["outcomes"][0]["kind"], "blocked_chain");
    assert_eq!(hop2["deliveries"].as_array().unwrap().len(), 0);

    // 3rd consecutive agent publish before a human posts refuses outright with waiting_for_person.
    let hop3_err = indep_topic_publish(
        &rig,
        "indep-brake-1",
        &topic,
        "indep-brake-hop-3",
        "Blocked turn @indep-brake-2",
    )
    .expect_err("3rd consecutive agent publish must refuse");
    assert_eq!(hop3_err.code(), Some("waiting_for_person"), "{hop3_err:?}");

    let db = room_fixture_db(&rig);
    let msgs_before_reset: i64 = db
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(msgs_before_reset, 2, "only origin + hop 1 should queue");

    // Subsequent human post resets the brake counter to 0.
    let reset = room_post(
        &rig,
        &ana,
        &room,
        "brake-reset-1",
        "Human unblock @indep-brake-1",
    );
    assert_eq!(reset["post"]["outcomes"][0]["kind"], "queued");
    assert_eq!(
        room_ok(&rig, &ana, "GET", &room_url, None)["agent_chain_count"],
        0
    );

    // Next agent-to-agent @mention queues again.
    let after_reset = indep_topic_publish(
        &rig,
        "indep-brake-1",
        &topic,
        "indep-brake-hop-4",
        "Resumed @indep-brake-2",
    )
    .expect("agent turn must resume after human reset");
    assert_eq!(after_reset["chain_count"], 1);
    assert_eq!(after_reset["outcomes"][0]["kind"], "queued");
    assert_eq!(after_reset["deliveries"].as_array().unwrap().len(), 1);
}

#[test]
fn cad2711_indep_ticket_comment_mentions_silent_note_human_isolation_and_agent_gate() {
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
    seed(&rig);
    let _keys = initialize_queue_keyring(&rig);
    let ana = login(&rig, "ana", "member");
    let ben = login(&rig, "ben", "member");
    roster.lock().unwrap().allowed_cookies = vec![ana.clone(), ben.clone()];

    let worker_uid = indep_register_agent(&rig, "indep-ticket-worker");
    let _assist_ben_uid = indep_register_agent(&rig, "assist-agent-ben");
    let _bystander_uid = indep_register_agent(&rig, "indep-ticket-bystander");
    let ana_actor =
        crate::store::human_threads::VerifiedHuman::verified(&rig.issuer, ORG, "ana").unwrap();

    // Channel A includes indep-ticket-worker to verify a ticket-origin member turn
    // (topic_id = None) cannot read or publish to unrelated channel A either.
    let room_a = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "mixed_group",
            "title": "Unrelated Channel",
            "client_id": "indep-ticket-unrelated-room",
            "humans": [],
            "agents": [worker_uid]
        })),
    )["room"]
        .as_str()
        .unwrap()
        .to_string();
    let topic_a = indep_room_topic(&rig, &room_a);

    // 1. Member posts @indep-ticket-worker on accessible issue VIS-1 via real HTTP board route.
    let (s1, r1) = room_http(
        &rig,
        &ana,
        "POST",
        "/api/issues/VIS-1/comments",
        Some(json!({ "body": "Triage @indep-ticket-worker please review VIS-1" })),
    );
    assert_eq!(s1, 200, "member issue comment failed: {r1}");
    assert!(
        r1["issue"]["comments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["body"] == "Triage @indep-ticket-worker please review VIS-1"),
        "committed comment missing from issue detail: {r1}"
    );
    let comment_files = std::fs::read_dir(rig.pm.join("visible/VIS-1/comments"))
        .unwrap()
        .count();
    assert_eq!(comment_files, 1, "expected 1 comment file written to pm");

    let ev1 = indep_latest_comment_mention_event(&rig);
    assert_eq!(ev1["issue"], "VIS-1");
    assert_eq!(ev1["origin_actor"], ana_actor.id);
    assert_eq!(ev1["silent"], false);
    assert_eq!(ev1["outcomes"].as_array().unwrap().len(), 1);
    assert_eq!(ev1["outcomes"][0]["kind"], "queued");
    assert_eq!(ev1["outcomes"][0]["handle"], "@indep-ticket-worker");

    let db = room_fixture_db(&rig);
    let (msg_id, msg_src, msg_sender, req_actor, req_mode, linked_topic): (
        String,
        String,
        String,
        String,
        String,
        Option<String>,
    ) = db
        .query_row(
            "SELECT m.id, m.source, COALESCE(m.sender,''), pr.origin_actor,
                    pr.authority_mode, tp.topic_id
               FROM human_room_post_requests pr
               JOIN messages m ON m.id=pr.message_id
               LEFT JOIN topic_posts tp ON tp.id=pr.post_id
              WHERE m.alias='indep-ticket-worker'",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(msg_src, "member");
    assert_eq!(msg_sender, "member");
    assert_eq!(req_actor, ana_actor.id);
    assert_eq!(req_mode, "member");
    assert!(
        linked_topic.is_none(),
        "ticket mention must have no topic_id"
    );

    // Put the queued ticket-origin turn into `running`: verify member_authority_refused
    // blocks both privileged RPCs AND topic_read/topic_publish on Channel A.
    db.execute(
        "UPDATE messages SET state='running' WHERE id=?1",
        [msg_id.as_str()],
    )
    .unwrap();

    for (method, params) in [
        (
            "agent_answer",
            json!({ "alias": "indep-ticket-bystander", "choice": "1" }),
        ),
        (
            "agent_send",
            json!({ "alias": "indep-ticket-bystander", "text": "escalate", "message": "tkt-esc-1" }),
        ),
        (
            "topic_read",
            json!({ "topic": topic_a, "cursor_format": "opaque-v1" }),
        ),
        (
            "topic_publish",
            json!({ "topic": topic_a, "id": "tkt-topic-esc-1", "text": "escape into channel" }),
        ),
    ] {
        let err = indep_agent_rpc(&rig, "indep-ticket-worker", method, params)
            .expect_err("running member ticket turn must refuse privileged/channel RPC");
        assert_eq!(
            err.code(),
            Some("member_authority_refused"),
            "ticket turn {method} returned wrong error: {err:?}"
        );
    }

    db.execute(
        "UPDATE messages SET state='completed' WHERE id=?1",
        [msg_id.as_str()],
    )
    .unwrap();

    // 2. `/note @indep-ticket-worker ...` records silent=true and queues 0 messages.
    let (s2, r2) = room_http(
        &rig,
        &ana,
        "POST",
        "/api/issues/VIS-1/comments",
        Some(json!({ "body": "/note @indep-ticket-worker @ben quiet ticket note" })),
    );
    assert_eq!(s2, 200, "silent /note issue comment failed: {r2}");
    assert!(
        r2["issue"]["comments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["body"] == "/note @indep-ticket-worker @ben quiet ticket note"),
        "/note comment missing from issue detail: {r2}"
    );
    let ev2 = indep_latest_comment_mention_event(&rig);
    assert_eq!(ev2["silent"], true, "expected silent=true event: {ev2}");
    for outcome in ev2["outcomes"].as_array().unwrap() {
        assert_eq!(outcome["kind"], "silent", "{outcome}");
    }
    let count_after_note: i64 = db
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count_after_note, 1, "/note must queue 0 new messages");

    // 3. `@ben` human mention notifies Ben cleanly and queues 0 messages to assist-agent-ben.
    let (s3, r3) = room_http(
        &rig,
        &ana,
        "POST",
        "/api/issues/VIS-1/comments",
        Some(json!({ "body": "Looping in @ben for visibility" })),
    );
    assert_eq!(s3, 200, "human mention issue comment failed: {r3}");
    let ev3 = indep_latest_comment_mention_event(&rig);
    assert_eq!(ev3["silent"], false);
    assert_eq!(ev3["outcomes"].as_array().unwrap().len(), 1);
    assert_eq!(ev3["outcomes"][0]["kind"], "notified");
    assert_eq!(ev3["outcomes"][0]["label"], "Ben");

    let assist_msgs: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE alias='assist-agent-ben'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(assist_msgs, 0, "human mention must never queue assistant");
    let count_after_human: i64 = db
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count_after_human, 1);

    // 4a. Direct agent call to issue_comment_mentions with valid schema fields (no `actor`)
    // is refused by the operator connection gate and queues 0 messages.
    let agent_err = indep_agent_rpc(
        &rig,
        "indep-ticket-worker",
        "issue_comment_mentions",
        json!({
            "issue_id": "VIS-1",
            "comment_key": "forged-key",
            "body": "Forged @indep-ticket-worker wake",
            "local_operator": true
        }),
    )
    .expect_err("agent caller must never invoke issue_comment_mentions");
    assert_eq!(agent_err.kind(), "rejected", "{agent_err:?}");
    assert!(
        agent_err.to_string().contains("operator"),
        "unexpected error for agent issue_comment_mentions: {agent_err:?}"
    );
    let count_after_agent: i64 = db
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count_after_agent, 1);

    // 4b. Supplying a forged `actor` field is refused as connection-bound and queues 0 messages.
    let forged_actor_err = indep_agent_rpc(
        &rig,
        "indep-ticket-worker",
        "issue_comment_mentions",
        json!({
            "issue_id": "VIS-1",
            "comment_key": "forged-actor-key",
            "body": "Forged @indep-ticket-worker actor wake",
            "local_operator": true,
            "actor": "operator"
        }),
    )
    .expect_err("forged actor field on issue_comment_mentions must be refused");
    assert_eq!(forged_actor_err.kind(), "rejected", "{forged_actor_err:?}");
    let forged_msg = forged_actor_err.to_string();
    assert!(
        forged_msg.contains("actor") && forged_msg.contains("connection-bound"),
        "unexpected error for forged actor on issue_comment_mentions: {forged_actor_err:?}"
    );
    let count_after_forged: i64 = db
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count_after_forged, 1);
}
