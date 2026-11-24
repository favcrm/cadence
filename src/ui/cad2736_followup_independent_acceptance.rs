//! Independent gate and hardening acceptance tests for CAD-2736.
//!
//! Written independently from the implementation to exercise:
//! 1. Fail-closed `submitting`-state member-turn confinement (`member_authority_refused`)
//!    and `health` inflight redaction (`update_waiting`, `adopted_oldest`) before
//!    `on_started` transitions the turn to `running`.
//! 2. Pre-`human_room_save_post_meta` pending-meta enforcement (`outcomes_json = ''`):
//!    refusing `silent` replay flips without creating agent turns or bell intents,
//!    and recovering partial multi-agent dispatches on matching-`silent` retries.

use super::*;
use crate::test_seam::{scoped, Asserted};

fn cad2736_register_agent(rig: &Rig, alias: &str) -> String {
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

fn cad2736_agent_rpc(rig: &Rig, alias: &str, method: &str, params: Value) -> crate::Result<Value> {
    scoped(Asserted::Agent(alias.to_string()), || {
        crate::client::rpc(rig._dir.path(), method, params)
    })
}

#[test]
fn cad2736_indep_submitting_state_member_turn_enforces_gate_and_redacts_health() {
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
    let owner = login(&rig, "grant-owner", "owner");
    let ana = login(&rig, "ana", "member");
    {
        let mut guard = roster.lock().unwrap();
        guard.owner_cookie = owner.clone();
        guard.allowed_cookies = vec![owner.clone(), ana.clone()];
    }

    let worker_uid = cad2736_register_agent(&rig, "worker");
    let _owner_inflight_uid = cad2736_register_agent(&rig, "owner-inflight");
    let ana_actor =
        crate::store::human_threads::VerifiedHuman::verified(&rig.issuer, ORG, "ana").unwrap();

    // Unrelated running owner turn on another registered agent so `health`
    // would disclose `update_waiting` unless redacted during `submitting`.
    let owner_send = scoped(Asserted::Operator, || {
        crate::client::rpc(
            rig._dir.path(),
            "agent_send",
            json!({
                "alias": "owner-inflight",
                "text": "Unrelated running owner turn",
                "message": "cad2736-owner-inflight-msg"
            }),
        )
    })
    .expect("operator agent_send for inflight control must succeed");
    assert_eq!(owner_send["message"], "cad2736-owner-inflight-msg");

    let db = room_fixture_db(&rig);
    let owner_running = db
        .execute(
            "UPDATE messages SET state='running', started=1000.0
              WHERE id='cad2736-owner-inflight-msg'",
            [],
        )
        .unwrap();
    assert_eq!(owner_running, 1);

    // Mixed channel with admitted shared `@worker` and non-owner member `ana`.
    let room = room_ok(
        &rig,
        &owner,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "mixed_group",
            "title": "Submitting Gate Channel",
            "client_id": "cad2736-submitting-room",
            "humans": ["ana"],
            "agents": [worker_uid.clone()]
        })),
    )["room"]
        .as_str()
        .unwrap()
        .to_string();

    let posted = room_post(
        &rig,
        &ana,
        &room,
        "cad2736-submitting-post-1",
        "Verify submitting gate @worker",
    );
    assert_room_post(&posted["post"]);
    assert_eq!(posted["post"]["outcomes"][0]["kind"], "queued");
    let post_id = posted["post"]["id"].as_str().unwrap();

    let (msg_id, bound_uid, origin_actor, authority_mode, msg_state): (
        String,
        String,
        String,
        String,
        String,
    ) = db
        .query_row(
            "SELECT m.id, pr.agent_uid, pr.origin_actor, pr.authority_mode, m.state
               FROM human_room_post_requests pr
               JOIN messages m ON m.id=pr.message_id
              WHERE pr.post_id=?1",
            [post_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(bound_uid, worker_uid);
    assert_eq!(origin_actor, ana_actor.id);
    assert_eq!(authority_mode, "member");
    assert_eq!(msg_state, "queued");

    // Hold the member-authored message in `state = 'submitting'` prior to
    // `on_started` transitioning it to `'running'`.
    let marked_submitting = db
        .execute(
            "UPDATE messages SET state='submitting', started=1001.0 WHERE id=?1",
            [msg_id.as_str()],
        )
        .unwrap();
    assert_eq!(marked_submitting, 1);

    let scope = crate::memory::Scope {
        paths: vec!["src/daemon/**".into()],
        ..crate::memory::Scope::default()
    };
    let blocked_text = concat!(
        "Member proposal while submitting must be refused\n\n",
        "**Why:** test\n\n**How to apply:** test\n"
    );
    let propose_params = crate::memory::cli::propose_params(&crate::memory::cli::Proposal {
        project: Some("visible"),
        kind: "gotcha",
        scope: &scope,
        source: Some("VIS-1"),
        confidence: None,
        from: None,
        text: Some(blocked_text),
        id: Some("cad2736-submitting-blocked-prop"),
        nature: "judgement",
        audience: "project",
        roles: &[],
        evidence_refs: &[],
        verification_condition: None,
    });

    // Disallowed RPCs must fail closed with `member_authority_refused` while in `submitting`.
    for (method, params) in [
        ("agent_list", json!({})),
        ("agent_show", json!({ "alias": "worker" })),
        ("memory_propose", propose_params),
        ("platform_accounts", json!({})),
        (
            "agent_send",
            json!({
                "alias": "owner-inflight",
                "text": "escalate during submitting",
                "message": "cad2736-submitting-esc-1"
            }),
        ),
        ("thread_read", json!({ "alias": "worker" })),
    ] {
        let err = cad2736_agent_rpc(&rig, "worker", method, params)
            .expect_err("submitting member turn must refuse disallowed RPC");
        assert_eq!(err.kind(), "rejected", "{method} wrong kind: {err:?}");
        assert_eq!(
            err.code(),
            Some("member_authority_refused"),
            "{method} returned unexpected error: {err:?}"
        );
    }
    assert!(
        !rig.pm
            .join("visible/memory/cad2736-submitting-blocked-prop.md")
            .exists(),
        "refused memory_propose during submitting must not write a memory file"
    );

    // `health` succeeds for the submitting worker but redacts `update_waiting`
    // and `adopted_oldest` so unrelated owner/fleet activity cannot leak.
    let worker_health = cad2736_agent_rpc(&rig, "worker", "health", json!({}))
        .expect("health must remain readable during submitting member turn");
    assert_eq!(worker_health["state"], "ready");
    assert_eq!(worker_health["update_waiting"], json!([]));
    assert_eq!(worker_health["adopted_oldest"], json!([]));
    assert!(
        !worker_health
            .to_string()
            .contains("cad2736-owner-inflight-msg"),
        "worker health leaked unrelated inflight owner turn: {worker_health}"
    );

    // Positive controls: the operator sees the running owner turn in `update_waiting`
    // and is admitted on `platform_accounts`.
    let operator_health = scoped(Asserted::Operator, || {
        crate::client::rpc(rig._dir.path(), "health", json!({}))
    })
    .expect("health must succeed for operator");
    let waiting = operator_health["update_waiting"]
        .as_array()
        .expect("operator health update_waiting array");
    assert!(
        waiting.iter().any(|row| {
            row["alias"] == "owner-inflight"
                && row["message"] == "cad2736-owner-inflight-msg"
                && row["state"] == "running"
        }),
        "expected unrelated running owner turn in operator update_waiting: {waiting:?}"
    );
    scoped(Asserted::Operator, || {
        crate::client::rpc(rig._dir.path(), "platform_accounts", json!({}))
    })
    .expect("platform_accounts must succeed for operator");
}

#[test]
fn cad2736_indep_pending_post_meta_blocks_silent_flip_and_recovers_retry() {
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

    let dev_uid = cad2736_register_agent(&rig, "dev");
    let retry_a_uid = cad2736_register_agent(&rig, "retry-a");
    let retry_b_uid = cad2736_register_agent(&rig, "retry-b");
    let ana_actor =
        crate::store::human_threads::VerifiedHuman::verified(&rig.issuer, ORG, "ana").unwrap();

    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();

    // Part A: Silent replay flip before `human_room_save_post_meta` runs.
    // Include `ben` in the channel so any leaked non-silent path would emit bell intents.
    let silent_room = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "mixed_group",
            "title": "Silent Flip Guard",
            "client_id": "cad2736-silent-flip-room",
            "humans": ["ben"],
            "agents": [dev_uid]
        })),
    )["room"]
        .as_str()
        .unwrap()
        .to_string();
    let silent_posts_url = format!("/api/human-threads/rooms/{silent_room}/posts");

    // Simulate crash right after `insert_post` with a non-`/note` body and `silent: true`,
    // before `human_room_save_post_meta` finalizes `outcomes_json`.
    let (pending_post, created) = shared
        .store
        .human_room_post_with_silent(
            &ana_actor,
            &silent_room,
            "cad2736-silent-flip-1",
            "check @dev",
            true,
        )
        .expect("direct store silent post before save_post_meta must succeed");
    assert!(created);
    assert!(
        shared
            .store
            .human_room_post_meta(&pending_post.id)
            .unwrap()
            .is_none(),
        "pre-save_post_meta row with empty outcomes_json must report None (pending)"
    );

    let db = room_fixture_db(&rig);
    let (pre_silent, pre_outcomes_raw): (i64, String) = db
        .query_row(
            "SELECT silent, outcomes_json FROM human_room_post_meta WHERE post_id=?1",
            [pending_post.id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(pre_silent, 1);
    assert_eq!(pre_outcomes_raw, "");

    // Replaying with the same `client_id` and `body` but flipping `silent: false`
    // must be refused over HTTP (`400` / `"rejected"`) and on the store directly.
    let (flip_status, flip_err) = room_http(
        &rig,
        &ana,
        "POST",
        &silent_posts_url,
        Some(json!({
            "client_id": "cad2736-silent-flip-1",
            "body": "check @dev",
            "silent": false
        })),
    );
    assert_eq!(
        flip_status, 400,
        "flipping silent=true->false before save_post_meta must return 400: {flip_err}"
    );
    assert_eq!(flip_err["code"], "rejected", "{flip_err}");

    let store_flip_err = shared
        .store
        .human_room_post_with_silent(
            &ana_actor,
            &silent_room,
            "cad2736-silent-flip-1",
            "check @dev",
            false,
        )
        .expect_err("store replay flipping silent=true->false must be refused");
    assert_eq!(store_flip_err.kind(), "rejected", "{store_flip_err:?}");

    let dev_msgs: i64 = db
        .query_row("SELECT COUNT(*) FROM messages WHERE alias='dev'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        dev_msgs, 0,
        "refused silent flip must create zero @dev agent messages"
    );
    let silent_reqs: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM human_room_post_requests WHERE post_id=?1",
            [pending_post.id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        silent_reqs, 0,
        "refused silent flip must bind zero requests"
    );
    let bell_intents: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM human_notification_intents WHERE room=?1",
            [silent_room.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        bell_intents, 0,
        "silent post and refused silent->non-silent replay must emit zero bell notifications"
    );
    let (after_flip_silent, after_flip_outcomes_raw): (i64, String) = db
        .query_row(
            "SELECT silent, outcomes_json FROM human_room_post_meta WHERE post_id=?1",
            [pending_post.id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(after_flip_silent, 1);
    assert_eq!(after_flip_outcomes_raw, "");

    // Positive control: retrying with matching `silent: true` finalizes meta quietly.
    let matching_silent = room_ok(
        &rig,
        &ana,
        "POST",
        &silent_posts_url,
        Some(json!({
            "client_id": "cad2736-silent-flip-1",
            "body": "check @dev",
            "silent": true
        })),
    );
    assert_eq!(matching_silent["created"], false);
    assert_room_post(&matching_silent["post"]);
    assert_eq!(matching_silent["post"]["silent"], true);
    let silent_outcomes = matching_silent["post"]["outcomes"].as_array().unwrap();
    assert_eq!(silent_outcomes.len(), 1, "{silent_outcomes:?}");
    assert_eq!(silent_outcomes[0]["kind"], "silent");
    let dev_msgs_after: i64 = db
        .query_row("SELECT COUNT(*) FROM messages WHERE alias='dev'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let bell_intents_after: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM human_notification_intents WHERE room=?1",
            [silent_room.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dev_msgs_after, 0);
    assert_eq!(bell_intents_after, 0);

    // Part B: Partial multi-agent retry recovery with pending meta (`outcomes_json = ''`).
    let retry_room = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind": "mixed_group",
            "title": "Partial Retry Recovery",
            "client_id": "cad2736-partial-retry-room",
            "humans": [],
            "agents": [retry_a_uid, retry_b_uid.clone()]
        })),
    )["room"]
        .as_str()
        .unwrap()
        .to_string();
    let retry_posts_url = format!("/api/human-threads/rooms/{retry_room}/posts");
    let retry_body = "Coordinate @retry-a @retry-b";

    let first = room_ok(
        &rig,
        &ana,
        "POST",
        &retry_posts_url,
        Some(json!({
            "client_id": "cad2736-partial-retry-1",
            "body": retry_body,
            "silent": false
        })),
    );
    assert_eq!(first["created"], true);
    assert_room_post(&first["post"]);
    let retry_post_id = first["post"]["id"].as_str().unwrap();
    assert_eq!(first["post"]["outcomes"].as_array().unwrap().len(), 2);

    // Simulate `@retry-a` dispatched and `@retry-b` missed before `human_room_save_post_meta`
    // finalized outcomes, leaving the pre-seeded pending row with `outcomes_json = ''`.
    let retry_b_msg_id: String = db
        .query_row(
            "SELECT message_id FROM human_room_post_requests WHERE post_id=?1 AND agent_uid=?2",
            rusqlite::params![retry_post_id, &retry_b_uid],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE human_room_post_meta SET outcomes_json='' WHERE post_id=?1",
        [retry_post_id],
    )
    .unwrap();
    db.execute(
        "DELETE FROM human_room_post_requests WHERE post_id=?1 AND agent_uid=?2",
        rusqlite::params![retry_post_id, &retry_b_uid],
    )
    .unwrap();
    db.execute(
        "DELETE FROM thread_entries WHERE message_id=?1",
        [&retry_b_msg_id],
    )
    .unwrap();
    db.execute("DELETE FROM messages WHERE id=?1", [&retry_b_msg_id])
        .unwrap();

    assert!(
        shared
            .store
            .human_room_post_meta(retry_post_id)
            .unwrap()
            .is_none(),
        "pending empty outcomes_json row must read as None so retry recovers"
    );

    // A mismatched `silent: true` replay on the pending partial post is also refused.
    let (partial_flip_status, partial_flip_err) = room_http(
        &rig,
        &ana,
        "POST",
        &retry_posts_url,
        Some(json!({
            "client_id": "cad2736-partial-retry-1",
            "body": retry_body,
            "silent": true
        })),
    );
    assert_eq!(
        partial_flip_status, 400,
        "flipping silent=false->true on pending partial post must return 400: {partial_flip_err}"
    );
    assert_eq!(partial_flip_err["code"], "rejected", "{partial_flip_err}");

    // Retrying with matching `silent: false` recovers `@retry-b` without duplicating `@retry-a`.
    let retried = room_ok(
        &rig,
        &ana,
        "POST",
        &retry_posts_url,
        Some(json!({
            "client_id": "cad2736-partial-retry-1",
            "body": retry_body,
            "silent": false
        })),
    );
    assert_eq!(retried["created"], false);
    assert_room_post(&retried["post"]);
    assert_eq!(retried["post"]["silent"], false);
    let outcomes = retried["post"]["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 2, "{outcomes:?}");
    for expected in ["@retry-a", "@retry-b"] {
        let hit = outcomes
            .iter()
            .find(|o| {
                o["handle"]
                    .as_str()
                    .is_some_and(|h| h.eq_ignore_ascii_case(expected))
            })
            .unwrap_or_else(|| panic!("missing recovered outcome for {expected}: {outcomes:?}"));
        assert_eq!(hit["kind"], "queued", "expected queued outcome: {hit}");
    }

    let retry_a_msgs: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE alias='retry-a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let retry_b_msgs: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE alias='retry-b'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let total_retry_reqs: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM human_room_post_requests WHERE post_id=?1",
            [retry_post_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(retry_a_msgs, 1, "@retry-a must not be duplicated on retry");
    assert_eq!(retry_b_msgs, 1, "@retry-b must be recovered once on retry");
    assert_eq!(
        total_retry_reqs, 2,
        "expected 2 bound requests after recovery"
    );

    let persisted_meta = shared
        .store
        .human_room_post_meta(retry_post_id)
        .unwrap()
        .expect("finalized post meta must be persisted after recovery");
    assert!(!persisted_meta.silent);
    assert_eq!(persisted_meta.outcomes.len(), 2);
}
