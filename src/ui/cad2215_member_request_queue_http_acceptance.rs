//! CAD-2215 independent signed member request-queue acceptance.
//! Exercises HTTP, daemon and durable store; not local-agent OS isolation.

use super::*;
use std::sync::Condvar;

fn queue_request(rig: &Rig, sub: &str, issue: &str, key: &str) -> Value {
    let envelope = rig.envelope(sub, json!({}), json!({}));
    let (status, body) = rig.call(
        "job_request",
        &envelope,
        &json!({
            "title":format!("queue request {key}"), "request_key":key, "issue":issue,
            "body":format!("QUEUE_BODY_{key}")
        }),
    );
    assert_eq!(status, 200, "signed accepted request {key} refused: {body}");
    let fields = body
        .as_object()
        .expect("request response must be an object");
    assert_eq!(
        fields.len(),
        2,
        "request response disclosed extra state: {body}"
    );
    assert_eq!(
        body["status"], "accepted",
        "request response depends on occupancy: {body}"
    );
    assert!(
        body["request_id"].as_str().is_some_and(|id| !id.is_empty()),
        "accepted request has no request ID: {body}"
    );
    assert!(
        body.get("job").is_none(),
        "request response exposed job ID: {body}"
    );
    body
}

fn queue_list(rig: &Rig, sub: &str) -> Value {
    let envelope = rig.envelope(sub, json!({}), json!({}));
    let (status, body) = rig.call("job_list", &envelope, &json!({}));
    assert_eq!(
        status, 200,
        "signed owner request list unavailable for {sub}: {body}"
    );
    assert!(body["requests"].is_array(), "request list missing: {body}");
    body
}

fn queue_row<'a>(body: &'a Value, id: &str) -> Option<&'a Value> {
    body["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["request_id"] == id)
}

#[test]
fn signed_request_shape_and_owner_pending_visibility_do_not_disclose_occupancy() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&["ana", "ben"]);
    seed(&rig);
    // seed's actual operator job occupies VIS-1. VIS-9 is a separate,
    // workspace-readable issue with no nonterminal job.
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    let count = |issue: &str| -> i64 {
        db.query_row("SELECT COUNT(*) FROM jobs WHERE issue_id=?1 AND state NOT IN ('done','cancelled','failed')", [issue], |row| row.get(0)).unwrap()
    };
    assert_eq!(count("VIS-1"), 1, "occupied control must be real");
    assert_eq!(count("VIS-9"), 0, "free control must be genuinely free");
    let occupied = queue_request(&rig, "ana", "VIS-1", "queue-occupied");
    let free = queue_request(&rig, "ana", "VIS-9", "queue-free");
    assert_eq!(occupied["status"], free["status"]);
    let occupied_id = occupied["request_id"].as_str().unwrap().to_string();
    let free_id = free["request_id"].as_str().unwrap().to_string();
    assert_ne!(occupied_id, free_id);
    assert_eq!(
        queue_request(&rig, "ana", "VIS-1", "queue-occupied"),
        occupied,
        "occupied replay changed receipt"
    );
    assert_eq!(
        queue_request(&rig, "ana", "VIS-9", "queue-free"),
        free,
        "free replay changed receipt"
    );
    let owner = queue_list(&rig, "ana");
    let pending =
        queue_row(&owner, &occupied_id).expect("owner cannot inspect its pending request");
    assert_eq!(pending["issue"], "VIS-1");
    assert_eq!(pending["status"], "pending");
    assert!(
        pending["job"].is_null(),
        "pending request already has a job: {pending}"
    );
    let free_row = queue_row(&owner, &free_id).expect("owner lost its accepted free request");
    assert_eq!(free_row["issue"], "VIS-9");
    assert!(
        matches!(free_row["status"].as_str(), Some("pending" | "created")),
        "unexpected request state: {free_row}"
    );
    if free_row["status"] == "pending" {
        assert!(free_row["job"].is_null());
    }
    let other = queue_list(&rig, "ben");
    assert!(
        queue_row(&other, &occupied_id).is_none(),
        "other member saw pending request: {other}"
    );
    assert!(
        queue_row(&other, &free_id).is_none(),
        "other member saw free request: {other}"
    );
    assert!(!other.to_string().contains("QUEUE_BODY_queue-occupied"));
    assert_eq!(
        count("VIS-1"),
        1,
        "pending request created a second nonterminal job"
    );
    assert!(count("VIS-9") <= 1, "free request created duplicate jobs");
}

fn queue_worker_observer() -> (
    Arc<dyn Fn(&str, &str) + Send + Sync>,
    std::sync::mpsc::Receiver<(String, String)>,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    let observer = Arc::new(move |id: &str, outcome: &str| {
        let _ = tx.send((id.to_string(), outcome.to_string()));
    });
    (observer, rx)
}

fn queue_wait_outcome(rx: &std::sync::mpsc::Receiver<(String, String)>, id: &str, want: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let remain = deadline
            .checked_duration_since(std::time::Instant::now())
            .expect("worker outcome deadline");
        let (seen, outcome) = rx
            .recv_timeout(remain)
            .expect("real joined queue worker emitted no expected outcome");
        if seen == id && outcome == want {
            return;
        }
        assert!(
            seen != id || outcome == "pending",
            "unexpected real worker outcome for {id}: {outcome}"
        );
    }
}

#[test]
fn private_holder_does_not_change_receipt_and_real_worker_promotes_after_release() {
    let _flag = AssistantFlagOn::arm();
    let (observer, rx) = queue_worker_observer();
    let rig = start_with_queue_service(&["ana", "ben"], Some(observer), true);
    seed(&rig);
    let holder = queue_request(&rig, "ben", "VIS-9", "queue-private-holder");
    let holder_id = holder["request_id"].as_str().unwrap().to_string();
    queue_wait_outcome(&rx, &holder_id, "created");
    let ben_list = queue_list(&rig, "ben");
    let ben_job = queue_row(&ben_list, &holder_id).unwrap()["job"]
        .as_str()
        .unwrap()
        .to_string();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    assert!(shared
        .store
        .assist_request_owns(ORG, "ben", &ben_job)
        .unwrap());
    assert!(!shared
        .store
        .assist_request_owns(ORG, "ana", &ben_job)
        .unwrap());

    let waiting = queue_request(&rig, "ana", "VIS-9", "queue-private-waiter");
    let waiting_id = waiting["request_id"].as_str().unwrap().to_string();
    assert_ne!(holder_id, waiting_id);
    queue_wait_outcome(&rx, &waiting_id, "pending");
    let ana_before = queue_list(&rig, "ana");
    let pending = queue_row(&ana_before, &waiting_id).expect("owner request missing");
    assert_eq!(pending["status"], "pending");
    assert!(pending["job"].is_null());
    assert!(
        !ana_before.to_string().contains(&ben_job),
        "waiter learned private holder"
    );
    assert!(queue_row(&ana_before, &holder_id).is_none());
    assert_eq!(
        queue_request(&rig, "ana", "VIS-9", "queue-private-waiter"),
        waiting
    );

    // Genuine operator daemon RPC ends the private blocker. No list or replay
    // is used to cause the waiting request's promotion.
    crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc(
            rig._dir.path(),
            "job_cancel",
            json!({"job":ben_job,"by":"operator"}),
        )
        .expect("operator recovery cancel failed")
    });
    queue_wait_outcome(&rx, &waiting_id, "created");
    let after = queue_list(&rig, "ana");
    let row = queue_row(&after, &waiting_id).expect("promoted owner request missing");
    assert_eq!(row["status"], "created");
    let ana_job = row["job"].as_str().expect("created owner job");
    assert_ne!(ana_job, ben_job);
    assert!(shared
        .store
        .assist_request_owns(ORG, "ana", ana_job)
        .unwrap());
    assert_eq!(
        queue_request(&rig, "ana", "VIS-9", "queue-private-waiter"),
        waiting
    );
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    let active: i64 = db.query_row("SELECT COUNT(*) FROM jobs WHERE issue_id=?1 AND state NOT IN ('done','cancelled','failed')", ["VIS-9"], |r| r.get(0)).unwrap();
    assert_eq!(active, 1, "queue created two active issue jobs");
}

#[test]
fn autonomous_worker_refuses_revoked_original_credential_then_recovers_same_request() {
    let _flag = AssistantFlagOn::arm();
    let (observer, rx) = queue_worker_observer();
    let rig = start_with_queue_service(&["ana", "ben"], Some(observer), true);
    seed(&rig);
    let holder = queue_request(&rig, "ben", "VIS-9", "queue-revoke-holder");
    let holder_id = holder["request_id"].as_str().unwrap().to_string();
    queue_wait_outcome(&rx, &holder_id, "created");
    let ben_list = queue_list(&rig, "ben");
    let ben_job = queue_row(&ben_list, &holder_id).unwrap()["job"]
        .as_str()
        .unwrap()
        .to_string();
    let waiting = queue_request(&rig, "ana", "VIS-9", "queue-revoke-waiter");
    let waiting_id = waiting["request_id"].as_str().unwrap().to_string();
    queue_wait_outcome(&rx, &waiting_id, "pending");
    rig.authority
        .lock()
        .unwrap()
        .members
        .get_mut("ana")
        .unwrap()
        .credential_live = false;
    crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc(
            rig._dir.path(),
            "job_cancel",
            json!({"job":ben_job,"by":"operator"}),
        )
        .expect("operator recovery cancel failed")
    });
    queue_wait_outcome(&rx, &waiting_id, "refused");
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    assert!(
        shared
            .store
            .assist_request_job(ORG, "ana", "queue-revoke-waiter")
            .unwrap()
            .is_none(),
        "revoked request acquired a job ledger"
    );
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    let active: i64 = db.query_row("SELECT COUNT(*) FROM jobs WHERE issue_id=?1 AND state NOT IN ('done','cancelled','failed')", ["VIS-9"], |r| r.get(0)).unwrap();
    assert_eq!(active, 0, "revoked queued credential created a job");
    let op = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc(rig._dir.path(), "job_show", json!({"job":ben_job}))
            .expect("operator lost recovery access")
    });
    assert!(op.to_string().contains(&ben_job));
    rig.authority
        .lock()
        .unwrap()
        .members
        .get_mut("ana")
        .unwrap()
        .credential_live = true;
    queue_wait_outcome(&rx, &waiting_id, "created");
    let list = queue_list(&rig, "ana");
    let row = queue_row(&list, &waiting_id).unwrap();
    assert_eq!(row["status"], "created");
    let job = row["job"].as_str().unwrap();
    assert!(shared.store.assist_request_owns(ORG, "ana", job).unwrap());
    assert_eq!(
        queue_request(&rig, "ana", "VIS-9", "queue-revoke-waiter"),
        waiting
    );
}

#[test]
fn concurrent_private_requests_share_issue_without_double_active_jobs() {
    let _flag = AssistantFlagOn::arm();
    let (observer, rx) = queue_worker_observer();
    let rig = start_with_queue_service(&["ana", "ben"], Some(observer), true);
    seed(&rig);
    let barrier = std::sync::Barrier::new(2);
    let (ana, ben) = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            queue_request(&rig, "ana", "VIS-9", "queue-concurrent-ana")
        });
        let b = scope.spawn(|| {
            barrier.wait();
            queue_request(&rig, "ben", "VIS-9", "queue-concurrent-ben")
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    let aid = ana["request_id"].as_str().unwrap().to_string();
    let bid = ben["request_id"].as_str().unwrap().to_string();
    assert_ne!(aid, bid);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut winner = None::<String>;
    let mut pending = std::collections::HashSet::new();
    while winner.is_none() || !pending.iter().any(|id| Some(id) != winner.as_ref()) {
        let remain = deadline
            .checked_duration_since(std::time::Instant::now())
            .expect("concurrent queue outcome deadline");
        let (id, outcome) = rx
            .recv_timeout(remain)
            .expect("worker did not process both requests");
        if id != aid && id != bid {
            continue;
        }
        match outcome.as_str() {
            "created" => {
                if let Some(existing) = &winner {
                    assert_eq!(existing, &id, "two concurrent requests created active jobs");
                } else {
                    winner = Some(id);
                }
            }
            "pending" => {
                pending.insert(id);
            }
            other => panic!("valid concurrent request unexpectedly {other}"),
        }
    }
    let winner = winner.unwrap();
    let (winner_sub, winner_receipt, loser_sub, loser_receipt) = if winner == aid {
        ("ana", &ana, "ben", &ben)
    } else {
        ("ben", &ben, "ana", &ana)
    };
    let loser = loser_receipt["request_id"].as_str().unwrap();
    let wlist = queue_list(&rig, winner_sub);
    let wrow = queue_row(&wlist, &winner).unwrap();
    assert_eq!(wrow["status"], "created");
    let winner_job = wrow["job"].as_str().unwrap().to_string();
    let llist = queue_list(&rig, loser_sub);
    let lrow = queue_row(&llist, loser).unwrap();
    assert_eq!(lrow["status"], "pending");
    assert!(lrow["job"].is_null());
    assert!(!llist.to_string().contains(&winner_job));
    assert!(queue_row(&llist, &winner).is_none());
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    let count_active = || -> i64 {
        db.query_row("SELECT COUNT(*) FROM jobs WHERE issue_id=?1 AND state NOT IN ('done','cancelled','failed')", ["VIS-9"], |r| r.get(0)).unwrap()
    };
    assert_eq!(count_active(), 1);
    crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc(
            rig._dir.path(),
            "job_cancel",
            json!({"job":winner_job,"by":"operator"}),
        )
        .unwrap()
    });
    queue_wait_outcome(&rx, loser, "created");
    let after = queue_list(&rig, loser_sub);
    let row = queue_row(&after, loser).unwrap();
    assert_eq!(row["status"], "created");
    assert_ne!(row["job"].as_str().unwrap(), winner_job);
    assert_eq!(count_active(), 1);
    let (akey, bkey) = ("queue-concurrent-ana", "queue-concurrent-ben");
    assert_eq!(queue_request(&rig, "ana", "VIS-9", akey), ana);
    assert_eq!(queue_request(&rig, "ben", "VIS-9", bkey), ben);
    assert_eq!(winner_receipt["request_id"], winner);
}

// ---- CAD-2215: an interrupted private promotion is not ordinary or complete --

const QUEUE_OWNER: &str = "ana";
const QUEUE_SUBSCRIBER: &str = "ben";
const QUEUE_UNRELATED: &str = "cy";
const QUEUE_ISSUE: &str = "VIS-9";
const QUEUE_KEY: &str = "cad2215-queue-interrupted";
const QUEUE_TITLE: &str = "CAD2215_QUEUE_PRIVATE_TITLE";
const QUEUE_BODY: &str = "CAD2215_QUEUE_PRIVATE_BODY";
const QUEUE_MARKER: &str = "CAD2215_QUEUE_PRIVATE_MESSAGE";

/// Park the real joined worker at the existing per-request outcome observer
/// AFTER it has committed `created`. The first blocks until released; later
/// outcomes pass through, so a genuine recovery afterward is still observed.
fn queue_held_observer() -> (
    Arc<dyn Fn(&str, &str) + Send + Sync>,
    std::sync::mpsc::Receiver<(String, String)>,
    Arc<(std::sync::Mutex<bool>, Condvar)>,
) {
    let (tx, rx) = std::sync::mpsc::channel::<(String, String)>();
    let released: Arc<(std::sync::Mutex<bool>, Condvar)> =
        Arc::new((std::sync::Mutex::new(false), Condvar::new()));
    let parked = Arc::new(std::sync::Mutex::new(false));
    let worker_released = Arc::clone(&released);
    let worker_parked = Arc::clone(&parked);
    let observer = Arc::new(move |id: &str, outcome: &str| {
        let _ = tx.send((id.to_string(), outcome.to_string()));
        if outcome != "created" {
            return;
        }
        let first = {
            let mut parked = worker_parked.lock().unwrap();
            let first = !*parked;
            *parked = true;
            first
        };
        if first {
            let (lock, cv) = &*worker_released;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = cv
                    .wait_timeout(released, Duration::from_secs(90))
                    .expect("held promotion observer released")
                    .0;
            }
        }
    });
    (observer, rx, released)
}

fn queue_release(released: &Arc<(std::sync::Mutex<bool>, Condvar)>) {
    *released.0.lock().unwrap() = true;
    released.1.notify_all();
}

fn queue_accept(rig: &Rig, sub: &str, key: &str, issue: &str, title: &str, body: &str) -> Value {
    let envelope = rig.envelope(sub, json!({}), json!({}));
    let (status, receipt) = rig.call(
        "job_request",
        &envelope,
        &json!({"title": title, "request_key": key, "issue": issue, "body": body}),
    );
    assert_eq!(
        status, 200,
        "signed accepted request {key} refused: {receipt}"
    );
    let fields = receipt
        .as_object()
        .expect("request response must be an object");
    assert_eq!(
        fields.len(),
        2,
        "request response disclosed extra state: {receipt}"
    );
    assert_eq!(
        receipt["status"], "accepted",
        "request response depends on occupancy: {receipt}"
    );
    assert!(
        receipt["request_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "accepted request has no request ID: {receipt}"
    );
    assert!(
        receipt.get("job").is_none(),
        "request response exposed job ID: {receipt}"
    );
    receipt
}

fn queue_owner_created(rig: &Rig, id: &str) -> String {
    let list = queue_list(rig, QUEUE_OWNER);
    let row = queue_row(&list, id).expect("owner lost its created request");
    assert_eq!(
        row["status"], "created",
        "real promotion is not visible to its owner: {row}"
    );
    row["job"]
        .as_str()
        .filter(|job| !job.is_empty())
        .expect("created request has no job")
        .to_string()
}

fn queue_conn(rig: &Rig) -> rusqlite::Connection {
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db
}

#[derive(Clone, Debug, PartialEq)]
struct QueueLedger {
    org: String,
    sub: String,
    request_key: String,
    job: String,
    title: String,
    body: String,
    acceptance: String,
    credential_id: String,
    authority_rev: Option<String>,
    state: String,
    created: f64,
}

fn read_queue_ledger(db: &rusqlite::Connection, sub: &str, key: &str) -> Option<QueueLedger> {
    use rusqlite::OptionalExtension;
    db.query_row(
        "SELECT org, sub, request_key, job, title, body, acceptance, credential_id, \
                authority_rev, state, created \
         FROM assist_requests WHERE org=?1 AND sub=?2 AND request_key=?3",
        rusqlite::params![ORG, sub, key],
        |row| {
            Ok(QueueLedger {
                org: row.get(0)?,
                sub: row.get(1)?,
                request_key: row.get(2)?,
                job: row.get(3)?,
                title: row.get(4)?,
                body: row.get(5)?,
                acceptance: row.get(6)?,
                credential_id: row.get(7)?,
                authority_rev: row.get(8)?,
                state: row.get(9)?,
                created: row.get(10)?,
            })
        },
    )
    .optional()
    .unwrap()
}

fn restore_queue_ledger(db: &rusqlite::Connection, row: &QueueLedger) {
    db.execute(
        "DELETE FROM assist_requests WHERE org=?1 AND sub=?2 AND request_key=?3",
        rusqlite::params![row.org, row.sub, row.request_key],
    )
    .unwrap();
    db.execute(
        "INSERT INTO assist_requests(org, sub, request_key, job, title, body, acceptance, \
                                     credential_id, authority_rev, state, created) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        rusqlite::params![
            row.org,
            row.sub,
            row.request_key,
            row.job,
            row.title,
            row.body,
            row.acceptance,
            row.credential_id,
            row.authority_rev,
            row.state,
            row.created
        ],
    )
    .unwrap();
}

#[derive(Clone, Debug, PartialEq)]
struct QueueRow {
    org: String,
    sub: String,
    request_key: String,
    issue: String,
    title: String,
    body: String,
    acceptance: String,
    credential_id: String,
    created: f64,
    last_attempt: f64,
    last_outcome: Option<String>,
    completed: i64,
    job_id: String,
}

fn read_queue_row(db: &rusqlite::Connection, sub: &str, key: &str) -> Option<QueueRow> {
    use rusqlite::OptionalExtension;
    db.query_row(
        "SELECT org, sub, request_key, issue, title, body, acceptance, credential_id, \
                created, last_attempt, last_outcome, completed, job_id \
         FROM assist_request_queue WHERE org=?1 AND sub=?2 AND request_key=?3",
        rusqlite::params![ORG, sub, key],
        |row| {
            Ok(QueueRow {
                org: row.get(0)?,
                sub: row.get(1)?,
                request_key: row.get(2)?,
                issue: row.get(3)?,
                title: row.get(4)?,
                body: row.get(5)?,
                acceptance: row.get(6)?,
                credential_id: row.get(7)?,
                created: row.get(8)?,
                last_attempt: row.get(9)?,
                last_outcome: row.get(10)?,
                completed: row.get(11)?,
                job_id: row.get(12)?,
            })
        },
    )
    .optional()
    .unwrap()
}

fn write_queue_row(db: &rusqlite::Connection, row: &QueueRow) {
    db.execute(
        "INSERT OR REPLACE INTO assist_request_queue(org, sub, request_key, issue, title, body, \
             acceptance, credential_id, created, last_attempt, last_outcome, completed, job_id) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        rusqlite::params![
            row.org,
            row.sub,
            row.request_key,
            row.issue,
            row.title,
            row.body,
            row.acceptance,
            row.credential_id,
            row.created,
            row.last_attempt,
            row.last_outcome,
            row.completed,
            row.job_id
        ],
    )
    .unwrap();
}

fn queue_intents(db: &rusqlite::Connection, job: &str) -> i64 {
    db.query_row(
        "SELECT COUNT(*) FROM events WHERE kind='assist_intent' AND job_id=?1",
        [job],
        |row| row.get(0),
    )
    .unwrap()
}

#[derive(Debug, PartialEq, Eq)]
struct QueueCounts {
    jobs: i64,
    tasks: i64,
    ledger: i64,
    queued: i64,
    subscribers: i64,
    shares: i64,
}

fn queue_counts(db: &rusqlite::Connection, job: &str) -> QueueCounts {
    QueueCounts {
        jobs: db
            .query_row("SELECT COUNT(*) FROM jobs WHERE id=?1", [job], |row| row.get(0))
            .unwrap(),
        tasks: db
            .query_row("SELECT COUNT(*) FROM tasks WHERE job_id=?1", [job], |row| row.get(0))
            .unwrap(),
        ledger: db
            .query_row(
                "SELECT COUNT(*) FROM assist_requests WHERE org=?1 AND sub=?2 AND request_key=?3",
                rusqlite::params![ORG, QUEUE_OWNER, QUEUE_KEY],
                |row| row.get(0),
            )
            .unwrap(),
        queued: db
            .query_row(
                "SELECT COUNT(*) FROM assist_request_queue WHERE org=?1 AND sub=?2 AND request_key=?3",
                rusqlite::params![ORG, QUEUE_OWNER, QUEUE_KEY],
                |row| row.get(0),
            )
            .unwrap(),
        subscribers: db
            .query_row("SELECT COUNT(*) FROM job_subscribers WHERE job=?1", [job], |row| row.get(0))
            .unwrap(),
        shares: db
            .query_row("SELECT COUNT(*) FROM assist_job_shares WHERE job=?1", [job], |row| row.get(0))
            .unwrap(),
    }
}

fn queue_operator_show(rig: &Rig, job: &str, phase: &str) -> Value {
    crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc(rig._dir.path(), "job_show", json!({"job": job}))
    })
    .unwrap_or_else(|error| panic!("{phase}: proven local operator recovery refused: {error}"))
}

fn queue_operator_delivery(rig: &Rig, job: &str, task: &str, delivery: &str, phase: &str) {
    let listed = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc(rig._dir.path(), "delivery_private_list", json!({}))
    })
    .unwrap_or_else(|error| panic!("{phase}: operator private recovery refused: {error}"));
    let row = listed["deliveries"]
        .as_array()
        .and_then(|rows| {
            rows.iter().find(|row| {
                row["origin"]["job"].as_str() == Some(job)
                    && row["origin"]["task"].as_str() == Some(task)
            })
        })
        .unwrap_or_else(|| panic!("{phase}: operator lost the genuine private delivery: {listed}"));
    assert_eq!(
        row["id"], delivery,
        "{phase}: operator recovered a different delivery"
    );
    assert!(
        row["results"].to_string().contains(QUEUE_MARKER),
        "{phase}: operator delivery lost the private message: {row}"
    );
}

#[allow(clippy::too_many_arguments)] // one assertion helper, one row of facts
fn queue_assert_hidden(
    rig: &Rig,
    readers: &[(&str, &str, bool)],
    job: &str,
    task: &str,
    delivery: &str,
    request_id: &str,
    db: &rusqlite::Connection,
    phase: &str,
    expected_reason: Option<&str>,
    replay: Option<&Value>,
    check_agents: bool,
) {
    let secrets: [&str; 6] = [job, task, QUEUE_TITLE, QUEUE_BODY, delivery, QUEUE_MARKER];
    if let Some(receipt) = replay {
        let before = read_queue_row(db, QUEUE_OWNER, QUEUE_KEY);
        let replayed = queue_accept(
            rig,
            QUEUE_OWNER,
            QUEUE_KEY,
            QUEUE_ISSUE,
            QUEUE_TITLE,
            QUEUE_BODY,
        );
        assert_eq!(
            &replayed, receipt,
            "{phase}: the real signed replay changed the accepted receipt"
        );
        assert_eq!(
            read_queue_row(db, QUEUE_OWNER, QUEUE_KEY),
            before,
            "{phase}: the real signed replay changed the durable queue row"
        );
    }
    for &(sub, cookie, is_owner) in readers {
        let list = queue_list(rig, sub);
        let text = list.to_string();
        for secret in secrets {
            assert!(
                !text.contains(secret),
                "{phase}: {sub} job_list leaked {secret}: {list}"
            );
        }
        assert!(
            !list["jobs"].to_string().contains(job),
            "{phase}: {sub} job list named the interrupted job: {list}"
        );
        if is_owner {
            assert_eq!(
                list["promotion_worker"], "running",
                "{phase}: parked joined worker not observed: {list}"
            );
            let row = queue_row(&list, request_id)
                .unwrap_or_else(|| panic!("{phase}: owner lost its pending request: {list}"));
            assert_eq!(
                row["status"], "pending",
                "{phase}: interrupted request read as complete: {row}"
            );
            assert!(
                row["job"].is_null(),
                "{phase}: pending owner request carried a job id: {row}"
            );
            match expected_reason {
                Some(reason) => assert_eq!(
                    row["reason"], reason,
                    "{phase}: unprovable completion reason is not truthful: {row}"
                ),
                None => assert!(
                    row["reason"].is_null(),
                    "{phase}: truthful pending request gained a reason: {row}"
                ),
            }
        } else {
            assert!(
                queue_row(&list, request_id).is_none(),
                "{phase}: {sub} saw another owner's request: {list}"
            );
        }
        if let Some(rows) = list["private_deliveries"].as_array() {
            assert!(
                rows.iter().all(|row| row["id"] != delivery),
                "{phase}: {sub} learned the interrupted private delivery: {list}"
            );
        }
        for verb in ["job_evidence", "job_events"] {
            let envelope = rig.envelope(sub, json!({}), json!({}));
            let (status, body) = rig.call(verb, &envelope, &json!({"job": job}));
            if status == 200 {
                for secret in secrets {
                    assert!(
                        !body.to_string().contains(secret),
                        "{phase}: {sub} {verb} disclosed {secret}: {body}"
                    );
                }
            } else {
                assert!(
                    matches!(status, 401 | 403 | 404),
                    "{phase}: {sub} {verb} returned unexpected {status}: {body}"
                );
            }
        }
        let issue = get(rig, cookie, &format!("/api/issues/{QUEUE_ISSUE}"));
        let issue_text = issue.to_string();
        assert!(
            issue_text.contains(QUEUE_ISSUE),
            "{phase}: {sub} issue read lost the accessible issue identity: {issue}"
        );
        for secret in secrets {
            assert!(
                !issue_text.contains(secret),
                "{phase}: {sub} issue read leaked {secret}: {issue}"
            );
        }
        if check_agents {
            let agents = get(rig, cookie, "/api/agents");
            for secret in secrets {
                assert!(
                    !agents.to_string().contains(secret),
                    "{phase}: {sub} agent read leaked {secret}: {agents}"
                );
            }
        }
    }
}

fn queue_assert_authorized(
    rig: &Rig,
    readers: &[(&str, &str, bool)],
    job: &str,
    task: &str,
    delivery: &str,
    request_id: &str,
    phase: &str,
) {
    let secrets: [&str; 6] = [job, task, QUEUE_TITLE, QUEUE_BODY, delivery, QUEUE_MARKER];
    for &(sub, cookie, is_owner) in readers {
        let list = queue_list(rig, sub);
        let text = list.to_string();
        if is_owner {
            let row = queue_row(&list, request_id)
                .unwrap_or_else(|| panic!("{phase}: owner lost the completed request: {list}"));
            assert_eq!(
                row["status"], "created",
                "{phase}: completed request is not created: {row}"
            );
            assert_eq!(
                row["job"], job,
                "{phase}: completed request names another job: {row}"
            );
            assert!(
                row["reason"].is_null(),
                "{phase}: completed request carries a failure reason: {row}"
            );
            assert!(
                list["jobs"].to_string().contains(job),
                "{phase}: owner job list omitted its completed job: {list}"
            );
            assert!(
                text.contains(delivery) && text.contains(QUEUE_MARKER),
                "{phase}: owner private delivery missing: {list}"
            );
        } else if sub == QUEUE_SUBSCRIBER {
            assert!(
                list["jobs"].to_string().contains(job),
                "{phase}: subscriber lost the shared completed job: {list}"
            );
            assert!(
                queue_row(&list, request_id).is_none(),
                "{phase}: subscriber saw the owner request record: {list}"
            );
            assert!(
                !text.contains(delivery) && !text.contains(QUEUE_MARKER),
                "{phase}: subscriber learned the private delivery: {list}"
            );
        } else {
            assert!(
                !text.contains(job),
                "{phase}: unrelated project reader learned the private job: {list}"
            );
            assert!(
                queue_row(&list, request_id).is_none(),
                "{phase}: unrelated reader saw the request row: {list}"
            );
        }
        let envelope = rig.envelope(sub, json!({}), json!({}));
        let (status, evidence) = rig.call("job_evidence", &envelope, &json!({"job": job}));
        let envelope = rig.envelope(sub, json!({}), json!({}));
        let (events_status, events) = rig.call("job_events", &envelope, &json!({"job": job}));
        let authorized = is_owner || sub == QUEUE_SUBSCRIBER;
        if authorized {
            assert_eq!(
                status, 200,
                "{phase}: {sub} evidence denied for an authorized completed job: {evidence}"
            );
            assert!(
                evidence.to_string().contains(job) && evidence.to_string().contains(task),
                "{phase}: {sub} evidence omitted job/task: {evidence}"
            );
            assert_eq!(
                events_status, 200,
                "{phase}: {sub} events denied for an authorized completed job: {events}"
            );
            assert!(
                events.to_string().contains(job),
                "{phase}: {sub} events omitted the job: {events}"
            );
        } else {
            assert!(
                matches!(status, 401 | 403 | 404),
                "{phase}: {sub} evidence admitted a foreign private job: {evidence}"
            );
            assert!(
                matches!(events_status, 401 | 403 | 404),
                "{phase}: {sub} events admitted a foreign private job: {events}"
            );
        }
        let issue = get(rig, cookie, &format!("/api/issues/{QUEUE_ISSUE}"));
        let issue_text = issue.to_string();
        assert!(
            issue_text.contains(QUEUE_ISSUE),
            "{phase}: {sub} issue read lost the accessible issue identity: {issue}"
        );
        // The issue-detail `agents` strip is open tasks only
        // (`open_task_ids_by_assignee` excludes `verified`); the terminal
        // verified task is intentionally absent. Authorized job/task
        // positives are the job list, evidence and events above.
        if !authorized {
            for secret in secrets {
                assert!(
                    !issue_text.contains(secret),
                    "{phase}: {sub} issue read leaked {secret}: {issue}"
                );
            }
        }
    }
}

#[test]
fn interrupted_private_promotion_stays_pending_and_private_until_genuinely_completed() {
    let _flag = AssistantFlagOn::arm();
    let (observer, events, release) = queue_held_observer();
    let rig = start_with_queue_service(
        &[QUEUE_OWNER, QUEUE_SUBSCRIBER, QUEUE_UNRELATED],
        Some(observer),
        true,
    );
    // `job_events` on this rig is scoped: without an initialized keyring every
    // read would be `scoped_cursor_unavailable` rather than an authorization
    // answer, so the hidden/authorized phases below could not be proven.
    let _keys = initialize_queue_keyring(&rig);
    seed(&rig);
    let owner_cookie = login(&rig, QUEUE_OWNER, "member");
    let subscriber_cookie = login(&rig, QUEUE_SUBSCRIBER, "member");
    let unrelated_cookie = login(&rig, QUEUE_UNRELATED, "member");
    let readers = [
        (QUEUE_OWNER, owner_cookie.as_str(), true),
        (QUEUE_SUBSCRIBER, subscriber_cookie.as_str(), false),
        (QUEUE_UNRELATED, unrelated_cookie.as_str(), false),
    ];

    // Genuine signed acceptance and genuine joined promotion. The observer
    // parks the real worker after it reports `created`, so the reconstructed
    // crash states below are observed instead of being healed by a later sweep.
    let receipt = queue_accept(
        &rig,
        QUEUE_OWNER,
        QUEUE_KEY,
        QUEUE_ISSUE,
        QUEUE_TITLE,
        QUEUE_BODY,
    );
    let request_id = receipt["request_id"].as_str().unwrap().to_string();
    queue_wait_outcome(&events, &request_id, "created");
    let job = queue_owner_created(&rig, &request_id);
    let (status, share) = rig.call(
        "job_share",
        &rig.envelope(QUEUE_OWNER, json!({}), json!({})),
        &json!({"job": job, "enabled": true}),
    );
    assert_eq!(
        status, 200,
        "owner share of the genuine job failed: {share}"
    );
    let (status, subscribed) = rig.call(
        "job_subscribe",
        &rig.envelope(QUEUE_SUBSCRIBER, json!({}), json!({})),
        &json!({"job": job}),
    );
    assert_eq!(
        status, 200,
        "signed subscriber admission failed: {subscribed}"
    );

    // A genuine private delivery: a real operator dispatch, the real worker
    // result, and the real verdict that binds the private ledger. This is the
    // private message an interrupted promotion must not disclose.
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    let owner_pm = shared.store.job(&job).unwrap().pm_alias;
    let worker_dir = rig._dir.path().join("queue-crash-worker");
    std::fs::create_dir_all(&worker_dir).unwrap();
    let worker_params = json!({"upstream": owner_pm}).to_string();
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "cad2215-queue-worker",
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: worker_dir.to_str().unwrap(),
            sandbox: "workspace-write",
            instructions: None,
            params: Some(worker_params.as_str()),
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
    let operator = |method: &str, args: Value| {
        crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
            crate::client::rpc(rig._dir.path(), method, args)
        })
    };
    let dispatched = operator(
        "task_dispatch",
        json!({"task": task, "to": "cad2215-queue-worker", "by": "operator"}),
    )
    .expect("real operator dispatch failed");
    let message = dispatched["message"]
        .as_str()
        .expect("dispatch message missing")
        .to_string();
    let stored = shared.store.message(&message).unwrap().unwrap();
    let sha = "1234567890abcdef1234567890abcdef12345678";
    shared
        .store
        .finish(
            &stored,
            "completed",
            &json!({"status": "completed", "sha": sha, "text": QUEUE_MARKER}),
            None,
        )
        .unwrap();
    operator(
        "task_verdict",
        json!({"task": task, "sha": sha, "verdict": "pass"}),
    )
    .expect("real verdict failed");
    let deliveries =
        operator("delivery_private_list", json!({})).expect("operator private list unavailable");
    let delivery_row = deliveries["deliveries"]
        .as_array()
        .and_then(|rows| {
            rows.iter().find(|row| {
                row["origin"]["job"].as_str() == Some(job.as_str())
                    && row["origin"]["task"].as_str() == Some(task.as_str())
            })
        })
        .unwrap_or_else(|| panic!("real private delivery missing: {deliveries}"));
    let delivery = delivery_row["id"]
        .as_str()
        .expect("delivery id missing")
        .to_string();
    assert!(
        delivery_row["results"].to_string().contains(QUEUE_MARKER),
        "real delivery lost the private message: {delivery_row}"
    );

    let db = queue_conn(&rig);
    let original_ledger =
        read_queue_ledger(&db, QUEUE_OWNER, QUEUE_KEY).expect("genuine request ledger missing");
    assert_eq!(original_ledger.job, job);
    let original_queue =
        read_queue_row(&db, QUEUE_OWNER, QUEUE_KEY).expect("genuine acceptance row missing");
    assert_eq!(
        original_queue.completed, 1,
        "genuine promotion did not complete its acceptance row"
    );
    assert_eq!(
        original_queue.job_id, job,
        "acceptance row does not name the genuine job"
    );
    let job_before = shared.store.job(&job).unwrap().to_json();
    let task_before = shared.store.task(&task).unwrap().to_json();
    let spec_before = std::fs::read(
        rig._dir
            .path()
            .join("assist_jobs")
            .join(format!("{job}.md")),
    )
    .unwrap();
    let counts_before = queue_counts(&db, &job);

    // Crash 1: the job exists and the acceptance row is durable, but the
    // canonical `assist_requests` binding never landed. Nothing may read it.
    assert_eq!(
        db.execute(
            "DELETE FROM assist_requests WHERE org=?1 AND sub=?2 AND request_key=?3",
            rusqlite::params![ORG, QUEUE_OWNER, QUEUE_KEY],
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.execute(
            "UPDATE assist_request_queue SET completed=0 \
             WHERE org=?1 AND sub=?2 AND request_key=?3 AND completed=1",
            rusqlite::params![ORG, QUEUE_OWNER, QUEUE_KEY],
        )
        .unwrap(),
        1
    );
    assert!(read_queue_ledger(&db, QUEUE_OWNER, QUEUE_KEY).is_none());
    assert_eq!(
        read_queue_row(&db, QUEUE_OWNER, QUEUE_KEY)
            .unwrap()
            .completed,
        0
    );
    queue_assert_hidden(
        &rig,
        &readers,
        &job,
        &task,
        &delivery,
        &request_id,
        &db,
        "crash before the ledger bind",
        None,
        Some(&receipt),
        true,
    );
    let shown = queue_operator_show(&rig, &job, "crash before the ledger bind");
    assert_eq!(shown["job"]["id"], job);
    assert!(shown["job"]["tasks"].to_string().contains(task.as_str()));
    assert_eq!(shown["job"]["title"], QUEUE_TITLE);

    // Crash 2: restore the EXACT original binding, but the queue row still
    // says the required subscription/notice effects did not finish.
    restore_queue_ledger(&db, &original_ledger);
    assert_eq!(
        read_queue_ledger(&db, QUEUE_OWNER, QUEUE_KEY).as_ref(),
        Some(&original_ledger)
    );
    assert_eq!(
        read_queue_row(&db, QUEUE_OWNER, QUEUE_KEY)
            .unwrap()
            .completed,
        0
    );
    queue_assert_hidden(
        &rig,
        &readers,
        &job,
        &task,
        &delivery,
        &request_id,
        &db,
        "crash after the ledger bind before completion",
        None,
        Some(&receipt),
        true,
    );
    queue_operator_show(&rig, &job, "crash after the ledger bind before completion");

    // Restore the genuine completed row: the authorized owner/subscriber
    // controls must come back, and the private delivery with them.
    assert_eq!(
        db.execute(
            "UPDATE assist_request_queue SET completed=1 \
             WHERE org=?1 AND sub=?2 AND request_key=?3 AND completed=0",
            rusqlite::params![ORG, QUEUE_OWNER, QUEUE_KEY],
        )
        .unwrap(),
        1
    );
    assert_eq!(
        read_queue_row(&db, QUEUE_OWNER, QUEUE_KEY).unwrap(),
        original_queue
    );
    assert_eq!(
        read_queue_ledger(&db, QUEUE_OWNER, QUEUE_KEY).unwrap(),
        original_ledger
    );
    queue_assert_authorized(
        &rig,
        &readers,
        &job,
        &task,
        &delivery,
        &request_id,
        "restored completed row",
    );
    queue_operator_delivery(&rig, &job, &task, &delivery, "restored completed row");

    // A completed row whose planned job id no longer names the ledger job is
    // not a valid completion: it reads pending and discloses nothing.
    assert_eq!(
        db.execute(
            "UPDATE assist_request_queue SET job_id='fixture-wrong-planned-job' \
             WHERE org=?1 AND sub=?2 AND request_key=?3 AND job_id=?4 AND completed=1",
            rusqlite::params![ORG, QUEUE_OWNER, QUEUE_KEY, job],
        )
        .unwrap(),
        1
    );
    queue_assert_hidden(
        &rig,
        &readers,
        &job,
        &task,
        &delivery,
        &request_id,
        &db,
        "completed with a mismatched planned job id",
        Some("promotion_attempt_failed"),
        None,
        false,
    );
    assert_eq!(
        db.execute(
            "UPDATE assist_request_queue SET job_id=?1 \
             WHERE org=?2 AND sub=?3 AND request_key=?4 AND job_id='fixture-wrong-planned-job'",
            rusqlite::params![job, ORG, QUEUE_OWNER, QUEUE_KEY],
        )
        .unwrap(),
        1
    );

    // A completed row whose job moved off the recorded issue is equally
    // unprovable; restore it below before the genuine recovery.
    assert_eq!(
        db.execute(
            "UPDATE jobs SET issue_id='SEC-1' WHERE id=?1",
            [job.as_str()]
        )
        .unwrap(),
        1
    );
    queue_assert_hidden(
        &rig,
        &readers,
        &job,
        &task,
        &delivery,
        &request_id,
        &db,
        "completed job moved off its recorded issue",
        Some("promotion_attempt_failed"),
        None,
        false,
    );
    assert_eq!(
        db.execute(
            "UPDATE jobs SET issue_id=?1 WHERE id=?2",
            rusqlite::params![QUEUE_ISSUE, job],
        )
        .unwrap(),
        1
    );
    assert_eq!(
        read_queue_row(&db, QUEUE_OWNER, QUEUE_KEY).unwrap(),
        original_queue
    );
    assert_eq!(
        read_queue_ledger(&db, QUEUE_OWNER, QUEUE_KEY).unwrap(),
        original_ledger
    );

    // A real pending sweep must refuse an already-bound recovery whose ledger
    // job disagrees with the persisted planned id, BEFORE any subscription or
    // notice: the request stays pending with no job and the receipt unchanged.
    let mut ambiguous = original_queue.clone();
    ambiguous.completed = 0;
    write_queue_row(&db, &ambiguous);
    assert_eq!(
        read_queue_row(&db, QUEUE_OWNER, QUEUE_KEY).unwrap(),
        ambiguous
    );
    assert_eq!(
        db.execute(
            "UPDATE assist_requests SET job='fixture-foreign-ledger-job' \
             WHERE org=?1 AND sub=?2 AND request_key=?3 AND job=?4",
            rusqlite::params![ORG, QUEUE_OWNER, QUEUE_KEY, job],
        )
        .unwrap(),
        1
    );
    let subscribers_before = shared.store.job_subscribers(&job).unwrap().len();
    let intents_before = queue_intents(&db, &job);
    let refusal_counts_before = queue_counts(&db, &job);
    let refused = shared
        .sweep_member_job_requests_for_test()
        .expect("real pending promotion sweep failed");
    assert!(
        refused
            .iter()
            .any(|outcome| outcome.request_id == request_id && outcome.outcome == "refused"),
        "ambiguous pending sweep was not refused: {refused:?}"
    );
    assert_eq!(
        read_queue_row(&db, QUEUE_OWNER, QUEUE_KEY)
            .unwrap()
            .completed,
        0
    );
    assert_eq!(
        read_queue_ledger(&db, QUEUE_OWNER, QUEUE_KEY).unwrap().job,
        "fixture-foreign-ledger-job"
    );
    assert_eq!(
        shared.store.job_subscribers(&job).unwrap().len(),
        subscribers_before,
        "ambiguous sweep subscribed the owner"
    );
    assert_eq!(
        queue_intents(&db, &job),
        intents_before,
        "ambiguous sweep emitted a notice"
    );
    assert_eq!(
        queue_counts(&db, &job),
        refusal_counts_before,
        "ambiguous sweep created durable rows"
    );
    queue_assert_hidden(
        &rig,
        &readers,
        &job,
        &task,
        &delivery,
        &request_id,
        &db,
        "ambiguous pending sweep",
        Some("promotion_unavailable"),
        Some(&receipt),
        false,
    );

    // Restore this request's exact own binding. The same real pending row is
    // then finished by the joined worker without a second publication.
    assert_eq!(
        db.execute(
            "UPDATE assist_requests SET job=?1 \
             WHERE org=?2 AND sub=?3 AND request_key=?4 AND job='fixture-foreign-ledger-job'",
            rusqlite::params![job, ORG, QUEUE_OWNER, QUEUE_KEY],
        )
        .unwrap(),
        1
    );
    write_queue_row(&db, &ambiguous);
    assert_eq!(
        read_queue_row(&db, QUEUE_OWNER, QUEUE_KEY).unwrap(),
        ambiguous
    );
    assert_eq!(
        read_queue_ledger(&db, QUEUE_OWNER, QUEUE_KEY).unwrap(),
        original_ledger
    );
    queue_release(&release);
    queue_wait_outcome(&events, &request_id, "created");
    assert_eq!(
        read_queue_row(&db, QUEUE_OWNER, QUEUE_KEY).unwrap(),
        original_queue,
        "genuine idempotent promotion did not restore the exact original acceptance row"
    );
    assert_eq!(
        read_queue_ledger(&db, QUEUE_OWNER, QUEUE_KEY).unwrap(),
        original_ledger,
        "genuine idempotent promotion changed the original ledger"
    );
    assert_eq!(
        shared.store.job(&job).unwrap().to_json(),
        job_before,
        "idempotent promotion changed the job"
    );
    assert_eq!(
        shared.store.task(&task).unwrap().to_json(),
        task_before,
        "idempotent promotion changed the task"
    );
    assert_eq!(
        std::fs::read(
            rig._dir
                .path()
                .join("assist_jobs")
                .join(format!("{job}.md"))
        )
        .unwrap(),
        spec_before,
        "idempotent promotion changed the spec bytes"
    );
    assert_eq!(
        queue_counts(&db, &job),
        counts_before,
        "idempotent promotion duplicated durable rows"
    );
    queue_assert_authorized(
        &rig,
        &readers,
        &job,
        &task,
        &delivery,
        &request_id,
        "genuine idempotent promotion",
    );
    queue_operator_delivery(&rig, &job, &task, &delivery, "genuine idempotent promotion");
    assert_eq!(
        queue_accept(
            &rig,
            QUEUE_OWNER,
            QUEUE_KEY,
            QUEUE_ISSUE,
            QUEUE_TITLE,
            QUEUE_BODY
        ),
        receipt,
        "authorized replay after completion changed the accepted receipt"
    );
    assert_eq!(
        queue_counts(&db, &job),
        counts_before,
        "authorized replay duplicated durable rows"
    );
    assert_eq!(
        read_queue_row(&db, QUEUE_OWNER, QUEUE_KEY).unwrap(),
        original_queue
    );
}
