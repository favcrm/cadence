//! CAD-1128 hosted-admission acceptance leaf (independent author).
//!
//! The deployment boundary: with a hosted `smtp.internal` relay configured,
//! ENCRYPTED custody use is unsupported and must refuse before key reads,
//! relay traffic or core/vault writes — while a LEGACY SMTP record stays
//! rotatable even when an encrypted config exists (the repaired guard checks
//! the target's durable tag, not "any configured vault").
//!
//! Real in-process daemon, real loopback HTTP relay shaped like
//! `smtp.backend/v2`, real core SQLite and core/vault byte observations. The
//! caller seam is an EXPLICITLY SIMULATED operator (`scoped(Asserted::Operator,
//! …)`), not a native operator-proof claim. Synthetic paths/credentials only;
//! the 32-byte key and the config record are provisioned by this fixture under
//! a short process-temp root. The SELF-HOSTED phase legitimately initializes
//! and unlocks with that key; the later hosted+locked phase then asserts every
//! refusal/revoke takes no key access, no relay traffic and no core/vault
//! mutation (an inotify `IN_OPEN|IN_ACCESS` watch plus byte-exact inventory).
//!
//! LIMITS: the relay is a synthetic loopback HTTP peer — no external/live
//! provider or outbound network is used; only refusal ordering, the admitted
//! legacy verify/test-send, legacy rotation success, the self-hosted
//! encrypted origin and link retarget, custody tag, relay/direct-dial
//! counters, key-access events and core/vault bytes are asserted. No
//! request-body echo, schema-shape or human-wording assertions. No production
//! change and no public debug hook. This author did not build, run, lint or
//! format.

#![cfg(all(feature = "test-seam", target_os = "linux"))]

use cadence_agent::platform::smtp_internal::SmtpInternal;
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon, store::Store};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const SENDER: &str = "owner@restaurant.example";
const WAIT: Duration = Duration::from_secs(30);

// ---------- real loopback hosted relay (owned workers, joined) ----------

struct Worker {
    socket: TcpStream,
    thread: JoinHandle<()>,
}

struct Relay {
    url: String,
    password: Arc<Mutex<String>>,
    requests: Arc<AtomicU64>,
    verifies: Arc<AtomicU64>,
    sends: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    workers: Arc<Mutex<Vec<Worker>>>,
}

impl Relay {
    fn start(password: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("relay bind");
        listener.set_nonblocking(true).expect("relay nonblocking");
        let url = format!(
            "http://127.0.0.1:{}",
            listener.local_addr().expect("relay address").port()
        );
        let password = Arc::new(Mutex::new(password.to_string()));
        let requests = Arc::new(AtomicU64::new(0));
        let verifies = Arc::new(AtomicU64::new(0));
        let sends = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let workers = Arc::new(Mutex::new(Vec::new()));
        let accept = std::thread::spawn({
            let password = password.clone();
            let requests = requests.clone();
            let verifies = verifies.clone();
            let sends = sends.clone();
            let stop = stop.clone();
            let workers = workers.clone();
            move || {
                while !stop.load(SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let socket = stream.try_clone().expect("relay socket clone");
                            let password = password.clone();
                            let requests = requests.clone();
                            let verifies = verifies.clone();
                            let sends = sends.clone();
                            let thread = std::thread::spawn(move || {
                                serve_relay(stream, password, requests, verifies, sends);
                            });
                            workers
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .push(Worker { socket, thread });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            }
        });
        Self {
            url,
            password,
            requests,
            verifies,
            sends,
            stop,
            accept: Some(accept),
            workers,
        }
    }

    fn set_password(&self, password: &str) {
        *self
            .password
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = password.to_string();
    }

    fn count(&self) -> u64 {
        self.requests.load(SeqCst)
    }

    fn verifies(&self) -> u64 {
        self.verifies.load(SeqCst)
    }

    fn sends(&self) -> u64 {
        self.sends.load(SeqCst)
    }

    /// Stop accepting, wake every blocked request socket, join every owned
    /// worker within one bound. Failures are returned, never hidden.
    fn stop(&mut self) -> Result<(), String> {
        self.stop.store(true, SeqCst);
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut outcome = Ok(());
        if let Some(handle) = self.accept.take() {
            if let Err(error) = join_worker(handle, deadline) {
                outcome = Err(error);
            }
        }
        let workers = {
            let mut guard = self
                .workers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *guard)
        };
        for worker in &workers {
            let _ = worker.socket.shutdown(Shutdown::Both);
        }
        for worker in workers {
            if let Err(error) = join_worker(worker.thread, deadline) {
                outcome = Err(error);
            }
        }
        outcome
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        if self.stop().is_err() {
            eprintln!("CAD-1128 hosted relay cleanup did not complete successfully");
        }
    }
}

fn join_worker(handle: JoinHandle<()>, deadline: Instant) -> Result<(), String> {
    while !handle.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if !handle.is_finished() {
        return Err("hosted relay worker cleanup deadline".to_string());
    }
    handle
        .join()
        .map_err(|_| "hosted relay worker panicked".to_string())
}

fn relay_reply(stream: &mut TcpStream, status: u16, body: &Value) {
    let text = body.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
}

fn serve_relay(
    mut stream: TcpStream,
    password: Arc<Mutex<String>>,
    requests: Arc<AtomicU64>,
    verifies: Arc<AtomicU64>,
    sends: Arc<AtomicU64>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let length = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|value| value.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0);
    while buf.len() < head_end + length {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    requests.fetch_add(1, SeqCst);
    let body: Value =
        serde_json::from_slice(&buf[head_end..head_end + length]).unwrap_or(Value::Null);
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or("")
        .to_string();
    match path.as_str() {
        "/v1/verify" => {
            verifies.fetch_add(1, SeqCst);
        }
        "/v1/send" => {
            sends.fetch_add(1, SeqCst);
        }
        _ => {}
    }
    let server = &body["server"];
    let shape_ok = matches!(
        (server["port"].as_u64(), server["tls_mode"].as_str()),
        (Some(465), Some("implicit")) | (Some(587), Some("starttls"))
    ) && server["host"].as_str().is_some_and(|host| !host.is_empty());
    if !shape_ok || !matches!(path.as_str(), "/v1/verify" | "/v1/send") {
        return relay_reply(
            &mut stream,
            400,
            &json!({"ok": false, "error": {"code": "invalid", "step": "validate", "message": "x"}}),
        );
    }
    let expected = password
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if server["password"].as_str() != Some(expected.as_str()) {
        return relay_reply(
            &mut stream,
            401,
            &json!({"ok": false, "error": {"code": "auth", "step": "auth", "message": "rejected"}}),
        );
    }
    if path == "/v1/verify" {
        return relay_reply(
            &mut stream,
            200,
            &json!({"ok": true, "data": {"verified": true}}),
        );
    }
    // `classify_sent` requires `accepted` to be the ARRAY of accepted
    // recipients (compared case-insensitively with the envelope `to`), not a
    // string: the merged cad1126 relay answers with `envelope.to` itself.
    relay_reply(
        &mut stream,
        200,
        &json!({"ok": true, "data": {
            "accepted": body["envelope"]["to"].clone(),
            "rejected": [],
            "server_reply": "250 2.0.0 OK queued"
        }}),
    )
}

// ---------- real daemon lifecycle on one retained state ----------

struct Daemon {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Daemon {
    fn start(state: &Path, pm: &Path, relay_url: Option<&str>) -> Self {
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.to_str().expect("pm path"));
        let stop = Arc::new(AtomicBool::new(false));
        let mut opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        cadence_agent::platform::smtp::attach(&mut opts);
        if let Some(relay_url) = relay_url {
            opts.smtp_internal = Some(SmtpInternal::new(relay_url).expect("hosted relay URL"));
        }
        let state_path = state.to_path_buf();
        let serve_state = state_path.clone();
        let thread = std::thread::spawn(move || {
            daemon::serve_with(&serve_state, opts).expect("fixture daemon exited with an error");
        });
        let deadline = Instant::now() + WAIT;
        while client::rpc_timeout(
            state_path.as_path(),
            "health",
            json!({}),
            Duration::from_secs(2),
        )
        .is_err()
            || Seam::token_at(&state_path).is_none()
        {
            assert!(Instant::now() < deadline, "fixture daemon never started");
            std::thread::sleep(Duration::from_millis(50));
        }
        Self {
            stop,
            thread: Some(thread),
        }
    }

    fn stop_bounded(&mut self) -> Result<(), &'static str> {
        self.stop.store(true, SeqCst);
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        let deadline = Instant::now() + WAIT;
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !thread.is_finished() {
            return Err("fixture daemon cleanup deadline");
        }
        thread.join().map_err(|_| "fixture daemon thread failed")
    }

    fn stop(&mut self) {
        if let Err(error) = self.stop_bounded() {
            panic!("{error}");
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Bounded like the explicit stop; an assertion failure elsewhere must
        // not hang teardown, and a cleanup failure is never silent.
        if self.stop_bounded().is_err() {
            eprintln!("CAD-1128 hosted daemon cleanup did not complete successfully");
        }
    }
}

fn op(state: &Path, method: &str, params: Value) -> cadence_agent::Result<Value> {
    // EXPLICIT SIMULATED OPERATOR SEAM. No native operator-proof claim.
    let state = state.to_path_buf();
    scoped(Asserted::Operator, || client::rpc(&state, method, params))
}

fn must<T, E>(result: std::result::Result<T, E>, stage: &'static str) -> T {
    result.unwrap_or_else(|_| panic!("{stage}"))
}

// ---------- synthetic key/config provisioning + key-access watch ----------

fn private_dir(path: &Path) {
    std::fs::DirBuilder::new().mode(0o700).create(path).ok();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .expect("private directory mode");
}

/// One 32-byte external key and the fixed path-only config record. Vault
/// starts uncreated, so only an admitted initialize could create it.
fn provision_key_and_config(root: &Path, state: &Path) -> PathBuf {
    let keys = root.join("keys");
    private_dir(&keys);
    let key_path = keys.join("key");
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("synthetic key entropy");
    let mut key = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key_path)
        .expect("key create");
    key.write_all(&bytes).expect("key write");
    key.sync_all().expect("key sync");
    drop(key);
    std::fs::File::open(&keys)
        .expect("key parent")
        .sync_all()
        .expect("key parent sync");

    let record_path = state.join("custody-encrypted.json");
    let record = serde_json::to_vec(&json!({
        "key_file": key_path,
        "vault_dir": state.join("vault"),
    }))
    .expect("config record encode");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&record_path)
        .expect("config record create");
    file.write_all(&record).expect("config record write");
    file.sync_all().expect("config record sync");
    drop(file);
    std::fs::File::open(state)
        .expect("state dir")
        .sync_all()
        .expect("state dir sync");

    for (path, mode) in [
        (state, 0o700),
        (keys.as_path(), 0o700),
        (key_path.as_path(), 0o600),
        (record_path.as_path(), 0o600),
    ] {
        let observed = std::fs::symlink_metadata(path).expect("fixture mode read");
        assert_eq!(
            observed.permissions().mode() & 0o7777,
            mode,
            "fixture mode for {}",
            path.display()
        );
        assert!(observed.is_file() || observed.is_dir());
    }
    key_path
}

/// IN_OPEN|IN_ACCESS watch on the one synthetic key file.
struct KeyWatch(OwnedFd);

impl KeyWatch {
    fn start(key: &Path) -> Self {
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(raw >= 0, "fixture key inotify unavailable");
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let path = CString::new(key.as_os_str().as_bytes()).expect("watch path");
        let watch = unsafe {
            libc::inotify_add_watch(
                fd.as_raw_fd(),
                path.as_ptr(),
                libc::IN_OPEN | libc::IN_ACCESS,
            )
        };
        assert!(watch >= 0, "fixture key watch failed");
        Self(fd)
    }

    fn drain(&self) -> usize {
        let mut count = 0;
        loop {
            let mut buffer = [0u8; 8192];
            let n =
                unsafe { libc::read(self.0.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
            if n < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().kind(),
                    std::io::ErrorKind::WouldBlock
                );
                return count;
            }
            assert!(n > 0);
            let mut at = 0;
            while at + std::mem::size_of::<libc::inotify_event>() <= n as usize {
                let event = unsafe {
                    std::ptr::read_unaligned(buffer.as_ptr().add(at).cast::<libc::inotify_event>())
                };
                assert_eq!(
                    event.mask & (libc::IN_Q_OVERFLOW | libc::IN_IGNORED),
                    0,
                    "key observation lost events/watch"
                );
                count += 1;
                at += std::mem::size_of::<libc::inotify_event>() + event.len as usize;
            }
        }
    }
}

/// Vault inventory for the preserve/refuse oracles: relative path, byte
/// length and the SHA-256 of the exact contents. A genuinely absent root is
/// the ONLY tolerated absence; every other inventory/entry/stat/read error
/// PANICS rather than proving "no mutation" from an empty tree.
fn tree(dir: &Path) -> Vec<(String, u64, String)> {
    fn open_dir(dir: &Path) -> std::fs::ReadDir {
        std::fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("vault inventory of {} failed: {error}", dir.display()))
    }
    fn walk(base: &Path, entries: std::fs::ReadDir, out: &mut Vec<(String, u64, String)>) {
        for entry in entries {
            let entry =
                entry.unwrap_or_else(|error| panic!("vault inventory entry unreadable: {error}"));
            let path = entry.path();
            let meta = path.symlink_metadata().unwrap_or_else(|error| {
                panic!("vault member {} uninspectable: {error}", path.display())
            });
            if meta.is_dir() {
                walk(base, open_dir(&path), out);
            } else {
                let bytes = std::fs::read(&path).unwrap_or_else(|error| {
                    panic!("vault member {} unreadable: {error}", path.display())
                });
                out.push((
                    path.strip_prefix(base)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned(),
                    meta.len(),
                    format!("{:x}", Sha256::digest(&bytes)),
                ));
            }
        }
    }
    let mut out = Vec::new();
    match std::fs::read_dir(dir) {
        Ok(root) => walk(dir, root, &mut out),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("vault inventory of {} failed: {error}", dir.display()),
    }
    out.sort();
    out
}

fn hosted_smtp(account: &str, secret: &str) -> Value {
    json!({
        "provider": "smtp", "account": account, "shape": "smtp",
        "host": "smtp.gmail.com", "port": 465, "tls_mode": "implicit",
        "username": SENDER, "secret": secret, "sender": SENDER,
        "sender_name": "Restaurant", "scopes": ["email:send"],
        "accept_same_uid_risk": true,
    })
}

const RECIPIENT: &str = "operator@example.com";

fn blocks() -> Value {
    json!([
        {"type": "heading", "text": "Hello {{first_name|friend}}"},
        {"type": "paragraph", "text": "A calm first line."},
        {"type": "button", "label": "Read more", "url": "https://example.com/posts/welcome"},
    ])
}

/// Local copy of the reviewed blog-post app plus a minimal action screen that
/// declares `smtp.bind` (v2 + records host contract; the slot operation needs
/// `records.slot`). Test-only bundle; never the shared fixture bytes.
fn app_source(root: &Path) -> PathBuf {
    let source = root.join("source");
    for name in [
        "app.md",
        "workflows/blog-post.md",
        "rubrics/blog.md",
        "templates/brief.md",
        "templates/post.md",
    ] {
        let to = source.join(name);
        std::fs::create_dir_all(to.parent().expect("app member parent")).expect("app member dir");
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/apps/blog-post")
                .join(name),
            &to,
        )
        .expect("app member copy");
    }
    let screen = source.join("screens/main");
    std::fs::create_dir_all(&screen).expect("screen dir");
    let js = "(()=>{})();";
    std::fs::write(screen.join("client.js"), js).expect("screen js");
    let sha = |bytes: &[u8]| format!("sha256:{:x}", Sha256::digest(bytes));
    let declaration = json!({
        "contract": "app-screens/v2", "app": "blog-post", "entry": "client.js",
        "host_contract": "screen-records.v1", "may": ["records.call", "records.slot"],
        "operations": ["smtp.bind"], "tools": {},
        "assets": [{"name": "client.js", "media_type": "text/javascript",
            "size": js.len(), "sha256": sha(js.as_bytes())}],
        "provenance": {
            "source_digest": sha(b"c1128-hosted-source"),
            "sdk_digest": sha(b"c1128-hosted-sdk"),
            "toolchain_digest": sha(b"c1128-hosted-toolchain"),
        },
    });
    std::fs::write(screen.join("screens.json"), declaration.to_string()).expect("screen decl");
    source
}

/// Install the local bundle and prepare approved campaign content, returning
/// `(install_id, context_id)`. This is the working app fixture the CRM SMTP
/// control uses; no production or shared-fixture bytes are involved.
fn install_and_campaign(state: &Path, root: &Path) -> (String, String) {
    let source = app_source(root);
    let installed = must(
        op(
            state,
            "app_workspace_install",
            json!({"source": source.to_str().expect("app source path")}),
        ),
        "hosted fixture app install failed",
    );
    let install = installed["install_id"]
        .as_str()
        .expect("install id")
        .to_string();
    let context = must(
        op(
            state,
            "app_context_create",
            json!({"install_id": install, "label": "brand",
                "input_defaults": {}, "request_id": "ctx-1"}),
        ),
        "hosted fixture context failed",
    );
    let context_id = context["context"]["id"]
        .as_str()
        .expect("context id")
        .to_string();
    must(
        op(
            state,
            "app_content_save",
            json!({"install_id": install, "context_id": context_id,
                "campaign_id": "launch-1", "subject": "Spring launch",
                "preheader": "News", "blocks": blocks()}),
        ),
        "hosted fixture content save failed",
    );
    let revision = must(
        op(
            state,
            "app_content_show",
            json!({"install_id": install, "context_id": context_id,
                "campaign_id": "launch-1"}),
        ),
        "hosted fixture content show failed",
    )["content"]["revision"]
        .as_u64()
        .expect("content revision");
    must(
        op(
            state,
            "app_content_approve",
            json!({"install_id": install, "context_id": context_id,
                "campaign_id": "launch-1", "expected_revision": revision}),
        ),
        "hosted fixture content approval failed",
    );
    (install, context_id)
}

#[test]
fn hosted_relay_admits_legacy_rotation_but_refuses_encrypted_custody_use() {
    let password_a = format!(
        "pw-a-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let password_b = format!(
        "pw-b-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let mut relay = Relay::start(&password_a);
    let root = tempfile::Builder::new()
        .prefix("c1128h")
        .tempdir()
        .expect("fixture root");
    let pm = root.path().join("pm");
    cadence_agent::issue::Pm::init(&pm).expect("pm init");
    let state = root.path().join("s");

    let encrypted_secret = format!(
        "enc-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );

    // Phase 0: no encrypted custody yet. The hosted working app binds a LEGACY
    // plaintext SMTP record and sends through the real relay; the legacy
    // authority must not be mutated and the hosted path must not dial SMTP
    // directly.
    let mut daemon = Daemon::start(&state, &pm, Some(&relay.url));
    let (install, context) = install_and_campaign(&state, root.path());
    let created = must(
        op(
            &state,
            "connection_create",
            hosted_smtp("owner", &password_a),
        ),
        "legacy hosted SMTP create failed",
    );
    let id = created["connection"]["id"]
        .as_str()
        .expect("connection id")
        .to_string();
    assert_eq!(
        relay.verifies(),
        1,
        "create must verify exactly once through the relay"
    );
    assert_eq!(relay.sends(), 0, "create must not send through the relay");
    let bound = must(
        op(
            &state,
            "crm_smtp_bind",
            json!({"install_id": &install, "context_id": &context,
                "connection_id": &id, "request_id": "legacy-bind"}),
        ),
        "legacy hosted CRM SMTP bind failed",
    );
    assert_eq!(
        bound["binding"]["connection_id"].as_str(),
        Some(id.as_str())
    );
    assert_eq!(bound["binding"]["auth_revision"], 1);
    let link_revision = bound["binding"]["link_revision"]
        .as_i64()
        .expect("live link revision");
    let dials_before = cadence_agent::platform::smtp::direct_dial_count();
    let sent = must(
        op(
            &state,
            "crm_smtp_test_send",
            json!({"install_id": &install, "context_id": &context,
                "campaign_id": "launch-1", "to_email": RECIPIENT}),
        ),
        "legacy hosted test send failed",
    );
    assert_eq!(
        sent["receipt"]["accepted"], true,
        "the real relay did not accept the legacy test send"
    );
    assert_eq!(relay.verifies(), 1);
    assert_eq!(
        relay.sends(),
        1,
        "the legacy test send must deliver once through the relay"
    );
    assert_eq!(
        cadence_agent::platform::smtp::direct_dial_count(),
        dials_before,
        "the hosted legacy send dialled SMTP directly"
    );
    daemon.stop();

    let store = Store::open(&state.join("cadence.sqlite3")).expect("core store");
    let legacy = store
        .platform_credential("smtp", "owner")
        .expect("core read")
        .expect("legacy record");
    assert!(
        !legacy.custody.starts_with("encrypted-sqlite-v1:"),
        "phase-1 enrollment unexpectedly landed in encrypted custody"
    );
    assert_eq!(legacy.credential_revision, 1);
    assert_eq!(legacy.exchange, "smtp");
    let legacy_tag = legacy.custody.clone();
    drop(store);

    // Provision the encrypted config while the daemon is stopped. The state
    // must be owner-private for the strict config loader.
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).expect("state mode");
    let key_path = provision_key_and_config(root.path(), &state);
    let key_before = std::fs::read(&key_path).expect("key bytes");
    relay.set_password(&password_b);
    assert!(
        !state.join("vault").join("custody.sqlite3").exists(),
        "the vault database existed before explicit initialization"
    );

    // Phase 1: self-hosted daemon (no hosted relay at all) initializes the
    // vault, enrolls an encrypted SMTP record and retargets the live CRM link
    // onto it. This phase is admitted; later phases must not exercise it.
    let mut daemon = Daemon::start(&state, &pm, None);
    let status = must(
        op(&state, "connection_custody_status", json!({})),
        "self-hosted custody status failed",
    );
    assert_eq!(status["custody"]["configured"], true);
    assert_eq!(status["custody"]["unlocked"], false);
    must(
        op(
            &state,
            "connection_custody_initialize",
            json!({"accept_same_uid_risk": true}),
        ),
        "self-hosted vault initialization failed",
    );
    must(
        op(
            &state,
            "connection_custody_unlock",
            json!({"accept_same_uid_risk": true}),
        ),
        "self-hosted vault unlock failed",
    );
    let enc_created = must(
        op(
            &state,
            "connection_create",
            hosted_smtp("enc", &encrypted_secret),
        ),
        "self-hosted encrypted SMTP create failed",
    );
    let enc_id = enc_created["connection"]["id"]
        .as_str()
        .expect("encrypted connection id")
        .to_string();
    let store = Store::open(&state.join("cadence.sqlite3")).expect("core store");
    let enc_record = store
        .platform_credential("smtp", "enc")
        .expect("core read")
        .expect("encrypted record");
    assert!(
        enc_record.custody.starts_with("encrypted-sqlite-v1:"),
        "self-hosted enrollment did not land in encrypted custody"
    );
    assert_eq!(enc_record.credential_revision, 1);
    assert_eq!(enc_record.exchange, "smtp");
    let enc_before = enc_record.to_json();
    assert_eq!(store.platform_credentials().expect("core read").len(), 2);
    drop(store);
    let relay_total = relay.count();
    let rebound = must(
        op(
            &state,
            "crm_smtp_rebind",
            json!({"install_id": &install, "context_id": &context,
                "connection_id": &enc_id, "expected_revision": link_revision}),
        ),
        "self-hosted CRM SMTP retarget onto encrypted custody failed",
    );
    assert_eq!(
        rebound["binding"]["connection_id"].as_str(),
        Some(enc_id.as_str())
    );
    assert_eq!(rebound["binding"]["auth_revision"], 1);
    assert_eq!(
        relay.count(),
        relay_total,
        "self-hosted enrollment or retarget contacted the hosted relay"
    );
    daemon.stop();

    // Phase 2: same self-hosted state, relay restored, vault locked.
    let mut daemon = Daemon::start(&state, &pm, Some(&relay.url));
    let status = must(
        op(&state, "connection_custody_status", json!({})),
        "custody status failed",
    );
    assert_eq!(status["custody"]["configured"], true);
    assert_eq!(status["custody"]["unlocked"], false);
    // `fenced` is a manager lifecycle detail, not the locked-startup contract;
    // startup must not be re-pinned to an incidental default.
    let vault_before = tree(&state.join("vault"));
    let (_, db_len, _) = vault_before
        .iter()
        .find(|(path, _, _)| path == "custody.sqlite3")
        .expect("self-hosted initialization created no vault database");
    assert!(
        *db_len > 0,
        "self-hosted encrypted enrollment wrote no vault ciphertext"
    );

    let watch = KeyWatch::start(&key_path);
    assert!(
        std::fs::read(&key_path).is_ok_and(|bytes| bytes == key_before),
        "fixture key calibration read"
    );
    assert!(watch.drain() > 0, "key watch calibration saw no access");

    // Eligible LEGACY rotation under the configured vault + hosted relay:
    // secret-only, exact expectations, inherited transport/sender, and the
    // hosted verify must run with the new secret.
    let rotated = must(
        op(
            &state,
            "connection_rotate",
            json!({"connection_id": id, "expected_connection_id": id,
                "expected_revision": 1, "secret": &password_b}),
        ),
        "legacy secret rotation under hosted + configured vault failed",
    );
    assert_eq!(rotated["connection"]["id"], id);
    assert_eq!(
        relay.verifies(),
        2,
        "legacy rotation must verify exactly once through the hosted relay"
    );
    assert_eq!(relay.sends(), 1, "legacy rotation must not send");
    assert_eq!(
        relay.count(),
        3,
        "only enrollment, the admitted send and rotation may reach the relay"
    );
    assert_eq!(watch.drain(), 0, "legacy rotation read the encrypted key");

    let store = Store::open(&state.join("cadence.sqlite3")).expect("core store");
    let rotated_record = store
        .platform_credential("smtp", "owner")
        .expect("core read")
        .expect("rotated record");
    assert_eq!(rotated_record.credential_revision, 2);
    assert_eq!(
        rotated_record.custody, legacy_tag,
        "legacy rotation changed the custody backend"
    );
    assert_eq!(rotated_record.exchange, "smtp");
    let authority_before = rotated_record.to_json();
    assert_eq!(store.platform_credentials().expect("core read").len(), 2);
    assert_eq!(
        store
            .platform_credential("smtp", "enc")
            .expect("core read")
            .expect("encrypted record")
            .to_json(),
        enc_before,
        "legacy rotation changed the encrypted record"
    );
    drop(store);

    // Encrypted initialize / unlock / new enrollment / rotation, and a
    // restored encrypted send, must refuse BEFORE key reads, relay traffic,
    // direct dialing, or core/vault mutation.
    let relay_before = relay.count();
    let dials_refusals = cadence_agent::platform::smtp::direct_dial_count();
    let refusals: [(&str, Value, &str); 4] = [
        (
            "connection_custody_initialize",
            json!({"accept_same_uid_risk":true}),
            "hosted encrypted initialize",
        ),
        (
            "connection_custody_unlock",
            json!({"accept_same_uid_risk":true}),
            "hosted encrypted unlock",
        ),
        (
            "connection_create",
            hosted_smtp("second", &password_b),
            "hosted encrypted create",
        ),
        (
            "connection_rotate",
            json!({"connection_id": &enc_id, "expected_connection_id": &enc_id,
                "expected_revision": 1, "secret": &password_b}),
            "hosted encrypted rotate",
        ),
    ];
    for (method, params, stage) in refusals {
        let error =
            op(&state, method, params).expect_err("hosted encrypted custody use must refuse");
        assert_eq!(
            error.code(),
            Some("unsupported_custody_use"),
            "{stage} did not refuse unsupported_custody_use: {error}"
        );
    }
    // The restored link now names the encrypted record: the real hosted
    // consumer must refuse before any relay, direct dial or key read.
    let send_error = op(
        &state,
        "crm_smtp_test_send",
        json!({"install_id": &install, "context_id": &context,
            "campaign_id": "launch-1", "to_email": RECIPIENT}),
    )
    .expect_err("the restored encrypted sender must not send on a hosted daemon");
    assert!(
        send_error.code() == Some("unsupported_custody_use")
            || send_error
                .to_string()
                .contains("reviewed registration or custody is unavailable"),
        "the restored encrypted send refused for an unexpected reason: {send_error}"
    );
    assert_eq!(
        relay.count(),
        relay_before,
        "a refusal ran hosted verification or delivery"
    );
    assert_eq!(
        cadence_agent::platform::smtp::direct_dial_count(),
        dials_refusals,
        "a refusal dialled SMTP directly"
    );
    assert_eq!(watch.drain(), 0, "a refusal read the encrypted key");

    let store = Store::open(&state.join("cadence.sqlite3")).expect("core store");
    assert_eq!(store.platform_credentials().expect("core read").len(), 2);
    assert_eq!(
        store
            .platform_credential("smtp", "owner")
            .expect("core read")
            .expect("record")
            .to_json(),
        authority_before,
        "a refused encrypted verb changed core authority"
    );
    assert_eq!(
        store
            .platform_credential("smtp", "enc")
            .expect("core read")
            .expect("encrypted record")
            .to_json(),
        enc_before,
        "a refused encrypted verb changed the encrypted record"
    );
    drop(store);
    assert_eq!(
        tree(&state.join("vault")),
        vault_before,
        "a refused encrypted verb mutated the vault"
    );

    // The operator endpoint refuses a supplied reason on encrypted custody;
    // no key read, no store write, no relay traffic.
    let reason_error = op(
        &state,
        "platform_revoke",
        json!({"platform": "smtp", "account": "enc",
            "expected_connection_id": &enc_id, "expected_revision": 1,
            "reason": "synthetic operator reason"}),
    )
    .expect_err("a supplied encrypted revoke reason must refuse");
    assert_eq!(
        reason_error.code(),
        Some("reason_unscreenable"),
        "supplied encrypted revoke reason was not refused: {reason_error}"
    );
    assert_eq!(
        watch.drain(),
        0,
        "the reason refusal read the encrypted key"
    );
    assert_eq!(relay.count(), relay_before);
    let store = Store::open(&state.join("cadence.sqlite3")).expect("core store");
    assert_eq!(
        store
            .platform_credential("smtp", "enc")
            .expect("core read")
            .expect("encrypted record")
            .to_json(),
        enc_before,
        "the reason refusal changed the encrypted record"
    );
    drop(store);

    // Fixed-cause encrypted revoke: exact CAS succeeds while the hosted
    // refusal stays closed. Removing the core record is admitted; the retained
    // ciphertext inventory must stay byte-identical without the key.
    let revoked = must(
        op(
            &state,
            "connection_revoke",
            json!({"connection_id": &enc_id,
                "expected_connection_id": &enc_id, "expected_revision": 1}),
        ),
        "fixed-cause encrypted revoke failed",
    );
    assert_eq!(revoked["connection_id"].as_str(), Some(enc_id.as_str()));
    assert_eq!(revoked["revoked"], true);
    assert_eq!(watch.drain(), 0, "revoke read the encrypted key");
    assert_eq!(
        relay.count(),
        relay_before,
        "revoke contacted the hosted relay"
    );
    let store = Store::open(&state.join("cadence.sqlite3")).expect("core store");
    assert!(
        store
            .platform_credential("smtp", "enc")
            .expect("core read")
            .is_none(),
        "revoke left the encrypted record"
    );
    assert_eq!(
        store
            .platform_credential("smtp", "owner")
            .expect("core read")
            .expect("record")
            .to_json(),
        authority_before,
        "revoke changed core authority"
    );
    assert_eq!(store.platform_credentials().expect("core read").len(), 1);
    drop(store);
    assert_eq!(
        tree(&state.join("vault")),
        vault_before,
        "revoke mutated the retained authenticated ciphertext inventory"
    );
    let status = must(
        op(&state, "connection_custody_status", json!({})),
        "final custody status failed",
    );
    assert_eq!(status["custody"]["configured"], true);
    assert_eq!(status["custody"]["unlocked"], false);

    // Final operation drain: the custody-status read above must not touch the
    // key either. Then retire the watcher and perform the fixture-owned
    // integrity read OUTSIDE every operation's no-access window (the fixture
    // read is not daemon access and must not contaminate a later drain).
    assert_eq!(
        watch.drain(),
        0,
        "final custody status read the encrypted key"
    );
    drop(watch);
    assert_eq!(
        std::fs::read(&key_path).expect("key bytes"),
        key_before,
        "fixture key bytes changed"
    );

    daemon.stop();
    relay.stop().expect("hosted relay cleanup");
}
