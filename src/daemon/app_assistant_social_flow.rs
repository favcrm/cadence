//! CAD-1327 implementer tests: the real Social Content assistant flow in the
//! daemon test seam, against the AgenticOS media door in its real job shape
//! (`tests/fixtures/aos_media_door.rs`) plus a read and a text door.
//!
//! These cover the author's own build (the positive flow, the confirmed
//! price ceiling and replay). The reviewer-written refusal check is separate
//! and is not edited here.
#![cfg(feature = "test-seam")]

use super::*;
use crate::store::app_records::RecordStore;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[path = "../../tests/fixtures/aos_media_door.rs"]
mod aos_media_door;

const HOSTED: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@4","transport":"hosted-media-lease@1"}]}"#;
const HANDLE: &str = "juicysuite_crm";
const MASTER_GENERATION: &str = "0123456789abcdef0123456789abcdef";
/// The descriptor the Social Content app ships (second PR); the registry
/// admits exactly this and nothing else.
const DESCRIPTOR: &str = r#"{
  "contract": "app-assistant/v1",
  "app": "social-content",
  "actions": [
    {
      "id": "social.posts.fetch",
      "description": "Read the recent public posts of a saved Instagram account (a small metered cost, confirmed first).",
      "input_schema": { "type": "object", "properties": { "handle": { "type": "string", "minLength": 1, "maxLength": 64 } }, "required": [], "additionalProperties": false },
      "effect": "read",
      "confirmation": "permission_required",
      "availability": "requires_context"
    },
    {
      "id": "social.draft.create",
      "description": "Draft a caption, and optionally one image, from a fetched post. Creates an editable draft only; nothing is published.",
      "input_schema": { "type": "object", "properties": { "post": { "type": "string", "minLength": 1, "maxLength": 128 }, "instructions": { "type": "string", "maxLength": 500 }, "with_image": { "type": "boolean" } }, "required": ["post"], "additionalProperties": false },
      "effect": "draft",
      "confirmation": "permission_required",
      "availability": "requires_context"
    },
    {
      "id": "social.drafts.status",
      "description": "Show which drafts are ready, waiting or failed.",
      "input_schema": { "type": "object", "properties": {}, "required": [], "additionalProperties": false },
      "effect": "read",
      "confirmation": "none",
      "availability": "requires_context"
    }
  ]
}
"#;

struct Door {
    media: aos_media_door::MediaDoor,
    text_price: Mutex<String>,
    text_calls: AtomicUsize,
    reads: AtomicUsize,
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
    let read_price = json!({"currency":"USD","scale":6,"amount":"0.002000"});
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
            json_reply(
                json!({"ok":true,"data":{"slug":"generate_text","repeated":false,"price":text_price,"result":{"text":"秋日菜單登場，歡迎品嚐。","finishReason":"stop","usage":{"inputTokens":9,"outputTokens":5,"totalTokens":14,"cachedInputTokens":null,"reasoningOutputTokens":null}}}}),
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

/// `ALL_PROXY` is process-global: one fixture at a time.
static SERIAL: Mutex<()> = Mutex::new(());

struct Fx {
    _serial: std::sync::MutexGuard<'static, ()>,
    dir: tempfile::TempDir,
    shared: Arc<Shared>,
    door: Arc<Door>,
    install: String,
    context: String,
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
    fn records(&self) -> RecordStore {
        RecordStore::open(self.dir.path(), &self.install).unwrap()
    }

    fn new() -> Self {
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
        let dir = tempfile::Builder::new().prefix("c1327").tempdir().unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let door = Arc::new(Door {
            media: aos_media_door::MediaDoor::new(),
            text_price: Mutex::new("0.002000".into()),
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
        let mut opts = ServeOptions {
            provider_deployments: Some(
                crate::platform::deployments::DeploymentMetadata::parse(HOSTED.as_bytes()).unwrap(),
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
        std::env::set_var("ALL_PROXY", format!("http://{addr}"));
        std::env::set_var("CADENCE_TEST_MEDIA_POLL_MS", "20");
        std::env::set_var("CADENCE_TEST_MEDIA_DEADLINE_MS", "1500");
        crate::platform::agenticos_external::attach(&mut opts).unwrap();
        let shared = Shared::new(dir.path(), &opts).unwrap();
        std::env::remove_var("ALL_PROXY");
        let cwd = dir.path().to_str().unwrap();
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
                    thread_id: "cad1327-thread".into(),
                    session_id: "cad1327-session".into(),
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
        fx.setup(true);
        fx
    }

    fn setup(&mut self, with_descriptor: bool) {
        let app = self.dir.path().join("app");
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let manifest_dir = std::path::Path::new(manifest_dir);
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
        if with_descriptor {
            std::fs::write(app.join("app-assistant.json"), DESCRIPTOR).unwrap();
        }
        let installed = self.operator(
            "app_workspace_install",
            json!({"source": app.to_str().unwrap()}),
        );
        let install = installed["install_id"].as_str().unwrap().to_string();
        self.operator(
            "app_local_install_approve",
            json!({"install_id": install, "digest": installed["digest"]}),
        );
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
            json!({"install_id": install, "label": "EF", "input_defaults": {}, "request_id": "ctx-1327"}),
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
                    "connection_id": hosted, "request_id": format!("bind-{slot}")}),
            );
        }
        RecordStore::open(self.dir.path(), &install)
            .unwrap()
            .app_social_sources_save(&context, 0, &[HANDLE.to_owned()], "src-1327")
            .unwrap();
        self.install = install;
        self.context = context;
    }

    fn create_turn(&self, message: &str) -> String {
        let params = json!({"alias":"master", "install_id":self.install,
            "context_id":self.context, "general":true});
        self.operator("conversation_create", params);
        self.operator(
            "thread_send",
            json!({"alias":"master", "text":"Draft a post from the newest",
                "message":message, "app":{"install_id":self.install,"context_id":self.context}}),
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

    fn invoke(
        &self,
        message: &str,
        token: &str,
        action: &str,
        op: &str,
        input: Value,
    ) -> Result<Value> {
        self.call(
            Asserted::Agent("master".into()),
            "app_assistant_invoke",
            json!({"install_id":self.install,"context_id":self.context,"message":message,
                "token":token,"action_id":action,"operation_id":op,"input":input}),
        )
    }

    fn decide(&self, who: Asserted, op: &Value) -> Result<Value> {
        self.call(
            who,
            "app_assistant_decision",
            json!({"install_id":self.install,"context_id":self.context,
                "operation_id":op["id"],"decision":"allow_once","expected_revision":op["revision"]}),
        )
    }

    fn wait_image(&self, draft: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(40);
        loop {
            let row = self
                .records()
                .app_social_draft_show(&self.context, draft)
                .unwrap();
            if row["asset_id"].is_string() {
                return row;
            }
            assert!(Instant::now() < deadline, "image never attached: {row}");
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

/// The ticket flow: fetch (confirmed, metered), then draft the newest post
/// with an image (confirmed with its cost), then the status read. The draft
/// is real, carries the generated caption, and gets its image through the
/// queued job; the result card references the draft.
#[test]
fn chat_fetches_drafts_with_an_image_and_reports_status() {
    let fx = Fx::new();
    let token = fx.create_turn("m-flow");

    let op = pending(
        &fx.invoke(
            "m-flow",
            &token,
            "social.posts.fetch",
            "op-fetch",
            json!({}),
        )
        .unwrap(),
    );
    assert!(
        op["permission_request"]["reason"]
            .as_str()
            .unwrap()
            .contains("USD 0.002"),
        "{op}"
    );
    assert_eq!(
        fx.door.reads.load(SeqCst),
        0,
        "nothing is spent before Allow"
    );
    let done = fx.decide(Asserted::Operator, &op).unwrap();
    assert_eq!(done["operation"]["status"], "succeeded", "{done}");
    assert_eq!(done["operation"]["result"]["newest_post_id"], "post-new");
    assert_eq!(fx.door.reads.load(SeqCst), 1);

    let op = pending(
        &fx.invoke(
            "m-flow",
            &token,
            "social.draft.create",
            "op-draft",
            json!({"post":"newest","instructions":"Keep it short","with_image":true}),
        )
        .unwrap(),
    );
    assert_eq!(op["permission_request"]["preview"]["post_id"], "post-new");
    assert!(op["permission_request"]["reason"]
        .as_str()
        .unwrap()
        .contains("image"));
    assert_eq!(
        fx.door.text_calls.load(SeqCst),
        0,
        "nothing is spent before Allow"
    );
    assert_eq!(fx.door.media.job_count(), 0);
    let done = fx.decide(Asserted::Operator, &op).unwrap();
    assert_eq!(done["operation"]["status"], "succeeded", "{done}");
    let draft = done["operation"]["result"]["draft_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        done["operation"]["resource_refs"][0]["kind"],
        "social_draft"
    );
    assert_eq!(done["operation"]["resource_refs"][0]["id"], draft.as_str());
    assert_eq!(done["operation"]["result"]["state"], "making_image");
    let shown = fx
        .records()
        .app_social_draft_show(&fx.context, &draft)
        .unwrap();
    assert_eq!(shown["caption"], "秋日菜單登場，歡迎品嚐。");
    assert_eq!(fx.door.text_calls.load(SeqCst), 1);
    let attached = fx.wait_image(&draft);
    assert!(attached["asset_id"].is_string());
    assert_eq!(fx.door.media.job_count(), 1, "one image job");

    let status = fx
        .invoke(
            "m-flow",
            &token,
            "social.drafts.status",
            "op-status",
            json!({}),
        )
        .unwrap();
    assert_eq!(status["operation"]["status"], "succeeded", "{status}");
    assert_eq!(status["operation"]["result"]["ready"], 1);
    assert_eq!(
        status["operation"]["result"]["drafts"][0]["image"],
        "attached"
    );
}

/// Exactly once: a repeated operation id replays the same operation, a second
/// decision is refused, and neither adds a provider call or a charge.
#[test]
fn replay_and_second_decision_charge_nothing_more() {
    let fx = Fx::new();
    let token = fx.create_turn("m-replay");
    fx.operator(
        "app_assistant_operations",
        json!({"install_id":fx.install,"context_id":fx.context}),
    );
    let fetch = pending(
        &fx.invoke("m-replay", &token, "social.posts.fetch", "op-f", json!({}))
            .unwrap(),
    );
    fx.decide(Asserted::Operator, &fetch).unwrap();
    let op = pending(
        &fx.invoke(
            "m-replay",
            &token,
            "social.draft.create",
            "op-d",
            json!({"post":"newest"}),
        )
        .unwrap(),
    );
    let done = fx.decide(Asserted::Operator, &op).unwrap();
    assert_eq!(done["operation"]["status"], "succeeded", "{done}");
    assert_eq!(fx.door.text_calls.load(SeqCst), 1);
    let drafts = fx.records().app_social_draft_list(&fx.context).unwrap();
    // The same operation id replays the finished operation, not a new run.
    let again = fx
        .invoke(
            "m-replay",
            &token,
            "social.draft.create",
            "op-d",
            json!({"post":"newest"}),
        )
        .unwrap();
    assert_eq!(again["operation"]["status"], "succeeded", "{again}");
    // A second Allow on the spent operation is refused.
    assert!(fx.decide(Asserted::Operator, &op).is_err());
    assert_eq!(fx.door.text_calls.load(SeqCst), 1, "no second charge");
    assert_eq!(fx.door.reads.load(SeqCst), 1);
    assert_eq!(
        fx.records().app_social_draft_list(&fx.context).unwrap(),
        drafts
    );
}

/// A price above what the operator was shown is refused and shown; no draft
/// is created and nothing is charged.
#[test]
fn a_price_above_the_confirmed_estimate_is_refused_and_shown() {
    let fx = Fx::new();
    let token = fx.create_turn("m-price");
    let fetch = pending(
        &fx.invoke("m-price", &token, "social.posts.fetch", "op-f", json!({}))
            .unwrap(),
    );
    fx.decide(Asserted::Operator, &fetch).unwrap();
    let op = pending(
        &fx.invoke(
            "m-price",
            &token,
            "social.draft.create",
            "op-d",
            json!({"post":"newest"}),
        )
        .unwrap(),
    );
    *fx.door.text_price.lock().unwrap() = "0.900000".into();
    let done = fx.decide(Asserted::Operator, &op).unwrap();
    assert_eq!(done["operation"]["status"], "failed", "{done}");
    assert!(
        done["operation"]["summary"]
            .as_str()
            .unwrap()
            .contains("price went above"),
        "{done}"
    );
    assert_eq!(fx.door.text_calls.load(SeqCst), 0);
    assert!(
        fx.records().app_social_draft_list(&fx.context).unwrap()["drafts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// Only the operator decides a paid action: an agent caller is refused and
/// the operation stays pending with nothing spent.
#[test]
fn an_agent_cannot_allow_its_own_paid_action() {
    let fx = Fx::new();
    let token = fx.create_turn("m-agent");
    let op = pending(
        &fx.invoke("m-agent", &token, "social.posts.fetch", "op-f", json!({}))
            .unwrap(),
    );
    assert!(fx.decide(Asserted::Agent("master".into()), &op).is_err());
    assert_eq!(fx.door.reads.load(SeqCst), 0);
    let shown = fx.operator(
        "app_assistant_operation_operator_show",
        json!({"install_id":fx.install,"context_id":fx.context,"operation_id":"op-f"}),
    );
    assert_eq!(shown["operation"]["status"], "pending_permission");
    // Only the three registered actions exist for this app: publish, send,
    // discard and another app's actions never run, declared or not.
    for (n, action) in [
        "social.draft.publish",
        "social.draft.discard",
        "customers.search",
    ]
    .into_iter()
    .enumerate()
    {
        let result = fx.invoke("m-agent", &token, action, &format!("op-x{n}"), json!({}));
        assert!(result.is_err(), "{action} was accepted: {result:?}");
    }
}
