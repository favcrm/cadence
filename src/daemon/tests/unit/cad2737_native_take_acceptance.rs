//! CAD-2737 acceptance: a native task is authorized by the current actor
//! take, and its opaque fence target cannot affect a different task.

use crate::store::connection_dispatch;
use crate::store::native_model_turn::{TaskRow, TASK_ACTIVE};
use crate::store::{NewAgent, Store, Take};
use serde_json::json;

#[test]
fn cad2737_only_current_store_local_actor_take_can_bind_and_fence_its_task() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite3");
    let store = Store::open(&path).unwrap();
    store
        .register_agent(&NewAgent {
            alias: "worker",
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    for id in ["alpha", "beta", "gamma"] {
        store.enqueue("worker", "work", None, id, "test").unwrap();
    }
    let uid = store.agent_uid_for_alias("worker").unwrap().unwrap();
    let workspace = store
        .read_tx(|conn| Ok(connection_dispatch::authority_in(&*conn)?.workspace_id))
        .unwrap();
    let candidate = |id: &str| TaskRow {
        durable_task_id: format!("task-{id}"),
        workspace_id: workspace.clone(),
        agent_uid: uid.clone(),
        session_binding: "session".into(),
        client_message_id: id.into(),
        server_message_id: id.into(),
        turn_token: format!("turn-{id}"),
        state: TASK_ACTIVE.into(),
        created_ms: 1,
    };
    let alpha = candidate("alpha");
    let beta = candidate("beta");
    let gamma = candidate("gamma");

    let (Take::Message(first), Some(old_claim)) =
        store.take_queued_for_actor("worker", None).unwrap()
    else {
        panic!("expected a managed Pi actor take");
    };
    assert_eq!(first.id, "alpha");
    assert_eq!(
        store
            .bind_claimed_native_task(&old_claim, &beta)
            .err()
            .unwrap()
            .code(),
        Some("principal_required")
    );
    assert!(store
        .native_model_task_by_id(&beta.durable_task_id)
        .unwrap()
        .is_none());

    store.requeue("alpha").unwrap();
    let (Take::Message(retaken), Some(current_claim)) =
        store.take_queued_for_actor("worker", None).unwrap()
    else {
        panic!("expected the requeued actor take");
    };
    assert_eq!(retaken.id, "alpha");
    assert_eq!(
        store
            .bind_claimed_native_task(&old_claim, &alpha)
            .err()
            .unwrap()
            .code(),
        Some("runtime_revoked")
    );
    assert!(store
        .native_model_task_by_id(&alpha.durable_task_id)
        .unwrap()
        .is_none());

    let other_store = Store::open_side(&path).unwrap();
    assert_eq!(
        other_store
            .bind_claimed_native_task(&current_claim, &alpha)
            .err()
            .unwrap()
            .code(),
        Some("principal_required")
    );
    assert!(store
        .native_model_task_by_id(&alpha.durable_task_id)
        .unwrap()
        .is_none());
    let (bound_alpha, alpha_fence) = store
        .bind_claimed_native_task(&current_claim, &alpha)
        .unwrap();
    assert_eq!(bound_alpha.state, TASK_ACTIVE);
    assert_eq!(bound_alpha.client_message_id, "alpha");

    let (Take::Message(second), Some(beta_claim)) =
        store.take_queued_for_actor("worker", None).unwrap()
    else {
        panic!("expected the second actor take");
    };
    assert_eq!(second.id, "beta");
    let (bound_beta, _beta_fence) = store.bind_claimed_native_task(&beta_claim, &beta).unwrap();
    assert_eq!(bound_beta.state, TASK_ACTIVE);

    assert_eq!(
        other_store
            .fence_claimed_native_task(&alpha_fence, "foreign store", 2)
            .unwrap_err()
            .code(),
        Some("principal_required")
    );
    let state = |id: &str| store.native_model_task_by_id(id).unwrap().unwrap().state;
    assert_eq!(state(&alpha.durable_task_id), TASK_ACTIVE);
    store
        .fence_claimed_native_task(&alpha_fence, "finished", 3)
        .unwrap();
    assert_eq!(state(&alpha.durable_task_id), "fenced");
    assert_eq!(state(&beta.durable_task_id), TASK_ACTIVE);
    assert_eq!(
        store
            .bind_claimed_native_task(&current_claim, &alpha)
            .err()
            .unwrap()
            .code(),
        Some("runtime_revoked")
    );
    assert_eq!(state(&beta.durable_task_id), TASK_ACTIVE);

    let (Take::Message(third), Some(gamma_claim)) =
        store.take_queued_for_actor("worker", None).unwrap()
    else {
        panic!("expected the third actor take");
    };
    assert_eq!(third.id, "gamma");
    store.finish(&third, "completed", &json!({}), None).unwrap();
    assert_eq!(
        store
            .bind_claimed_native_task(&gamma_claim, &gamma)
            .err()
            .unwrap()
            .code(),
        Some("runtime_revoked")
    );
    assert!(store
        .native_model_task_by_id(&gamma.durable_task_id)
        .unwrap()
        .is_none());
}
