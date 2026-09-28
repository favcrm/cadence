//! CAD692 reviewed Local release through actual native provider turns.
#![allow(clippy::disallowed_methods)]
mod common;
use common::app_release::{Release, A, B, OWNER, REVIEWER, WRITER};
use common::{plant_member_pane, LaneShell};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
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

fn rewrite_asset_receipt_as_v1(db: &rusqlite::Connection, receipt: &Value, media_type: &str) {
    let turn_id: String = db
        .query_row(
            "SELECT turn_id FROM app_capability_results WHERE id=?",
            [receipt["id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    let legacy_digest = cadence_agent::store::app_runs::material_digest(&json!({
        "run_id":receipt["run_id"],"step_id":receipt["step_id"],
        "message_id":receipt["message_id"],"turn_id":turn_id,
        "slot":receipt["slot"],"request_id":receipt["request_id"],
        "binding_digest":receipt["binding_digest"],
        "input_digest":receipt["input_digest"],"result":receipt["result"],
        "asset_digest":receipt["asset"]["digest"]
    }));
    db.execute(
        "UPDATE app_capability_results SET receipt_schema=1,result_digest=?,asset_type=? WHERE id=?",
        rusqlite::params![legacy_digest,media_type,receipt["id"].as_str().unwrap()],
    ).unwrap();
}

#[test]
fn cad632_actual_turn_read_is_once_scoped_and_selected_post_is_frozen() {
    let (h, calls, price) = Release::with_capability();
    let a = h.context("Client A", A, "source-context-a");
    let source_a = h
        .daemon
        .operator_rpc(
            "app_binding_create",
            json!({
                "install_id":h.install["install_id"],"context_id":a["id"],"slot":"source",
                "connection_id":h.connection,"request_id":"source-binding-a"
            }),
        )
        .unwrap()["binding"]
        .clone();
    let quoted = h
        .daemon
        .operator_rpc(
            "app_binding_quote",
            json!({
                "install_id":h.install["install_id"],"context_id":a["id"],"slot":"source"
            }),
        )
        .unwrap();
    assert_eq!(quoted["binding_digest"], source_a["digest"]);
    assert_eq!(quoted["quote"]["total_price_micros"], 1880);
    let run_a = h.create(&a, "source-run-a");
    assert_eq!(run_a["snapshot"]["quotes"]["source"], quoted["quote"]);
    std::fs::write(
        h.daemon.state.join(format!(
            "app-capability-probe-{}.json",
            run_a["id"].as_str().unwrap()
        )),
        json!({"source":A,"context_id":a["id"],"install_id":h.install["install_id"]}).to_string(),
    )
    .unwrap();
    h.dispatch(&run_a);
    let finished_a = h.wait_state(run_a["id"].as_str().unwrap(), "succeeded");
    let result_a = h
        .daemon
        .operator_rpc(
            "app_run_capability_results",
            json!({"run_id":finished_a["id"]}),
        )
        .unwrap()["results"][0]
        .clone();
    assert_eq!(result_a["slot"], "source");
    assert_eq!(
        result_a["result"]["posts"][0]["caption"],
        format!("CONTEXT_SOURCE={A}\nSecond line")
    );
    let asset = h
        .daemon
        .operator_rpc(
            "app_run_capability_asset",
            json!({"receipt_id":result_a["id"]}),
        )
        .unwrap();
    assert_eq!(asset["digest"], result_a["asset"]["digest"]);
    assert_eq!(asset["media_type"], "application/octet-stream");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "concurrent replay reached provider twice"
    );
    assert!(result_a.get("turn_id").is_none());
    let probes: Value = serde_json::from_str(
        &std::fs::read_to_string(h.daemon.state.join(format!(
            "app-capability-result-{}.json",
            run_a["id"].as_str().unwrap()
        )))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(probes["id"], result_a["id"]);
    assert_eq!(probes["probes"].as_array().unwrap().len(), 9);

    let selected = h
        .daemon
        .operator_rpc(
            "app_run_create",
            json!({
                "install_id":h.install["install_id"],"context_id":a["id"],"workflow":"draft",
                "inputs":{"subject":"Selected post","writer":WRITER,"reviewer":REVIEWER},
                "request_id":"source-selected-a","owner_pm":OWNER,
                "source_receipt_id":result_a["id"],"selected_post_id":"post-1"
            }),
        )
        .unwrap();
    assert_eq!(
        selected["snapshot"]["source"]["post"]["caption"],
        result_a["result"]["posts"][0]["caption"]
    );
    assert_eq!(
        selected["snapshot"]["inputs"]["source"],
        format!("CONTEXT_SOURCE={A} Second line")
    );
    assert_eq!(
        selected["snapshot"]["source"]["binding_digest"],
        source_a["digest"]
    );
    price.store(2000, Ordering::SeqCst);
    assert!(
        h.daemon
            .operator_rpc(
                "app_run_approve",
                json!({
                    "run_id":selected["id"],"digest":selected["snapshot_digest"]
                })
            )
            .is_err(),
        "live price change must refuse frozen run approval"
    );
    price.store(1880, Ordering::SeqCst);
    for changed in [
        json!({"install_id":h.install["install_id"],"context_id":a["id"],"workflow":"draft",
            "inputs":{"subject":"Changed","writer":WRITER,"reviewer":REVIEWER,"source":"forged"},
            "request_id":"source-forged-input","owner_pm":OWNER,
            "source_receipt_id":result_a["id"],"selected_post_id":"post-1"}),
        json!({"install_id":h.install["install_id"],"context_id":a["id"],"workflow":"draft",
            "inputs":{"subject":"Missing","writer":WRITER,"reviewer":REVIEWER},
            "request_id":"source-forged-post","owner_pm":OWNER,
            "source_receipt_id":result_a["id"],"selected_post_id":"not-present"}),
    ] {
        assert!(h.daemon.operator_rpc("app_run_create", changed).is_err());
    }
    let b = h.context("Client B", B, "source-context-b");
    h.daemon
        .operator_rpc(
            "app_binding_create",
            json!({
                "install_id":h.install["install_id"],"context_id":b["id"],"slot":"source",
                "connection_id":h.connection,"request_id":"source-binding-b"
            }),
        )
        .unwrap();
    assert!(h
        .daemon
        .operator_rpc(
            "app_run_create",
            json!({
                "install_id":h.install["install_id"],"context_id":b["id"],"workflow":"draft",
                "inputs":{"subject":"Cross","writer":WRITER,"reviewer":REVIEWER},
                "request_id":"source-cross-context","owner_pm":OWNER,
                "source_receipt_id":result_a["id"],"selected_post_id":"post-1"
            })
        )
        .is_err());
    let run_b = h.create(&b, "source-run-b");
    std::fs::write(
        h.daemon.state.join(format!(
            "app-capability-probe-{}.json",
            run_b["id"].as_str().unwrap()
        )),
        json!({"source":B,"context_id":b["id"],"install_id":h.install["install_id"],"other_receipt_id":result_a["id"]}).to_string(),
    )
    .unwrap();
    h.dispatch(&run_b);
    h.wait_state(run_b["id"].as_str().unwrap(), "succeeded");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    h.daemon
        .operator_rpc(
            "app_binding_revoke",
            json!({
                "install_id":h.install["install_id"],"binding_id":source_a["id"],
                "expected_revision":source_a["revision"]
            }),
        )
        .unwrap();
    assert!(
        h.daemon
            .operator_rpc(
                "app_run_approve",
                json!({
                    "run_id":selected["id"],"digest":selected["snapshot_digest"]
                })
            )
            .is_err(),
        "revocation released an old run capability"
    );
    assert!(
        h.daemon
            .operator_rpc(
                "app_run_create",
                json!({
                    "install_id":h.install["install_id"],"context_id":a["id"],"workflow":"draft",
                    "inputs":{"subject":"After revoke","writer":WRITER,"reviewer":REVIEWER},
                    "request_id":"source-after-revoke","owner_pm":OWNER,
                    "source_receipt_id":result_a["id"],"selected_post_id":"post-1"
                })
            )
            .is_err(),
        "revoked source binding was reused by a new run"
    );
    assert_eq!(
        h.daemon
            .operator_rpc(
                "app_run_capability_result",
                json!({"receipt_id":result_a["id"]})
            )
            .unwrap()["id"],
        result_a["id"],
        "operator lost historical audit receipt"
    );
}

#[test]
fn cad709_failed_run_retains_verified_source_for_a_new_plan_without_another_read() {
    let (h, calls, _) = Release::with_capability();
    let context = h.context("Recovery client", A, "source-recovery-context");
    let binding = h
        .daemon
        .operator_rpc(
            "app_binding_create",
            json!({
                "install_id":h.install["install_id"], "context_id":context["id"],
                "slot":"source", "connection_id":h.connection,
                "request_id":"source-recovery-binding"
            }),
        )
        .unwrap()["binding"]
        .clone();
    let run = h.create(&context, "source-recovery-read");
    let run_id = run["id"].as_str().unwrap();
    std::fs::write(
        h.daemon
            .state
            .join(format!("app-capability-probe-{run_id}.json")),
        json!({"source":A,"context_id":context["id"],"install_id":h.install["install_id"]})
            .to_string(),
    )
    .unwrap();
    // The broker persists the paid read, then the independent reviewer fails.
    std::fs::write(
        h.daemon
            .state
            .join(format!("app-review-asset-override-{run_id}.json")),
        json!({"asset_receipt_id":"missing-receipt","asset_sha256":"sha256:missing"}).to_string(),
    )
    .unwrap();
    h.dispatch(&run);
    assert_eq!(h.wait_state(run_id, "failed")["state"], "failed");
    let receipt = h
        .daemon
        .operator_rpc("app_run_capability_results", json!({"run_id":run_id}))
        .unwrap()["results"][0]
        .clone();
    assert_eq!(receipt["slot"], "source");
    assert_eq!(receipt["binding_digest"], binding["digest"]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let selected = h
        .daemon
        .operator_rpc(
            "app_run_create",
            json!({
                "install_id":h.install["install_id"],"context_id":context["id"],"workflow":"draft",
                "inputs":{"subject":"Recovered source","writer":WRITER,"reviewer":REVIEWER},
                "request_id":"source-recovery-caption","owner_pm":OWNER,
                "source_receipt_id":receipt["id"],"selected_post_id":"post-1"
            }),
        )
        .unwrap();
    assert_eq!(
        selected["snapshot"]["source"]["receipt_digest"],
        receipt["digest"]
    );
    assert_eq!(selected["snapshot"]["source"]["source_run_id"], run["id"]);
    assert_eq!(
        selected["snapshot"]["inputs"]["source"],
        format!("CONTEXT_SOURCE={A} Second line")
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "selection re-ran the paid provider read"
    );
    let mut lane = LaneShell::spawn(h.root.path());
    plant_member_pane(
        &h.daemon,
        "source-native-worker",
        "claude",
        None,
        lane.pid(),
    );
    for detached in [false, true] {
        assert_eq!(
            native(
                &mut lane,
                &h.daemon.state,
                detached,
                "app_run_create",
                json!({
                    "install_id":h.install["install_id"],"context_id":context["id"],"workflow":"draft",
                    "inputs":{"subject":"Stolen source","writer":WRITER,"reviewer":REVIEWER},
                    "request_id":"source-recovery-stolen","owner_pm":OWNER,
                    "source_receipt_id":receipt["id"],"selected_post_id":"post-1"
                })
            )["ok"],
            false,
            "agent or detached child selected an operator source receipt"
        );
    }

    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    db.execute(
        "UPDATE app_runs SET approved_digest=NULL WHERE id=?",
        [run_id],
    )
    .unwrap();
    assert!(h.daemon.operator_rpc("app_run_create", json!({
        "install_id":h.install["install_id"],"context_id":context["id"],"workflow":"draft",
        "inputs":{"subject":"Lost approval","writer":WRITER,"reviewer":REVIEWER},
        "request_id":"source-recovery-no-approval","owner_pm":OWNER,
        "source_receipt_id":receipt["id"],"selected_post_id":"post-1"
    })).is_err(), "a failed run with lost approval cannot release a receipt");
    db.execute(
        "UPDATE app_runs SET approved_digest=? WHERE id=?",
        rusqlite::params![run["snapshot_digest"].as_str(), run_id],
    )
    .unwrap();
    db.execute(
        "UPDATE app_capability_claims SET call_id='other-call' WHERE run_id=? AND slot='source'",
        [run_id],
    )
    .unwrap();
    assert!(h.daemon.operator_rpc("app_run_create", json!({
        "install_id":h.install["install_id"],"context_id":context["id"],"workflow":"draft",
        "inputs":{"subject":"Broken claim","writer":WRITER,"reviewer":REVIEWER},
        "request_id":"source-recovery-no-claim","owner_pm":OWNER,
        "source_receipt_id":receipt["id"],"selected_post_id":"post-1"
    })).is_err(), "a receipt without the original broker claim cannot be reused");
    db.execute(
        "UPDATE app_capability_claims SET call_id=? WHERE run_id=? AND slot='source'",
        rusqlite::params![receipt["id"].as_str(), run_id],
    )
    .unwrap();

    for (request_id, context_id, post_id, supplied) in [
        (
            "source-recovery-forged-post",
            context["id"].clone(),
            "other-post",
            None,
        ),
        (
            "source-recovery-forged-caption",
            context["id"].clone(),
            "post-1",
            Some("forged"),
        ),
    ] {
        let mut inputs = json!({"subject":"Forged","writer":WRITER,"reviewer":REVIEWER});
        if let Some(text) = supplied {
            inputs["source"] = json!(text);
        }
        assert!(h
            .daemon
            .operator_rpc(
                "app_run_create",
                json!({
                    "install_id":h.install["install_id"],"context_id":context_id,"workflow":"draft",
                    "inputs":inputs,"request_id":request_id,"owner_pm":OWNER,
                    "source_receipt_id":receipt["id"],"selected_post_id":post_id
                })
            )
            .is_err());
    }
    let other = h.context("Other client", B, "source-recovery-other-context");
    h.daemon
        .operator_rpc(
            "app_binding_create",
            json!({
                "install_id":h.install["install_id"],"context_id":other["id"],"slot":"source",
                "connection_id":h.connection,"request_id":"source-recovery-other-binding"
            }),
        )
        .unwrap();
    assert!(h
        .daemon
        .operator_rpc(
            "app_run_create",
            json!({
                "install_id":h.install["install_id"],"context_id":other["id"],"workflow":"draft",
                "inputs":{"subject":"Cross context","writer":WRITER,"reviewer":REVIEWER},
                "request_id":"source-recovery-cross-context","owner_pm":OWNER,
                "source_receipt_id":receipt["id"],"selected_post_id":"post-1"
            })
        )
        .is_err());
    h.daemon
        .operator_rpc(
            "app_binding_revoke",
            json!({
                "install_id":h.install["install_id"],"binding_id":binding["id"],
                "expected_revision":binding["revision"]
            }),
        )
        .unwrap();
    assert!(h
        .daemon
        .operator_rpc(
            "app_run_create",
            json!({
                "install_id":h.install["install_id"],"context_id":context["id"],"workflow":"draft",
                "inputs":{"subject":"Stale binding","writer":WRITER,"reviewer":REVIEWER},
                "request_id":"source-recovery-stale-binding","owner_pm":OWNER,
                "source_receipt_id":receipt["id"],"selected_post_id":"post-1"
            })
        )
        .is_err());
}

#[test]
fn cad709_context_free_capability_run_approves_only_its_frozen_snapshot() {
    let (h, _calls, _price) = Release::with_capability();
    for (slot, request_id) in [
        ("publication", "context-free-capability-publication"),
        ("source", "context-free-capability-source"),
    ] {
        let binding = h
            .daemon
            .operator_rpc(
                "app_binding_create",
                json!({"install_id":h.install["install_id"],"slot":slot,
                    "connection_id":h.connection,"request_id":request_id}),
            )
            .unwrap()["binding"]
            .clone();
        assert!(binding["context_id"].is_null());
    }
    let created = h
        .daemon
        .operator_rpc(
            "app_run_create",
            json!({"install_id":h.install["install_id"],"workflow":"draft",
                "inputs":{"subject":"No brand context","source":format!("CONTEXT_SOURCE={A}"),
                    "writer":WRITER,"reviewer":REVIEWER},
                "request_id":"context-free-capability-run","owner_pm":OWNER}),
        )
        .unwrap();
    assert_eq!(created["snapshot"]["schema"], 4);
    assert!(created["context_id"].is_null());
    assert_eq!(created["snapshot"].get("context"), Some(&Value::Null));
    assert!(created["snapshot"]["capabilities"]["source"].is_object());
    assert!(
        h.daemon
            .operator_rpc(
                "app_run_approve",
                json!({"run_id":created["id"],"digest":created["snapshot_digest"],
                "context_id":"forged-context"}),
            )
            .is_err(),
        "caller cannot supply a context to a context-free approval"
    );
    assert!(
        h.daemon
            .operator_rpc(
                "app_run_approve",
                json!({"run_id":created["id"],"digest":"forged-snapshot-digest"}),
            )
            .is_err(),
        "caller cannot approve a different snapshot"
    );
    let approved = h
        .daemon
        .operator_rpc(
            "app_run_approve",
            json!({"run_id":created["id"],"digest":created["snapshot_digest"]}),
        )
        .unwrap();
    assert_eq!(approved["state"], "approved");
}

#[test]
fn cad713_reviewed_run_asset_is_pinned_into_one_local_outbox_draft() {
    let (h, calls, _) = Release::with_capability();
    let context = h.context("Client A", A, "asset-context-a");
    h.bind(&context, "asset-publication-a");
    h.daemon
        .operator_rpc(
            "app_binding_create",
            json!({
                "install_id":h.install["install_id"],"context_id":context["id"],"slot":"source",
                "connection_id":h.connection,"request_id":"asset-source-binding-a"
            }),
        )
        .unwrap();
    let run = h.create(&context, "asset-run-a");
    std::fs::write(
        h.daemon.state.join(format!(
            "app-capability-probe-{}.json",
            run["id"].as_str().unwrap()
        )),
        json!({"source":A,"context_id":context["id"],"install_id":h.install["install_id"]})
            .to_string(),
    )
    .unwrap();
    h.dispatch(&run);
    let completed = h.wait_state(run["id"].as_str().unwrap(), "succeeded");
    let receipt = h
        .daemon
        .operator_rpc("app_run_capability_results", json!({"run_id":run["id"]}))
        .unwrap()["results"][0]
        .clone();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(completed["reviews"][0]["asset_receipt_id"], receipt["id"]);
    assert_eq!(
        completed["reviews"][0]["asset_digest"],
        receipt["asset"]["digest"]
    );
    let waiting = h.stage(&completed, "asset-release-a");
    assert_eq!(waiting["authority"]["asset"]["receipt_id"], receipt["id"]);
    assert_eq!(
        waiting["authority"]["asset"]["digest"],
        receipt["asset"]["digest"]
    );
    assert!(h
        .daemon
        .operator_rpc(
            "app_effect_stage",
            json!({"run_id":run["id"],
        "artifact_id":completed["artifacts"][0]["id"],"slot":"publication",
        "request_id":"asset-forged-path","title":"Draft","asset_path":"/etc/passwd"})
        )
        .is_err());
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    db.execute(
        "UPDATE app_capability_results SET asset_type='image/png' WHERE id=?",
        [receipt["id"].as_str().unwrap()],
    )
    .unwrap();
    assert!(
        h.daemon
            .operator_rpc(
                "app_effect_decide",
                json!({"effect_id":waiting["effect_id"],
        "digest":waiting["digest"],"decision":"accept"})
            )
            .is_err(),
        "staged binary draft released after reviewed MIME changed"
    );
    assert!(h.items().as_array().unwrap().is_empty());
    db.execute(
        "UPDATE app_capability_results SET asset_type='application/octet-stream' WHERE id=?",
        [receipt["id"].as_str().unwrap()],
    )
    .unwrap();
    let mut lane = LaneShell::spawn(h.root.path());
    plant_member_pane(&h.daemon, "asset-native-worker", "claude", None, lane.pid());
    for detached in [false, true] {
        for (method, params) in [
            (
                "app_effect_stage",
                json!({"run_id":run["id"],"artifact_id":completed["artifacts"][0]["id"],
                "slot":"publication","request_id":"asset-native-forged","title":"Draft",
                "asset_receipt_id":receipt["id"]}),
            ),
            (
                "app_effect_decide",
                json!({"effect_id":waiting["effect_id"],"digest":waiting["digest"],
                "decision":"accept"}),
            ),
        ] {
            assert_eq!(
                native(&mut lane, &h.daemon.state, detached, method, params)["ok"],
                false,
                "agent or detached child used binary release authority"
            );
        }
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let outcomes = std::thread::scope(|scope| {
        let first = barrier.clone();
        let second = barrier.clone();
        let daemon = &h.daemon;
        let effect = &waiting;
        let one = scope.spawn(move || { first.wait(); daemon.operator_rpc("app_effect_decide", json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"})) });
        let two = scope.spawn(move || { second.wait(); daemon.operator_rpc("app_effect_decide", json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"})) });
        (one.join().unwrap(), two.join().unwrap())
    });
    assert!(outcomes.0.is_ok() || outcomes.1.is_ok());
    let done = h
        .daemon
        .operator_rpc("app_effect_show", json!({"effect_id":waiting["effect_id"]}))
        .unwrap()["effect"]
        .clone();
    assert_eq!(done["state"], "done", "{}", done["record"]["outcome"]);
    let items = h.items();
    assert_eq!(items.as_array().unwrap().len(), 1);
    assert_eq!(
        items[0]["attachments"][0]["sha256"],
        receipt["asset"]["digest"]
            .as_str()
            .unwrap()
            .trim_start_matches("sha256:")
    );
    assert_eq!(
        items[0]["attachments"][0]["media_type"],
        receipt["asset"]["media_type"]
    );
    let path = h
        .root
        .path()
        .join("outbox/app-items")
        .join(waiting["effect_id"].as_str().unwrap())
        .join("attachments/asset.bin");
    assert_eq!(std::fs::read(&path).unwrap(), b"run-bound fixture asset");
    assert_eq!(
        h.stage(&completed, "asset-release-a")["effect_id"],
        waiting["effect_id"]
    );
    assert_eq!(h.items().as_array().unwrap().len(), 1);
    let executed:i64=db.query_row("SELECT count(*) FROM events WHERE kind='effect_executed' AND json_extract(payload,'$.effect_id')=?",
        [waiting["effect_id"].as_str().unwrap()],|r|r.get(0)).unwrap();
    assert_eq!(executed, 1, "concurrent binary release committed twice");
    std::fs::write(&path, b"changed asset").unwrap();
    let mut opts = cadence_agent::daemon::ServeOptions::default();
    cadence_agent::platform::local::register_at(
        &h.daemon.state,
        &mut opts,
        h.root.path().join("outbox"),
        "http://localhost:3119".into(),
    );
    assert_eq!(
        opts.platforms["local"].read_back("publish_app_text", &waiting["record"]["input"]),
        cadence_agent::contract_fixture::Verified::False,
        "outbox read-back accepted corrupted binary attachment"
    );
    std::fs::remove_file(&path).unwrap();
    let escaped = h.root.path().join("outside-asset");
    std::fs::write(&escaped, b"run-bound fixture asset").unwrap();
    std::os::unix::fs::symlink(&escaped, &path).unwrap();
    assert_eq!(
        opts.platforms["local"].read_back("publish_app_text", &waiting["record"]["input"]),
        cadence_agent::contract_fixture::Verified::False,
        "outbox read-back followed an attachment symlink"
    );
    let stage_new = |request: &str| {
        h.daemon.operator_rpc(
            "app_effect_stage",
            json!({
        "run_id":completed["id"],"artifact_id":completed["artifacts"][0]["id"],
        "slot":"publication","request_id":request,"title":"Reviewed draft"}),
        )
    };
    db.execute(
        "UPDATE app_capability_results SET asset_type='image/png' WHERE id=?",
        [receipt["id"].as_str().unwrap()],
    )
    .unwrap();
    assert!(
        stage_new("asset-changed-mime").is_err(),
        "changed MIME retained old review"
    );
    db.execute("UPDATE app_capability_results SET asset_type='application/octet-stream',asset=zeroblob(2097153) WHERE id=?",
        [receipt["id"].as_str().unwrap()]).unwrap();
    assert!(
        stage_new("asset-oversize").is_err(),
        "oversized asset retained old review"
    );
    assert_eq!(h.items().as_array().unwrap().len(), 1);
}

#[test]
fn cad713_cross_run_review_asset_receipt_cannot_complete_or_stage() {
    let (h, _, _) = Release::with_capability();
    let context = h.context("Client A", A, "asset-forged-context");
    h.bind(&context, "asset-forged-publication");
    h.daemon
        .operator_rpc(
            "app_binding_create",
            json!({"install_id":h.install["install_id"],
        "context_id":context["id"],"slot":"source","connection_id":h.connection,
        "request_id":"asset-forged-source"}),
        )
        .unwrap();
    let first = h.create(&context, "asset-source-run");
    std::fs::write(
        h.daemon.state.join(format!(
            "app-capability-probe-{}.json",
            first["id"].as_str().unwrap()
        )),
        json!({"source":A,"context_id":context["id"],"install_id":h.install["install_id"]})
            .to_string(),
    )
    .unwrap();
    h.dispatch(&first);
    h.wait_state(first["id"].as_str().unwrap(), "succeeded");
    let other = h
        .daemon
        .operator_rpc("app_run_capability_results", json!({"run_id":first["id"]}))
        .unwrap()["results"][0]
        .clone();
    let run = h.create(&context, "asset-forged-run");
    std::fs::write(
        h.daemon.state.join(format!(
            "app-capability-probe-{}.json",
            run["id"].as_str().unwrap()
        )),
        json!({"source":A,"context_id":context["id"],"install_id":h.install["install_id"]})
            .to_string(),
    )
    .unwrap();
    std::fs::write(
        h.daemon.state.join(format!(
            "app-review-asset-override-{}.json",
            run["id"].as_str().unwrap()
        )),
        json!({"asset_receipt_id":other["id"],"asset_sha256":other["asset"]["digest"]}).to_string(),
    )
    .unwrap();
    h.dispatch(&run);
    let failed = h.wait_state(run["id"].as_str().unwrap(), "failed");
    assert!(failed["reviews"].as_array().unwrap().is_empty());
    assert!(h
        .daemon
        .operator_rpc(
            "app_effect_stage",
            json!({"run_id":run["id"],
        "artifact_id":failed["artifacts"][0]["id"],"slot":"publication",
        "request_id":"asset-forged-stage","title":"Forged"})
        )
        .is_err());
    assert!(h.items().as_array().unwrap().is_empty());
}

#[test]
fn cad713_legacy_asset_receipt_cannot_release_with_unreviewed_mime() {
    let (h, _, _) = Release::with_capability();
    let context = h.context("Client A", A, "legacy-asset-context");
    h.bind(&context, "legacy-asset-publication");
    h.daemon
        .operator_rpc(
            "app_binding_create",
            json!({"install_id":h.install["install_id"],"context_id":context["id"],
                "slot":"source","connection_id":h.connection,
                "request_id":"legacy-asset-source"}),
        )
        .unwrap();
    let run = h.create(&context, "legacy-asset-run");
    std::fs::write(
        h.daemon.state.join(format!(
            "app-capability-probe-{}.json",
            run["id"].as_str().unwrap()
        )),
        json!({"source":A,"context_id":context["id"],"install_id":h.install["install_id"]})
            .to_string(),
    )
    .unwrap();
    h.dispatch(&run);
    let completed = h.wait_state(run["id"].as_str().unwrap(), "succeeded");
    let receipt = h
        .daemon
        .operator_rpc("app_run_capability_results", json!({"run_id":run["id"]}))
        .unwrap()["results"][0]
        .clone();
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    rewrite_asset_receipt_as_v1(&db, &receipt, "image/png");
    assert_eq!(
        h.daemon
            .operator_rpc(
                "app_run_capability_result",
                json!({"receipt_id":receipt["id"]})
            )
            .unwrap()["asset"]["media_type"],
        "image/png",
        "historical result reads must remain available"
    );
    assert!(
        h.daemon
            .operator_rpc(
                "app_effect_stage",
                json!({
                    "run_id":completed["id"],"artifact_id":completed["artifacts"][0]["id"],
                    "slot":"publication","request_id":"legacy-mime-swap","title":"Reviewed draft"
                })
            )
            .is_err(),
        "v1 asset hash does not authenticate MIME and cannot back a new release"
    );
    assert!(h.items().as_array().unwrap().is_empty());
}

#[test]
fn cad713_legacy_asset_receipt_cannot_receive_new_asset_review() {
    let (h, _, _) = Release::with_capability();
    let context = h.context("Client A", A, "legacy-review-context");
    h.bind(&context, "legacy-review-publication");
    h.daemon
        .operator_rpc(
            "app_binding_create",
            json!({
                "install_id":h.install["install_id"],"context_id":context["id"],
                "slot":"source","connection_id":h.connection,
                "request_id":"legacy-review-source"
            }),
        )
        .unwrap();
    let run = h.create(&context, "legacy-review-run");
    let run_id = run["id"].as_str().unwrap();
    std::fs::write(
        h.daemon
            .state
            .join(format!("context-hold-reviewer-{run_id}")),
        "hold",
    )
    .unwrap();
    std::fs::write(
        h.daemon
            .state
            .join(format!("app-capability-probe-{run_id}.json")),
        json!({"source":A,"context_id":context["id"],"install_id":h.install["install_id"]})
            .to_string(),
    )
    .unwrap();
    h.dispatch(&run);
    let held = h
        .daemon
        .state
        .join(format!("context-reviewer-{run_id}.held"));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !held.exists() {
        assert!(
            Instant::now() < deadline,
            "reviewer did not reach asset hold"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let receipt = h
        .daemon
        .operator_rpc("app_run_capability_results", json!({"run_id":run_id}))
        .unwrap()["results"][0]
        .clone();
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    rewrite_asset_receipt_as_v1(&db, &receipt, "application/octet-stream");
    assert_eq!(
        h.daemon
            .operator_rpc(
                "app_run_capability_result",
                json!({"receipt_id":receipt["id"]})
            )
            .unwrap()["id"],
        receipt["id"]
    );
    std::fs::write(
        h.daemon
            .state
            .join(format!("context-reviewer-{run_id}.release")),
        "go",
    )
    .unwrap();
    let failed = h.wait_state(run_id, "failed");
    assert!(
        failed["reviews"].as_array().unwrap().is_empty(),
        "v1 receipt received a new binary review despite unauthenticated MIME"
    );
    assert!(h
        .daemon
        .operator_rpc(
            "app_effect_stage",
            json!({
                "run_id":run_id,"artifact_id":failed["artifacts"][0]["id"],
                "slot":"publication","request_id":"legacy-review-stage","title":"Reviewed draft"
            })
        )
        .is_err());
    assert!(h.items().as_array().unwrap().is_empty());
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
    for table in ["platform_grants", "platform_defaults"] {
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
    // The routing PM already owns this real run. Attach its inert fixture
    // row to our actual shell instead of registering a duplicate alias.
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    assert_eq!(db.execute(
        "UPDATE agents SET endpoint_kind='pty',pid=?,pid_start=?,enabled=0,generation='planted',session_id='planted' WHERE alias=? AND role='pm' AND provider='inbox'",
        rusqlite::params![owner.pid() as i64,common::proc_start(owner.pid()),OWNER],
    ).unwrap(),1);
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

#[test]
fn cad692_same_accepted_artifact_two_effects_have_distinct_intact_receipts() {
    let h = Release::new();
    let c = h.context("Two releases", A, "same-artifact-context");
    h.bind(&c, "same-artifact-binding");
    let run = h.complete(&c, "same-artifact-run");
    let first = h.stage(&run, "same-artifact-first");
    let first_done = h.decide(&first);
    assert_eq!(first_done["state"], "done");
    assert_eq!(first_done["record"]["outcome"]["verified"], true);
    let first_item = h
        .daemon
        .operator_rpc("platform_outbox", json!({"effect_id":first["effect_id"]}))
        .unwrap()["item"]
        .clone();
    let second = h.stage(&run, "same-artifact-second");
    assert_ne!(first["effect_id"], second["effect_id"]);
    assert_eq!(second["state"], "waiting");
    assert_eq!(
        h.items().as_array().unwrap().len(),
        1,
        "staging matched another completed item's content"
    );
    let second_done = h.decide(&second);
    assert_eq!(second_done["state"], "done");
    assert_eq!(second_done["record"]["outcome"]["verified"], true);
    let second_item = h
        .daemon
        .operator_rpc("platform_outbox", json!({"effect_id":second["effect_id"]}))
        .unwrap()["item"]
        .clone();
    assert_eq!(h.items().as_array().unwrap().len(), 2);
    assert_eq!(first_item["post"], second_item["post"]);
    assert_eq!(first_item["effect_id"], first["effect_id"]);
    assert_eq!(second_item["effect_id"], second["effect_id"]);
    assert_eq!(first_item["provenance"]["effect_id"], first["effect_id"]);
    assert_eq!(second_item["provenance"]["effect_id"], second["effect_id"]);
    assert_eq!(
        first_item["provenance"]["artifact_id"],
        second_item["provenance"]["artifact_id"]
    );
    assert!(first_item["provenance"]["authority_digest"].is_string());
    assert!(second_item["provenance"]["authority_digest"].is_string());
    // The accepted run/artifact/binding authority is identical. Effect identity
    // is pinned separately by provenance, the full release and encoded input.
    assert_eq!(
        first_item["provenance"]["authority_digest"],
        second_item["provenance"]["authority_digest"]
    );
    for receipt in [&first_done, &second_done] {
        assert!(receipt["digest"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
    }
    assert_ne!(
        first_done["digest"], second_done["digest"],
        "second release borrowed the first effect's decision digest"
    );
    for item in [&first_item, &second_item] {
        assert!(item["input_digest"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
    }
    assert_ne!(
        first_item["input_digest"], second_item["input_digest"],
        "second release borrowed the first effect's encoded input receipt"
    );
    for receipt in [&first_done, &second_done] {
        assert_eq!(
            h.daemon
                .operator_rpc("app_effect_show", json!({"effect_id":receipt["effect_id"]}))
                .unwrap()["effect"],
            *receipt,
            "second release changed an exact durable effect receipt"
        );
    }
    assert_eq!(
        h.daemon
            .operator_rpc("platform_outbox", json!({"effect_id":first["effect_id"]}))
            .unwrap()["item"],
        first_item,
        "second release changed first item's durable receipt"
    );
}

#[test]
fn cad692_full_preview_bound_rejects_before_effect_write_without_truncating_artifact() {
    let h = Release::new();
    let bounded = "é".repeat(5000);
    let c = h.context("Bounded", "REPEAT_UTF8_E_5000", "preview-positive");
    h.bind(&c, "preview-positive-binding");
    let run = h.complete(&c, "preview-positive-run");
    let artifact = h.artifact(&run);
    assert_eq!(artifact["text"], format!("Context draft: {bounded}"));
    let effect = h.stage(&run, "preview-positive-effect");
    assert_eq!(effect["state"], "waiting");
    assert!(effect["record"]["preview"].as_str().unwrap().len() <= 16 * 1024);
    assert_eq!(h.decide(&effect)["state"], "done");
    let item = h
        .daemon
        .operator_rpc("platform_outbox", json!({"effect_id":effect["effect_id"]}))
        .unwrap();
    assert_eq!(
        item["item"]["post"],
        format!("# Reviewed draft\n\nContext draft: {bounded}")
    );
    let huge = "x".repeat(17 * 1024);
    let c = h.context("Too large", "REPEAT_ASCII_X_17408", "preview-negative");
    h.bind(&c, "preview-negative-binding");
    let run = h.complete(&c, "preview-negative-run");
    let artifact = h.artifact(&run);
    assert_eq!(artifact["text"], format!("Context draft: {huge}"));
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    let before: i64 = db
        .query_row("SELECT count(*) FROM platform_effects", [], |r| r.get(0))
        .unwrap();
    let refused=h.daemon.operator_rpc("app_effect_stage",json!({"run_id":run["id"],"artifact_id":artifact["id"],"slot":"publication","request_id":"oversized-preview","title":"Full preview"})).unwrap_err();
    assert_eq!(refused.kind(), "rejected");
    assert!(
        refused.to_string().contains("preview"),
        "wrong gate hid preview limit: {refused}"
    );
    let after: i64 = db
        .query_row("SELECT count(*) FROM platform_effects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        before, after,
        "oversized preview persisted an unreviewable effect"
    );
    assert_eq!(
        h.artifact(&run),
        artifact,
        "preview refusal truncated source artifact"
    );
    assert_eq!(h.items().as_array().unwrap().len(), 1);
}

#[test]
fn cad692_zero_row_checked_claim_refuses_actual_operator_without_adapter_write() {
    let h = Release::with_options(|opts, state| {
        let state = state.to_path_buf();
        opts.effect_execute_gate = Some(std::sync::Arc::new(move |row| {
            assert_eq!(row.state, "decided");
            let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
            db.execute_batch("CREATE TRIGGER reject_app_execution_claim BEFORE UPDATE OF state ON platform_effects WHEN new.authorization_kind='app_artifact' AND old.state='decided' AND new.state='executing' BEGIN SELECT RAISE(IGNORE); END;").unwrap();
            true
        }));
    });
    let c = h.context("Claim", A, "zero-claim-context");
    h.bind(&c, "zero-claim-binding");
    let run = h.complete(&c, "zero-claim-run");
    let effect = h.stage(&run, "zero-claim-effect");
    let rejected = h
        .daemon
        .operator_rpc(
            "app_effect_decide",
            json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"}),
        )
        .unwrap_err();
    assert_eq!(rejected.kind(), "rejected");
    assert!(
        rejected.to_string().contains("claim"),
        "checked claim oracle not reached: {rejected}"
    );
    let after = h
        .daemon
        .operator_rpc("app_effect_show", json!({"effect_id":effect["effect_id"]}))
        .unwrap()["effect"]
        .clone();
    assert_eq!(
        after["state"], "decided",
        "zero-row claim pretended executing or successful"
    );
    assert_eq!(after["authority"], effect["authority"]);
    assert!(
        h.items().as_array().unwrap().is_empty(),
        "zero-row claim invoked Local write"
    );
}

#[test]
fn cad692_claim_wins_local_commit_before_binding_revoke_and_keeps_actual_receipt() {
    let (gate, claimed, resume) = controlled_release_gate("executing");
    let h = Release::with_options(move |opts, _| {
        opts.app_release_claim_gate = Some(gate);
    });
    let c = h.context("Claim wins", A, "commit-context");
    let binding = h.bind(&c, "commit-binding");
    let run = h.complete(&c, "commit-run");
    let effect = h.stage(&run, "commit-effect");
    std::thread::scope(|scope| {
        let daemon = &h.daemon;
        let effect = &effect;
        let accept=scope.spawn(move ||daemon.operator_rpc("app_effect_decide",json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"})));
        claimed
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("actual checked-claim gate was not reached");
        let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
        let state: String = db
            .query_row(
                "SELECT state FROM platform_effects WHERE effect_id=?",
                [effect["effect_id"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            state, "executing",
            "trusted barrier was reached before actual checked claim"
        );
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let install = &h.install;
        let binding = &binding;
        let revoke=scope.spawn(move || {
            started_tx.send(()).unwrap();
            let result=daemon.operator_rpc("app_binding_revoke",json!({"install_id":install["install_id"],"binding_id":binding["id"],"expected_revision":binding["revision"]}));
            finished_tx.send(()).unwrap(); result
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let early = finished_rx.recv_timeout(std::time::Duration::from_millis(150));
        // Always release our own barrier before asserting, so a broken lock
        // yields a semantic failure instead of stranding provider/RPC threads.
        resume.send(()).unwrap();
        let accepted = accept.join().unwrap().unwrap();
        let revoked = revoke.join().unwrap().unwrap();
        assert!(
            matches!(early, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
            "binding revoke crossed claimed Local commit lock"
        );
        assert_eq!(accepted["effect"]["state"], "done");
        assert_eq!(revoked["binding"]["state"], "revoked");
    });
    let item = h
        .daemon
        .operator_rpc("platform_outbox", json!({"effect_id":effect["effect_id"]}))
        .unwrap();
    assert_eq!(
        item["item"]["post"],
        format!("# Reviewed draft\n\nContext draft: {A}")
    );
    let after = h
        .daemon
        .operator_rpc("app_effect_show", json!({"effect_id":effect["effect_id"]}))
        .unwrap()["effect"]
        .clone();
    assert_eq!(
        after["state"], "done",
        "later revoke erased an actual committed result"
    );
    assert_eq!(after["authority"], effect["authority"]);
}

#[test]
fn cad692_binding_revoke_wins_before_claim_and_actual_release_writes_nothing() {
    let (gate, decided, resume) = controlled_release_gate("decided");
    let h = Release::with_options(move |opts, _| {
        opts.effect_execute_gate = Some(gate);
    });
    let c = h.context("Revoke wins", A, "revoke-context");
    let binding = h.bind(&c, "revoke-binding");
    let run = h.complete(&c, "revoke-run");
    let effect = h.stage(&run, "revoke-effect");
    std::thread::scope(|scope| {
        let daemon = &h.daemon;
        let effect = &effect;
        let accept=scope.spawn(move ||daemon.operator_rpc("app_effect_decide",json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"})));
        decided
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("actual durable-decision gate was not reached");
        let revoked=daemon.operator_rpc("app_binding_revoke",json!({"install_id":h.install["install_id"],"binding_id":binding["id"],"expected_revision":binding["revision"]}));
        resume.send(()).unwrap();
        assert_eq!(revoked.unwrap()["binding"]["state"], "revoked");
        let refused = accept.join().unwrap().unwrap_err();
        assert_eq!(
            refused.kind(),
            "rejected",
            "revocation winning before claim still executed"
        );
    });
    assert!(
        h.items().as_array().unwrap().is_empty(),
        "revoked-before-claim release wrote Local item"
    );
    assert_eq!(h.artifact(&run)["text"], format!("Context draft: {A}"));
    let after = h
        .daemon
        .operator_rpc("app_effect_show", json!({"effect_id":effect["effect_id"]}))
        .unwrap()["effect"]
        .clone();
    assert_ne!(after["state"], "done");
    assert_eq!(after["authority"], effect["authority"]);
}

type ReleaseGate = std::sync::Arc<dyn Fn(&cadence_agent::store::EffectRow) -> bool + Send + Sync>;

fn controlled_release_gate(
    expected: &'static str,
) -> (
    ReleaseGate,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let resume_rx = std::sync::Mutex::new(resume_rx);
    let gate = std::sync::Arc::new(move |row: &cadence_agent::store::EffectRow| {
        assert_eq!(row.state, expected);
        if entered_tx.send(()).is_err() {
            return false;
        }
        resume_rx
            .lock()
            .unwrap()
            .recv_timeout(std::time::Duration::from_secs(30))
            .is_ok()
    });
    (gate, entered_rx, resume_tx)
}
#[test]
fn cad692_claim_wins_local_commit_before_context_archive_and_keeps_actual_receipt() {
    let (gate, claimed, resume) = controlled_release_gate("executing");
    let h = Release::with_options(move |opts, _| {
        opts.app_release_claim_gate = Some(gate);
    });
    let c = h.context("Claim wins", A, "commit-context");
    h.bind(&c, "commit-binding");
    let run = h.complete(&c, "commit-run");
    let effect = h.stage(&run, "commit-effect");
    std::thread::scope(|scope| {
        let daemon = &h.daemon;
        let effect = &effect;
        let accept=scope.spawn(move ||daemon.operator_rpc("app_effect_decide",json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"})));
        claimed
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("actual checked-claim gate was not reached");
        let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
        let state: String = db
            .query_row(
                "SELECT state FROM platform_effects WHERE effect_id=?",
                [effect["effect_id"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            state, "executing",
            "trusted barrier was reached before actual checked claim"
        );
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let install = &h.install;
        let context = &c;
        let revoke=scope.spawn(move || {
            started_tx.send(()).unwrap();
            let result=daemon.operator_rpc("app_context_archive",json!({"install_id":install["install_id"],"context_id":context["id"],"expected_revision":context["revision"]}));
            finished_tx.send(()).unwrap(); result
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let early = finished_rx.recv_timeout(std::time::Duration::from_millis(150));
        // Always release our own barrier before asserting, so a broken lock
        // yields a semantic failure instead of stranding provider/RPC threads.
        resume.send(()).unwrap();
        let accepted = accept.join().unwrap().unwrap();
        let revoked = revoke.join().unwrap().unwrap();
        assert!(
            matches!(early, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
            "context archive crossed claimed Local commit lock"
        );
        assert_eq!(accepted["effect"]["state"], "done");
        assert_eq!(revoked["context"]["state"], "archived");
    });
    let item = h
        .daemon
        .operator_rpc("platform_outbox", json!({"effect_id":effect["effect_id"]}))
        .unwrap();
    assert_eq!(
        item["item"]["post"],
        format!("# Reviewed draft\n\nContext draft: {A}")
    );
    let after = h
        .daemon
        .operator_rpc("app_effect_show", json!({"effect_id":effect["effect_id"]}))
        .unwrap()["effect"]
        .clone();
    assert_eq!(
        after["state"], "done",
        "later archive erased an actual committed result"
    );
    assert_eq!(after["authority"], effect["authority"]);
}

#[test]
fn cad692_operator_archives_historical_context_after_private_install_bundle_removed() {
    let h = Release::new();
    let context = h.context("Removed installation", A, "removed-install-context");
    h.bind(&context, "removed-install-binding");
    let run = h.complete(&context, "removed-install-run");
    let artifact = h.artifact(&run);
    let effect = h.stage(&run, "removed-install-effect");
    let bundle = h
        .root
        .path()
        .join("pm/.apps/installations")
        .join(h.install["install_id"].as_str().unwrap())
        .join("bundle");
    assert!(
        bundle.join("app.md").is_file(),
        "private fixture bundle removal control is absent"
    );
    std::fs::remove_dir_all(&bundle).unwrap();
    let archived=h.daemon.operator_rpc("app_context_archive",json!({"install_id":h.install["install_id"],"context_id":context["id"],"expected_revision":context["revision"]})).unwrap()["context"].clone();
    assert_eq!(
        archived["state"], "archived",
        "historical archive incorrectly requires current installed bundle"
    );
    assert_eq!(archived["id"], context["id"]);
    assert_eq!(
        h.artifact(&run),
        artifact,
        "historical archive lost actual accepted bytes"
    );
    assert!(h.daemon.operator_rpc("app_effect_decide",json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"decision":"accept"})).is_err(),"removed bundle inherited release authority");
    assert!(h.items().as_array().unwrap().is_empty());
}

#[test]
fn cad692_current_binding_revision_rejects_actual_late_reviewer_result() {
    let h = Release::new();
    let context = h.context("Revision fence", A, "revision-fence");
    let binding = h.bind(&context, "revision-held-binding");
    let run = h.create(&context, "revision-held-review");
    let id = run["id"].as_str().unwrap();
    std::fs::write(
        h.daemon.state.join(format!("context-hold-reviewer-{id}")),
        "hold",
    )
    .unwrap();
    h.dispatch(&run);
    let deadline = Instant::now() + Duration::from_secs(25);
    while !h
        .daemon
        .state
        .join(format!("context-reviewer-{id}.held"))
        .exists()
    {
        assert!(
            Instant::now() < deadline,
            "real reviewer never fetched its own dependency"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let before = h
        .daemon
        .operator_rpc("app_run_show", json!({"run_id":id}))
        .unwrap();
    assert_eq!(before["state"], "running");
    assert_eq!(before["approved_digest"], before["snapshot_digest"]);
    assert_eq!(before["artifacts"].as_array().unwrap().len(), 1);
    assert!(before["reviews"].as_array().unwrap().is_empty());
    let artifact = h.artifact(&before);
    assert_eq!(artifact["text"], format!("Context draft: {A}"));
    let reviewer = before["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["state"] == "dispatched")
        .expect("actual reviewer step dispatched");
    let emitted = h.daemon.state.join(format!(
        "context-result-emitted-{id}-{}",
        reviewer["step_id"].as_str().unwrap()
    ));
    // Counterfactual fixture: change only the persisted CAS revision, bypassing
    // CRUD's proactive cancellation. The real authenticated completion must
    // independently enforce current-binding proof inside its SQL transaction.
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    let active: i64 = db.query_row(
        "SELECT count(*) FROM app_run_steps s JOIN tasks t ON t.id=s.task_id JOIN messages m ON m.id=s.message_id WHERE s.run_id=? AND s.step_id=? AND s.state='dispatched' AND t.state='dispatched' AND t.dispatch_message=m.id AND m.alias=? AND m.state='running' AND m.turn_id IS NOT NULL AND length(m.turn_id)>0",
        rusqlite::params![id, reviewer["step_id"].as_str(), REVIEWER], |row| row.get(0)).unwrap();
    assert_eq!(
        active, 1,
        "held dependency-fetch reviewer lacks an actual running assigned turn"
    );
    assert_eq!(db.execute("UPDATE app_bindings SET revision=revision+1 WHERE id=? AND install_id=? AND revision=?",
        rusqlite::params![binding["id"].as_str(), h.install["install_id"].as_str(), binding["revision"].as_i64()]).unwrap(), 1);
    drop(db);
    assert_eq!(
        h.daemon
            .operator_rpc("app_run_show", json!({"run_id":id}))
            .unwrap(),
        before,
        "raw revision fixture unexpectedly invalidated run before completion"
    );
    std::fs::write(
        h.daemon
            .state
            .join(format!("context-reviewer-{id}.release")),
        "release",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let agent = h
            .daemon
            .operator_rpc("agent_show", json!({"alias":REVIEWER}))
            .unwrap();
        if emitted.exists()
            && matches!(agent["agent"]["state"].as_str(), Some("idle" | "attention"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "actual reviewer finish never settled: {agent}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let finished = h
        .daemon
        .operator_rpc("app_run_show", json!({"run_id":id}))
        .unwrap();
    assert_eq!(
        finished["state"], "failed",
        "stale-binding authenticated review was accepted"
    );
    assert!(
        finished["reviews"].as_array().unwrap().is_empty(),
        "stale-binding review was persisted"
    );
    assert_eq!(
        finished["artifacts"], before["artifacts"],
        "historical writer artifact was changed"
    );
    assert_eq!(finished["snapshot"], run["snapshot"]);
    assert_eq!(
        h.daemon
            .operator_rpc("app_run_artifact", json!({"artifact_id":artifact["id"]}))
            .unwrap(),
        artifact
    );
}

#[test]
fn cad692_context_free_schema3_real_review_and_explicit_local_release() {
    let h = Release::new();
    let binding=h.daemon.operator_rpc("app_binding_create",json!({"install_id":h.install["install_id"],"slot":"publication","connection_id":h.connection,"request_id":"context-free-binding"})).unwrap()["binding"].clone();
    assert!(binding["context_id"].is_null());
    let created=h.daemon.operator_rpc("app_run_create",json!({"install_id":h.install["install_id"],"workflow":"draft","inputs":{"subject":"No context needed","source":format!("CONTEXT_SOURCE={A}"),"writer":WRITER,"reviewer":REVIEWER},"request_id":"context-free-run","owner_pm":OWNER})).unwrap();
    assert_eq!(created["snapshot"]["schema"], 3);
    assert!(created["context_id"].is_null());
    assert_eq!(
        created["snapshot"].get("context"),
        Some(&Value::Null),
        "context-free schema3 snapshot must carry the explicit null context variant"
    );
    assert_eq!(
        created["snapshot"]["publication"]["binding"]["id"],
        binding["id"]
    );
    h.dispatch(&created);
    let run = h.wait_state(created["id"].as_str().unwrap(), "succeeded");
    assert_eq!(run["reviews"].as_array().unwrap().len(), 1);
    assert_eq!(h.artifact(&run)["text"], format!("Context draft: {A}"));
    assert!(h.items().as_array().unwrap().is_empty());
    let effect = h.stage(&run, "context-free-effect");
    assert_eq!(effect["state"], "waiting");
    assert!(h.items().as_array().unwrap().is_empty());
    assert_eq!(h.decide(&effect)["state"], "done");
    let detail = h
        .daemon
        .operator_rpc("platform_outbox", json!({"effect_id":effect["effect_id"]}))
        .unwrap();
    assert_eq!(
        detail["item"]["scope"]["install_id"],
        h.install["install_id"]
    );
    assert!(detail["item"]["scope"]["context_id"].is_null());
    assert_eq!(
        detail["item"]["post"],
        format!("# Reviewed draft\n\nContext draft: {A}")
    );
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    for table in ["app_contexts", "platform_grants", "platform_defaults"] {
        let count: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 0,
            "context-free release created ambient context or grant authority"
        );
    }
}

#[test]
fn cad692_actual_unbound_draft_cannot_inherit_later_publication_binding() {
    let h = Release::new();
    let created=h.daemon.operator_rpc("app_run_create",json!({"install_id":h.install["install_id"],"workflow":"draft","inputs":{"subject":"Unbound draft","source":format!("CONTEXT_SOURCE={A}"),"writer":WRITER,"reviewer":REVIEWER},"request_id":"unbound-run","owner_pm":OWNER})).unwrap();
    assert_eq!(created["snapshot"]["schema"], 3);
    assert!(created["snapshot"]["publication"]["binding"].is_null());
    h.dispatch(&created);
    let run = h.wait_state(created["id"].as_str().unwrap(), "succeeded");
    let artifact = h.artifact(&run);
    assert_eq!(artifact["text"], format!("Context draft: {A}"));
    assert_eq!(run["reviews"].as_array().unwrap().len(), 1);
    let binding=h.daemon.operator_rpc("app_binding_create",json!({"install_id":h.install["install_id"],"slot":"publication","connection_id":h.connection,"request_id":"later-binding"})).unwrap()["binding"].clone();
    assert_eq!(binding["state"], "configured");
    assert!(binding["context_id"].is_null());
    let after = h
        .daemon
        .operator_rpc("app_run_show", json!({"run_id":run["id"]}))
        .unwrap();
    assert_eq!(
        after["snapshot"], created["snapshot"],
        "configuration rewrote historical null binding"
    );
    assert_eq!(after["snapshot_digest"], created["snapshot_digest"]);
    assert_eq!(after["state"], "succeeded");
    let denied=h.daemon.operator_rpc("app_effect_stage",json!({"run_id":run["id"],"artifact_id":artifact["id"],"slot":"publication","request_id":"unbound-stage","title":"Cannot inherit"})).unwrap_err();
    assert_eq!(denied.kind(), "rejected");
    assert!(
        denied.to_string().contains("binding"),
        "unrelated refusal masked null binding proof: {denied}"
    );
    assert_eq!(h.artifact(&run), artifact);
    assert!(h.items().as_array().unwrap().is_empty());
    let db = rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3")).unwrap();
    let effects: i64 = db
        .query_row("SELECT count(*) FROM platform_effects", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        effects, 0,
        "unbound historical run staged inherited authority"
    );
    let fresh=h.daemon.operator_rpc("app_run_create",json!({"install_id":h.install["install_id"],"workflow":"draft","inputs":{"subject":"New approval","source":format!("CONTEXT_SOURCE={B}"),"writer":WRITER,"reviewer":REVIEWER},"request_id":"bound-new-run","owner_pm":OWNER})).unwrap();
    assert_eq!(
        fresh["snapshot"]["publication"]["binding"]["id"], binding["id"],
        "new run does not visibly pin now-configured binding"
    );
    assert_eq!(
        fresh["state"], "awaiting_approval",
        "binding silently approved new execution"
    );
}

fn release_needs_you(daemon: &common::TestDaemon, effect: &Value) -> bool {
    let db = rusqlite::Connection::open(daemon.state.join("cadence.sqlite3")).unwrap();
    db.query_row(
        "SELECT needs_you FROM platform_effects WHERE effect_id=?",
        [effect["effect_id"].as_str().unwrap()],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        != 0
}
fn resolve_release(
    daemon: &common::TestDaemon,
    effect: &Value,
    resolution: &str,
) -> cadence_agent::Result<Value> {
    daemon.operator_rpc(
        "app_effect_resolve",
        json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"resolution":resolution}),
    )
}
fn show_release(daemon: &common::TestDaemon, effect: &Value) -> Value {
    daemon
        .operator_rpc("app_effect_show", json!({"effect_id":effect["effect_id"]}))
        .unwrap()["effect"]
        .clone()
}
fn assert_release_evidence_unchanged(before: &Value, after: &Value) {
    for key in ["effect_id", "request", "digest", "authority"] {
        assert_eq!(before[key], after[key], "resolution changed {key}");
    }
    for key in ["input", "preview", "decision", "outcome", "source_hash"] {
        assert_eq!(
            before["record"][key], after["record"][key],
            "resolution changed record {key}"
        );
    }
}

#[test]
fn cad692_actual_local_commit_uncertainty_reconciles_and_closes_without_replay() {
    use common::app_release::fault::{wrap, Fault};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let h = Release::with_options(move |opts, _| wrap(opts, Fault::UncertainAfterCommit, observed));
    let context = h.context("Actual uncertain commit", A, "uncertain-commit-context");
    let binding = h.bind(&context, "uncertain-commit-binding");
    let run = h.complete(&context, "uncertain-commit-run");
    let artifact = h.artifact(&run);
    assert_eq!(run["reviews"].as_array().unwrap().len(), 1);
    let staged = h.stage(&run, "uncertain-commit-effect");
    assert!(h.items().as_array().unwrap().is_empty());
    let uncertain = h.decide(&staged);
    assert_eq!(
        uncertain["state"], "reconcile",
        "confirmation uncertainty became terminal failure"
    );
    assert!(release_needs_you(&h.daemon, &uncertain));
    assert_eq!(uncertain["record"]["outcome"]["kind"], "uncertain");
    assert_eq!(
        uncertain["record"]["outcome"]["verified"], true,
        "actual Local read-back did not find committed bytes"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let items = h.items();
    assert_eq!(items.as_array().unwrap().len(), 1);
    let item = h
        .daemon
        .operator_rpc("platform_outbox", json!({"effect_id":staged["effect_id"]}))
        .unwrap();
    assert_eq!(
        item["item"]["post"],
        format!("# Reviewed draft\n\nContext draft: {A}")
    );
    assert_eq!(item["item"]["provenance"]["artifact_id"], artifact["id"]);
    assert_eq!(item["item"]["provenance"]["binding_id"], binding["id"]);
    assert!(h
        .daemon
        .operator_rpc(
            "app_effect_decide",
            json!({"effect_id":staged["effect_id"],"digest":staged["digest"],"decision":"accept"})
        )
        .is_err());
    assert_eq!(h.items(), items);
    let Release {
        root,
        daemon,
        install,
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
    wrap(&mut opts, Fault::UncertainAfterCommit, calls.clone());
    let restarted = common::TestDaemon::start_on_opts(state, opts);
    let restored = show_release(&restarted, &staged);
    assert_eq!(restored, uncertain, "restart rewrote uncertain receipt");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "restart automatically sent again"
    );
    assert!(restarted
        .operator_rpc(
            "app_effect_decide",
            json!({"effect_id":staged["effect_id"],"digest":staged["digest"],"decision":"accept"})
        )
        .is_err());
    restarted.operator_rpc("app_context_archive", json!({"install_id":install["install_id"],"context_id":context["id"],"expected_revision":context["revision"]})).unwrap();
    let bundle = root
        .path()
        .join("pm/.apps/installations")
        .join(install["install_id"].as_str().unwrap())
        .join("bundle");
    assert!(bundle.join("app.md").is_file());
    std::fs::remove_dir_all(bundle).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let (first, second) = std::thread::scope(|scope| {
        let one = barrier.clone();
        let two = barrier.clone();
        let daemon = &restarted;
        let effect = &staged;
        let first = scope.spawn(move || {
            one.wait();
            resolve_release(daemon, effect, "close")
        });
        let second = scope.spawn(move || {
            two.wait();
            resolve_release(daemon, effect, "close")
        });
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_eq!(
        usize::from(first.is_ok()) + usize::from(second.is_ok()),
        1,
        "resolution state CAS permitted duplicate close"
    );
    let closed = show_release(&restarted, &staged);
    assert_eq!(closed["state"], "closed");
    assert_eq!(closed["record"]["close_reason"], "operator_reconciled");
    assert!(!release_needs_you(&restarted, &closed));
    assert_release_evidence_unchanged(&uncertain, &closed);
    assert!(resolve_release(&restarted, &staged, "close").is_err());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "resolution executed provider hook"
    );
    assert_eq!(
        restarted
            .operator_rpc("platform_outbox", json!({}))
            .unwrap()["items"],
        items
    );
}

#[test]
fn cad692_populated_resolution_native_setsid_forged_digest_and_state_guards() {
    use common::app_release::fault::{wrap, Fault};
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let h = Release::with_options(move |opts, _| wrap(opts, Fault::UncertainAfterCommit, calls));
    let context = h.context("Private resolution", A, "resolution-context");
    h.bind(&context, "resolution-binding");
    let run = h.complete(&context, "resolution-run");
    let staged = h.stage(&run, "resolution-effect");
    assert!(
        resolve_release(&h.daemon, &staged, "close").is_err(),
        "waiting effect closed through reconciliation"
    );
    assert!(
        resolve_release(&h.daemon, &staged, "acknowledge").is_err(),
        "waiting effect acknowledged"
    );
    assert_eq!(show_release(&h.daemon, &staged), staged);
    let uncertain = h.decide(&staged);
    assert_eq!(uncertain["state"], "reconcile");
    let items = h.items();
    let mut lane = LaneShell::spawn(h.root.path());
    plant_member_pane(
        &h.daemon,
        "release-resolution-worker",
        "claude",
        None,
        lane.pid(),
    );
    let mut failures = Vec::new();
    for detached in [false, true] {
        for resolution in ["close", "acknowledge"] {
            for forged in [false, true] {
                let mut params = json!({"effect_id":staged["effect_id"],"digest":staged["digest"],"resolution":resolution});
                if forged {
                    params["operator"] = json!(true);
                    params["agent"] = json!(OWNER);
                }
                let reply = native(
                    &mut lane,
                    &h.daemon.state,
                    detached,
                    "app_effect_resolve",
                    params,
                );
                if reply["ok"] != false
                    || reply["error"]["kind"] != "rejected"
                    || reply.get("result").is_some()
                {
                    failures.push(format!(
                        "detached={detached} forged={forged} resolution={resolution}: {reply}"
                    ));
                }
                assert!(
                    !reply.to_string().contains(A),
                    "resolution refusal disclosed private material"
                );
            }
        }
    }
    assert!(
        failures.is_empty(),
        "native app resolution operator guard failed: {failures:?}"
    );
    for params in [
        json!({"effect_id":staged["effect_id"],"digest":"sha256:wrong","resolution":"close"}),
        json!({"effect_id":staged["effect_id"],"digest":staged["digest"],"resolution":"acknowledge"}),
        json!({"effect_id":staged["effect_id"],"digest":staged["digest"],"resolution":"accept"}),
        json!({"effect_id":staged["effect_id"],"digest":staged["digest"],"resolution":"close","body":"forged"}),
        json!({"effect_id":staged["effect_id"],"digest":staged["digest"],"resolution":null}),
    ] {
        assert!(h.daemon.operator_rpc("app_effect_resolve", params).is_err());
        assert_eq!(
            show_release(&h.daemon, &staged),
            uncertain,
            "refused resolution changed receipt"
        );
        assert!(release_needs_you(&h.daemon, &uncertain));
        assert_eq!(
            h.items(),
            items,
            "refused resolution wrote outward material"
        );
    }
    assert_eq!(
        resolve_release(&h.daemon, &staged, "close").unwrap()["effect"]["state"],
        "closed"
    );
    assert_eq!(h.items(), items);
}

#[test]
fn cad692_known_refusal_acknowledges_flag_without_rewriting_terminal_evidence() {
    use common::app_release::fault::{wrap, Fault};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let h = Release::with_options(move |opts, _| wrap(opts, Fault::RefusedBeforeCommit, observed));
    let context = h.context("Refused release", A, "refused-context");
    h.bind(&context, "refused-binding");
    let run = h.complete(&context, "refused-run");
    let effect = h.stage(&run, "refused-effect");
    let failed = h.decide(&effect);
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["record"]["outcome"]["kind"], "refused");
    assert_eq!(failed["record"]["outcome"]["verified"], false);
    assert!(release_needs_you(&h.daemon, &failed));
    assert!(h.items().as_array().unwrap().is_empty());
    assert!(resolve_release(&h.daemon, &effect, "close").is_err());
    let mut lane = LaneShell::spawn(h.root.path());
    plant_member_pane(&h.daemon, "release-ack-worker", "claude", None, lane.pid());
    let mut failures = Vec::new();
    for detached in [false, true] {
        for forged in [false, true] {
            let mut params = json!({"effect_id":effect["effect_id"],"digest":effect["digest"],"resolution":"acknowledge"});
            if forged {
                params["operator"] = json!(true);
                params["agent"] = json!(OWNER);
            }
            let reply = native(
                &mut lane,
                &h.daemon.state,
                detached,
                "app_effect_resolve",
                params,
            );
            if reply["ok"] != false
                || reply["error"]["kind"] != "rejected"
                || reply.get("result").is_some()
            {
                failures.push(format!("detached={detached} forged={forged}: {reply}"));
            }
            assert!(!reply.to_string().contains(A));
        }
    }
    assert!(
        failures.is_empty(),
        "native terminal acknowledge operator guard failed: {failures:?}"
    );
    assert!(release_needs_you(&h.daemon, &failed));
    let mut wrong = effect.clone();
    wrong["digest"] = json!("sha256:wrong");
    assert!(resolve_release(&h.daemon, &wrong, "acknowledge").is_err());
    assert_eq!(show_release(&h.daemon, &effect), failed);
    let acknowledged =
        resolve_release(&h.daemon, &effect, "acknowledge").unwrap()["effect"].clone();
    assert_eq!(acknowledged["state"], "failed");
    assert!(!release_needs_you(&h.daemon, &acknowledged));
    assert_release_evidence_unchanged(&failed, &acknowledged);
    assert!(resolve_release(&h.daemon, &effect, "acknowledge").is_err());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "acknowledge called provider again"
    );
    assert!(h.items().as_array().unwrap().is_empty());
}

#[test]
fn cad692_legacy_adapter_without_typed_app_hook_refuses_without_legacy_execute() {
    use common::app_release::fault::{wrap, Fault};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let h = Release::with_options(move |opts, _| wrap(opts, Fault::Unsupported, observed));
    let context = h.context("Unsupported hook", A, "unsupported-context");
    h.bind(&context, "unsupported-binding");
    let run = h.complete(&context, "unsupported-run");
    let effect = h.stage(&run, "unsupported-effect");
    let failed = h.decide(&effect);
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["record"]["outcome"]["kind"], "refused");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "typed unsupported fallback called legacy execute"
    );
    assert!(h.items().as_array().unwrap().is_empty());
}
