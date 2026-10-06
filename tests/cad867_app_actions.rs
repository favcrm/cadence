//! CAD-867 app-actions/v2 positive and adversarial integration checks.
//! This stays independent from the live-view stale-pin guard in
//! `cad864_app_views.rs`: a real seam-armed daemon and board exercise the
//! closed CRM action slice over both peers.
#![cfg(feature = "test-seam")]

use cadence_agent::issue::Pm;
use cadence_agent::store::{app_records::RecordStore, Store};
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_ENV, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

struct Workspace {
    _root: tempfile::TempDir,
    pm: Pm,
    state: PathBuf,
    stop: Arc<AtomicBool>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
}

impl Workspace {
    fn new() -> Self {
        let root = tempfile::Builder::new().prefix("c867a").tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let source = root.path().join("source");
        for name in [
            "app.md",
            "workflows/blog-post.md",
            "rubrics/blog.md",
            "templates/brief.md",
            "templates/post.md",
        ] {
            let destination = source.join(name);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("apps/blog-post")
                    .join(name),
                destination,
            )
            .unwrap();
        }
        let manifest = source.join("app.md");
        let text = std::fs::read_to_string(&manifest)
            .unwrap()
            .replace("app: blog-post", "app: crm")
            .replace(
                "  connections: [publish]",
                "  connections: [publish]\n  views:\n    contract: app-views/v1\n  bindings:\n    contract: app-bindings/v1\n  actions:\n    contract: app-actions/v2",
            );
        std::fs::write(manifest, text).unwrap();
        for (contract, version, dir, name) in [
            ("app-views", "v1", "views", "app-views-v1.json"),
            ("app-bindings", "v1", "bindings", "app-bindings-v1.json"),
            ("app-actions", "v2", "actions", "app-actions-v2.json"),
        ] {
            let destination = source.join(dir).join(name);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("contracts")
                    .join(contract)
                    .join(version)
                    .join("examples/crm.json"),
                destination,
            )
            .unwrap();
        }

        let state = root.path().join("s");
        std::fs::create_dir_all(&state).unwrap();
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(Arc::clone(&stop)),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let dir = state.clone();
        let handle = std::thread::spawn(move || daemon::serve_with(&dir, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&state).is_none()
        {
            assert!(
                !handle.is_finished() && std::time::Instant::now() < deadline,
                "daemon never started"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Self {
            _root: root,
            pm,
            state,
            stop,
            daemon: Some(handle),
        }
    }

    fn source(&self) -> PathBuf {
        self._root.path().join("source")
    }

    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let state = self.state.clone();
        scoped(who, || client::rpc(&state, method, params))
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|error| panic!("operator {method}: {error}"))
    }

    fn install(&self) -> Value {
        self.op("app_workspace_install", json!({"source": self.source()}))
    }

    fn context(&self, install: &str) -> String {
        self.op(
            "app_context_create",
            json!({
                "install_id": install,
                "label": "CAD-867 action acceptance",
                "input_defaults": {},
                "request_id": "cad867-action-context"
            }),
        )["context"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn plant_agent(&self, alias: &str) {
        Store::open(&self.state.join("cadence.sqlite3"))
            .unwrap()
            .register_agent(&cadence_agent::store::NewAgent {
                alias,
                provider: "claude",
                endpoint_kind: "managed",
                role: "worker",
                cwd: "/tmp",
                sandbox: "read-only",
                instructions: None,
                params: Some("{\"upstream\":\"lead\"}"),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
    }

    fn stop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.daemon.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        self.stop();
    }
}

struct PortLease {
    port: u16,
    _lock: std::fs::File,
}

fn test_port() -> PortLease {
    use std::os::fd::AsRawFd;
    let dir = Path::new("/tmp/cadence-test-ports");
    std::fs::create_dir_all(dir).unwrap();
    let span = 90;
    let start = std::process::id() as usize * 31 % span;
    for i in 0..span {
        let port = 3110 + ((start + i) % span) as u16;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{port}.lock")))
            .unwrap();
        // SAFETY: this descriptor is owned by the port lease.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            continue;
        }
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return PortLease { port, _lock: lock };
        }
    }
    panic!("no free test board port in 3110-3199");
}

struct Board {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
}

impl Board {
    fn start(state: &Path, pm: &Path, port: u16) -> Self {
        let (startup, ready) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(Arc::clone(&stop)),
            startup: Some(startup),
            test_seam: true,
            ..Default::default()
        };
        let (state, pm) = (state.to_path_buf(), pm.to_path_buf());
        let thread = std::thread::spawn(move || cadence_agent::ui::serve(&state, &pm, &opts));
        ready
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
            .unwrap();
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Board {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn raw(port: u16, request: &str) -> (u16, String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.write_all(request.as_bytes()).unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    (status, head.to_string(), body.to_string())
}

fn sign_in(state: &Path, port: u16) -> (String, String) {
    cadence_agent::operator_auth::ensure_secret(state).unwrap();
    let secret = cadence_agent::operator_auth::read_secret(state).unwrap();
    let mint = scoped(Asserted::Operator, || {
        client::rpc(
            state,
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )
    })
    .unwrap();
    let nonce = mint["nonce"].as_str().unwrap();
    let host = format!("cadence-{port}.localhost:{port}");
    let token = Seam::token_at(state).unwrap();
    let body = json!({"nonce": nonce}).to_string();
    let request = format!(
        "POST /api/session HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         X-Cadence-Board: 1\r\nOrigin: http://{host}\r\nSec-Fetch-Site: same-origin\r\n\
         {AS_HEADER}: operator\r\n{TOKEN_HEADER}: {token}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let (status, head, body) = raw(port, &request);
    assert_eq!(status, 200, "{head}\n{body}");
    let cookie = head
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("set-cookie:"))
        .and_then(|line| {
            line.split_once(':')
                .map(|(_, value)| value.trim().to_string())
        })
        .expect("operator login returned no cookie")
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let key = serde_json::from_str::<Value>(&body).unwrap()["session_key"]
        .as_str()
        .unwrap()
        .to_string();
    (cookie, key)
}

fn post_as(
    state: &Path,
    port: u16,
    who: Option<&str>,
    session: Option<(&str, &str)>,
    path: &str,
    body: &Value,
) -> (u16, String, String) {
    let host = format!("cadence-{port}.localhost:{port}");
    let seam = who
        .map(|who| {
            format!(
                "{AS_HEADER}: {who}\r\n{TOKEN_HEADER}: {}\r\n",
                Seam::token_at(state).unwrap()
            )
        })
        .unwrap_or_default();
    let session_headers = session
        .map(|(cookie, key)| format!("Cookie: {cookie}\r\nX-Cadence-Session: {key}\r\n"))
        .unwrap_or_default();
    let text = body.to_string();
    let request = format!(
        "POST {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         X-Cadence-Board: 1\r\nOrigin: http://{host}\r\nSec-Fetch-Site: same-origin\r\n\
         {seam}{session_headers}Content-Length: {}\r\n\r\n{text}",
        text.len()
    );
    raw(port, &request)
}

fn create_input(name: &str) -> Value {
    let email = format!(
        "{}@example.test",
        name.to_ascii_lowercase().replace(' ', "-")
    );
    json!({
        "display_name": name,
        "email": email,
        "phone": "+1 555 0100",
        "source": "acceptance",
        "tags": ["vip"]
    })
}

fn rpc_create_params(installed: &Value, context: &str, input: Value) -> Value {
    json!({
        "install_id": installed["install_id"],
        "context_id": context,
        "view_id": "customer-create-form",
        "action_id": "customer.create",
        "digest": installed["digest"],
        "view_descriptor_digest": installed["view_descriptor_digest"],
        "view_binding_digest": installed["view_binding_digest"],
        "input": input
    })
}

fn rpc_update_params(
    installed: &Value,
    context: &str,
    id: &str,
    revision: i64,
    input: Value,
) -> Value {
    json!({
        "install_id": installed["install_id"],
        "context_id": context,
        "view_id": "customer-edit-form",
        "action_id": "customer.update",
        "digest": installed["digest"],
        "view_descriptor_digest": installed["view_descriptor_digest"],
        "view_binding_digest": installed["view_binding_digest"],
        "record_id": id,
        "expected_revision": revision,
        "input": input
    })
}

fn http_body(installed: &Value, input: Value, revision: Option<i64>) -> Value {
    let mut body = json!({
        "digest": installed["digest"],
        "descriptor": installed["view_descriptor_digest"],
        "binding": installed["view_binding_digest"],
        "input": input
    });
    if let Some(revision) = revision {
        body["expected_revision"] = json!(revision);
    }
    body
}

fn records(w: &Workspace, install: &str, context: &str) -> Vec<Value> {
    RecordStore::open(&w.state, install)
        .unwrap()
        .app_record_list(context)
        .unwrap()["records"]
        .as_array()
        .unwrap()
        .clone()
}

/// Spawn the same test binary under `setsid` with an explicit unproven
/// process identity; the daemon must not treat a detached child of this
/// test as its operator.
#[test]
fn cad867_detached_child_probe() {
    let (Ok(state), Ok(raw)) = (
        std::env::var("CAD867_ACTION_PROBE_STATE"),
        std::env::var("CAD867_ACTION_PROBE_PARAMS"),
    ) else {
        return;
    };
    let params: Value = serde_json::from_str(&raw).unwrap();
    let result = client::rpc(&PathBuf::from(state), "app_view_action", params);
    assert!(
        result.is_err(),
        "setsid child unexpectedly reached the operator-only action"
    );
}

#[test]
fn cad867_customer_actions_are_operator_only_pinned_and_cas_bound_on_both_peers() {
    const AGENT: &str = "cad867-action-agent";
    let w = Workspace::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install);
    w.plant_agent(AGENT);
    let create_path = format!(
        "/api/app-installations/{install}/contexts/{context}/views/customer-create-form/actions/customer.create"
    );
    let rpc_params = rpc_create_params(&installed, &context, create_input("RPC customer"));

    // Caller identity is checked before descriptor/payload trust: a
    // registered agent, an unattributed process, a detached child and a
    // forged actor field all refuse without creating a record.
    assert!(w
        .rpc(
            Asserted::Agent(AGENT.into()),
            "app_view_action",
            rpc_params.clone()
        )
        .is_err());
    assert!(w
        .rpc(Asserted::Unproven, "app_view_action", rpc_params.clone())
        .is_err());
    let mut command = std::process::Command::new("setsid");
    command
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "cad867_detached_child_probe", "--nocapture"])
        .env(AS_ENV, "unproven")
        .env("CAD867_ACTION_PROBE_STATE", &w.state)
        .env("CAD867_ACTION_PROBE_PARAMS", rpc_params.to_string());
    let child = cadence_agent::reaper::output(&mut command)
        .expect("setsid is required for the detached-child proof");
    assert!(
        child.status.success(),
        "setsid child proof failed: {}",
        String::from_utf8_lossy(&child.stderr)
    );
    let mut forged_actor = rpc_params.clone();
    forged_actor["actor"] = json!("operator");
    assert!(w
        .rpc(Asserted::Operator, "app_view_action", forged_actor)
        .is_err());
    assert!(
        records(&w, install, &context).is_empty(),
        "a refused caller/forgery created a record"
    );

    // Operator-positive daemon route and native unknown-consent policy.
    let created = w
        .rpc(Asserted::Operator, "app_view_action", rpc_params)
        .unwrap_or_else(|error| panic!("operator create failed: {error}"));
    let rpc_id = created["record"]["id"].as_str().unwrap().to_string();
    assert!(
        rpc_id.starts_with("cust-") && rpc_id.len() == 37,
        "record identity is host minted: {rpc_id}"
    );
    assert_eq!(
        created["record"]["profile"]["consent"]["email"],
        json!("unknown")
    );
    assert!(created["record"]["profile"]["consent"]["sms"].is_null());
    assert_eq!(created["record"]["revision"], json!(1));

    // Real HTTP positive path for create and update; the body carries
    // only receipt pins, typed input and update-only CAS revision.
    let lease = test_port();
    let port = lease.port;
    let board = Board::start(&w.state, &w.pm.dir, port);
    let (cookie, session_key) = sign_in(&w.state, port);
    let session = Some((cookie.as_str(), session_key.as_str()));
    let (status, _, body) = post_as(
        &w.state,
        port,
        Some("operator"),
        session,
        &create_path,
        &http_body(&installed, create_input("HTTP customer"), None),
    );
    assert_eq!(status, 200, "operator HTTP create: {body}");
    let http_created: Value = serde_json::from_str(&body).unwrap();
    let http_id = http_created["record"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        http_created["record"]["profile"]["consent"]["email"],
        json!("unknown")
    );

    let count_before_denials = records(&w, install, &context).len();
    let (status, _, body) = post_as(
        &w.state,
        port,
        Some(&format!("agent:{AGENT}")),
        session,
        &create_path,
        &http_body(&installed, create_input("Stolen session"), None),
    );
    assert!(
        status >= 400,
        "agent peer replayed an operator session: {status} {body}"
    );
    assert_eq!(
        records(&w, install, &context).len(),
        count_before_denials,
        "agent HTTP peer with a stolen session did not create a record"
    );
    // A stolen-session replay can invalidate the held session; perform a
    // fresh operator login before testing the body schema independently.
    let (cookie2, session_key2) = sign_in(&w.state, port);
    let session2 = Some((cookie2.as_str(), session_key2.as_str()));
    let mut forged_http = http_body(&installed, create_input("Forged actor"), None);
    forged_http["actor"] = json!("operator");
    let (status, _, body) = post_as(
        &w.state,
        port,
        Some("operator"),
        session2,
        &create_path,
        &forged_http,
    );
    assert_eq!(status, 400, "HTTP body rejects forged actor field: {body}");
    assert_eq!(
        records(&w, install, &context).len(),
        count_before_denials,
        "HTTP schema refusal did not create a record"
    );

    let update_path = format!(
        "/api/app-installations/{install}/contexts/{context}/views/customer-edit-form/actions/customer.update/records/{http_id}"
    );
    let update_body = http_body(&installed, json!({"display_name":"HTTP updated"}), Some(1));
    let (status, _, body) = post_as(
        &w.state,
        port,
        Some("operator"),
        session2,
        &update_path,
        &update_body,
    );
    assert_eq!(status, 200, "operator HTTP update: {body}");
    let http_updated: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(http_updated["record"]["revision"], json!(2));
    assert_eq!(
        http_updated["record"]["profile"]["display_name"],
        json!("HTTP updated")
    );
    assert_eq!(
        http_updated["record"]["profile"]["email"],
        json!("http-customer@example.test"),
        "an absent optional field preserves the existing profile value"
    );
    assert_eq!(
        http_updated["record"]["profile"]["consent"]["email"],
        json!("unknown"),
        "update preserves native consent state"
    );
    drop(board);

    // Refuse forged consent, stale descriptor pins and an incorrect form
    // route without changing the current RPC-created record.
    let before = RecordStore::open(&w.state, install)
        .unwrap()
        .app_record_show(&context, &rpc_id)
        .unwrap();
    let mut forged_consent = rpc_update_params(
        &installed,
        &context,
        &rpc_id,
        1,
        json!({"display_name":"Bad"}),
    );
    forged_consent["input"]["consent"] = json!({"email":"granted"});
    assert!(w
        .rpc(Asserted::Operator, "app_view_action", forged_consent)
        .is_err());
    let mut stale = rpc_update_params(
        &installed,
        &context,
        &rpc_id,
        1,
        json!({"display_name":"Stale"}),
    );
    stale["view_descriptor_digest"] = json!(format!("sha256:{}", "f".repeat(64)));
    assert!(w.rpc(Asserted::Operator, "app_view_action", stale).is_err());
    let mut wrong_form = rpc_update_params(
        &installed,
        &context,
        &rpc_id,
        1,
        json!({"display_name":"Wrong form"}),
    );
    wrong_form["view_id"] = json!("customer-detail");
    assert!(w
        .rpc(Asserted::Operator, "app_view_action", wrong_form)
        .is_err());
    assert_eq!(
        RecordStore::open(&w.state, install)
            .unwrap()
            .app_record_show(&context, &rpc_id)
            .unwrap(),
        before,
        "forged consent, stale pin and wrong form leave the customer unchanged"
    );

    // One expected revision can be consumed once under concurrent direct
    // RPCs; the record store's CAS rejects the loser and preserves consent.
    let concurrent_revision = before["record"]["revision"].as_i64().unwrap();
    let state_a = w.state.clone();
    let state_b = w.state.clone();
    let params_a = rpc_update_params(
        &installed,
        &context,
        &rpc_id,
        concurrent_revision,
        json!({"display_name":"CAS A"}),
    );
    let params_b = rpc_update_params(
        &installed,
        &context,
        &rpc_id,
        concurrent_revision,
        json!({"display_name":"CAS B"}),
    );
    let a = std::thread::spawn(move || {
        scoped(Asserted::Operator, || {
            client::rpc(&state_a, "app_view_action", params_a)
        })
    });
    let b = std::thread::spawn(move || {
        scoped(Asserted::Operator, || {
            client::rpc(&state_b, "app_view_action", params_b)
        })
    });
    let results = [a.join().unwrap(), b.join().unwrap()];
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        1,
        "exactly one concurrent update consumes a revision"
    );
    assert_eq!(
        results.iter().filter(|result| result.is_err()).count(),
        1,
        "the concurrent stale revision refuses"
    );
    let after = RecordStore::open(&w.state, install)
        .unwrap()
        .app_record_show(&context, &rpc_id)
        .unwrap();
    assert_eq!(after["record"]["revision"], json!(concurrent_revision + 1));
    assert!(matches!(
        after["record"]["profile"]["display_name"].as_str(),
        Some("CAS A" | "CAS B")
    ));
    assert_eq!(
        after["record"]["profile"]["email"],
        before["record"]["profile"]["email"]
    );
    assert_eq!(
        after["record"]["profile"]["consent"],
        before["record"]["profile"]["consent"]
    );
    assert_eq!(
        records(&w, install, &context).len(),
        2,
        "only the daemon create and board create produced records"
    );
}
