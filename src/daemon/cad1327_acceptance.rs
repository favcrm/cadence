//! Independent CAD-1327 refusal acceptance check, written by the reviewer
//! from the ticket (not by the implementer; the implementer must not edit or
//! weaken it). It drives the real `Shared::dispatch` guards — the live-turn
//! verifier on `app_assistant_invoke`, the operator-connection proof on
//! `app_assistant_decision`, the install-time `app-assistant/v1` validator,
//! the registry allow-list and the confirmed-price ceiling — against a door
//! that counts every provider call. Every "nothing was charged" assertion is
//! paired with a positive control in the same fixture that moves the counter.
#![cfg(feature = "test-seam")]

use super::*;
use crate::store::app_records::RecordStore;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[path = "../../tests/fixtures/aos_media_door.rs"]
mod aos_media_door;

const HOSTED: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@4","transport":"hosted-media-lease@1"}]}"#;
const HANDLE: &str = "juicysuite_crm";
const MASTER_GENERATION: &str = "0123456789abcdef0123456789abcdef";
const CRM_SOURCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/workspace-apps/crm");

/// The three registered Social Content actions, exactly as the registry
/// owns them (the descriptor may not change handler, effect or scope).
const FETCH: &str = r#"{ "id": "social.posts.fetch", "description": "Read recent posts.", "input_schema": { "type": "object", "properties": { "handle": { "type": "string", "minLength": 1, "maxLength": 64 } }, "required": [], "additionalProperties": false }, "effect": "read", "confirmation": "permission_required", "availability": "requires_context" }"#;
const DRAFT: &str = r#"{ "id": "social.draft.create", "description": "Draft a caption.", "input_schema": { "type": "object", "properties": { "post": { "type": "string", "minLength": 1, "maxLength": 128 }, "instructions": { "type": "string", "maxLength": 500 }, "with_image": { "type": "boolean" } }, "required": ["post"], "additionalProperties": false }, "effect": "draft", "confirmation": "permission_required", "availability": "requires_context" }"#;
const STATUS: &str = r#"{ "id": "social.drafts.status", "description": "Draft status.", "input_schema": { "type": "object", "properties": {}, "required": [], "additionalProperties": false }, "effect": "read", "confirmation": "none", "availability": "requires_context" }"#;
const CRM_SEARCH: &str = r#"{ "id": "customers.search", "description": "Search customers.", "input_schema": { "type": "object", "properties": { "query": { "type": "string", "maxLength": 160 }, "limit": { "type": "integer", "minimum": 1, "maximum": 20 }, "cursor": { "type": "string", "maxLength": 256 } }, "required": [], "additionalProperties": false }, "effect": "read", "confirmation": "none", "availability": "requires_context" }"#;

fn descriptor(app: Option<&str>, actions: &[&str]) -> String {
    let app = app.map_or(String::new(), |a| format!(r#""app": "{a}","#));
    format!(
        r#"{{ "contract": "app-assistant/v1", {app} "actions": [{}] }}"#,
        actions.join(",")
    )
}

fn social_descriptor() -> String {
    descriptor(Some("social-content"), &[FETCH, DRAFT, STATUS])
}

/// The step request id the ticket's exactly-once rule derives from
/// (install, context, operation, step). Re-derived here from the reviewed
/// domain string so a derivation that drops a scope component fails.
fn step_id(install: &str, context: &str, operation: &str, step: &str) -> String {
    let digest = crate::store::app_runs::material_digest(
        &json!({"domain":"cad1327-assistant-step-v1","install":install,"context":context,"operation":operation,"step":step}),
    );
    format!("sa-{step}-{}", &digest["sha256:".len()..][..36])
}

struct Door {
    media: aos_media_door::MediaDoor,
    read_price: Mutex<String>,
    text_price: Mutex<String>,
    /// Overrides the media door's image price (`chargeMinor`) when set.
    image_minor: Mutex<Option<u64>>,
    /// When set, a text call is counted and the connection is dropped
    /// without an answer (an uncertain provider outcome).
    drop_text: AtomicBool,
    text_calls: AtomicUsize,
    reads: AtomicUsize,
}

impl Door {
    fn spent(&self) -> (usize, usize, usize) {
        (
            self.reads.load(SeqCst),
            self.text_calls.load(SeqCst),
            self.media.job_count(),
        )
    }
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
    let read_price =
        json!({"currency":"USD","scale":6,"amount":door.read_price.lock().unwrap().clone()});
    let text_price =
        json!({"currency":"USD","scale":6,"amount":door.text_price.lock().unwrap().clone()});
    let reply = match (method.as_str(), route) {
        ("GET", "/v1/runtime/tools/read_instagram_posts") => json_reply(
            json!({"ok":true,"data":{"slug":"read_instagram_posts","displayName":"Read Instagram posts","effect":"read","chargePrecondition":"max_charge_minor@1","price":read_price,"unitPrice":null}}),
        ),
        ("POST", "/v1/runtime/tools/call") if sent["slug"] == "read_instagram_posts" => {
            door.reads.fetch_add(1, SeqCst);
            json_reply(
                json!({"ok":true,"data":{"slug":"read_instagram_posts","repeated":false,"price":read_price,"result":{"success":true,"status":"ok","user":{"username":HANDLE,"is_private":false},"items":[
                    {"id":"post-old","code":"OldOld1","created_at":"2026-09-01T00:00:00Z","caption":{"text":"An older post"}},
                    {"id":"post-new","code":"NewNew1","created_at":"2026-09-27T00:00:00Z","caption":{"text":"Autumn menu is here"}}]}}}),
            )
        }
        ("GET", "/v1/runtime/tools/generate_text") => json_reply(
            json!({"ok":true,"data":{"slug":"generate_text","displayName":"Generate text","effect":"draft","chargePrecondition":"max_charge_minor@1","price":text_price,"unitPrice":null}}),
        ),
        ("POST", "/v1/runtime/tools/call") if sent["slug"] == "generate_text" => {
            door.text_calls.fetch_add(1, SeqCst);
            if door.drop_text.load(SeqCst) {
                return;
            }
            json_reply(
                json!({"ok":true,"data":{"slug":"generate_text","repeated":false,"price":text_price,"result":{"text":"秋日菜單登場，歡迎品嚐。","finishReason":"stop","usage":{"inputTokens":9,"outputTokens":5,"totalTokens":14,"cachedInputTokens":null,"reasoningOutputTokens":null}}}}),
            )
        }
        ("GET", "/v1/runtime/media/price/image") if door.image_minor.lock().unwrap().is_some() => {
            let minor = door.image_minor.lock().unwrap().unwrap();
            json_reply(
                json!({"ok":true,"data":{"kind":"image","model":aos_media_door::IMAGE_MODEL,
                    "price":{"slug":"generate_image","chargeMinor":minor,"currency":"USD","version":"2026-09-29T00:00:00.000Z"}}}),
            )
        }
        (m, r) if r.starts_with("/v1/runtime/media/") => {
            let key = header("idempotency-key");
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

/// One fixture at a time inside this module.
static SERIAL: Mutex<()> = Mutex::new(());

struct Fx {
    _serial: std::sync::MutexGuard<'static, ()>,
    dir: tempfile::TempDir,
    shared: Arc<Shared>,
    door: Arc<Door>,
    install: String,
    context: String,
}

fn refused(result: Result<Value>, needle: &str, what: &str) -> String {
    match result {
        Ok(value) => panic!("{what} was accepted: {value}"),
        Err(error) => {
            let text = error.to_string();
            assert!(
                text.contains(needle),
                "{what} refused for the wrong reason (want '{needle}'): {text}"
            );
            text
        }
    }
}

impl Fx {
    fn call(&self, who: Asserted, method: &str, params: Value) -> Result<Value> {
        scoped(who, || {
            self.shared.dispatch(method, &params, std::process::id())
        })
    }
    fn operator(&self, method: &str, params: Value) -> Value {
        self.call(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }
    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(crate::rollout::db_file(self.dir.path())).unwrap()
    }
    fn records_of(&self, install: &str) -> RecordStore {
        RecordStore::open(self.dir.path(), install).unwrap()
    }
    fn records(&self) -> RecordStore {
        self.records_of(&self.install)
    }
    fn drafts(&self, context: &str) -> usize {
        self.records().app_social_draft_list(context).unwrap()["drafts"]
            .as_array()
            .unwrap()
            .len()
    }

    /// A fixture with the Social Content package installed with
    /// `descriptor`, or with nothing installed.
    fn with_social(descriptor: Option<&str>) -> Self {
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
        let dir = tempfile::Builder::new().prefix("a1327").tempdir().unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let door = Arc::new(Door {
            media: aos_media_door::MediaDoor::new(),
            read_price: Mutex::new("0.002000".into()),
            text_price: Mutex::new("0.002000".into()),
            image_minor: Mutex::new(None),
            drop_text: AtomicBool::new(false),
            text_calls: AtomicUsize::new(0),
            reads: AtomicUsize::new(0),
        });
        let serving = door.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let door = serving.clone();
                std::thread::spawn(move || serve(stream, &door));
            }
        });
        // `ALL_PROXY` is process-global and another module's fixture may set
        // it concurrently. The transport captures it when it is attached, so
        // retry until the value is provably ours across the attach.
        let proxy = format!("http://{addr}");
        let opts = loop {
            let mut opts = ServeOptions {
                provider_deployments: Some(
                    crate::platform::deployments::DeploymentMetadata::parse(HOSTED.as_bytes())
                        .unwrap(),
                ),
                image_job_window_ms: 0,
                image_job_backoff_ms: 100,
                ..Default::default()
            };
            opts.provider_env
                .set("CADENCE_PM_DIR", pm.to_str().unwrap());
            crate::platform::local::register_at(
                dir.path(),
                &mut opts,
                dir.path().join("outbox"),
                "http://127.0.0.1:3010".into(),
            );
            std::env::set_var("ALL_PROXY", &proxy);
            crate::platform::agenticos_external::attach(&mut opts).unwrap();
            let ours = std::env::var("ALL_PROXY").ok().as_deref() == Some(proxy.as_str());
            if ours {
                std::env::remove_var("ALL_PROXY");
                break opts;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd = dir.path().to_str().unwrap();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "w1",
                provider: "pi",
                endpoint_kind: "managed",
                role: "worker",
                cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared
            .store
            .register_agent(&NewAgent {
                alias: "master",
                provider: "pi",
                endpoint_kind: "managed",
                role: "worker",
                cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared
            .store
            .set_identity_with_quota(
                "master",
                &crate::adapter::Identity {
                    thread_id: "cad1327a-thread".into(),
                    session_id: "cad1327a-session".into(),
                    model: None,
                    effort: None,
                    pid: std::process::id(),
                    endpoint: None,
                    generation: Some(MASTER_GENERATION.into()),
                    attach: None,
                },
                None,
            )
            .unwrap();
        let mut fx = Self {
            _serial: serial,
            dir,
            shared,
            door,
            install: String::new(),
            context: String::new(),
        };
        if let Some(descriptor) = descriptor {
            let install = fx
                .install_social("app", descriptor)
                .expect("social install");
            let context = fx.add_context(&install, "ctx-a");
            fx.install = install;
            fx.context = context;
        }
        fx
    }

    fn new() -> Self {
        Self::with_social(Some(&social_descriptor()))
    }

    /// The Social Content package as the author's flow ships it, with the
    /// given `app-assistant.json`.
    fn social_package(&self, name: &str, descriptor: &str) -> std::path::PathBuf {
        let app = self.dir.path().join(name);
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        copy_dir(&manifest_dir.join("workspace-apps/social-content"), &app);
        std::fs::copy(
            manifest_dir.join("tests/fixtures/apps/social-content-writer/app.md"),
            app.join("app.md"),
        )
        .unwrap();
        copy_dir(
            &manifest_dir.join("tests/fixtures/apps/ig-tools-fixture/screens"),
            &app.join("screens"),
        );
        let screens = app.join("screens/feed/screens.json");
        let text = std::fs::read_to_string(&screens).unwrap();
        let text = text.replace("ig-tools-fixture", "social-content").replace(
            r#""tools": { "instagram.read": "source" }"#,
            r#""tools": { "instagram.read": "source", "caption.generate": "writer", "social.draft": "image" }"#,
        );
        assert!(
            text.contains("caption.generate"),
            "screen tools edit applied"
        );
        std::fs::write(&screens, text).unwrap();
        std::fs::write(app.join("app-assistant.json"), descriptor).unwrap();
        app
    }

    fn install_dir(&self, app: &std::path::Path) -> Result<String> {
        let installed = self.call(
            Asserted::Operator,
            "app_workspace_install",
            json!({"source": app.to_str().unwrap()}),
        )?;
        let install = installed["install_id"].as_str().unwrap().to_string();
        let approved = self.call(
            Asserted::Operator,
            "app_local_install_approve",
            json!({"install_id": install, "digest": installed["digest"]}),
        );
        if let Err(error) = approved {
            // An app without capabilities may already be approved.
            assert!(
                !error.to_string().contains("operator"),
                "approve refused: {error}"
            );
        }
        Ok(install)
    }

    fn install_social(&self, name: &str, descriptor: &str) -> Result<String> {
        let app = self.social_package(name, descriptor);
        self.install_dir(&app)
    }

    fn add_context(&self, install: &str, request: &str) -> String {
        let connections = self.operator("connection_list", json!({}))["connections"].clone();
        let hosted = connections
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["account"] == "hosted")
            .expect("hosted connection")["id"]
            .clone();
        let context = self.operator(
            "app_context_create",
            json!({"install_id": install, "label": request, "input_defaults": {}, "request_id": request}),
        );
        let context = context["context"]["id"]
            .as_str()
            .or_else(|| context["id"].as_str())
            .unwrap()
            .to_string();
        for slot in ["source", "writer", "image"] {
            self.operator(
                "app_binding_create",
                json!({"install_id": install, "context_id": context, "slot": slot,
                    "connection_id": hosted, "request_id": format!("bind-{request}-{slot}")}),
            );
        }
        self.records_of(install)
            .app_social_sources_save(&context, 0, &[HANDLE.to_owned()], &format!("src-{request}"))
            .unwrap();
        context
    }

    fn create_turn_in(&self, install: &str, context: &str, message: &str) -> String {
        self.operator(
            "conversation_create",
            json!({"alias":"master", "install_id":install, "context_id":context, "general":true}),
        );
        self.operator(
            "thread_send",
            json!({"alias":"master", "text":"Draft a post from the newest",
                "message":message, "app":{"install_id":install,"context_id":context}}),
        );
        let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(MASTER_GENERATION);
        self.db()
            .execute(
                "UPDATE messages SET state='submitting', started=1.0 WHERE id=?",
                [message],
            )
            .unwrap();
        self.shared.store.mark_running(message, &token).unwrap();
        token
    }

    fn create_turn(&self, message: &str) -> String {
        self.create_turn_in(&self.install, &self.context, message)
    }

    #[allow(clippy::too_many_arguments)]
    fn invoke_in(
        &self,
        install: &str,
        context: &str,
        message: &str,
        token: &str,
        action: &str,
        op: &str,
        input: Value,
    ) -> Result<Value> {
        self.call(
            Asserted::Agent("master".into()),
            "app_assistant_invoke",
            json!({"install_id":install,"context_id":context,"message":message,
                "token":token,"action_id":action,"operation_id":op,"input":input}),
        )
    }

    fn invoke(
        &self,
        message: &str,
        token: &str,
        action: &str,
        op: &str,
        input: Value,
    ) -> Result<Value> {
        self.invoke_in(
            &self.install,
            &self.context,
            message,
            token,
            action,
            op,
            input,
        )
    }

    fn decide_in(
        &self,
        who: Asserted,
        install: &str,
        context: &str,
        op: &Value,
        decision: &str,
    ) -> Result<Value> {
        self.call(
            who,
            "app_assistant_decision",
            json!({"install_id":install,"context_id":context,
                "operation_id":op["id"],"decision":decision,"expected_revision":op["revision"]}),
        )
    }

    fn decide(&self, who: Asserted, op: &Value, decision: &str) -> Result<Value> {
        self.decide_in(who, &self.install, &self.context, op, decision)
    }

    fn show_in(&self, install: &str, context: &str, op: &str) -> Result<Value> {
        self.call(
            Asserted::Operator,
            "app_assistant_operation_operator_show",
            json!({"install_id":install,"context_id":context,"operation_id":op}),
        )
    }

    fn show(&self, op: &str) -> Value {
        self.show_in(&self.install, &self.context, op)
            .unwrap_or_else(|e| panic!("show {op}: {e}"))["operation"]
            .clone()
    }

    /// Fetch through the operator's Allow so posts exist to draft from.
    fn fetched(&self, message: &str, token: &str, op: &str) {
        let pending = pending(
            &self
                .invoke(message, token, "social.posts.fetch", op, json!({}))
                .unwrap(),
        );
        let done = self
            .decide(Asserted::Operator, &pending, "allow_once")
            .unwrap();
        assert_eq!(done["operation"]["status"], "succeeded", "{done}");
    }

    fn wait_jobs(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while self.door.media.job_count() < n {
            assert!(Instant::now() < deadline, "image job never submitted");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn pending(reply: &Value) -> Value {
    let op = reply["operation"].clone();
    assert_eq!(op["status"], "pending_permission", "{reply}");
    assert_eq!(op["permission_request"]["allow_always"], false, "{reply}");
    op
}

/// (1) Only the operator connection can Allow a paid action. The agent that
/// requested it, and an unproven caller, are refused by the operator proof
/// and nothing is spent; the operation stays pending.
#[test]
fn agent_and_unproven_callers_cannot_allow_a_paid_social_action() {
    let fx = Fx::new();
    let token = fx.create_turn("m-1");
    let op = pending(
        &fx.invoke("m-1", &token, "social.posts.fetch", "op-1", json!({}))
            .unwrap(),
    );
    // The requesting chat agent is held to its app-turn verbs ...
    refused(
        fx.decide(Asserted::Agent("master".into()), &op, "allow_once"),
        "not app_assistant_decision",
        "agent allow on its own paid operation",
    );
    // ... and any other agent connection meets the operator proof itself.
    refused(
        fx.decide(Asserted::Agent("w1".into()), &op, "allow_once"),
        "operator action",
        "another agent's allow",
    );
    refused(
        fx.decide(Asserted::Unproven, &op, "allow_once"),
        "operator",
        "unproven allow",
    );
    // An agent cannot reach the operator's view/decision family at all.
    refused(
        fx.call(
            Asserted::Agent("master".into()),
            "app_assistant_operation_operator_show",
            json!({"install_id":fx.install,"context_id":fx.context,"operation_id":"op-1"}),
        ),
        "operator",
        "agent operator_show",
    );
    assert_eq!(fx.door.spent(), (0, 0, 0), "a refused Allow spent");
    let shown = fx.show("op-1");
    assert_eq!(shown["status"], "pending_permission", "{shown}");
    assert_eq!(shown["revision"], op["revision"], "{shown}");
    // Positive control: the operator's Allow once runs the paid read.
    let done = fx.decide(Asserted::Operator, &op, "allow_once").unwrap();
    assert_eq!(done["operation"]["status"], "succeeded", "{done}");
    assert_eq!(fx.door.spent(), (1, 0, 0));
}

/// (2) The agent turn never spends: invoking a paid action only quotes and
/// parks it. No draft, no generation intent, no text call, no image job.
#[test]
fn invoke_alone_never_drafts_or_calls_a_paid_door() {
    let fx = Fx::new();
    let token = fx.create_turn("m-2");
    let fetch = pending(
        &fx.invoke("m-2", &token, "social.posts.fetch", "op-f", json!({}))
            .unwrap(),
    );
    assert!(
        fetch["permission_request"]["reason"]
            .as_str()
            .unwrap()
            .contains("USD 0.002"),
        "the card shows the cost: {fetch}"
    );
    assert_eq!(fx.door.spent(), (0, 0, 0), "fetch invoke spent");
    fx.decide(Asserted::Operator, &fetch, "allow_once").unwrap();
    let op = pending(
        &fx.invoke(
            "m-2",
            &token,
            "social.draft.create",
            "op-d",
            json!({"post":"newest","with_image":true}),
        )
        .unwrap(),
    );
    let reason = op["permission_request"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("USD") && reason.contains("image"),
        "{reason}"
    );
    assert_eq!(fx.door.spent(), (1, 0, 0), "draft invoke spent");
    assert_eq!(fx.drafts(&fx.context), 0, "draft invoke created a draft");
    assert!(
        fx.records()
            .app_social_generation_intents(&fx.context)
            .unwrap()
            .is_empty(),
        "draft invoke recorded a generation intent"
    );
    // A repeated invoke of the same operation id replays the parked
    // operation; still nothing is spent.
    let again = fx
        .invoke(
            "m-2",
            &token,
            "social.draft.create",
            "op-d",
            json!({"post":"newest","with_image":true}),
        )
        .unwrap();
    assert_eq!(again["operation"]["status"], "pending_permission");
    assert_eq!(fx.door.spent(), (1, 0, 0));
    // Positive control: the operator's Allow creates the draft and spends.
    let done = fx.decide(Asserted::Operator, &op, "allow_once").unwrap();
    assert_eq!(done["operation"]["status"], "succeeded", "{done}");
    assert_eq!(fx.drafts(&fx.context), 1);
    fx.wait_jobs(1);
    assert_eq!(fx.door.spent(), (1, 1, 1));
}

/// (3) A paid social action is allowed one time only: Always allow and a
/// standing block are refused, and the refusal consumes nothing.
#[test]
fn allow_always_and_standing_grants_are_refused_for_social_actions() {
    let fx = Fx::new();
    let token = fx.create_turn("m-3");
    let op = pending(
        &fx.invoke("m-3", &token, "social.posts.fetch", "op-3", json!({}))
            .unwrap(),
    );
    refused(
        fx.decide(Asserted::Operator, &op, "allow_always"),
        "one time only",
        "allow_always on social.posts.fetch",
    );
    for action in ["social.posts.fetch", "social.draft.create"] {
        assert!(
            fx.call(
                Asserted::Operator,
                "app_assistant_permission_block",
                json!({"install_id":fx.install,"context_id":fx.context,"action_id":action,"resource_id":HANDLE}),
            )
            .is_err(),
            "standing block accepted for {action}"
        );
    }
    let permissions = fx.operator(
        "app_assistant_permissions",
        json!({"install_id":fx.install,"context_id":fx.context}),
    );
    assert_eq!(
        permissions["permissions"].as_array().map(Vec::len),
        Some(0),
        "{permissions}"
    );
    assert_eq!(fx.door.spent(), (0, 0, 0));
    let shown = fx.show("op-3");
    assert_eq!(shown["status"], "pending_permission", "{shown}");
    // Positive control: Allow once on the same operation still works.
    fx.decide(Asserted::Operator, &op, "allow_once").unwrap();
    assert_eq!(fx.door.spent(), (1, 0, 0));
    // A later identical request parks again; nothing was remembered.
    let next = pending(
        &fx.invoke("m-3", &token, "social.posts.fetch", "op-3b", json!({}))
            .unwrap(),
    );
    assert_eq!(next["status"], "pending_permission");
    assert_eq!(fx.door.spent(), (1, 0, 0));
}

/// (4) Exactly once: a second Allow (stale or current revision) and a
/// replayed operation id never charge again or create another draft; an
/// operation id cannot be rebound to other input.
#[test]
fn a_second_allow_and_a_replayed_operation_never_charge_twice() {
    let fx = Fx::new();
    let token = fx.create_turn("m-4");
    fx.fetched("m-4", &token, "op-f");
    let op = pending(
        &fx.invoke(
            "m-4",
            &token,
            "social.draft.create",
            "op-d",
            json!({"post":"newest","with_image":true}),
        )
        .unwrap(),
    );
    let done = fx.decide(Asserted::Operator, &op, "allow_once").unwrap();
    assert_eq!(done["operation"]["status"], "succeeded", "{done}");
    fx.wait_jobs(1);
    let spent = fx.door.spent();
    assert_eq!(spent, (1, 1, 1));
    refused(
        fx.decide(Asserted::Operator, &op, "allow_once"),
        "not pending",
        "second Allow at the confirmed revision",
    );
    refused(
        fx.decide(Asserted::Operator, &done["operation"], "allow_once"),
        "not pending",
        "second Allow at the current revision",
    );
    let replay = fx
        .invoke(
            "m-4",
            &token,
            "social.draft.create",
            "op-d",
            json!({"post":"newest","with_image":true}),
        )
        .unwrap();
    assert_eq!(replay["operation"]["status"], "succeeded", "{replay}");
    assert_eq!(
        replay["operation"]["result"]["draft_id"],
        done["operation"]["result"]["draft_id"]
    );
    refused(
        fx.invoke(
            "m-4",
            &token,
            "social.draft.create",
            "op-d",
            json!({"post":"post-old"}),
        ),
        "already bound",
        "operation id rebound to other input",
    );
    let fetch_replay = fx
        .invoke("m-4", &token, "social.posts.fetch", "op-f", json!({}))
        .unwrap();
    assert_eq!(fetch_replay["operation"]["status"], "succeeded");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(fx.door.spent(), spent, "a replay charged again");
    assert_eq!(fx.drafts(&fx.context), 1, "a replay created another draft");
}

/// (5) The registry is an allow-list bound to the app. A social id in a
/// non-social descriptor and a non-social id in a social-content
/// descriptor are refused at install; a CRM installation gets no social
/// action and CRM behaviour is unchanged; a CRM id on the Social Content
/// installation is refused at invoke even when the descriptor declares it.
#[test]
fn social_and_crm_action_ids_never_cross_apps() {
    let fx = Fx::with_social(None);
    // Install-time validator (the same function the install path runs).
    for (text, what) in [
        (
            descriptor(Some("crm"), &[CRM_SEARCH, FETCH]),
            "social id in a crm descriptor",
        ),
        (
            descriptor(None, &[FETCH]),
            "social id in an app-less descriptor",
        ),
        (
            descriptor(Some("social-content"), &[FETCH, CRM_SEARCH]),
            "crm id in a social-content descriptor",
        ),
    ] {
        refused(
            crate::issue::app_assistant::validate(&text).map(|_| Value::Null),
            "does not belong",
            what,
        );
    }
    // Install of a CRM package advertising a social action is refused.
    let crm_bad = fx.dir.path().join("crm-bad");
    copy_dir(std::path::Path::new(CRM_SOURCE), &crm_bad);
    let crm_descriptor = std::fs::read_to_string(crm_bad.join("app-assistant.json")).unwrap();
    let mut parsed: Value = serde_json::from_str(&crm_descriptor).unwrap();
    parsed["actions"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::from_str(FETCH).unwrap());
    std::fs::write(crm_bad.join("app-assistant.json"), parsed.to_string()).unwrap();
    refused(
        fx.install_dir(&crm_bad).map(Value::from),
        "app-assistant.json",
        "a CRM package advertising social.posts.fetch",
    );
    // Install of a Social Content package advertising a CRM action is
    // refused.
    refused(
        fx.install_social(
            "social-bad",
            &descriptor(Some("social-content"), &[FETCH, CRM_SEARCH]),
        )
        .map(Value::from),
        "app-assistant.json",
        "a social-content package advertising customers.search",
    );

    // The real CRM package: its actions list has no social id, a social id
    // is refused at invoke, and its own read still works (unchanged).
    let crm = fx.install_dir(std::path::Path::new(CRM_SOURCE)).unwrap();
    let crm_ctx = fx.operator(
        "app_context_create",
        json!({"install_id": crm, "label": "CRM", "input_defaults": {}, "request_id": "ctx-crm"}),
    );
    let crm_ctx = crm_ctx["context"]["id"]
        .as_str()
        .or_else(|| crm_ctx["id"].as_str())
        .unwrap()
        .to_string();
    let token = fx.create_turn_in(&crm, &crm_ctx, "m-crm");
    let listed = fx
        .call(
            Asserted::Agent("master".into()),
            "app_assistant_actions",
            json!({"install_id":crm,"context_id":crm_ctx,"message":"m-crm","token":token}),
        )
        .unwrap();
    let ids: Vec<&str> = listed["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["id"].as_str())
        .collect();
    assert!(ids.contains(&"customers.search"), "{listed}");
    assert!(
        !ids.iter().any(|id| id.starts_with("social.")),
        "CRM lists a social action: {listed}"
    );
    for action in [
        "social.posts.fetch",
        "social.draft.create",
        "social.drafts.status",
    ] {
        let input = if action == "social.draft.create" {
            json!({"post":"newest"})
        } else {
            json!({})
        };
        assert!(
            fx.invoke_in(&crm, &crm_ctx, "m-crm", &token, action, "op-crm-x", input)
                .is_err(),
            "CRM installation ran {action}"
        );
    }
    let search = fx
        .invoke_in(
            &crm,
            &crm_ctx,
            "m-crm",
            &token,
            "customers.search",
            "op-crm-search",
            json!({}),
        )
        .unwrap();
    assert_eq!(search["operation"]["status"], "succeeded", "{search}");

    // A Social Content package whose descriptor declares a CRM id without
    // naming its app passes the descriptor grammar; the runtime allow-list
    // (bound to the installed manifest's app, not the descriptor) refuses
    // it at discovery and invoke.
    let sneaky = fx
        .install_social("social-sneaky", &descriptor(None, &[CRM_SEARCH]))
        .expect("descriptor without app and only a CRM id installs");
    let sneaky_ctx = fx.add_context(&sneaky, "ctx-sneaky");
    let token = fx.create_turn_in(&sneaky, &sneaky_ctx, "m-sneaky");
    let listed = fx
        .call(
            Asserted::Agent("master".into()),
            "app_assistant_actions",
            json!({"install_id":sneaky,"context_id":sneaky_ctx,"message":"m-sneaky","token":token}),
        )
        .unwrap();
    assert_eq!(
        listed["actions"].as_array().map(Vec::len),
        Some(0),
        "{listed}"
    );
    refused(
        fx.invoke_in(
            &sneaky,
            &sneaky_ctx,
            "m-sneaky",
            &token,
            "customers.search",
            "op-sneaky",
            json!({}),
        ),
        "not available for this installation's app",
        "CRM id on the Social Content installation",
    );
    assert_eq!(fx.door.spent(), (0, 0, 0));
}

/// (6) A live price above what the operator confirmed fails the operation
/// before anything is claimed: no provider call, no draft, no image job.
/// Each slot is held to its own confirmed price.
#[test]
fn a_price_above_the_confirmed_estimate_fails_with_nothing_charged() {
    let fx = Fx::new();
    let token = fx.create_turn("m-6");
    // The paid read.
    let op = pending(
        &fx.invoke("m-6", &token, "social.posts.fetch", "op-f1", json!({}))
            .unwrap(),
    );
    *fx.door.read_price.lock().unwrap() = "0.500000".into();
    let done = fx.decide(Asserted::Operator, &op, "allow_once").unwrap();
    assert_eq!(done["operation"]["status"], "failed", "{done}");
    assert_eq!(fx.door.spent(), (0, 0, 0), "an over-price read was charged");
    *fx.door.read_price.lock().unwrap() = "0.002000".into();
    fx.fetched("m-6", &token, "op-f2");
    assert_eq!(fx.door.spent(), (1, 0, 0));

    // The caption step.
    let op = pending(
        &fx.invoke(
            "m-6",
            &token,
            "social.draft.create",
            "op-d1",
            json!({"post":"newest"}),
        )
        .unwrap(),
    );
    *fx.door.text_price.lock().unwrap() = "0.900000".into();
    let done = fx.decide(Asserted::Operator, &op, "allow_once").unwrap();
    assert_eq!(done["operation"]["status"], "failed", "{done}");
    assert!(
        done["operation"]["summary"]
            .as_str()
            .unwrap_or_default()
            .contains("price"),
        "{done}"
    );
    assert_eq!(
        fx.door.spent(),
        (1, 0, 0),
        "an over-price caption was charged"
    );
    assert_eq!(fx.drafts(&fx.context), 0, "an over-price action drafted");

    // The image step: the image price rises above its confirmed share while
    // the text price drops by more, so the total still looks confirmed.
    *fx.door.text_price.lock().unwrap() = "0.002000".into();
    let op = pending(
        &fx.invoke(
            "m-6",
            &token,
            "social.draft.create",
            "op-d2",
            json!({"post":"newest","with_image":true}),
        )
        .unwrap(),
    );
    let preview = op["permission_request"]["preview"].clone();
    let image = preview["image_micros"].as_u64().expect("image estimate");
    assert!(image > 0, "{preview}");
    *fx.door.text_price.lock().unwrap() = "0.000001".into();
    // `chargeMinor` is in the door's minor unit; raise it a little.
    *fx.door.image_minor.lock().unwrap() = Some(31_501);
    let done = fx.decide(Asserted::Operator, &op, "allow_once").unwrap();
    assert_eq!(done["operation"]["status"], "failed", "{done}");
    assert_eq!(
        fx.door.spent(),
        (1, 0, 0),
        "an over-price image was charged or its caption ran: {done}"
    );
    assert_eq!(
        fx.drafts(&fx.context),
        0,
        "an over-price image action drafted"
    );

    // Positive control: at the confirmed prices the same request runs.
    *fx.door.text_price.lock().unwrap() = "0.002000".into();
    *fx.door.image_minor.lock().unwrap() = None;
    let op = pending(
        &fx.invoke(
            "m-6",
            &token,
            "social.draft.create",
            "op-d3",
            json!({"post":"newest"}),
        )
        .unwrap(),
    );
    let done = fx.decide(Asserted::Operator, &op, "allow_once").unwrap();
    assert_eq!(done["operation"]["status"], "succeeded", "{done}");
    assert_eq!(fx.door.spent(), (1, 1, 0));
}

/// (7) The draft action cannot reach publish, approve, send, discard,
/// connect, settings or a CRM action: those ids are not registered, extra
/// input fields are refused before any operation exists, and a CRM record
/// id is not a post.
#[test]
fn draft_create_cannot_reach_publish_discard_or_crm_actions() {
    let fx = Fx::new();
    let token = fx.create_turn("m-7");
    for (n, action) in [
        "social.draft.publish",
        "social.draft.stage",
        "social.draft.approve",
        "social.draft.send",
        "social.draft.discard",
        "social.accounts.connect",
        "social.accounts.disconnect",
        "social.destination.set",
        "social.settings.update",
        "customers.search",
        "customer.tags.update",
        "campaigns.create_draft",
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            fx.invoke("m-7", &token, action, &format!("op-x{n}"), json!({}))
                .is_err(),
            "{action} was accepted on the Social Content installation"
        );
    }
    for (n, input) in [
        json!({"post":"newest","action":"publish"}),
        json!({"post":"newest","draft_id":"d-1"}),
        json!({"post":"newest","customer_id":"cust-a"}),
        json!({"post":"newest","handle":HANDLE}),
        json!({"post":"newest","with_image":"true"}),
        json!({"post":""}),
        json!({"post":"x".repeat(129)}),
        json!({"post":"newest","instructions":"x".repeat(501)}),
        json!({}),
    ]
    .into_iter()
    .enumerate()
    {
        let op = format!("op-in{n}");
        refused(
            fx.invoke("m-7", &token, "social.draft.create", &op, input.clone()),
            "assistant action input",
            &format!("draft input {input}"),
        );
        assert!(
            fx.show_in(&fx.install, &fx.context, &op).is_err(),
            "invalid input left an operation {op}"
        );
    }
    fx.fetched("m-7", &token, "op-f");
    refused(
        fx.invoke(
            "m-7",
            &token,
            "social.draft.create",
            "op-crm-id",
            json!({"post":"cust-a"}),
        ),
        "not in the latest fetched posts",
        "a CRM record id as the post",
    );
    // Positive control: the allowed draft is an editable draft only.
    let op = pending(
        &fx.invoke(
            "m-7",
            &token,
            "social.draft.create",
            "op-d",
            json!({"post":"post-old"}),
        )
        .unwrap(),
    );
    let done = fx.decide(Asserted::Operator, &op, "allow_once").unwrap();
    assert_eq!(done["operation"]["status"], "succeeded", "{done}");
    let draft = done["operation"]["result"]["draft_id"].as_str().unwrap();
    let row = fx
        .records()
        .app_social_draft_show(&fx.context, draft)
        .unwrap();
    assert!(
        row["discarded_at"].is_null() && row["discarded"] != true,
        "{row}"
    );
    assert_eq!(fx.door.spent(), (1, 1, 0));
}

/// (8) A run interrupted after the Allow reads `unknown` and is never
/// retried automatically: no second Allow, and a replay of the operation id
/// returns the stored outcome without calling the provider again.
#[test]
fn an_interrupted_run_reads_unknown_and_is_not_retried() {
    let fx = Fx::new();
    let token = fx.create_turn("m-8");
    fx.fetched("m-8", &token, "op-f");
    let op = pending(
        &fx.invoke(
            "m-8",
            &token,
            "social.draft.create",
            "op-d",
            json!({"post":"newest"}),
        )
        .unwrap(),
    );
    fx.door.drop_text.store(true, SeqCst);
    let done = fx.decide(Asserted::Operator, &op, "allow_once").unwrap();
    assert_eq!(done["operation"]["status"], "unknown", "{done}");
    let calls = fx.door.text_calls.load(SeqCst);
    assert!(calls >= 1, "the provider was never reached");
    fx.door.drop_text.store(false, SeqCst);
    assert_eq!(fx.show("op-d")["status"], "unknown");
    refused(
        fx.decide(Asserted::Operator, &op, "allow_once"),
        "not pending",
        "Allow after an uncertain run",
    );
    let replay = fx
        .invoke(
            "m-8",
            &token,
            "social.draft.create",
            "op-d",
            json!({"post":"newest"}),
        )
        .unwrap();
    assert_eq!(replay["operation"]["status"], "unknown", "{replay}");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        fx.door.text_calls.load(SeqCst),
        calls,
        "the run was retried"
    );

    // A daemon that stopped right after the Allow's CAS leaves `running`;
    // the next read reports `unknown` and the decision stays closed.
    let op = pending(
        &fx.invoke("m-8", &token, "social.posts.fetch", "op-crash", json!({}))
            .unwrap(),
    );
    fx.records()
        .app_assistant_operation_set(crate::store::app_records::AssistantOperationUpdate {
            operation_id: "op-crash",
            context: &fx.context,
            expected_revision: op["revision"].as_i64().unwrap(),
            status: "running",
            summary: "Working on it",
            result: &Value::Null,
            resource_refs: &json!([]),
            permission_request: &Value::Null,
            error: None,
        })
        .unwrap();
    assert_eq!(fx.show("op-crash")["status"], "unknown");
    refused(
        fx.decide(Asserted::Operator, &op, "allow_once"),
        "not pending",
        "Allow after an interrupted run",
    );
    assert_eq!(fx.door.reads.load(SeqCst), 1, "interrupted fetch retried");
}

/// (9) Step request ids are scoped to (install, context, operation, step):
/// each context's run charges once under its own ids, the recorded ids are
/// exactly the scoped derivation (so a derivation that drops the install or
/// context fails here), and an operation id already used in one context
/// cannot be reused from another context to replay or spend.
#[test]
fn step_ids_never_collide_across_contexts_or_installs() {
    let fx = Fx::new();
    let ctx_b = fx.add_context(&fx.install, "ctx-b");
    let run = |context: &str, message: &str, fetch_op: &str, draft_op: &str| {
        let token = fx.create_turn_in(&fx.install, context, message);
        let fetch = pending(
            &fx.invoke_in(
                &fx.install,
                context,
                message,
                &token,
                "social.posts.fetch",
                fetch_op,
                json!({}),
            )
            .unwrap(),
        );
        fx.decide_in(
            Asserted::Operator,
            &fx.install,
            context,
            &fetch,
            "allow_once",
        )
        .unwrap();
        let op = pending(
            &fx.invoke_in(
                &fx.install,
                context,
                message,
                &token,
                "social.draft.create",
                draft_op,
                json!({"post":"newest"}),
            )
            .unwrap(),
        );
        let done = fx
            .decide_in(Asserted::Operator, &fx.install, context, &op, "allow_once")
            .unwrap();
        assert_eq!(done["operation"]["status"], "succeeded", "{done}");
        let intents = fx.records().app_social_generation_intents(context).unwrap();
        let ids: Vec<String> = intents
            .iter()
            .filter_map(|i| i["request_id"].as_str().map(str::to_owned))
            .collect();
        assert_eq!(
            ids,
            vec![step_id(&fx.install, context, draft_op, "cap")],
            "caption request id is not the scoped derivation"
        );
        token
    };
    run(&fx.context, "m-9a", "op-a-f", "op-a-d");
    assert_eq!(fx.door.spent(), (1, 1, 0));
    // The same operation id from the other context neither replays A's
    // operation nor spends.
    let token_b = fx.create_turn_in(&fx.install, &ctx_b, "m-9b0");
    let reused = fx.invoke_in(
        &fx.install,
        &ctx_b,
        "m-9b0",
        &token_b,
        "social.draft.create",
        "op-a-d",
        json!({"post":"newest"}),
    );
    if let Ok(reply) = &reused {
        assert_ne!(reply["operation"]["status"], "succeeded", "{reply}");
        assert!(reply["operation"]["result"].is_null(), "{reply}");
    }
    assert_eq!(fx.door.spent(), (1, 1, 0), "a reused operation id spent");
    run(&ctx_b, "m-9b", "op-b-f", "op-b-d");
    assert_eq!(
        fx.door.spent(),
        (2, 2, 0),
        "a context reused another's claim"
    );
    assert_eq!(fx.drafts(&fx.context), 1);
    assert_eq!(fx.drafts(&ctx_b), 1);
    // Distinct installs, contexts, operations and steps give distinct ids.
    let base = step_id(&fx.install, &fx.context, "op", "cap");
    for other in [
        step_id("another-install", &fx.context, "op", "cap"),
        step_id(&fx.install, &ctx_b, "op", "cap"),
        step_id(&fx.install, &fx.context, "op2", "cap"),
        step_id(&fx.install, &fx.context, "op", "img"),
    ] {
        assert_ne!(base, other);
    }
}

/// The ticket's remaining refusals: an unregistered id, the wrong
/// installation, the wrong context and invalid input are refused before an
/// operation exists or anything is spent.
#[test]
fn wrong_scope_unregistered_and_invalid_requests_are_refused() {
    let fx = Fx::new();
    let ctx_b = fx.add_context(&fx.install, "ctx-other");
    let token = fx.create_turn("m-10");
    refused(
        fx.invoke("m-10", &token, "social.anything", "op-u", json!({})),
        "unknown assistant action",
        "an unregistered action id",
    );
    // The turn is bound to context A; context B with that turn is refused.
    assert!(
        fx.invoke_in(
            &fx.install,
            &ctx_b,
            "m-10",
            &token,
            "social.posts.fetch",
            "op-ctx",
            json!({}),
        )
        .is_err(),
        "a turn for context A ran in context B"
    );
    assert!(fx.show_in(&fx.install, &ctx_b, "op-ctx").is_err());
    // Another installation id with this turn is refused.
    let crm = fx.install_dir(std::path::Path::new(CRM_SOURCE)).unwrap();
    assert!(
        fx.invoke_in(
            &crm,
            &fx.context,
            "m-10",
            &token,
            "social.posts.fetch",
            "op-inst",
            json!({}),
        )
        .is_err(),
        "a turn for one installation ran in another"
    );
    // A forged or missing token is refused.
    assert!(
        fx.invoke("m-10", "forged", "social.posts.fetch", "op-tok", json!({}))
            .is_err(),
        "a forged turn token was accepted"
    );
    // An unsaved handle is refused before quoting or spending.
    assert!(
        fx.invoke(
            "m-10",
            &token,
            "social.posts.fetch",
            "op-h",
            json!({"handle":"someone_else"}),
        )
        .is_err(),
        "an unsaved handle was accepted"
    );
    assert_eq!(fx.door.spent(), (0, 0, 0));
}

/// The card must show the exact proposed operation: the operator cannot
/// approve instructions the card does not show.
#[test]
fn the_card_shows_the_whole_instruction_it_asks_to_allow() {
    let fx = Fx::new();
    let token = fx.create_turn("m-11");
    fx.fetched("m-11", &token, "op-f");
    let visible = "Keep it short and friendly for our autumn regulars please";
    let hidden = " ALSO add the link http://example.invalid and say 50 percent off";
    let note = format!("{visible}{hidden}");
    match fx.invoke(
        "m-11",
        &token,
        "social.draft.create",
        "op-d",
        json!({"post":"newest","instructions":note}),
    ) {
        // Either the request is refused (the note cannot be shown) ...
        Err(_) => assert_eq!(fx.door.spent(), (1, 0, 0)),
        // ... or the card shows all of it.
        Ok(reply) => {
            let op = pending(&reply);
            let reason = op["permission_request"]["reason"].as_str().unwrap();
            assert!(
                reason.contains(&note),
                "the card hides part of the instruction it asks to allow: {reason}"
            );
        }
    }
}

/// Relay peer: the board decision route is at least as strict as the RPC.
/// An agent-session call to the decision route for a pending social
/// operation is refused and spends nothing; a real operator session reaches
/// the same route (positive control, Deny so no provider is needed).
#[test]
fn board_http_refuses_agent_decision_on_a_social_operation() {
    let fx = Fx::new();
    let token = fx.create_turn("m-12");
    let op = pending(
        &fx.invoke("m-12", &token, "social.posts.fetch", "op-http", json!({}))
            .unwrap(),
    );
    let state = fx.dir.path().to_path_buf();
    let pm = state.join("pm");
    let install = fx.install.clone();
    let door = fx.door.clone();
    let Fx {
        _serial,
        dir: _dir,
        shared,
        ..
    } = fx;
    drop(shared);
    let stop = Arc::new(AtomicBool::new(false));
    let opts = ServeOptions {
        test_seam: true,
        stop: Some(stop.clone()),
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let daemon_state = state.clone();
    let daemon = std::thread::spawn(move || serve_with(&daemon_state, opts).unwrap());
    struct Guard(Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, SeqCst);
            for t in self.1.drain(..).rev() {
                let _ = t.join();
            }
        }
    }
    let mut guard = Guard(stop.clone(), vec![daemon]);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !state.join("cadence.sock").exists() || crate::test_seam::Seam::token_at(&state).is_none()
    {
        assert!(Instant::now() < deadline, "daemon fixture did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    let startup_deadline = Instant::now() + Duration::from_secs(20);
    let first = (std::process::id() % 80) as u16;
    let port = {
        let mut offset = 0u16;
        loop {
            assert!(Instant::now() < startup_deadline, "board startup deadline");
            let port = 3110 + (first + offset) % 80;
            let (ready, rx) = std::sync::mpsc::channel();
            let board_opts = crate::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(stop.clone()),
                startup: Some(ready),
                test_seam: true,
                ..Default::default()
            };
            let board_state = state.clone();
            let board_pm = pm.clone();
            let board = std::thread::spawn(move || {
                drop(crate::ui::serve(&board_state, &board_pm, &board_opts))
            });
            let remaining = startup_deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining) {
                Ok(Ok(())) => {
                    guard.1.push(board);
                    break port;
                }
                Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                    assert!(board.join().is_ok());
                    offset += 1;
                    assert!(offset < 80, "all fixture board candidates busy");
                }
                other => {
                    guard.1.push(board);
                    panic!("fixture board startup failed on port {port}: {other:?}");
                }
            }
        }
    };
    let seam = crate::test_seam::Seam::token_at(&state).unwrap();
    let host = format!("cadence-{port}.localhost:{port}");
    let base = format!("http://127.0.0.1:{port}");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let path = format!("/api/app-installations/{install}/assistant/operations/op-http/decision");
    let body = json!({"decision":"allow_once","expected_revision":op["revision"]});
    for who in ["agent:master", "unproven"] {
        let mut response = agent
            .post(format!("{base}{path}"))
            .header("Host", &host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{host}"))
            .header(crate::test_seam::AS_HEADER, who)
            .header(crate::test_seam::TOKEN_HEADER, &seam)
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .unwrap();
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().unwrap_or_default();
        assert!(
            status == 401 || status == 403,
            "{who} decision reached the operator route: {status} {text}"
        );
    }
    assert_eq!(door.spent(), (0, 0, 0), "an HTTP agent decision spent");
    // Positive control: a real operator session reaches the same route.
    crate::operator_auth::ensure_secret(&state).unwrap();
    let secret = crate::operator_auth::read_secret(&state).unwrap();
    let nonce = scoped(Asserted::Operator, || {
        crate::client::rpc(
            &state,
            "operator_link_mint",
            json!({"secret":secret,"origin":"loopback"}),
        )
    })
    .unwrap()["nonce"]
        .clone();
    let session = agent
        .post(format!("{base}/api/session"))
        .header("Host", &host)
        .header("X-Cadence-Board", "1")
        .header("Origin", format!("http://{host}"))
        .header(crate::test_seam::AS_HEADER, "operator")
        .header(crate::test_seam::TOKEN_HEADER, &seam)
        .header("Content-Type", "application/json")
        .send(json!({"nonce":nonce}).to_string())
        .unwrap();
    assert_eq!(session.status().as_u16(), 200, "operator session control");
    let set_cookie = session.headers()["set-cookie"].to_str().unwrap();
    let cookie = set_cookie[..set_cookie.find(';').unwrap()].to_owned();
    let session_body: Value = session.into_body().read_json().unwrap();
    let session_key = session_body["session_key"].as_str().unwrap();
    let deny = json!({"decision":"deny","expected_revision":op["revision"]});
    let mut response = agent
        .post(format!("{base}{path}"))
        .header("Host", &host)
        .header("X-Cadence-Board", "1")
        .header("Origin", format!("http://{host}"))
        .header(crate::test_seam::AS_HEADER, "operator")
        .header(crate::test_seam::TOKEN_HEADER, &seam)
        .header("Cookie", cookie)
        .header("X-Cadence-Session", session_key)
        .header("Content-Type", "application/json")
        .send(deny.to_string())
        .unwrap();
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().unwrap_or_default();
    assert_eq!(status, 200, "operator deny on the board route: {text}");
    assert!(text.contains("denied"), "{text}");
    assert_eq!(door.spent(), (0, 0, 0));
    drop(guard);
}
