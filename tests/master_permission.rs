//! CAD-615: master permission requests. The caller gate lives on the
//! daemon (and the board relay); the record and the never-list are
//! unit-tested in `src/master_perm.rs`. These prove a wrong caller
//! cannot file or decide, a single-use grant is exact, a planted
//! grant cannot cover the never-list, and HTTP is as strict as RPC.
//!
//! The whole binary is seam-only. Without the feature the imports and
//! `refused` would be unused, and default clippy denies that.

#![cfg(feature = "test-seam")]
#![allow(clippy::disallowed_methods)]
#![allow(clippy::duplicate_mod)]
mod board_common;
mod common;

use board_common::op;
use common::{test_env, PlanFixture, TestDaemon};
use serde_json::json;

fn refused(r: cadence_agent::Result<serde_json::Value>) -> String {
    match r {
        Ok(v) => panic!("call succeeded where a refusal was required: {v}"),
        Err(e) => e.to_string(),
    }
}

/// An agent cannot approve, the operator cannot file, a non-master
/// cannot ask, and a forged request id is not a grant.
#[cfg(feature = "test-seam")]
#[test]
fn role_gates_refuse_the_wrong_caller() {
    let d = TestDaemon::start();
    let cwd = d.dir.path().to_string_lossy().to_string();
    let ask = json!({
        "argv": ["cadence", "issue", "new", "--project", "cadence", "hello"],
        "cwd": cwd,
        "reason": "need a ticket",
    });
    let agent = refused(d.agent_rpc("worker", "master_ask_permission", ask.clone()));
    assert!(
        agent.contains("master") || agent.contains("refused"),
        "{agent}"
    );
    let op_ask = refused(d.operator_rpc("master_ask_permission", ask));
    assert!(op_ask.contains("master"), "{op_ask}");
    let forged = refused(d.agent_rpc(
        "worker",
        "master_permission_allow_once",
        json!({"id": "no-such"}),
    ));
    assert!(
        forged.contains("operator") || forged.contains("refused"),
        "{forged}"
    );
    let missing = refused(d.operator_rpc(
        "master_permission_allow_once",
        json!({"id": "forged-request-id"}),
    ));
    assert!(missing.contains("no permission request"), "{missing}");
    let listed = d
        .operator_rpc("master_permission_list", json!({}))
        .expect("operator lists");
    assert_eq!(listed["requests"].as_array().map(Vec::len), Some(0));
}

/// The master files `ls` of a checkout; allow-once runs it once; a
/// second use, a changed argv, and a planted never-list grant all fail.
#[cfg(feature = "test-seam")]
#[test]
fn master_allow_once_is_exact_and_single_use() {
    let f = PlanFixture::start();
    test_env().set(cadence_agent::master::TEST_NO_LANDLOCK, "1");
    let _pi = f.d.mock_pi("normal");
    f.d.operator_rpc(
        "master_start",
        json!({"provider": "pi", "unconfined": true}),
    )
    .expect("master starts");

    let repo = f
        .pm_dir
        .parent()
        .unwrap()
        .join("repo")
        .canonicalize()
        .unwrap();
    let file = repo.join("f");
    let argv = vec!["ls".to_string(), file.to_string_lossy().to_string()];
    let cwd = repo.to_string_lossy().to_string();
    let filed =
        f.d.agent_rpc(
            "master",
            "master_ask_permission",
            json!({"argv": argv, "cwd": cwd, "reason": "read a project file"}),
        )
        .expect("master files");
    let id = filed["id"].as_str().unwrap();
    f.d.operator_rpc("master_permission_allow_once", json!({"id": id}))
        .expect("operator allows once");

    let used =
        f.d.agent_rpc(
            "master",
            "master_permission_use",
            json!({"argv": argv, "cwd": cwd}),
        )
        .expect("use");
    assert_eq!(used["applied"], json!(true), "{used}");
    assert!(
        used["stdout"].as_str().unwrap_or("").contains("f"),
        "{used}"
    );

    let again =
        f.d.agent_rpc(
            "master",
            "master_permission_use",
            json!({"argv": argv, "cwd": cwd}),
        )
        .expect("second use answers");
    assert_eq!(again["applied"], json!(false), "{again}");

    let other_file = repo.join("g");
    std::fs::write(&other_file, "y").unwrap();
    let other = vec!["ls".to_string(), other_file.to_string_lossy().to_string()];
    let moved =
        f.d.agent_rpc(
            "master",
            "master_permission_use",
            json!({"argv": other, "cwd": cwd}),
        )
        .expect("changed argv answers");
    assert_eq!(moved["applied"], json!(false), "{moved}");

    let never = refused(f.d.agent_rpc(
        "master",
        "master_ask_permission",
        json!({
            "argv": ["cadence", "secret", "show"],
            "cwd": cwd,
            "reason": "please",
        }),
    ));
    assert!(never.contains("never requestable"), "{never}");

    let planted = serde_json::json!({
        "requests": [],
        "grants": [{
            "id": "g-planted",
            "request_id": "r-planted",
            "argv": ["cadence", "merge", "1"],
            "cwd": cwd,
            "uses_left": 1,
            "expires_at": 9_999_999_999_i64,
        }],
    });
    std::fs::write(
        f.d.state.join("master-permissions.json"),
        serde_json::to_string(&planted).unwrap(),
    )
    .unwrap();
    let covered = refused(f.d.agent_rpc(
        "master",
        "master_permission_use",
        json!({"argv": ["cadence", "merge", "1"], "cwd": cwd}),
    ));
    assert!(
        covered.contains("never requestable") || covered.contains("cannot allow"),
        "{covered}"
    );
}

/// Eight concurrent `master_permission_use` calls — the path the Pi
/// guard takes for `ls`/`cat`/`grep`/`find` — consume the grant once.
#[cfg(feature = "test-seam")]
#[test]
fn concurrent_permission_use_succeeds_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;

    let f = PlanFixture::start();
    test_env().set(cadence_agent::master::TEST_NO_LANDLOCK, "1");
    let _pi = f.d.mock_pi("normal");
    f.d.operator_rpc(
        "master_start",
        json!({"provider": "pi", "unconfined": true}),
    )
    .expect("master starts");

    let repo = f
        .pm_dir
        .parent()
        .unwrap()
        .join("repo")
        .canonicalize()
        .unwrap();
    let file = repo.join("f");
    let argv = vec!["ls".to_string(), file.to_string_lossy().to_string()];
    let cwd = repo.to_string_lossy().to_string();
    let filed =
        f.d.agent_rpc(
            "master",
            "master_ask_permission",
            json!({"argv": argv, "cwd": cwd, "reason": "read a project file"}),
        )
        .expect("master files");
    let id = filed["id"].as_str().unwrap();
    f.d.operator_rpc("master_permission_allow_once", json!({"id": id}))
        .expect("operator allows once");

    let state = f.d.state.clone();
    let params = json!({"argv": argv, "cwd": cwd});
    let wins = Arc::new(AtomicUsize::new(0));
    let mut threads = Vec::new();
    for _ in 0..8 {
        let state = state.clone();
        let params = params.clone();
        let wins = Arc::clone(&wins);
        threads.push(thread::spawn(move || {
            let out = cadence_agent::test_seam::scoped(
                cadence_agent::test_seam::Asserted::Agent("master".to_string()),
                || cadence_agent::client::rpc(&state, "master_permission_use", params),
            );
            if out.ok().and_then(|v| v["applied"].as_bool()) == Some(true) {
                wins.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(wins.load(Ordering::SeqCst), 1);
}

/// The board's permission routes are operator-only. An unsigned write
/// and an agent write are refused; the operator's forged id is the
/// daemon's refusal, not a grant.
#[cfg(feature = "test-seam")]
#[test]
fn http_permission_routes_are_operator_only() {
    let f = PlanFixture::start();
    let (port, _board) = board_common::start_ui(f.pm_dir.clone(), f.d.state.clone());
    let host = op::board_host(port);
    let path = "/api/master/permissions/forged-request-id/allow-once";

    let unsigned = op::request(
        "POST",
        path,
        &host,
        Some(&format!("http://{host}")),
        None,
        "{}",
    );
    let (status, _, body) = op::raw(port, &unsigned);
    assert!(status == 401 || status == 403, "unsigned {status} {body}");

    let agent = op::assert_as(
        op::request(
            "POST",
            path,
            &host,
            Some(&format!("http://{host}")),
            None,
            "{}",
        ),
        &f.d.state,
        "agent:worker",
    );
    let (status, _, body) = op::raw(port, &agent);
    assert!(status == 401 || status == 403, "agent {status} {body}");

    let session = board_common::sign_in(&f.d.state, port);
    let (status, _, body) = board_common::op_write_json(&session, port, "POST", path, &host, "{}");
    assert!(status >= 400, "forged id {status} {body}");
    assert!(body.contains("no permission request"), "{body}");

    let listed = op::raw(port, &session.request("GET", "/api/master/permissions", ""));
    assert_eq!(listed.0, 200, "{} {}", listed.0, listed.2);

    // The board relays over its own daemon connection. A request that
    // asserts the operator seam but holds no session must still be
    // refused here — otherwise any client of an operator board receives
    // the pending argv.
    let relayed = op::assert_as(
        op::request(
            "GET",
            "/api/master/permissions",
            &host,
            Some(&format!("http://{host}")),
            None,
            "",
        ),
        &f.d.state,
        "operator",
    );
    let (status, _, body) = op::raw(port, &relayed);
    assert!(
        status == 401 || status == 403,
        "operator seam without a session {status} {body}"
    );

    let agent_get = op::assert_as(
        op::request(
            "GET",
            "/api/master/permissions",
            &host,
            Some(&format!("http://{host}")),
            None,
            "",
        ),
        &f.d.state,
        "agent:worker",
    );
    let (status, _, body) = op::raw(port, &agent_get);
    assert!(status == 401 || status == 403, "agent get {status} {body}");
}

/// CAD-1076: a pi master over a gateway-faithful fake (`gateway-cap`).
fn gateway_master(mode: &str) -> (PlanFixture, common::MockPi) {
    let f = PlanFixture::start();
    test_env().set(cadence_agent::master::TEST_NO_LANDLOCK, "1");
    let pi = f.d.mock_pi(mode);
    f.d.operator_rpc(
        "master_start",
        json!({"provider": "pi", "unconfined": true}),
    )
    .expect("master starts");
    (f, pi)
}

/// The master's settled message `id`, after it reached a final state.
fn settled(d: &TestDaemon, id: &str) -> serde_json::Value {
    d.wait_message("master", id, &["completed", "failed", "unknown"], 60)
}

/// `new_session` requests fake-pi received for the master.
fn new_sessions(d: &TestDaemon) -> usize {
    let journal = d.state.join("master/cwd/pi-rpc.jsonl");
    std::fs::read_to_string(journal)
        .unwrap_or_default()
        .matches("\"new_session\"")
        .count()
}

/// CAD-1076, the Demo sequence: an approved command that prints
/// nothing, its injected "Ran approved command" notice, a turn whose
/// tool output is empty, then turns until the provider refuses the
/// session's history (the gateway's 400). Every turn completes: the
/// refusal resets the provider session, the retry carries a
/// continuity pack, and the notice names the empty output.
#[cfg(feature = "test-seam")]
#[test]
fn empty_approved_output_then_more_turns_all_complete() {
    let (f, _pi) = gateway_master("gateway-cap");
    let d = &f.d;
    d.wait_agent("master", "idle", 30);
    let repo = f
        .pm_dir
        .parent()
        .unwrap()
        .join("repo")
        .canonicalize()
        .unwrap();
    let empty = repo.join("empty");
    std::fs::create_dir(&empty).unwrap();
    let argv = vec!["ls".to_string(), empty.to_string_lossy().to_string()];
    let cwd = repo.to_string_lossy().to_string();
    let filed = d
        .agent_rpc(
            "master",
            "master_ask_permission",
            json!({"argv": argv, "cwd": cwd, "reason": "list"}),
        )
        .expect("master files");
    d.operator_rpc(
        "master_permission_allow_once",
        json!({"id": filed["id"].as_str().unwrap()}),
    )
    .expect("operator allows once");
    let used = d
        .agent_rpc(
            "master",
            "master_permission_use",
            json!({"argv": argv, "cwd": cwd}),
        )
        .expect("use");
    assert_eq!(used["applied"], json!(true), "{used}");
    assert_eq!(used["stdout"], json!(""), "{used}");
    let messages = d.rpc("agent_show", json!({"alias": "master"})).unwrap()["messages"].clone();
    let notice = messages
        .as_array()
        .unwrap()
        .iter()
        .find(|m| {
            m["body"]
                .as_str()
                .unwrap_or("")
                .contains("Ran approved command")
        })
        .unwrap_or_else(|| panic!("no injected notice: {messages}"))
        .clone();
    assert!(
        notice["body"]
            .as_str()
            .unwrap()
            .contains("exit 0, no output"),
        "{notice}"
    );
    let notice = settled(d, notice["id"].as_str().unwrap());
    assert_eq!(notice["state"], "completed", "{notice}");
    for i in 0..8 {
        let id = format!("m{i}");
        let text = if i == 0 {
            "run empty tool".to_string()
        } else {
            format!("turn {i}")
        };
        d.operator_send("master", json!({"text": text, "message": id}))
            .unwrap();
        let m = settled(d, &id);
        assert_eq!(m["state"], "completed", "{id}: {m}");
    }
    let resets: Vec<_> = d
        .events("master")
        .into_iter()
        .filter(|e| e["kind"] == "provider_session_reset")
        .collect();
    assert!(!resets.is_empty(), "the cap was crossed without a reset");
    assert_eq!(resets.len(), new_sessions(d), "{resets:?}");
    let packs = d
        .events("master")
        .into_iter()
        .filter(|e| {
            e["kind"] == cadence_agent::continuity::PACK_EVENT && e["payload"]["reason"] == "new"
        })
        .count();
    assert!(packs >= resets.len(), "every reset carries a pack: {packs}");
}

/// CAD-1076: a history no new session repairs is retried exactly
/// once per message, then the message fails with the provider's
/// error — never a silent loop.
#[cfg(feature = "test-seam")]
#[test]
fn a_rejected_history_is_retried_once_then_fails() {
    let (f, _pi) = gateway_master("gateway-always");
    let d = &f.d;
    d.wait_agent("master", "idle", 30);
    let before = new_sessions(d);
    d.operator_send("master", json!({"text": "hello", "message": "h1"}))
        .unwrap();
    let m = settled(d, "h1");
    assert_eq!(m["state"], "failed", "{m}");
    assert!(m.to_string().contains("invalid_request"), "{m}");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(new_sessions(d), before + 1, "one reset for h1");
    let resets = d
        .events("master")
        .into_iter()
        .filter(|e| e["kind"] == "provider_session_reset" && e["payload"]["message"] == "h1")
        .count();
    assert_eq!(resets, 1);
}
