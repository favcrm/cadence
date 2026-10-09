//! CAD-1315 acceptance check, written by the Spec/security reviewer from the
//! ticket ("a reviewer-written check: Retry cannot double-charge the same
//! key; an agent or container caller cannot settle or forge a job result").
//! The implementer may not edit or weaken it.
//!
//! Everything goes through the daemon's public RPCs against the AgenticOS
//! media door in its real job shape (`tests/fixtures/aos_media_door.rs`).
//! A charge is a job AgenticOS creates: AgenticOS reserves once per caller
//! key and answers a replay of the same key and body with the existing job.
//! So "never a second charge" is measured as: one distinct idempotency key per
//! image intent, one AgenticOS job, and every submit body byte-identical (a
//! changed body under the same key would be `key_conflict`, a different key a
//! second reserve). A plain POST count is not the measure: re-checking the
//! same key is allowed and free.
//!
//! (a) Retry: the same request id (repeated, and fired concurrently), another
//!     request id while the job is pending or unresolved, a daemon restart
//!     mid-job, and a replay after completion never create a second key/job.
//! (b) Forgery: an agent or unproven caller relaying the operator's real
//!     session cannot invoke, re-check or read a job; another draft cannot
//!     take over a job's request id, and a finished job's receipt never
//!     settles another draft's intent.
#![cfg(feature = "test-seam")]

use cadence_agent::platform::deployments::DeploymentMetadata;
use cadence_agent::store::app_records::RecordStore;
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[path = "fixtures/aos_media_door.rs"]
mod aos_media_door;
use aos_media_door::Scenario;

const HOSTED: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"}]}"#;
const HANDLE: &str = "juicysuite_crm";
const CAPTION: &str = "Image job caption";
const DEST: &str = "17841400008460056";

#[derive(Default)]
struct Door {
    media: aos_media_door::MediaDoor,
    /// Every image submit: (idempotency key, exact body bytes).
    submits: Mutex<Vec<(String, Vec<u8>)>>,
}

fn fake_door() -> (String, Arc<Door>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let state = Arc::new(Door::default());
    let door = state.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let door = door.clone();
            std::thread::spawn(move || serve(stream, &door));
        }
    });
    (addr, state)
}

fn serve(stream: std::net::TcpStream, door: &Door) {
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    let head = |reader: &mut BufReader<_>| -> Vec<String> {
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return lines;
            }
            let line = line.trim_end().to_string();
            if line.is_empty() {
                return lines;
            }
            lines.push(line);
        }
    };
    let connect = head(&mut reader);
    if !connect
        .first()
        .is_some_and(|l| l.starts_with("CONNECT api.internal:80 "))
    {
        return;
    }
    writer
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .unwrap();
    let request = head(&mut reader);
    let Some(first) = request.first() else {
        return;
    };
    let mut parts = first.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let header = |name: &str| -> Option<String> {
        request.iter().skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    };
    let length: usize = header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let sent: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let route = path.split_once('?').map_or(path.as_str(), |(r, _)| r);
    let raw = |status: u16, content_type: &str, bytes: &[u8]| {
        let mut out = format!(
            "HTTP/1.1 {status} OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            bytes.len()
        )
        .into_bytes();
        out.extend_from_slice(bytes);
        out
    };
    let json_reply = |value: Value| raw(200, "application/json", value.to_string().as_bytes());
    let price = json!({"currency":"USD","scale":6,"amount":"0.002000"});
    let reply = match (method.as_str(), route) {
        ("GET", "/v1/runtime/tools/read_instagram_posts") => json_reply(
            json!({"ok":true,"data":{"slug":"read_instagram_posts","displayName":"Read Instagram posts","effect":"read","chargePrecondition":"max_charge_minor@1","price":price,"unitPrice":null}}),
        ),
        ("POST", "/v1/runtime/tools/call") if sent["slug"] == "read_instagram_posts" => json_reply(
            json!({"ok":true,"data":{"slug":"read_instagram_posts","repeated":false,"price":price,"result":{"success":true,"status":"ok","user":{"username":HANDLE,"is_private":false},"items":[{"id":"post-1","code":"AbCd123","created_at":"2026-09-27T00:00:00Z","caption":{"text":"Door caption"}}]}}}),
        ),
        (m, r) if r.starts_with("/v1/runtime/media/") => {
            let key = header("idempotency-key");
            if m == "POST" && r == "/v1/runtime/media/image" {
                door.submits
                    .lock()
                    .unwrap()
                    .push((key.clone().unwrap_or_default(), body.clone()));
            }
            match door.media.reply(m, r, key.as_deref(), &body) {
                Some((status, content_type, bytes)) => raw(status, content_type, &bytes),
                None => raw(404, "application/json", b"{}"),
            }
        }
        _ => json_reply(json!({"ok":false,"error":{"code":"not_found","message":"no route"}})),
    };
    let _ = writer.write_all(&reply);
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// `ALL_PROXY` is process-global: one daemon fixture at a time.
static SERIAL: Mutex<()> = Mutex::new(());

struct Fx {
    _serial: std::sync::MutexGuard<'static, ()>,
    root: tempfile::TempDir,
    daemon: Option<(
        Arc<AtomicBool>,
        std::thread::JoinHandle<cadence_agent::Result<()>>,
    )>,
    door: Arc<Door>,
    door_addr: String,
    install: String,
    context: String,
    session: Value,
    action_token: Value,
}

impl Fx {
    fn dir(&self) -> std::path::PathBuf {
        self.root.path().join("s")
    }

    fn rpc_as(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(who, || client::rpc(&self.dir(), method, params))
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc_as(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }

    /// A screen action carrying the operator's real session credentials and
    /// mount action token, asserted as `who`.
    fn screen_as(
        &self,
        who: Asserted,
        method: &str,
        mut params: Value,
    ) -> cadence_agent::Result<Value> {
        params["action_token"] = self.action_token.clone();
        params["token"] = self.session["token"].clone();
        params["key"] = self.session["key"].clone();
        params["origin"] = json!("loopback");
        self.rpc_as(who, method, params)
    }

    fn screen(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        self.screen_as(Asserted::Operator, method, params)
    }

    fn start() -> Self {
        for name in [
            "CADENCE_PUBLISH_SEND_URL",
            "CADENCE_PUBLISH_SEND_CREDENTIAL_FILE",
            "CADENCE_PUBLISH_READ_URL",
            "CADENCE_PUBLISH_READ_CREDENTIAL_FILE",
            "CADENCE_AGENTICOS_EXTERNAL_URL",
            "NO_PROXY",
            "no_proxy",
        ] {
            std::env::remove_var(name);
        }
        let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let root = tempfile::Builder::new().prefix("c1315").tempdir().unwrap();
        cadence_agent::issue::Pm::init(&root.path().join("pm")).unwrap();
        std::fs::create_dir_all(root.path().join("s")).unwrap();
        let (door_addr, door) = fake_door();
        let mut fx = Self {
            _serial: serial,
            root,
            daemon: None,
            door,
            door_addr,
            install: String::new(),
            context: String::new(),
            session: Value::Null,
            action_token: Value::Null,
        };
        fx.launch();
        fx.setup();
        fx
    }

    fn launch(&mut self) {
        let (dir, stop) = (self.dir(), Arc::new(AtomicBool::new(false)));
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set(
            "CADENCE_PM_DIR",
            self.root.path().join("pm").to_str().unwrap(),
        );
        let mut opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(Arc::clone(&stop)),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            provider_deployments: Some(DeploymentMetadata::parse(HOSTED.as_bytes()).unwrap()),
            image_job_window_ms: 0,
            image_job_backoff_ms: 100,
            ..Default::default()
        };
        cadence_agent::platform::local::register_at(
            &dir,
            &mut opts,
            dir.join("outbox"),
            "http://127.0.0.1:3010".into(),
        );
        std::env::set_var("ALL_PROXY", format!("http://{}", self.door_addr));
        std::env::set_var("CADENCE_TEST_MEDIA_POLL_MS", "20");
        std::env::set_var("CADENCE_TEST_MEDIA_DEADLINE_MS", "1500");
        let handle = std::thread::spawn(move || daemon::serve_with(&dir, opts));
        let deadline = Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&self.dir(), "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&self.dir()).is_none()
        {
            assert!(
                !handle.is_finished() && Instant::now() < deadline,
                "daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        std::env::remove_var("ALL_PROXY");
        self.daemon = Some((stop, handle));
    }

    fn stop_daemon(&mut self) {
        if let Some((stop, handle)) = self.daemon.take() {
            stop.store(true, SeqCst);
            let _ = handle.join();
        }
    }

    fn setup(&mut self) {
        let app = self.root.path().join("app");
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        copy_dir(
            &std::path::Path::new(manifest_dir).join("workspace-apps/social-content"),
            &app,
        );
        copy_dir(
            &std::path::Path::new(manifest_dir)
                .join("tests/fixtures/apps/ig-tools-fixture/screens"),
            &app.join("screens"),
        );
        let screens = app.join("screens/feed/screens.json");
        let text = std::fs::read_to_string(&screens).unwrap();
        let text = text.replace("ig-tools-fixture", "social-content").replace(
            r#""tools": { "instagram.read": "source" }"#,
            r#""tools": { "instagram.read": "source", "social.draft": "image" }"#,
        );
        assert!(text.contains("social.draft"), "screen tools edit applied");
        std::fs::write(&screens, text).unwrap();
        let installed = self.op(
            "app_workspace_install",
            json!({"source": app.to_str().unwrap()}),
        );
        let install = installed["install_id"].as_str().unwrap().to_string();
        self.op(
            "app_local_install_approve",
            json!({"install_id": install, "digest": installed["digest"]}),
        );
        let connections = self.op("connection_list", json!({}))["connections"].clone();
        let find = |account: &str| {
            connections
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["account"] == account)
                .unwrap_or_else(|| panic!("no {account} connection"))["id"]
                .clone()
        };
        let context = self.op(
            "app_context_create",
            json!({"install_id": install, "label": "EF", "input_defaults": {},
                "request_id": "ctx-1315"}),
        );
        let context = context["context"]["id"]
            .as_str()
            .or_else(|| context["id"].as_str())
            .unwrap()
            .to_string();
        for (slot, connection) in [
            ("source", find("hosted")),
            ("image", find("hosted")),
            ("publication", find("local")),
        ] {
            let bound = self.op(
                "app_binding_create",
                json!({"install_id": install, "context_id": context, "slot": slot,
                    "connection_id": connection, "request_id": format!("bind-{slot}")}),
            );
            if slot == "publication" {
                self.op(
                    "app_binding_publish_set",
                    json!({"install_id": install, "binding_id": bound["binding"]["id"],
                        "expected_revision": bound["binding"]["revision"],
                        "destination_id": DEST, "destination_label": "Harbour stills",
                        "toolkit": "instagram", "timezone": "Asia/Hong_Kong",
                        "grant_id": "dpq_typed_on_binding"}),
                );
            }
        }
        RecordStore::open(&self.dir(), &install)
            .unwrap()
            .app_social_sources_save(&context, 0, &[HANDLE.to_owned()], "src-1315")
            .unwrap();
        self.install = install;
        self.context = context;
        self.mount();
    }

    /// Open an operator session and mount the screen (again after a restart:
    /// sessions and action tokens do not survive one).
    fn mount(&mut self) {
        let (install, context) = (self.install.clone(), self.context.clone());
        cadence_agent::operator_auth::ensure_secret(&self.dir()).unwrap();
        let secret = cadence_agent::operator_auth::read_secret(&self.dir()).unwrap();
        let nonce = self.op(
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )["nonce"]
            .clone();
        self.session = self.op(
            "operator_session_open",
            json!({"nonce": nonce, "origin": "loopback"}),
        );
        let mint = self.op(
            "app_screen_mint",
            json!({"install_id": install, "context_id": context, "tag": "feed",
                "token": self.session["token"], "key": self.session["key"],
                "origin": "loopback", "generation": 1}),
        );
        let mount = mint["mount"].as_str().unwrap();
        self.op(
            "app_screen_consume",
            json!({"nonce": mount.rsplit('/').next().unwrap()}),
        );
        self.action_token = mint["action_token"].clone();
    }

    fn fetch(&self) -> (String, String) {
        let fetched = self
            .screen(
                "app_tool_invoke",
                json!({"tool_alias": "instagram.read", "input": {"handle": HANDLE},
                    "request_id": "fetch-1315"}),
            )
            .unwrap_or_else(|e| panic!("fetch: {e}"));
        let receipt = fetched["receipt"]["id"].as_str().unwrap().to_string();
        let post = fetched["receipt"]["result"]["posts"][0]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("no normalized post: {fetched}"))
            .to_string();
        (receipt, post)
    }

    fn new_draft(&self, tag: &str, source: &(String, String)) -> String {
        let created = self
            .screen(
                "app_social_draft_create",
                json!({"tool_alias": "social.draft", "request_id": format!("draft-{tag}"),
                    "caption": CAPTION,
                    "source": {"kind": "tool_receipt", "receipt_id": source.0, "post_id": source.1}}),
            )
            .unwrap_or_else(|e| panic!("draft create: {e}"));
        created["draft_id"].as_str().unwrap().to_string()
    }

    fn image_params(draft: &str, request: &str) -> Value {
        json!({"tool_alias": "social.draft", "input": {}, "request_id": request,
            "generation_scope": {"operation": "image", "draft_id": draft, "revision": 1}})
    }

    fn image_as(&self, who: Asserted, draft: &str, request: &str) -> cadence_agent::Result<Value> {
        self.screen_as(who, "app_tool_invoke", Self::image_params(draft, request))
    }

    fn image(&self, draft: &str, request: &str) -> cadence_agent::Result<Value> {
        self.image_as(Asserted::Operator, draft, request)
    }

    /// Every image intent of one draft, read from the record file.
    fn intents(&self, draft: &str) -> Vec<Value> {
        RecordStore::open(&self.dir(), &self.install)
            .unwrap()
            .app_social_generation_intents(&self.context)
            .unwrap()
            .into_iter()
            .filter(|row| row["scope"]["draft_id"] == draft && row["scope"]["operation"] == "image")
            .collect()
    }

    fn intent(&self, draft: &str) -> Value {
        let rows = self.intents(draft);
        assert_eq!(rows.len(), 1, "one image intent per draft: {rows:?}");
        rows.into_iter().next().unwrap()
    }

    fn wait_intent(&self, draft: &str, state: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            let row = self.intent(draft);
            if row["state"] == state {
                return row;
            }
            assert!(
                Instant::now() < deadline,
                "intent never reached {state}: {row}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_door(&self, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !self.door.media.log().iter().any(|l| l.starts_with(what)) {
            assert!(Instant::now() < deadline, "door never saw {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The charge invariant: one key, one AgenticOS job, one exact body per
    /// key, across every submit the door has seen so far.
    fn assert_one_charge_per_key(&self, keys: usize, why: &str) {
        let submits = self.door.submits.lock().unwrap().clone();
        let distinct: BTreeSet<&String> = submits.iter().map(|(k, _)| k).collect();
        assert_eq!(distinct.len(), keys, "{why}: distinct keys {distinct:?}");
        assert_eq!(self.door.media.job_count(), keys, "{why}: AgenticOS jobs");
        for (key, body) in &submits {
            let first = &submits.iter().find(|(k, _)| k == key).unwrap().1;
            assert_eq!(
                body, first,
                "{why}: key {key} was replayed with a different body"
            );
        }
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop_daemon();
    }
}

/// (a) No retry path creates a second key or a second AgenticOS job.
#[test]
fn retry_never_charges_the_same_image_intent_twice() {
    let mut fx = Fx::start();
    let source = fx.fetch();

    // 1. The first invoke for one request id, fired by several screens at
    //    once, starts exactly one job.
    fx.door.media.set(Scenario::Hold);
    let held = fx.new_draft("held", &source);
    std::thread::scope(|scope| {
        let calls: Vec<_> = (0..6)
            .map(|_| scope.spawn(|| fx.image(&held, "img-held")))
            .collect();
        for call in calls {
            let reply = call
                .join()
                .unwrap()
                .unwrap_or_else(|e| panic!("invoke: {e}"));
            assert_eq!(reply["generation_intent"]["state"], "pending", "{reply}");
        }
    });
    fx.wait_door("GET job");
    fx.assert_one_charge_per_key(1, "concurrent first invokes");

    // 2. While the job runs: the same request id again, and other request ids,
    //    neither mint a key nor create a job. Another request id is answered
    //    with the existing intent and never reaches the door.
    for n in 0..3 {
        let same = fx.image(&held, "img-held").unwrap();
        assert_eq!(same["generation_intent"]["state"], "pending", "{same}");
        let submits = fx.door.submits.lock().unwrap().len();
        let other = fx.image(&held, &format!("img-held-other-{n}")).unwrap();
        assert_eq!(other["generation_intent"]["request_id"], "img-held");
        assert_eq!(other["generation_intent"]["state"], "pending");
        assert_eq!(
            fx.door.submits.lock().unwrap().len(),
            submits,
            "another request id reached the door"
        );
    }
    fx.assert_one_charge_per_key(1, "retries while pending");

    // 3. The daemon dies mid-job; the next boot settles it under the SAME key
    //    with the same body. (The in-process worker of the stopped daemon ends
    //    its current attempt first; a real process would just be gone.)
    fx.stop_daemon();
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(fx.intent(&held)["state"], "pending");
    fx.door.media.set(Scenario::Normal);
    fx.launch();
    fx.mount();
    let done = fx.wait_intent(&held, "completed");
    assert!(done["receipt_id"].is_string(), "{done}");
    fx.assert_one_charge_per_key(1, "restart mid-job");

    // 4. A replay after completion touches no media door at all and settles
    //    nothing new.
    let traffic = fx.door.media.log().len();
    let replay = fx.image(&held, "img-held").unwrap();
    assert_eq!(replay["receipt"]["id"], done["receipt_id"], "{replay}");
    assert_eq!(
        fx.door.media.log().len(),
        traffic,
        "a settled job was resubmitted"
    );
    assert_eq!(fx.intent(&held)["receipt_id"], done["receipt_id"]);
    fx.assert_one_charge_per_key(1, "replay after completion");

    // 5. A job AgenticOS holds unresolved: Retry under another request id
    //    never mints a key while funds may be held; the same id re-checks the
    //    same key with the same body.
    fx.door.media.set(Scenario::Uncertain);
    let unresolved = fx.new_draft("unresolved", &source);
    fx.image(&unresolved, "img-unresolved").unwrap();
    fx.wait_intent(&unresolved, "uncertain");
    fx.assert_one_charge_per_key(2, "unresolved job");
    for n in 0..3 {
        let submits = fx.door.submits.lock().unwrap().len();
        let other = fx
            .image(&unresolved, &format!("img-unresolved-other-{n}"))
            .unwrap();
        assert_eq!(other["generation_intent"]["request_id"], "img-unresolved");
        assert_eq!(
            fx.door.submits.lock().unwrap().len(),
            submits,
            "another request id reached the door while AgenticOS holds the job"
        );
    }
    fx.image(&unresolved, "img-unresolved").unwrap();
    fx.wait_intent(&unresolved, "uncertain");
    fx.assert_one_charge_per_key(2, "re-check of an unresolved job");
}

/// (b) Only the operator's proven connection drives a job; nothing forges,
/// re-checks, reads or substitutes a job result.
#[test]
fn an_agent_or_unproven_caller_cannot_drive_forge_or_substitute_an_image_job() {
    let fx = Fx::start();
    let source = fx.fetch();
    let intruders = || [Asserted::Agent("intruder".into()), Asserted::Unproven];

    // Before any job exists: an agent or unproven caller relaying the real
    // session cannot start one.
    fx.door.media.set(Scenario::Hold);
    let target = fx.new_draft("target", &source);
    for who in intruders() {
        let label = who.as_str();
        let refused = fx.image_as(who, &target, "img-forged");
        assert!(
            refused.is_err(),
            "{label} started an image job: {refused:?}"
        );
    }
    assert!(
        fx.intents(&target).is_empty(),
        "a refused caller wrote an intent"
    );
    assert!(fx.door.submits.lock().unwrap().is_empty());

    // The operator's job is in flight.
    fx.image(&target, "img-target").unwrap();
    fx.wait_door("GET job");
    let before = fx.intent(&target);

    // An agent or unproven caller cannot re-check it, start another, or read
    // the listing / receipt surface.
    for who in intruders() {
        let label = who.as_str();
        for request in ["img-target", "img-forged-2"] {
            let refused = fx.image_as(who.clone(), &target, request);
            assert!(
                refused.is_err(),
                "{label} drove the job ({request}): {refused:?}"
            );
        }
        let listed = fx.screen_as(
            who.clone(),
            "app_social_draft_list",
            json!({"tool_alias": "social.draft"}),
        );
        assert!(listed.is_err(), "{label} read the job listing: {listed:?}");
    }
    let after = fx.intent(&target);
    assert_eq!(after["request_id"], before["request_id"]);
    assert_eq!(after["state"], "pending");
    fx.assert_one_charge_per_key(1, "intruder calls");

    // Another draft cannot take over the running job's request id.
    let other = fx.new_draft("other", &source);
    let taken = fx.image(&other, "img-target");
    assert!(taken.is_err(), "another draft took over a job: {taken:?}");
    assert!(fx.intents(&other).is_empty(), "{:?}", fx.intents(&other));

    // The job completes; its receipt is retained for the target only.
    fx.door.media.set(Scenario::Normal);
    let done = fx.wait_intent(&target, "completed");
    let receipt = done["receipt_id"].clone();
    for who in intruders() {
        let label = who.as_str();
        let read = fx.rpc_as(who, "app_tool_result", json!({"receipt_id": receipt}));
        assert!(read.is_err(), "{label} read the retained image: {read:?}");
    }

    // A finished job's request id replayed under another draft's scope never
    // settles that draft's intent with the substituted result.
    let _ = fx.image(&other, "img-target");
    assert!(
        fx.intents(&other)
            .iter()
            .all(|row| row["receipt_id"] != receipt && row["state"] != "completed"),
        "a result was substituted onto another draft: {:?}",
        fx.intents(&other)
    );
    assert_eq!(fx.intent(&target)["receipt_id"], receipt);
    fx.assert_one_charge_per_key(1, "substitution attempts");
}
