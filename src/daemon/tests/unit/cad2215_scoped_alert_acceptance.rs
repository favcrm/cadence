//! CAD-2215: independent scoped monitor-alert acceptance.
//!
//! A non-operator reader of a monitor's alerts gets an opaque page: the page
//! names the opaque format, its cursor is an opaque token or `null`, and every
//! visible alert carries a stable keyed `id` plus its own encrypted `ack`
//! token. The document omits the global `seq`, the `event_seq` and the
//! `fingerprint`, so a reader cannot observe another monitor's activity or the
//! workspace's alert volume. Visibility is decided by the reader's complete
//! readable coverage, so a monitor that covers any private task is absent and
//! alerts inserted for hidden monitors between two pages neither appear nor
//! move the visible continuation.
//!
//! An acknowledgement is scoped to the proved principal, the exact monitor and
//! the exact alert: a cursor or `ack` token from another reader, another
//! monitor or another workspace is refused and leaves the alert open, a
//! numeric `alert` is not a scoped ack, and access withdrawn after a token was
//! issued refuses the ack without invalidating the token — restoring access
//! acknowledges the same alert. A proven operator keeps numeric positions,
//! numeric continuation and numeric acknowledgement; missing key material
//! refuses every scoped alert read.
//!
//! Key material and the keyring record are fixture-owned files outside the
//! tracker; the only setup operation is the real operator
//! `scoped_cursor_initialize` RPC. No production code or existing acceptance is
//! modified. This does NOT establish OS isolation or private-project
//! availability.

use super::*;
use crate::test_seam::{scoped, Asserted};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

const IDENTITY_BYTE: u8 = 0x41;
const KEY_A_BYTE: u8 = 0x52;
const KEY_B_BYTE: u8 = 0x63;
const KEY_ID_A: u8 = 0x0c;
const KEY_ID_B: u8 = 0x0d;

const CURSOR_FORMAT: &str = "opaque-v1";
const VISIBLE_MARKER: &str = "CAD2215_AL_VISIBLE_";
const HIDDEN_MARKER: &str = "CAD2215_AL_HIDDEN_";
const VISIBLE_AFTER_MARKER: &str = "CAD2215_AL_VISIBLE_AFTER";

const OPEN_MONITOR: &str = "open-monitor";
const OTHER_OPEN_MONITOR: &str = "open-monitor-2";
const RESTRICTED_MONITOR: &str = "restricted-monitor";
const MIXED_MONITOR: &str = "mixed-monitor";

const OPEN_FIRST_SEQ: i64 = 1000;
const OTHER_FIRST_SEQ: i64 = 5000;
const RESTRICTED_FIRST_SEQ: i64 = 2000;
const MIXED_FIRST_SEQ: i64 = 3000;

fn hex_id(byte: u8) -> String {
    format!("{byte:02x}").repeat(16)
}

fn private_dir(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn write_private(path: &Path, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    drop(file);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// Owner-provisioned key material and the fixed record content. One directory
/// serves every world in a test, so a token can only differ by the codec's own
/// workspace binding.
struct Keys {
    _dir: tempfile::TempDir,
    config: Value,
}

fn keys() -> Keys {
    let dir = tempfile::tempdir().unwrap();
    private_dir(dir.path());
    for (name, byte) in [
        ("identity.key", IDENTITY_BYTE),
        ("cursor-a.key", KEY_A_BYTE),
        ("cursor-b.key", KEY_B_BYTE),
    ] {
        write_private(&dir.path().join(name), &[byte; 32]);
    }
    let config = json!({
        "version": 1,
        "identity_key_file": dir.path().join("identity.key").to_str().unwrap(),
        "active_key_id": hex_id(KEY_ID_A),
        "keys": [
            { "id": hex_id(KEY_ID_A), "key_file": dir.path().join("cursor-a.key").to_str().unwrap() },
            { "id": hex_id(KEY_ID_B), "key_file": dir.path().join("cursor-b.key").to_str().unwrap() },
        ],
    });
    Keys { _dir: dir, config }
}

impl Keys {
    /// The same retained ring with the other key active. The persisted keyring
    /// fingerprint excludes the active id, so this is a legal rotation.
    fn with_active_b(&self) -> Value {
        let mut config = self.config.clone();
        config["active_key_id"] = json!(hex_id(KEY_ID_B));
        config
    }
}

fn write_config(state: &Path, config: &Value) {
    write_private(
        &state.join("scoped-cursors.json"),
        &serde_json::to_vec(config).unwrap(),
    );
}

fn write_project(pm: &Path, key: &str, prefix: &str, issue: &str, policy: &str) {
    let project = pm.join(key);
    let issue_dir = project.join(issue);
    std::fs::create_dir_all(&issue_dir).unwrap();
    std::fs::write(
        project.join("project.yaml"),
        format!("key: {key}\nprefix: {prefix}\n{policy}"),
    )
    .unwrap();
    std::fs::write(
        issue_dir.join("issue.md"),
        format!(
            "---\nid: {issue}\ntitle: {issue} fixture\nstatus: backlog\n\
             priority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"
        ),
    )
    .unwrap();
}

/// The policy block that makes a project private for a scoped reader.
const PRIVATE_POLICY: &str = "policy:\n  version: 1\n  visibility: private\n";

fn visible_project_policy() -> &'static str {
    "key: open\nprefix: VIS\n"
}

/// The readable project made private: a revocation of the same project, not a
/// replacement with another project's identity.
fn private_open_policy() -> &'static str {
    "key: open\nprefix: VIS\npolicy:\n  version: 1\n  visibility: private\n"
}

struct World {
    _state: tempfile::TempDir,
    pm: PathBuf,
}

impl World {
    fn state(&self) -> &Path {
        self._state.path()
    }

    fn boot(&self) -> Arc<Shared> {
        self.boot_with(|_| {})
    }

    /// Boot with test-only read hooks installed through `ServeOptions`.
    fn boot_with(&self, configure: impl FnOnce(&mut ServeOptions)) -> Arc<Shared> {
        let mut opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", self.pm.to_str().unwrap());
        configure(&mut opts);
        Shared::new(self.state(), &opts).unwrap()
    }

    fn visible_policy_path(&self) -> PathBuf {
        self.pm.join("open").join("project.yaml")
    }
}

fn insert_alert(tx: &rusqlite::Transaction<'_>, seq: i64, monitor: &str, task: &str, marker: &str) {
    tx.execute(
        "INSERT INTO monitor_alerts(seq,monitor_id,task_id,event_seq,fingerprint,\
         kind,payload,state,created,updated) VALUES(?,?,?,?,?,?,?,?,?,?)",
        rusqlite::params![
            seq,
            monitor,
            task,
            seq,
            format!("fp-{seq}"),
            "fixture_alert",
            json!({"marker": marker}).to_string(),
            "open",
            1000.0 + seq as f64,
            1000.0 + seq as f64,
        ],
    )
    .unwrap();
}

/// Append one alert after a page was read.
fn append_alert(state: &Path, seq: i64, monitor: &str, task: &str, marker: &str) {
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO monitor_alerts(seq,monitor_id,task_id,event_seq,fingerprint,\
         kind,payload,state,created,updated) VALUES(?,?,?,?,?,?,?,?,?,?)",
        rusqlite::params![
            seq,
            monitor,
            task,
            seq,
            format!("fp-{seq}"),
            "fixture_alert",
            json!({"marker": marker}).to_string(),
            "open",
            1000.0 + seq as f64,
            1000.0 + seq as f64,
        ],
    )
    .unwrap();
}

fn alert_state(state: &Path, seq: i64) -> String {
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    db.query_row(
        "SELECT state FROM monitor_alerts WHERE seq=?1",
        [seq],
        |row| row.get(0),
    )
    .unwrap()
}

/// Build a real workspace: a readable and a private project, two readable
/// monitors, a hidden monitor and a mixed (partly private) monitor, plus
/// `open_alerts` alerts on the first readable monitor. `Some(keys)` writes the
/// keyring record and binds it with the real operator initialization RPC.
fn build_world(keys: Option<&Keys>, open_alerts: usize) -> World {
    let state = tempfile::tempdir().unwrap();
    private_dir(state.path());
    let pm = state.path().join("tracker");
    crate::issue::Pm::init(&pm).unwrap();
    std::fs::set_permissions(&pm, std::fs::Permissions::from_mode(0o700)).unwrap();
    write_project(&pm, "open", "VIS", "VIS-1", "");
    write_project(&pm, "hidden", "HID", "HID-1", PRIVATE_POLICY);
    if let Some(keys) = keys {
        write_config(state.path(), &keys.config);
    }
    let world = World { _state: state, pm };
    {
        let shared = world.boot();
        for alias in ["reader", "peer"] {
            register(&shared, world.state(), alias);
        }
        let mut db = rusqlite::Connection::open(world.state().join("cadence.sqlite3")).unwrap();
        db.execute_batch(
            "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,repo,state,created,updated)
             VALUES ('open-job','Open','spec.md','reader','VIS-1','fixture','open',1,1),
                    ('hidden-job','Hidden','spec.md','reader','HID-1','fixture','open',2,2);
             INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
             VALUES ('open-task','open-job','Open','reader','running',1,1),
                    ('open-task-2','open-job','Open 2','reader','running',2,2),
                    ('restricted-task','hidden-job','Hidden','reader','running',3,3);",
        )
        .unwrap();
        for (monitor, coverage) in [
            (OPEN_MONITOR, vec!["open-task"]),
            (OTHER_OPEN_MONITOR, vec!["open-task-2"]),
            (RESTRICTED_MONITOR, vec!["restricted-task"]),
            (MIXED_MONITOR, vec!["open-task", "restricted-task"]),
        ] {
            let tasks = coverage.into_iter().map(str::to_string).collect::<Vec<_>>();
            shared
                .store
                .register_monitor(monitor, "fixture", "reader", 60, &tasks, false, false)
                .unwrap();
        }
        {
            let tx = db.transaction().unwrap();
            for index in 0..open_alerts {
                insert_alert(
                    &tx,
                    OPEN_FIRST_SEQ + index as i64,
                    OPEN_MONITOR,
                    "open-task",
                    &format!("{VISIBLE_MARKER}{index}"),
                );
            }
            insert_alert(
                &tx,
                OTHER_FIRST_SEQ,
                OTHER_OPEN_MONITOR,
                "open-task-2",
                "CAD2215_AL_OTHER_VISIBLE",
            );
            for index in 0..11 {
                insert_alert(
                    &tx,
                    RESTRICTED_FIRST_SEQ + index,
                    RESTRICTED_MONITOR,
                    "restricted-task",
                    &format!("{HIDDEN_MARKER}{index}"),
                );
            }
            insert_alert(
                &tx,
                MIXED_FIRST_SEQ,
                MIXED_MONITOR,
                "restricted-task",
                "CAD2215_AL_MIXED_HIDDEN",
            );
            tx.commit().unwrap();
        }
        drop(db);
        if keys.is_some() {
            let response = scoped(Asserted::Operator, || {
                shared.dispatch("scoped_cursor_initialize", &json!({}), std::process::id())
            })
            .expect("the operator must initialize the keyring");
            assert_eq!(response["initialized"], json!(true), "{response}");
        }
    }
    world
}

fn call(shared: &Arc<Shared>, who: Asserted, method: &str, params: &Value) -> Result<Value> {
    let shared = Arc::clone(shared);
    scoped(who, || shared.dispatch(method, params, std::process::id()))
}

fn scoped_alerts(
    shared: &Arc<Shared>,
    who: &str,
    monitor: &str,
    after: Option<&str>,
) -> Result<Value> {
    let mut params = json!({"monitor": monitor, "cursor_format": CURSOR_FORMAT});
    if let Some(token) = after {
        params["after"] = json!(token);
    }
    call(
        shared,
        Asserted::Agent(who.to_string()),
        "monitor_alerts",
        &params,
    )
}

fn scoped_page(shared: &Arc<Shared>, who: &str, monitor: &str, after: Option<&str>) -> Value {
    let page = scoped_alerts(shared, who, monitor, after)
        .unwrap_or_else(|error| panic!("scoped alert page for {who}: {error:?}"));
    assert_scoped_page(&page, monitor);
    page
}

fn ack(shared: &Arc<Shared>, who: Asserted, monitor: &str, token: Value) -> Result<Value> {
    call(
        shared,
        who,
        "monitor_alert_ack",
        &json!({"monitor": monitor, "ack": token}),
    )
}

fn alerts(page: &Value) -> &Vec<Value> {
    page["alerts"]
        .as_array()
        .unwrap_or_else(|| panic!("alert page has no alerts array: {page}"))
}

fn markers(page: &Value) -> Vec<String> {
    alerts(page)
        .iter()
        .map(|alert| {
            alert["payload"]["marker"]
                .as_str()
                .unwrap_or_else(|| panic!("fixture alert has no marker: {alert}"))
                .to_string()
        })
        .collect()
}

fn ids(page: &Value) -> Vec<String> {
    alerts(page)
        .iter()
        .map(|alert| {
            alert["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| panic!("scoped alert has no stable id: {alert}"))
                .to_string()
        })
        .collect()
}

fn alert_ack(page: &Value, marker: &str) -> Value {
    alerts(page)
        .iter()
        .find(|alert| alert["payload"]["marker"] == json!(marker))
        .unwrap_or_else(|| panic!("alert {marker} is missing from the page: {page}"))["ack"]
        .clone()
}

fn page_cursor(page: &Value) -> Option<String> {
    match &page["cursor"] {
        Value::Null => None,
        Value::String(token) if !token.is_empty() && token.parse::<i64>().is_err() => {
            Some(token.clone())
        }
        other => panic!("scoped alert page cursor is not opaque or null: {other:?}"),
    }
}

fn assert_scoped_page(page: &Value, monitor: &str) {
    assert_eq!(
        page[cadence_core::board_api::READER_SCOPE_FIELD],
        json!(crate::daemon::monitors_rpc::MONITOR_READER_SCOPE_ACK),
        "the scoped alert page did not acknowledge its reader: {page}"
    );
    assert_eq!(
        page["cursor_format"],
        json!(CURSOR_FORMAT),
        "the scoped alert page did not name the opaque format: {page}"
    );
    if let Some(named) = page.get("monitor") {
        assert_eq!(
            named,
            &json!(monitor),
            "the page named another monitor: {page}"
        );
    }
    let body = page
        .as_object()
        .unwrap_or_else(|| panic!("alert page is not an object: {page}"));
    for field in [
        "seq",
        "event_seq",
        "fingerprint",
        "keyring_fingerprint",
        "watermark",
        "position",
    ] {
        assert!(
            !body.contains_key(field),
            "the scoped alert page exposed structural {field}: {page}"
        );
    }
    match &page["cursor"] {
        Value::Null => {}
        Value::String(token) => {
            assert!(!token.is_empty(), "empty page cursor: {page}");
            assert!(
                token.parse::<i64>().is_err(),
                "the page cursor is a numeric position: {token}"
            );
        }
        other => panic!("the page cursor is not a string or null: {other:?}"),
    }
    let mut seen_ids = Vec::new();
    let mut seen_acks = Vec::new();
    for alert in alerts(page) {
        let alert = alert
            .as_object()
            .unwrap_or_else(|| panic!("alert row is not an object: {alert}"));
        for field in ["seq", "event_seq", "fingerprint", "watermark", "position"] {
            assert!(
                !alert.contains_key(field),
                "the scoped alert row exposed {field}: {alert:?}"
            );
        }
        if let Some(named) = alert.get("monitor") {
            assert_eq!(
                named,
                &json!(monitor),
                "a row belongs to another monitor: {alert:?}"
            );
        }
        let id = alert["id"].as_str().unwrap_or_default();
        assert!(!id.is_empty(), "the alert lost its stable id: {alert:?}");
        assert!(
            !seen_ids.contains(&id.to_string()),
            "duplicate stable id {id} in one page"
        );
        seen_ids.push(id.to_string());
        let ack = alert["ack"].as_str().unwrap_or_default();
        assert!(!ack.is_empty(), "the alert lost its ack token: {alert:?}");
        assert!(
            ack.parse::<i64>().is_err(),
            "the alert ack token is a numeric position: {ack}"
        );
        assert!(
            !seen_acks.contains(&ack.to_string()),
            "two alerts shared one ack token in a page"
        );
        seen_acks.push(ack.to_string());
    }
    let rendered = page.to_string();
    for needle in ["csfk_", "keyring_fingerprint"] {
        assert!(
            !rendered.contains(needle),
            "the scoped alert page leaked {needle}: {rendered}"
        );
    }
}

/// The stable, reader-visible shape of a `monitor_list` page: ids, coverage,
/// counts, mode and delivery. Wall-clock fields (`created`, `updated`,
/// `next_check_at`, `last_check_at`) are excluded because two fixture worlds
/// are built at different times.
fn stable_monitors(page: &Value) -> Vec<Value> {
    page["monitors"]
        .as_array()
        .unwrap_or_else(|| panic!("monitor list has no monitors array: {page}"))
        .iter()
        .map(|monitor| {
            json!({
                "id": monitor["id"],
                "project": monitor["project"],
                "owner": monitor["owner"],
                "interval_secs": monitor["interval_secs"],
                "monitoring": monitor["monitoring"],
                "coverage": monitor["coverage"],
                "delivery": monitor["delivery"],
                "dispatch_enabled": monitor["dispatch_enabled"],
                "auto_dispatch_enabled": monitor["auto_dispatch_enabled"],
                "open_alerts": monitor["open_alerts"],
                "total_alerts": monitor["total_alerts"],
            })
        })
        .collect()
}

/// Insert one unrelated hidden issue + job + task + event through the real
/// SQLite store and the tracker directory. It is hidden activity only.
fn insert_unrelated_hidden_work(pm: &Path, state: &Path, marker: &str) {
    let issue = pm.join("hidden").join("HID-2");
    std::fs::create_dir_all(&issue).unwrap();
    std::fs::write(
        issue.join("issue.md"),
        "---\nid: HID-2\ntitle: HID-2 fixture\nstatus: backlog\n\
         priority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    db.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated)
         VALUES('hidden-job-3','Hidden 3','spec.md','reader','HID-2','open',9,9);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
         VALUES('hidden-task-3','hidden-job-3','Hidden 3','reader','running',9,9);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO events(alias,kind,payload,job_id,task_id,at) VALUES(?,?,?,?,?,?)",
        rusqlite::params![
            "reader",
            "hidden_progress",
            json!({"marker": marker}).to_string(),
            "hidden-job-3",
            "hidden-task-3",
            9.0,
        ],
    )
    .unwrap();
}

const PRIVATE_ORG: &str = "cadence-org";
const PRIVATE_OWNER_SUB: &str = "sub-owner";
const OTHER_OWNER_SUB: &str = "sub-other";
const PM_JOB: &str = "pm-private-job";
const PM_TASK: &str = "pm-private-task";
const PM_MONITOR: &str = "pm-private-monitor";
const PM_ALERT: i64 = 9001;
const PM_ALERT_MARKER: &str = "pm-private-alert";

/// A workspace-visible project and a settled member-private job whose current
/// PM is the native `reader` agent. Exactly one real `assist_requests` owner
/// row gives the job trusted provenance, so the PM exception keeps its task
/// visible even though the job is member-private. The monitor covers that task
/// and carries one alert.
fn pm_private_alert_world(keys: &Keys) -> World {
    let world = build_world(Some(keys), 1);
    {
        let shared = world.boot();
        let db = rusqlite::Connection::open(world.state().join("cadence.sqlite3")).unwrap();
        db.execute_batch(
            "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,repo,state,created,updated)
             VALUES('pm-private-job','PM private','spec.md','reader','VIS-1','fixture','open',5,5);
             INSERT INTO tasks(id,job_id,title,assignee,state,created,updated)
             VALUES('pm-private-task','pm-private-job','PM private','manager','running',5,5);",
        )
        .unwrap();
        drop(db);
        // The real settle writer: one owner bound to the job with no queue
        // row, so provenance is trusted (owners=1) and the unique ledger
        // owner is settled.
        shared
            .store
            .assist_request_bind(
                PRIVATE_ORG,
                PRIVATE_OWNER_SUB,
                "req-owner",
                PM_JOB,
                "private title",
                "private body",
                "private acceptance",
                "cred-private",
            )
            .expect("the single settled request owner");
        shared
            .store
            .register_monitor(
                PM_MONITOR,
                // The monitor's project label must agree with the covered
                // task's job repo (`fixture`), or the store refuses the
                // registration as unbound.
                "fixture",
                "reader",
                60,
                &[PM_TASK.to_string()],
                false,
                false,
            )
            .expect("the monitor covering the private task");
        let db = rusqlite::Connection::open(world.state().join("cadence.sqlite3")).unwrap();
        db.execute(
            "INSERT INTO monitor_alerts(seq,monitor_id,task_id,event_seq,fingerprint,kind,payload,state,created,updated)
             VALUES(?1,?2,?3,1,'pm-private-fp','test',?4,'open',1,1)",
            rusqlite::params![
                PM_ALERT,
                PM_MONITOR,
                PM_TASK,
                json!({"marker": PM_ALERT_MARKER}).to_string(),
            ],
        )
        .unwrap();

        // Explicit fixture proof that the native PM is genuinely authorized:
        // the reader's project scope admits the covered issue, the monitor's
        // coverage names that issue, the settled provenance keeps the PM
        // exception (the task is not denied), and the monitor is visible.
        let scope = shared
            .project_scope_for_actor("agents/reader")
            .expect("the reader project scope");
        assert!(
            crate::daemon::identity::scope_allows_issue(&scope, Some("VIS-1")),
            "the reader scope does not admit VIS-1: {scope:?}"
        );
        assert!(
            scope
                .as_ref()
                .is_some_and(|(readable, _)| readable.contains("open")),
            "the reader scope does not admit the open project: {scope:?}"
        );
        let view = shared
            .store
            .project_monitor_view(PM_MONITOR)
            .expect("the monitor view");
        assert_eq!(
            view.coverage,
            vec![(PM_TASK.to_string(), Some("VIS-1".to_string()))],
            "the monitor coverage does not bind the private task to VIS-1: {:?}",
            view.coverage
        );
        let denied = shared
            .store
            .private_job_denials(crate::store::PrivateJobReader::Agent("reader"))
            .expect("the reader denials");
        assert!(
            !denied.tasks.contains(PM_TASK) && !denied.jobs.contains(PM_JOB),
            "the PM exception is not in force for the private job: {denied:?}"
        );
        assert_eq!(
            shared
                .store
                .assist_job_owner(PM_JOB)
                .expect("the unique owner"),
            Some((PRIVATE_ORG.to_string(), PRIVATE_OWNER_SUB.to_string())),
            "the private job provenance is not the single settled owner"
        );
    }
    world
}

/// Mint the native PM's genuine opaque ack token for the private monitor's
/// alert while the job's provenance is still unique.
fn reader_alert_token(shared: &Arc<Shared>) -> String {
    let page = scoped(Asserted::Agent("reader".to_string()), || {
        shared.dispatch(
            "monitor_alerts",
            &json!({"monitor": PM_MONITOR, "cursor_format": CURSOR_FORMAT}),
            std::process::id(),
        )
    })
    .expect("the PM must read the private monitor's alert");
    page["alerts"]
        .as_array()
        .unwrap_or_else(|| panic!("the private monitor page has no alerts: {page}"))
        .iter()
        .find(|alert| alert["payload"]["marker"] == json!(PM_ALERT_MARKER))
        .unwrap_or_else(|| panic!("the private alert is missing from the page: {page}"))["ack"]
        .as_str()
        .filter(|token| !token.is_empty())
        .expect("the private alert must carry its ack token")
        .to_string()
}

fn ack_event_count(state: &Path, monitor: &str) -> i64 {
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    db.query_row(
        "SELECT COUNT(*) FROM events WHERE kind='monitor_alert_ack' \
         AND json_extract(payload,'$.monitor')=?1",
        [monitor],
        |row| row.get(0),
    )
    .unwrap()
}

fn assert_refusal(error: Error, what: &str) {
    assert_ne!(error.kind(), "internal", "{what}: {error:?}");
    let rendered = format!("{error} {error:?}");
    for needle in [VISIBLE_MARKER, HIDDEN_MARKER, "csc1_", "csrid_", "csfk_"] {
        assert!(
            !rendered.contains(needle),
            "{what} leaked {needle}: {rendered}"
        );
    }
}

#[test]
fn scoped_alert_pages_are_opaque_and_hidden_insertion_does_not_move_the_continuation() {
    let keys = keys();
    let world = build_world(Some(&keys), 105);
    let shared = world.boot();

    let first = scoped_page(&shared, "reader", OPEN_MONITOR, None);
    let first_markers = markers(&first);
    assert!(!first_markers.is_empty(), "{first}");
    assert!(
        first_markers.len() < 105,
        "the fixture did not exercise continuation: {} alerts",
        first_markers.len()
    );
    let first_ids = ids(&first);
    for (index, marker) in first_markers.iter().enumerate() {
        assert_eq!(
            marker,
            &format!("{VISIBLE_MARKER}{index}"),
            "scoped order changed"
        );
    }
    assert!(
        !first.to_string().contains(HIDDEN_MARKER),
        "the scoped page exposed a hidden monitor's alert: {first}"
    );
    let token = page_cursor(&first).expect("a non-empty page must carry its forward cursor");

    // The page filter is part of the token binding: a cursor minted with
    // `open=false` is not a cursor for the `open=true` projection.
    let filter_mismatch = call(
        &shared,
        Asserted::Agent("reader".to_string()),
        "monitor_alerts",
        &json!({
            "monitor": OPEN_MONITOR,
            "cursor_format": CURSOR_FORMAT,
            "open": true,
            "after": token,
        }),
    )
    .expect_err("a cursor must not cross the page filter");
    assert_refusal(filter_mismatch, "cross-filter alert cursor");
    // Its own filtered first page works, so the refusal is the binding.
    let open_only = call(
        &shared,
        Asserted::Agent("reader".to_string()),
        "monitor_alerts",
        &json!({"monitor": OPEN_MONITOR, "cursor_format": CURSOR_FORMAT, "open": true}),
    )
    .expect("the open-only scoped page");
    assert_scoped_page(&open_only, OPEN_MONITOR);

    // Hidden monitors gain alerts and the visible monitor gains two after the
    // first page was read.
    for (seq, monitor, task, marker) in [
        (
            6000,
            RESTRICTED_MONITOR,
            "restricted-task",
            format!("{HIDDEN_MARKER}between"),
        ),
        (
            6001,
            MIXED_MONITOR,
            "restricted-task",
            "CAD2215_AL_MIXED_BETWEEN".into(),
        ),
    ] {
        append_alert(world.state(), seq, monitor, task, &marker);
    }
    for index in 0..2 {
        append_alert(
            world.state(),
            7000 + index,
            OPEN_MONITOR,
            "open-task",
            &format!("{VISIBLE_AFTER_MARKER}{index}"),
        );
    }

    // The continuation resumes exactly after the first page and carries the
    // newly visible alerts, never the hidden ones.
    let second = scoped_page(&shared, "reader", OPEN_MONITOR, Some(&token));
    let second_markers = markers(&second);
    let rendered = second.to_string();
    assert!(
        !rendered.contains(HIDDEN_MARKER),
        "hidden insertion changed the continuation: {second}"
    );
    for index in 0..2 {
        assert!(
            second_markers.contains(&format!("{VISIBLE_AFTER_MARKER}{index}")),
            "the continuation skipped visible alert {index}: {second}"
        );
    }
    for marker in &second_markers {
        assert!(
            !first_markers.contains(marker),
            "the continuation repeated {marker}"
        );
    }

    // Walk to the terminal empty page; every visible alert appears once.
    let mut seen = first_markers.clone();
    seen.extend(second_markers);
    let mut after = page_cursor(&second).expect("the inserted alerts must be continuable");
    let mut terminal = None;
    for _ in 0..8 {
        let page = scoped_page(&shared, "reader", OPEN_MONITOR, Some(&after));
        assert!(
            !page.to_string().contains(HIDDEN_MARKER),
            "a later page exposed hidden activity: {page}"
        );
        if alerts(&page).is_empty() {
            terminal = Some((page, after.clone()));
            break;
        }
        for marker in markers(&page) {
            assert!(
                !seen.contains(&marker),
                "scoped pagination repeated {marker}"
            );
            seen.push(marker);
        }
        after = page_cursor(&page).expect("a non-empty page must carry its forward cursor");
    }
    let (terminal, supplied) = terminal.expect("pagination did not reach an empty page");
    assert_eq!(
        terminal["cursor"],
        json!(supplied),
        "the terminal empty page did not echo the supplied token: {terminal}"
    );
    let mut expected: Vec<String> = (0..105)
        .map(|index| format!("{VISIBLE_MARKER}{index}"))
        .collect();
    for index in 0..2 {
        expected.push(format!("{VISIBLE_AFTER_MARKER}{index}"));
    }
    assert_eq!(
        seen, expected,
        "the scoped alert feed lost or reordered alerts"
    );

    // Stable identity across a re-read, an active-key rotation and a restart.
    // The active key is not part of the persisted fingerprint, and row ids are
    // keyed by the identity key, so both must leave the ids intact.
    write_config(world.state(), &keys.with_active_b());
    let rotated = scoped_page(&shared, "reader", OPEN_MONITOR, None);
    assert_eq!(
        ids(&rotated),
        first_ids,
        "an active-key rotation changed stable alert ids"
    );
    // A cursor sealed under the previous active key still opens.
    let resumed = scoped_page(&shared, "reader", OPEN_MONITOR, Some(&token));
    assert!(
        !markers(&resumed).is_empty(),
        "an old cursor stopped opening after a legal rotation: {resumed}"
    );
    drop(shared);
    let shared = world.boot();
    let restarted = scoped_page(&shared, "reader", OPEN_MONITOR, None);
    assert_eq!(
        ids(&restarted),
        first_ids,
        "a daemon restart changed stable alert ids"
    );
}

#[test]
fn scoped_alert_acknowledgement_is_principal_and_target_bound_and_respects_revocation() {
    let keys = keys();
    let world = build_world(Some(&keys), 8);
    let shared = world.boot();

    let page = scoped_page(&shared, "reader", OPEN_MONITOR, None);
    let first_marker = format!("{VISIBLE_MARKER}0");
    let own_ack = alert_ack(&page, &first_marker);
    let peer_ack = alert_ack(&page, &format!("{VISIBLE_MARKER}1"));
    let later_ack = alert_ack(&page, &format!("{VISIBLE_MARKER}2"));

    // A numeric alert is not a scoped acknowledgement.
    let numeric = call(
        &shared,
        Asserted::Agent("reader".to_string()),
        "monitor_alert_ack",
        &json!({"monitor": OPEN_MONITOR, "alert": OPEN_FIRST_SEQ + 2}),
    )
    .expect_err("a scoped reader must not acknowledge by numeric position");
    assert_refusal(numeric, "numeric scoped ack");
    assert_eq!(
        alert_state(world.state(), OPEN_FIRST_SEQ + 2),
        "open",
        "a refused numeric ack changed the alert"
    );

    // The token is bound to its exact monitor: another readable monitor, a
    // hidden monitor and a mixed monitor are all refused, and their alerts stay
    // open.
    for (monitor, seq) in [
        (OTHER_OPEN_MONITOR, OTHER_FIRST_SEQ),
        (RESTRICTED_MONITOR, RESTRICTED_FIRST_SEQ),
        (MIXED_MONITOR, MIXED_FIRST_SEQ),
    ] {
        let crossed = ack(
            &shared,
            Asserted::Agent("reader".to_string()),
            monitor,
            own_ack.clone(),
        )
        .expect_err("an ack token must not acknowledge another monitor");
        assert_refusal(crossed, "cross-monitor ack");
        assert_eq!(
            alert_state(world.state(), seq),
            "open",
            "a cross-monitor ack changed {monitor}'s alert"
        );
    }

    // The token is bound to the proved reader: a peer with the same readable
    // projects cannot use it, and its own token acknowledges its own alert.
    let peer_page = scoped_page(&shared, "peer", OPEN_MONITOR, None);
    let peer_own = alert_ack(&peer_page, &format!("{VISIBLE_MARKER}1"));
    assert_ne!(
        peer_own, peer_ack,
        "two readers received the same ack token for one alert"
    );
    let stolen = ack(
        &shared,
        Asserted::Agent("reader".to_string()),
        OPEN_MONITOR,
        peer_own.clone(),
    )
    .expect_err("a reader must not use another reader's ack token");
    assert_refusal(stolen, "cross-reader ack");
    assert_eq!(
        alert_state(world.state(), OPEN_FIRST_SEQ + 1),
        "open",
        "a stolen ack changed the alert"
    );
    ack(
        &shared,
        Asserted::Agent("peer".to_string()),
        OPEN_MONITOR,
        peer_own,
    )
    .expect("the peer's own ack token must acknowledge its readable alert");
    assert_eq!(
        alert_state(world.state(), OPEN_FIRST_SEQ + 1),
        "acknowledged",
        "the peer's own acknowledgement did not persist"
    );

    // The reader's own token acknowledges exactly its alert, and the scoped
    // read afterwards shows the acknowledged state.
    ack(
        &shared,
        Asserted::Agent("reader".to_string()),
        OPEN_MONITOR,
        own_ack.clone(),
    )
    .expect("the reader's own ack token must acknowledge its readable alert");
    assert_eq!(
        alert_state(world.state(), OPEN_FIRST_SEQ),
        "acknowledged",
        "the reader's acknowledgement did not persist"
    );
    let refreshed = scoped_page(&shared, "reader", OPEN_MONITOR, None);
    let acknowledged_row = alerts(&refreshed)
        .iter()
        .find(|alert| alert["payload"]["marker"] == json!(first_marker))
        .expect("the acknowledged alert is missing from the scoped page");
    assert_eq!(
        acknowledged_row["state"],
        json!("acknowledged"),
        "{refreshed}"
    );

    // Revocation between the read that minted a token and the ack refuses the
    // ack and leaves the alert open; restoring access acknowledges with the
    // same token, so the refusal did not invalidate it.
    std::fs::write(world.visible_policy_path(), private_open_policy()).unwrap();
    let revoked_page = scoped_alerts(&shared, "reader", OPEN_MONITOR, None)
        .expect_err("a withdrawn reader must not read its monitor's alerts");
    assert_refusal(revoked_page, "withdrawn alert read");
    let revoked_ack = ack(
        &shared,
        Asserted::Agent("reader".to_string()),
        OPEN_MONITOR,
        later_ack.clone(),
    )
    .expect_err("a withdrawn reader must not acknowledge its monitor's alert");
    assert_refusal(revoked_ack, "withdrawn alert ack");
    assert_eq!(
        alert_state(world.state(), OPEN_FIRST_SEQ + 2),
        "open",
        "a withdrawn acknowledgement changed the alert"
    );

    std::fs::write(world.visible_policy_path(), visible_project_policy()).unwrap();
    ack(
        &shared,
        Asserted::Agent("reader".to_string()),
        OPEN_MONITOR,
        later_ack,
    )
    .expect("the pre-revocation ack token must still work after access returns");
    assert_eq!(
        alert_state(world.state(), OPEN_FIRST_SEQ + 2),
        "acknowledged",
        "the restored acknowledgement did not persist"
    );

    // Another workspace with the same key material refuses both the cursor and
    // the ack token, while its own reader page works.
    let other_world = build_world(Some(&keys), 8);
    let other_shared = other_world.boot();
    let other_page = scoped_page(&shared, "reader", OPEN_MONITOR, None);
    let foreign_cursor = page_cursor(&other_page);
    let cross_workspace_read = scoped_alerts(
        &other_shared,
        "reader",
        OPEN_MONITOR,
        foreign_cursor.as_deref(),
    )
    .expect_err("an alert cursor must not cross workspaces");
    assert_refusal(cross_workspace_read, "cross-workspace alert cursor");
    let foreign_ack = alert_ack(&other_page, &format!("{VISIBLE_MARKER}3"));
    let cross_workspace_ack = ack(
        &other_shared,
        Asserted::Agent("reader".to_string()),
        OPEN_MONITOR,
        foreign_ack,
    )
    .expect_err("an ack token must not cross workspaces");
    assert_refusal(cross_workspace_ack, "cross-workspace alert ack");
    assert_eq!(
        alert_state(other_world.state(), OPEN_FIRST_SEQ + 3),
        "open",
        "a cross-workspace ack changed the other workspace's alert"
    );
    let own = scoped_page(&other_shared, "reader", OPEN_MONITOR, None);
    assert!(!alerts(&own).is_empty(), "{own}");
}

#[test]
fn scoped_reads_require_the_format_and_the_keyring_while_operator_controls_stay_numeric() {
    let keys = keys();
    let world = build_world(Some(&keys), 8);
    let shared = world.boot();

    // A scoped read names the opaque format; an unknown or numeric format is
    // refused and never served as numeric positions.
    for params in [
        json!({"monitor": OPEN_MONITOR}),
        json!({"monitor": OPEN_MONITOR, "cursor_format": "numeric"}),
    ] {
        let refused = call(
            &shared,
            Asserted::Agent("reader".to_string()),
            "monitor_alerts",
            &params,
        )
        .expect_err("a scoped alert read must name the opaque format");
        assert_refusal(refused, "scoped format requirement");
    }
    // A numeric or non-string `after` is refused rather than coerced.
    for after in [json!(OPEN_FIRST_SEQ), json!(null), json!(true), json!([1])] {
        let refused = call(
            &shared,
            Asserted::Agent("reader".to_string()),
            "monitor_alerts",
            &json!({
                "monitor": OPEN_MONITOR,
                "cursor_format": CURSOR_FORMAT,
                "after": after,
            }),
        )
        .expect_err("a scoped alert read must continue with an opaque token");
        assert_refusal(refused, "scoped continuation shape");
    }

    // The operator keeps numeric positions, numeric continuation and numeric
    // acknowledgement for every monitor, including the hidden ones.
    let operator = call(
        &shared,
        Asserted::Operator,
        "monitor_alerts",
        &json!({"monitor": OPEN_MONITOR}),
    )
    .expect("operator alert read");
    for alert in alerts(&operator) {
        assert!(
            alert["seq"].as_i64().is_some(),
            "the operator alert lost its numeric position: {alert}"
        );
        assert!(
            alert["event_seq"].as_i64().is_some() && alert["fingerprint"].is_string(),
            "the operator alert lost its numeric audit fields: {alert}"
        );
    }
    assert!(
        operator["cursor"].as_i64().is_some(),
        "the operator page has no numeric cursor: {operator}"
    );
    let continued = call(
        &shared,
        Asserted::Operator,
        "monitor_alerts",
        &json!({"monitor": OPEN_MONITOR, "after": OPEN_FIRST_SEQ + 5}),
    )
    .expect("operator numeric continuation");
    for alert in alerts(&continued) {
        assert!(
            alert["seq"].as_i64().unwrap_or(0) > OPEN_FIRST_SEQ + 5,
            "the operator numeric cursor was ignored: {alert}"
        );
    }
    let hidden = call(
        &shared,
        Asserted::Operator,
        "monitor_alerts",
        &json!({"monitor": RESTRICTED_MONITOR}),
    )
    .expect("the operator reads a monitor a scoped reader cannot see");
    assert_eq!(alerts(&hidden).len(), 11, "{hidden}");
    call(
        &shared,
        Asserted::Operator,
        "monitor_alert_ack",
        &json!({"monitor": OPEN_MONITOR, "alert": OPEN_FIRST_SEQ}),
    )
    .expect("operator numeric acknowledgement");
    assert_eq!(
        alert_state(world.state(), OPEN_FIRST_SEQ),
        "acknowledged",
        "the operator acknowledgement did not persist"
    );

    // The monitor's internal `event_cursor` watermark is part of the numeric
    // document only: a scoped reader of the same monitor never receives it.
    let scoped_show = call(
        &shared,
        Asserted::Agent("reader".to_string()),
        "monitor_show",
        &json!({"monitor": OPEN_MONITOR}),
    )
    .expect("scoped monitor show");
    assert_eq!(
        scoped_show[cadence_core::board_api::READER_SCOPE_FIELD],
        json!(crate::daemon::monitors_rpc::MONITOR_READER_SCOPE_ACK),
        "{scoped_show}"
    );
    assert!(
        scoped_show["monitor"].get("event_cursor").is_none(),
        "scoped monitor show exposed the internal event watermark: {scoped_show}"
    );
    let operator_show = call(
        &shared,
        Asserted::Operator,
        "monitor_show",
        &json!({"monitor": OPEN_MONITOR}),
    )
    .expect("operator monitor show");
    assert!(
        operator_show["monitor"]["event_cursor"].as_i64().is_some(),
        "the operator lost its numeric monitor watermark: {operator_show}"
    );
    let scoped_list = call(
        &shared,
        Asserted::Agent("reader".to_string()),
        "monitor_list",
        &json!({}),
    )
    .expect("scoped monitor list");
    for monitor in scoped_list["monitors"].as_array().expect("monitor list") {
        assert!(
            monitor.get("event_cursor").is_none(),
            "scoped monitor list exposed an internal event watermark: {monitor}"
        );
    }
    let operator_list = call(&shared, Asserted::Operator, "monitor_list", &json!({}))
        .expect("operator monitor list");
    for monitor in operator_list["monitors"].as_array().expect("monitor list") {
        assert!(
            monitor["event_cursor"].as_i64().is_some(),
            "the operator lost a numeric monitor watermark: {monitor}"
        );
    }

    // Missing key material refuses the scoped read; the operator numeric read
    // is unaffected.
    let unconfigured = build_world(None, 4);
    let unconfigured_shared = unconfigured.boot();
    let refused = scoped_alerts(&unconfigured_shared, "reader", OPEN_MONITOR, None)
        .expect_err("a scoped alert read without a keyring must fail closed");
    assert_refusal(refused, "unconfigured scoped alert read");
    let operator = call(
        &unconfigured_shared,
        Asserted::Operator,
        "monitor_alerts",
        &json!({"monitor": OPEN_MONITOR}),
    )
    .expect("the operator numeric read needs no key material");
    assert!(
        operator["cursor"].as_i64().is_some(),
        "the operator read lost its numeric cursor: {operator}"
    );
    for alert in alerts(&operator) {
        assert!(alert["seq"].as_i64().is_some(), "{alert}");
    }
}

/// A selected monitor whose covered task is rebound to a hidden project after
/// selection must not be returned: the selected-row reproof refuses the page.
/// The list then omits it, and a fresh alert read refuses.
#[test]
fn selected_monitor_task_moved_to_a_private_project_is_not_returned() {
    let keys = keys();
    // Baseline: the same fixture monitor is readable and returns its alerts.
    let control = build_world(Some(&keys), 8);
    let control_shared = control.boot();
    let baseline = scoped_page(&control_shared, "reader", OPEN_MONITOR, None);
    assert_eq!(markers(&baseline).len(), 8, "{baseline}");

    let world = build_world(Some(&keys), 8);
    let state = world.state().to_path_buf();
    let fired = Arc::new(AtomicBool::new(false));
    let shared = world.boot_with(|opts| {
        let fired = Arc::clone(&fired);
        let state = state.clone();
        opts.monitor_read_snapshot_hook = Some(Arc::new(move |tag: &str| {
            if tag != "alerts" || fired.swap(true, Ordering::SeqCst) {
                return;
            }
            // Rebind the selected monitor's covered task to the private
            // project's job: the readable project set is unchanged, but the
            // task -> job -> issue binding now resolves to HID-1 (private).
            let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
            db.execute(
                "UPDATE tasks SET job_id='hidden-job' WHERE id='open-task'",
                [],
            )
            .unwrap();
        }));
    });

    let refused = scoped_alerts(&shared, "reader", OPEN_MONITOR, None)
        .expect_err("a selected monitor whose covered task moved private must not be returned");
    assert_refusal(refused, "selected monitor task moved private");
    assert!(
        fired.load(Ordering::SeqCst),
        "the monitor read hook never fired"
    );

    // The list omits the now-private monitor and keeps the readable one.
    let list = call(
        &shared,
        Asserted::Agent("reader".to_string()),
        "monitor_list",
        &json!({}),
    )
    .expect("the still-readable monitor must list");
    let ids = list["monitors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|monitor| monitor["id"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(
        !ids.contains(&OPEN_MONITOR.to_string()),
        "the now-private monitor was returned by the list: {list}"
    );
    assert!(ids.contains(&OTHER_OPEN_MONITOR.to_string()), "{list}");
    assert!(!ids.contains(&RESTRICTED_MONITOR.to_string()), "{list}");

    // A fresh alert read refuses: the monitor's coverage is not readable now.
    let fresh = scoped_alerts(&shared, "reader", OPEN_MONITOR, None)
        .expect_err("a fresh read of the now-private monitor must be refused");
    assert_refusal(fresh, "fresh read after selected task moved");
}

/// An unrelated hidden issue/job/event inserted between selection and response
/// must not refuse the read or change the visible monitor list or alert page.
#[test]
fn unrelated_hidden_issue_and_job_insertion_does_not_change_the_visible_monitor_page() {
    let keys = keys();
    let control = build_world(Some(&keys), 8);
    let control_shared = control.boot();
    let control_list = call(
        &control_shared,
        Asserted::Agent("reader".to_string()),
        "monitor_list",
        &json!({}),
    )
    .expect("control monitor list");
    let control_page = scoped_page(&control_shared, "reader", OPEN_MONITOR, None);

    let world = build_world(Some(&keys), 8);
    let pm = world.pm.clone();
    let state = world.state().to_path_buf();
    let fired = Arc::new(AtomicBool::new(false));
    let shared = world.boot_with(|opts| {
        let fired = Arc::clone(&fired);
        let pm = pm.clone();
        let state = state.clone();
        opts.monitor_read_snapshot_hook = Some(Arc::new(move |_tag: &str| {
            if fired.swap(true, Ordering::SeqCst) {
                return;
            }
            insert_unrelated_hidden_work(&pm, &state, "CAD2215_AL_INSERTED_HIDDEN");
        }));
    });

    let list = call(
        &shared,
        Asserted::Agent("reader".to_string()),
        "monitor_list",
        &json!({}),
    )
    .expect("an unrelated hidden insertion must not refuse the monitor list");
    assert_eq!(
        stable_monitors(&list),
        stable_monitors(&control_list),
        "an unrelated hidden insertion changed the visible monitor list: {list}"
    );

    let page = scoped_page(&shared, "reader", OPEN_MONITOR, None);
    assert_eq!(
        markers(&page),
        markers(&control_page),
        "an unrelated hidden insertion changed the visible alert page: {page}"
    );
    for needle in ["CAD2215_AL_INSERTED_HIDDEN", HIDDEN_MARKER] {
        assert!(
            !page.to_string().contains(needle),
            "the visible alert page exposed {needle}: {page}"
        );
    }
    let token = page_cursor(&page).expect("the first page must carry a token");
    let next = scoped_page(&shared, "reader", OPEN_MONITOR, Some(&token));
    assert!(
        alerts(&next).is_empty(),
        "the continuation changed after an unrelated hidden insertion: {next}"
    );
}

/// Control: the private job's current native PM can read the monitor and
/// acknowledge its alert with the opaque token even though the job is
/// member-private, because the provenance is trusted and unique.
#[test]
fn native_pm_opaque_ack_control_succeeds() {
    let keys = keys();
    let world = pm_private_alert_world(&keys);
    let shared = world.boot();
    let token = reader_alert_token(&shared);
    let acked = scoped(Asserted::Agent("reader".to_string()), || {
        shared.dispatch(
            "monitor_alert_ack",
            &json!({"monitor": PM_MONITOR, "ack": token}),
            std::process::id(),
        )
    })
    .expect("the PM's own opaque ack token must acknowledge the private alert");
    assert_eq!(acked["alert"]["state"], json!("acknowledged"), "{acked}");
    assert_eq!(
        alert_state(world.state(), PM_ALERT),
        "acknowledged",
        "the control acknowledgement did not persist"
    );
}

/// The authorization race: the ack's preliminary reproof passes with a unique
/// settled owner, then a second real `assist_request_bind` for a DIFFERENT
/// `(org, sub, request_key)` is bound to the SAME job while the ack is held
/// before the authoritative write. Provenance becomes ambiguous, which removes
/// even the PM/assignee exception; the write must re-authorize and refuse.
#[test]
fn native_pm_ack_refuses_when_the_job_provenance_becomes_ambiguous_before_the_write() {
    let keys = keys();
    let world = pm_private_alert_world(&keys);
    let state = world.state().to_path_buf();
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    let entered = Arc::new(Mutex::new(entered_tx));
    let resume = Arc::new(Mutex::new(Some(resume_rx)));
    let raced = Arc::new(AtomicBool::new(false));
    let shared = world.boot_with(|opts| {
        let entered = Arc::clone(&entered);
        let resume = Arc::clone(&resume);
        let raced = Arc::clone(&raced);
        opts.monitor_ack_writer_hook = Some(Arc::new(move || {
            if raced.swap(true, Ordering::SeqCst) {
                return;
            }
            entered.lock().unwrap().send(()).expect("signal the test");
            let receiver = resume.lock().unwrap().take().expect("resume receiver");
            receiver.recv().expect("wait for the ambiguity to commit");
        }));
    });

    // The token is minted while provenance is unique; the alert is untouched.
    let token = reader_alert_token(&shared);
    let events_before = ack_event_count(&state, PM_MONITOR);
    assert_eq!(alert_state(&state, PM_ALERT), "open");
    assert_eq!(
        shared
            .store
            .assist_job_owner(PM_JOB)
            .expect("the unique owner probe"),
        Some((PRIVATE_ORG.to_string(), PRIVATE_OWNER_SUB.to_string()))
    );

    let ack_shared = Arc::clone(&shared);
    let ack_token = token.clone();
    let ack_thread = thread::spawn(move || {
        scoped(Asserted::Agent("reader".to_string()), || {
            ack_shared.dispatch(
                "monitor_alert_ack",
                &json!({"monitor": PM_MONITOR, "ack": ack_token}),
                std::process::id(),
            )
        })
    });

    // The ack is now held after its preliminary reproof and before the write.
    entered_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the ack never reached the pre-write hook");
    // A real second owner bound to the same job makes provenance ambiguous.
    let bound = shared
        .store
        .assist_request_bind(
            PRIVATE_ORG,
            OTHER_OWNER_SUB,
            "req-other",
            PM_JOB,
            "other title",
            "other body",
            "other acceptance",
            "cred-other",
        )
        .expect("the second owner bind must commit");
    assert_eq!(bound, PM_JOB, "the second request bound another job");
    assert!(
        shared
            .store
            .assist_job_owner(PM_JOB)
            .expect("the ambiguous owner probe")
            .is_none(),
        "the second owner did not make provenance ambiguous"
    );
    // The real denial snapshot proves the PM/assignee exception is gone.
    let denied = shared
        .store
        .private_job_denials(crate::store::PrivateJobReader::Agent("reader"))
        .expect("the denial snapshot");
    assert!(
        denied.jobs.contains(PM_JOB),
        "ambiguous provenance left the private job readable: {denied:?}"
    );
    assert!(
        denied.tasks.contains(PM_TASK),
        "ambiguous provenance left the PM/assignee task readable: {denied:?}"
    );
    resume_tx.send(()).expect("resume the held ack");

    let result = ack_thread.join().expect("the ack thread must not panic");
    assert!(
        result.is_err(),
        "the ack succeeded after its provenance became ambiguous: {result:?}"
    );
    assert_eq!(
        alert_state(&state, PM_ALERT),
        "open",
        "a refused ack changed the alert"
    );
    assert_eq!(
        ack_event_count(&state, PM_MONITOR),
        events_before,
        "a refused ack wrote an acknowledgement event"
    );
}
