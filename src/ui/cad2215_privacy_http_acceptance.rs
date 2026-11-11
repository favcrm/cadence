//! CAD-2215 independent signed-board privacy acceptance.
//! The product implementer must not edit or weaken this test.
//! Local fixture authority only: this is not process/filesystem isolation.

use super::*;

#[path = "cad2215_thread_legacy_http_acceptance.rs"]
mod cad2215_thread_legacy_http_acceptance;

#[path = "cad2215_master_state_privacy_http_acceptance.rs"]
mod cad2215_master_state_privacy_http_acceptance;

#[path = "cad2215_assist_job_mutation_http_acceptance.rs"]
mod cad2215_assist_job_mutation_http_acceptance;
#[path = "cad2215_job_request_issue_http_acceptance.rs"]
mod cad2215_job_request_issue_http_acceptance;
#[path = "cad2215_scoped_stream_http_acceptance.rs"]
mod cad2215_scoped_stream_http_acceptance;
#[path = "cad2215_wiki_fts_isolation_http_acceptance.rs"]
mod cad2215_wiki_fts_isolation_http_acceptance;
#[path = "cad2215_wiki_memory_http_acceptance.rs"]
mod cad2215_wiki_memory_http_acceptance;

#[path = "cad2215_milestone_privacy_http_acceptance.rs"]
mod cad2215_milestone_privacy_http_acceptance;

#[path = "cad2215_assist_job_binding_http_acceptance.rs"]
mod cad2215_assist_job_binding_http_acceptance;
#[path = "cad2215_identity_collision_http_acceptance.rs"]
mod cad2215_identity_collision_http_acceptance;
#[path = "cad2215_master_thread_http_acceptance.rs"]
mod cad2215_master_thread_http_acceptance;

#[path = "cad2215_anonymous_privacy_http_acceptance.rs"]
mod cad2215_anonymous_privacy_http_acceptance;
#[path = "cad2215_cli_privacy_http_acceptance.rs"]
mod cad2215_cli_privacy_http_acceptance;
#[path = "cad2215_stream_privacy_http_acceptance.rs"]
mod cad2215_stream_privacy_http_acceptance;
#[path = "cad2215_thread_privacy_http_acceptance.rs"]
mod cad2215_thread_privacy_http_acceptance;

#[path = "cad2215_agent_legacy_http_acceptance.rs"]
mod cad2215_agent_legacy_http_acceptance;

#[path = "cad2215_agent_detail_revoke_http_acceptance.rs"]
mod cad2215_agent_detail_revoke_http_acceptance;

#[path = "cad2215_member_request_queue_http_acceptance.rs"]
mod cad2215_member_request_queue_http_acceptance;

#[path = "cad2215_app_conversation_grant_http_acceptance.rs"]
mod cad2215_app_conversation_grant_http_acceptance;

#[path = "cad2215_member_private_delivery_http_acceptance.rs"]
mod cad2215_member_private_delivery_http_acceptance;

#[path = "cad2215_member_monitor_control_http_acceptance.rs"]
mod cad2215_member_monitor_control_http_acceptance;

fn sso(rig: &Rig, sub: &str, role: &str) -> String {
    let now = crate::issue::time::now_epoch();
    let header = json!({"alg": "EdDSA", "typ": "JWT", "kid": KID});
    let claims = json!({
        "iss": rig.issuer,
        "aud": rig.public_host,
        "sub": sub,
        "email": format!("{sub}@cad2215.example.test"),
        "name": sub,
        "company": ORG,
        "role": role,
        "iat": now,
        "exp": now + 60,
        "jti": format!("cad2215-sso-{}", uuid::Uuid::new_v4()),
    });
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()),
    );
    format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(rig.key.sign(signed.as_bytes()).as_ref()),
    )
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .build(),
    )
}

fn login(rig: &Rig, sub: &str, role: &str) -> String {
    let url = format!("http://127.0.0.1:{}/__platform/session", rig.board_port);
    let response = http_agent()
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1")
        .send_json(json!({"assertion": sso(rig, sub, role)}))
        .expect("board session exchange failed");
    assert_eq!(
        response.status().as_u16(),
        200,
        "signed {role} login refused"
    );
    response
        .headers()
        .get("set-cookie")
        .expect("session exchange did not set a cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

fn get(rig: &Rig, cookie: &str, path: &str) -> Value {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let mut response = http_agent()
        .get(&url)
        .header("Host", &rig.public_host)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("Cookie", cookie)
        .call()
        .expect("board GET failed");
    assert_eq!(response.status().as_u16(), 200, "board GET {path} failed");
    response
        .body_mut()
        .read_json()
        .expect("board response is not JSON")
}

// Observe the actual daemon worker, not a test-triggered sweep or guessed ID.
// Subscribe before submitting so a fast autonomous promotion cannot be lost.
fn queue_observer() -> (
    Arc<dyn Fn(&str, &str) + Send + Sync>,
    std::sync::mpsc::Receiver<(String, String)>,
) {
    let (tx, rx) = std::sync::mpsc::channel::<(String, String)>();
    let observer = Arc::new(move |id: &str, outcome: &str| {
        let _ = tx.send((id.to_string(), outcome.to_string()));
    });
    (observer, rx)
}

fn queue_rig(subs: &[&str]) -> (Rig, std::sync::mpsc::Receiver<(String, String)>) {
    let (observer, events) = queue_observer();
    let rig = start_with_queue_service(subs, Some(observer), false);
    // Real daemon custody and credential record; the fixture platform
    // independently checks the member's current authority and provenance.
    enroll_queue_service(&rig);
    (rig, events)
}

fn promoted_job(
    rig: &Rig,
    events: &std::sync::mpsc::Receiver<(String, String)>,
    sub: &str,
    receipt: &Value,
) -> String {
    let fields = receipt
        .as_object()
        .expect("request receipt must be an object");
    assert_eq!(
        fields.len(),
        2,
        "request receipt disclosed extra fields: {receipt}"
    );
    assert_eq!(
        receipt["status"], "accepted",
        "request was not accepted: {receipt}"
    );
    let id = receipt["request_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .expect("missing request ID");
    assert!(
        receipt.get("job").is_none(),
        "immediate receipt disclosed job ID"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|d| !d.is_zero())
            .expect("actual promotion observer deadline");
        let (observed, outcome) = events
            .recv_timeout(left)
            .expect("actual promotion observer did not report");
        if observed != id {
            continue;
        }
        match outcome.as_str() {
            "created" => break,
            "pending" => continue,
            _ => panic!("actual promotion for {id} failed: {outcome}"),
        }
    }
    let envelope = rig.envelope(sub, json!({}), json!({}));
    let (status, list) = rig.call("job_list", &envelope, &json!({}));
    assert_eq!(status, 200, "signed owner request list failed: {list}");
    let rows: Vec<_> = list["requests"]
        .as_array()
        .expect("owner requests missing")
        .iter()
        .filter(|row| row["request_id"].as_str() == Some(id))
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "owner request row missing or duplicated: {list}"
    );
    assert_eq!(
        rows[0]["status"], "created",
        "observed promotion not visible to its owner"
    );
    rows[0]["job"]
        .as_str()
        .filter(|s| !s.is_empty())
        .expect("created owner request has no job ID")
        .to_string()
}

fn seed(rig: &Rig) {
    for (key, prefix, policy) in [
        ("visible", "VIS", ""),
        (
            "secret",
            "SEC",
            "policy:\n  version: 1\n  visibility: private\n",
        ),
    ] {
        let project = rig.pm.join(key);
        let issue = project.join(format!("{prefix}-1"));
        std::fs::create_dir_all(&issue).unwrap();
        std::fs::write(
            project.join("project.yaml"),
            format!("key: {key}\nprefix: {prefix}\n{policy}"),
        )
        .unwrap();
        std::fs::write(issue.join("issue.md"), format!(
            "---\nid: {prefix}-1\ntitle: {key}\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"
        )).unwrap();
    }
    // An existing readable, otherwise-unoccupied issue for genuine signed
    // member requests. VIS-1 already has the ordinary fixture job.
    let request_issue = rig.pm.join("visible/VIS-9");
    std::fs::create_dir_all(&request_issue).unwrap();
    std::fs::write(request_issue.join("issue.md"),
        "---\nid: VIS-9\ntitle: Request issue\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n").unwrap();
    let state = rig._dir.path();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(state, &opts).unwrap();
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "cad2215-worker",
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
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    db.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,repo,state,created,updated)
         VALUES('cad2215-visible-job','Visible job','spec.md','pm','VIS-1','visible','open',1,1),
               ('cad2215-secret-job','Secret job','spec.md','pm','SEC-1','visible','open',2,2),
               ('cad2215-free-job','Free job','spec.md','pm',NULL,'visible','open',3,3);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
         VALUES('cad2215-visible-task','cad2215-visible-job','Visible task','cad2215-worker','running',1,1),
               ('cad2215-secret-task','cad2215-secret-job','Secret task','cad2215-worker','running',2,2),
               ('cad2215-free-task','cad2215-free-job','Free task','cad2215-worker','running',3,3);"
    ).unwrap();
    for (monitor, task) in [
        ("cad2215-visible-monitor", "cad2215-visible-task"),
        ("cad2215-secret-monitor", "cad2215-secret-task"),
        ("cad2215-free-monitor", "cad2215-free-task"),
    ] {
        // The same readable project label is intentionally used for all:
        // monitor authorization must follow actual task coverage, not the label.
        shared
            .store
            .register_monitor(
                monitor,
                "visible",
                "cad2215-worker",
                60,
                &[task.into()],
                false,
                false,
            )
            .unwrap();
    }
    db.execute_batch(
        "INSERT INTO monitor_alerts(seq,monitor_id,task_id,event_seq,fingerprint,kind,payload,state,created,updated)
         VALUES(221501,'cad2215-visible-monitor','cad2215-visible-task',1,'cad2215-visible-alert','test','{\"marker\":\"cad2215-visible-payload\"}','open',1,1),
               (221502,'cad2215-secret-monitor','cad2215-secret-task',2,'cad2215-secret-alert','test','{\"marker\":\"cad2215-secret-payload\"}','open',1,1),
               (221503,'cad2215-free-monitor','cad2215-free-task',3,'cad2215-free-alert','test','{\"marker\":\"cad2215-free-payload\"}','open',1,1);"
    ).unwrap();
}

#[test]
fn signed_member_cannot_read_private_or_unbound_work_but_owner_can_recover_it() {
    let rig = start(&[]);
    // Scoped member reads page through the opaque cursor codec, which needs
    // the operator-provisioned keyring this daemon would carry after
    // `daemon cursor-key-init`. Without it the daemon answers
    // `scoped_cursor_unavailable`, which is not an authorization verdict.
    let _keys = initialize_queue_keyring(&rig);
    seed(&rig);
    let member = login(&rig, "cad2215-member", "member");
    let owner = login(&rig, "cad2215-owner", "owner");

    // First reads occur after all fixture data is committed. Owner positives
    // prove the shared model and real HTTP transport saw the sentinel rows.
    let owner_agents = get(&rig, &owner, "/api/agents");
    let member_agents = get(&rig, &member, "/api/agents");
    let owner_detail = get(&rig, &owner, "/api/agents/cad2215-worker");
    let member_detail = get(&rig, &member, "/api/agents/cad2215-worker");
    let owner_overview = get(&rig, &owner, "/api/overview");
    let member_overview = get(&rig, &member, "/api/overview");

    let owner_agent_text = format!("{owner_agents} {owner_detail}");
    for id in [
        "cad2215-visible-task",
        "cad2215-secret-task",
        "cad2215-free-task",
    ] {
        assert!(owner_agent_text.contains(id), "owner recovery omitted {id}");
    }
    let member_agent_text = format!("{member_agents} {member_detail}");
    assert!(
        member_agent_text.contains("cad2215-visible-task"),
        "readable task missing"
    );
    for id in [
        "cad2215-secret-job",
        "cad2215-secret-task",
        "cad2215-free-job",
        "cad2215-free-task",
        "Secret job",
        "Secret task",
        "Free job",
        "Free task",
    ] {
        assert!(
            !member_agent_text.contains(id),
            "member agent read leaked {id}"
        );
    }

    let owner_monitor = &owner_overview["monitoring"];
    let member_monitor = &member_overview["monitoring"];
    assert_eq!(
        owner_monitor["available"], true,
        "monitor fixture unavailable"
    );
    let owner_text = owner_monitor.to_string();
    for id in [
        "cad2215-visible-monitor",
        "cad2215-secret-monitor",
        "cad2215-free-monitor",
        "cad2215-secret-payload",
        "cad2215-free-payload",
    ] {
        assert!(owner_text.contains(id), "owner recovery omitted {id}");
    }
    assert_eq!(
        member_monitor["available"], true,
        "readable monitoring must remain available"
    );
    let member_text = member_monitor.to_string();
    assert!(
        member_text.contains("cad2215-visible-monitor"),
        "readable monitor missing"
    );
    assert!(
        member_text.contains("cad2215-visible-payload"),
        "readable alert missing"
    );
    for id in [
        "cad2215-secret-monitor",
        "cad2215-free-monitor",
        "cad2215-secret-task",
        "cad2215-free-task",
        "cad2215-secret-alert",
        "cad2215-free-alert",
        "cad2215-secret-payload",
        "cad2215-free-payload",
    ] {
        assert!(!member_text.contains(id), "member monitoring leaked {id}");
    }
    assert_eq!(
        member_monitor["open_alerts"], 1,
        "member aggregate includes hidden alerts"
    );
}
