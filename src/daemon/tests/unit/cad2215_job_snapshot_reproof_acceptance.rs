//! CAD-2215 independent native read reproof acceptance.
//! Changes actual project visibility during a real event/detail RPC. This
//! tests project-access revocation, not a private user-membership relay.
//!
//! A non-operator `job_events` read is scoped: it names the opaque cursor
//! format, walks on the opaque cursor the page returned, and needs an
//! explicitly initialized keyring. The key files below are fixture-owned and
//! live outside the state directory and the tracker; the only setup
//! operation is the real operator `scoped_cursor_initialize` RPC.
use super::*;
use crate::test_seam::{scoped, Asserted};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Owner-provisioned key material for the scoped cursor codec. Nothing here
/// generates, derives or persists key bytes inside the state dir or tracker.
struct CursorKeys {
    _dir: tempfile::TempDir,
    config: Value,
}

fn private_dir(path: &std::path::Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn write_private(path: &std::path::Path, bytes: &[u8]) {
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

fn cursor_keys() -> CursorKeys {
    let dir = tempfile::Builder::new()
        .prefix("cad2215-keys-")
        .tempdir()
        .unwrap();
    private_dir(dir.path());
    for (name, byte) in [("identity.key", 0xa1u8), ("cursor-a.key", 0xb2u8)] {
        write_private(&dir.path().join(name), &[byte; 32]);
    }
    let id = "01".repeat(16);
    let config = json!({
        "version": 1,
        "identity_key_file": dir.path().join("identity.key").to_str().unwrap(),
        "active_key_id": &id,
        "keys": [{ "id": &id, "key_file": dir.path().join("cursor-a.key").to_str().unwrap() }],
    });
    CursorKeys { _dir: dir, config }
}

/// Write the fixed keyring record into `state` and bind it through the real
/// operator RPC. A scoped read without this fails closed.
fn initialize_cursor_keyring(shared: &Arc<Shared>, state: &std::path::Path, keys: &CursorKeys) {
    write_private(
        &state.join("scoped-cursors.json"),
        &serde_json::to_vec(&keys.config).unwrap(),
    );
    let answer = scoped(Asserted::Operator, || {
        shared.dispatch("scoped_cursor_initialize", &json!({}), std::process::id())
    })
    .expect("the operator must initialize the scoped-cursor keyring");
    assert_eq!(answer["initialized"], json!(true), "{answer}");
}

/// A scoped page carries no global sequence position at any depth.
fn assert_no_global_seq(value: &Value, what: &str) {
    match value {
        Value::Object(map) => {
            assert!(
                !map.contains_key("seq"),
                "{what} exposed a global sequence position: {value}"
            );
            for nested in map.values() {
                assert_no_global_seq(nested, what);
            }
        }
        Value::Array(items) => {
            for nested in items {
                assert_no_global_seq(nested, what);
            }
        }
        _ => {}
    }
}

/// A refusal must be the access refusal, never a keyring/format/parse failure
/// that would make the authority check pass for the wrong reason.
fn assert_revoked_for_access(error: &Error, method: &str, marker: &str) {
    let rendered = format!("{error:?} {error}");
    // A withdrawn project is the plain rejection; every cursor/keyring failure
    // is a structured refusal with its own code and must not be mistaken for it.
    assert_eq!(
        error.kind(),
        "rejected",
        "{method} refused with an unexpected error class: {rendered}"
    );
    assert_eq!(
        error.code(),
        None,
        "{method} refused with a structured code, not withdrawn access: {rendered}"
    );
    for wrong_reason in ["cursor_format", "scoped cursor", "scoped_cursor"] {
        assert!(
            !rendered.contains(wrong_reason),
            "{method} refused for {wrong_reason}, not withdrawn access: {rendered}"
        );
    }
    assert!(
        !rendered.contains(marker),
        "{method} refusal leaked the protected marker: {rendered}"
    );
}

/// `reader_params` is the scoped-reader request; `operator_params` is the
/// proven operator's numeric request. Only `job_events` differs between them.
fn run_snapshot_revocation(
    method: &str,
    reader_params: Value,
    operator_params: Value,
    target: &str,
    marker: &str,
) {
    let temp = tempfile::tempdir().unwrap();
    // The scoped-cursor record is read through the strict 0700 state chain.
    private_dir(temp.path());
    let pm = temp.path().join("tracker");
    crate::issue::Pm::init(&pm).unwrap();
    let project = pm.join("race");
    let issue = project.join("RACE-1");
    std::fs::create_dir_all(&issue).unwrap();
    let policy_path = project.join("project.yaml");
    std::fs::write(&policy_path, "key: race\nprefix: RACE\n").unwrap();
    std::fs::write(issue.join("issue.md"),
        "---\nid: RACE-1\ntitle: Race fixture\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n").unwrap();

    let armed = Arc::new(AtomicBool::new(false));
    let (selected_tx, selected_rx) = std::sync::mpsc::channel::<(String, String)>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let wanted = method.to_string();
    let event_hook: Arc<dyn Fn(&str) + Send + Sync> = {
        let armed = armed.clone();
        let wanted = wanted.clone();
        let selected_tx = selected_tx.clone();
        let release_rx = release_rx.clone();
        Arc::new(move |job| {
            if wanted != "job_events" || !armed.swap(false, Ordering::SeqCst) {
                return;
            }
            selected_tx
                .send(("job_events".to_string(), job.to_string()))
                .unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(15))
                .expect("release job-events snapshot");
        })
    };
    #[allow(clippy::type_complexity)] // one fixture callback, one shape
    let detail_hook: Arc<dyn Fn(&str, &str) + Send + Sync> = {
        let armed = armed.clone();
        let wanted = wanted.clone();
        let selected_tx = selected_tx.clone();
        let release_rx = release_rx.clone();
        Arc::new(move |kind, id| {
            if kind != wanted || !armed.swap(false, Ordering::SeqCst) {
                return;
            }
            selected_tx
                .send((kind.to_string(), id.to_string()))
                .unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(15))
                .expect("release detail snapshot");
        })
    };
    let mut opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    opts.job_events_snapshot_hook = Some(event_hook);
    opts.job_detail_snapshot_hook = Some(detail_hook);
    let shared = Shared::new(temp.path(), &opts).unwrap();
    register(&shared, temp.path(), "reader");
    // Scoped job-event reads re-prove an initialized keyring; a detail read
    // needs none, and no other surface here reads one.
    let _keys = (method == "job_events").then(|| {
        // Strict keyring admission pins the tracker chain: a fixture tracker
        // must be owner-only (the runner umask can otherwise leave it 0775).
        private_dir(&pm);
        let keys = cursor_keys();
        initialize_cursor_keyring(&shared, temp.path(), &keys);
        keys
    });
    let db = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
    db.execute_batch("INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated) VALUES ('snapshot-job','JOB_DETAIL_SECRET_2215','spec.md','reader','RACE-1','open',1,1); INSERT INTO tasks(id,job_id,title,assignee,state,created,updated) VALUES ('snapshot-task','snapshot-job','TASK_DETAIL_SECRET_2215','reader','running',1,1);").unwrap();
    shared
        .store
        .event_public_scoped(
            "reader",
            "fixture_progress",
            json!({"marker":"JOB_EVENT_SECRET_2215"}),
            Some("snapshot-job"),
            Some("snapshot-task"),
        )
        .unwrap();
    let pid = std::process::id();
    let reader = || Asserted::Agent("reader".to_string());
    scoped(reader(), || {
        assert!(
            matches!(&shared.agent_caller(pid, "acceptance reader").unwrap(), AgentCaller::Agent(alias) if alias == "reader")
        )
    });
    scoped(Asserted::Operator, || {
        assert!(matches!(
            shared.agent_caller(pid, "acceptance operator").unwrap(),
            AgentCaller::Operator
        ))
    });
    let call = |who: Asserted, params: &Value| scoped(who, || shared.dispatch(method, params, pid));
    let positive = call(reader(), &reader_params)
        .unwrap_or_else(|e| panic!("authorized {method} positive failed: {e}"));
    assert!(
        positive.to_string().contains(marker),
        "authorized {method} lost actual marker: {positive}"
    );
    if method == "job_events" {
        // A scoped page names its reader mode, keeps its stable keyed identity
        // and carries no global position; its only continuation is a token.
        assert_eq!(
            positive[cadence_core::board_api::READER_SCOPE_FIELD],
            json!(cadence_core::board_api::JOB_EVENTS_SCOPE_ACK),
            "scoped {method} page did not name its reader mode: {positive}"
        );
        assert_no_global_seq(&positive, "scoped job event page");
        let cursor = positive["cursor"]
            .as_str()
            .filter(|cursor| !cursor.is_empty())
            .unwrap_or_else(|| panic!("scoped {method} page has no opaque cursor: {positive}"));
        assert!(
            cursor.parse::<i64>().is_err(),
            "scoped {method} cursor is a numeric position: {cursor}"
        );
        if !reader_params
            .get("tail")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            // A forward continuation walks on the returned opaque cursor — the
            // numeric `after` the operator still uses is refused for a scoped
            // reader, so the feed cannot fall back to shared sequence values.
            let mut continuation = reader_params.clone();
            continuation["after"] = positive["cursor"].clone();
            let empty = call(reader(), &continuation).expect("authorized job event continuation");
            assert!(
                empty["events"].as_array().unwrap().is_empty(),
                "the opaque continuation re-served rows: {empty}"
            );
            assert_eq!(
                empty["cursor"], positive["cursor"],
                "an empty opaque continuation did not echo its cursor: {empty}"
            );
            assert_no_global_seq(&empty, "empty scoped job event continuation");
            let numeric = call(reader(), &{
                let mut numeric = reader_params.clone();
                numeric["after"] = json!(0);
                numeric
            })
            .expect_err("a scoped reader must not be served a numeric position");
            assert_eq!(
                numeric.code(),
                Some("job_events_cursor_numeric"),
                "a numeric continuation was refused for another reason: {numeric:?}"
            );
        }
    }
    let operator_before =
        call(Asserted::Operator, &operator_params).expect("operator positive before revoke");
    assert!(operator_before.to_string().contains(marker));

    armed.store(true, Ordering::SeqCst);
    let shared_read = Arc::clone(&shared);
    let thread_method = method.to_string();
    let thread_params = reader_params.clone();
    let pending = std::thread::spawn(move || {
        scoped(Asserted::Agent("reader".to_string()), || {
            shared_read.dispatch(&thread_method, &thread_params, std::process::id())
        })
    });
    let (actual_method, actual_id) = selected_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("actual selected snapshot not observed");
    assert_eq!(actual_method, method);
    assert_eq!(actual_id, target);
    // A real current policy edit withdraws the native agent's project access
    // while its already-selected rows/detail are held behind the hook.
    std::fs::write(
        &policy_path,
        "key: race\nprefix: RACE\npolicy:\n  version: 1\n  visibility: private\n",
    )
    .unwrap();
    release_tx.send(()).expect("release selected snapshot");
    let result = pending.join().expect("read thread panicked");
    assert!(
        result.is_err(),
        "{method} returned a page/detail after access revocation: {result:?}"
    );
    assert!(
        !format!("{result:?}").contains(marker),
        "{method} refusal leaked protected marker: {result:?}"
    );
    if let Err(error) = &result {
        assert_revoked_for_access(error, method, marker);
    }
    let fresh = call(reader(), &reader_params);
    assert!(
        fresh.is_err(),
        "fresh {method} read still admitted after project revocation"
    );
    if let Err(error) = &fresh {
        assert_revoked_for_access(error, method, marker);
    }
    let operator_after =
        call(Asserted::Operator, &operator_params).expect("operator recovery after revoke");
    assert!(
        operator_after.to_string().contains(marker),
        "operator recovery lost protected {method} data: {operator_after}"
    );
}

#[test]
fn job_events_immediate_forward_and_tail_reprove_project_access_after_selection() {
    for (reader_params, operator_params) in [
        (
            json!({"job":"snapshot-job", "cursor_format": cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT}),
            json!({"job":"snapshot-job", "after":0}),
        ),
        (
            json!({"job":"snapshot-job", "cursor_format": cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT, "tail":true}),
            json!({"job":"snapshot-job", "tail":true}),
        ),
    ] {
        run_snapshot_revocation(
            "job_events",
            reader_params,
            operator_params,
            "snapshot-job",
            "JOB_EVENT_SECRET_2215",
        );
    }
}

#[test]
fn job_and_task_details_reprove_project_access_after_assembly() {
    run_snapshot_revocation(
        "job_show",
        json!({"job":"snapshot-job"}),
        json!({"job":"snapshot-job"}),
        "snapshot-job",
        "JOB_DETAIL_SECRET_2215",
    );
    run_snapshot_revocation(
        "task_show",
        json!({"task":"snapshot-task"}),
        json!({"task":"snapshot-task"}),
        "snapshot-task",
        "TASK_DETAIL_SECRET_2215",
    );
}

/// A new issue arriving in a READABLE project between selection and reproof
/// must not refuse the selected `job_list`: the reader's readable project set
/// did not change, and the new issue is not in the reply. Comparing the whole
/// issue map would turn an unrelated arrival into an error signal — the same
/// oracle the scoped event reader already avoids.
#[test]
fn unrelated_issue_during_job_list_does_not_refuse_the_selected_page() {
    let temp = tempfile::tempdir().unwrap();
    private_dir(temp.path());
    let pm = temp.path().join("tracker");
    crate::issue::Pm::init(&pm).unwrap();
    let project = pm.join("race");
    let issue = project.join("RACE-1");
    std::fs::create_dir_all(&issue).unwrap();
    std::fs::write(project.join("project.yaml"), "key: race\nprefix: RACE\n").unwrap();
    std::fs::write(
        issue.join("issue.md"),
        "---\nid: RACE-1\ntitle: Race fixture\nstatus: backlog\npriority: P2\n\
         created: '2026-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();

    let fired = Arc::new(AtomicBool::new(false));
    #[allow(clippy::type_complexity)] // one fixture callback, one shape
    let hook: Arc<dyn Fn(&str, &str) + Send + Sync> = {
        let fired = fired.clone();
        let pm = pm.clone();
        Arc::new(move |kind, _| {
            if kind != "job_list" || fired.swap(true, Ordering::SeqCst) {
                return;
            }
            // A real tracker change in the same readable project, committed
            // while the reply is assembled.
            let late = pm.join("race").join("RACE-9");
            std::fs::create_dir_all(&late).unwrap();
            std::fs::write(
                late.join("issue.md"),
                "---\nid: RACE-9\ntitle: Later issue\nstatus: backlog\npriority: P2\n\
                 created: '2026-01-01T00:00:00Z'\n---\n",
            )
            .unwrap();
        })
    };
    let mut opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    opts.job_list_snapshot_hook = Some(hook);
    let shared = Shared::new(temp.path(), &opts).unwrap();
    register(&shared, temp.path(), "reader");
    let db = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
    db.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated) \
         VALUES ('snapshot-job','LIST_SECRET_2215','spec.md','reader','RACE-1','open',1,1);
         INSERT INTO tasks(id,job_id,title,assignee,state,created,updated) \
         VALUES ('snapshot-task','snapshot-job','LIST_TASK_2215','reader','running',1,1);",
    )
    .unwrap();

    let pid = std::process::id();
    let page = scoped(Asserted::Agent("reader".to_string()), || {
        shared.dispatch("job_list", &json!({}), pid)
    })
    .expect("an unrelated readable issue refused a selected job list");
    assert!(
        fired.load(Ordering::SeqCst),
        "the fixture interleaving never fired"
    );
    let ids = page["jobs"]
        .as_array()
        .expect("job list has no jobs array")
        .iter()
        .filter_map(|job| job["id"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["snapshot-job"], "job list changed: {page}");
    assert!(
        pm.join("race/RACE-9/issue.md").is_file(),
        "the fixture did not commit the interleaved issue"
    );
}

/// The mirror case: a returned job whose ISSUE leaves the reader's project set
/// between selection and reproof must refuse the page. The readable project
/// SET is unchanged by such a move, so only a re-admission of the issues the
/// reply names can catch it.
#[test]
fn a_carried_issue_moved_out_of_reach_refuses_the_selected_page() {
    let temp = tempfile::tempdir().unwrap();
    private_dir(temp.path());
    let pm = temp.path().join("tracker");
    crate::issue::Pm::init(&pm).unwrap();
    let race = pm.join("race");
    let issue = race.join("RACE-1");
    std::fs::create_dir_all(&issue).unwrap();
    std::fs::write(race.join("project.yaml"), "key: race\nprefix: RACE\n").unwrap();
    std::fs::write(
        issue.join("issue.md"),
        "---\nid: RACE-1\ntitle: Race fixture\nstatus: backlog\npriority: P2\n\
         created: '2026-01-01T00:00:00Z'\n---\n",
    )
    .unwrap();
    // A project this reader cannot see, ready to receive the issue.
    let secret = pm.join("secret");
    std::fs::create_dir_all(secret.join("RACE-1")).unwrap();
    // `visibility: private` is what makes a project unreadable to this reader.
    std::fs::write(
        secret.join("project.yaml"),
        "key: secret\nprefix: SEC\npolicy:\n  version: 1\n  visibility: private\n",
    )
    .unwrap();

    let fired = Arc::new(AtomicBool::new(false));
    #[allow(clippy::type_complexity)] // one fixture callback, one shape
    let hook: Arc<dyn Fn(&str, &str) + Send + Sync> = {
        let fired = fired.clone();
        let pm = pm.clone();
        Arc::new(move |kind, _| {
            if kind != "job_list" || fired.swap(true, Ordering::SeqCst) {
                return;
            }
            // Move the issue the reply names into the unreadable project while
            // the reply is assembled: same id, different project.
            let hidden = pm.join("secret").join("RACE-1");
            std::fs::write(
                hidden.join("issue.md"),
                "---\nid: RACE-1\ntitle: Race fixture\nstatus: backlog\npriority: P2\n\
                 created: '2026-01-01T00:00:00Z'\n---\n",
            )
            .unwrap();
            std::fs::remove_dir_all(pm.join("race").join("RACE-1")).unwrap();
        })
    };
    let mut opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    opts.job_list_snapshot_hook = Some(hook);
    let shared = Shared::new(temp.path(), &opts).unwrap();
    register(&shared, temp.path(), "reader");
    let db = rusqlite::Connection::open(temp.path().join("cadence.sqlite3")).unwrap();
    db.execute_batch(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,state,created,updated) \
         VALUES ('snapshot-job','LIST_SECRET_2215','spec.md','reader','RACE-1','open',1,1);",
    )
    .unwrap();

    let pid = std::process::id();
    let result = scoped(Asserted::Agent("reader".to_string()), || {
        shared.dispatch("job_list", &json!({}), pid)
    });
    assert!(
        fired.load(Ordering::SeqCst),
        "the fixture interleaving never fired"
    );
    let error = result.expect_err("a carried issue moved out of reach must refuse the page");
    assert_eq!(error.kind(), "rejected", "{error:?}");
    assert!(
        format!("{error:?}").contains("job list refused"),
        "the refusal did not name the reproof: {error:?}"
    );
}
