//! CAD-2215 independent signed-board master state/snapshot acceptance.
//! The product implementer must not edit or weaken this check.
//! Actual assistant ownership is ledger-backed; the issue/worker transition
//! below is synthetic, not an API or local-agent isolation proof.

use super::*;
use std::io::{BufRead, BufReader};

#[path = "cad2215_master_legacy_http_acceptance.rs"]
mod cad2215_master_legacy_http_acceptance;

#[path = "cad2215_master_boundary_http_acceptance.rs"]
mod cad2215_master_boundary_http_acceptance;

fn initial_master(rig: &Rig, cookie: &str, who: &str) -> Value {
    let mut socket = TcpStream::connect(("127.0.0.1", rig.board_port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(25)))
        .unwrap();
    write!(socket, "GET /api/threads/master/stream HTTP/1.1\r\nHost: {}\r\nCookie: {}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n", rig.public_host, cookie).unwrap();
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(
        line.starts_with("HTTP/1.1 200 "),
        "{who} master stream refused: {line}"
    );
    loop {
        line.clear();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "{who} stream closed in headers"
        );
        if line == "\r\n" || line == "\n" {
            break;
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    let mut event = String::new();
    let mut data = String::new();
    loop {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|d| !d.is_zero())
            .unwrap_or_else(|| panic!("{who} master snapshot deadline expired"));
        reader.get_ref().set_read_timeout(Some(remaining)).unwrap();
        line.clear();
        let n = reader
            .read_line(&mut line)
            .unwrap_or_else(|e| panic!("{who} master snapshot failed: {e}"));
        assert!(n > 0, "{who} stream closed before master snapshot");
        if line == "\n" || line == "\r\n" {
            if event == "master" {
                return serde_json::from_str(&data).expect("master SSE data not JSON");
            }
            event.clear();
            data.clear();
        } else if let Some(value) = line.strip_prefix("event: ") {
            event = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("data: ") {
            data.push_str(value.trim());
        }
    }
}

fn routing(state: &Value) -> Value {
    json!({"turn":state.get("turn"), "queue":state.get("queue"), "queued":state.get("queued")})
}

#[test]
fn hidden_assistant_job_cannot_change_another_members_master_state_or_initial_snapshot() {
    let _flag = AssistantFlagOn::arm();
    let (rig, events) = queue_rig(&["ana", "ben"]);
    seed(&rig);
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
    let home = shared.store.ensure_thread("master").unwrap();
    let ben = login(&rig, "ben", "member");
    let operator = login(&rig, "cad2215-master-state-owner", "owner");
    let visible = get(&rig, &ben, "/api/issues/VIS-1");
    assert!(
        visible.to_string().contains("VIS-1"),
        "member lost authorized ordinary issue"
    );
    let before = get(&rig, &ben, "/api/master/state");
    let before_sse = initial_master(&rig, &ben, "member baseline");

    // Actual signed request, real owner/subscriber ledger. The subsequent
    // same-readable-project issue rebind and task assignment are synthetic
    // fixture transitions, not supported APIs or isolation proofs.
    let env = rig.envelope("ana", json!({}), json!({}));
    let (status, requested) = rig.call("job_request", &env, &json!({
        "title":"CAD2215_MASTER_PRIVATE_JOB", "request_key":"cad2215-master-state", "issue":"VIS-9"
    }));
    assert_eq!(status, 200, "real owner request failed: {requested}");
    let job = promoted_job(&rig, &events, "ana", &requested);
    assert!(shared.store.assist_request_owns(ORG, "ana", &job).unwrap());
    assert!(!shared.store.assist_request_owns(ORG, "ben", &job).unwrap());
    assert!(!shared.store.job_is_subscriber(&job, ORG, "ben").unwrap());
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id='VIS-1' WHERE id=?1", [&job])
            .unwrap(),
        1
    );
    let task: String = db
        .query_row(
            "SELECT id FROM tasks WHERE job_id=?1 ORDER BY id LIMIT 1",
            [&job],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        db.execute(
            "UPDATE tasks SET assignee='master', state='running' WHERE id=?1",
            [&task]
        )
        .unwrap(),
        1
    );
    for (id, body) in [
        ("cad2215-master-private-head", "CAD2215_MASTER_PRIVATE_HEAD"),
        (
            "cad2215-master-private-second",
            "CAD2215_MASTER_PRIVATE_SECOND",
        ),
    ] {
        shared
            .store
            .enqueue_task("master", body, None, id, "user", Some(&task))
            .unwrap();
    }
    let owner_state = get(&rig, &operator, "/api/master/state");
    let owner_sse = initial_master(&rig, &operator, "operator control");
    for (which, state) in [("HTTP", &owner_state), ("SSE", &owner_sse)] {
        let text = state.to_string();
        assert!(
            text.contains("cad2215-master-private-head"),
            "operator {which} did not observe private head"
        );
        assert!(
            text.contains("cad2215-master-private-second"),
            "operator {which} did not observe private queue"
        );
        assert!(
            text.contains(&home.id),
            "operator {which} did not observe conversation identity"
        );
        assert!(
            state["queued"].as_u64().is_some_and(|n| n >= 2),
            "operator {which} did not observe queue count"
        );
        assert!(
            !state["turn"]["since"].is_null(),
            "operator {which} did not observe turn timing"
        );
    }
    let after = get(&rig, &ben, "/api/master/state");
    let after_sse = initial_master(&rig, &ben, "member after private work");
    assert_eq!(
        routing(&after),
        routing(&before),
        "hidden-only work changed member HTTP turn/queue/count metadata"
    );
    assert_eq!(
        routing(&after_sse),
        routing(&before_sse),
        "hidden-only work changed initial SSE turn/queue/count metadata"
    );
    for (which, state) in [("HTTP", &after), ("SSE", &after_sse)] {
        let text = state.to_string();
        for secret in [
            job.as_str(),
            task.as_str(),
            home.id.as_str(),
            "cad2215-master-private-head",
            "cad2215-master-private-second",
            "CAD2215_MASTER_PRIVATE_HEAD",
            "CAD2215_MASTER_PRIVATE_SECOND",
        ] {
            assert!(
                !text.contains(secret),
                "member {which} exposed hidden master metadata {secret}"
            );
        }
    }
    let visible_after = get(&rig, &ben, "/api/issues/VIS-1");
    assert!(
        visible_after.to_string().contains("VIS-1"),
        "hidden work made authorized ordinary issue unavailable"
    );
}
