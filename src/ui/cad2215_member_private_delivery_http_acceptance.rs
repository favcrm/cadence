//! CAD-2215 independently authored signed-board private-delivery acceptance.
//! Exercises the real member HTTP transport, daemon proof and durable producer;
//! this is not a local-agent process/filesystem isolation proof.
use super::*;

fn member_delivery_call(rig: &Rig, sub: &str, claims: Value, args: Value) -> (u16, Value) {
    let envelope = rig.envelope(sub, json!({}), claims);
    rig.call("job_list", &envelope, &args)
}

fn member_delivery_hidden(status: u16, body: &Value, id: &str, marker: &str, phase: &str) {
    assert!(
        !body.to_string().contains(id),
        "{phase}: private delivery id leaked ({status}): {body}"
    );
    assert!(
        !body.to_string().contains(marker),
        "{phase}: private delivery body leaked ({status}): {body}"
    );
    if status == 200 {
        assert_eq!(
            body["private_deliveries_state"], "available",
            "{phase}: unacknowledged projection: {body}"
        );
        assert!(body.get("_reader_scope").is_none());
        assert!(!body["private_deliveries"]
            .as_array()
            .expect("scoped delivery field missing")
            .iter()
            .any(|r| r["id"] == id));
    }
}

#[allow(clippy::too_many_arguments)] // one assertion helper, one row of facts
fn member_unacked_unavailable(
    status: u16,
    body: &Value,
    job: &str,
    task: &str,
    message: &str,
    request_id: &str,
    delivery: &str,
    marker: &str,
    phase: &str,
) {
    assert!(
        status >= 400,
        "{phase}: unacknowledged member list succeeded ({status}): {body}"
    );
    for key in [
        "jobs",
        "requests",
        "private_deliveries",
        "private_deliveries_state",
        "promotion_worker",
        "count",
        "total",
        "_reader_scope",
    ] {
        assert!(
            body.get(key).is_none(),
            "{phase}: unacknowledged list exposed {key}: {body}"
        );
    }
    let text = body.to_string();
    for secret in [job, task, message, request_id, delivery, marker] {
        assert!(
            !text.contains(secret),
            "{phase}: unacknowledged list disclosed {secret}: {body}"
        );
    }
}

// Park the real joined worker *after* it reports actual promotion. This
// preserves the genuine queue path while preventing a periodic retry from
// healing our intentionally installed incomplete crash-window row mid-read.
fn member_delivery_held_queue_rig(
    subs: &[&str],
) -> (
    Rig,
    std::sync::mpsc::Receiver<(String, String)>,
    Arc<(Mutex<bool>, std::sync::Condvar)>,
) {
    let (tx, rx) = std::sync::mpsc::channel::<(String, String)>();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let worker_release = Arc::clone(&release);
    let observer = Arc::new(move |id: &str, outcome: &str| {
        let _ = tx.send((id.to_string(), outcome.to_string()));
        if outcome == "created" {
            let (lock, cv) = &*worker_release;
            drop(
                cv.wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(90), |go| !*go)
                    .unwrap(),
            );
        }
    });
    let rig = start_with_queue_service(subs, Some(observer), false);
    enroll_queue_service(&rig);
    (rig, rx, release)
}

#[test]
fn signed_member_private_deliveries_require_current_owner_project_and_authority() {
    let _flag = AssistantFlagOn::arm();
    let (rig, events, release_worker) = member_delivery_held_queue_rig(&["ana", "ben"]);
    seed(&rig);
    let request_args = json!({"title":"member private delivery", "request_key":"member-private-delivery", "issue":"VIS-9", "body":"work"});
    let envelope = rig.envelope("ana", json!({}), json!({}));
    let (status, receipt) = rig.call("job_request", &envelope, &request_args);
    assert_eq!(
        status, 200,
        "genuine signed member request failed: {receipt}"
    );
    let job = promoted_job(&rig, &events, "ana", &receipt);
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    let original = shared
        .store
        .assist_request_for_job(&job)
        .unwrap()
        .expect("unique request missing");
    assert_eq!(original.sub, "ana");
    assert_eq!(original.credential_id.as_deref(), Some("cred-ana"));
    let owner_pm = shared.store.job(&job).unwrap().pm_alias;
    let worker_dir = rig._dir.path().join("member-delivery-worker");
    std::fs::create_dir_all(&worker_dir).unwrap();
    let worker_params = json!({"upstream":owner_pm}).to_string();
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "member-delivery-worker",
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: worker_dir.to_str().unwrap(),
            sandbox: "workspace-write",
            instructions: None,
            params: Some(&worker_params),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let task = shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let native = |method: &str, args: Value| {
        crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
            crate::client::rpc(rig._dir.path(), method, args)
        })
    };
    let dispatched = native(
        "task_dispatch",
        json!({"task":task,"to":"member-delivery-worker","by":"operator"}),
    )
    .expect("real deferred-authority dispatch failed");
    let message = dispatched["message"]
        .as_str()
        .expect("dispatch message missing")
        .to_string();
    let msg = shared.store.message(&message).unwrap().unwrap();
    let sha = "1234567890abcdef1234567890abcdef12345678";
    let marker = "MEMBER_PRIVATE_DELIVERY_SENTINEL_2215";
    shared
        .store
        .finish(
            &msg,
            "completed",
            &json!({"status":"completed","sha":sha,"text":marker}),
            None,
        )
        .unwrap();
    native(
        "task_verdict",
        json!({"task":task,"sha":sha,"verdict":"pass"}),
    )
    .expect("real verdict failed");
    let op =
        native("delivery_private_list", json!({})).expect("operator private recovery unavailable");
    let native_rows = op["deliveries"]
        .as_array()
        .expect("operator private delivery list missing");
    let native_row = native_rows
        .iter()
        .find(|r| r["origin"]["job"] == job && r["origin"]["task"] == task)
        .expect("real dispatched task did not produce delivery");
    let delivery = native_row["id"]
        .as_str()
        .expect("delivery id missing")
        .to_string();
    assert_eq!(native_row["state"], "completed");
    assert!(native_row["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["message"] == message && r["body"] == marker));

    let (status, owner) = member_delivery_call(&rig, "ana", json!({}), json!({}));
    assert_eq!(status, 200, "owner projection refused: {owner}");
    assert_eq!(owner["private_deliveries_state"], "available");
    assert!(owner.get("_reader_scope").is_none());
    let rows = owner["private_deliveries"]
        .as_array()
        .expect("owner private deliveries missing");
    let own: Vec<_> = rows.iter().filter(|r| r["id"] == delivery).collect();
    assert_eq!(
        own.len(),
        1,
        "owner missing or duplicated genuine delivery: {owner}"
    );
    assert_eq!(own[0]["origin"]["owner_org"], ORG);
    assert_eq!(own[0]["origin"]["owner_sub"], "ana");
    assert_eq!(own[0]["origin"]["issue"], "VIS-9");
    assert!(own[0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["message"] == message && r["body"] == marker));
    // Simulate only an old daemon omitting its scope acknowledgement: the
    // native handler still produced the genuine authorized owner projection.
    let old_seam = crate::delivery::publication::arm_member_job_list_response(
        rig._dir.path(),
        Arc::new(|value: &mut Value| {
            value
                .as_object_mut()
                .expect("native member list object")
                .remove(cadence_core::board_api::READER_SCOPE_FIELD);
        }),
    );
    let (old_status, old) = member_delivery_call(&rig, "ana", json!({}), json!({}));
    drop(old_seam);
    let request_id = receipt["request_id"].as_str().unwrap();
    member_unacked_unavailable(
        old_status,
        &old,
        &job,
        &task,
        &message,
        request_id,
        &delivery,
        marker,
        "missing ACK",
    );
    let (restored_status, restored_ack) = member_delivery_call(&rig, "ana", json!({}), json!({}));
    assert_eq!(restored_status, 200);
    assert_eq!(restored_ack["private_deliveries_state"], "available");
    assert!(
        restored_ack.to_string().contains(&delivery) && restored_ack.to_string().contains(marker),
        "current daemon positive lost after old seam: {restored_ack}"
    );
    // The genuine native handler still runs. Corrupt only the response
    // after it returns, modeling a malformed/misbound older peer.
    for case in ["invalid_ack", "malformed_row", "wrong_owner"] {
        let hook = Arc::new(move |value: &mut Value| match case {
            "invalid_ack" => {
                value[cadence_core::board_api::READER_SCOPE_FIELD] = json!("invalid-scope-ack")
            }
            "malformed_row" => {
                value["private_deliveries"][0]["results"] = json!("not-a-result-array")
            }
            "wrong_owner" => value["private_deliveries"][0]["origin"]["owner_sub"] = json!("ben"),
            _ => unreachable!(),
        });
        let seam =
            crate::delivery::publication::arm_member_job_list_response(rig._dir.path(), hook);
        let (status, body) = member_delivery_call(&rig, "ana", json!({}), json!({}));
        drop(seam);
        if case == "invalid_ack" {
            member_unacked_unavailable(
                status,
                &body,
                &job,
                &task,
                &message,
                request_id,
                &delivery,
                marker,
                "invalid ACK",
            );
            continue;
        }
        assert_eq!(
            status, 200,
            "{case}: ordinary member list unavailable: {body}"
        );
        assert_eq!(
            body["private_deliveries_state"], "unavailable",
            "{case}: bad projection accepted: {body}"
        );
        assert!(
            body.get("private_deliveries").is_none(),
            "{case}: board forwarded unproven rows: {body}"
        );
        assert!(
            body.get("_reader_scope").is_none(),
            "{case}: internal scope leaked"
        );
        assert!(
            !body.to_string().contains(&delivery) && !body.to_string().contains(marker),
            "{case}: private data leaked: {body}"
        );
        assert_eq!(body["jobs"], owner["jobs"], "{case}: ordinary jobs lost");
        assert_eq!(
            body["requests"], owner["requests"],
            "{case}: ordinary requests lost"
        );
    }
    let (status, other) = member_delivery_call(&rig, "ben", json!({}), json!({}));
    assert_eq!(status, 200, "other member positive control failed: {other}");
    member_delivery_hidden(status, &other, &delivery, marker, "other member");

    // A real explicit share + signed subscription grants the ordinary job,
    // not the owner's private delivery or its identifier.
    let (status, share) = rig.call(
        "job_share",
        &rig.envelope("ana", json!({}), json!({})),
        &json!({"job":job,"enabled":true}),
    );
    assert_eq!(status, 200, "owner share failed: {share}");
    let (status, sub) = rig.call(
        "job_subscribe",
        &rig.envelope("ben", json!({}), json!({})),
        &json!({"job":job}),
    );
    assert_eq!(status, 200, "subscriber admission failed: {sub}");
    let (status, subscribed) = member_delivery_call(&rig, "ben", json!({}), json!({}));
    assert_eq!(status, 200, "subscriber list failed: {subscribed}");
    assert!(
        subscribed.to_string().contains(&job),
        "subscriber's legitimately shared job missing: {subscribed}"
    );
    member_delivery_hidden(status, &subscribed, &delivery, marker, "subscriber");

    // A real revoked-share scenario: save the genuine prior subscriber
    // job list, revoke the actual owner share, then model an old daemon
    // returning that stale data without a scope ACK. Its response must not
    // leak *any* job/request/private row through the signed board relay.
    let (status, withdrawn) = rig.call(
        "job_share",
        &rig.envelope("ana", json!({}), json!({})),
        &json!({"job":job,"enabled":false}),
    );
    assert_eq!(status, 200, "owner share withdrawal failed: {withdrawn}");
    let (status, current_ben) = member_delivery_call(&rig, "ben", json!({}), json!({}));
    assert_eq!(status, 200, "current subscriber list failed: {current_ben}");
    assert!(
        !current_ben.to_string().contains(&job),
        "revoked share still visible: {current_ben}"
    );
    let stale_jobs = subscribed["jobs"].clone();
    let stale_requests = owner["requests"].clone();
    let stale_private = owner["private_deliveries"].clone();
    let seam = crate::delivery::publication::arm_member_job_list_response(
        rig._dir.path(),
        Arc::new(move |value: &mut Value| {
            value["jobs"] = stale_jobs.clone();
            value["requests"] = stale_requests.clone();
            value["private_deliveries"] = stale_private.clone();
            value
                .as_object_mut()
                .unwrap()
                .remove(cadence_core::board_api::READER_SCOPE_FIELD);
        }),
    );
    let (status, stale) = member_delivery_call(&rig, "ben", json!({}), json!({}));
    drop(seam);
    member_unacked_unavailable(
        status,
        &stale,
        &job,
        &task,
        &message,
        request_id,
        &delivery,
        marker,
        "revoked share on old daemon",
    );
    let (status, reshared) = rig.call(
        "job_share",
        &rig.envelope("ana", json!({}), json!({})),
        &json!({"job":job,"enabled":true}),
    );
    assert_eq!(
        status, 200,
        "owner could not restore genuine share: {reshared}"
    );
    let (status, subscriber_again) = member_delivery_call(&rig, "ben", json!({}), json!({}));
    assert_eq!(
        status, 200,
        "restored subscriber list failed: {subscriber_again}"
    );
    assert!(
        subscriber_again.to_string().contains(&job),
        "restored share lost legitimate job: {subscriber_again}"
    );
    member_delivery_hidden(
        status,
        &subscriber_again,
        &delivery,
        marker,
        "restored subscriber",
    );

    // Reproduce the actual bind-before-completed crash window on the
    // genuine accepted queue row and genuine completed task/result. The
    // real worker is parked after its created observer, not mocked.
    let db_window = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    let task_at_window = shared.store.task(&task).unwrap().to_json();
    let message_at_window = shared.store.message(&message).unwrap().unwrap().to_json();
    let was_complete: i64 = db_window.query_row(
        "SELECT completed FROM assist_request_queue WHERE org=?1 AND sub='ana' AND request_key='member-private-delivery' AND job_id=?2",
        rusqlite::params![ORG, job], |r| r.get(0)).unwrap();
    assert_eq!(was_complete, 1, "genuine queue row was not completed");
    assert_eq!(db_window.execute(
        "UPDATE assist_request_queue SET completed=0 WHERE org=?1 AND sub='ana' AND request_key='member-private-delivery' AND job_id=?2 AND completed=1",
        rusqlite::params![ORG, job]).unwrap(), 1);
    let (status, pending_window) = member_delivery_call(&rig, "ana", json!({}), json!({}));
    assert_eq!(
        status, 200,
        "owner pending request list failed: {pending_window}"
    );
    let request_id = receipt["request_id"].as_str().unwrap();
    let request_row = pending_window["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["request_id"].as_str() == Some(request_id))
        .expect("pending request missing");
    assert_eq!(
        request_row["status"], "pending",
        "crash-window request did not remain pending: {pending_window}"
    );
    member_delivery_hidden(
        status,
        &pending_window,
        &delivery,
        marker,
        "bound but incomplete queue row",
    );
    let (status, subscriber_window) = member_delivery_call(&rig, "ben", json!({}), json!({}));
    assert_eq!(
        status, 200,
        "subscriber crash-window list failed: {subscriber_window}"
    );
    member_delivery_hidden(
        status,
        &subscriber_window,
        &delivery,
        marker,
        "subscriber crash window",
    );
    assert_eq!(db_window.execute(
        "UPDATE assist_request_queue SET completed=1 WHERE org=?1 AND sub='ana' AND request_key='member-private-delivery' AND job_id=?2 AND completed=0",
        rusqlite::params![ORG, job]).unwrap(), 1);
    let (status, recovered_window) = member_delivery_call(&rig, "ana", json!({}), json!({}));
    assert_eq!(
        status, 200,
        "restored queue row denied owner: {recovered_window}"
    );
    assert_eq!(recovered_window["private_deliveries_state"], "available");
    assert!(
        recovered_window.to_string().contains(&delivery)
            && recovered_window.to_string().contains(marker),
        "restored completion did not restore private result: {recovered_window}"
    );
    assert_eq!(
        shared.store.task(&task).unwrap().to_json(),
        task_at_window,
        "queue-row recovery reran task"
    );
    assert_eq!(
        shared.store.message(&message).unwrap().unwrap().to_json(),
        message_at_window,
        "queue-row recovery changed durable result"
    );
    // A genuine completed row with a damaged planned job id is not a
    // pre-queue legacy record. Its unique real request still names job.
    assert_eq!(db_window.execute(
        "UPDATE assist_request_queue SET job_id='fixture-wrong-planned-job' WHERE org=?1 AND sub='ana' AND request_key='member-private-delivery' AND job_id=?2 AND completed=1",
        rusqlite::params![ORG, job]).unwrap(), 1);
    assert_eq!(
        shared
            .store
            .assist_request_for_job(&job)
            .unwrap()
            .unwrap()
            .sub,
        "ana",
        "fixture lost the real unique request binding"
    );
    let (status, damaged_plan) = member_delivery_call(&rig, "ana", json!({}), json!({}));
    member_delivery_hidden(
        status,
        &damaged_plan,
        &delivery,
        marker,
        "mismatched planned job id",
    );
    assert_eq!(db_window.execute(
        "UPDATE assist_request_queue SET job_id=?1 WHERE org=?2 AND sub='ana' AND request_key='member-private-delivery' AND job_id='fixture-wrong-planned-job' AND completed=1",
        rusqlite::params![job, ORG]).unwrap(), 1);
    let (status, restored_plan) = member_delivery_call(&rig, "ana", json!({}), json!({}));
    assert_eq!(
        status, 200,
        "restored planned id denied owner: {restored_plan}"
    );
    assert_eq!(restored_plan["private_deliveries_state"], "available");
    assert!(
        restored_plan.to_string().contains(&delivery) && restored_plan.to_string().contains(marker),
        "restored planned id did not restore genuine delivery: {restored_plan}"
    );
    assert_eq!(
        shared.store.task(&task).unwrap().to_json(),
        task_at_window,
        "planned-id test reran task"
    );
    assert_eq!(
        shared.store.message(&message).unwrap().unwrap().to_json(),
        message_at_window,
        "planned-id test changed durable result"
    );
    // A second principal's actual queue row also claims the already-bound
    // job. The row is synthetic fixture corruption, not a new accepted
    // request; the genuine owner row, task, job and result remain unchanged.
    // The actual joined worker remains parked throughout this test.
    assert_eq!(db_window.execute(
        "INSERT INTO assist_request_queue(org,sub,request_key,issue,title,body,acceptance,credential_id,created,last_attempt,last_outcome,completed,job_id) SELECT org,'ben','fixture-cross-owner-queue',issue,'synthetic conflict','','','cred-ben',created,0,NULL,1,job_id FROM assist_request_queue WHERE org=?1 AND sub='ana' AND request_key='member-private-delivery' AND job_id=?2",
        rusqlite::params![ORG, job]).unwrap(), 1);
    let conflict_rows = shared.store.assist_requests_queued(ORG, "ben").unwrap();
    let conflict: Vec<_> = conflict_rows
        .iter()
        .filter(|r| r.request_key == "fixture-cross-owner-queue")
        .collect();
    assert_eq!(conflict.len(), 1, "conflicting queue row missing");
    assert_eq!(conflict[0].planned_job_id, job);
    let conflict_request_id = conflict[0].request_id.clone();
    assert!(
        native("delivery_private_list", json!({}))
            .unwrap()
            .to_string()
            .contains(&delivery),
        "operator lost private recovery during ambiguity"
    );
    for (sub, own_request, other_request) in [
        ("ana", request_id, conflict_request_id.as_str()),
        ("ben", conflict_request_id.as_str(), request_id),
    ] {
        let (status, ambiguous_queue) = member_delivery_call(&rig, sub, json!({}), json!({}));
        assert_eq!(
            status, 200,
            "{sub}: own pending request unavailable: {ambiguous_queue}"
        );
        member_delivery_hidden(
            status,
            &ambiguous_queue,
            &delivery,
            marker,
            "cross-principal queue ambiguity",
        );
        let text = ambiguous_queue.to_string();
        for secret in [
            job.as_str(),
            other_request,
            message.as_str(),
            delivery.as_str(),
            marker,
        ] {
            assert!(
                !text.contains(secret),
                "{sub}: ambiguous queue leaked {secret}: {ambiguous_queue}"
            );
        }
        let requests = ambiguous_queue["requests"]
            .as_array()
            .expect("own accepted requests missing");
        let own: Vec<_> = requests
            .iter()
            .filter(|r| r["request_id"].as_str() == Some(own_request))
            .collect();
        assert_eq!(
            own.len(),
            1,
            "{sub}: own receipt missing or duplicated: {ambiguous_queue}"
        );
        assert_eq!(
            own[0]["status"], "pending",
            "{sub}: ambiguous request claimed creation: {ambiguous_queue}"
        );
        assert!(
            own[0]["job"].is_null(),
            "{sub}: ambiguous request disclosed bound job: {ambiguous_queue}"
        );
    }
    assert_eq!(db_window.execute("DELETE FROM assist_request_queue WHERE org=?1 AND sub='ben' AND request_key='fixture-cross-owner-queue' AND job_id=?2", rusqlite::params![ORG, job]).unwrap(), 1);
    let (status, restored_owner_queue) = member_delivery_call(&rig, "ana", json!({}), json!({}));
    assert_eq!(
        status, 200,
        "owner recovery after removing conflict failed: {restored_owner_queue}"
    );
    assert_eq!(
        restored_owner_queue["private_deliveries_state"],
        "available"
    );
    assert!(
        restored_owner_queue.to_string().contains(&delivery)
            && restored_owner_queue.to_string().contains(marker),
        "real owner delivery did not recover: {restored_owner_queue}"
    );
    let restored_request = restored_owner_queue["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["request_id"].as_str() == Some(request_id))
        .expect("original accepted receipt did not survive ambiguity");
    assert_eq!(restored_request["status"], "created");
    assert_eq!(restored_request["job"], job);
    assert!(
        native("delivery_private_list", json!({}))
            .unwrap()
            .to_string()
            .contains(&delivery),
        "operator recovery was lost"
    );
    assert_eq!(
        shared.store.task(&task).unwrap().to_json(),
        task_at_window,
        "queue ambiguity reran task"
    );
    assert_eq!(
        shared.store.message(&message).unwrap().unwrap().to_json(),
        message_at_window,
        "queue ambiguity changed result"
    );
    let (lock, cv) = &*release_worker;
    *lock.lock().unwrap() = true;
    cv.notify_all();
    for (claims, args) in [
        (json!({"actor":"users/ana"}), json!({})),
        (json!({}), json!({"owner":"ana", "job":job})),
    ] {
        let (status, forged) = member_delivery_call(&rig, "ben", claims, args);
        assert_ne!(status, 200, "forged identity accepted: {forged}");
        member_delivery_hidden(status, &forged, &delivery, marker, "forged identity");
    }
    // Removing project authority denies even the true owner; recovery is
    // still available to the proven local operator.
    let policy = rig.pm.join("visible/project.yaml");
    let original_policy = std::fs::read(&policy).unwrap();
    std::fs::write(
        &policy,
        "key: visible\nprefix: VIS\npolicy:\n  version: 1\n  visibility: private\n",
    )
    .unwrap();
    let (status, denied) = member_delivery_call(&rig, "ana", json!({}), json!({}));
    member_delivery_hidden(status, &denied, &delivery, marker, "revoked project");
    assert!(
        native("delivery_private_list", json!({}))
            .unwrap()
            .to_string()
            .contains(&delivery),
        "operator lost recovery under project denial"
    );
    std::fs::write(&policy, original_policy).unwrap();

    // The actual job request remains C1. A live C2 signs a new request after
    // rotation; completed history stays readable to the same current owner.
    rig.authority
        .lock()
        .unwrap()
        .members
        .get_mut("ana")
        .unwrap()
        .recorded_credential = "cred-ana-c2".to_string();
    assert_eq!(
        shared
            .store
            .assist_request_for_job(&job)
            .unwrap()
            .unwrap()
            .credential_id
            .as_deref(),
        Some("cred-ana")
    );
    let c2 = json!({"credential_id":"cred-ana-c2"});
    let (status, rotated) = member_delivery_call(&rig, "ana", c2.clone(), json!({}));
    assert_eq!(status, 200, "live C2 lost completed history: {rotated}");
    assert_eq!(rotated["private_deliveries_state"], "available");
    assert!(rotated.get("_reader_scope").is_none());
    assert!(
        rotated.to_string().contains(&delivery) && rotated.to_string().contains(marker),
        "current owner lost history after rotation: {rotated}"
    );
    let (status, stale) = member_delivery_call(&rig, "ana", json!({}), json!({}));
    assert_ne!(status, 200, "revoked C1 accepted: {stale}");
    member_delivery_hidden(status, &stale, &delivery, marker, "revoked C1 caller");
    rig.authority
        .lock()
        .unwrap()
        .members
        .get_mut("ana")
        .unwrap()
        .credential_live = false;
    let (status, revoked) = member_delivery_call(&rig, "ana", c2.clone(), json!({}));
    assert_ne!(
        status, 200,
        "revoked current credential accepted: {revoked}"
    );
    member_delivery_hidden(
        status,
        &revoked,
        &delivery,
        marker,
        "revoked current credential",
    );
    rig.authority
        .lock()
        .unwrap()
        .members
        .get_mut("ana")
        .unwrap()
        .credential_live = true;
    rig.authority
        .lock()
        .unwrap()
        .members
        .get_mut("ana")
        .unwrap()
        .role = None;
    let (status, removed) = member_delivery_call(&rig, "ana", c2.clone(), json!({}));
    assert_ne!(status, 200, "removed membership accepted: {removed}");
    member_delivery_hidden(status, &removed, &delivery, marker, "removed membership");
    rig.authority
        .lock()
        .unwrap()
        .members
        .get_mut("ana")
        .unwrap()
        .role = Some("member");

    // Synthetic damaged provenance: make the existing genuine request no
    // longer name this job, then restore it; next add a second conflicting
    // ownership row and remove only that fixture row. Neither grants access.
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    assert_eq!(db.execute("UPDATE assist_requests SET job='fixture-detached' WHERE org=?1 AND sub='ana' AND job=?2", rusqlite::params![ORG, job]).unwrap(), 1);
    let (status, missing) = member_delivery_call(&rig, "ana", c2.clone(), json!({}));
    member_delivery_hidden(status, &missing, &delivery, marker, "missing ownership");
    assert_eq!(db.execute("UPDATE assist_requests SET job=?1 WHERE org=?2 AND sub='ana' AND job='fixture-detached'", rusqlite::params![job, ORG]).unwrap(), 1);
    assert_eq!(db.execute("INSERT INTO assist_requests(org,sub,request_key,job,created) VALUES(?1,'ben','fixture-ambiguous-owner',?2,1)", rusqlite::params![ORG, job]).unwrap(), 1);
    let (status, ambiguous) = member_delivery_call(&rig, "ana", c2.clone(), json!({}));
    member_delivery_hidden(status, &ambiguous, &delivery, marker, "ambiguous ownership");
    assert_eq!(db.execute("DELETE FROM assist_requests WHERE org=?1 AND sub='ben' AND request_key='fixture-ambiguous-owner'", [ORG]).unwrap(), 1);
    let (status, restored) = member_delivery_call(&rig, "ana", c2.clone(), json!({}));
    assert_eq!(status, 200, "restored owner list failed: {restored}");
    assert!(
        restored.to_string().contains(&delivery) && restored.to_string().contains(marker),
        "restored owner positive missing: {restored}"
    );

    // Park the actual signed HTTP read after projection, revoke the CURRENT
    // credential at the independent platform, then allow final reproof.
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let release_hook = Arc::clone(&release);
    let hook = Arc::new(move || {
        entered_tx.send(()).unwrap();
        let (lock, cv) = &*release_hook;
        drop(
            cv.wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(25), |go| !*go)
                .unwrap(),
        );
    });
    let seam =
        crate::delivery::publication::arm_member_delivery_post_projection(rig._dir.path(), hook);
    let env = rig.envelope("ana", json!({}), c2.clone());
    let (entered, result) = std::thread::scope(|scope| {
        let inflight = scope.spawn(|| rig.call("job_list", &env, &json!({})));
        let entered = entered_rx.recv_timeout(Duration::from_secs(20)).is_ok();
        rig.authority
            .lock()
            .unwrap()
            .members
            .get_mut("ana")
            .unwrap()
            .credential_live = false;
        let (lock, cv) = &*release;
        *lock.lock().unwrap() = true;
        cv.notify_all();
        (entered, inflight.join().expect("signed HTTP read panicked"))
    });
    drop(seam);
    assert!(entered, "actual daemon never reached post-projection seam");
    assert_ne!(
        result.0, 200,
        "in-flight current-credential revocation returned success: {}",
        result.1
    );
    member_delivery_hidden(
        result.0,
        &result.1,
        &delivery,
        marker,
        "post-projection revocation",
    );
    rig.authority
        .lock()
        .unwrap()
        .members
        .get_mut("ana")
        .unwrap()
        .credential_live = true;
    let (status, final_owner) = member_delivery_call(&rig, "ana", c2, json!({}));
    assert_eq!(
        status, 200,
        "re-enabled current owner lost access: {final_owner}"
    );
    assert!(
        final_owner.to_string().contains(&delivery) && final_owner.to_string().contains(marker)
    );
    assert!(
        native("delivery_private_list", json!({}))
            .unwrap()
            .to_string()
            .contains(&delivery),
        "operator recovery was lost"
    );
}
