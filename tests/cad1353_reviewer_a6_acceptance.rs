#![cfg(feature = "test-seam")]

use cadence_agent::adapter::ProviderEnv;
use cadence_agent::daemon::{ServeOptions, Shared};
use cadence_agent::issue::Pm;
use cadence_agent::store::NewAgent;
use cadence_agent::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::time::Duration;

#[test]
fn cad1353_reviewer_a6_queue_changes_both_public_agent_views() {
    let root = tempfile::Builder::new()
        .prefix("a6-review-")
        .tempdir()
        .unwrap();
    let pm = root.path().join("pm");
    Pm::init(&pm).unwrap();
    let env = ProviderEnv::refusing_providers();
    env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let opts = ServeOptions {
        provider_env: env,
        ..Default::default()
    };
    let shared = Shared::new(root.path(), &opts).unwrap();

    // Persistent managed snapshots only: no external provider is launched.
    for alias in ["a6-queued", "a6-clear"] {
        shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider: "codex",
                endpoint_kind: "managed",
                role: "worker",
                cwd: root.path().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: Some("provider_default"),
            })
            .unwrap();
        shared.store.set_agent_state(alias, "idle", None).unwrap();
    }
    assert!(shared.store.agent("a6-queued").unwrap().endpoint.is_none());
    shared
        .store
        .enqueue(
            "a6-queued",
            "first pending assignment",
            None,
            "a6-first",
            "test",
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    shared
        .store
        .enqueue(
            "a6-queued",
            "second pending assignment",
            None,
            "a6-second",
            "test",
        )
        .unwrap();

    for method in ["agent_show", "agent_list"] {
        let params = if method == "agent_show" {
            // Delivery facts must not depend on the message-history window.
            json!({"alias": "a6-queued", "limit": 0})
        } else {
            json!({})
        };
        let answer = scoped(Asserted::Operator, || {
            shared.dispatch(method, &params, std::process::id())
        })
        .unwrap();
        let row: &Value = if method == "agent_show" {
            &answer["agent"]
        } else {
            let rows = answer["agents"].as_array().unwrap();
            let clear = rows.iter().find(|r| r["alias"] == "a6-clear").unwrap();
            assert_eq!(clear["delivery"]["queued"], 0, "{answer}");
            assert_eq!(clear["delivery"]["state"], "clear", "{answer}");
            rows.iter().find(|r| r["alias"] == "a6-queued").unwrap()
        };
        assert_eq!(row["state"], "idle", "{method}: {row}");
        let delivery = &row["delivery"];
        assert_eq!(delivery["queued"], 2, "{method}: {row}");
        assert_eq!(delivery["state"], "idle-no-turn", "{method}: {row}");
        assert!(
            delivery["oldest_secs"].as_u64().is_some_and(|s| s >= 1),
            "{method}: oldest queued work must have an age: {row}"
        );
        assert_eq!(delivery["blocker"]["owner"], "daemon", "{method}: {row}");
        assert!(
            delivery["blocker"]["reason"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty()),
            "{method}: {row}"
        );
        let label = row["state_label"].as_str().expect("display label");
        assert_ne!(label.trim(), "idle", "{method}: {row}");
        assert!(label.contains("2 queued"), "{method}: {row}");
    }
}
