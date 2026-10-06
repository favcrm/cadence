//! Independent CAD-867 action-guard acceptance: real installed CRM daemon
//! RPC and authenticated loopback-board controls, followed by scoped refusal
//! cases and a mixed-peer CAS race. Identity is asserted by `test-seam`; the
//! detached children are real OS processes, not native operator-auth proof.
#![cfg(all(feature = "test-seam", unix))]

use cadence_agent::issue::Pm;
use cadence_agent::store::{app_records::RecordStore, Store};
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_ENV, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Barrier};
use std::time::Duration;

const AGENT: &str = "cad867-action-guard-agent";
const LEAK_ATTEMPT: &str = "CAD867_ACTION_LEAK_ATTEMPT";
const LEAK_EMAIL: &str = "cad867_action_leak_attempt@example.test";
const CUSTOMER_CANARY: &str = "CAD867_ACTION_CUSTOMER_CANARY";
const CUSTOMER_EMAIL: &str = "cad867-action-customer@example.invalid";
const FOREIGN_CANARY: &str = "CAD867_ACTION_FOREIGN_CANARY";
const FOREIGN_EMAIL: &str = "cad867-action-foreign@example.invalid";
const RACE_RPC: &str = "CAD867_ACTION_RACE_RPC";
const RACE_HTTP: &str = "CAD867_ACTION_RACE_HTTP";
const REDACTION_MARKERS: &[&str] = &[
    LEAK_ATTEMPT,
    LEAK_EMAIL,
    CUSTOMER_CANARY,
    CUSTOMER_EMAIL,
    FOREIGN_CANARY,
    FOREIGN_EMAIL,
    RACE_RPC,
    RACE_HTTP,
];

struct Fixture {
    _root: tempfile::TempDir,
    pm: Pm,
    state: PathBuf,
    source_crm: PathBuf,
    source_preview: PathBuf,
    stop: Arc<AtomicBool>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
}

struct CustomerSeed<'a> {
    install: &'a str,
    context: &'a str,
    record_id: &'a str,
    name: &'a str,
    email: &'a str,
    email_consent: &'a str,
    sms_consent: Option<&'a str>,
    provenance: Option<Value>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("cad867-action-guard")
            .tempdir()
            .unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let source_crm = root.path().join("source-crm");
        let source_preview = root.path().join("source-preview");
        write_package(&source_crm, "crm", true);
        // A second real install has the same v1 form preview and customer
        // binding, but deliberately no app-actions/v2 declaration or file.
        write_package(&source_preview, "blog-post", false);

        let state = root.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let options = daemon::ServeOptions {
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
        let daemon_state = state.clone();
        let daemon = std::thread::spawn(move || daemon::serve_with(&daemon_state, options));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&state).is_none()
        {
            assert!(
                !daemon.is_finished() && std::time::Instant::now() < deadline,
                "fixture daemon did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        Self {
            _root: root,
            pm,
            state,
            source_crm,
            source_preview,
            stop,
            daemon: Some(daemon),
        }
    }

    fn rpc(&self, caller: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let state = self.state.clone();
        scoped(caller, || client::rpc(&state, method, params))
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|error| panic!("operator {method} failed: {error}"))
    }

    fn install(&self, source: &Path) -> Value {
        self.op(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        )
    }

    fn show(&self, install: &str) -> Value {
        self.op("app_workspace_show", json!({"install_id": install}))
    }

    fn context(&self, install: &str, request_id: &str) -> String {
        self.op(
            "app_context_create",
            json!({
                "install_id": install,
                "label": "CAD-867 independent action acceptance",
                "input_defaults": {},
                "request_id": request_id
            }),
        )["context"]["id"]
            .as_str()
            .expect("context create returned an id")
            .to_string()
    }

    fn create_customer(&self, seed: CustomerSeed<'_>) -> Value {
        let mut consent = json!({"email": seed.email_consent});
        if let Some(sms) = seed.sms_consent {
            consent["sms"] = json!(sms);
        }
        let mut params = json!({
            "install_id": seed.install,
            "context_id": seed.context,
            "record_id": seed.record_id,
            "profile": {
                "schema": 1,
                "display_name": seed.name,
                "email": seed.email,
                "phone": "+1 555 0134",
                "tags": ["acceptance", "customer"],
                "source": "acceptance",
                "consent": consent
            }
        });
        if let Some(provenance) = seed.provenance {
            params["consent_provenance"] = provenance;
        }
        self.op("app_record_create", params)
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
        if let Some(daemon) = self.daemon.take() {
            let _ = daemon.join();
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn write_package(root: &Path, app: &str, with_actions: bool) {
    for relative in [
        "app.md",
        "workflows/blog-post.md",
        "rubrics/blog.md",
        "templates/brief.md",
        "templates/post.md",
    ] {
        let destination = root.join(relative);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("apps/blog-post")
                .join(relative),
            destination,
        )
        .unwrap();
    }

    let manifest_path = root.join("app.md");
    let manifest = std::fs::read_to_string(&manifest_path)
        .unwrap()
        .replace("app: blog-post", &format!("app: {app}"));
    let needs = if with_actions {
        "  views:\n    contract: app-views/v1\n  bindings:\n    contract: app-bindings/v1\n  actions:\n    contract: app-actions/v2"
    } else {
        "  views:\n    contract: app-views/v1\n  bindings:\n    contract: app-bindings/v1"
    };
    let manifest = manifest.replace(
        "  connections: [publish]",
        &format!("  connections: [publish]\n{needs}"),
    );
    std::fs::write(manifest_path, manifest).unwrap();

    for (directory, contract, version, file) in [
        ("views", "app-views", "v1", "app-views-v1.json"),
        ("bindings", "app-bindings", "v1", "app-bindings-v1.json"),
        ("actions", "app-actions", "v2", "app-actions-v2.json"),
    ] {
        if directory == "actions" && !with_actions {
            continue;
        }
        let destination = root.join(directory).join(file);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let content = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("contracts")
                .join(contract)
                .join(version)
                .join("examples/crm.json"),
        )
        .unwrap()
        .replace("\"app\": \"crm\"", &format!("\"app\": \"{app}\""));
        std::fs::write(destination, content).unwrap();
    }
}

struct PortLease {
    port: u16,
    _lock: std::fs::File,
}

fn lease_port() -> PortLease {
    use std::os::fd::AsRawFd;
    let lock_dir = Path::new("/tmp/cadence-test-ports");
    std::fs::create_dir_all(lock_dir).unwrap();
    let span = 90usize;
    let start = std::process::id() as usize * 31 % span;
    for offset in 0..span {
        let port = 3110 + ((start + offset) % span) as u16;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_dir.join(format!("{port}.lock")))
            .unwrap();
        // SAFETY: `lock` owns this descriptor and remains held by the lease.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            continue;
        }
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return PortLease { port, _lock: lock };
        }
    }
    panic!("no free loopback board port in 3110-3199");
}

struct Board {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
}

impl Board {
    fn start(state: &Path, pm: &Path, port: u16) -> Self {
        let (startup, ready) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let options = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(Arc::clone(&stop)),
            startup: Some(startup),
            test_seam: true,
            ..Default::default()
        };
        let (state, pm) = (state.to_path_buf(), pm.to_path_buf());
        let thread = std::thread::spawn(move || cadence_agent::ui::serve(&state, &pm, &options));
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

fn raw_http(port: u16, request: &str) -> (u16, String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.write_all(request.as_bytes()).unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    let response = String::from_utf8_lossy(&bytes).to_string();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let (headers, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    (status, headers.to_string(), body.to_string())
}

#[derive(Clone)]
struct OperatorSession {
    cookie: String,
    key: String,
}

fn sign_in(state: &Path, port: u16) -> OperatorSession {
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
    let host = format!("cadence-{port}.localhost:{port}");
    let token = Seam::token_at(state).unwrap();
    let body = json!({"nonce": mint["nonce"]}).to_string();
    let request = format!(
        "POST /api/session HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         X-Cadence-Board: 1\r\nOrigin: http://{host}\r\nSec-Fetch-Site: same-origin\r\n\
         {AS_HEADER}: operator\r\n{TOKEN_HEADER}: {token}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let (status, headers, response) = raw_http(port, &request);
    assert_eq!(status, 200, "operator sign-in: {headers}\n{response}");
    let cookie = headers
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
    let key = serde_json::from_str::<Value>(&response).unwrap()["session_key"]
        .as_str()
        .expect("operator login returned no session key")
        .to_string();
    OperatorSession { cookie, key }
}

fn post_as(
    state: &Path,
    port: u16,
    asserted: &str,
    session: Option<&OperatorSession>,
    path: &str,
    body: &Value,
) -> (u16, String, String) {
    let host = format!("cadence-{port}.localhost:{port}");
    let token = Seam::token_at(state).unwrap();
    let session = session
        .map(|session| {
            format!(
                "Cookie: {}\r\nX-Cadence-Session: {}\r\n",
                session.cookie, session.key
            )
        })
        .unwrap_or_default();
    let body = body.to_string();
    let request = format!(
        "POST {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         X-Cadence-Board: 1\r\nOrigin: http://{host}\r\nSec-Fetch-Site: same-origin\r\n\
         {AS_HEADER}: {asserted}\r\n{TOKEN_HEADER}: {token}\r\n{session}\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    raw_http(port, &request)
}

fn create_path(receipt: &Value, context: &str) -> String {
    format!(
        "/api/app-installations/{}/contexts/{context}/views/customer-create-form/actions/customer.create",
        receipt["install_id"].as_str().unwrap()
    )
}

fn update_path(receipt: &Value, context: &str, record: &str) -> String {
    format!(
        "/api/app-installations/{}/contexts/{context}/views/customer-edit-form/actions/customer.update/records/{record}",
        receipt["install_id"].as_str().unwrap()
    )
}

fn create_input(name: &str) -> Value {
    let email = format!(
        "{}@example.test",
        name.to_ascii_lowercase().replace(' ', "-")
    );
    json!({
        "display_name": name,
        "email": email,
        "phone": "+1 555 0198",
        "source": "acceptance",
        "tags": ["cad867", "customer"]
    })
}

fn action_create_params(receipt: &Value, context: &str, input: Value) -> Value {
    json!({
        "install_id": receipt["install_id"],
        "context_id": context,
        "view_id": "customer-create-form",
        "action_id": "customer.create",
        "digest": receipt["digest"],
        "view_descriptor_digest": receipt["view_descriptor_digest"],
        "view_binding_digest": receipt["view_binding_digest"],
        "input": input
    })
}

fn action_update_params(
    receipt: &Value,
    context: &str,
    record: &str,
    revision: i64,
    input: Value,
) -> Value {
    json!({
        "install_id": receipt["install_id"],
        "context_id": context,
        "view_id": "customer-edit-form",
        "action_id": "customer.update",
        "digest": receipt["digest"],
        "view_descriptor_digest": receipt["view_descriptor_digest"],
        "view_binding_digest": receipt["view_binding_digest"],
        "record_id": record,
        "expected_revision": revision,
        "input": input
    })
}

fn action_http_body(receipt: &Value, input: Value, revision: Option<i64>) -> Value {
    let mut body = json!({
        "digest": receipt["digest"],
        "descriptor": receipt["view_descriptor_digest"],
        "binding": receipt["view_binding_digest"],
        "input": input
    });
    if let Some(revision) = revision {
        body["expected_revision"] = json!(revision);
    }
    body
}

fn read_show(fixture: &Fixture, receipt: &Value, context: &str, record: &str) -> Value {
    let result = fixture.op(
        "app_view_read",
        json!({
            "install_id": receipt["install_id"],
            "view_id": "customer-detail",
            "op": "show",
            "digest": receipt["digest"],
            "view_descriptor_digest": receipt["view_descriptor_digest"],
            "view_binding_digest": receipt["view_binding_digest"],
            "context_id": context,
            "record_id": record
        }),
    );
    assert_eq!(result["rows"].as_array().unwrap().len(), 1);
    assert_eq!(result["rows"][0]["record_id"], json!(record));
    for key in ["digest", "view_descriptor_digest", "view_binding_digest"] {
        assert_eq!(result[key], receipt[key], "host show did not echo {key}");
    }
    result
}

fn host_revision(fixture: &Fixture, receipt: &Value, context: &str, record: &str) -> i64 {
    read_show(fixture, receipt, context, record)["record_revision"]
        .as_i64()
        .filter(|revision| *revision > 0)
        .expect("customer show did not return the host record_revision")
}

fn assert_action_receipt(
    response: &Value,
    receipt: &Value,
    install: &str,
    context: &str,
    id: &str,
    revision: i64,
) {
    let keys = response
        .as_object()
        .expect("action success is an object")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        keys,
        [
            "digest",
            "record",
            "view_binding_digest",
            "view_descriptor_digest"
        ]
        .into_iter()
        .collect(),
        "success returns the record and exactly the three reviewed pins"
    );
    for key in ["digest", "view_descriptor_digest", "view_binding_digest"] {
        assert_eq!(response[key], receipt[key], "action receipt pin {key}");
    }
    assert_eq!(response["record"]["id"], json!(id));
    assert_eq!(response["record"]["install_id"], json!(install));
    assert_eq!(response["record"]["context_id"], json!(context));
    assert_eq!(response["record"]["kind"], json!("customer"));
    assert_eq!(response["record"]["revision"], json!(revision));
}

fn assert_host_minted(id: &str) {
    let suffix = id
        .strip_prefix("cust-")
        .expect("create id is not host-minted cust-<hex>");
    assert_eq!(suffix.len(), 32, "host ID has 32 lowercase hex digits");
    assert!(
        suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "host ID suffix is not lowercase hexadecimal: {suffix}"
    );
}

fn store_records(fixture: &Fixture, install: &str, context: &str) -> Value {
    RecordStore::open(&fixture.state, install)
        .unwrap()
        .app_record_list(context)
        .unwrap()["records"]
        .clone()
}

fn snapshot(fixture: &Fixture, scopes: &[(&str, &str)]) -> Value {
    Value::Array(
        scopes
            .iter()
            .map(|(install, context)| {
                json!({
                    "install_id": install,
                    "context_id": context,
                    "records": store_records(fixture, install, context)
                })
            })
            .collect(),
    )
}

fn assert_no_private_markers(text: &str) {
    for marker in REDACTION_MARKERS {
        assert!(!text.contains(marker), "refusal disclosed canary {marker}");
    }
}

fn assert_rpc_refused(
    fixture: &Fixture,
    params: Value,
    expected_error: Option<&str>,
    scopes: &[(&str, &str)],
    before: &Value,
) {
    let error = fixture
        .rpc(Asserted::Operator, "app_view_action", params)
        .expect_err("invalid action request unexpectedly succeeded")
        .to_string();
    if let Some(expected) = expected_error {
        assert!(
            error.contains(expected),
            "refused for the wrong reason; expected {expected:?}, got {error:?}"
        );
    }
    assert_no_private_markers(&error);
    assert_eq!(
        snapshot(fixture, scopes),
        *before,
        "refused RPC changed a record, revision, history or consent provenance"
    );
}

struct HttpRefusal<'a> {
    port: u16,
    asserted: &'a str,
    session: Option<&'a OperatorSession>,
    path: &'a str,
    body: &'a Value,
    expected_status: u16,
    expected_body: &'a str,
    scopes: &'a [(&'a str, &'a str)],
    before: &'a Value,
}

fn assert_http_refused(fixture: &Fixture, refusal: HttpRefusal<'_>) {
    let (status, _, response) = post_as(
        &fixture.state,
        refusal.port,
        refusal.asserted,
        refusal.session,
        refusal.path,
        refusal.body,
    );
    assert_eq!(
        status, refusal.expected_status,
        "HTTP refusal response: {response}"
    );
    assert!(
        response.contains(refusal.expected_body),
        "HTTP refused for an unexpected reason: {response}"
    );
    assert_no_private_markers(&response);
    assert_eq!(
        snapshot(fixture, refusal.scopes),
        *refusal.before,
        "refused HTTP action changed a record, revision, history or consent provenance"
    );
}

fn propose_upgrade(fixture: &Fixture, installed: &Value, request_id: &str) -> Value {
    let proposed = fixture.op(
        "app_workspace_upgrade_check",
        json!({
            "install_id": installed["install_id"],
            "source": fixture.source_crm,
            "expected_digest": installed["digest"],
            "expected_generation": installed["catalog_generation"]
        }),
    );
    fixture.op(
        "app_workspace_upgrade",
        json!({
            "install_id": installed["install_id"],
            "source": fixture.source_crm,
            "expected_digest": installed["digest"],
            "expected_generation": installed["catalog_generation"],
            "expected_new_digest": proposed["digest"],
            "request_id": request_id
        }),
    );
    fixture.show(installed["install_id"].as_str().unwrap())
}

fn spawn_setsid(mut command: Command) -> std::process::Output {
    // SAFETY: setsid is called in the child immediately before exec; no
    // parent process state or identity environment is altered.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    cadence_agent::reaper::output(&mut command).expect("wait for our own detached probe child")
}

/// Child entrypoint for the daemon RPC setsid probe. The identity is
/// explicitly `unproven` through the test seam; this is not native proof.
#[test]
#[ignore = "spawned by cad867_pinned_customer_actions_refuse_without_mutation_on_rpc_and_http"]
fn cad867_rpc_detached_child_probe() {
    let (Ok(state), Ok(raw_params)) = (
        std::env::var("CAD867_ACTION_RPC_PROBE_STATE"),
        std::env::var("CAD867_ACTION_RPC_PROBE_PARAMS"),
    ) else {
        return;
    };
    let params = serde_json::from_str(&raw_params).unwrap();
    let error = client::rpc(Path::new(&state), "app_view_action", params)
        .expect_err("unproven setsid RPC reached the operator-only action")
        .to_string();
    assert!(
        error.contains("app view action") && error.contains("operator"),
        "{error}"
    );
    assert_no_private_markers(&error);
}

/// Child entrypoint for a detached real HTTP client. The caller role is
/// asserted by the fixture's X-Cadence-Test-As header, not by native auth.
#[test]
#[ignore = "spawned by cad867_pinned_customer_actions_refuse_without_mutation_on_rpc_and_http"]
fn cad867_http_detached_child_probe() {
    let (Ok(state), Ok(port), Ok(path), Ok(raw_body)) = (
        std::env::var("CAD867_ACTION_HTTP_PROBE_STATE"),
        std::env::var("CAD867_ACTION_HTTP_PROBE_PORT"),
        std::env::var("CAD867_ACTION_HTTP_PROBE_PATH"),
        std::env::var("CAD867_ACTION_HTTP_PROBE_BODY"),
    ) else {
        return;
    };
    let port: u16 = port.parse().unwrap();
    let host = format!("cadence-{port}.localhost:{port}");
    let token = Seam::token_at(Path::new(&state)).expect("child can read this fixture token");
    let request = format!(
        "POST {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         X-Cadence-Board: 1\r\nOrigin: http://{host}\r\nSec-Fetch-Site: same-origin\r\n\
         {AS_HEADER}: agent:{AGENT}\r\n{TOKEN_HEADER}: {token}\r\n\
         Content-Length: {}\r\n\r\n{raw_body}",
        raw_body.len()
    );
    let (status, _, response) = raw_http(port, &request);
    println!("HTTP_STATUS={status}; {response}");
    assert_eq!(status, 403, "detached HTTP agent request: {response}");
    assert!(
        response.contains("operator_only"),
        "not the operator route guard: {response}"
    );
    assert_no_private_markers(&response);
}

/// Prove pinned CRM create/edit success first, then exercise the daemon
/// guards and real HTTP relay. The second installation provides genuine
/// foreign context/record receipts and a v1-only inert form control.
#[test]
fn cad867_pinned_customer_actions_refuse_without_mutation_on_rpc_and_http() {
    let fixture = Fixture::new();
    let installed_a = fixture.install(&fixture.source_crm);
    let install_a = installed_a["install_id"].as_str().unwrap().to_string();
    let original_a = installed_a.clone();
    assert_eq!(
        installed_a["action_descriptor"]["contract"],
        "app-actions/v2"
    );
    assert_eq!(installed_a["action_descriptor"]["app"], "crm");
    assert_eq!(
        installed_a["action_descriptor"]["actions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let installed_b = fixture.install(&fixture.source_preview);
    let install_b = installed_b["install_id"].as_str().unwrap().to_string();
    assert!(installed_b["action_descriptor"].is_null());
    assert_ne!(installed_a["digest"], installed_b["digest"]);

    let context_a = fixture.context(&install_a, "cad867-action-context-a");
    let context_a_other = fixture.context(&install_a, "cad867-action-context-a-other");
    let context_b = fixture.context(&install_b, "cad867-action-context-b");
    fixture.plant_agent(AGENT);

    // Seed a real customer with positive and negative consent plus its
    // method/note provenance; action updates must carry this profile forward.
    let seeded = fixture.create_customer(CustomerSeed {
        install: &install_a,
        context: &context_a,
        record_id: "cust-action-consent-seed",
        name: CUSTOMER_CANARY,
        email: CUSTOMER_EMAIL,
        email_consent: "granted",
        sms_consent: Some("denied"),
        provenance: Some(json!({"method":"web_form","note":"CAD867 consent seed"})),
    });
    let seed_id = seeded["record"]["id"].as_str().unwrap().to_string();
    let initial_record = seeded["record"].clone();
    assert_eq!(initial_record["profile"]["consent"]["email"], "granted");
    assert_eq!(initial_record["profile"]["consent"]["sms"], "denied");
    assert!(initial_record["consent_history"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| { entry["method"] == "web_form" && entry["note"] == "CAD867 consent seed" }));

    let other_context_record = fixture.create_customer(CustomerSeed {
        install: &install_a,
        context: &context_a_other,
        record_id: "cust-other-context",
        name: "CAD867_OTHER_CONTEXT_CUSTOMER",
        email: "cad867-other-context@example.invalid",
        email_consent: "unknown",
        sms_consent: None,
        provenance: None,
    });
    let other_context_id = other_context_record["record"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let foreign_b = fixture.create_customer(CustomerSeed {
        install: &install_b,
        context: &context_b,
        record_id: "cust-foreign-install",
        name: FOREIGN_CANARY,
        email: FOREIGN_EMAIL,
        email_consent: "unknown",
        sms_consent: None,
        provenance: None,
    });
    let foreign_b_id = foreign_b["record"]["id"].as_str().unwrap().to_string();

    let current_a = fixture.show(&install_a);
    let current_b = fixture.show(&install_b);
    assert_eq!(current_a["action_descriptor"]["contract"], "app-actions/v2");
    assert_eq!(current_b["action_descriptor"], Value::Null);
    let preview_views = current_b["view_descriptor"]["views"]
        .as_array()
        .expect("preview install has its v1 descriptor");
    assert!(preview_views
        .iter()
        .any(|view| view["id"] == "customer-create-form"));
    assert!(preview_views
        .iter()
        .any(|view| view["id"] == "customer-edit-form"));

    // B's item is an actual installed/scope-bound record, not an invented
    // foreign ID; B's current bound read is the positive control for it.
    let foreign_show = fixture.op(
        "app_record_show",
        json!({
            "install_id": install_b,
            "context_id": context_b,
            "record_id": foreign_b_id
        }),
    );
    assert_eq!(foreign_show["record"]["id"], foreign_b_id);
    assert_eq!(
        foreign_show["record"]["profile"]["display_name"],
        FOREIGN_CANARY
    );
    let b_list = fixture.op(
        "app_view_read",
        json!({
            "install_id": install_b,
            "view_id": "customers",
            "op": "list",
            "digest": current_b["digest"],
            "view_descriptor_digest": current_b["view_descriptor_digest"],
            "view_binding_digest": current_b["view_binding_digest"],
            "context_id": context_b
        }),
    );
    assert!(b_list["rows"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| { row["record_id"] == foreign_b_id && row["name"] == FOREIGN_CANARY }));

    let port_lease = lease_port();
    let port = port_lease.port;
    let _board = Board::start(&fixture.state, &fixture.pm.dir, port);
    let session = sign_in(&fixture.state, port);
    let crm_create_path = create_path(&current_a, &context_a);
    let rpc_create =
        action_create_params(&current_a, &context_a, create_input("CAD867 RPC Create"));

    // Current-pin RPC update control using the host-owned show revision.
    let revision_before_rpc_edit = host_revision(&fixture, &current_a, &context_a, &seed_id);
    assert_eq!(revision_before_rpc_edit, initial_record["revision"]);
    let rpc_edited = fixture
        .rpc(
            Asserted::Operator,
            "app_view_action",
            action_update_params(
                &current_a,
                &context_a,
                &seed_id,
                revision_before_rpc_edit,
                json!({"display_name":"CAD867 RPC Edit"}),
            ),
        )
        .unwrap_or_else(|error| panic!("pinned RPC edit control failed: {error}"));
    assert_action_receipt(
        &rpc_edited,
        &current_a,
        &install_a,
        &context_a,
        &seed_id,
        revision_before_rpc_edit + 1,
    );
    assert_eq!(
        rpc_edited["record"]["profile"]["display_name"],
        "CAD867 RPC Edit"
    );
    assert_eq!(rpc_edited["record"]["profile"]["email"], CUSTOMER_EMAIL);
    assert_eq!(
        rpc_edited["record"]["profile"]["consent"],
        initial_record["profile"]["consent"]
    );
    assert_eq!(
        rpc_edited["record"]["consent_history"],
        initial_record["consent_history"]
    );

    // The second positive edit goes over the authenticated real board and
    // also takes its expected_revision from app_view_read, never Store state.
    let revision_before_http_edit = host_revision(&fixture, &current_a, &context_a, &seed_id);
    let http_edit_body = action_http_body(
        &current_a,
        json!({"display_name":"CAD867 HTTP Edit"}),
        Some(revision_before_http_edit),
    );
    let (status, _, http_edit_text) = post_as(
        &fixture.state,
        port,
        "operator",
        Some(&session),
        &update_path(&current_a, &context_a, &seed_id),
        &http_edit_body,
    );
    assert_eq!(status, 200, "pinned HTTP edit control: {http_edit_text}");
    let http_edited: Value = serde_json::from_str(&http_edit_text).unwrap();
    assert_action_receipt(
        &http_edited,
        &current_a,
        &install_a,
        &context_a,
        &seed_id,
        revision_before_http_edit + 1,
    );
    assert_eq!(
        http_edited["record"]["profile"]["display_name"],
        "CAD867 HTTP Edit"
    );
    assert_eq!(http_edited["record"]["profile"]["email"], CUSTOMER_EMAIL);
    assert_eq!(
        http_edited["record"]["profile"]["consent"],
        initial_record["profile"]["consent"]
    );
    assert_eq!(
        http_edited["record"]["consent_history"],
        initial_record["consent_history"]
    );

    // Both peers reach real create operations with the same installed pins;
    // there is no client record id or create revision in either request.
    let rpc_created = fixture
        .rpc(Asserted::Operator, "app_view_action", rpc_create.clone())
        .unwrap_or_else(|error| panic!("pinned RPC create control failed: {error}"));
    let rpc_created_id = rpc_created["record"]["id"].as_str().unwrap().to_string();
    assert_host_minted(&rpc_created_id);
    assert_action_receipt(
        &rpc_created,
        &current_a,
        &install_a,
        &context_a,
        &rpc_created_id,
        1,
    );
    assert_eq!(
        rpc_created["record"]["profile"]["consent"]["email"],
        "unknown"
    );
    assert!(rpc_created["record"]["profile"]["consent"]["sms"].is_null());

    let http_created_body = action_http_body(&current_a, create_input("CAD867 HTTP Create"), None);
    let (status, _, http_created_text) = post_as(
        &fixture.state,
        port,
        "operator",
        Some(&session),
        &crm_create_path,
        &http_created_body,
    );
    assert_eq!(
        status, 200,
        "pinned HTTP create control: {http_created_text}"
    );
    let http_created: Value = serde_json::from_str(&http_created_text).unwrap();
    let http_created_id = http_created["record"]["id"].as_str().unwrap().to_string();
    assert_host_minted(&http_created_id);
    assert_action_receipt(
        &http_created,
        &current_a,
        &install_a,
        &context_a,
        &http_created_id,
        1,
    );
    assert_eq!(
        http_created["record"]["profile"]["consent"]["email"],
        "unknown"
    );
    assert!(http_created["record"]["profile"]["consent"]["sms"].is_null());

    // Upgrade this actual installed bundle once, changing all three pins
    // independently while retaining a valid action/view/binding pair. The
    // old values below are genuine pins from the successful pre-upgrade install.
    let old_a = fixture.show(&install_a);
    assert_eq!(old_a["digest"], original_a["digest"]);
    let manifest_path = fixture.source_crm.join("app.md");
    let manifest = std::fs::read_to_string(&manifest_path).unwrap();
    assert!(manifest.contains("version: 0.1.0"));
    std::fs::write(
        manifest_path,
        manifest.replace("version: 0.1.0", "version: 0.2.0"),
    )
    .unwrap();
    let descriptor_path = fixture.source_crm.join("views/app-views-v1.json");
    let descriptor = std::fs::read_to_string(&descriptor_path).unwrap();
    assert!(descriptor.contains("CRM — customers overview"));
    std::fs::write(
        descriptor_path,
        descriptor.replace(
            "CRM — customers overview",
            "CRM — customers overview refreshed",
        ),
    )
    .unwrap();
    let binding_path = fixture.source_crm.join("bindings/app-bindings-v1.json");
    let binding = std::fs::read_to_string(&binding_path).unwrap();
    assert!(binding.contains("CRM bindings — customers"));
    std::fs::write(
        binding_path,
        binding.replace(
            "CRM bindings — customers",
            "CRM bindings — customers refreshed",
        ),
    )
    .unwrap();
    let current_a = propose_upgrade(&fixture, &old_a, "cad867-action-guard-upgrade");
    for key in ["digest", "view_descriptor_digest", "view_binding_digest"] {
        assert_ne!(old_a[key], current_a[key], "upgrade did not move {key}");
    }
    assert_eq!(current_a["action_descriptor"]["contract"], "app-actions/v2");

    // Prove the upgraded action route still succeeds over both peers before
    // exercising authentic stale pins and the broader refusal matrix.
    let revision_after_upgrade = host_revision(&fixture, &current_a, &context_a, &seed_id);
    let rpc_after_upgrade = fixture
        .rpc(
            Asserted::Operator,
            "app_view_action",
            action_update_params(
                &current_a,
                &context_a,
                &seed_id,
                revision_after_upgrade,
                json!({"display_name":"CAD867 RPC Post Upgrade"}),
            ),
        )
        .unwrap_or_else(|error| panic!("current upgraded RPC control failed: {error}"));
    assert_action_receipt(
        &rpc_after_upgrade,
        &current_a,
        &install_a,
        &context_a,
        &seed_id,
        revision_after_upgrade + 1,
    );
    let revision_for_http_after_upgrade = host_revision(&fixture, &current_a, &context_a, &seed_id);
    let (status, _, http_after_upgrade_text) = post_as(
        &fixture.state,
        port,
        "operator",
        Some(&session),
        &update_path(&current_a, &context_a, &seed_id),
        &action_http_body(
            &current_a,
            json!({"display_name":"CAD867 HTTP Post Upgrade"}),
            Some(revision_for_http_after_upgrade),
        ),
    );
    assert_eq!(
        status, 200,
        "current upgraded HTTP control: {http_after_upgrade_text}"
    );
    let http_after_upgrade: Value = serde_json::from_str(&http_after_upgrade_text).unwrap();
    assert_action_receipt(
        &http_after_upgrade,
        &current_a,
        &install_a,
        &context_a,
        &seed_id,
        revision_for_http_after_upgrade + 1,
    );
    assert_eq!(
        http_after_upgrade["record"]["profile"]["consent"],
        initial_record["profile"]["consent"]
    );
    assert_eq!(
        http_after_upgrade["record"]["consent_history"],
        initial_record["consent_history"]
    );

    let scopes = [
        (install_a.as_str(), context_a.as_str()),
        (install_a.as_str(), context_a_other.as_str()),
        (install_a.as_str(), context_b.as_str()),
        (install_b.as_str(), context_b.as_str()),
    ];
    let before_refusals = snapshot(&fixture, &scopes);
    let current_revision = host_revision(&fixture, &current_a, &context_a, &seed_id);
    let create_attempt = action_create_params(&current_a, &context_a, create_input(LEAK_ATTEMPT));
    let update_attempt = action_update_params(
        &current_a,
        &context_a,
        &seed_id,
        current_revision,
        json!({"display_name":LEAK_ATTEMPT}),
    );

    // Agent, unattributed and detached-child daemon requests have complete,
    // valid action selectors/pins/context/input, so refusal is the operator
    // guard rather than setup, missing-method or malformed-payload behavior.
    let agent_error = fixture
        .rpc(
            Asserted::Agent(AGENT.to_string()),
            "app_view_action",
            update_attempt.clone(),
        )
        .expect_err("registered agent reached customer.update")
        .to_string();
    assert!(agent_error.contains(AGENT) && agent_error.contains("operator action"));
    assert_no_private_markers(&agent_error);
    assert_eq!(snapshot(&fixture, &scopes), before_refusals);
    let unproven_error = fixture
        .rpc(
            Asserted::Unproven,
            "app_view_action",
            update_attempt.clone(),
        )
        .expect_err("unproven caller reached customer.update")
        .to_string();
    assert!(
        unproven_error.contains("not provably the operator"),
        "{unproven_error}"
    );
    assert_no_private_markers(&unproven_error);
    assert_eq!(snapshot(&fixture, &scopes), before_refusals);

    let mut detached_rpc = Command::new(std::env::current_exe().unwrap());
    detached_rpc
        .args([
            "--ignored",
            "--exact",
            "cad867_rpc_detached_child_probe",
            "--nocapture",
        ])
        .env("CAD867_ACTION_RPC_PROBE_STATE", &fixture.state)
        .env("CAD867_ACTION_RPC_PROBE_PARAMS", update_attempt.to_string())
        .env(AS_ENV, "unproven");
    let detached_rpc_output = spawn_setsid(detached_rpc);
    assert!(
        detached_rpc_output.status.success(),
        "detached RPC child failed: {}",
        String::from_utf8_lossy(&detached_rpc_output.stderr)
    );
    assert_eq!(snapshot(&fixture, &scopes), before_refusals);

    // HTTP's agent route is tested without replaying/revoking the valid
    // operator session. The detached child is a real setsid HTTP client;
    // its role comes from the fixture-only identity header, not the kernel.
    assert_http_refused(
        &fixture,
        HttpRefusal {
            port,
            asserted: &format!("agent:{AGENT}"),
            session: None,
            path: &update_path(&current_a, &context_a, &seed_id),
            body: &action_http_body(
                &current_a,
                json!({"display_name":LEAK_ATTEMPT}),
                Some(current_revision),
            ),
            expected_status: 403,
            expected_body: "operator_only",
            scopes: &scopes,
            before: &before_refusals,
        },
    );
    let mut detached_http = Command::new(std::env::current_exe().unwrap());
    detached_http
        .args([
            "--ignored",
            "--exact",
            "cad867_http_detached_child_probe",
            "--nocapture",
        ])
        .env("CAD867_ACTION_HTTP_PROBE_STATE", &fixture.state)
        .env("CAD867_ACTION_HTTP_PROBE_PORT", port.to_string())
        .env(
            "CAD867_ACTION_HTTP_PROBE_PATH",
            update_path(&current_a, &context_a, &seed_id),
        )
        .env(
            "CAD867_ACTION_HTTP_PROBE_BODY",
            action_http_body(
                &current_a,
                json!({"display_name":LEAK_ATTEMPT}),
                Some(current_revision),
            )
            .to_string(),
        );
    let detached_http_output = spawn_setsid(detached_http);
    assert!(
        detached_http_output.status.success(),
        "detached HTTP child failed: {}",
        String::from_utf8_lossy(&detached_http_output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&detached_http_output.stdout).contains("HTTP_STATUS=403"),
        "detached child did not report the expected route refusal: {}",
        String::from_utf8_lossy(&detached_http_output.stdout)
    );
    assert_eq!(snapshot(&fixture, &scopes), before_refusals);

    // Each old pin below is authentic from the pre-upgrade installed
    // receipt. Hold the other two pins at their current values and exercise
    // every individual stale-pin refusal through both actual peers.
    let old_pins = [
        ("digest", "installation digest is stale"),
        ("view_descriptor_digest", "view descriptor digest is stale"),
        ("view_binding_digest", "view binding digest is stale"),
    ];
    for (pin, expected) in old_pins {
        let mut rpc = create_attempt.clone();
        rpc[pin] = old_a[pin].clone();
        assert_rpc_refused(&fixture, rpc, Some(expected), &scopes, &before_refusals);

        let mut body = action_http_body(&current_a, create_input(LEAK_ATTEMPT), None);
        let http_key = match pin {
            "digest" => "digest",
            "view_descriptor_digest" => "descriptor",
            "view_binding_digest" => "binding",
            _ => unreachable!(),
        };
        body[http_key] = old_a[pin].clone();
        assert_http_refused(
            &fixture,
            HttpRefusal {
                port,
                asserted: "operator",
                session: Some(&session),
                path: &crm_create_path,
                body: &body,
                expected_status: 400,
                expected_body: "app view action refused or unavailable",
                scopes: &scopes,
                before: &before_refusals,
            },
        );
    }

    // The authentic v1-only installation has a real form preview and
    // binding but no actions companion. It cannot use that preview to write.
    let preview_create = action_create_params(&current_b, &context_b, create_input(LEAK_ATTEMPT));
    assert_rpc_refused(
        &fixture,
        preview_create,
        Some("does not declare the pinned action/view/binding pair"),
        &scopes,
        &before_refusals,
    );
    assert_http_refused(
        &fixture,
        HttpRefusal {
            port,
            asserted: "operator",
            session: Some(&session),
            path: &create_path(&current_b, &context_b),
            body: &action_http_body(&current_b, create_input(LEAK_ATTEMPT), None),
            expected_status: 400,
            expected_body: "app view action refused or unavailable",
            scopes: &scopes,
            before: &before_refusals,
        },
    );

    // Replay B's authentic three pins against A, and A's against B. The
    // real receipts differ; install-scoped digest binding rejects before
    // either installation's record file can be mutated.
    let mut b_receipt_on_a = create_attempt.clone();
    b_receipt_on_a["digest"] = current_b["digest"].clone();
    b_receipt_on_a["view_descriptor_digest"] = current_b["view_descriptor_digest"].clone();
    b_receipt_on_a["view_binding_digest"] = current_b["view_binding_digest"].clone();
    assert_rpc_refused(
        &fixture,
        b_receipt_on_a,
        Some("installation digest is stale"),
        &scopes,
        &before_refusals,
    );
    let mut a_receipt_on_b =
        action_create_params(&current_b, &context_b, create_input(LEAK_ATTEMPT));
    a_receipt_on_b["digest"] = current_a["digest"].clone();
    a_receipt_on_b["view_descriptor_digest"] = current_a["view_descriptor_digest"].clone();
    a_receipt_on_b["view_binding_digest"] = current_a["view_binding_digest"].clone();
    assert_rpc_refused(
        &fixture,
        a_receipt_on_b,
        Some("installation digest is stale"),
        &scopes,
        &before_refusals,
    );
    let mut b_receipt_http_on_a = action_http_body(&current_a, create_input(LEAK_ATTEMPT), None);
    b_receipt_http_on_a["digest"] = current_b["digest"].clone();
    b_receipt_http_on_a["descriptor"] = current_b["view_descriptor_digest"].clone();
    b_receipt_http_on_a["binding"] = current_b["view_binding_digest"].clone();
    assert_http_refused(
        &fixture,
        HttpRefusal {
            port,
            asserted: "operator",
            session: Some(&session),
            path: &crm_create_path,
            body: &b_receipt_http_on_a,
            expected_status: 400,
            expected_body: "app view action refused or unavailable",
            scopes: &scopes,
            before: &before_refusals,
        },
    );
    let mut a_receipt_http_on_b = action_http_body(&current_b, create_input(LEAK_ATTEMPT), None);
    a_receipt_http_on_b["digest"] = current_a["digest"].clone();
    a_receipt_http_on_b["descriptor"] = current_a["view_descriptor_digest"].clone();
    a_receipt_http_on_b["binding"] = current_a["view_binding_digest"].clone();
    assert_http_refused(
        &fixture,
        HttpRefusal {
            port,
            asserted: "operator",
            session: Some(&session),
            path: &create_path(&current_b, &context_b),
            body: &a_receipt_http_on_b,
            expected_status: 400,
            expected_body: "app view action refused or unavailable",
            scopes: &scopes,
            before: &before_refusals,
        },
    );

    // A real B context under A's install is not an A context. A's current
    // action descriptor/pins are otherwise valid, so this reaches context
    // ownership proof before any record file opens.
    let mut foreign_context_rpc =
        action_create_params(&current_a, &context_b, create_input(LEAK_ATTEMPT));
    foreign_context_rpc["install_id"] = json!(install_a);
    assert_rpc_refused(
        &fixture,
        foreign_context_rpc,
        Some("context is unavailable for this installation"),
        &scopes,
        &before_refusals,
    );
    let foreign_context_http_path = create_path(&current_a, &context_b);
    assert_http_refused(
        &fixture,
        HttpRefusal {
            port,
            asserted: "operator",
            session: Some(&session),
            path: &foreign_context_http_path,
            body: &action_http_body(&current_a, create_input(LEAK_ATTEMPT), None),
            expected_status: 400,
            expected_body: "app view action refused or unavailable",
            scopes: &scopes,
            before: &before_refusals,
        },
    );

    // Both a record in A's other real context and B's actual customer are
    // unavailable through A's context-bound update, over both peers.
    for foreign_record in [&other_context_id, &foreign_b_id] {
        let rpc = action_update_params(
            &current_a,
            &context_a,
            foreign_record,
            1,
            json!({"display_name":LEAK_ATTEMPT}),
        );
        assert_rpc_refused(
            &fixture,
            rpc,
            Some("record is unavailable for this installation and context"),
            &scopes,
            &before_refusals,
        );
        assert_http_refused(
            &fixture,
            HttpRefusal {
                port,
                asserted: "operator",
                session: Some(&session),
                path: &update_path(&current_a, &context_a, foreign_record),
                body: &action_http_body(&current_a, json!({"display_name":LEAK_ATTEMPT}), Some(1)),
                expected_status: 400,
                expected_body: "app view action refused or unavailable",
                scopes: &scopes,
                before: &before_refusals,
            },
        );
    }

    // RPC selectors are closed; the board derives action/view IDs from its
    // route and accepts no selector fields in JSON. Unknown dotted action IDs
    // are exercised on RPC (HTTP's unknown route would be a router 404).
    let mut unknown_action = create_attempt.clone();
    unknown_action["action_id"] = json!("customer.delete");
    assert_rpc_refused(
        &fixture,
        unknown_action,
        Some("unsupported installed customer action"),
        &scopes,
        &before_refusals,
    );
    let mut wrong_form = create_attempt.clone();
    wrong_form["view_id"] = json!("customer-edit-form");
    assert_rpc_refused(
        &fixture,
        wrong_form,
        Some("action does not match the installed form route"),
        &scopes,
        &before_refusals,
    );
    let wrong_form_http_path = format!(
        "/api/app-installations/{install_a}/contexts/{context_a}/views/customer-edit-form/actions/customer.create"
    );
    assert_http_refused(
        &fixture,
        HttpRefusal {
            port,
            asserted: "operator",
            session: Some(&session),
            path: &wrong_form_http_path,
            body: &action_http_body(&current_a, create_input(LEAK_ATTEMPT), None),
            expected_status: 400,
            expected_body: "app view action refused or unavailable",
            scopes: &scopes,
            before: &before_refusals,
        },
    );
    for (key, value) in [
        ("actor", json!("operator")),
        ("source", json!("forged")),
        ("op", json!("record.create")),
        ("consent", json!({"email":"granted"})),
        ("action_id", json!("customer.delete")),
        ("view_id", json!("customers")),
        ("record_id", json!("cust-client-chosen")),
        ("expected_revision", json!(1)),
    ] {
        let mut params = create_attempt.clone();
        params[key] = value.clone();
        let expected = if key == "actor" {
            "authority is connection-bound"
        } else if matches!(key, "record_id" | "expected_revision") {
            "does not accept a record id or revision"
        } else if key == "action_id" {
            "unsupported installed customer action"
        } else if key == "view_id" {
            "action does not match the installed form route"
        } else {
            "unsupported fields"
        };
        assert_rpc_refused(&fixture, params, Some(expected), &scopes, &before_refusals);

        let mut body = action_http_body(&current_a, create_input(LEAK_ATTEMPT), None);
        body[key] = value;
        assert_http_refused(
            &fixture,
            HttpRefusal {
                port,
                asserted: "operator",
                session: Some(&session),
                path: &crm_create_path,
                body: &body,
                expected_status: 400,
                expected_body: "invalid app view action schema",
                scopes: &scopes,
                before: &before_refusals,
            },
        );
    }

    // The body itself admits only the declared typed input keys. `source`
    // is a legitimate input field, but actor/consent and arbitrary keys are not.
    for (key, value) in [
        ("actor", json!("operator")),
        ("consent", json!({"email":"granted"})),
        ("install_id", json!(install_b)),
        ("arbitrary", json!("not in the descriptor")),
    ] {
        let mut rpc = create_attempt.clone();
        rpc["input"][key] = value.clone();
        assert_rpc_refused(
            &fixture,
            rpc,
            Some("customer action input has unsupported fields"),
            &scopes,
            &before_refusals,
        );
        let mut body = action_http_body(&current_a, create_input(LEAK_ATTEMPT), None);
        body["input"][key] = value;
        assert_http_refused(
            &fixture,
            HttpRefusal {
                port,
                asserted: "operator",
                session: Some(&session),
                path: &crm_create_path,
                body: &body,
                expected_status: 400,
                expected_body: "app view action refused or unavailable",
                scopes: &scopes,
                before: &before_refusals,
            },
        );
    }

    // Stale CAS is a valid update request except for its host-owned revision;
    // verify an explicit daemon stale refusal and bounded HTTP 400.
    let stale_revision = current_revision - 1;
    assert!(stale_revision > 0);
    assert_rpc_refused(
        &fixture,
        action_update_params(
            &current_a,
            &context_a,
            &seed_id,
            stale_revision,
            json!({"display_name":LEAK_ATTEMPT}),
        ),
        Some("record revision is stale"),
        &scopes,
        &before_refusals,
    );
    assert_http_refused(
        &fixture,
        HttpRefusal {
            port,
            asserted: "operator",
            session: Some(&session),
            path: &update_path(&current_a, &context_a, &seed_id),
            body: &action_http_body(
                &current_a,
                json!({"display_name":LEAK_ATTEMPT}),
                Some(stale_revision),
            ),
            expected_status: 400,
            expected_body: "app view action refused or unavailable",
            scopes: &scopes,
            before: &before_refusals,
        },
    );

    // Same host revision, one real RPC and one real HTTP request released by
    // a barrier. Store CAS permits exactly one winner; there is no retry.
    let race_revision = host_revision(&fixture, &current_a, &context_a, &seed_id);
    assert_eq!(snapshot(&fixture, &scopes), before_refusals);
    let before_race_record = fixture.op(
        "app_record_show",
        json!({
            "install_id": install_a,
            "context_id": context_a,
            "record_id": seed_id
        }),
    );
    let before_race_provenance = before_race_record["record"]["consent_history"].clone();
    let barrier = Arc::new(Barrier::new(3));
    let rpc_barrier = Arc::clone(&barrier);
    let rpc_state = fixture.state.clone();
    let rpc_params = action_update_params(
        &current_a,
        &context_a,
        &seed_id,
        race_revision,
        json!({"display_name":RACE_RPC}),
    );
    let rpc_thread = std::thread::spawn(move || {
        rpc_barrier.wait();
        scoped(Asserted::Operator, || {
            client::rpc(&rpc_state, "app_view_action", rpc_params)
        })
    });
    let http_barrier = Arc::clone(&barrier);
    let http_state = fixture.state.clone();
    let http_session = session.clone();
    let http_path = update_path(&current_a, &context_a, &seed_id);
    let http_body = action_http_body(
        &current_a,
        json!({"display_name":RACE_HTTP}),
        Some(race_revision),
    );
    let http_thread = std::thread::spawn(move || {
        http_barrier.wait();
        post_as(
            &http_state,
            port,
            "operator",
            Some(&http_session),
            &http_path,
            &http_body,
        )
    });
    barrier.wait();
    let rpc_result = rpc_thread.join().unwrap();
    let (http_status, _, http_response) = http_thread.join().unwrap();
    let rpc_won = rpc_result.is_ok();
    let http_won = http_status == 200;
    assert_ne!(rpc_won, http_won, "exactly one same-revision peer must win");
    if let Err(error) = &rpc_result {
        assert!(
            error.to_string().contains("record revision is stale"),
            "{error}"
        );
        assert_no_private_markers(&error.to_string());
    }
    if !http_won {
        assert_eq!(http_status, 400, "HTTP race loser: {http_response}");
        assert!(http_response.contains("app view action refused or unavailable"));
        assert_no_private_markers(&http_response);
    }
    let after_race = fixture.op(
        "app_record_show",
        json!({
            "install_id": install_a,
            "context_id": context_a,
            "record_id": seed_id
        }),
    )["record"]
        .clone();
    assert_eq!(after_race["revision"], json!(race_revision + 1));
    assert_eq!(
        after_race["profile"]["display_name"],
        if rpc_won { RACE_RPC } else { RACE_HTTP }
    );
    assert_eq!(
        after_race["profile"]["consent"],
        initial_record["profile"]["consent"]
    );
    assert_eq!(after_race["consent_history"], before_race_provenance);
    assert_eq!(
        snapshot(&fixture, &scopes)[0]["records"]
            .as_array()
            .unwrap()
            .len(),
        3,
        "only the host seed and two positive action creates exist in A's context"
    );
}
