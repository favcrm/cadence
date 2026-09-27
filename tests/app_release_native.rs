//! CAD692 reviewed Local release through actual native provider turns.
#![allow(clippy::disallowed_methods)]
mod common;
use common::app_release::{Release, A, B, OWNER, REVIEWER, WRITER};
use common::{plant_member_pane, LaneShell};
use serde_json::{json, Value};
use std::path::Path;
fn native(
    lane: &mut LaneShell,
    state: &Path,
    detached: bool,
    method: &str,
    params: Value,
) -> Value {
    let frame = json!({"method":method,"params":params});
    assert_eq!(frame.as_object().unwrap().len(), 2);
    assert!(frame.get(cadence_agent::test_seam::FRAME_FIELD).is_none());
    let wire = frame.to_string();
    assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
    assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
    let request = lane
        .dir
        .path()
        .join(format!("context-rpc-{}.json", lane.seq));
    std::fs::write(&request, wire).unwrap();
    let prefix = if detached { "setsid " } else { "" };
    let (rc,text)=lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.socket(socket.AF_UNIX);s.settimeout(10);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}",state.join("cadence.sock").display(),request.display()));
    assert_eq!(rc, 0, "native process failed: {text}");
    serde_json::from_str(text.trim()).unwrap()
}

#[test]
fn cad692_actual_accepted_artifact_waits_then_releases_one_project_free_local_item() {
    let h = Release::new();
    let context = h.context("Client A", A, "a");
    let binding = h.bind(&context, "binding-a");
    let run = h.complete(&context, "run-a");
    assert_eq!(run["snapshot"]["schema"], 3);
    assert_eq!(run["reviews"].as_array().unwrap().len(), 1);
    let artifact = h.artifact(&run);
    assert_eq!(artifact["text"], format!("Context draft: {A}"));
    assert!(
        h.items().as_array().unwrap().is_empty(),
        "review completion wrote outward material"
    );
    let waiting = h.stage(&run, "release-a");
    assert_eq!(waiting["state"], "waiting");
    assert!(waiting["digest"].is_string());
    assert_eq!(
        h.stage(&run, "release-a"),
        waiting,
        "immutable stage replay changed receipt"
    );
    assert!(
        h.items().as_array().unwrap().is_empty(),
        "staging performed a send"
    );
    let changed = h.daemon.operator_rpc("app_effect_stage", json!({"run_id":run["id"],"artifact_id":artifact["id"],"slot":"publication","request_id":"release-a","title":"Different title"}));
    assert!(changed.is_err(), "same request accepted changed input");
    let done = h.decide(&waiting);
    assert_eq!(done["state"], "done");
    let items = h.items();
    assert_eq!(items.as_array().unwrap().len(), 1);
    assert_eq!(items[0]["effect_id"], waiting["effect_id"]);
    let detail = h
        .daemon
        .operator_rpc("platform_outbox", json!({"effect_id":waiting["effect_id"]}))
        .unwrap();
    assert_eq!(
        detail["item"]["post"],
        format!("# Reviewed draft\n\nContext draft: {A}")
    );
    assert_eq!(detail["item"]["scope"]["kind"], "app_artifact");
    assert_eq!(
        detail["item"]["scope"]["install_id"],
        h.install["install_id"]
    );
    assert_eq!(detail["item"]["scope"]["context_id"], context["id"]);
    assert!(
        detail["item"]["project"].is_null(),
        "app item impersonated a project"
    );
    assert_eq!(detail["item"]["provenance"]["run_id"], run["id"]);
    assert_eq!(detail["item"]["provenance"]["artifact_id"], artifact["id"]);
    assert_eq!(detail["item"]["provenance"]["binding_id"], binding["id"]);
    let _ = h.daemon.operator_rpc(
        "app_effect_decide",
        json!({"effect_id":waiting["effect_id"],"digest":waiting["digest"],"decision":"accept"}),
    );
    assert_eq!(h.items(), items, "repeated release duplicated Local write");
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    for table in ["platform_grants", "platform_account_defaults"] {
        let count: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "release created ambient authority in {table}");
    }
    assert!(!h.root.path().join("pm/projects/site").exists());
}

#[test]
fn cad692_populated_native_setsid_and_forged_callers_cannot_stage_bind_or_release() {
    let h = Release::new();
    let c = h.context("Private", A, "private");
    let binding = h.bind(&c, "private-binding");
    let run = h.complete(&c, "private-run");
    let effect = h.stage(&run, "private-effect");
    let mut lane = LaneShell::spawn(h.root.path());
    plant_member_pane(
        &h.daemon,
        "release-native-worker",
        "claude",
        None,
        lane.pid(),
    );
    let methods = [
        (
            "app_binding_list",
            json!({"install_id":h.install["install_id"]}),
        ),
        (
            "app_binding_show",
            json!({"install_id":h.install["install_id"],"binding_id":binding["id"]}),
        ),
        (
            "app_binding_update",
            json!({"install_id":h.install["install_id"],"binding_id":binding["id"],"expected_revision":binding["revision"],"connection_id":h.connection}),
        ),
        (
            "app_binding_revoke",
            json!({"install_id":h.install["install_id"],"binding_id":binding["id"],"expected_revision":binding["revision"]}),
        ),
        (
            "app_effect_stage",
            json!({"run_id":run["id"],"artifact_id":run["artifacts"][0]["id"],"slot":"publication","request_id":"attack","title":"Stolen"}),
        ),
        ("app_effect_show", json!({"effect_id":effect["effect_id"]})),
        (
            "app_effect_list",
            json!({"install_id":h.install["install_id"]}),
        ),
        (
            "app_effect_decide",
            json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"}),
        ),
    ];
    let mut failures = Vec::new();
    for detached in [false, true] {
        for (method, params) in &methods {
            for forged in [false, true] {
                let mut params = params.clone();
                if forged {
                    params["operator"] = json!(true);
                    params["agent"] = json!(OWNER);
                }
                let answer = native(&mut lane, &h.daemon.state, detached, method, params);
                if answer["ok"] != false
                    || answer["error"]["kind"] != "rejected"
                    || answer.get("result").is_some()
                {
                    failures.push(format!(
                        "{method} detached={detached} forged={forged}: {answer}"
                    ));
                }
                assert!(
                    !answer.to_string().contains(A),
                    "native release projection leaked private body"
                );
            }
        }
    }
    assert!(
        failures.is_empty(),
        "native app release operator guard failed: {failures:?}"
    );
    assert_eq!(
        h.daemon
            .operator_rpc("app_effect_show", json!({"effect_id":effect["effect_id"]}))
            .unwrap()["effect"],
        effect
    );
    assert!(h.items().as_array().unwrap().is_empty());
    assert_eq!(
        h.decide(&effect)["state"],
        "done",
        "positive release unavailable after denied calls"
    );
}

#[test]
fn cad692_shared_workers_two_contexts_pin_exact_binding_and_artifact() {
    let h = Release::new();
    let a = h.context("A", A, "two-a");
    let b = h.context("B", B, "two-b");
    h.bind(&a, "bind-a");
    h.bind(&b, "bind-b");
    let ra = h.complete(&a, "two-run-a");
    let rb = h.complete(&b, "two-run-b");
    assert_eq!(ra["snapshot"]["assignments"], rb["snapshot"]["assignments"]);
    let ea = h.stage(&ra, "two-effect-a");
    let eb = h.stage(&rb, "two-effect-b");
    assert_ne!(ea["digest"], eb["digest"]);
    let wrong=h.daemon.operator_rpc("app_effect_stage",json!({"run_id":rb["id"],"artifact_id":ra["artifacts"][0]["id"],"slot":"publication","request_id":"wrong-artifact","title":"Wrong"}));
    assert_eq!(wrong.unwrap_err().kind(), "rejected");
    h.decide(&ea);
    h.decide(&eb);
    assert_eq!(h.items().as_array().unwrap().len(), 2);
    for (effect, canary, foreign) in [(&ea, A, B), (&eb, B, A)] {
        let item = h
            .daemon
            .operator_rpc("platform_outbox", json!({"effect_id":effect["effect_id"]}))
            .unwrap();
        assert!(item["item"]["post"].as_str().unwrap().contains(canary));
        assert!(!item["item"]["post"].as_str().unwrap().contains(foreign));
    }
}

#[test]
fn cad692_existing_other_install_cannot_stage_or_retarget_accepted_material() {
    let h = Release::new();
    let a = h.context("Original", A, "install-a");
    h.bind(&a, "install-binding-a");
    let ra = h.complete(&a, "install-run-a");
    let other_source = h.root.path().join("other-bundle");
    std::fs::create_dir_all(other_source.join("workflows")).unwrap();
    let manifest = std::fs::read_to_string(h.root.path().join("bundle/app.md")).unwrap();
    let changed = manifest.replace("app: local-content", "app: other-content");
    assert_ne!(changed, manifest);
    std::fs::write(other_source.join("app.md"), changed).unwrap();
    std::fs::copy(
        h.root.path().join("bundle/workflows/draft.md"),
        other_source.join("workflows/draft.md"),
    )
    .unwrap();
    let other = h
        .daemon
        .operator_rpc("app_workspace_install", json!({"source":other_source}))
        .unwrap();
    assert_ne!(other["install_id"], h.install["install_id"]);
    h.daemon
        .operator_rpc(
            "app_local_install_approve",
            json!({"install_id":other["install_id"],"digest":other["digest"]}),
        )
        .unwrap();
    let b = h.daemon.operator_rpc("app_context_create", json!({"install_id":other["install_id"],"label":"Other","input_defaults":{"source":format!("CONTEXT_SOURCE={B}")},"request_id":"other-context"})).unwrap()["context"].clone();
    h.daemon.operator_rpc("app_binding_create", json!({"install_id":other["install_id"],"context_id":b["id"],"slot":"publication","connection_id":h.connection,"request_id":"other-binding"})).unwrap();
    let rb = h.daemon.operator_rpc("app_run_create", json!({"install_id":other["install_id"],"context_id":b["id"],"workflow":"draft","inputs":{"subject":"Other install","writer":WRITER,"reviewer":REVIEWER},"request_id":"other-run","owner_pm":OWNER})).unwrap();
    h.dispatch(&rb);
    let rb = h.wait_state(rb["id"].as_str().unwrap(), "succeeded");
    let wrong = h.daemon.operator_rpc("app_effect_stage", json!({"run_id":rb["id"],"artifact_id":ra["artifacts"][0]["id"],"slot":"publication","request_id":"cross-install","title":"Wrong"}));
    assert_eq!(wrong.unwrap_err().kind(), "rejected");
    let good = h.stage(&rb, "other-positive");
    assert_eq!(h.decide(&good)["state"], "done");
    let item = h
        .daemon
        .operator_rpc("platform_outbox", json!({"effect_id":good["effect_id"]}))
        .unwrap();
    assert_eq!(item["item"]["scope"]["install_id"], other["install_id"]);
    assert!(item["item"]["post"].as_str().unwrap().contains(B));
    assert!(!item["item"]["post"].as_str().unwrap().contains(A));
}

#[test]
fn cad692_binding_context_and_capability_changes_refuse_waiting_and_decided_release() {
    for hold_decided in [false, true] {
        for mutation in [
            "binding_update",
            "binding_revoke",
            "context_update",
            "context_archive",
            "capability_revoke",
        ] {
            let h = Release::with_decision_gate(hold_decided);
            let c = h.context("Mutation", A, "mutation-context");
            let binding = h.bind(&c, "mutation-binding");
            let run = h.complete(&c, "mutation-run");
            let effect = h.stage(&run, "mutation-effect");
            if hold_decided {
                assert_eq!(
                    h.decide(&effect)["state"],
                    "decided",
                    "trusted gate did not isolate durable decision"
                );
            }
            assert!(h.items().as_array().unwrap().is_empty());
            let (method, params) = match mutation {
                "binding_update" => (
                    "app_binding_update",
                    json!({"install_id":h.install["install_id"],"binding_id":binding["id"],"expected_revision":binding["revision"],"connection_id":h.connection}),
                ),
                "binding_revoke" => (
                    "app_binding_revoke",
                    json!({"install_id":h.install["install_id"],"binding_id":binding["id"],"expected_revision":binding["revision"]}),
                ),
                "context_update" => (
                    "app_context_update",
                    json!({"install_id":h.install["install_id"],"context_id":c["id"],"expected_revision":c["revision"],"label":"Changed","input_defaults":{"source":format!("CONTEXT_SOURCE={B}")}}),
                ),
                "context_archive" => (
                    "app_context_archive",
                    json!({"install_id":h.install["install_id"],"context_id":c["id"],"expected_revision":c["revision"]}),
                ),
                _ => (
                    "app_local_install_revoke",
                    json!({"install_id":h.install["install_id"],"digest":h.install["digest"]}),
                ),
            };
            h.daemon.operator_rpc(method, params).unwrap();
            let attempt = h.daemon.operator_rpc("app_effect_decide", json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"}));
            assert!(
                attempt.is_err(),
                "stale {mutation} release accepted, held={hold_decided}"
            );
            let after = h
                .daemon
                .operator_rpc("app_effect_show", json!({"effect_id":effect["effect_id"]}))
                .unwrap()["effect"]
                .clone();
            assert_ne!(
                after["state"], "done",
                "invalidated effect acquired successful outcome"
            );
            assert_eq!(
                after["authority"], effect["authority"],
                "mutation rewrote historical authority"
            );
            assert!(
                h.items().as_array().unwrap().is_empty(),
                "stale {mutation} wrote outbox"
            );
            assert_eq!(h.artifact(&run)["text"], format!("Context draft: {A}"));
        }
    }
}

#[test]
fn cad692_concurrent_accepts_have_one_claim_one_write_and_one_outcome() {
    let h = Release::new();
    let c = h.context("Concurrent", A, "concurrent-context");
    h.bind(&c, "concurrent-binding");
    let run = h.complete(&c, "concurrent-run");
    let effect = h.stage(&run, "concurrent-effect");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let outcomes = std::thread::scope(|scope| {
        let first = barrier.clone();
        let second = barrier.clone();
        let daemon = &h.daemon;
        let effect = &effect;
        let one=scope.spawn(move || {first.wait();daemon.operator_rpc("app_effect_decide",json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"}))});
        let two=scope.spawn(move || {second.wait();daemon.operator_rpc("app_effect_decide",json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"}))});
        (one.join().unwrap(), two.join().unwrap())
    });
    assert!(
        outcomes.0.is_ok() || outcomes.1.is_ok(),
        "both populated operator accepts failed"
    );
    assert_eq!(
        h.items().as_array().unwrap().len(),
        1,
        "concurrent claim duplicated local write"
    );
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    let outcomes:i64=db.query_row("SELECT count(*) FROM events WHERE kind='effect_executed' AND json_extract(payload,'$.effect_id')=?",[effect["effect_id"].as_str().unwrap()],|row|row.get(0)).unwrap();
    assert_eq!(
        outcomes, 1,
        "concurrent accept delivered duplicate effect outcomes"
    );
}

#[test]
fn cad692_routing_owner_legacy_effect_paths_neither_expose_nor_release_app_child() {
    let h = Release::new();
    let c = h.context("Owner projection", A, "projection-context");
    h.bind(&c, "projection-binding");
    let run = h.complete(&c, "projection-run");
    let effect = h.stage(&run, "projection-effect");
    assert_eq!(
        effect["record"]["agent"], OWNER,
        "fixture must exercise matching routing owner"
    );
    let mut owner = LaneShell::spawn(h.root.path());
    plant_member_pane(&h.daemon, OWNER, "inbox", None, owner.pid());
    let attempts = [
        (
            "agent_respond",
            json!({"alias":OWNER,"request":effect["request"],"decision":"accept"}),
        ),
        (
            "platform_effect_close",
            json!({"effect_id":effect["effect_id"]}),
        ),
        (
            "request_wait",
            json!({"request":effect["request"],"wait":0}),
        ),
        ("request_close", json!({"request":effect["request"]})),
    ];
    let mut failures = vec![];
    for detached in [false, true] {
        for (method, params) in &attempts {
            for forged in [false, true] {
                let mut params = params.clone();
                if forged {
                    params["operator"] = json!(true);
                    params["agent"] = json!(OWNER);
                }
                let result = native(&mut owner, &h.daemon.state, detached, method, params);
                if result["ok"] != false || result["error"]["kind"] != "rejected" {
                    failures.push(format!(
                        "{method} detached={detached} forged={forged}: {result}"
                    ));
                }
                assert!(
                    !result.to_string().contains(A),
                    "alternate effect path leaked app preview"
                );
            }
        }
        for (method, params) in [
            ("platform_effects", json!({"agent":OWNER})),
            ("agent_requests", json!({"alias":OWNER})),
            ("agent_show", json!({"alias":OWNER})),
            ("agent_events", json!({"alias":OWNER,"tail":true})),
        ] {
            let result = native(&mut owner, &h.daemon.state, detached, method, params);
            assert_eq!(
                result["ok"], true,
                "ordinary owner projection unavailable: {method}"
            );
            assert!(
                !result.to_string().contains(A),
                "routing owner projection exposed private artifact"
            );
            if matches!(method, "platform_effects" | "agent_requests") {
                assert!(
                    !result
                        .to_string()
                        .contains(effect["effect_id"].as_str().unwrap()),
                    "legacy material projection exposed app child"
                );
            }
        }
    }
    assert!(
        failures.is_empty(),
        "alternate app release route accepted: {failures:?}"
    );
    for (method, params) in &attempts {
        assert_eq!(
            h.daemon
                .operator_rpc(method, params.clone())
                .unwrap_err()
                .kind(),
            "rejected",
            "legacy operator path bypassed pinned app decision"
        );
    }
    let generic = h
        .daemon
        .operator_rpc("platform_effects", json!({}))
        .unwrap();
    assert!(
        !generic
            .to_string()
            .contains(effect["effect_id"].as_str().unwrap()),
        "legacy operator list duplicated private app contract"
    );
    assert_eq!(
        h.daemon
            .operator_rpc("app_effect_show", json!({"effect_id":effect["effect_id"]}))
            .unwrap()["effect"],
        effect
    );
    h.decide(&effect);
    for detached in [false, true] {
        let result = native(
            &mut owner,
            &h.daemon.state,
            detached,
            "agent_show",
            json!({"alias":OWNER}),
        );
        assert_eq!(result["ok"], true);
        assert!(
            !result.to_string().contains(A),
            "outcome delivery leaked app body to routing PM"
        );
    }
    assert_eq!(h.items().as_array().unwrap().len(), 1);
}

#[test]
fn cad692_waiting_release_survives_restart_and_actual_reviewer_retirement() {
    let h = Release::new();
    let c = h.context("History", A, "history-context");
    h.bind(&c, "history-binding");
    let run = h.complete(&c, "history-run");
    h.daemon
        .operator_rpc("agent_stop", json!({"alias":REVIEWER}))
        .unwrap();
    h.daemon
        .operator_rpc("agent_stop", json!({"alias":WRITER}))
        .unwrap();
    let effect = h.stage(&run, "history-effect");
    assert_eq!(
        effect["state"], "waiting",
        "retired actual reviewer lost accepted historical provenance"
    );
    let Release {
        root,
        daemon,
        install: _,
        connection: _,
    } = h;
    let state = daemon.state.clone();
    drop(daemon);
    let mut opts = common::daemon_opts();
    opts.provider_env
        .set("CADENCE_PM_DIR", root.path().join("pm").to_str().unwrap());
    cadence_agent::platform::local::register_at(
        &state,
        &mut opts,
        root.path().join("outbox"),
        "http://localhost:3119".into(),
    );
    let restarted = common::TestDaemon::start_on_opts(state, opts);
    let restored = restarted
        .operator_rpc("app_effect_show", json!({"effect_id":effect["effect_id"]}))
        .unwrap()["effect"]
        .clone();
    assert_eq!(
        restored, effect,
        "waiting restart rewrote child or lost pending release"
    );
    assert!(
        restarted
            .operator_rpc("platform_outbox", json!({}))
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .is_empty(),
        "restart automatically released waiting send"
    );
    let released = restarted
        .operator_rpc(
            "app_effect_decide",
            json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"}),
        )
        .unwrap()["effect"]
        .clone();
    assert_eq!(
        released["state"], "done",
        "current release required a retired worker to be live"
    );
    let detail = restarted
        .operator_rpc("platform_outbox", json!({"effect_id":effect["effect_id"]}))
        .unwrap();
    assert_eq!(
        detail["item"]["post"],
        format!("# Reviewed draft\n\nContext draft: {A}")
    );
}

#[test]
fn cad692_decided_restart_reconciles_without_replay() {
    let h = Release::with_decision_gate(true);
    let c = h.context("Uncertain", A, "uncertain-context");
    h.bind(&c, "uncertain-binding");
    let run = h.complete(&c, "uncertain-run");
    let effect = h.stage(&run, "uncertain-effect");
    assert_eq!(h.decide(&effect)["state"], "decided");
    let decided = h
        .daemon
        .operator_rpc("app_effect_show", json!({"effect_id":effect["effect_id"]}))
        .unwrap()["effect"]
        .clone();
    let Release {
        root,
        daemon,
        install: _,
        connection: _,
    } = h;
    let state = daemon.state.clone();
    drop(daemon);
    let mut opts = common::daemon_opts();
    opts.provider_env
        .set("CADENCE_PM_DIR", root.path().join("pm").to_str().unwrap());
    cadence_agent::platform::local::register_at(
        &state,
        &mut opts,
        root.path().join("outbox"),
        "http://localhost:3119".into(),
    );
    let restarted = common::TestDaemon::start_on_opts(state, opts);
    let restored = restarted
        .operator_rpc("app_effect_show", json!({"effect_id":effect["effect_id"]}))
        .unwrap()["effect"]
        .clone();
    assert_eq!(restored["state"], "reconcile");
    assert_eq!(restored["authority"], decided["authority"]);
    assert_eq!(restored["digest"], decided["digest"]);
    assert!(restarted.operator_rpc("app_effect_decide",json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"})).is_err(),"uncertain prior decision re-executed");
    assert!(restarted
        .operator_rpc("platform_outbox", json!({}))
        .unwrap()["items"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn cad692_valid_legacy_publish_grant_cannot_call_internal_app_release_tool() {
    let h = Release::new();
    let c = h.context("Tool isolation", A, "tool-context");
    h.bind(&c, "tool-binding");
    let run = h.complete(&c, "tool-run");
    let effect = h.stage(&run, "tool-effect");
    assert_eq!(effect["record"]["tool"], "publish_app_text");
    let mut lane = LaneShell::spawn(h.root.path());
    let alias = "release-legacy-publisher";
    plant_member_pane(&h.daemon, alias, "claude", None, lane.pid());
    h.daemon
        .operator_rpc(
            "platform_grant",
            json!({"agent":alias,"platform":"local","account":"local","scopes":["publish"]}),
        )
        .unwrap();
    let before = h
        .daemon
        .operator_rpc(
            "app_effect_list",
            json!({"install_id":h.install["install_id"]}),
        )
        .unwrap();
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    let count_before: i64 = db
        .query_row("SELECT count(*) FROM platform_effects", [], |r| r.get(0))
        .unwrap();
    for detached in [false, true] {
        let granted = native(
            &mut lane,
            &h.daemon.state,
            detached,
            "platform_grants",
            json!({"agent":alias}),
        );
        assert_eq!(granted["ok"], true);
        assert!(
            granted["result"]["grants"]
                .as_array()
                .unwrap()
                .iter()
                .any(|g| g["platform"] == "local" && g["account"] == "local"),
            "caller lacks positive exact account grant"
        );
        for forged in [false, true] {
            let mut params = json!({"platform":"local","account":"local","tool":"publish_app_text","input":effect["record"]["input"]});
            if forged {
                params["operator"] = json!(true);
                params["authorization_kind"] = json!("app_artifact");
                params["effect_id"] = effect["effect_id"].clone();
            }
            let denied = native(
                &mut lane,
                &h.daemon.state,
                detached,
                "platform_call",
                params,
            );
            assert_eq!(
                denied["ok"], false,
                "legacy grant reached internal app tool"
            );
            assert_eq!(denied["error"]["kind"], "rejected");
        }
    }
    let count_after: i64 = db
        .query_row("SELECT count(*) FROM platform_effects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count_after, count_before,
        "internal tool refusal staged an ambient send"
    );
    assert_eq!(
        h.daemon
            .operator_rpc(
                "app_effect_list",
                json!({"install_id":h.install["install_id"]})
            )
            .unwrap(),
        before
    );
    assert!(h.items().as_array().unwrap().is_empty());
    assert_eq!(
        h.decide(&effect)["state"],
        "done",
        "internal tool guard broke approved app release"
    );
}

#[test]
fn cad692_actual_shared_b_reviewer_cannot_inspect_a_release_material() {
    let h = Release::new();
    let a = h.context("A", A, "live-a");
    let b = h.context("B", B, "live-b");
    let ba = h.bind(&a, "live-binding-a");
    h.bind(&b, "live-binding-b");
    let ra = h.complete(&a, "live-run-a");
    let effect = h.stage(&ra, "live-effect-a");
    let rb = h.create(&b, "live-run-b");
    std::fs::write(h.daemon.state.join(format!("app-release-probe-{}.json",rb["id"].as_str().unwrap())),json!({"effect_id":effect["effect_id"],"install_id":h.install["install_id"],"binding_id":ba["id"],"canaries":[A,B]}).to_string()).unwrap();
    h.dispatch(&rb);
    let done = h.wait_state(rb["id"].as_str().unwrap(), "succeeded");
    assert_eq!(h.artifact(&done)["text"], format!("Context draft: {B}"));
    let journal = std::fs::read_to_string(
        h.daemon
            .state
            .join("agents")
            .join(REVIEWER)
            .join("app-release-receipts.jsonl"),
    )
    .unwrap();
    let receipt = journal
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|entry| entry["run_id"] == rb["id"] && entry["native_release_probes"].is_array())
        .expect("actual B reviewer probe receipt");
    let cases = receipt["native_release_probes"].as_array().unwrap();
    assert_eq!(cases.len(), 14);
    assert!(cases.iter().all(|case| case["redacted"] == true));
    assert_eq!(cases.iter().filter(|case| case["ok"] == true).count(), 4);
    assert_eq!(
        h.daemon
            .operator_rpc("app_effect_show", json!({"effect_id":effect["effect_id"]}))
            .unwrap()["effect"],
        effect,
        "current other-context worker changed waiting release"
    );
    assert!(h.items().as_array().unwrap().is_empty());
}

#[test]
fn cad692_waiting_restart_changed_trusted_local_sink_refuses_old_release() {
    let h = Release::new();
    let c = h.context("Sink", A, "sink-context");
    h.bind(&c, "sink-binding");
    let run = h.complete(&c, "sink-run");
    let effect = h.stage(&run, "sink-effect");
    let original = h
        .daemon
        .operator_rpc("connection_show", json!({"connection_id":h.connection}))
        .unwrap()["connection"]
        .clone();
    assert_eq!(original["provider"], "local");
    assert!(h.items().as_array().unwrap().is_empty());
    let Release {
        root,
        daemon,
        install: _,
        connection,
    } = h;
    let state = daemon.state.clone();
    drop(daemon);
    let new_sink = root.path().join("changed-trusted-outbox");
    let mut opts = common::daemon_opts();
    opts.provider_env
        .set("CADENCE_PM_DIR", root.path().join("pm").to_str().unwrap());
    cadence_agent::platform::local::register_at(
        &state,
        &mut opts,
        new_sink.clone(),
        "http://localhost:3119".into(),
    );
    let restarted = common::TestDaemon::start_on_opts(state, opts);
    let current = restarted
        .operator_rpc("connection_show", json!({"connection_id":connection}))
        .unwrap()["connection"]
        .clone();
    assert_eq!(
        current["id"], original["id"],
        "trusted sink change unexpectedly changed workspace connection identity"
    );
    assert_ne!(
        current["registration_digest"], original["registration_digest"],
        "trusted composition control lacks a changed sink receipt"
    );
    let restored = restarted
        .operator_rpc("app_effect_show", json!({"effect_id":effect["effect_id"]}))
        .unwrap()["effect"]
        .clone();
    assert_eq!(
        restored["state"], "waiting",
        "restart itself masked final current-sink guard"
    );
    assert_eq!(restored["authority"], effect["authority"]);
    assert_eq!(restored["digest"], effect["digest"]);
    let refused = restarted
        .operator_rpc(
            "app_effect_decide",
            json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"}),
        )
        .unwrap_err();
    assert_eq!(
        refused.kind(),
        "rejected",
        "changed Local registration released old approved sink"
    );
    assert!(restarted
        .operator_rpc("platform_outbox", json!({}))
        .unwrap()["items"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(
        !root
            .path()
            .join("outbox/app-items")
            .join(effect["effect_id"].as_str().unwrap())
            .exists(),
        "old sink received unapproved release"
    );
    assert!(
        !new_sink
            .join("app-items")
            .join(effect["effect_id"].as_str().unwrap())
            .exists(),
        "new sink inherited old approval"
    );
}
