use super::*;
use crate::store::NewAgent;

fn cad627_shared(dir: &Path) -> Arc<Shared> {
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", dir.join("no-pm").to_str().unwrap());
    let shared = Shared::new(dir, &opts).unwrap();
    shared
        .store
        .register_agent(&NewAgent {
            alias: "w1",
            provider: "fake",
            endpoint_kind: "managed",
            role: "worker",
            cwd: dir.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared
}

/// CAD-407: `serve` — so `daemon start`, `run` and `restart` — refuses
/// while an interrupted restore's aside files exist, before the
/// shutdown marker is consumed or a store is created or opened. The
/// error names the leftover and the `mv` that puts it back.
fn shared() -> (tempfile::TempDir, Arc<Shared>) {
    let dir = tempfile::tempdir().unwrap();
    let shared = Shared::new(dir.path(), &ServeOptions::default()).unwrap();
    (dir, shared)
}

/// A live lease held by `name` — the update's own claim, taken the
/// way a fixture asserts the operator (CAD-482).
fn hold_lease(state: &Path, name: &str) {
    let seam = state.join("seam");
    std::fs::create_dir_all(&seam).unwrap();
    std::fs::write(seam.join("token"), "test-token").unwrap();
    let target = "b".repeat(40);
    let caller = crate::rollout::Caller {
        identity: name.to_string(),
        source: "as",
    };
    crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::rollout::claim(
            state,
            &crate::rollout::ClaimRequest {
                caller: &caller,
                reason: "cadence update",
                target: Some(&target),
                ttl: Duration::from_secs(3600),
                takeover: false,
                now: crate::rollout::unix_now(),
            },
        )
        .unwrap();
    });
}

/// CAD-561: the RPC that gates the fleet is the operator's, proved
/// by the handler itself — a table-level `Handler` rule never admits
/// an agent, and the handler runs `operator_connection`. Pinned at
/// the source, the same way the CAD-339 table parse pins methods.
fn register(shared: &Shared, dir: &Path, alias: &str) {
    shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: dir.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
}

/// Deterministic boundary for the stop/resume gap: while a stop
/// reservation is held — the state an alias is in between the old
/// actor's map removal and the stop's final write — a launch is
/// rejected; releasing the reservation unblocks it.
fn stop_cloud(shared: &Shared, stop: &AtomicBool, thread: thread::JoinHandle<()>) {
    shared.store.set_enabled("cloud-1", false).unwrap();
    shared.begin_closing();
    let handle = {
        let lc = shared.lifecycle.lock().unwrap();
        lc.agents
            .get("cloud-1")
            .and_then(|ctl| ctl.thread.lock().unwrap().take())
    };
    if let Some(ctl) = shared.lifecycle.lock().unwrap().agents.get("cloud-1") {
        ctl.wake.notify_all();
    }
    if let Some(handle) = handle {
        let _ = handle.join();
    }
    stop.store(true, Ordering::SeqCst);
    let _ = thread.join();
}

/// Calls a held-turn mock records. `healthy` ends the lost polls.
#[derive(Default)]
struct HoldCalls {
    posts: std::sync::atomic::AtomicUsize,
    archives: std::sync::atomic::AtomicUsize,
    deletes: std::sync::atomic::AtomicUsize,
    healthy: AtomicBool,
    stop: AtomicBool,
}

/// A Devin mock whose session GET fails with 503 after the first
/// message post, so that turn is held until `healthy` is set.
fn cloud_hold_server() -> (String, Arc<HoldCalls>, thread::JoinHandle<()>) {
    let calls = Arc::new(HoldCalls::default());
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = server.server_addr().to_ip().unwrap();
    let seen = Arc::clone(&calls);
    let thread = thread::spawn(move || {
        while !seen.stop.load(Ordering::SeqCst) {
            let mut req = match server.recv_timeout(Duration::from_millis(50)) {
                Ok(Some(req)) => req,
                _ => continue,
            };
            let path = req.url().to_string();
            let method = req.method().as_str().to_string();
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let posts = seen.posts.load(Ordering::SeqCst);
            let session = |detail: &str| {
                json!({
                    "session_id": "devin-created",
                    "status": "running",
                    "status_detail": detail,
                    "url": "https://app.devin.ai/sessions/devin-created",
                    "pull_requests": [],
                })
                .to_string()
            };
            let (status, payload): (u16, String) = if path.contains("/repositories") {
                (
                    200,
                    json!({"repositories": [{"name": "cadence", "owner": "favcrm"}]}).to_string(),
                )
            } else if method == "POST" && path.ends_with("/sessions") {
                (200, session("working"))
            } else if method == "POST" && path.ends_with("/archive") {
                seen.archives.fetch_add(1, Ordering::SeqCst);
                (200, json!({"ok": true}).to_string())
            } else if method == "DELETE" {
                seen.deletes.fetch_add(1, Ordering::SeqCst);
                (200, json!({"ok": true}).to_string())
            } else if method == "POST" && path.contains("/messages") {
                seen.posts.fetch_add(1, Ordering::SeqCst);
                (200, json!({"ok": true}).to_string())
            } else if method == "GET" && path.contains("/messages") {
                let items: Vec<Value> = (2..=posts)
                    .map(|n| {
                        json!({"event_id": format!("evt-{n}"), "source": "devin",
                                   "message": format!("reply {n}"), "created_at": n})
                    })
                    .collect();
                let total = items.len();
                (
                    200,
                    json!({"items": items, "has_next_page": false,
                               "end_cursor": null, "total": total})
                    .to_string(),
                )
            } else if method == "GET" && path.contains("/sessions/") {
                if posts == 0 {
                    (200, session("working"))
                } else if seen.healthy.load(Ordering::SeqCst) {
                    (200, session("finished"))
                } else {
                    (503, json!({"error": "unavailable"}).to_string())
                }
            } else {
                (200, json!({"ok": true}).to_string())
            };
            let _ = req.respond(tiny_http::Response::from_string(payload).with_status_code(status));
        }
    });
    (format!("http://{addr}"), calls, thread)
}

fn devin_env(shared: &Shared, base: &str) {
    shared.provider_env.set("CADENCE_DEVIN_API_BASE", base);
    shared
        .provider_env
        .set("CADENCE_DEVIN_API_KEY", "cog_test_secret_value");
    shared.provider_env.set("CADENCE_DEVIN_ORG_ID", "org-test");
    shared
        .provider_env
        .set("CADENCE_DEVIN_POLL_INTERVAL_MS", "20");
    shared
        .provider_env
        .set("CADENCE_DEVIN_POLL_BUDGET_MS", "150");
}

/// Register `cloud-1`, send it one message and wait until the actor
/// holds that turn.
fn hold_cloud_turn(shared: &Arc<Shared>, dir: &Path, base: &str, id: &str) {
    devin_env(shared, base);
    shared
        .store
        .register_agent(&NewAgent {
            alias: "cloud-1",
            provider: "devin",
            endpoint_kind: "cloud",
            role: "worker",
            cwd: dir.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: Some(r#"{"repos":["favcrm/cadence"]}"#),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared
        .store
        .enqueue("cloud-1", "do the task", None, id, "user")
        .unwrap();
    shared.launch_actor("cloud-1").unwrap();
    let started = Instant::now();
    loop {
        let held = shared
            .lifecycle
            .lock()
            .unwrap()
            .agents
            .get("cloud-1")
            .is_some_and(|ctl| ctl.cloud_held.load(Ordering::SeqCst));
        if held && shared.store.message(id).unwrap().unwrap().state == "unknown" {
            return;
        }
        if started.elapsed() > Duration::from_secs(8) {
            panic!(
                "turn was not held: {:?} {:?}",
                shared.store.message(id).ok(),
                shared.store.events("cloud-1", 0, 40).ok()
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn join_actor(shared: &Shared, alias: &str) {
    let handle = {
        let lc = shared.lifecycle.lock().unwrap();
        lc.agents
            .get(alias)
            .and_then(|ctl| ctl.thread.lock().unwrap().take())
    };
    if let Some(ctl) = shared.lifecycle.lock().unwrap().agents.get(alias) {
        ctl.wake.notify_all();
    }
    if let Some(handle) = handle {
        let _ = handle.join();
    }
}

// ---------- CAD-160: task-bound messages restate the task ----------

/// A pm, a pty worker in its group, and job `j1` whose default task
/// `j1-t1` carries a title, a spec and a CAD-300 acceptance listing.
fn task_bound_fixture(acceptance: &str) -> (tempfile::TempDir, Arc<Shared>) {
    let (dir, shared) = shared();
    register(&shared, dir.path(), "pm");
    shared
        .store
        .register_agent(&NewAgent {
            alias: "w1",
            provider: "devin",
            endpoint_kind: "pty",
            role: "worker",
            cwd: dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: Some(&json!({"upstream": "pm"}).to_string()),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared
        .store
        .create_job(
            "j1",
            None,
            "/specs/j1.md",
            &"0".repeat(64),
            "pm",
            None,
            None,
            None,
            2,
            None,
            Some("Wire the email provider"),
            None,
            None,
            None,
            Some("w1"),
            Some(acceptance),
        )
        .unwrap();
    (dir, shared)
}

fn stored_body(shared: &Shared, id: &str) -> String {
    shared.store.message(id).unwrap().unwrap().body
}

// ---------- CAD-132: provider WAL watch ----------

/// A WAL-mode db whose writer connection stays open — closing the
/// last connection auto-checkpoints, which would erase the fixture.
fn wal_db(dir: &Path, name: &str) -> (PathBuf, rusqlite::Connection) {
    let db = dir.join(name);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute_batch(
        "CREATE TABLE t(x BLOB);
             INSERT INTO t VALUES (randomblob(262144));",
    )
    .unwrap();
    (db, conn)
}

fn wal_size(db: &Path) -> u64 {
    crate::doctor::host::wal_sibling(db)
        .and_then(|w| std::fs::metadata(w).ok())
        .map(|m| m.len())
        .unwrap_or(0)
}

/// `quiet_secs = 0` makes a just-written WAL "quiet" — tests then
/// exercise the checkpoint itself; the live daemon passes
/// `WAL_QUIET_SECS`.
fn pass(
    roots: &[crate::doctor::host::WalRoot],
    busy: &HashSet<String>,
    max_bytes: u64,
    shared: &Shared,
    watch: &mut WalWatch,
) {
    wal_pass(roots, busy, max_bytes, 0, false, &shared.store, watch);
}

/// A real process tree for the CAD-385 tests: `sh` (the process a
/// pane row records) and its `sleep` child (the caller, whose
/// ancestry reaches `sh`). One process group, killed on drop.
struct PaneTree {
    sh: std::process::Child,
    caller: u32,
}

impl PaneTree {
    fn spawn() -> Self {
        use std::os::unix::process::CommandExt;
        // `; :` keeps sh from exec-ing into sleep: sh stays the parent.
        let sh = std::process::Command::new("sh")
            .args(["-c", "sleep 30; :"])
            .process_group(0)
            .spawn()
            .unwrap();
        let children = format!("/proc/{0}/task/{0}/children", sh.id());
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let caller = loop {
            let found = std::fs::read_to_string(&children)
                .ok()
                .and_then(|t| t.split_whitespace().next()?.parse().ok());
            if let Some(pid) = found {
                break pid;
            }
            assert!(std::time::Instant::now() < deadline, "sleep never started");
            std::thread::sleep(Duration::from_millis(10));
        };
        Self { sh, caller }
    }

    fn pane_pid(&self) -> u32 {
        self.sh.id()
    }
}

impl Drop for PaneTree {
    fn drop(&mut self) {
        // Our own group: sh and the sleep it started, nothing else.
        unsafe { libc::kill(-(self.sh.id() as libc::pid_t), libc::SIGKILL) };
        let _ = self.sh.wait();
    }
}

/// Record `alias` as a live pty pane at `pid` with `pid_start` as
/// its recorded process start time (`None`: a row from before v14).
fn record_pane(shared: &Shared, dir: &Path, alias: &str, pid: u32, pid_start: Option<u64>) {
    register(shared, dir, alias);
    let conn = rusqlite::Connection::open(dir.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET endpoint_kind='pty', generation='g1', pid=?1, pid_start=?2 \
             WHERE alias=?3",
        rusqlite::params![pid, pid_start.map(|s| s as i64), alias],
    )
    .unwrap();
}

/// Every pid → alias mapping the daemon's Unix socket uses, for one
/// caller, rendered comparably.
fn identity_answers(shared: &Shared, peer: u32) -> Vec<String> {
    vec![
        format!(
            "slot_identity: {:?}",
            shared
                .slot_identity(peer)
                .map(|w| w.map(|w| w.lane().to_string()))
        ),
        format!(
            "caller_identity: {:?}",
            shared.caller_identity(peer).map(|c| match c {
                Caller::NoAgentIdentity => "none".to_string(),
                Caller::Agent(v) => v.agent.alias.clone(),
            })
        ),
        format!("operator_evidence: {:?}", shared.operator_evidence(peer)),
        format!(
            "derived_caller: {:?}",
            shared.derived_caller("elsewhere", peer, "answer")
        ),
    ]
}

// ---- CAD-162: turn-token staleness keyed on the endpoint's scheme ----

const GEN: &str = "0123456789abcdef0123456789abcdef";
const OLD_GEN: &str = "fedcba9876543210fedcba9876543210";

/// An agent of `(provider, kind)` whose live endpoint is at
/// `generation`, holding one `running` message minted `token`.
/// Returns the message id. No actor runs — the RPC is judged
/// against the store alone, exactly as the daemon does.
fn running_turn(
    shared: &Arc<Shared>,
    dir: &Path,
    alias: &str,
    (provider, kind): (&str, &str),
    generation: Option<&str>,
    token: &str,
) -> String {
    shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider,
            endpoint_kind: kind,
            role: "worker",
            cwd: dir.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    shared
        .store
        .set_identity(
            alias,
            &adapter::Identity {
                thread_id: format!("thread-{alias}"),
                session_id: format!("session-{alias}"),
                model: None,
                effort: None,
                pid: 0,
                endpoint: None,
                generation: generation.map(str::to_string),
                attach: None,
            },
        )
        .unwrap();
    let id = format!("m-{alias}");
    shared
        .store
        .enqueue(alias, "work", None, &id, "test")
        .unwrap();
    let Take::Message(msg) = shared.store.take_queued(alias).unwrap() else {
        panic!("{alias}: queued message must be taken");
    };
    shared.store.mark_running(&msg.id, token).unwrap();
    id
}

fn report(shared: &Arc<Shared>, id: &str, token: &str, kind: &str) -> Result<Value> {
    shared.dispatch(
        "message_report",
        &json!({"message": id, "token": token, "kind": kind, "text": "reported"}),
        0,
    )
}

/// A refused report changed nothing: the message is still `running`
/// under the same token and carries no ack or result.
fn assert_refused_untouched(shared: &Arc<Shared>, id: &str, token: &str, what: &str) {
    for kind in ["ack", "result"] {
        let err = report(shared, id, token, kind)
            .expect_err(&format!("{what}: {kind} accepted as current"))
            .to_string();
        assert!(err.contains("stale endpoint generation"), "{what}: {err}");
    }
    let m = shared.store.message(id).unwrap().unwrap();
    assert_eq!(m.state, "running", "{what}");
    assert_eq!(m.turn_id.as_deref(), Some(token), "{what}");
    assert!(m.result.is_none(), "{what}: {:?}", m.result);
}

/// Endpoints whose turn tokens embed their generation, with each
/// one's own token shape and another kind's shape.
fn schemed_endpoints() -> Vec<((&'static str, &'static str), &'static str, &'static str)> {
    vec![
        (("claude", "pty"), "pty", "claude"),
        (("devin", "pty"), "pty", "claude"),
        (("cursor", "pty"), "pty", "claude"),
        (("tui-stub", "pty"), "pty", "claude"),
        (("claude", "managed"), "claude", "pty"),
    ]
}

mod board;
mod caller;
mod cloud_hold;
mod continuity;
mod lifecycle;
mod misc;
mod redaction;
mod reporting;
mod tasks;
mod wal;
