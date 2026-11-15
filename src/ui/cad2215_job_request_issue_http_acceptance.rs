//! CAD-2215 independent signed member job-request acceptance.
//! The product implementer must not edit or weaken this check.
//!
//! Contract: a fresh or replayed valid request is accepted durably and
//! answers the exact `{request_id, status:"accepted"}` receipt, independent
//! of whether the requested issue is free or held. Job publication is the
//! joined autonomous worker's job; the signed owner observes the eventual
//! canonical job through `job_list.requests`. This exercises the real signed
//! `/api/assist/job_request` transport, the real joined worker via the
//! opt-in queue service fixture and its per-request outcome observer, the
//! real store/ledger and the spec tree. It does not establish local-agent
//! OS or filesystem isolation.
//!
//! Fixture assumptions (all real, all created before the calls under test):
//!   * `start_with_queue_service(subs, Some(observer), true)` boots the
//!     signed board, daemon, platform authority and the enrolled
//!     `hosted-board` `assist.revalidate` queue service the promotion
//!     re-proof needs; the observer receives `(request_id, outcome)` from
//!     the real promotion sweep.
//!   * `seed` writes workspace-visible project `visible` (its `VIS-1` is
//!     already occupied by a synthetic nonterminal operator job) and
//!     private project `secret` (`SEC-1`, no member grant).
//!   * This module materializes extra workspace-visible projects on the
//!     real tracker: a readable, job-free issue per accepted request, a
//!     second readable issue for the retarget attempt, an issue another
//!     member occupies first, and a separate project for the positive
//!     control. `create_job` admits one nonterminal job per issue.
//!   * The interrupted-request case pre-creates the deterministic spec
//!     path with sentinel bytes and no job/ledger row; promotion must refuse
//!     without replacing or deleting those bytes.
//!   * The writer-serialization case holds the real `Pm` writer lock while
//!     the request is accepted and the worker attempts promotion, so the
//!     accepted work stays pending (no publication) until the lock is
//!     released; a policy file change emulated under that same held lock is
//!     honored before any job row is written.
//!   * The ambiguous-identity case signs two distinct live members whose
//!     `(sub, request_key)` pairs join to the same colon-delimited bytes
//!     (`"a:b"`+`"c"` vs `"a"`+`"b:c"`) and requires two distinct canonical
//!     jobs on two distinct free issues.

use super::*;

/// The signed member under test.
const MEMBER: &str = "ana";
/// A second signed member used only as an unrelated private-job holder.
const OTHER: &str = "ben";
/// `seed`'s private project issue: it exists but no member grant admits it.
const PRIVATE_ISSUE: &str = "SEC-1";
/// No project declares this issue: it must be as opaque as the private one.
const ABSENT_ISSUE: &str = "VIS-99";
/// A title sentinel that must never surface to a different member.
const HOLDER_TITLE: &str = "CAD2215_HOLDER_PRIVATE_TITLE";
/// The requester's own title, used to prove operator recovery.
const OWNER_TITLE: &str = "CAD2215_OWNER_ISSUE_TITLE";

fn request(rig: &Rig, sub: &str, args: Value) -> (u16, Value) {
    let envelope = rig.envelope(sub, json!({}), json!({}));
    rig.call("job_request", &envelope, &args)
}

/// Materialize one real workspace-visible project/issue on the live
/// fixture tracker. No policy block means the project is workspace-visible
/// (`Project::allows` is true for every member), the shape `seed` uses.
fn readable_issue(rig: &Rig, key: &str, prefix: &str, id: &str) {
    let project = rig.pm.join(key);
    let issue = project.join(id);
    std::fs::create_dir_all(&issue).unwrap();
    std::fs::write(
        project.join("project.yaml"),
        format!("key: {key}\nprefix: {prefix}\n"),
    )
    .unwrap();
    std::fs::write(
        issue.join("issue.md"),
        format!(
            "---\nid: {id}\ntitle: {id} fixture\nstatus: backlog\npriority: P2\n\
             created: '2026-01-01T00:00:00Z'\n---\n"
        ),
    )
    .unwrap();
}

fn fixture_db(rig: &Rig) -> rusqlite::Connection {
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    db
}

/// A whole-publication probe: the real `jobs`/`tasks`/`assist_requests`
/// rows plus every daemon-owned `assist_jobs` spec file. A refused or
/// waiting request must leave this byte-identical; only promotion adds to it.
#[derive(Debug, PartialEq, Eq)]
struct Durable {
    jobs: Vec<(String, Option<String>, String)>,
    tasks: Vec<(String, String)>,
    requests: Vec<(String, String, String)>,
    specs: Vec<(String, Vec<u8>)>,
}

fn durable(rig: &Rig) -> Durable {
    let db = fixture_db(rig);
    let jobs = {
        let mut stmt = db
            .prepare("SELECT id, issue_id, state FROM jobs ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    };
    let tasks = {
        let mut stmt = db
            .prepare("SELECT id, job_id FROM tasks ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    let requests = {
        let mut stmt = db
            .prepare("SELECT sub, request_key, job FROM assist_requests ORDER BY sub, request_key")
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    };
    drop(db);
    let specs = {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(rig._dir.path().join("assist_jobs")) {
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_file() {
                    out.push((
                        path.file_name().unwrap().to_string_lossy().into_owned(),
                        std::fs::read(&path).unwrap(),
                    ));
                }
            }
        }
        out.sort();
        out
    };
    Durable {
        jobs,
        tasks,
        requests,
        specs,
    }
}

/// The durable acceptance rows that are NOT publication: identity fields
/// only, so a sweep updating `last_outcome`/`last_attempt` is not a diff.
fn queued_rows(rig: &Rig) -> Vec<(String, String, String, String, String)> {
    let db = fixture_db(rig);
    let mut stmt = db
        .prepare(
            "SELECT org, sub, request_key, issue, job_id FROM assist_request_queue \
             ORDER BY org, sub, request_key",
        )
        .unwrap();
    stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

fn job_issue(rig: &Rig, job: &str) -> Option<String> {
    fixture_db(rig)
        .query_row("SELECT issue_id FROM jobs WHERE id=?1", [job], |r| {
            r.get::<_, Option<String>>(0)
        })
        .ok()
        .flatten()
}

fn ledger_job(rig: &Rig, sub: &str, key: &str) -> Option<String> {
    fixture_db(rig)
        .query_row(
            "SELECT job FROM assist_requests WHERE org=?1 AND sub=?2 AND request_key=?3",
            rusqlite::params![ORG, sub, key],
            |r| r.get::<_, String>(0),
        )
        .ok()
}

/// The canonical job id the request lane derives from `(org, sub,
/// request_key)`. Every component is length-prefixed
/// (`"<len>:<value>|<len>:<value>|<len>:<value>"`), because both the subject
/// and the request key may contain ':'. This case only needs the
/// deterministic spec filename that id implies, so it can pre-create that
/// exact file before any promotion has run.
fn canonical_job_id(sub: &str, key: &str) -> String {
    let digest = crate::operator_auth::digest(&format!(
        "{}:{}|{}:{}|{}:{}",
        ORG.len(),
        ORG,
        sub.len(),
        sub,
        key.len(),
        key
    ));
    format!("assist-{}", &digest[..digest.len().min(24)])
}

/// The exact acceptance receipt: two fields only, no job and no occupancy.
fn accepted(rig: &Rig, sub: &str, args: Value, phase: &str) -> (Value, String) {
    let (status, body) = request(rig, sub, args);
    assert_eq!(status, 200, "{phase}: signed request refused: {body}");
    let fields = body
        .as_object()
        .unwrap_or_else(|| panic!("{phase}: receipt is not an object: {body}"));
    assert_eq!(
        fields.len(),
        2,
        "{phase}: receipt disclosed extra state: {body}"
    );
    assert_eq!(
        body["status"], "accepted",
        "{phase}: receipt depends on occupancy: {body}"
    );
    assert!(
        body.get("job").is_none(),
        "{phase}: receipt exposed a job id: {body}"
    );
    let id = body["request_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| panic!("{phase}: receipt has no request id: {body}"))
        .to_string();
    (body, id)
}

/// The real joined worker's per-request outcome witness.
#[allow(clippy::type_complexity)] // one fixture channel pair, one shape
fn queue_observer() -> (
    Arc<dyn Fn(&str, &str) + Send + Sync>,
    std::sync::mpsc::Receiver<(String, String)>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let observer = Arc::new(move |id: &str, outcome: &str| {
        let _ = tx.send((id.to_string(), outcome.to_string()));
    });
    (observer, rx)
}

/// Wait for one request's real promotion outcome. Event-driven: the worker
/// emits on the channel; the deadline only bounds a broken fixture. A
/// `pending` re-attempt for the same request is tolerated while waiting for
/// a later outcome.
fn wait_outcome(
    rx: &std::sync::mpsc::Receiver<(String, String)>,
    id: &str,
    want: &str,
    phase: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let remain = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or_else(|| panic!("{phase}: worker outcome deadline for {id}"));
        let (seen, outcome) = rx
            .recv_timeout(remain)
            .unwrap_or_else(|_| panic!("{phase}: joined queue worker emitted no outcome for {id}"));
        if seen == id && outcome == want {
            return;
        }
        assert!(
            seen != id || outcome == "pending",
            "{phase}: unexpected worker outcome for {id}: {outcome}"
        );
    }
}

fn owner_list(rig: &Rig, sub: &str) -> Value {
    let envelope = rig.envelope(sub, json!({}), json!({}));
    let (status, body) = rig.call("job_list", &envelope, &json!({}));
    assert_eq!(
        status, 200,
        "signed owner request list unavailable for {sub}: {body}"
    );
    assert!(body["requests"].is_array(), "request list missing: {body}");
    body
}

fn request_row<'a>(list: &'a Value, id: &str) -> Option<&'a Value> {
    list["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["request_id"] == id)
}

fn created_job(list: &Value, id: &str, phase: &str) -> String {
    let row = request_row(list, id)
        .unwrap_or_else(|| panic!("{phase}: owner request row missing: {list}"));
    assert_eq!(
        row["status"], "created",
        "{phase}: request not created: {row}"
    );
    row["job"]
        .as_str()
        .unwrap_or_else(|| panic!("{phase}: created request has no job: {row}"))
        .to_string()
}

fn operator_cancel(rig: &Rig, job: &str) {
    crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc(
            rig._dir.path(),
            "job_cancel",
            json!({"job": job, "by": "operator"}),
        )
        .unwrap_or_else(|e| panic!("operator recovery cancel of {job} failed: {e}"))
    });
}

fn operator_shows(rig: &Rig, job: &str, phase: &str) -> Value {
    crate::client::rpc(rig._dir.path(), "job_show", json!({"job":job}))
        .unwrap_or_else(|e| panic!("{phase}: proven local operator recovery refused: {e}"))
}

/// A refusal with the expected status, NO publication change and NO new
/// acceptance row.
fn refused_without_effect(rig: &Rig, sub: &str, args: Value, expected: u16, phase: &str) -> Value {
    let before = durable(rig);
    let queued = queued_rows(rig);
    let (status, body) = request(rig, sub, args);
    assert_eq!(
        status, expected,
        "{phase}: expected the contract refusal {expected}, got {status}: {body}"
    );
    assert!(
        !body.get("job").is_some_and(Value::is_string),
        "{phase}: refusal returned a canonical job: {body}"
    );
    assert_eq!(
        durable(rig),
        before,
        "{phase}: a refused request changed jobs/tasks/requests/specs"
    );
    assert_eq!(
        queued_rows(rig),
        queued,
        "{phase}: a refused request wrote an acceptance row"
    );
    body
}

#[test]
fn signed_member_job_request_requires_a_currently_readable_existing_issue() {
    let _flag = AssistantFlagOn::arm();
    let (observer, rx) = queue_observer();
    let rig = start_with_queue_service(&[MEMBER], Some(observer), true);
    seed(&rig);
    readable_issue(&rig, "free", "FREE", "FREE-1");
    readable_issue(&rig, "free", "FREE", "FREE-2");

    // The field is required: an otherwise well-formed request is refused
    // before any acceptance or publication record.
    refused_without_effect(
        &rig,
        MEMBER,
        json!({"title": "CAD2215_ISSUE_REQUIRED", "request_key": "cad2215-issue-required"}),
        400,
        "missing issue",
    );

    // A syntactically valid but absent issue and an existing private issue
    // are indistinguishable: same status, same error body, no echo of the
    // id, and neither an acceptance row nor publication.
    let absent = refused_without_effect(
        &rig,
        MEMBER,
        json!({"title": "CAD2215_ABSENT", "request_key": "cad2215-absent", "issue": ABSENT_ISSUE}),
        403,
        "absent issue",
    );
    let private = refused_without_effect(
        &rig,
        MEMBER,
        json!({"title": "CAD2215_PRIVATE", "request_key": "cad2215-private", "issue": PRIVATE_ISSUE}),
        403,
        "private issue",
    );
    assert_eq!(
        private, absent,
        "private and absent issues must deny with the same opaque body"
    );
    for (phase, body, id) in [
        ("absent", &absent, ABSENT_ISSUE),
        ("private", &private, PRIVATE_ISSUE),
    ] {
        assert!(
            !body.to_string().contains(id),
            "{phase} denial echoed the issue id: {body}"
        );
    }

    // A readable free issue is accepted with the exact receipt, and the
    // joined worker promotes it to a genuinely bound job.
    let before = durable(&rig);
    let (receipt, request_id) = accepted(
        &rig,
        MEMBER,
        json!({
            "title": OWNER_TITLE, "request_key": "cad2215-owner-issue", "issue": "FREE-1",
            "body": "bound request"
        }),
        "readable issue",
    );
    assert_eq!(
        queued_rows(&rig).len(),
        1,
        "acceptance row not written exactly once"
    );
    wait_outcome(&rx, &request_id, "created", "readable issue");
    let list = owner_list(&rig, MEMBER);
    let job = created_job(&list, &request_id, "readable issue");
    assert_eq!(
        job_issue(&rig, &job).as_deref(),
        Some("FREE-1"),
        "job not bound to the issue"
    );
    assert_eq!(
        ledger_job(&rig, MEMBER, "cad2215-owner-issue").as_deref(),
        Some(job.as_str())
    );
    let after = durable(&rig);
    assert_eq!(
        after.jobs.len(),
        before.jobs.len() + 1,
        "job row not created exactly once"
    );
    assert_eq!(
        after.tasks.len(),
        before.tasks.len() + 1,
        "draft task not created exactly once"
    );
    assert_eq!(
        after.requests.len(),
        before.requests.len() + 1,
        "request ledger not written exactly once"
    );
    assert_eq!(
        after.specs.len(),
        before.specs.len() + 1,
        "spec not written exactly once"
    );
    assert!(
        list.to_string().contains(&job),
        "owner list omitted the bound job: {list}"
    );
    let shown = operator_shows(&rig, &job, "created job");
    assert_eq!(shown["job"]["id"].as_str(), Some(job.as_str()));
    assert_eq!(shown["job"]["issue"].as_str(), Some("FREE-1"));

    // Same key + same issue is a replay: the identical receipt is returned
    // and nothing durable is duplicated.
    let before_replay = durable(&rig);
    let queued_replay = queued_rows(&rig);
    let (replay, replay_id) = accepted(
        &rig,
        MEMBER,
        json!({
            "title": OWNER_TITLE, "request_key": "cad2215-owner-issue", "issue": "FREE-1",
            "body": "bound request"
        }),
        "same-issue replay",
    );
    assert_eq!(replay, receipt, "replay changed the receipt");
    assert_eq!(replay_id, request_id, "replay changed the request id");
    assert_eq!(
        durable(&rig),
        before_replay,
        "same-issue replay duplicated publication"
    );
    assert_eq!(
        queued_rows(&rig),
        queued_replay,
        "same-issue replay added a request row"
    );

    // Same key + a DIFFERENT readable issue must not retarget the recorded
    // request. FREE-2 is free and readable, so the refusal is about the
    // retarget, and the original binding must be untouched.
    let retarget = refused_without_effect(
        &rig,
        MEMBER,
        json!({
            "title": "CAD2215_RETARGET", "request_key": "cad2215-owner-issue", "issue": "FREE-2"
        }),
        403,
        "issue retarget",
    );
    assert_eq!(
        job_issue(&rig, &job).as_deref(),
        Some("FREE-1"),
        "retarget refusal changed the job binding"
    );
    assert_eq!(
        ledger_job(&rig, MEMBER, "cad2215-owner-issue").as_deref(),
        Some(job.as_str())
    );
    assert!(
        !retarget.to_string().contains(&job),
        "retarget refusal disclosed the recorded job: {retarget}"
    );
}

#[test]
fn occupied_readable_issue_stays_pending_and_promotes_after_release() {
    let _flag = AssistantFlagOn::arm();
    let (observer, rx) = queue_observer();
    let rig = start_with_queue_service(&[MEMBER, OTHER], Some(observer), true);
    seed(&rig);
    readable_issue(&rig, "busy", "BUSY", "BUSY-1");
    readable_issue(&rig, "free", "FREE", "FREE-1");

    // The other member occupies the readable issue with a private job. Its
    // title never leaves that member's request.
    let (_, holder_id) = accepted(
        &rig,
        OTHER,
        json!({
            "title": HOLDER_TITLE, "request_key": "cad2215-holder-key", "issue": "BUSY-1",
            "body": "holder private body"
        }),
        "holder request",
    );
    wait_outcome(&rx, &holder_id, "created", "holder request");
    let holder_list = owner_list(&rig, OTHER);
    let holder = created_job(&holder_list, &holder_id, "holder request");
    assert_eq!(job_issue(&rig, &holder).as_deref(), Some("BUSY-1"));
    assert_eq!(
        ledger_job(&rig, OTHER, "cad2215-holder-key").as_deref(),
        Some(holder.as_str())
    );
    assert_eq!(ledger_job(&rig, MEMBER, "cad2215-holder-key"), None);

    // A different member's request on the same readable issue is ACCEPTED
    // with the same occupancy-independent receipt and then waits: the
    // receipt and the owner row disclose no holder job and no title.
    let before = durable(&rig);
    let (waiter_receipt, waiter_id) = accepted(
        &rig,
        MEMBER,
        json!({
            "title": "CAD2215_REQUESTER_TITLE", "request_key": "cad2215-occupied-key",
            "issue": "BUSY-1", "body": "requester body"
        }),
        "occupied issue",
    );
    assert_ne!(holder_id, waiter_id, "two requests shared one request id");
    assert_eq!(
        waiter_receipt["status"], "accepted",
        "occupied issue changed the receipt: {waiter_receipt}"
    );
    assert_eq!(
        durable(&rig),
        before,
        "the waiting acceptance published a job/spec/ledger"
    );
    wait_outcome(&rx, &waiter_id, "pending", "occupied issue");
    let ana = owner_list(&rig, MEMBER);
    let waiting = request_row(&ana, &waiter_id)
        .unwrap_or_else(|| panic!("owner cannot inspect its pending request: {ana}"));
    assert_eq!(waiting["issue"], "BUSY-1");
    assert_eq!(waiting["status"], "pending");
    assert!(
        waiting["job"].is_null(),
        "pending request already has a job: {waiting}"
    );
    assert!(
        request_row(&ana, &holder_id).is_none(),
        "requester saw the holder's request: {ana}"
    );
    for secret in [holder.as_str(), HOLDER_TITLE] {
        assert!(
            !ana.to_string().contains(secret),
            "pending owner list disclosed the holder: {ana}"
        );
    }
    let ben = owner_list(&rig, OTHER);
    assert!(
        request_row(&ben, &waiter_id).is_none(),
        "holder saw the other member's request: {ben}"
    );

    // Release the blocker through a genuine operator call; the joined
    // worker then promotes the waiting request without any list/replay
    // causing it.
    operator_cancel(&rig, &holder);
    wait_outcome(&rx, &waiter_id, "created", "after release");
    let ana_after = owner_list(&rig, MEMBER);
    let job = created_job(&ana_after, &waiter_id, "after release");
    assert_ne!(job, holder, "promotion reused the holder's job");
    assert_eq!(job_issue(&rig, &job).as_deref(), Some("BUSY-1"));
    assert_eq!(
        ledger_job(&rig, MEMBER, "cad2215-occupied-key").as_deref(),
        Some(job.as_str())
    );
    let active: i64 = fixture_db(&rig)
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE issue_id='BUSY-1' \
             AND state NOT IN ('done','cancelled','failed')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(active, 1, "queue created two active jobs on one issue");
    for id in [holder.as_str(), job.as_str()] {
        let shown = operator_shows(&rig, id, "operator recovery");
        assert_eq!(shown["job"]["id"].as_str(), Some(id));
    }
    assert_eq!(
        operator_shows(&rig, &holder, "holder recovery")["job"]["issue"].as_str(),
        Some("BUSY-1")
    );
}

#[test]
fn revoked_project_authority_before_promotion_fails_closed() {
    let _flag = AssistantFlagOn::arm();
    let (observer, rx) = queue_observer();
    let rig = start_with_queue_service(&[MEMBER], Some(observer), true);
    seed(&rig);
    readable_issue(&rig, "free", "FREE", "FREE-1");
    readable_issue(&rig, "other", "OTHER", "OTHER-1");

    // Hold the real tracker writer lock so the accepted request cannot be
    // published while the revocation is committed under that same lock.
    let pm = crate::issue::Pm::at(&rig.pm).unwrap();
    let lock = pm
        .lock()
        .expect("fixture could not hold the tracker writer lock");
    let before = durable(&rig);
    let (receipt, request_id) = accepted(
        &rig,
        MEMBER,
        json!({
            "title": OWNER_TITLE, "request_key": "cad2215-revoked-issue", "issue": "FREE-1",
            "body": "before revocation"
        }),
        "pre-revocation acceptance",
    );
    assert_eq!(
        durable(&rig),
        before,
        "acceptance under the writer lock published a job/spec/ledger"
    );
    wait_outcome(&rx, &request_id, "pending", "writer lock held");
    let held = owner_list(&rig, MEMBER);
    let row = request_row(&held, &request_id).expect("owner lost its accepted request");
    assert_eq!(row["status"], "pending");
    assert!(row["job"].is_null());

    // The policy file change emulated under the held writer lock revokes the
    // member's access; promotion must honor it before any job row exists.
    std::fs::write(
        rig.pm.join("free/project.yaml"),
        "key: free\nprefix: FREE\npolicy:\n  version: 2\n  visibility: private\n",
    )
    .unwrap();
    drop(lock);
    wait_outcome(&rx, &request_id, "refused", "post-revocation promotion");
    assert_eq!(
        durable(&rig),
        before,
        "a revoked promotion published a job/spec/ledger"
    );
    assert_eq!(
        request_row(&owner_list(&rig, MEMBER), &request_id),
        None,
        "owner still reads a request for a revoked issue"
    );
    assert!(
        !owner_list(&rig, MEMBER).to_string().contains(OWNER_TITLE),
        "revoked issue list disclosed the request title"
    );

    // A replay of the same key+issue after revocation is an opaque denial
    // that neither names a job nor the old title.
    let replay = refused_without_effect(
        &rig,
        MEMBER,
        json!({
            "title": OWNER_TITLE, "request_key": "cad2215-revoked-issue", "issue": "FREE-1",
            "body": "after revocation"
        }),
        403,
        "revoked replay",
    );
    assert!(
        !replay.to_string().contains(OWNER_TITLE),
        "revoked replay disclosed the recorded title: {replay}"
    );
    assert!(
        !replay.to_string().contains("assist-"),
        "revoked replay disclosed a job id: {replay}"
    );

    // A fresh key on the now-unreadable issue is refused the same way.
    refused_without_effect(
        &rig,
        MEMBER,
        json!({
            "title": "CAD2215_REVOKED_NEW", "request_key": "cad2215-revoked-new",
            "issue": "FREE-1"
        }),
        403,
        "revoked new request",
    );

    // Operator recovery of an EXISTING job stays intact: a different
    // readable project still accepts, promotes and is operator-visible.
    let (_, other_id) = accepted(
        &rig,
        MEMBER,
        json!({
            "title": OWNER_TITLE, "request_key": "cad2215-revoked-other",
            "issue": "OTHER-1", "body": "other project"
        }),
        "readable other project",
    );
    wait_outcome(&rx, &other_id, "created", "readable other project");
    let job = created_job(
        &owner_list(&rig, MEMBER),
        &other_id,
        "readable other project",
    );
    assert_eq!(job_issue(&rig, &job).as_deref(), Some("OTHER-1"));
    let shown = operator_shows(&rig, &job, "post-revocation operator recovery");
    assert_eq!(shown["job"]["id"].as_str(), Some(job.as_str()));
    assert_eq!(shown["job"]["issue"].as_str(), Some("OTHER-1"));
    assert_eq!(shown["job"]["title"].as_str(), Some(OWNER_TITLE));
    assert_eq!(receipt["status"], "accepted");
}

/// An interrupted earlier promotion can leave the deterministic spec file
/// behind with no job or ledger row. The accepted request's promotion must
/// fail closed without replacing or deleting those bytes and without
/// creating a job, task or ledger row.
#[test]
fn preexisting_orphan_spec_bytes_are_not_replaced_or_deleted() {
    let _flag = AssistantFlagOn::arm();
    let (observer, rx) = queue_observer();
    let rig = start_with_queue_service(&[MEMBER], Some(observer), true);
    seed(&rig);
    readable_issue(&rig, "free", "FREE", "FREE-1");

    let key = "cad2215-orphan-key";
    let planned = canonical_job_id(MEMBER, key);
    let spec = rig
        ._dir
        .path()
        .join("assist_jobs")
        .join(format!("{planned}.md"));
    std::fs::create_dir_all(spec.parent().unwrap()).unwrap();
    let sentinel = b"ORPHANED INTERRUPTED SPEC BYTES\n";
    std::fs::write(&spec, sentinel).unwrap();

    let before = durable(&rig);
    let (_, request_id) = accepted(
        &rig,
        MEMBER,
        json!({
            "title": "CAD2215_ORPHAN_TITLE", "request_key": key, "issue": "FREE-1",
            "body": "orphan body"
        }),
        "orphan spec request",
    );
    wait_outcome(&rx, &request_id, "refused", "orphan spec promotion");
    assert_eq!(
        std::fs::read(&spec).unwrap(),
        sentinel.to_vec(),
        "the orphan spec bytes were replaced or deleted"
    );
    assert_eq!(
        durable(&rig),
        before,
        "the orphan-spec promotion changed jobs/tasks/requests/specs"
    );
    assert_eq!(
        job_issue(&rig, &planned),
        None,
        "a job row was created for the orphan canonical id"
    );
    assert_eq!(
        ledger_job(&rig, MEMBER, key),
        None,
        "a request ledger row was created for the orphan key"
    );
    let list = owner_list(&rig, MEMBER);
    let row = request_row(&list, &request_id).expect("owner lost its refused request");
    assert_eq!(row["status"], "pending");
    assert!(row["job"].is_null());
    assert_eq!(
        row["reason"], "promotion_unavailable",
        "refused promotion reason is not truthful: {row}"
    );
}

/// Colon-delimited concatenation would alias two different `(sub,
/// request_key)` pairs (`"a:b"`+`"c"` and `"a"`+`"b:c"`) onto the same bytes
/// under one org. Both are genuine, live members with distinct free issues,
/// so both requests must be accepted and promoted to their OWN canonical
/// job with different ids.
#[test]
fn colon_ambiguous_identity_pairs_get_distinct_canonical_jobs() {
    let _flag = AssistantFlagOn::arm();
    let (observer, rx) = queue_observer();
    let rig = start_with_queue_service(&[], Some(observer), true);
    seed(&rig);
    readable_issue(&rig, "free", "FREE", "FREE-1");
    readable_issue(&rig, "free", "FREE", "FREE-2");

    // The subject is server-derived as `assist_<sub>`, so the envelope keeps
    // the ':' form; only the registered agent alias must satisfy the alias
    // grammar, so each member is bound under a distinct valid alias.
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    for (sub, alias) in [("a:b", "assist-agent-a-b"), ("a", "assist-agent-a")] {
        shared
            .store
            .assistant_bind(ORG, sub, &format!("assist_{sub}"), alias, Some("rev0"))
            .unwrap_or_else(|e| panic!("binding ambiguous member {sub}: {e}"));
        rig.authority
            .lock()
            .unwrap()
            .members
            .insert(sub.to_string(), MemberAuthority::live(sub));
    }

    let (_, first_id) = accepted(
        &rig,
        "a:b",
        json!({
            "title": "CAD2215_AMBIGUOUS_ONE", "request_key": "c", "issue": "FREE-1"
        }),
        "ambiguous member one",
    );
    wait_outcome(&rx, &first_id, "created", "ambiguous member one");
    let (_, second_id) = accepted(
        &rig,
        "a",
        json!({
            "title": "CAD2215_AMBIGUOUS_TWO", "request_key": "b:c", "issue": "FREE-2"
        }),
        "ambiguous member two",
    );
    assert_ne!(
        first_id, second_id,
        "two identities received one request id"
    );
    wait_outcome(&rx, &second_id, "created", "ambiguous member two");
    let first = created_job(&owner_list(&rig, "a:b"), &first_id, "ambiguous member one");
    let second = created_job(&owner_list(&rig, "a"), &second_id, "ambiguous member two");
    assert_ne!(
        first, second,
        "colon-ambiguous identities collapsed onto one canonical job"
    );
    assert_eq!(job_issue(&rig, &first).as_deref(), Some("FREE-1"));
    assert_eq!(job_issue(&rig, &second).as_deref(), Some("FREE-2"));
    assert_eq!(
        ledger_job(&rig, "a:b", "c").as_deref(),
        Some(first.as_str()),
        "first member's request ledger does not name its own job"
    );
    assert_eq!(
        ledger_job(&rig, "a", "b:c").as_deref(),
        Some(second.as_str()),
        "second member's request ledger does not name its own job"
    );
    // Neither owner's list exposes the other's request or job.
    assert!(request_row(&owner_list(&rig, "a:b"), &second_id).is_none());
    assert!(request_row(&owner_list(&rig, "a"), &first_id).is_none());
    for (id, issue) in [(&first, "FREE-1"), (&second, "FREE-2")] {
        let shown = operator_shows(&rig, id, "ambiguous operator recovery");
        assert_eq!(shown["job"]["id"].as_str(), Some(id.as_str()));
        assert_eq!(shown["job"]["issue"].as_str(), Some(issue));
    }
}

/// Accepted work is durable before publication. While the test holds the
/// real PM writer lock, the request still gets the exact accepted receipt
/// and the owner sees it pending with no job; publication happens only after
/// the lock is released. A different readable project still admits.
#[test]
fn writer_lock_serializes_publication_until_released() {
    let _flag = AssistantFlagOn::arm();
    let (observer, rx) = queue_observer();
    let rig = start_with_queue_service(&[MEMBER], Some(observer), true);
    seed(&rig);
    readable_issue(&rig, "free", "FREE", "FREE-1");
    readable_issue(&rig, "other", "OTHER", "OTHER-1");

    let pm = crate::issue::Pm::at(&rig.pm).unwrap();
    let lock = pm
        .lock()
        .expect("fixture could not hold the tracker writer lock");
    let before = durable(&rig);
    let (receipt, request_id) = accepted(
        &rig,
        MEMBER,
        json!({
            "title": OWNER_TITLE, "request_key": "cad2215-writer-serialized",
            "issue": "FREE-1", "body": "writer serialization"
        }),
        "writer-serialized acceptance",
    );
    assert_eq!(receipt["status"], "accepted");
    assert_eq!(
        durable(&rig),
        before,
        "acceptance under the writer lock published a job/spec/ledger"
    );
    wait_outcome(&rx, &request_id, "pending", "writer lock held");
    let held = owner_list(&rig, MEMBER);
    let row = request_row(&held, &request_id).expect("owner lost its accepted request");
    assert_eq!(row["status"], "pending");
    assert!(row["job"].is_null());
    assert_eq!(
        durable(&rig),
        before,
        "a pending serialized request published a job/spec/ledger"
    );
    drop(lock);
    wait_outcome(&rx, &request_id, "created", "writer lock released");
    let job = created_job(
        &owner_list(&rig, MEMBER),
        &request_id,
        "writer lock released",
    );
    assert_eq!(job_issue(&rig, &job).as_deref(), Some("FREE-1"));
    let shown = operator_shows(&rig, &job, "writer-lock recovery");
    assert_eq!(shown["job"]["id"].as_str(), Some(job.as_str()));

    // The refusal/queue path is not a blanket denial: a different readable
    // project still accepts and promotes.
    let (_, other_id) = accepted(
        &rig,
        MEMBER,
        json!({
            "title": OWNER_TITLE, "request_key": "cad2215-writer-other",
            "issue": "OTHER-1", "body": "other project"
        }),
        "readable other project",
    );
    wait_outcome(&rx, &other_id, "created", "readable other project");
    let other = created_job(
        &owner_list(&rig, MEMBER),
        &other_id,
        "readable other project",
    );
    assert_eq!(job_issue(&rig, &other).as_deref(), Some("OTHER-1"));
}
