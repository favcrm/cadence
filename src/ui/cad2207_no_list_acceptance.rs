//! CAD-2207: the operator must not enumerate another member's assistant.
//! Uses the real daemon and signed-member board fixture; a normal agent is
//! retained as a positive control so a blanket refusal cannot pass.

use super::super::start;
use crate::daemon::team_assistants_refusal::AssistantFlagOn;
use crate::daemon::ServeOptions;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

fn legacy_list(state: &std::path::Path) -> Value {
    let mut stream = UnixStream::connect(crate::client::socket_path(state)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .write_all(b"{\"method\":\"agent_list\",\"params\":{}}\n")
        .unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn legacy_fleet_refusal_does_not_disclose_whether_assistants_exist() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&[]);
    let response = legacy_list(rig._dir.path());
    assert_eq!(
        response["ok"], false,
        "a legacy fleet request succeeded when no assistant existed: {response}"
    );
}

#[test]
fn operator_operational_list_withholds_personal_assistant() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&["alice"]);
    let state = rig._dir.path().to_path_buf();
    let opened = rig.member_post(
        "/__platform/session",
        None,
        &json!({"assertion": rig.alice_sso()}),
    );
    assert_eq!(opened.status().as_u16(), 200);
    let cookie = opened
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    drop(opened);
    rig.authorize_as("alice");
    let mut response = rig.member_post("/api/member-assistant/state", Some(&cookie), &json!({}));
    assert_eq!(response.status().as_u16(), 200);
    let own: Value = response.body_mut().read_json().unwrap();
    let private_alias = own["assistant"]["alias"].as_str().unwrap();

    let options = ServeOptions::default();
    options
        .provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(&state, &options).unwrap();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "cad2207-visible-worker",
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: state.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();

    // A client predating the privacy-aware protocol must not receive a
    // silently filtered list: an old board would use it for attribution and
    // mistake this member assistant's process for a human.
    let legacy = legacy_list(&state);
    assert_eq!(
        legacy["ok"], false,
        "legacy list received a partial fleet usable for unsafe attribution: {legacy}"
    );
    assert!(
        !legacy.to_string().contains(private_alias),
        "legacy refusal exposed a personal assistant: {legacy}"
    );

    let operational = scoped(Asserted::Operator, || {
        crate::client::rpc(&state, "agent_operational", json!({}))
    })
    .expect("operator lost the ordinary-agent operational list");
    let agents = operational["agents"].as_array().unwrap();
    assert!(agents
        .iter()
        .any(|row| row["alias"] == "cad2207-visible-worker"));
    assert!(
        !operational.to_string().contains(private_alias),
        "operator operational list exposed the member's personal assistant: {operational}"
    );
}
