//! CAD-1353 acceptance check: a queued delivery on a live idle actor is
//! not reported as a plain idle agent.
//!
//! This is the implementer's local verification of the surface; the
//! ticket asks for a reviewer-written check, which must replace or
//! independently confirm this file before the PR is reviewed.
#![cfg(feature = "test-seam")]

use super::*;
use crate::adapter::Identity;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::sync::Arc;

struct Fx {
    _dir: tempfile::TempDir,
    shared: Arc<Shared>,
}

fn fx() -> Fx {
    let dir = tempfile::Builder::new().prefix("c1353").tempdir().unwrap();
    let pm = dir.path().join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let cwd = dir.path().to_str().unwrap();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "w1",
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd,
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    // A live idle actor: no turn is running and the message below stays
    // queued until a delivery loop claims it. Fake/managed actors may
    // carry no endpoint address; their state is the liveness signal, so
    // the delivery view must not send the operator to `agent resume`.
    shared
        .store
        .set_identity_with_quota(
            "w1",
            &Identity {
                thread_id: "cad1353-thread".into(),
                session_id: "cad1353-session".into(),
                model: None,
                effort: None,
                pid: std::process::id(),
                endpoint: None,
                generation: None,
                attach: None,
            },
            None,
        )
        .unwrap();
    shared
        .store
        .enqueue("w1", "queued work", None, "cad1353-queued", "test")
        .unwrap();
    Fx { _dir: dir, shared }
}

impl Fx {
    fn call(&self, method: &str, params: Value) -> Value {
        scoped(Asserted::Operator, || {
            self.shared.dispatch(method, &params, std::process::id())
        })
        .unwrap()
    }
}

fn assert_delivery(row: &Value) {
    let delivery = &row["delivery"];
    assert_eq!(delivery["queued"], 1, "queued count: {row}");
    assert_eq!(
        delivery["state"], "idle-no-turn",
        "a live idle actor with a queued delivery must name the block: {row}"
    );
    assert!(
        delivery["oldest_secs"].is_u64(),
        "the age must be visible: {row}"
    );
    assert_eq!(delivery["blocker"]["owner"], "daemon", "{row}");
    assert!(
        delivery["blocker"]["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "a reason must be named: {row}"
    );
    assert!(
        row["state_label"]
            .as_str()
            .is_some_and(|label| label.contains("1 queued")),
        "the label must not read as plain idle: {row}"
    );
}

#[test]
fn idle_agent_with_queued_work_reports_the_delivery_block() {
    let fx = fx();
    let show = fx.call("agent_show", json!({"alias": "w1"}));
    assert_delivery(&show["agent"]);

    let list = fx.call("agent_list", json!({}));
    let row = list["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["alias"] == "w1")
        .expect("w1 in agent_list");
    assert_delivery(row);
}
