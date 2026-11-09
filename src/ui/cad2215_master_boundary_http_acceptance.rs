//! CAD-2215 independent master boundary supplementary acceptance.
//! The product implementer must not edit or weaken this check.
//! Local-agent identity uses the existing explicit test seam, not OS isolation.

use super::*;

fn register_master(rig: &Rig) -> Arc<crate::daemon::Shared> {
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "master",
            provider: "fake",
            endpoint_kind: "fake",
            role: "pm",
            cwd: rig._dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared.store.ensure_thread("master").unwrap();
    shared
}

#[test]
fn readable_issue_does_not_grant_member_master_home_turn_metadata() {
    let rig = start(&[]);
    seed(&rig);
    let shared = register_master(&rig);
    let home = shared.store.ensure_thread("master").unwrap();
    let member = login(&rig, "cad2215-home-reader", "member");
    let owner = login(&rig, "cad2215-home-operator", "owner");
    let visible_issue = get(&rig, &member, "/api/issues/VIS-1");
    assert!(
        visible_issue.to_string().contains("VIS-1"),
        "member cannot read fixture issue"
    );
    let before = get(&rig, &member, "/api/master/state");
    let before_sse = initial_master(&rig, &member, "home baseline");
    // The issue and ordinary task are genuinely readable; master Home itself
    // remains operator-private. This is not an assistant ownership fixture.
    shared
        .store
        .enqueue_task(
            "master",
            "CAD2215_HOME_PROJECT_READABLE",
            None,
            "cad2215-home-project-readable",
            "user",
            Some("cad2215-visible-task"),
        )
        .unwrap();
    let operator = get(&rig, &owner, "/api/master/state");
    let operator_sse = initial_master(&rig, &owner, "home operator");
    for (which, body) in [("HTTP", &operator), ("SSE", &operator_sse)] {
        let text = body.to_string();
        assert!(
            text.contains("cad2215-home-project-readable"),
            "operator {which} missed Home message"
        );
        assert!(
            text.contains(&home.id),
            "operator {which} missed Home conversation"
        );
        assert!(
            body["queued"].as_u64().is_some_and(|n| n >= 1),
            "operator {which} missed count"
        );
        assert!(
            !body["turn"]["since"].is_null(),
            "operator {which} missed timestamp"
        );
    }
    let after = get(&rig, &member, "/api/master/state");
    let after_sse = initial_master(&rig, &member, "home member after");
    assert_eq!(
        routing(&before),
        routing(&after),
        "readable-issue Home turn changed member HTTP routing/counts"
    );
    assert_eq!(
        routing(&before_sse),
        routing(&after_sse),
        "readable-issue Home turn changed member SSE routing/counts"
    );
    for (which, body) in [("HTTP", &after), ("SSE", &after_sse)] {
        let text = body.to_string();
        for hidden in [
            "cad2215-home-project-readable",
            "CAD2215_HOME_PROJECT_READABLE",
            home.id.as_str(),
        ] {
            assert!(
                !text.contains(hidden),
                "member {which} leaked Home metadata {hidden}"
            );
        }
    }
}

#[test]
fn local_agent_connection_cannot_read_global_master_stamp() {
    let rig = start(&[]);
    seed(&rig);
    let _shared = register_master(&rig);
    // This invokes the real daemon over its Unix socket. Only the peer's
    // identity is provided by the existing test seam; it is not OS proof.
    let state = rig._dir.path();
    let agent = || crate::test_seam::Asserted::Agent("cad2215-worker".into());
    let list = crate::test_seam::scoped(agent(), || {
        crate::client::rpc(state, "agent_list", json!({}))
    });
    assert!(
        list.is_ok(),
        "fixture agent identity did not reach a permitted daemon read"
    );
    let denied = crate::test_seam::scoped(agent(), || {
        crate::client::rpc(state, "master_stamp", json!({}))
    });
    assert!(
        denied.is_err(),
        "nonoperator local agent received global master stamp: {denied:?}"
    );
    let owner = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc(state, "master_stamp", json!({}))
    })
    .expect("operator master stamp refused");
    assert!(
        owner["queued"].is_number(),
        "operator did not receive real global stamp"
    );
    let denied_threads = crate::test_seam::scoped(agent(), || {
        crate::client::rpc(state, "threads_stamp", json!({}))
    });
    assert!(
        denied_threads.is_err(),
        "nonoperator local agent received global threads stamp: {denied_threads:?}"
    );
    let owner_threads = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc(state, "threads_stamp", json!({}))
    })
    .expect("operator threads stamp refused");
    assert!(
        owner_threads["topic_event"].is_number(),
        "operator did not receive real global threads stamp"
    );
}
