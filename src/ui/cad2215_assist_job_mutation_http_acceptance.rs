//! CAD-2215 independent signed member job mutation binding acceptance.
//! The product implementer must not edit or weaken this check.
//! This is HTTP/daemon authorization coverage, not local-agent isolation.

use super::*;

fn mutation_call(rig: &Rig, sub: &str, verb: &str, args: Value) -> (u16, Value) {
    let envelope = rig.envelope(sub, json!({}), json!({}));
    rig.call(verb, &envelope, &args)
}

fn job_state(
    shared: &crate::daemon::Shared,
    job: &str,
) -> (bool, bool, Option<String>, Option<String>) {
    let request = shared
        .store
        .assist_request_for_job(job)
        .unwrap()
        .expect("real request ledger missing");
    (
        shared.store.assist_job_is_shared(job, ORG).unwrap(),
        shared.store.job_is_subscriber(job, ORG, "ben").unwrap(),
        request.credential_id,
        shared.store.assist_job_authority_rev(job).unwrap(),
    )
}

fn refused_without_mutation(
    rig: &Rig,
    shared: &crate::daemon::Shared,
    sub: &str,
    verb: &str,
    args: Value,
    job: &str,
    phase: &str,
) {
    let before = job_state(shared, job);
    let (status, body) = mutation_call(rig, sub, verb, args);
    assert!(
        status == 401 || status == 403 || status == 404,
        "{phase}: {sub} {verb} should deny inaccessible job, got {status}: {body}"
    );
    assert!(
        !body["job"].is_object() && !body["job_record"].is_object(),
        "{phase}: {verb} returned a job record on refusal"
    );
    assert_eq!(
        job_state(shared, job),
        before,
        "{phase}: refused {verb} changed job authorization state"
    );
}

fn operator_recovery(rig: &Rig, job: &str) {
    let result = crate::client::rpc(rig._dir.path(), "job_show", json!({"job":job}))
        .expect("proven operator job recovery refused");
    assert_eq!(result["job"]["id"].as_str(), Some(job));
}

#[test]
fn signed_share_subscribe_reconcile_require_a_readable_job_binding() {
    let _flag = AssistantFlagOn::arm();
    let (rig, events) = queue_rig(&["ana", "ben"]);
    seed(&rig);
    let (status, requested) = mutation_call(
        &rig,
        "ana",
        "job_request",
        json!({
            "title":"CAD2215_MUTATION_BINDING", "request_key":"cad2215-mutation-binding", "issue":"VIS-9"
        }),
    );
    assert_eq!(
        status, 200,
        "signed genuine job request failed: {requested}"
    );
    let job = promoted_job(&rig, &events, "ana", &requested);
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    assert!(shared.store.assist_request_owns(ORG, "ana", &job).unwrap());
    assert!(!shared.store.assist_request_owns(ORG, "ben", &job).unwrap());
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    let binding: Option<String> = db
        .query_row("SELECT issue_id FROM jobs WHERE id=?1", [&job], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        binding.as_deref(),
        Some("VIS-9"),
        "request did not bind the readable issue"
    );
    // Synthetic retained legacy unbound record, not a supported new request.
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id=NULL WHERE id=?1", [&job])
            .unwrap(),
        1
    );
    operator_recovery(&rig, &job);

    // Fixture reads the real current pin from store so a reconcile denial
    // cannot pass merely because the supplied revision is stale or invalid.
    let pin = shared
        .store
        .assist_job_authority_rev(&job)
        .unwrap()
        .unwrap();
    refused_without_mutation(
        &rig,
        &shared,
        "ana",
        "job_share",
        json!({"job":job,"enabled":true}),
        &job,
        "unbound",
    );
    // Set a genuine share row directly as a fixture prerequisite: otherwise a
    // subscription could be denied for missing sharing rather than binding.
    shared
        .store
        .assist_job_share(&job, ORG, "ana", true)
        .unwrap();
    assert!(shared.store.assist_job_is_shared(&job, ORG).unwrap());
    refused_without_mutation(
        &rig,
        &shared,
        "ben",
        "job_subscribe",
        json!({"job":job}),
        &job,
        "unbound shared",
    );
    refused_without_mutation(
        &rig,
        &shared,
        "ana",
        "job_reconcile",
        json!({"job":job,"expected_authority_rev":pin}),
        &job,
        "unbound",
    );

    // Synthetic transition from a legacy record to a readable project, not a
    // supported rebind API. Restore no-share baseline for signed operations.
    shared
        .store
        .assist_job_share(&job, ORG, "ana", false)
        .unwrap();
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id='VIS-9' WHERE id=?1", [&job])
            .unwrap(),
        1
    );
    let (status, share) =
        mutation_call(&rig, "ana", "job_share", json!({"job":job,"enabled":true}));
    assert_eq!(status, 200, "readable owner share refused: {share}");
    assert!(
        shared.store.assist_job_is_shared(&job, ORG).unwrap(),
        "successful share not committed"
    );
    let (status, sub) = mutation_call(&rig, "ben", "job_subscribe", json!({"job":job}));
    assert_eq!(status, 200, "readable shared subscription refused: {sub}");
    assert!(
        shared.store.job_is_subscriber(&job, ORG, "ben").unwrap(),
        "successful subscription not committed"
    );
    let (status, evidence) = mutation_call(&rig, "ana", "job_evidence", json!({"job":job}));
    assert_eq!(status, 200, "readable owner evidence refused: {evidence}");
    let pin = evidence["authority_rev"]
        .as_str()
        .filter(|s| !s.is_empty())
        .expect("current revision missing")
        .to_string();
    let before_rev = shared
        .store
        .assist_job_authority_rev(&job)
        .unwrap()
        .unwrap();
    let (status, reconcile) = mutation_call(
        &rig,
        "ana",
        "job_reconcile",
        json!({"job":job,"expected_authority_rev":pin}),
    );
    assert_eq!(status, 200, "readable owner reconcile refused: {reconcile}");
    let after_rev = shared
        .store
        .assist_job_authority_rev(&job)
        .unwrap()
        .unwrap();
    assert_ne!(
        before_rev, after_rev,
        "successful reconcile did not update authority revision"
    );
    operator_recovery(&rig, &job);

    // Synthetic transition to an existing private project, not a supported rebind API.
    // A share row and Ben's subscription remain, so denial cannot be
    // attributed to missing ownership or an unshared job.
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id='SEC-1' WHERE id=?1", [&job])
            .unwrap(),
        1
    );
    let pin = shared
        .store
        .assist_job_authority_rev(&job)
        .unwrap()
        .unwrap();
    refused_without_mutation(
        &rig,
        &shared,
        "ana",
        "job_share",
        json!({"job":job,"enabled":false}),
        &job,
        "private binding",
    );
    refused_without_mutation(
        &rig,
        &shared,
        "ben",
        "job_subscribe",
        json!({"job":job}),
        &job,
        "private binding",
    );
    refused_without_mutation(
        &rig,
        &shared,
        "ana",
        "job_reconcile",
        json!({"job":job,"expected_authority_rev":pin}),
        &job,
        "private binding",
    );
    operator_recovery(&rig, &job);
}
