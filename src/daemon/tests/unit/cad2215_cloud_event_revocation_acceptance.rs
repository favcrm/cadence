//! CAD-2215: independent acceptance for cloud-message event authority.
//!
//! A scoped agent reader must not receive an `agent_events` page whose rows it
//! was admitted to see at selection time if the cloud authority behind those
//! rows was withdrawn while the page was being built. The refusal is proven
//! through the real admission path, not a mock echo:
//!
//! * a real `cloud_agents` row, an active `local_peer` source grant, a real
//!   enrolled peer credential and a settled `cloud_agent_operations` row make
//!   `admit_cloud_projection` admit the message;
//! * `agent_events` selects it (the test-only selection hook asserts the row is
//!   actually present), then revokes the grant through the real store API;
//! * the page must be refused with the reader-access-changed rejection for the
//!   tail page, the `wait` page, a foreign-alias message, and a tail whose
//!   older `has_older` witness is the revoked row;
//! * revoking an alias NO selected row names must still return the page, so the
//!   refusal is not an activity oracle.
//!
//! The same bad case is proven for `agent_show`: its page (including a
//! `context_message` digest whose real pin names a message outside the selected
//! page) is built first, then the selected row's cloud grant is revoked through
//! `Store::cloud_agent_source_revoke` before the reproof. A still-authorized
//! control returns the page and the real digest; an unrelated hidden insertion
//! between selection and reproof must not turn the read into a refusal.
//!
//! The external platform contract is served by a loopback mock of the one
//! documented readback call. That is a stand-in for the remote platform, never
//! for the daemon's admission, projection or reproof, which all run for real.
//! This is NOT an OS isolation test: the test-seam peer UID exists only to make
//! the split-UID admission path reachable in-process.

use super::*;
use crate::cloud_agent::{registration_epoch, ENDPOINT_KIND, PROVIDER};
use crate::remote_cli::RemoteTarget;
use crate::store::cloud_agent::{
    CloudAgent, CloudSource, Owner, CONSENT_SCOPE, SOURCE_ACTIVE, SOURCE_PEER, STATE_CONNECTED,
};
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

const ORG: &str = "cad2215-cloud-org";
const ORG_ID: &str = "cad2215-cloud-org-id";
const ISSUER: &str = "https://cad2215-cloud.example.test";
const SUBJECT: &str = "cloud-subject-cad2215";
const OPEN_PROJECT: &str = "open";
const OPEN_ISSUE: &str = "VIS-1";
const OPEN_JOB: &str = "cad2215-cloud-job";
const OPEN_TASK: &str = "cad2215-cloud-task";
const READER: &str = "cad2215-cloud-reader";
const CLOUD_A: &str = "cad2215-cloud-a";
const CLOUD_B: &str = "cad2215-cloud-b";
const OP_A: &str = "cad2215-cloud-op-a";
const OP_B: &str = "cad2215-cloud-op-b";
const REPLY: &str = "cad2215 cloud reply";
const CLOUD_BODY: &str = "cloud reply a";
const FOREIGN_KICKOFF: &str = "cad2215-cloud-foreign-kickoff";

const REVOKE_NONE: u8 = 0;
const REVOKE_A: u8 = 1;
const REVOKE_B: u8 = 2;

const SHOW_NONE: u8 = 0;
const SHOW_REVOKE_A: u8 = 1;
const SHOW_INSERT_HIDDEN: u8 = 2;

const DETAIL_NONE: u8 = 0;
const DETAIL_REVOKE_A: u8 = 1;

const RESULT_MARKER: &str = "CAD2215_CLOUD_RESULT";
const ERROR_MARKER: &str = "CAD2215_CLOUD_ERROR";
const HIDDEN_NOISE: &str = "cad2215-cloud-hidden-noise";

#[derive(Clone, Debug)]
struct HookCall {
    alias: String,
    rows: usize,
    messages: Vec<String>,
}

#[derive(Clone)]
struct GrantTarget {
    cloud_agent_id: String,
    uid: String,
    epoch: i64,
    owner: Owner,
}

/// A scratch home for the peer-credential record, process-isolated under the
/// repo's own env lock exactly like the CAD-1922 acceptance does.
struct Home {
    _dir: tempfile::TempDir,
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl Home {
    fn new() -> Home {
        let lock = crate::home::CAD1922_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let names = ["HOME", "CADENCE_HOME", "XDG_CONFIG_HOME"];
        let saved: Vec<(&'static str, Option<OsString>)> = names
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();
        for name in names {
            std::env::remove_var(name);
        }
        // Same shape as the CAD-1922 fixture: a first-run home keeps its
        // credentials at `<home>/.cadence/config/remote-auth`.
        std::env::set_var("HOME", dir.path());
        Home {
            _dir: dir,
            _lock: lock,
            saved,
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        for (name, value) in &self.saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

/// The one published readback call, answered on a loopback origin. It echoes
/// the exact conversation/operation the daemon sent, which is all the contract
/// client validates.
struct MockPlatform {
    endpoint: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for MockPlatform {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn mock_platform() -> MockPlatform {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("cloud mock did not bind");
    let address = server.server_addr().to_ip().expect("cloud mock address");
    let endpoint = format!("http://{address}");
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        while !seen.load(Ordering::SeqCst) {
            let mut request = match server.recv_timeout(Duration::from_millis(50)) {
                Ok(Some(request)) => request,
                _ => continue,
            };
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            let sent: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let conversation = sent["arguments"]["conversation_id"]
                .as_str()
                .unwrap_or_default();
            let operation = sent["arguments"]["operation_id"]
                .as_str()
                .unwrap_or_default();
            let answer = json!({
                "ok": true,
                "operation": {
                    "operation_id": operation,
                    "conversation_id": conversation,
                    "status": "completed",
                    "text": REPLY,
                },
            });
            let header = tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap();
            let _ = request.respond(
                tiny_http::Response::from_string(answer.to_string())
                    .with_header(header)
                    .with_status_code(200),
            );
        }
    });
    MockPlatform {
        endpoint,
        stop,
        thread: Some(thread),
    }
}

struct World {
    shared: Arc<Shared>,
    _mock: MockPlatform,
    _home: Home,
    _state: tempfile::TempDir,
    _keys: tempfile::TempDir,
    agent_uid: u32,
    mode: Arc<AtomicU8>,
    show_mode: Arc<AtomicU8>,
    show_calls: Arc<Mutex<Vec<String>>>,
    detail_mode: Arc<AtomicU8>,
    hook_calls: Arc<Mutex<Vec<HookCall>>>,
}

impl World {
    fn last_hook(&self) -> HookCall {
        self.hook_calls
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("the selection hook must have run")
    }

    fn last_show(&self) -> String {
        self.show_calls
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("the agent_show hook must have run")
    }
}

fn write_project(pm: &std::path::Path) {
    let project = pm.join(OPEN_PROJECT);
    let issue = project.join(OPEN_ISSUE);
    std::fs::create_dir_all(&issue).unwrap();
    std::fs::write(
        project.join("project.yaml"),
        format!("key: {OPEN_PROJECT}\nprefix: VIS\n"),
    )
    .unwrap();
    std::fs::write(
        issue.join("issue.md"),
        format!(
            "---\nid: {OPEN_ISSUE}\ntitle: cloud fixture\nstatus: backlog\n\
             priority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n"
        ),
    )
    .unwrap();
}

fn register(shared: &Shared, state: &std::path::Path, alias: &str, provider: &str, kind: &str) {
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias,
            provider,
            endpoint_kind: kind,
            role: "worker",
            cwd: state.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
}

/// Insert one settled cloud operation for `cloud_agent_id`, owned by the
/// reader's peer source. A settled row is admitted directly and `finish_cloud`
/// short-circuits, so no canonical thread row is needed.
fn insert_operation(
    db: &rusqlite::Connection,
    cloud_agent_id: &str,
    operation: &str,
    conversation: &str,
    recipient_uid: &str,
    target: &GrantTarget,
) {
    db.execute(
        "INSERT INTO cloud_agent_operations(operation_id, cloud_agent_id, source_kind, \
         source_uid, registration_epoch, owner_issuer, owner_subject, recipient_uid, \
         sender_label, thread_id, conversation_id, remote_session_id, request_digest, \
         request_text, status, remote_operation_id, message_id, reply_text, created, updated, \
         grant_admitted_at, claimed_at) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'reader',?9,?10,'remote-session-1','digest', \
                'request','completed',?1,?1,'reply',1,1,0,0)",
        rusqlite::params![
            operation,
            cloud_agent_id,
            SOURCE_PEER,
            target.uid,
            target.epoch,
            target.owner.issuer,
            target.owner.subject,
            recipient_uid,
            format!("thread-{operation}"),
            conversation,
        ],
    )
    .unwrap();
}

/// The canonical cloud message backing one operation. `issue` is what lets a
/// scoped project filter admit the row at all.
fn insert_message(db: &rusqlite::Connection, message: &str, alias: &str, body: &str) {
    // The row must belong to this registration: terminal rows older than an
    // agent registration are intentionally excluded from its history.
    assert_eq!(
        db.execute(
            "INSERT INTO messages(id, alias, body, sender, source, state, created, completed, \
             issue, task_id) SELECT ?1, ?2, ?3, 'cloud-agent', 'cloud_agent', 'completed', \
             created, created, ?4, ?5 FROM agents WHERE alias=?2",
            rusqlite::params![message, alias, body, OPEN_ISSUE, OPEN_TASK],
        )
        .unwrap(),
        1
    );
}

fn insert_event(db: &rusqlite::Connection, alias: &str, message: &str, task: Option<&str>) {
    db.execute(
        "INSERT INTO events(alias, kind, payload, at, task_id) VALUES(?1, 'cloud_reply', ?2, 1, ?3)",
        rusqlite::params![
            alias,
            json!({"message": message, "result": {"status": "completed"}}).to_string(),
            task,
        ],
    )
    .unwrap();
}

impl World {
    fn add_event(&self, alias: &str, message: &str, task: Option<&str>) {
        let db = rusqlite::Connection::open(self._state.path().join("cadence.sqlite3")).unwrap();
        insert_event(&db, alias, message, task);
    }
}

/// The split-UID daemon UID, distinct from the test's own.
fn agent_uid() -> u32 {
    let euid = unsafe { libc::geteuid() };
    if euid == 1000 {
        1001
    } else {
        1000
    }
}

/// Provision test-owned keys through the real operator initialization path.
fn initialize_keyring(shared: &Arc<Shared>, state: &std::path::Path) -> tempfile::TempDir {
    let keys = tempfile::tempdir().unwrap();
    std::fs::set_permissions(keys.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let write_key = |name: &str, byte: u8| {
        let path = keys.path().join(name);
        std::fs::write(&path, [byte; 32]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        path
    };
    let identity = write_key("identity.key", 0x41);
    let cursor = write_key("cursor.key", 0x52);
    let id = "0c".repeat(16);
    let config = json!({
        "version": 1,
        "identity_key_file": identity,
        "active_key_id": id.clone(),
        "keys": [{ "id": id, "key_file": cursor }],
    });
    let path = state.join("scoped-cursors.json");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    // This fixture configures split UIDs, so the in-process call must also
    // provide the test-only owner peer context used by the real socket path.
    let _peer = crate::daemon::serve::test_peer_uid_scope(unsafe { libc::geteuid() });
    let initialized = scoped(Asserted::Operator, || {
        shared.dispatch("scoped_cursor_initialize", &json!({}), std::process::id())
    })
    .expect("the operator must initialize the test keyring");
    assert_eq!(initialized["initialized"], json!(true), "{initialized}");
    keys
}

fn build_world() -> World {
    build_world_with_thread_observers(None, None)
}

fn build_world_with_thread_observers(
    projection_observer: Option<crate::daemon::CloudProjectionObserver>,
    batch_observer: Option<crate::daemon::AgentThreadBatchObserver>,
) -> World {
    let home = Home::new();
    let state = tempfile::tempdir().unwrap();
    std::fs::set_permissions(state.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let pm = state.path().join("tracker");
    crate::issue::Pm::init(&pm).unwrap();
    std::fs::set_permissions(&pm, std::fs::Permissions::from_mode(0o700)).unwrap();
    write_project(&pm);
    let mock = mock_platform();
    let uid = agent_uid();

    let mode = Arc::new(AtomicU8::new(REVOKE_NONE));
    let show_mode = Arc::new(AtomicU8::new(SHOW_NONE));
    let show_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let hook_calls: Arc<Mutex<Vec<HookCall>>> = Arc::new(Mutex::new(Vec::new()));
    let grants: Arc<Mutex<Option<(GrantTarget, GrantTarget)>>> = Arc::new(Mutex::new(None));
    let hook_slot: Arc<Mutex<Option<Arc<Shared>>>> = Arc::new(Mutex::new(None));

    // `agent_show`'s witness: the page (agent row, messages, context digest,
    // task bindings) is already built; commit a real authority change or an
    // unrelated hidden insertion before the reproof runs.
    let show_hook: Option<crate::daemon::AgentShowSelectedHook> = {
        let calls = Arc::clone(&show_calls);
        let slot = Arc::clone(&hook_slot);
        let grants = Arc::clone(&grants);
        let mode = Arc::clone(&show_mode);
        let state = state.path().to_path_buf();
        Some(Arc::new(move |alias: &str| {
            calls.lock().unwrap().push(alias.to_string());
            match mode.load(Ordering::SeqCst) {
                SHOW_REVOKE_A => {
                    let Some(shared) = slot.lock().unwrap().clone() else {
                        return;
                    };
                    let Some((a, _)) = grants.lock().unwrap().clone() else {
                        return;
                    };
                    let _ = shared.store.cloud_agent_source_revoke(
                        &a.cloud_agent_id,
                        SOURCE_PEER,
                        &a.uid,
                        a.epoch,
                        &a.owner,
                    );
                }
                SHOW_INSERT_HIDDEN => {
                    // A message the reader cannot see (no issue, no task) on
                    // the page's own alias: selected after the page was built,
                    // so it must not turn the read into a refusal.
                    if let Ok(db) = rusqlite::Connection::open(state.join("cadence.sqlite3")) {
                        let _ = db.execute(
                            "INSERT OR IGNORE INTO messages(id, alias, body, sender, source, \
                             state, created, completed) VALUES(?1, ?2, 'hidden noise', \
                             'operator', 'send', 'completed', 1, 1)",
                            rusqlite::params![HIDDEN_NOISE, CLOUD_A],
                        );
                    }
                }
                _ => {}
            }
        }))
    };

    // `task_show`'s witness: the detail (task row plus every admitted
    // message body) is assembled; commit a real grant revocation before the
    // reproof runs.
    let detail_mode = Arc::new(AtomicU8::new(DETAIL_NONE));
    let detail_hook: Option<crate::daemon::JobDetailSnapshotHook> = {
        let slot = Arc::clone(&hook_slot);
        let grants = Arc::clone(&grants);
        let mode = Arc::clone(&detail_mode);
        Some(Arc::new(move |kind: &str, id: &str| {
            let target = match kind {
                "task_show" => OPEN_TASK,
                "job_show" => OPEN_JOB,
                _ => return,
            };
            if id != target || mode.load(Ordering::SeqCst) != DETAIL_REVOKE_A {
                return;
            }
            let Some(shared) = slot.lock().unwrap().clone() else {
                return;
            };
            let Some((a, _)) = grants.lock().unwrap().clone() else {
                return;
            };
            let _ = shared.store.cloud_agent_source_revoke(
                &a.cloud_agent_id,
                SOURCE_PEER,
                &a.uid,
                a.epoch,
                &a.owner,
            );
        }))
    };

    let hook: Option<crate::daemon::AgentEventsSelectedHook> = {
        let log = Arc::clone(&hook_calls);
        let slot = Arc::clone(&hook_slot);
        let mode = Arc::clone(&mode);
        let grants = Arc::clone(&grants);
        Some(Arc::new(
            move |alias: &str, rows: &[crate::store::Event]| {
                let messages = rows
                    .iter()
                    .filter_map(|event| {
                        event
                            .payload
                            .get("message")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect::<Vec<_>>();
                log.lock().unwrap().push(HookCall {
                    alias: alias.to_string(),
                    rows: rows.len(),
                    messages,
                });
                let selected = mode.load(Ordering::SeqCst);
                if selected == REVOKE_NONE {
                    return;
                }
                let Some(shared) = slot.lock().unwrap().clone() else {
                    return;
                };
                let Some((a, b)) = grants.lock().unwrap().clone() else {
                    return;
                };
                let target = match selected {
                    REVOKE_A => a,
                    REVOKE_B => b,
                    _ => return,
                };
                let _ = shared.store.cloud_agent_source_revoke(
                    &target.cloud_agent_id,
                    SOURCE_PEER,
                    &target.uid,
                    target.epoch,
                    &target.owner,
                );
            },
        ))
    };

    let opts = ServeOptions {
        test_seam: true,
        agent_uid: Some(uid),
        agent_events_selected_hook: hook,
        agent_show_selected_hook: show_hook,
        job_detail_snapshot_hook: detail_hook,
        cloud_projection_observer: projection_observer,
        agent_thread_batch_observer: batch_observer,
        ..ServeOptions::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(state.path(), &opts).unwrap();
    let keys = initialize_keyring(&shared, state.path());
    *hook_slot.lock().unwrap() = Some(Arc::clone(&shared));

    register(&shared, state.path(), READER, "fake", "fake");
    register(&shared, state.path(), CLOUD_A, PROVIDER, ENDPOINT_KIND);
    register(&shared, state.path(), CLOUD_B, PROVIDER, ENDPOINT_KIND);

    let reader = shared.store.agent(READER).unwrap();
    let cloud_a = shared.store.agent(CLOUD_A).unwrap();
    let cloud_b = shared.store.agent(CLOUD_B).unwrap();
    let epoch = registration_epoch(reader.created);
    let owner = Owner::new(ISSUER, SUBJECT);

    let target = RemoteTarget {
        org: ORG.to_string(),
        endpoint: mock.endpoint.clone(),
        org_id: ORG_ID.to_string(),
        identity: None,
    };
    crate::remote_cli::cloud_agent_save_peer(
        &target,
        &owner,
        &reader.uid,
        epoch,
        &json!({
            "token": "collab-peer-cad2215",
            "expires_at": crate::issue::time::now_epoch() + 3600,
        }),
    )
    .expect("the reader's peer credential must be recorded");

    let connect = |alias: &str, recipient_uid: &str, remote: &str| {
        shared
            .store
            .cloud_agent_connect(&CloudAgent {
                id: format!("{alias}-agent"),
                alias: alias.to_string(),
                organization_id: ORG_ID.to_string(),
                org_slug: ORG.to_string(),
                endpoint: mock.endpoint.clone(),
                remote_agent_id: remote.to_string(),
                runtime: "devin".to_string(),
                model: None,
                capabilities: json!({}),
                provider_session_id: None,
                provenance: json!({}),
                recipient_uid: recipient_uid.to_string(),
                consent_scope: CONSENT_SCOPE.to_string(),
                state: STATE_CONNECTED.to_string(),
                created: 1.0,
                updated: 1.0,
            })
            .expect("real cloud_agent_connect");
    };
    connect(CLOUD_A, cloud_a.uid.as_str(), "remote-a");
    connect(CLOUD_B, cloud_b.uid.as_str(), "remote-b");

    let grant_a = GrantTarget {
        cloud_agent_id: format!("{CLOUD_A}-agent"),
        uid: reader.uid.clone(),
        epoch,
        owner: owner.clone(),
    };
    let grant_b = GrantTarget {
        cloud_agent_id: format!("{CLOUD_B}-agent"),
        uid: reader.uid.clone(),
        epoch,
        owner: owner.clone(),
    };
    for grant in [&grant_a, &grant_b] {
        shared
            .store
            .cloud_agent_source_grant(&CloudSource {
                cloud_agent_id: grant.cloud_agent_id.clone(),
                kind: SOURCE_PEER.to_string(),
                uid: grant.uid.clone(),
                registration_epoch: grant.epoch,
                owner: grant.owner.clone(),
                label: READER.to_string(),
                scope: CONSENT_SCOPE.to_string(),
                granted_by: "operator".to_string(),
                granted_at: 1.0,
                invoker_id: None,
                state: SOURCE_ACTIVE.to_string(),
            })
            .expect("real cloud source grant");
    }

    {
        let db = rusqlite::Connection::open(state.path().join("cadence.sqlite3")).unwrap();
        db.execute_batch(&format!(
            "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,repo,state,created,updated) \
             VALUES('{OPEN_JOB}','Cloud job','spec.md','{READER}','{OPEN_ISSUE}','fixture', \
                    'open',1,1); \
             INSERT INTO tasks(id,job_id,title,assignee,state,created,updated) \
             VALUES('{OPEN_TASK}','{OPEN_JOB}','Cloud task','{READER}','running',1,1);"
        ))
        .unwrap();
        insert_operation(
            &db,
            &grant_a.cloud_agent_id,
            OP_A,
            "conv-a",
            &cloud_a.uid,
            &grant_a,
        );
        insert_operation(
            &db,
            &grant_b.cloud_agent_id,
            OP_B,
            "conv-b",
            &cloud_b.uid,
            &grant_b,
        );
        insert_message(&db, OP_A, CLOUD_A, CLOUD_BODY);
        insert_message(&db, OP_B, CLOUD_B, "cloud reply b");
        // The settled cloud content a stale page would release if the grant
        // were withdrawn mid-read.
        db.execute(
            "UPDATE messages SET result=?1, error=?2 WHERE id=?3",
            rusqlite::params![
                json!({"status": "completed", "marker": RESULT_MARKER}).to_string(),
                ERROR_MARKER,
                OP_A,
            ],
        )
        .unwrap();
    }
    *grants.lock().unwrap() = Some((grant_a, grant_b));

    World {
        shared,
        _mock: mock,
        _home: home,
        _state: state,
        _keys: keys,
        agent_uid: uid,
        mode,
        show_mode,
        show_calls,
        detail_mode,
        hook_calls,
    }
}

fn events(world: &World, params: &Value) -> Result<Value> {
    let shared = Arc::clone(&world.shared);
    let _uid = crate::daemon::serve::test_peer_uid_scope(world.agent_uid);
    scoped(Asserted::Agent(READER.to_string()), || {
        shared.dispatch("agent_events", params, std::process::id())
    })
}

fn tail(alias: &str) -> Value {
    json!({"alias": alias, "tail": true, "cursor_format": "opaque-v1"})
}

fn job_detail(world: &World, job: &str) -> Result<Value> {
    let shared = Arc::clone(&world.shared);
    let _uid = crate::daemon::serve::test_peer_uid_scope(world.agent_uid);
    scoped(Asserted::Agent(READER.to_string()), || {
        shared.dispatch("job_show", &json!({"job": job}), std::process::id())
    })
}

/// Point the fixture task's dispatch at one message, the way `job dispatch`
/// records a kickoff.
fn bind_dispatch(world: &World, task: &str, message: &str) {
    let db = rusqlite::Connection::open(world._state.path().join("cadence.sqlite3")).unwrap();
    assert_eq!(
        db.execute(
            "UPDATE tasks SET dispatch_message=?1 WHERE id=?2",
            rusqlite::params![message, task],
        )
        .unwrap(),
        1
    );
}

fn task_detail(world: &World, task: &str) -> Result<Value> {
    let shared = Arc::clone(&world.shared);
    let _uid = crate::daemon::serve::test_peer_uid_scope(world.agent_uid);
    scoped(Asserted::Agent(READER.to_string()), || {
        shared.dispatch("task_show", &json!({"task": task}), std::process::id())
    })
}

fn show(world: &World, params: &Value) -> Result<Value> {
    let shared = Arc::clone(&world.shared);
    let _uid = crate::daemon::serve::test_peer_uid_scope(world.agent_uid);
    scoped(Asserted::Agent(READER.to_string()), || {
        shared.dispatch("agent_show", params, std::process::id())
    })
}

/// A real `context_pin` record in the store's own event table, built with the
/// production `Manifest::pin_payload` shape and read back through
/// `Store::context_pin`. Returns the digest the page must compute from it.
fn insert_context_pin(db: &rusqlite::Connection, alias: &str, message: &str) -> String {
    let manifest = crate::context_pack::Manifest {
        schema: crate::context_pack::SCHEMA,
        task: crate::context_pack::TaskContract {
            task: OPEN_ISSUE.to_string(),
            issue: OPEN_ISSUE.to_string(),
            title: "cloud fixture".to_string(),
            revision: "rev-1".to_string(),
            request: "ship the fixture".to_string(),
            acceptance: vec!["fixture review".to_string()],
            execution: None,
        },
        project: OPEN_PROJECT.to_string(),
        project_revision: "proj-rev-1".to_string(),
        role: "worker".to_string(),
        assigned_role: None,
        documents: Vec::new(),
        withheld: Vec::new(),
        digest: "manifest-digest".to_string(),
    };
    db.execute(
        "INSERT INTO events(alias, kind, payload, at) VALUES(?1, 'context_pin', ?2, 1)",
        rusqlite::params![alias, manifest.pin_payload(message).to_string()],
    )
    .unwrap();
    manifest.task.semantic_digest()
}

fn assert_show_refusal(error: Error, what: &str, forbidden: &[&str]) {
    assert_eq!(error.kind(), "rejected", "{what}: {error:?}");
    assert!(
        error.code().is_none(),
        "{what} gained a structured code: {error:?}"
    );
    let rendered = format!("{error:?}");
    for secret in forbidden {
        assert!(
            !rendered.contains(secret),
            "{what} leaked {secret}: {rendered}"
        );
    }
}

#[test]
fn selected_page_is_refused_after_cloud_grant_revocation() {
    let world = build_world();
    world.add_event(CLOUD_A, OP_A, None);

    // Positive control first: the same request is served while the grant is
    // live, so the refusal below is a revocation, not a broken fixture.
    let page = events(&world, &tail(CLOUD_A)).expect("the admitted cloud page must be served");
    let rows = page["events"].as_array().expect("page events");
    assert_eq!(rows.len(), 1, "the admitted cloud event is missing: {page}");
    assert_eq!(rows[0]["payload"]["message"], json!(OP_A));
    let first = world.last_hook();
    assert_eq!(first.alias, CLOUD_A, "{first:?}");
    assert_eq!(
        first.rows, 1,
        "the selection hook did not observe the admitted row: {first:?}"
    );
    assert_eq!(first.messages, vec![OP_A.to_string()], "{first:?}");

    // Revoke inside the hook, after selection and before the reproof.
    world.mode.store(REVOKE_A, Ordering::SeqCst);
    let error = events(&world, &tail(CLOUD_A)).expect_err("the revoked page must not be served");
    assert_eq!(error.kind(), "rejected", "{error:?}");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("agent events refused"),
        "the refusal did not name the reproof: {rendered}"
    );
    for secret in [OP_A, OP_B, "cloud reply a", "cloud reply b"] {
        assert!(
            !rendered.contains(secret),
            "the refusal leaked {secret}: {rendered}"
        );
    }
    let last = world.last_hook();
    assert_eq!(
        last.rows, 1,
        "the hook must run on a page that was actually admitted: {last:?}"
    );
    assert_eq!(last.messages, vec![OP_A.to_string()], "{last:?}");
}

#[test]
fn waiting_page_is_refused_after_cloud_grant_revocation() {
    let world = build_world();
    world.add_event(CLOUD_A, OP_A, None);

    // The page already has rows, so the wait returns through the same
    // response-time reproof.
    let page = events(
        &world,
        &json!({"alias": CLOUD_A, "wait": 1, "cursor_format": "opaque-v1"}),
    )
    .expect("the admitted wait page must be served");
    assert_eq!(page["events"].as_array().map(Vec::len), Some(1), "{page}");

    world.mode.store(REVOKE_A, Ordering::SeqCst);
    let error = events(
        &world,
        &json!({"alias": CLOUD_A, "wait": 1, "cursor_format": "opaque-v1"}),
    )
    .expect_err("the waiting page must not be served after revocation");
    assert_eq!(error.kind(), "rejected", "{error:?}");
    assert!(
        format!("{error:?}").contains("agent events refused"),
        "{error:?}"
    );
    assert_eq!(world.last_hook().rows, 1);
}

#[test]
fn foreign_alias_message_is_refused_for_its_own_revoked_authority() {
    let world = build_world();
    // The reader's own stream names a message delivered under the cloud alias.
    world.add_event(READER, OP_A, Some(OPEN_TASK));

    // Control: the foreign message is admitted and served.
    let page = events(&world, &tail(READER)).expect("the foreign admitted page must be served");
    assert_eq!(page["events"].as_array().map(Vec::len), Some(1), "{page}");
    let first = world.last_hook();
    assert_eq!(first.alias, READER, "{first:?}");
    assert_eq!(first.messages, vec![OP_A.to_string()], "{first:?}");

    // Revoking an alias the page does not name is not an activity oracle: the
    // row's own backing connection is still authorized.
    world.mode.store(REVOKE_B, Ordering::SeqCst);
    events(&world, &tail(READER)).expect("revoking an unselected alias must not refuse the page");

    // Revoking the foreign message's own connection refuses the page.
    world.mode.store(REVOKE_A, Ordering::SeqCst);
    let error = events(&world, &tail(READER)).expect_err("the foreign page must be refused");
    assert_eq!(error.kind(), "rejected", "{error:?}");
    assert!(
        format!("{error:?}").contains("agent events refused"),
        "{error:?}"
    );
}

#[test]
fn tail_has_older_witness_is_reproved_for_its_own_authority() {
    let world = build_world();
    // Older witness first (lowest seq), then 50 newer returned rows on the
    // same stream. The witness is a foreign message under the other cloud
    // alias, so revoking only that alias proves the witness itself is re-proved.
    world.add_event(CLOUD_A, OP_B, Some(OPEN_TASK));
    for _ in 0..50 {
        world.add_event(CLOUD_A, OP_A, None);
    }

    let page = events(&world, &tail(CLOUD_A)).expect("the admitted tail page must be served");
    assert_eq!(
        page["has_older"],
        json!(true),
        "fixture must produce an older witness: {page}"
    );
    assert_eq!(page["events"].as_array().map(Vec::len), Some(50), "{page}");
    let first = world.last_hook();
    assert_eq!(
        first.rows, 51,
        "the hook must see the returned page plus its witness: {first:?}"
    );
    assert_eq!(
        first.messages.first().map(String::as_str),
        Some(OP_B),
        "witness first: {first:?}"
    );
    assert_eq!(
        first.messages[1..]
            .iter()
            .filter(|id| id.as_str() == OP_A)
            .count(),
        50,
        "{first:?}"
    );

    // Revoke ONLY the witness's backing connection.
    world.mode.store(REVOKE_B, Ordering::SeqCst);
    let error = events(&world, &tail(CLOUD_A)).expect_err("the witnessed tail must be refused");
    assert_eq!(error.kind(), "rejected", "{error:?}");
    let rendered = format!("{error:?}");
    assert!(rendered.contains("agent events refused"), "{rendered}");
    for secret in [OP_A, OP_B, "cloud reply a", "cloud reply b"] {
        assert!(
            !rendered.contains(secret),
            "the refusal leaked {secret}: {rendered}"
        );
    }
    let last = world.last_hook();
    assert_eq!(
        last.rows, 51,
        "the witness page must have been admitted before the reproof: {last:?}"
    );
    assert_eq!(
        last.messages.first().map(String::as_str),
        Some(OP_B),
        "{last:?}"
    );
}

#[test]
fn authorized_show_returns_cloud_content_and_the_context_digest() {
    let world = build_world();
    let db = rusqlite::Connection::open(world._state.path().join("cadence.sqlite3")).unwrap();
    let digest = insert_context_pin(&db, CLOUD_A, OP_A);

    let page =
        show(&world, &json!({"alias": CLOUD_A})).expect("the admitted agent page must be served");
    let messages = page["messages"].as_array().expect("page messages");
    assert_eq!(messages.len(), 1, "admitted cloud message missing: {page}");
    assert_eq!(messages[0]["id"], json!(OP_A));
    // Agent detail returns admitted message content, unlike the body-less
    // message index. The revocation cases must protect these exact fields.
    assert_eq!(messages[0]["body"], json!(CLOUD_BODY), "{page}");
    assert_eq!(
        messages[0]["result"]["marker"],
        json!(RESULT_MARKER),
        "{page}"
    );
    assert_eq!(messages[0]["error"], json!(ERROR_MARKER), "{page}");
    assert!(
        messages[0].get("seq").is_none(),
        "global position leaked: {page}"
    );
    assert_eq!(
        world.last_show(),
        CLOUD_A,
        "agent_show hook did not observe the page"
    );

    let digest_page = show(
        &world,
        &json!({"alias": CLOUD_A, "context_message": OP_A, "limit": 0}),
    )
    .expect("the admitted context digest must be served");
    assert_eq!(
        digest_page["context_contract_digest"],
        json!(digest),
        "the page did not compute the real pin's digest: {digest_page}"
    );
    assert!(
        digest_page["messages"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "the context read must be outside the selected message page: {digest_page}"
    );
}

#[test]
fn revoked_cloud_grant_refuses_the_show_message_page() {
    let world = build_world();
    world.show_mode.store(SHOW_REVOKE_A, Ordering::SeqCst);

    let error = show(&world, &json!({"alias": CLOUD_A}))
        .expect_err("a stale cloud message page must be refused");
    assert_show_refusal(
        error,
        "agent_show message page",
        &[OP_A, CLOUD_BODY, REPLY, RESULT_MARKER, ERROR_MARKER],
    );
    assert_eq!(
        world.last_show(),
        CLOUD_A,
        "the hook must run after the page (and its messages) were built"
    );
}

#[test]
fn revoked_cloud_grant_refuses_the_context_digest_outside_the_page() {
    let world = build_world();
    let db = rusqlite::Connection::open(world._state.path().join("cadence.sqlite3")).unwrap();
    let digest = insert_context_pin(&db, CLOUD_A, OP_A);
    world.show_mode.store(SHOW_REVOKE_A, Ordering::SeqCst);

    let error = show(
        &world,
        &json!({"alias": CLOUD_A, "context_message": OP_A, "limit": 0}),
    )
    .expect_err("a stale cloud context digest must be refused");
    assert_show_refusal(
        error,
        "agent_show context digest",
        &[
            OP_A,
            CLOUD_BODY,
            REPLY,
            RESULT_MARKER,
            ERROR_MARKER,
            digest.as_str(),
        ],
    );
}

#[test]
fn unrelated_hidden_insertion_does_not_refuse_the_show_page() {
    let world = build_world();
    world.show_mode.store(SHOW_INSERT_HIDDEN, Ordering::SeqCst);

    let page = show(&world, &json!({"alias": CLOUD_A}))
        .expect("an unrelated hidden insertion must not refuse the page");
    let messages = page["messages"].as_array().expect("page messages");
    assert_eq!(
        messages.len(),
        1,
        "hidden insertion changed the page: {page}"
    );
    assert_eq!(messages[0]["id"], json!(OP_A));
    assert!(
        !page.to_string().contains(HIDDEN_NOISE),
        "hidden insertion leaked: {page}"
    );
    assert_eq!(world.last_show(), CLOUD_A);
}

#[test]
fn scoped_thread_ignores_foreign_arrivals_between_cloud_snapshots() {
    use std::sync::atomic::AtomicUsize;
    const FOREIGN: &str = "cad2215-snapshot-foreign-reader";
    const VISIBLE: &str = "CAD2215_SNAPSHOT_VISIBLE";
    const HIDDEN: &str = "CAD2215_SNAPSHOT_FOREIGN";
    type HookState = (
        std::sync::Weak<Shared>,
        std::path::PathBuf,
        GrantTarget,
        String,
    );
    let state: Arc<Mutex<Option<HookState>>> = Arc::new(Mutex::new(None));
    let fired = Arc::new(AtomicUsize::new(0));
    let initial_refreshes = Arc::new(AtomicUsize::new(0));
    let armed = Arc::new(AtomicBool::new(false));
    let observer: crate::daemon::CloudProjectionObserver = {
        let state = Arc::clone(&state);
        let fired = Arc::clone(&fired);
        let armed = Arc::clone(&armed);
        Arc::new(move |alias: &str, _withheld: Option<&str>| {
            if alias != CLOUD_A || !armed.load(Ordering::SeqCst) {
                return;
            }
            let call = fired.fetch_add(1, Ordering::SeqCst);
            // Initial admission: 32 foreign rows fill the first raw batch.
            // First in-scan refresh: add row 33 after the new snapshot.
            let range = match call {
                0 => 0..32,
                1 => 32..33,
                _ => return,
            };
            let (shared, path, grant, cloud_uid) = state.lock().unwrap().clone().unwrap();
            let shared = shared.upgrade().expect("test world still alive");
            let db = rusqlite::Connection::open(path.join("cadence.sqlite3")).unwrap();
            for n in range {
                let operation = format!("cad2215-snapshot-foreign-op-{n}");
                let text = format!("{HIDDEN}-{n}");
                insert_operation(
                    &db,
                    &grant.cloud_agent_id,
                    &operation,
                    &format!("cad2215-snapshot-foreign-conversation-{n}"),
                    &cloud_uid,
                    &grant,
                );
                insert_message(&db, &operation, CLOUD_A, &text);
                shared
                    .store
                    .thread_append(
                        CLOUD_A,
                        crate::store::NewEntry {
                            role: "agent",
                            kind: "assistant_text",
                            text: &text,
                            payload: None,
                            message_id: Some(&operation),
                        },
                    )
                    .unwrap()
                    .expect("append foreign row after projection snapshot");
            }
        })
    };
    let batches: crate::daemon::AgentThreadBatchObserver = {
        let armed = Arc::clone(&armed);
        let initial_refreshes = Arc::clone(&initial_refreshes);
        Arc::new(move |alias: &str, phase: &str, _selected: usize| {
            if alias == CLOUD_A && phase == "initial-refresh" && armed.load(Ordering::SeqCst) {
                initial_refreshes.fetch_add(1, Ordering::SeqCst);
            }
        })
    };
    let world = build_world_with_thread_observers(Some(observer), Some(batches));
    register(&world.shared, world._state.path(), FOREIGN, "fake", "fake");
    let foreign = world.shared.store.agent(FOREIGN).unwrap();
    let cloud = world.shared.store.agent(CLOUD_A).unwrap();
    let owner = Owner::new(ISSUER, SUBJECT);
    let grant = GrantTarget {
        cloud_agent_id: format!("{CLOUD_A}-agent"),
        uid: foreign.uid.clone(),
        epoch: registration_epoch(foreign.created),
        owner: owner.clone(),
    };
    let target = RemoteTarget {
        org: ORG.to_string(),
        endpoint: world._mock.endpoint.clone(),
        org_id: ORG_ID.to_string(),
        identity: None,
    };
    crate::remote_cli::cloud_agent_save_peer(
        &target,
        &owner,
        &grant.uid,
        grant.epoch,
        &json!({"token":"collab-cad2215-snapshot-foreign",
                "expires_at":crate::issue::time::now_epoch()+3600}),
    )
    .expect("enroll foreign source");
    world
        .shared
        .store
        .cloud_agent_source_grant(&CloudSource {
            cloud_agent_id: grant.cloud_agent_id.clone(),
            kind: SOURCE_PEER.to_string(),
            uid: grant.uid.clone(),
            registration_epoch: grant.epoch,
            owner: owner.clone(),
            label: FOREIGN.to_string(),
            scope: CONSENT_SCOPE.to_string(),
            granted_by: "operator".to_string(),
            granted_at: 1.0,
            invoker_id: None,
            state: SOURCE_ACTIVE.to_string(),
        })
        .expect("grant foreign source");
    world.shared.store.ensure_thread(CLOUD_A).unwrap();
    world
        .shared
        .store
        .thread_append(
            CLOUD_A,
            crate::store::NewEntry {
                role: "agent",
                kind: "assistant_text",
                text: VISIBLE,
                payload: None,
                message_id: Some(OP_A),
            },
        )
        .unwrap()
        .expect("append reader-visible control");
    *state.lock().unwrap() = Some((
        Arc::downgrade(&world.shared),
        world._state.path().to_path_buf(),
        grant,
        cloud.uid,
    ));
    let call = |reader: &str, limit: i64| {
        let _uid = crate::daemon::serve::test_peer_uid_scope(world.agent_uid);
        scoped(Asserted::Agent(reader.to_string()), || {
            world.shared.dispatch(
                "thread_read",
                &json!({"alias":CLOUD_A,"cursor_format":"opaque-v1","limit":limit}),
                std::process::id(),
            )
        })
    };
    let baseline = call(READER, 10).expect("baseline visible thread page");
    assert_eq!(baseline["entries"].as_array().unwrap().len(), 1);
    assert_eq!(baseline["entries"][0]["text"], VISIBLE);
    armed.store(true, Ordering::SeqCst);
    let during = call(READER, 10).expect("foreign-only arrivals must not refuse the page");
    armed.store(false, Ordering::SeqCst);
    assert!(
        fired.load(Ordering::SeqCst) >= 3,
        "initial admission and both readmissions must run"
    );
    assert!(
        initial_refreshes.load(Ordering::SeqCst) >= 2,
        "the initial scan must refresh twice, not merely reach confirmation"
    );
    assert_eq!(
        during["entries"], baseline["entries"],
        "foreign rows changed visible entries"
    );
    for field in [
        "alias",
        "thread",
        "cursor_format",
        "more_before",
        "more_after",
        "cloud_withheld",
        "_reader_scope",
    ] {
        assert_eq!(
            during.get(field),
            baseline.get(field),
            "foreign rows changed {field}"
        );
    }
    for field in ["cursor", "before_cursor"] {
        assert!(baseline[field]
            .as_str()
            .is_some_and(|s| s.starts_with("csc1_")));
        assert!(during[field]
            .as_str()
            .is_some_and(|s| s.starts_with("csc1_")));
    }
    let foreign_page = call(FOREIGN, 100).expect("foreign owner positive control");
    let text = foreign_page.to_string();
    for n in 0..33 {
        assert!(
            text.contains(&format!("{HIDDEN}-{n}\"")),
            "owner cannot see foreign row {n}: {text}"
        );
    }
}

#[test]
fn scoped_agent_thread_hides_foreign_cloud_before_pagination_and_seals_positions() {
    struct Observation {
        visible_forward: Vec<String>,
        visible_backward: Vec<String>,
        more: (bool, bool, bool),
        foreign_workspace_cursor: String,
    }

    fn observe(with_foreign: bool, foreign_workspace: Option<&str>) -> Observation {
        const FOREIGN_READER: &str = "cad2215-thread-foreign-reader";
        const FOREIGN_OPERATION: &str = "cad2215-thread-foreign-operation";
        const OPERATOR_CONTROL: &str = "cad2215-thread-operator-control";
        const FIRST: &str = "CAD2215_THREAD_SOURCE_FIRST";
        const LAST: &str = "CAD2215_THREAD_SOURCE_LAST";
        const HIDDEN: &str = "CAD2215_THREAD_FOREIGN_HIDDEN";

        let world = build_world();
        register(
            &world.shared,
            world._state.path(),
            FOREIGN_READER,
            "fake",
            "fake",
        );
        register(
            &world.shared,
            world._state.path(),
            OPERATOR_CONTROL,
            "fake",
            "fake",
        );
        let foreign = world.shared.store.agent(FOREIGN_READER).unwrap();
        let cloud_a = world.shared.store.agent(CLOUD_A).unwrap();
        let owner = Owner::new(ISSUER, SUBJECT);
        let target = RemoteTarget {
            org: ORG.to_string(),
            endpoint: world._mock.endpoint.clone(),
            org_id: ORG_ID.to_string(),
            identity: None,
        };
        let grant = GrantTarget {
            cloud_agent_id: format!("{CLOUD_A}-agent"),
            uid: foreign.uid.clone(),
            epoch: registration_epoch(foreign.created),
            owner: owner.clone(),
        };
        crate::remote_cli::cloud_agent_save_peer(
            &target,
            &owner,
            &grant.uid,
            grant.epoch,
            &json!({
                "token": "collab-cad2215-foreign-thread-peer",
                "expires_at": crate::issue::time::now_epoch() + 3600,
            }),
        )
        .expect("save independently enrolled foreign source credential");
        world
            .shared
            .store
            .cloud_agent_source_grant(&CloudSource {
                cloud_agent_id: grant.cloud_agent_id.clone(),
                kind: SOURCE_PEER.to_string(),
                uid: grant.uid.clone(),
                registration_epoch: grant.epoch,
                owner: grant.owner.clone(),
                label: FOREIGN_READER.to_string(),
                scope: CONSENT_SCOPE.to_string(),
                granted_by: "operator".to_string(),
                granted_at: 1.0,
                invoker_id: None,
                state: SOURCE_ACTIVE.to_string(),
            })
            .expect("grant independent foreign source");
        let db = rusqlite::Connection::open(world._state.path().join("cadence.sqlite3")).unwrap();
        insert_operation(
            &db,
            &grant.cloud_agent_id,
            FOREIGN_OPERATION,
            "cad2215-foreign-thread-conversation",
            &cloud_a.uid,
            &grant,
        );
        insert_message(&db, FOREIGN_OPERATION, CLOUD_A, HIDDEN);

        world.shared.store.ensure_thread(CLOUD_A).unwrap();
        world.shared.store.ensure_thread(CLOUD_B).unwrap();
        world.shared.store.ensure_thread(OPERATOR_CONTROL).unwrap();
        let append = |alias: &str, kind: &str, text: &str, message: Option<&str>| {
            world
                .shared
                .store
                .thread_append(
                    alias,
                    crate::store::NewEntry {
                        role: "agent",
                        kind,
                        text,
                        payload: None,
                        message_id: message,
                    },
                )
                .unwrap()
                .expect("append real thread entry")
        };
        append(CLOUD_A, "assistant_text", FIRST, Some(OP_A));
        if with_foreign {
            append(CLOUD_A, "assistant_text", HIDDEN, Some(FOREIGN_OPERATION));
        }
        append(CLOUD_A, "message", LAST, Some(OP_A));
        append(
            CLOUD_B,
            "message",
            "CAD2215_OTHER_THREAD_CONTROL",
            Some(OP_B),
        );
        append(
            OPERATOR_CONTROL,
            "message",
            "CAD2215_OPERATOR_CONTROL",
            None,
        );

        let reader_call = |alias: &str, params: Value| -> Result<Value> {
            let _uid = crate::daemon::serve::test_peer_uid_scope(world.agent_uid);
            scoped(Asserted::Agent(alias.to_string()), || {
                world
                    .shared
                    .dispatch("thread_read", &params, std::process::id())
            })
        };
        let read =
            |params: Value| reader_call(READER, params).expect("registered agent thread read");
        let check_page = |page: &Value| -> Vec<String> {
            let object = page.as_object().expect("thread page object");
            for field in ["seq", "scanned_before", "reply_seq"] {
                assert!(
                    !object.contains_key(field),
                    "global {field} on page: {page}"
                );
            }
            let rows = page["entries"].as_array().expect("thread entries");
            rows.iter()
                .map(|row| {
                    let fields = row.as_object().expect("thread entry object");
                    assert!(!fields.contains_key("seq"), "global row seq: {row}");
                    assert!(
                        row["id"]
                            .as_str()
                            .is_some_and(|s| !s.is_empty() && s.parse::<i64>().is_err()),
                        "missing keyed opaque row id: {row}"
                    );
                    row["text"].as_str().expect("entry text").to_string()
                })
                .collect()
        };
        let token = |page: &Value, field: &str| -> String {
            page[field]
                .as_str()
                .filter(|s| !s.is_empty() && s.parse::<i64>().is_err())
                .unwrap_or_else(|| panic!("missing opaque {field}: {page}"))
                .to_string()
        };

        // The second enrolled peer is the foreign source's actual owner, not
        // an operator that cloud-source isolation intentionally excludes.
        if with_foreign {
            let owner_page = reader_call(
                FOREIGN_READER,
                json!({
                    "alias": CLOUD_A, "cursor_format": "opaque-v1", "tail": true, "limit": 10
                }),
            )
            .expect("foreign-source owner read");
            assert!(
                check_page(&owner_page).contains(&HIDDEN.to_string()),
                "the genuine foreign cloud row was not readable by its owner: {owner_page}"
            );
        }

        let first = read(json!({"alias": CLOUD_A, "cursor_format": "opaque-v1", "limit": 1}));
        let first_rows = check_page(&first);
        assert_eq!(first_rows, vec![FIRST.to_string()], "first page: {first}");
        let first_id = first["entries"][0]["id"].as_str().unwrap().to_string();
        let after_first = token(&first, "cursor");
        let before_first = token(&first, "before_cursor");
        let repeat = read(json!({"alias": CLOUD_A, "cursor_format": "opaque-v1", "limit": 1}));
        assert_eq!(check_page(&repeat), first_rows);
        assert_eq!(repeat["entries"][0]["id"], first_id, "row id is not stable");
        assert_ne!(
            token(&repeat, "cursor"),
            after_first,
            "encrypted continuation must use a fresh random nonce"
        );

        let second = read(json!({
            "alias": CLOUD_A, "cursor_format": "opaque-v1", "after": after_first, "limit": 1
        }));
        let second_rows = check_page(&second);
        assert_eq!(
            second_rows,
            vec![LAST.to_string()],
            "hidden row consumed the forward limit: {second}"
        );
        let after_second = token(&second, "cursor");
        let empty = read(json!({
            "alias": CLOUD_A, "cursor_format": "opaque-v1", "after": after_second, "limit": 1
        }));
        assert!(
            check_page(&empty).is_empty(),
            "end must have no visible rows: {empty}"
        );
        assert_eq!(
            empty["more_after"].as_bool(),
            Some(false),
            "end did not exhaust: {empty}"
        );

        let tail =
            read(json!({"alias": CLOUD_A, "cursor_format": "opaque-v1", "tail": true, "limit": 1}));
        let tail_rows = check_page(&tail);
        assert_eq!(tail_rows, vec![LAST.to_string()], "tail: {tail}");
        let older_token = token(&tail, "before_cursor");
        let older = read(json!({
            "alias": CLOUD_A, "cursor_format": "opaque-v1", "before": older_token, "limit": 1
        }));
        let older_rows = check_page(&older);
        assert_eq!(
            older_rows,
            vec![FIRST.to_string()],
            "hidden row consumed backward limit: {older}"
        );
        assert_eq!(
            older["entries"][0]["id"], first_id,
            "backward row id differs"
        );
        assert_eq!(
            older["more_before"].as_bool(),
            Some(false),
            "oldest page is not exhausted: {older}"
        );

        // Numeric positions and tokens from the wrong principal, thread,
        // direction, or workspace must fail rather than reset to the start.
        for params in [
            json!({"alias": CLOUD_A, "cursor_format": "opaque-v1", "after": 0}),
            json!({"alias": CLOUD_A, "cursor_format": "opaque-v1", "before": 1}),
            json!({"alias": CLOUD_A, "cursor_format": "opaque-v1", "before": after_first}),
            json!({"alias": CLOUD_A, "cursor_format": "opaque-v1", "after": before_first}),
            json!({"alias": CLOUD_B, "cursor_format": "opaque-v1", "after": after_first}),
        ] {
            let result = reader_call(READER, params.clone());
            assert!(
                result.is_err(),
                "invalid scoped continuation reset or succeeded: {params} => {result:?}"
            );
        }
        let foreign = reader_call(
            FOREIGN_READER,
            json!({
                "alias": CLOUD_A, "cursor_format": "opaque-v1", "after": after_first
            }),
        );
        assert!(
            foreign.is_err(),
            "another principal reused a continuation: {foreign:?}"
        );
        if let Some(cursor) = foreign_workspace {
            let result = reader_call(
                READER,
                json!({
                    "alias": CLOUD_A, "cursor_format": "opaque-v1", "after": cursor
                }),
            );
            assert!(
                result.is_err(),
                "another workspace reused a continuation: {result:?}"
            );
        }

        // A genuinely proven operator retains the pre-cutover numeric API.
        // Use an ordinary alias: an operator does not own another peer's cloud source.
        let operator = {
            let _uid = crate::daemon::serve::test_peer_uid_scope(unsafe { libc::geteuid() });
            scoped(Asserted::Operator, || {
                world.shared.dispatch(
                    "thread_read",
                    &json!({"alias": OPERATOR_CONTROL, "after": 0, "limit": 1}),
                    std::process::id(),
                )
            })
            .expect("operator numeric thread recovery")
        };
        assert!(
            operator["entries"][0]["seq"].as_i64().is_some(),
            "operator lost numeric row: {operator}"
        );
        assert!(
            operator["cursor"].as_i64().is_some(),
            "operator lost numeric cursor: {operator}"
        );
        assert_eq!(operator["entries"][0]["text"], "CAD2215_OPERATOR_CONTROL");

        Observation {
            visible_forward: [first_rows, second_rows].concat(),
            visible_backward: [tail_rows, older_rows].concat(),
            more: (
                first["more_after"]
                    .as_bool()
                    .expect("forward exhaustion metadata"),
                tail["more_before"]
                    .as_bool()
                    .expect("backward exhaustion metadata"),
                older["more_before"]
                    .as_bool()
                    .expect("oldest exhaustion metadata"),
            ),
            foreign_workspace_cursor: after_first,
        }
    }

    // World construction uses a process-wide HOME lock, so construct each
    // sequentially and carry only the already-issued opaque token across.
    let without = observe(false, None);
    let with = observe(true, Some(&without.foreign_workspace_cursor));
    assert_eq!(
        with.visible_forward, without.visible_forward,
        "foreign-source traffic changed visible forward pagination"
    );
    assert_eq!(
        with.visible_backward, without.visible_backward,
        "foreign-source traffic changed visible backward pagination"
    );
    assert_eq!(
        with.more, without.more,
        "foreign-source traffic changed visible exhaustion metadata"
    );
}

#[test]
fn scoped_agent_thread_cursor_rejects_other_workspace_with_every_other_binding_equal() {
    const MARKER_ONE: &str = "CAD2215_NAMESPACE_CONTROL_FIRST";
    const MARKER_TWO: &str = "CAD2215_NAMESPACE_CONTROL_SECOND";
    let world = build_world();
    world.shared.store.ensure_thread(CLOUD_A).unwrap();
    for text in [MARKER_ONE, MARKER_TWO] {
        world
            .shared
            .store
            .thread_append(
                CLOUD_A,
                crate::store::NewEntry {
                    role: "agent",
                    kind: "assistant_text",
                    text,
                    payload: None,
                    message_id: Some(OP_A),
                },
            )
            .unwrap()
            .expect("real cloud-source thread fixture entry");
    }

    // Back up the real, initialized fixture database with SQLite's online
    // backup API; never copy just the main file while WAL may be live.
    let cloned = tempfile::tempdir().unwrap();
    std::fs::set_permissions(cloned.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = rusqlite::Connection::open(world._state.path().join("cadence.sqlite3")).unwrap();
    let mut destination =
        rusqlite::Connection::open(cloned.path().join("cadence.sqlite3")).unwrap();
    {
        let backup = rusqlite::backup::Backup::new(&source, &mut destination).unwrap();
        backup
            .run_to_completion(64, Duration::from_millis(5), None)
            .unwrap();
    }
    let original_workspace: String = source
        .query_row(
            "SELECT workspace_id FROM connection_metadata WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    // A fresh synthetic workspace UUID. Clear only the copied fingerprint so
    // the production initialize RPC will bind it to the new namespace.
    let clone_workspace = uuid::Uuid::new_v4().simple().to_string();
    assert_ne!(original_workspace, clone_workspace);
    assert_eq!(destination.execute(
        "UPDATE connection_metadata SET workspace_id=?1, cursor_keyring_fingerprint=NULL WHERE singleton=1",
        [&clone_workspace],
    ).unwrap(), 1);
    drop(destination);
    drop(source);

    // The record contains absolute paths to World-owned keys: copying this
    // file intentionally references identical identity and encryption keys.
    std::fs::copy(
        world._state.path().join("scoped-cursors.json"),
        cloned.path().join("scoped-cursors.json"),
    )
    .unwrap();
    std::fs::set_permissions(
        cloned.path().join("scoped-cursors.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let opts = ServeOptions {
        test_seam: true,
        agent_uid: Some(world.agent_uid),
        ..ServeOptions::default()
    };
    opts.provider_env.set(
        "CADENCE_PM_DIR",
        world._state.path().join("tracker").to_str().unwrap(),
    );
    let cloned_shared = Shared::new(cloned.path(), &opts).unwrap();
    {
        let _uid = crate::daemon::serve::test_peer_uid_scope(unsafe { libc::geteuid() });
        let initialized = scoped(Asserted::Operator, || {
            cloned_shared.dispatch("scoped_cursor_initialize", &json!({}), std::process::id())
        })
        .expect("operator must bind clone's namespace through the real initializer");
        assert_eq!(
            initialized["initialized"],
            json!(true),
            "clone was not initialized: {initialized}"
        );
    }

    let original_agent = world.shared.store.agent(READER).unwrap();
    let clone_agent = cloned_shared.store.agent(READER).unwrap();
    assert_eq!(
        original_agent.uid, clone_agent.uid,
        "agent principal differs"
    );
    assert_eq!(
        registration_epoch(original_agent.created),
        registration_epoch(clone_agent.created),
        "agent registration incarnation differs"
    );
    let original_thread = world.shared.store.thread(CLOUD_A).unwrap().unwrap();
    let clone_thread = cloned_shared.store.thread(CLOUD_A).unwrap().unwrap();
    assert_eq!(
        original_thread.id, clone_thread.id,
        "thread resource differs"
    );
    assert_eq!(
        original_thread.created.to_bits(),
        clone_thread.created.to_bits(),
        "thread resource incarnation differs"
    );
    assert_ne!(
        world.shared.store.connection_workspace_id().unwrap(),
        cloned_shared.store.connection_workspace_id().unwrap()
    );
    {
        let _uid = crate::daemon::serve::test_peer_uid_scope(world.agent_uid);
        let (scope_a, scope_b) = scoped(Asserted::Agent(READER.to_string()), || {
            (
                world
                    .shared
                    .project_scope(std::process::id(), "fixture scope")
                    .unwrap(),
                cloned_shared
                    .project_scope(std::process::id(), "fixture scope")
                    .unwrap(),
            )
        });
        assert_eq!(scope_a, scope_b, "readable scope differs");
    }
    let call = |shared: &Arc<Shared>, params: Value| -> Result<Value> {
        let _uid = crate::daemon::serve::test_peer_uid_scope(world.agent_uid);
        scoped(Asserted::Agent(READER.to_string()), || {
            shared.dispatch("thread_read", &params, std::process::id())
        })
    };
    let initial = || json!({"alias": CLOUD_A, "cursor_format": "opaque-v1", "limit": 1});
    let next = |cursor: &str| {
        json!({
            "alias": CLOUD_A, "cursor_format": "opaque-v1", "after": cursor, "limit": 1
        })
    };
    let a = call(&world.shared, initial()).expect("original workspace control page");
    let b = call(&cloned_shared, initial()).expect("cloned workspace control page");
    for page in [&a, &b] {
        assert_eq!(
            page["entries"][0]["text"], MARKER_ONE,
            "initial page: {page}"
        );
        assert!(
            page["entries"][0].get("seq").is_none(),
            "scoped row leaked seq: {page}"
        );
    }
    let a_cursor = a["cursor"]
        .as_str()
        .filter(|s| !s.is_empty() && s.parse::<i64>().is_err())
        .expect("original opaque continuation");
    let b_cursor = b["cursor"]
        .as_str()
        .filter(|s| !s.is_empty() && s.parse::<i64>().is_err())
        .expect("clone opaque continuation");
    let a_local = call(&world.shared, next(a_cursor)).expect("original continuation control");
    let b_local = call(&cloned_shared, next(b_cursor)).expect("clone continuation control");
    assert_eq!(
        a_local["entries"][0]["text"], MARKER_TWO,
        "original continuation: {a_local}"
    );
    assert_eq!(
        b_local["entries"][0]["text"], MARKER_TWO,
        "clone continuation: {b_local}"
    );
    let cross_ab = call(&cloned_shared, next(a_cursor));
    let cross_ba = call(&world.shared, next(b_cursor));
    assert!(
        cross_ab.is_err(),
        "original workspace token accepted by clone: {cross_ab:?}"
    );
    assert!(
        cross_ba.is_err(),
        "clone workspace token accepted by original: {cross_ba:?}"
    );
}

#[test]
fn scoped_ordinary_thread_warns_only_for_eligible_cloud_history() {
    const ORDINARY: &str = "cad2215-derived-thread-worker";
    const FOREIGN: &str = "cad2215-derived-thread-foreign";
    const FOREIGN_OP: &str = "cad2215-derived-thread-foreign-op";
    const FOREIGN_MESSAGE: &str = "cad2215-derived-thread-foreign-result";
    const ELIGIBLE_MESSAGE: &str = "cad2215-derived-thread-eligible-result";
    const VISIBLE: &str = "CAD2215_DERIVED_THREAD_VISIBLE";
    const FOREIGN_TEXT: &str = "CAD2215_DERIVED_THREAD_FOREIGN";
    const ELIGIBLE_TEXT: &str = "CAD2215_DERIVED_THREAD_ELIGIBLE";

    let world = build_world();
    register(&world.shared, world._state.path(), ORDINARY, "fake", "fake");
    register(&world.shared, world._state.path(), FOREIGN, "fake", "fake");
    world.shared.store.ensure_thread(ORDINARY).unwrap();
    let reader = world.shared.store.agent(READER).unwrap();
    let foreign = world.shared.store.agent(FOREIGN).unwrap();
    let cloud = world.shared.store.agent(CLOUD_A).unwrap();
    let owner = Owner::new(ISSUER, SUBJECT);
    let foreign_grant = GrantTarget {
        cloud_agent_id: format!("{CLOUD_A}-agent"),
        uid: foreign.uid.clone(),
        epoch: registration_epoch(foreign.created),
        owner: owner.clone(),
    };
    let target = RemoteTarget {
        org: ORG.to_string(),
        endpoint: world._mock.endpoint.clone(),
        org_id: ORG_ID.to_string(),
        identity: None,
    };
    crate::remote_cli::cloud_agent_save_peer(
        &target,
        &owner,
        &foreign_grant.uid,
        foreign_grant.epoch,
        &json!({"token":"collab-cad2215-derived-foreign",
                "expires_at":crate::issue::time::now_epoch()+3600}),
    )
    .expect("foreign peer credential");
    world
        .shared
        .store
        .cloud_agent_source_grant(&CloudSource {
            cloud_agent_id: foreign_grant.cloud_agent_id.clone(),
            kind: SOURCE_PEER.to_string(),
            uid: foreign_grant.uid.clone(),
            registration_epoch: foreign_grant.epoch,
            owner: foreign_grant.owner.clone(),
            label: FOREIGN.to_string(),
            scope: CONSENT_SCOPE.to_string(),
            granted_by: "operator".to_string(),
            granted_at: 1.0,
            invoker_id: None,
            state: SOURCE_ACTIVE.to_string(),
        })
        .expect("foreign peer source grant");
    let db = rusqlite::Connection::open(world._state.path().join("cadence.sqlite3")).unwrap();
    const RESTRICTED_PROJECT: &str = "withheld-private";
    const RESTRICTED_ISSUE: &str = "WPRIV-1";
    const RESTRICTED_JOB: &str = "cad2215-withheld-private-job";
    const RESTRICTED_TASK: &str = "cad2215-withheld-private-task";
    const RESTRICTED_MESSAGE: &str = "cad2215-withheld-private-derived";
    const RESTRICTED_TEXT: &str = "CAD2215_WITHHELD_PRIVATE_DERIVED";
    let restricted_project = world._state.path().join("tracker").join(RESTRICTED_PROJECT);
    let restricted_issue = restricted_project.join(RESTRICTED_ISSUE);
    std::fs::create_dir_all(&restricted_issue).unwrap();
    std::fs::write(
        restricted_project.join("project.yaml"),
        "key: withheld-private\nprefix: WPRIV\npolicy:\n  version: 1\n  visibility: private\n",
    )
    .unwrap();
    std::fs::write(restricted_issue.join("issue.md"),
        "---\nid: WPRIV-1\ntitle: restricted cloud fixture\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n").unwrap();
    db.execute(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,repo,state,created,updated) \
         VALUES(?1,'Restricted fixture','spec.md',?2,?3,'fixture','open',1,1)",
        rusqlite::params![RESTRICTED_JOB, FOREIGN, RESTRICTED_ISSUE],
    )
    .unwrap();
    db.execute(
        "INSERT INTO tasks(id,job_id,title,assignee,state,created,updated) \
         VALUES(?1,?2,'Restricted task',?3,'running',1,1)",
        rusqlite::params![RESTRICTED_TASK, RESTRICTED_JOB, ORDINARY],
    )
    .unwrap();
    {
        let _uid = crate::daemon::serve::test_peer_uid_scope(world.agent_uid);
        let scope = scoped(Asserted::Agent(READER.to_string()), || {
            world
                .shared
                .project_scope(std::process::id(), "restricted fixture scope")
        })
        .unwrap()
        .expect("reader has scoped project access");
        assert!(
            scope.0.contains(OPEN_PROJECT),
            "open control project missing from reader scope"
        );
        assert!(
            !scope.0.contains(RESTRICTED_PROJECT),
            "restricted fixture project is readable by READER"
        );
        assert!(
            scope
                .1
                .get(RESTRICTED_ISSUE)
                .is_some_and(|holders| holders.iter().any(|p| p == RESTRICTED_PROJECT)),
            "restricted issue is not bound to its private project"
        );
    }
    insert_operation(
        &db,
        &foreign_grant.cloud_agent_id,
        FOREIGN_OP,
        "cad2215-derived-thread-foreign-conversation",
        &cloud.uid,
        &foreign_grant,
    );
    insert_message(&db, FOREIGN_OP, CLOUD_A, FOREIGN_TEXT);
    db.execute(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,created) \
         VALUES('cad2215-derived-thread-visible',?1,?2,'send','completed',?3,?4,1)",
        rusqlite::params![ORDINARY, VISIBLE, OPEN_TASK, OPEN_ISSUE],
    )
    .unwrap();
    world
        .shared
        .store
        .thread_append(
            ORDINARY,
            crate::store::NewEntry {
                role: "system",
                kind: "message",
                text: VISIBLE,
                payload: None,
                message_id: Some("cad2215-derived-thread-visible"),
            },
        )
        .unwrap()
        .expect("ordinary control entry");
    let call = |agent: &str, params: Value| -> Result<Value> {
        let _uid = crate::daemon::serve::test_peer_uid_scope(world.agent_uid);
        scoped(Asserted::Agent(agent.to_string()), || {
            world
                .shared
                .dispatch("thread_read", &params, std::process::id())
        })
    };
    let first = call(
        READER,
        json!({"alias":ORDINARY, "cursor_format":"opaque-v1", "limit":10}),
    )
    .expect("ordinary visible control");
    assert!(
        first.to_string().contains(VISIBLE),
        "fixture's visible entry missing: {first}"
    );
    let cursor = first["cursor"]
        .as_str()
        .filter(|s| !s.is_empty() && s.parse::<i64>().is_err())
        .expect("opaque control cursor")
        .to_string();
    let continuation =
        || json!({"alias":ORDINARY, "cursor_format":"opaque-v1", "after":cursor, "limit":10});
    let baseline = call(READER, continuation()).expect("empty control continuation");
    assert!(
        baseline["entries"].as_array().unwrap().is_empty(),
        "control not exhausted: {baseline}"
    );
    assert!(
        baseline.get("cloud_withheld").is_none(),
        "unexpected baseline warning: {baseline}"
    );

    // The durable origin is a real settled cloud operation and the delivery
    // is bound only to FOREIGN, never READER. It is project-readable so the
    // real cloud-source projection, not the project filter, decides it.
    db.execute(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,origin,sender_uid,recipient_uid,created) \
         VALUES(?1,?2,?3,'worker_result','completed',?4,?5,?6,?7,?7,1)",
        rusqlite::params![FOREIGN_MESSAGE, ORDINARY, FOREIGN_TEXT, OPEN_TASK, OPEN_ISSUE,
                          FOREIGN_OP, foreign.uid],
    ).unwrap();
    world
        .shared
        .store
        .thread_append(
            ORDINARY,
            crate::store::NewEntry {
                role: "system",
                kind: "message",
                text: FOREIGN_TEXT,
                payload: None,
                message_id: Some(FOREIGN_MESSAGE),
            },
        )
        .unwrap()
        .expect("foreign derived thread entry");
    let (derived, _) = world
        .shared
        .store
        .cloud_projection_rows(ORDINARY, Some(FOREIGN_MESSAGE))
        .unwrap();
    assert_eq!(
        derived.len(),
        1,
        "fixture did not classify the foreign row as cloud-derived"
    );
    assert_eq!(derived[0].sender_uid.as_deref(), Some(foreign.uid.as_str()));
    assert_eq!(
        derived[0].recipient_uid.as_deref(),
        Some(foreign.uid.as_str())
    );
    let owner_page = call(
        FOREIGN,
        json!({"alias":ORDINARY, "cursor_format":"opaque-v1", "limit":10}),
    )
    .expect("foreign delivery's actual enrolled owner");
    assert!(
        owner_page.to_string().contains(FOREIGN_TEXT),
        "foreign source owner control: {owner_page}"
    );
    let after_foreign =
        call(READER, continuation()).expect("same scoped continuation after foreign row");
    assert_eq!(
        after_foreign, baseline,
        "foreign-only row changed a member-observable response field"
    );
    // Same selected conversation, readable project, and exact delivery UID
    // and registration epoch, but the immutable owner on this settled cloud
    // operation is another human. A UID match alone must not turn that
    // foreign owner's retained activity into a visible warning or page row.
    const OLD_OWNER_OPERATION: &str = "cad2215-other-owner-operation";
    const OLD_OWNER_MESSAGE: &str = "cad2215-other-owner-delivery";
    const OLD_OWNER_TEXT: &str = "CAD2215_OTHER_OWNER_PRIVATE_ACTIVITY";
    let old_owner = Owner::new(ISSUER, "cad2215-previous-cloud-owner");
    assert_ne!(old_owner.subject, owner.subject);
    let old_grant = GrantTarget {
        cloud_agent_id: format!("{CLOUD_B}-agent"),
        uid: reader.uid.clone(),
        epoch: registration_epoch(reader.created),
        owner: old_owner.clone(),
    };
    let cloud_b = world.shared.store.agent(CLOUD_B).unwrap();
    insert_operation(
        &db,
        &old_grant.cloud_agent_id,
        OLD_OWNER_OPERATION,
        "cad2215-other-owner-conversation",
        &cloud_b.uid,
        &old_grant,
    );
    insert_message(
        &db,
        OLD_OWNER_OPERATION,
        CLOUD_B,
        "cad2215 old-owner cloud request",
    );
    db.execute(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,origin,sender_uid,recipient_uid,created) \
         VALUES(?1,?2,?3,'worker_result','completed',?4,?5,?6,?7,?7,1)",
        rusqlite::params![OLD_OWNER_MESSAGE, ORDINARY, OLD_OWNER_TEXT, OPEN_TASK, OPEN_ISSUE,
                          OLD_OWNER_OPERATION, reader.uid],
    ).unwrap();
    world
        .shared
        .store
        .thread_append(
            ORDINARY,
            crate::store::NewEntry {
                role: "system",
                kind: "message",
                text: OLD_OWNER_TEXT,
                payload: None,
                message_id: Some(OLD_OWNER_MESSAGE),
            },
        )
        .unwrap()
        .expect("same-UID different-owner derived thread row");
    let (old_rows, _) = world
        .shared
        .store
        .cloud_projection_rows(ORDINARY, Some(OLD_OWNER_MESSAGE))
        .unwrap();
    assert_eq!(
        old_rows.len(),
        1,
        "old-owner row must be real derived cloud data"
    );
    assert_eq!(old_rows[0].sender_uid.as_deref(), Some(reader.uid.as_str()));
    assert_eq!(
        old_rows[0].recipient_uid.as_deref(),
        Some(reader.uid.as_str())
    );
    assert_eq!(
        old_rows[0].owner_issuer.as_deref(),
        Some(old_owner.issuer.as_str())
    );
    assert_eq!(
        old_rows[0].owner_subject.as_deref(),
        Some(old_owner.subject.as_str())
    );
    let stored_old = world
        .shared
        .store
        .cloud_agent_operation(&old_grant.cloud_agent_id, OLD_OWNER_OPERATION)
        .unwrap()
        .expect("stored old-owner operation");
    assert_eq!(stored_old.source_uid, reader.uid);
    assert_eq!(
        stored_old.registration_epoch,
        registration_epoch(reader.created)
    );
    assert_eq!(stored_old.owner, old_owner);

    let after_old_owner = call(READER, continuation())
        .expect("same empty continuation after another owner's delivery");
    assert_eq!(
        after_old_owner, baseline,
        "different owner's same-UID row changed scoped response or warning"
    );
    let refreshed_first = call(
        READER,
        json!({"alias":ORDINARY, "cursor_format":"opaque-v1", "limit":10}),
    )
    .expect("fresh readable page after another owner's delivery");
    assert_eq!(
        refreshed_first["entries"], first["entries"],
        "foreign owner changed visible rows"
    );
    for field in [
        "alias",
        "thread",
        "cursor_format",
        "more_before",
        "more_after",
        "cloud_withheld",
        "_reader_scope",
    ] {
        assert_eq!(
            refreshed_first.get(field),
            first.get(field),
            "foreign owner changed {field}"
        );
    }
    for field in ["cursor", "before_cursor"] {
        assert_eq!(
            refreshed_first[field].as_str().map(str::len),
            first[field].as_str().map(str::len),
            "foreign owner changed {field} shape"
        );
    }
    assert!(!refreshed_first.to_string().contains(OLD_OWNER_TEXT));
    // Same requested thread and reader-bound source, but the real task and
    // issue are in a private project outside the reader's project scope.
    db.execute(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,origin,sender_uid,recipient_uid,created) \
         VALUES(?1,?2,?3,'worker_result','completed',?4,?5,?6,?7,?7,1)",
        rusqlite::params![RESTRICTED_MESSAGE, ORDINARY, RESTRICTED_TEXT,
                          RESTRICTED_TASK, RESTRICTED_ISSUE, OP_B, reader.uid],
    ).unwrap();
    world
        .shared
        .store
        .thread_append(
            ORDINARY,
            crate::store::NewEntry {
                role: "system",
                kind: "message",
                text: RESTRICTED_TEXT,
                payload: None,
                message_id: Some(RESTRICTED_MESSAGE),
            },
        )
        .unwrap()
        .expect("reader-bound restricted-project thread row");
    let home_thread = world.shared.store.ensure_thread(ORDINARY).unwrap();
    let home_rows = world
        .shared
        .store
        .thread_entries_of(&home_thread.id, 0, 100)
        .unwrap();
    assert!(
        home_rows
            .iter()
            .any(|row| row.message_id.as_deref() == Some(RESTRICTED_MESSAGE)),
        "restricted control row is not in the requested conversation"
    );
    let (restricted_derived, _) = world
        .shared
        .store
        .cloud_projection_rows(ORDINARY, Some(RESTRICTED_MESSAGE))
        .unwrap();
    assert_eq!(
        restricted_derived.len(),
        1,
        "restricted row not classified as cloud-derived"
    );
    assert_eq!(
        restricted_derived[0].sender_uid.as_deref(),
        Some(reader.uid.as_str())
    );
    assert_eq!(
        restricted_derived[0].recipient_uid.as_deref(),
        Some(reader.uid.as_str())
    );
    let restricted_active = call(READER, continuation())
        .expect("same continuation with restricted row and live cloud source");
    assert_eq!(
        restricted_active, baseline,
        "project-hidden own-bound row changed the response while its source was live"
    );
    world
        .shared
        .store
        .cloud_agent_source_revoke(
            &format!("{CLOUD_B}-agent"),
            SOURCE_PEER,
            &reader.uid,
            registration_epoch(reader.created),
            &owner,
        )
        .expect("revoke reader's restricted-project cloud source");
    let restricted_revoked =
        call(READER, continuation()).expect("same continuation after project-hidden source revoke");
    assert_eq!(
        restricted_revoked, baseline,
        "project-hidden own-bound revoked row changed the empty continuation"
    );

    // A different conversation of the SAME alias is not the requested
    // thread. This row really is bound to READER, unlike the foreign-bound
    // row above; its cloud source is then revoked through the real store.
    // Merely adding that ineligible row must not change even the warning in
    // the exhausted requested conversation's response.
    const OTHER_MESSAGE: &str = "cad2215-other-conversation-own-derived";
    const OTHER_TEXT: &str = "CAD2215_OTHER_CONVERSATION_OWN_REVOKED";
    let requested_thread = world.shared.store.ensure_thread(ORDINARY).unwrap();
    let (other_thread, created) = world
        .shared
        .store
        .conversation_create(
            ORDINARY,
            "cad2215-other-conversation-install",
            None,
            crate::store::ConversationKind::Fresh,
        )
        .expect("create alternate same-alias conversation");
    assert!(created);
    assert_ne!(other_thread.id, requested_thread.id);
    db.execute(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,origin,sender_uid,recipient_uid,created) \
         VALUES(?1,?2,?3,'worker_result','completed',?4,?5,?6,?7,?7,1)",
        rusqlite::params![OTHER_MESSAGE, ORDINARY, OTHER_TEXT, OPEN_TASK, OPEN_ISSUE,
                          OP_B, reader.uid],
    ).unwrap();
    world
        .shared
        .store
        .thread_append_to(
            &other_thread.id,
            crate::store::NewEntry {
                role: "system",
                kind: "message",
                text: OTHER_TEXT,
                payload: None,
                message_id: Some(OTHER_MESSAGE),
            },
        )
        .unwrap()
        .expect("own-bound alternate-conversation row");
    let other_rows = world
        .shared
        .store
        .thread_entries_of(&other_thread.id, 0, 100)
        .unwrap();
    assert!(
        other_rows
            .iter()
            .any(|row| row.message_id.as_deref() == Some(OTHER_MESSAGE)),
        "the other-conversation control row was not stored"
    );
    let requested_rows = world
        .shared
        .store
        .thread_entries_of(&requested_thread.id, 0, 100)
        .unwrap();
    assert!(
        !requested_rows
            .iter()
            .any(|row| row.message_id.as_deref() == Some(OTHER_MESSAGE)),
        "alternate row unexpectedly belongs to the requested conversation"
    );
    let (other_derived, _) = world
        .shared
        .store
        .cloud_projection_rows(ORDINARY, Some(OTHER_MESSAGE))
        .unwrap();
    assert_eq!(
        other_derived.len(),
        1,
        "alternate row not classified as cloud-derived"
    );
    assert_eq!(
        other_derived[0].sender_uid.as_deref(),
        Some(reader.uid.as_str())
    );
    assert_eq!(
        other_derived[0].recipient_uid.as_deref(),
        Some(reader.uid.as_str())
    );
    let after_other = call(READER, continuation())
        .expect("same requested conversation after unrelated own-bound revoke");
    assert_eq!(
        after_other, baseline,
        "own-bound revoked row in another conversation changed the empty continuation"
    );

    // A genuinely caller-bound cloud-derived delivery is readable while the
    // enrolled source grant is active. Revoking exactly that source leaves
    // its local delivery row in place and forces the real guard to withhold it
    // and report incomplete cloud history, rather than suppressing all warnings.
    db.execute(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,origin,sender_uid,recipient_uid,created) \
         VALUES(?1,?2,?3,'worker_result','completed',?4,?5,?6,?7,?7,1)",
        rusqlite::params![ELIGIBLE_MESSAGE, ORDINARY, ELIGIBLE_TEXT, OPEN_TASK, OPEN_ISSUE,
                          OP_A, reader.uid],
    ).unwrap();
    world
        .shared
        .store
        .thread_append(
            ORDINARY,
            crate::store::NewEntry {
                role: "system",
                kind: "message",
                text: ELIGIBLE_TEXT,
                payload: None,
                message_id: Some(ELIGIBLE_MESSAGE),
            },
        )
        .unwrap()
        .expect("reader-bound derived thread entry");
    let eligible = call(READER, continuation()).expect("reader source currently authorized");
    assert!(
        eligible.to_string().contains(ELIGIBLE_TEXT),
        "eligible source control: {eligible}"
    );
    assert!(
        eligible.get("cloud_withheld").is_none(),
        "eligible control unexpectedly withheld: {eligible}"
    );
    world
        .shared
        .store
        .cloud_agent_source_revoke(
            &format!("{CLOUD_A}-agent"),
            SOURCE_PEER,
            &reader.uid,
            registration_epoch(reader.created),
            &owner,
        )
        .expect("revoke real reader source grant");
    let denied =
        call(READER, continuation()).expect("eligible unavailable source gives a scoped warning");
    assert!(
        !denied.to_string().contains(ELIGIBLE_TEXT),
        "revoked content escaped: {denied}"
    );
    assert!(
        !denied.to_string().contains(FOREIGN_TEXT),
        "foreign content escaped: {denied}"
    );
    assert!(
        denied
            .get("cloud_withheld")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty()),
        "eligible unavailable cloud history was silently hidden: {denied}"
    );
}

#[test]
fn scoped_thread_never_returns_rows_revoked_during_final_scan_refresh() {
    use std::sync::atomic::AtomicUsize;
    const ORDINARY: &str = "cad2215-midscan-worker";
    const FOREIGN: &str = "cad2215-midscan-foreign";
    const LATE: &str = "cad2215-midscan-late-foreign";
    const SECRET: &str = "CAD2215_MIDSCAN_REVOKED_SECRET";
    type HookState = (
        std::sync::Weak<Shared>,
        std::path::PathBuf,
        GrantTarget,
        String,
    );
    let slot: Arc<Mutex<Option<HookState>>> = Arc::new(Mutex::new(None));
    let armed = Arc::new(AtomicBool::new(false));
    let injected = Arc::new(AtomicBool::new(false));
    let refreshed = Arc::new(AtomicUsize::new(0));
    let batch: crate::daemon::AgentThreadBatchObserver = {
        let slot = Arc::clone(&slot);
        let armed = Arc::clone(&armed);
        let injected = Arc::clone(&injected);
        Arc::new(move |alias: &str, phase: &str, selected: usize| {
            if alias != ORDINARY
                || phase != "confirmation"
                || selected != 9
                || !armed.load(Ordering::SeqCst)
                || injected.swap(true, Ordering::SeqCst)
            {
                return;
            }
            let (shared, path, grant, foreign_uid) = slot.lock().unwrap().clone().unwrap();
            let shared = shared.upgrade().expect("test world still alive");
            shared
                .store
                .cloud_agent_source_revoke(
                    &grant.cloud_agent_id,
                    SOURCE_PEER,
                    &grant.uid,
                    grant.epoch,
                    &grant.owner,
                )
                .expect("revoke the actual selected rows' cloud source");
            let db = rusqlite::Connection::open(path.join("cadence.sqlite3")).unwrap();
            db.execute(
                "INSERT INTO messages(id,alias,body,source,state,task_id,issue,origin,sender_uid,recipient_uid,created) \
                 VALUES(?1,?2,'foreign arrival','worker_result','completed',?3,?4,?5,?6,?6,1)",
                rusqlite::params![LATE, ORDINARY, OPEN_TASK, OPEN_ISSUE, OP_A, foreign_uid],
            ).unwrap();
            shared
                .store
                .thread_append(
                    ORDINARY,
                    crate::store::NewEntry {
                        role: "system",
                        kind: "message",
                        text: "foreign arrival",
                        payload: None,
                        message_id: Some(LATE),
                    },
                )
                .unwrap()
                .expect("insert unseen foreign row into next scan batch");
        })
    };
    let projection: crate::daemon::CloudProjectionObserver = {
        let armed = Arc::clone(&armed);
        let injected = Arc::clone(&injected);
        let refreshed = Arc::clone(&refreshed);
        Arc::new(move |alias: &str, _withheld: Option<&str>| {
            if alias == ORDINARY && armed.load(Ordering::SeqCst) && injected.load(Ordering::SeqCst)
            {
                refreshed.fetch_add(1, Ordering::SeqCst);
            }
        })
    };
    let world = build_world_with_thread_observers(Some(projection), Some(batch));
    register(&world.shared, world._state.path(), ORDINARY, "fake", "fake");
    register(&world.shared, world._state.path(), FOREIGN, "fake", "fake");
    let reader = world.shared.store.agent(READER).unwrap();
    let foreign = world.shared.store.agent(FOREIGN).unwrap();
    let grant = GrantTarget {
        cloud_agent_id: format!("{CLOUD_A}-agent"),
        uid: reader.uid.clone(),
        epoch: registration_epoch(reader.created),
        owner: Owner::new(ISSUER, SUBJECT),
    };
    world.shared.store.ensure_thread(ORDINARY).unwrap();
    let db = rusqlite::Connection::open(world._state.path().join("cadence.sqlite3")).unwrap();
    // Nine admitted rows and 23 delivery-foreign rows exactly fill the first
    // 32-row raw batch while leaving fewer than the limit of 10 visible.
    for n in 0..32 {
        let message = format!("cad2215-midscan-entry-{n}");
        let body = if n < 9 {
            format!("{SECRET}-{n}")
        } else {
            format!("foreign-{n}")
        };
        let uid = if n < 9 { &reader.uid } else { &foreign.uid };
        db.execute(
            "INSERT INTO messages(id,alias,body,source,state,task_id,issue,origin,sender_uid,recipient_uid,created) \
             VALUES(?1,?2,?3,'worker_result','completed',?4,?5,?6,?7,?7,1)",
            rusqlite::params![message, ORDINARY, body, OPEN_TASK, OPEN_ISSUE, OP_A, uid],
        ).unwrap();
        world
            .shared
            .store
            .thread_append(
                ORDINARY,
                crate::store::NewEntry {
                    role: "system",
                    kind: "message",
                    text: &body,
                    payload: None,
                    message_id: Some(&message),
                },
            )
            .unwrap()
            .expect("append scoped test row");
    }
    drop(db);
    *slot.lock().unwrap() = Some((
        Arc::downgrade(&world.shared),
        world._state.path().to_path_buf(),
        grant,
        foreign.uid,
    ));
    let call = || {
        let _uid = crate::daemon::serve::test_peer_uid_scope(world.agent_uid);
        scoped(Asserted::Agent(READER.to_string()), || {
            world.shared.dispatch(
                "thread_read",
                &json!({"alias":ORDINARY,"cursor_format":"opaque-v1","limit":10}),
                std::process::id(),
            )
        })
    };
    let baseline = call().expect("eligible source must read before revocation");
    assert_eq!(baseline["entries"].as_array().unwrap().len(), 9);
    assert!(
        baseline.to_string().contains(SECRET),
        "positive control lacks eligible content"
    );
    armed.store(true, Ordering::SeqCst);
    let raced = call();
    armed.store(false, Ordering::SeqCst);
    assert!(
        injected.load(Ordering::SeqCst),
        "confirmation must select nine rows before revocation"
    );
    assert!(
        refreshed.load(Ordering::SeqCst) > 0,
        "foreign arrival must force a new cloud admission"
    );
    if let Ok(page) = raced {
        assert!(
            !page.to_string().contains(SECRET),
            "revoked first-batch content escaped: {page}"
        );
    }
    let after = call().expect("post-revocation read must remain available");
    assert!(
        !after.to_string().contains(SECRET),
        "real grant revoke did not withhold content: {after}"
    );
    assert!(
        after
            .get("cloud_withheld")
            .and_then(Value::as_str)
            .is_some(),
        "accessible withheld cloud content needs a warning: {after}"
    );
}

#[test]
fn scoped_thread_does_not_skip_a_row_restored_before_final_admission() {
    const ORDINARY: &str = "cad2215-restore-worker";
    const ELIGIBLE_ID: &str = "cad2215-restore-derived";
    const LATER_ID: &str = "cad2215-restore-later";
    const SECRET: &str = "CAD2215_RESTORED_VISIBLE_ROW";
    const LATER: &str = "CAD2215_RESTORE_LATER_LOCAL";
    type State = (std::sync::Weak<Shared>, CloudSource);
    let state: Arc<Mutex<Option<State>>> = Arc::new(Mutex::new(None));
    let armed = Arc::new(AtomicBool::new(false));
    let restored = Arc::new(AtomicBool::new(false));
    let batch: crate::daemon::AgentThreadBatchObserver = {
        let state = Arc::clone(&state);
        let armed = Arc::clone(&armed);
        let restored = Arc::clone(&restored);
        Arc::new(move |alias: &str, phase: &str, selected: usize| {
            if alias != ORDINARY
                || phase != "confirmation"
                || selected != 1
                || !armed.load(Ordering::SeqCst)
                || restored.swap(true, Ordering::SeqCst)
            {
                return;
            }
            let (shared, grant) = state.lock().unwrap().clone().unwrap();
            shared
                .upgrade()
                .expect("world live")
                .store
                .cloud_agent_source_grant(&grant)
                .expect("restore real source grant after confirmation scan");
        })
    };
    let world = build_world_with_thread_observers(None, Some(batch));
    register(&world.shared, world._state.path(), ORDINARY, "fake", "fake");
    let reader = world.shared.store.agent(READER).unwrap();
    let owner = Owner::new(ISSUER, SUBJECT);
    let grant = CloudSource {
        cloud_agent_id: format!("{CLOUD_A}-agent"),
        kind: SOURCE_PEER.to_string(),
        uid: reader.uid.clone(),
        registration_epoch: registration_epoch(reader.created),
        owner: owner.clone(),
        label: READER.to_string(),
        scope: CONSENT_SCOPE.to_string(),
        granted_by: "operator".to_string(),
        granted_at: 1.0,
        invoker_id: None,
        state: SOURCE_ACTIVE.to_string(),
    };
    world.shared.store.ensure_thread(ORDINARY).unwrap();
    let db = rusqlite::Connection::open(world._state.path().join("cadence.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,origin,sender_uid,recipient_uid,created) \
         VALUES(?1,?2,?3,'worker_result','completed',?4,?5,?6,?7,?7,1)",
        rusqlite::params![ELIGIBLE_ID, ORDINARY, SECRET, OPEN_TASK, OPEN_ISSUE, OP_A, reader.uid],
    ).unwrap();
    db.execute(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,created) \
         VALUES(?1,?2,?3,'send','completed',?4,?5,1)",
        rusqlite::params![LATER_ID, ORDINARY, LATER, OPEN_TASK, OPEN_ISSUE],
    )
    .unwrap();
    for (id, text) in [(ELIGIBLE_ID, SECRET), (LATER_ID, LATER)] {
        world
            .shared
            .store
            .thread_append(
                ORDINARY,
                crate::store::NewEntry {
                    role: "system",
                    kind: "message",
                    text,
                    payload: None,
                    message_id: Some(id),
                },
            )
            .unwrap()
            .expect("append ordered control row");
    }
    drop(db);
    *state.lock().unwrap() = Some((Arc::downgrade(&world.shared), grant.clone()));
    let call = || {
        let _uid = crate::daemon::serve::test_peer_uid_scope(world.agent_uid);
        scoped(Asserted::Agent(READER.to_string()), || {
            world.shared.dispatch(
                "thread_read",
                &json!({"alias":ORDINARY,"cursor_format":"opaque-v1","limit":10}),
                std::process::id(),
            )
        })
    };
    let allowed = call().expect("live grant control");
    assert!(allowed.to_string().contains(SECRET) && allowed.to_string().contains(LATER));
    world
        .shared
        .store
        .cloud_agent_source_revoke(
            &grant.cloud_agent_id,
            SOURCE_PEER,
            &grant.uid,
            grant.registration_epoch,
            &owner,
        )
        .expect("revoke real source before scan");
    let denied = call().expect("denied initial control");
    assert!(!denied.to_string().contains(SECRET) && denied.to_string().contains(LATER));
    assert!(
        denied.get("cloud_withheld").is_some(),
        "own denied row must warn: {denied}"
    );
    armed.store(true, Ordering::SeqCst);
    let raced = call();
    armed.store(false, Ordering::SeqCst);
    assert!(
        restored.load(Ordering::SeqCst),
        "confirmation scan did not restore grant"
    );
    if let Ok(page) = raced {
        assert!(
            page.to_string().contains(SECRET),
            "page advanced past the newly authorized row: {page}"
        );
    }
    let fresh = call().expect("restored grant positive control");
    assert!(fresh.to_string().contains(SECRET) && fresh.to_string().contains(LATER));
}

#[test]
fn scoped_cloud_thread_handles_expired_peer_without_foreign_activity_leak() {
    const FOREIGN: &str = "cad2215-expired-foreign-peer";
    const FOREIGN_OP: &str = "cad2215-expired-foreign-operation";
    const FIRST: &str = "CAD2215_EXPIRED_OWN_FIRST";
    const OWN: &str = "CAD2215_EXPIRED_OWN_WITHHELD";
    const OTHER: &str = "CAD2215_EXPIRED_FOREIGN_ONLY";
    let world = build_world();
    register(&world.shared, world._state.path(), FOREIGN, "fake", "fake");
    let foreign = world.shared.store.agent(FOREIGN).unwrap();
    let reader = world.shared.store.agent(READER).unwrap();
    let cloud = world.shared.store.agent(CLOUD_A).unwrap();
    let owner = Owner::new(ISSUER, SUBJECT);
    let target = RemoteTarget {
        org: ORG.to_string(),
        endpoint: world._mock.endpoint.clone(),
        org_id: ORG_ID.to_string(),
        identity: None,
    };
    let foreign_grant = GrantTarget {
        cloud_agent_id: format!("{CLOUD_A}-agent"),
        uid: foreign.uid.clone(),
        epoch: registration_epoch(foreign.created),
        owner: owner.clone(),
    };
    crate::remote_cli::cloud_agent_save_peer(
        &target, &owner, &foreign_grant.uid, foreign_grant.epoch,
        &json!({"token":"collab-cad2215-expired-foreign", "expires_at":crate::issue::time::now_epoch()+3600}),
    ).expect("enroll foreign peer");
    world
        .shared
        .store
        .cloud_agent_source_grant(&CloudSource {
            cloud_agent_id: foreign_grant.cloud_agent_id.clone(),
            kind: SOURCE_PEER.to_string(),
            uid: foreign_grant.uid.clone(),
            registration_epoch: foreign_grant.epoch,
            owner: owner.clone(),
            label: FOREIGN.to_string(),
            scope: CONSENT_SCOPE.to_string(),
            granted_by: "operator".to_string(),
            granted_at: 1.0,
            invoker_id: None,
            state: SOURCE_ACTIVE.to_string(),
        })
        .expect("grant real foreign source");
    let db = rusqlite::Connection::open(world._state.path().join("cadence.sqlite3")).unwrap();
    insert_operation(
        &db,
        &foreign_grant.cloud_agent_id,
        FOREIGN_OP,
        "cad2215-expired-foreign-conversation",
        &cloud.uid,
        &foreign_grant,
    );
    insert_message(&db, FOREIGN_OP, CLOUD_A, OTHER);
    world.shared.store.ensure_thread(CLOUD_A).unwrap();
    let append = |text: &str, message: &str| {
        world
            .shared
            .store
            .thread_append(
                CLOUD_A,
                crate::store::NewEntry {
                    role: "agent",
                    kind: "assistant_text",
                    text,
                    payload: None,
                    message_id: Some(message),
                },
            )
            .unwrap()
            .expect("append cloud operation thread entry")
    };
    append(FIRST, OP_A);
    let call = |agent: &str, params: Value| {
        let _uid = crate::daemon::serve::test_peer_uid_scope(world.agent_uid);
        scoped(Asserted::Agent(agent.to_string()), || {
            world
                .shared
                .dispatch("thread_read", &params, std::process::id())
        })
    };
    let first = call(
        READER,
        json!({"alias":CLOUD_A,"cursor_format":"opaque-v1","limit":10}),
    )
    .expect("own credential initially authorizes own operation");
    assert!(
        first.to_string().contains(FIRST),
        "own positive control: {first}"
    );
    let cursor = first["cursor"]
        .as_str()
        .expect("own continuation")
        .to_string();
    let continuation =
        || json!({"alias":CLOUD_A,"cursor_format":"opaque-v1","after":cursor,"limit":10});
    // Expire only the reader peer through the real storage API in World's
    // temporary HOME. No sleep and no access to live credentials.
    crate::remote_cli::cloud_agent_save_peer(
        &target,
        &owner,
        &reader.uid,
        registration_epoch(reader.created),
        &json!({"token":"collab-cad2215-expired-own", "expires_at":1}),
    )
    .expect("expire reader credential");
    let before = call(READER, continuation());
    append(OTHER, FOREIGN_OP);
    let foreign_page = call(
        FOREIGN,
        json!({"alias":CLOUD_A,"cursor_format":"opaque-v1","limit":10}),
    )
    .expect("foreign enrolled owner can read its own operation");
    assert!(
        foreign_page.to_string().contains(OTHER),
        "foreign owner control: {foreign_page}"
    );
    let after = call(READER, continuation());
    match (before, after) {
        (Err(a), Err(b)) => assert_eq!(a.to_string(), b.to_string(), "foreign row changed refusal"),
        (Ok(a), Ok(b)) => assert_eq!(a, b, "foreign row changed scoped response"),
        (a, b) => panic!("foreign row changed success/refusal: {a:?} / {b:?}"),
    }
    append(OWN, OP_A);
    match call(READER, continuation()) {
        Err(_) => {} // Identity/credential refusal is explicitly fail-closed.
        Ok(page) => {
            assert!(
                !page.to_string().contains(OWN),
                "expired credential disclosed own content: {page}"
            );
            assert!(
                !page.to_string().contains(OTHER),
                "foreign content disclosed: {page}"
            );
            assert!(
                page.get("cloud_withheld")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty()),
                "own project-visible operation withheld without warning: {page}"
            );
        }
    }
}

#[test]
fn scoped_thread_ignores_prior_owner_origin_without_current_target_credential() {
    const ORDINARY: &str = "cad2215-foreign-target-worker";
    const VISIBLE_ID: &str = "cad2215-foreign-target-visible";
    const VISIBLE_TEXT: &str = "CAD2215_FOREIGN_TARGET_VISIBLE";
    const OTHER_ALIAS: &str = "cad2215-foreign-target-cloud";
    const OTHER_CLOUD_ID: &str = "cad2215-foreign-target-cloud-agent";
    const OTHER_ORG_ID: &str = "cad2215-foreign-target-organization";
    const OTHER_OPERATION: &str = "cad2215-foreign-target-operation";
    const OTHER_MESSAGE: &str = "cad2215-foreign-target-delivery";
    const OTHER_TEXT: &str = "CAD2215_FOREIGN_TARGET_PRIOR_OWNER_SECRET";

    let world = build_world();
    register(&world.shared, world._state.path(), ORDINARY, "fake", "fake");
    world.shared.store.ensure_thread(ORDINARY).unwrap();
    let reader = world.shared.store.agent(READER).unwrap();
    let current_owner = Owner::new(ISSUER, SUBJECT);
    let current_target = RemoteTarget {
        org: ORG.to_string(),
        endpoint: world._mock.endpoint.clone(),
        org_id: ORG_ID.to_string(),
        identity: None,
    };
    let current_pin = crate::remote_cli::cloud_agent_peer_owner(
        &current_target,
        &reader.uid,
        registration_epoch(reader.created),
    )
    .expect("reader's real current target credential");
    assert_eq!(current_pin.issuer, current_owner.issuer);
    assert_eq!(current_pin.subject_id, current_owner.subject);
    let db = rusqlite::Connection::open(world._state.path().join("cadence.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,created) \
         VALUES(?1,?2,?3,'send','completed',?4,?5,1)",
        rusqlite::params![VISIBLE_ID, ORDINARY, VISIBLE_TEXT, OPEN_TASK, OPEN_ISSUE],
    )
    .unwrap();
    world
        .shared
        .store
        .thread_append(
            ORDINARY,
            crate::store::NewEntry {
                role: "system",
                kind: "message",
                text: VISIBLE_TEXT,
                payload: None,
                message_id: Some(VISIBLE_ID),
            },
        )
        .unwrap()
        .expect("visible ordinary control entry");
    let call = |params: Value| -> Result<Value> {
        let _uid = crate::daemon::serve::test_peer_uid_scope(world.agent_uid);
        scoped(Asserted::Agent(READER.to_string()), || {
            world
                .shared
                .dispatch("thread_read", &params, std::process::id())
        })
    };
    // Cloud identity is deliberately a different organization ID: peer_matches
    // includes target organization ID and audience as well as UID and epoch.
    register(
        &world.shared,
        world._state.path(),
        OTHER_ALIAS,
        PROVIDER,
        ENDPOINT_KIND,
    );
    let other_cloud = world.shared.store.agent(OTHER_ALIAS).unwrap();
    world
        .shared
        .store
        .cloud_agent_connect(&CloudAgent {
            id: OTHER_CLOUD_ID.to_string(),
            alias: OTHER_ALIAS.to_string(),
            organization_id: OTHER_ORG_ID.to_string(),
            org_slug: ORG.to_string(),
            endpoint: world._mock.endpoint.clone(),
            remote_agent_id: "cad2215-foreign-target-remote".to_string(),
            runtime: "devin".to_string(),
            model: None,
            capabilities: json!({}),
            provider_session_id: None,
            provenance: json!({}),
            recipient_uid: other_cloud.uid.clone(),
            consent_scope: CONSENT_SCOPE.to_string(),
            state: STATE_CONNECTED.to_string(),
            created: 1.0,
            updated: 1.0,
        })
        .expect("connect distinct cloud target");
    let other_target = RemoteTarget {
        org: ORG.to_string(),
        endpoint: world._mock.endpoint.clone(),
        org_id: OTHER_ORG_ID.to_string(),
        identity: None,
    };
    assert_ne!(other_target.org_id, current_target.org_id);
    let missing = crate::remote_cli::cloud_agent_peer_owner(
        &other_target,
        &reader.uid,
        registration_epoch(reader.created),
    )
    .expect_err("reader unexpectedly has a credential for foreign target");
    assert!(
        missing
            .to_string()
            .contains("no enrolled cloud-peer credential"),
        "fixture's missing-target credential failed for another reason: {missing:?}"
    );

    let first = call(json!({"alias":ORDINARY, "cursor_format":"opaque-v1", "limit":10}))
        .expect("real visible scoped thread control");
    assert!(
        first.to_string().contains(VISIBLE_TEXT),
        "visible fixture row missing: {first}"
    );
    assert_eq!(
        first["_reader_scope"],
        cadence_core::board_api::THREAD_OPAQUE_SCOPE_ACK
    );
    let cursor = first["cursor"]
        .as_str()
        .filter(|s| !s.is_empty() && s.parse::<i64>().is_err())
        .expect("opaque continuation cursor")
        .to_string();
    let continuation = || {
        json!({
            "alias":ORDINARY, "cursor_format":"opaque-v1", "after":cursor, "limit":10
        })
    };
    let baseline = call(continuation()).expect("exhausted continuation before hidden insertion");
    assert!(
        baseline["entries"].as_array().unwrap().is_empty(),
        "not exhausted: {baseline}"
    );
    assert_eq!(
        baseline["cursor"], cursor,
        "empty page must echo supplied token"
    );
    assert!(
        baseline.get("cloud_withheld").is_none(),
        "unexpected baseline warning: {baseline}"
    );

    let prior_owner = Owner::new(ISSUER, "cad2215-foreign-target-previous-owner");
    assert_ne!(prior_owner.subject, current_owner.subject);
    let prior_source = GrantTarget {
        cloud_agent_id: OTHER_CLOUD_ID.to_string(),
        uid: reader.uid.clone(),
        epoch: registration_epoch(reader.created),
        owner: prior_owner.clone(),
    };
    insert_operation(
        &db,
        OTHER_CLOUD_ID,
        OTHER_OPERATION,
        "cad2215-foreign-target-conversation",
        &other_cloud.uid,
        &prior_source,
    );
    // The ancestry CTE resolves cloud owner only through this canonical parent.
    insert_message(
        &db,
        OTHER_OPERATION,
        OTHER_ALIAS,
        "prior owner cloud request",
    );
    db.execute(
        "INSERT INTO messages(id,alias,body,source,state,task_id,issue,origin,sender_uid,recipient_uid,created) \
         VALUES(?1,?2,?3,'worker_result','completed',?4,?5,?6,?7,?7,1)",
        rusqlite::params![OTHER_MESSAGE, ORDINARY, OTHER_TEXT, OPEN_TASK, OPEN_ISSUE,
                          OTHER_OPERATION, reader.uid],
    ).unwrap();
    world
        .shared
        .store
        .thread_append(
            ORDINARY,
            crate::store::NewEntry {
                role: "system",
                kind: "message",
                text: OTHER_TEXT,
                payload: None,
                message_id: Some(OTHER_MESSAGE),
            },
        )
        .unwrap()
        .expect("same-UID prior-owner derived row");
    let stored = world
        .shared
        .store
        .cloud_agent_operation(OTHER_CLOUD_ID, OTHER_OPERATION)
        .unwrap()
        .expect("stored settled prior-owner operation");
    assert_eq!(stored.source_uid, reader.uid);
    assert_eq!(
        stored.registration_epoch,
        registration_epoch(reader.created)
    );
    assert_eq!(stored.owner, prior_owner);
    assert_eq!(stored.status, "completed");
    let (derived, _) = world
        .shared
        .store
        .cloud_projection_rows(ORDINARY, Some(OTHER_MESSAGE))
        .unwrap();
    assert_eq!(
        derived.len(),
        1,
        "fixture must classify the row as cloud-derived"
    );
    assert_eq!(derived[0].cloud_agent_id.as_deref(), Some(OTHER_CLOUD_ID));
    assert_eq!(derived[0].sender_uid.as_deref(), Some(reader.uid.as_str()));
    assert_eq!(
        derived[0].recipient_uid.as_deref(),
        Some(reader.uid.as_str())
    );
    assert_eq!(
        derived[0].owner_issuer.as_deref(),
        Some(prior_owner.issuer.as_str())
    );
    assert_eq!(
        derived[0].owner_subject.as_deref(),
        Some(prior_owner.subject.as_str())
    );
    let selected = world.shared.store.ensure_thread(ORDINARY).unwrap();
    assert!(
        world
            .shared
            .store
            .thread_entries_of(&selected.id, 0, 100)
            .unwrap()
            .iter()
            .any(|row| row.message_id.as_deref() == Some(OTHER_MESSAGE)),
        "prior-owner row must be in the requested conversation"
    );

    let after_foreign = call(continuation())
        .expect("a foreign target without a current peer credential must not fail this read");
    assert_eq!(
        after_foreign, baseline,
        "prior-owner foreign target changed the exhausted page, token, or warning"
    );
}

/// A cloud grant withdrawn between a task detail's selection and its response
/// must not serve the message bodies the earlier snapshot admitted. The event
/// reader re-proves after selection for exactly this reason; the detail path
/// carries bodies too.
#[test]
fn task_detail_refuses_bodies_when_the_cloud_grant_is_revoked_after_selection() {
    let world = build_world();

    // Positive control: the admitted cloud message body is served while the
    // grant is live, so the refusal below is a revocation, not a broken
    // fixture.
    let live = task_detail(&world, OPEN_TASK).expect("the admitted task detail must be served");
    assert!(
        live.to_string().contains(CLOUD_BODY),
        "the admitted cloud body is missing: {live}"
    );

    // Revoke inside the hook: after the bodies are selected, before the
    // response leaves.
    world.detail_mode.store(DETAIL_REVOKE_A, Ordering::SeqCst);
    let error = task_detail(&world, OPEN_TASK)
        .expect_err("a grant revoked after selection must not serve the body");
    assert_eq!(error.kind(), "rejected", "{error:?}");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("task show refused"),
        "the refusal did not name the reproof: {rendered}"
    );
    assert!(
        !rendered.contains(CLOUD_BODY),
        "the refusal leaked the revoked body: {rendered}"
    );
}

/// A kickoff the reader cannot admit is redacted out of the detail, not
/// turned into a refusal: the denial is already visible in the reply, so
/// refusing the whole row would break a readable detail and disclose that a
/// kickoff it never saw exists.
#[test]
fn task_detail_hides_a_kickoff_it_cannot_admit_without_refusing() {
    let world = build_world();
    // The kickoff belongs to another task and carries no cloud operation, so
    // it cannot be admitted: `task_json` redacts it instead of refusing.
    let db = rusqlite::Connection::open(world._state.path().join("cadence.sqlite3")).unwrap();
    assert_eq!(
        db.execute(
            "INSERT OR IGNORE INTO messages(id, alias, body, sender, source, state, created, \
             completed, issue, task_id) SELECT ?1, ?2, 'foreign kickoff', 'cloud-agent', \
             'cloud_agent', 'running', created, created, ?3, 'cad2215-other-task' FROM agents \
             WHERE alias=?2",
            rusqlite::params![FOREIGN_KICKOFF, CLOUD_B, OPEN_ISSUE],
        )
        .unwrap(),
        1
    );
    bind_dispatch(&world, OPEN_TASK, FOREIGN_KICKOFF);
    // A task whose kickoff reported no SHA: the diagnostic is kickoff-derived,
    // so a reader who was not served the kickoff must not be told about it.
    let db = rusqlite::Connection::open(world._state.path().join("cadence.sqlite3")).unwrap();
    db.execute(
        "UPDATE tasks SET state='review', head_sha=NULL WHERE id=?1",
        rusqlite::params![OPEN_TASK],
    )
    .unwrap();

    let detail = task_detail(&world, OPEN_TASK)
        .expect("a kickoff the reader cannot admit must not refuse a readable detail");
    let task = &detail["task"];
    assert_eq!(task["dispatch_message"], Value::Null, "{detail}");
    assert!(task.get("kickoff").is_none(), "{detail}");
    let attention = task
        .get("attention")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        !attention.contains("kickoff"),
        "a redacted kickoff still leaked its result: {detail}"
    );
}

/// The detail verbs serve an admitted kickoff's message id and state, which is
/// member-visible metadata: a grant withdrawn between selection and the
/// response must refuse the page rather than serve the earlier snapshot.
fn detail_refuses_revoked_kickoff(verb: &str) {
    let world = build_world();
    bind_dispatch(&world, OPEN_TASK, OP_A);

    let live = job_detail(&world, OPEN_JOB)
        .unwrap_or_else(|e| panic!("{verb} refused a detail with a live kickoff grant: {e}"));
    assert!(
        live.to_string().contains("\"kickoff\""),
        "{verb} did not serve the admitted kickoff: {live}"
    );

    // Revoke inside the hook: after the detail is assembled, before the reply.
    world.detail_mode.store(DETAIL_REVOKE_A, Ordering::SeqCst);
    let error = job_detail(&world, OPEN_JOB)
        .expect_err("a revoked kickoff grant must not serve the earlier snapshot");
    assert_eq!(error.kind(), "rejected", "{error:?}");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains(&format!("{} refused", verb.replace('_', " "))),
        "the refusal did not name the reproof: {rendered}"
    );
    for secret in [OP_A, CLOUD_BODY] {
        assert!(
            !rendered.contains(secret),
            "the refusal leaked {secret}: {rendered}"
        );
    }
}

#[test]
fn job_detail_refuses_a_kickoff_grant_revoked_after_selection() {
    detail_refuses_revoked_kickoff("job_show");
}
