//! CAD-2215 independent signed member-assistant project-binding acceptance.
//! The product implementer must not edit or weaken this check.
//! This does not establish local-agent process/filesystem isolation.

use super::*;

fn assist_call(rig: &Rig, sub: &str, verb: &str, args: Value) -> (u16, Value) {
    let envelope = rig.envelope(sub, json!({}), json!({}));
    rig.call(verb, &envelope, &args)
}

fn expect_job_hidden(rig: &Rig, sub: &str, job: &str, phase: &str) {
    let (status, list) = assist_call(rig, sub, "job_list", json!({}));
    assert_eq!(status, 200, "{phase}: {sub} job_list unavailable");
    assert!(
        !list.to_string().contains(job),
        "{phase}: {sub} list exposed job"
    );
    for verb in ["job_evidence", "job_events"] {
        let (status, _) = assist_call(rig, sub, verb, json!({"job":job}));
        assert!(
            status == 401 || status == 403 || status == 404,
            "{phase}: {sub} {verb} did not deny inaccessible job: {status}"
        );
    }
}

fn expect_job_readable(rig: &Rig, sub: &str, job: &str, phase: &str) {
    let (status, list) = assist_call(rig, sub, "job_list", json!({}));
    assert_eq!(status, 200, "{phase}: {sub} list failed");
    assert!(
        list.to_string().contains(job),
        "{phase}: {sub} list omitted authorized job"
    );
    for verb in ["job_evidence", "job_events"] {
        let (status, body) = assist_call(rig, sub, verb, json!({"job":job}));
        assert_eq!(
            status, 200,
            "{phase}: {sub} {verb} denied authorized job: {body}"
        );
        assert_eq!(
            body["job"].as_str(),
            Some(job),
            "{phase}: wrong job in {verb}"
        );
    }
}

fn expect_operator_recovery(rig: &Rig, job: &str, phase: &str) {
    let body = crate::client::rpc(rig._dir.path(), "job_show", json!({"job":job}))
        .unwrap_or_else(|e| panic!("{phase}: proven local operator recovery refused: {e}"));
    assert_eq!(
        body["job"]["id"].as_str(),
        Some(job),
        "{phase}: operator got wrong job"
    );
}

#[test]
fn signed_assistant_job_reads_require_current_readable_issue_binding_and_authority() {
    let _flag = AssistantFlagOn::arm();
    let (rig, events) = queue_rig(&["ana", "ben"]);
    // A permitted `job_events` read is scoped: the fixture keyring must be
    // initialized, so a refusal below is authorization, not missing keys.
    let _keys = initialize_queue_keyring(&rig);
    seed(&rig);
    let (status, requested) = assist_call(
        &rig,
        "ana",
        "job_request",
        json!({
            "title":"CAD2215_BINDING_JOB", "request_key":"cad2215-binding-job", "issue":"VIS-9"
        }),
    );
    assert_eq!(
        status, 200,
        "signed owner could not create genuine draft: {requested}"
    );
    // The creation response is not treated as evidence that later reads are
    // authorized: this test exercises the independent read endpoints below.
    let job = promoted_job(&rig, &events, "ana", &requested);
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    assert!(
        shared.store.assist_request_owns(ORG, "ana", &job).unwrap(),
        "real ownership ledger missing"
    );
    assert!(!shared.store.assist_request_owns(ORG, "ben", &job).unwrap());
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    let initial: Option<String> = db
        .query_row("SELECT issue_id FROM jobs WHERE id=?1", [&job], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        initial.as_deref(),
        Some("VIS-9"),
        "request did not bind the readable issue"
    );
    // Simulate a retained legacy unbound record; this is a fixture mutation,
    // not a supported API or a newly-created unbound request.
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id=NULL WHERE id=?1", [&job])
            .unwrap(),
        1
    );

    expect_job_hidden(&rig, "ana", &job, "unbound draft");
    expect_operator_recovery(&rig, &job, "unbound draft");

    // Synthetic transition from the legacy unbound record to a readable
    // issue, not a supported rebind API. The real ledger is unchanged.
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id='VIS-9' WHERE id=?1", [&job])
            .unwrap(),
        1
    );
    expect_job_readable(&rig, "ana", &job, "readable binding");
    expect_operator_recovery(&rig, &job, "readable binding");
    expect_job_hidden(&rig, "ben", &job, "unshared readable binding");

    // The owner explicitly shares, and a second signed member subscribes.
    let (status, share) = assist_call(&rig, "ana", "job_share", json!({"job":job,"enabled":true}));
    assert_eq!(status, 200, "owner share failed: {share}");
    assert_eq!(share["shared"], true);
    let (status, subscribe) = assist_call(&rig, "ben", "job_subscribe", json!({"job":job}));
    assert_eq!(status, 200, "signed subscriber failed: {subscribe}");
    assert!(
        shared.store.job_is_subscriber(&job, ORG, "ben").unwrap(),
        "real subscriber ledger missing"
    );
    expect_job_readable(&rig, "ben", &job, "active share and readable binding");

    // Sharing withdrawal is a fresh authorization decision, not an irrevocable
    // grant from a stale subscriber row.
    let (status, share) = assist_call(&rig, "ana", "job_share", json!({"job":job,"enabled":false}));
    assert_eq!(status, 200, "owner share withdrawal failed: {share}");
    assert_eq!(share["shared"], false);
    expect_job_hidden(&rig, "ben", &job, "withdrawn share");
    expect_job_readable(&rig, "ana", &job, "owner after withdrawal");

    // Synthetic transition to a private project, not a supported rebind API;
    // a job owner is not a proven operator or an ungranted project reader.
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id='SEC-1' WHERE id=?1", [&job])
            .unwrap(),
        1
    );
    expect_job_hidden(&rig, "ana", &job, "private binding without grant");
    expect_job_hidden(&rig, "ben", &job, "private binding without grant");
    expect_operator_recovery(&rig, &job, "private binding without grant");
}

#[test]
fn signed_idempotent_job_request_replay_requires_current_readable_binding() {
    let _flag = AssistantFlagOn::arm();
    let (rig, events) = queue_rig(&["ana"]);
    seed(&rig);
    let args = json!({"title":"CAD2215_REPLAY_PRIVATE_TITLE", "request_key":"cad2215-replay-binding", "issue":"VIS-9"});
    let (status, created) = assist_call(&rig, "ana", "job_request", args.clone());
    assert_eq!(
        status, 200,
        "real signed request could not create job: {created}"
    );
    let job = promoted_job(&rig, &events, "ana", &created);
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    assert!(
        shared.store.assist_request_owns(ORG, "ana", &job).unwrap(),
        "real ownership ledger missing"
    );
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    let count = || -> (i64, i64) {
        (
            db.query_row("SELECT COUNT(*) FROM jobs WHERE id=?1", [&job], |r| r.get(0)).unwrap(),
            db.query_row("SELECT COUNT(*) FROM assist_requests WHERE org=?1 AND sub='ana' AND request_key='cad2215-replay-binding'", [ORG], |r| r.get(0)).unwrap(),
        )
    };
    assert_eq!(
        count(),
        (1, 1),
        "fixture request did not create exactly one canonical job"
    );

    // Synthetic transition to a real private issue without an Ana grant;
    // not a supported rebind API. The real owner/key/subscription are unchanged.
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id='SEC-1' WHERE id=?1", [&job])
            .unwrap(),
        1
    );
    let before = count();
    let (status, refused) = assist_call(&rig, "ana", "job_request", args.clone());
    assert!(
        status == 401 || status == 403 || status == 404,
        "inaccessible replay returned successful read-equivalent result ({status}): {refused}"
    );
    assert!(
        !refused["job_record"].is_object() && !refused["job"].is_object(),
        "inaccessible replay returned a job record"
    );
    let text = refused.to_string();
    assert!(
        !text.contains("CAD2215_REPLAY_PRIVATE_TITLE"),
        "inaccessible replay disclosed private title"
    );
    assert!(
        !text.contains("SEC-1"),
        "inaccessible replay disclosed private issue ID"
    );
    assert_eq!(
        count(),
        before,
        "refused replay duplicated job or ownership ledger"
    );
    let issue: Option<String> = db
        .query_row("SELECT issue_id FROM jobs WHERE id=?1", [&job], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        issue.as_deref(),
        Some("SEC-1"),
        "refused replay changed binding"
    );

    // Synthetic restoration of the original VIS-9 binding, not a rebind API.
    // The idempotent request must resume the same canonical job without duplication.
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id='VIS-9' WHERE id=?1", [&job])
            .unwrap(),
        1
    );
    let (status, replay) = assist_call(&rig, "ana", "job_request", args);
    assert_eq!(status, 200, "readable replay refused: {replay}");
    assert_eq!(
        replay, created,
        "authorized replay changed its occupancy-independent receipt"
    );
    assert!(
        replay.get("job").is_none() && replay.get("job_record").is_none(),
        "replay disclosed job details"
    );
    assert_eq!(
        count(),
        (1, 1),
        "readable replay duplicated job or request ledger"
    );
    assert!(
        shared.store.assist_request_owns(ORG, "ana", &job).unwrap(),
        "replay lost original ownership"
    );
    let issue: Option<String> = db
        .query_row("SELECT issue_id FROM jobs WHERE id=?1", [&job], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        issue.as_deref(),
        Some("VIS-9"),
        "readable replay changed approved binding"
    );
}
