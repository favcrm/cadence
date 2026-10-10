//! CAD-1332 acceptance check — written by the independent reviewer, not the
//! implementer. The implementer may not edit or weaken it.
//!
//! A real in-process daemon over the `--features test-seam` caller-identity
//! harness; the AgenticOS hosted publish door is a fake behind an HTTP
//! CONNECT proxy (`ALL_PROXY`), as in `cad1328_acceptance`.
//!
//! The rules it proves, from the ticket ("a same-request_id social effect
//! stage retry must return the stored effect"):
//! (a) Staging twice with one request_id returns the same effect (id,
//!     digest, approval) and leaves exactly one effect row.
//! (b) The same request_id with a changed revision/caption, or a changed
//!     binding/destination, is refused by the stored-effect comparison (not
//!     by an earlier guard) and no second row appears. Each stable field
//!     (install, context, draft, revision, caption, image, binding,
//!     toolkit, destination) is also refused at the store's own stage guard
//!     with otherwise realistic frozen material.
//! (c) A retry after the effect was posted, declined or refused returns the
//!     stored terminal effect unchanged: no re-arm, no new row, no send.
//! (d) An agent or unproven caller still cannot stage.
//! (e) Positive controls: a fresh request_id for the changed material stages
//!     a new effect, so a blanket refusal cannot pass.
#![cfg(feature = "test-seam")]

use cadence_agent::platform::deployments::DeploymentMetadata;
use cadence_agent::store::app_records::RecordStore;
use cadence_agent::store::app_social_drafts::EffectStage;
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon, operator_auth};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[path = "fixtures/aos_media_door.rs"]
mod aos_media_door;

const HOSTED: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"}]}"#;
const PREFIX: &str = "/v1/runtime/connectors/hosted-publish";
const AOS_CONN: &str = "conn_aos_1332";
const WORKSPACE: &str = "ws_1332";
const DEST: &str = "17841400008461332";
/// A second destination on the same hosted account (the changed-destination case).
const DEST2: &str = "17841400008462332";
const STANDING: &str = "dpq_standing_cad1332";
const HANDLE: &str = "juicysuite_crm";
const CAPTION: &str = "CAD-1332 caption";

/// The fake hosted door. Every request is logged as (method, path).
#[derive(Default)]
struct Door {
    media: aos_media_door::MediaDoor,
    seen: Mutex<Vec<(String, String)>>,
    /// `POST /publish` answers `processing` instead of `posted`.
    processing: AtomicBool,
    /// What `GET /publish/<key>/status` answers.
    status: Mutex<String>,
    /// key -> the last publish body, so status echoes its binding.
    published: Mutex<HashMap<String, Value>>,
}

impl Door {
    fn posts(&self, tail: &str) -> usize {
        let path = format!("{PREFIX}{tail}");
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p)| m == "POST" && p.split('?').next() == Some(path.as_str()))
            .count()
    }
    /// Every POST to the publish door (media import, preflight, publish).
    fn all_posts(&self) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p)| m == "POST" && p.starts_with(PREFIX))
            .count()
    }
}

fn fake_door() -> (String, Arc<Door>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let door = Arc::new(Door::default());
    *door.status.lock().unwrap() = "processing".into();
    let state = door.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let door = state.clone();
            std::thread::spawn(move || serve(stream, &door));
        }
    });
    (addr, door)
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
    door.seen
        .lock()
        .unwrap()
        .push((method.clone(), path.clone()));
    let sent: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (route, query) = path.split_once('?').unwrap_or((path.as_str(), ""));
    let query_value = |name: &str| -> Option<String> {
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix(&format!("{name}=")).map(str::to_owned))
    };
    let price = json!({"currency":"USD","scale":6,"amount":"0.002000"});
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
    let echo = |sent: &Value| {
        json!({
            "key": sent["key"],
            "destinationId": DEST,
            "captionDigest": sent["grant"]["captionDigest"],
            "imageDigest": sent["grant"]["imageDigest"],
        })
    };
    let hex = |bytes: &[u8]| {
        use sha2::Digest as _;
        format!("{:x}", sha2::Sha256::digest(bytes))
    };
    let status_route = route
        .strip_prefix(&format!("{PREFIX}/publish/"))
        .and_then(|rest| rest.strip_suffix("/status"));
    let reply = match (method.as_str(), route) {
        ("GET", "/v1/runtime/tools/read_instagram_posts") => json_reply(
            json!({"ok":true,"data":{"slug":"read_instagram_posts","displayName":"Read Instagram posts","effect":"read","chargePrecondition":"max_charge_minor@1","price":price,"unitPrice":null}}),
        ),
        ("POST", "/v1/runtime/tools/call") if sent["slug"] == "read_instagram_posts" => json_reply(
            json!({"ok":true,"data":{"slug":"read_instagram_posts","repeated":false,"price":price,"result":{"success":true,"status":"ok","user":{"username":HANDLE,"is_private":false},"items":[{"id":"post-1","code":"AbCd123","created_at":"2026-09-27T00:00:00Z","caption":{"text":"Door caption"}}]}}}),
        ),
        (m, r) if r.starts_with("/v1/runtime/media/") => {
            match door
                .media
                .reply(m, r, header("idempotency-key").as_deref(), &body)
            {
                Some((status, content_type, bytes)) => raw(status, content_type, &bytes),
                None => raw(404, "application/json", b"{}"),
            }
        }
        ("GET", r) if r == format!("{PREFIX}/destinations") => json_reply(
            json!({"ok":true,"data":{"version":"1","destinations":[{"connectionId":AOS_CONN,"toolkit":"instagram","displayName":"Harbour stills","destinationId":DEST,"status":"active","available":true,"publishable":true},{"connectionId":AOS_CONN,"toolkit":"instagram","displayName":"Harbour moving","destinationId":DEST2,"status":"active","available":true,"publishable":true}]}}),
        ),
        ("GET", r) if r == format!("{PREFIX}/publish/grants") => json_reply(
            json!({"ok":true,"data":{"grants":[{"kind":"standing","id":STANDING,"workspaceId":WORKSPACE,"connectionId":AOS_CONN,"destinationId":DEST,"toolkit":"instagram","expiresAt":null,"revokedAt":null}]}}),
        ),
        ("GET", _) if status_route.is_some() => {
            let key = status_route.unwrap();
            match door.published.lock().unwrap().get(key) {
                Some(sent) => {
                    let mut data = echo(sent);
                    data["version"] = json!("1");
                    data["state"] = json!(door.status.lock().unwrap().clone());
                    if data["state"] == "posted" {
                        data["permalink"] = json!("https://www.instagram.com/p/CAD1332/");
                    }
                    json_reply(json!({"ok":true,"data":data}))
                }
                None => raw(
                    404,
                    "application/json",
                    br#"{"ok":false,"error":{"code":"not_found","message":"no send under this key"}}"#,
                ),
            }
        }
        ("POST", r) if r == format!("{PREFIX}/media/import") => {
            let digest = query_value("digest").unwrap_or_default();
            let mime = header("content-type").unwrap_or_default();
            if hex(&body) != digest {
                json_reply(json!({"ok":false,"error":{"code":"digest_mismatch"}}))
            } else {
                json_reply(json!({"ok":true,"data":{
                    "mediaKey": format!("dp1.{WORKSPACE}.{AOS_CONN}.{}", &digest[..32]),
                    "connectionId": query_value("connectionId"),
                    "digest": digest, "mime": mime, "sizeBytes": body.len(),
                    "readBack": {"bytes": body.len(), "digest": digest},
                }}))
            }
        }
        ("POST", r) if r == format!("{PREFIX}/publish/preflight") => {
            let mut data = echo(&sent);
            data["decision"] = json!("approved");
            data["executable"] = json!(true);
            data["repeated"] = json!(false);
            data["reason"] = Value::Null;
            json_reply(json!({"ok":true,"data":data}))
        }
        ("POST", r) if r == format!("{PREFIX}/publish") => {
            if let Some(key) = sent["key"].as_str() {
                door.published
                    .lock()
                    .unwrap()
                    .insert(key.to_string(), sent.clone());
            }
            let mut data = echo(&sent);
            data["version"] = json!("1");
            data["result"] = if door.processing.load(SeqCst) {
                json!({"key": sent["key"], "decision": "approved", "executed": true,
                    "status": "processing", "repeated": false})
            } else {
                json!({"key": sent["key"], "decision": "approved", "executed": true,
                    "status": "posted", "permalink": "https://www.instagram.com/p/CAD1332/",
                    "repeated": false})
            };
            json_reply(json!({"ok":true,"data":data}))
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

const CHANGED: &str = "reused for changed frozen material";

/// One stable-field change applied to a copy of the stored frozen material.
type Change<'a> = Box<dyn Fn(&mut Value) + 'a>;

struct Fx {
    _serial: std::sync::MutexGuard<'static, ()>,
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    door: Arc<Door>,
    install: String,
    context: String,
    session: Value,
    action_token: Value,
}

/// One staged effect: the draft it froze and the request that staged it.
struct Staged {
    id: String,
    digest: String,
    draft: String,
    revision: i64,
    request: String,
}

impl Fx {
    fn dir(&self) -> std::path::PathBuf {
        self.root.path().join("s")
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
        let root = tempfile::Builder::new().prefix("c1332").tempdir().unwrap();
        cadence_agent::issue::Pm::init(&root.path().join("pm")).unwrap();
        let dir = root.path().join("s");
        std::fs::create_dir_all(&dir).unwrap();
        let (door_addr, door) = fake_door();
        let stop = Arc::new(AtomicBool::new(false));
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", root.path().join("pm").to_str().unwrap());
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
            ..Default::default()
        };
        cadence_agent::platform::local::register_at(
            &dir,
            &mut opts,
            dir.join("outbox"),
            "http://127.0.0.1:3010".into(),
        );
        std::env::set_var("ALL_PROXY", format!("http://{door_addr}"));
        std::env::set_var("CADENCE_TEST_MEDIA_POLL_MS", "20");
        std::env::set_var("CADENCE_TEST_MEDIA_DEADLINE_MS", "1500");
        let run_dir = dir.clone();
        let handle = std::thread::spawn(move || daemon::serve_with(&run_dir, opts));
        let deadline = Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&dir, "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&dir).is_none()
        {
            assert!(
                !handle.is_finished() && Instant::now() < deadline,
                "daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        std::env::remove_var("ALL_PROXY");
        let mut fx = Self {
            _serial: serial,
            root,
            stop,
            daemon: Some(handle),
            door,
            install: String::new(),
            context: String::new(),
            session: Value::Null,
            action_token: Value::Null,
        };
        fx.setup();
        fx
    }

    fn rpc_as(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(who, || client::rpc(&self.dir(), method, params))
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc_as(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }

    /// A screen action as `who`: the session credentials plus the action token.
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

    /// Social Content bound to the hosted account (source, image) and the
    /// Instagram destination (publication), plus a mounted screen session.
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
                "request_id": "ctx-1332"}),
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
            self.op(
                "app_binding_create",
                json!({"install_id": install, "context_id": context, "slot": slot,
                    "connection_id": connection, "request_id": format!("bind-{slot}")}),
            );
        }
        self.install = install;
        self.context = context;
        self.set_destination(DEST, "Harbour stills");
        RecordStore::open(&self.dir(), &self.install)
            .unwrap()
            .app_social_sources_save(&self.context, 0, &[HANDLE.to_owned()], "src-1332")
            .unwrap();
        operator_auth::ensure_secret(&self.dir()).unwrap();
        let secret = operator_auth::read_secret(&self.dir()).unwrap();
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
            json!({"install_id": self.install, "context_id": self.context, "tag": "feed",
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

    /// The operator points the publication binding at `destination`.
    fn set_destination(&self, destination: &str, label: &str) {
        let listed = self.op(
            "app_binding_list",
            json!({"install_id": self.install, "context_id": self.context}),
        );
        let binding = listed["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["slot"] == "publication")
            .unwrap_or_else(|| panic!("no publication binding: {listed}"))
            .clone();
        self.op(
            "app_binding_publish_set",
            json!({"install_id": self.install, "binding_id": binding["id"],
                "expected_revision": binding["revision"],
                "destination_id": destination, "destination_label": label,
                "toolkit": "instagram", "timezone": "Asia/Hong_Kong",
                "grant_id": "dpq_typed_on_binding"}),
        );
    }

    /// Fetch, draft a caption, generate and attach an image. Returns
    /// `(draft_id, revision)`.
    fn draft_with_image(&self, tag: &str) -> (String, i64) {
        let fetched = self
            .screen(
                "app_tool_invoke",
                json!({"tool_alias": "instagram.read", "input": {"handle": HANDLE},
                    "request_id": format!("fetch-{tag}")}),
            )
            .unwrap_or_else(|e| panic!("fetch: {e}"));
        let receipt = fetched["receipt"]["id"].as_str().unwrap().to_string();
        let post = fetched["receipt"]["result"]["posts"][0]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("no normalized post: {fetched}"))
            .to_string();
        let created = self
            .screen(
                "app_social_draft_create",
                json!({"tool_alias": "social.draft", "request_id": format!("draft-{tag}"),
                    "caption": CAPTION,
                    "source": {"kind": "tool_receipt", "receipt_id": receipt, "post_id": post}}),
            )
            .unwrap_or_else(|e| panic!("draft create: {e}"));
        let draft = created["draft_id"].as_str().unwrap().to_string();
        self.screen(
            "app_tool_invoke",
            json!({"tool_alias": "social.draft", "input": {},
                "request_id": format!("image-{tag}"),
                "generation_scope": {"operation": "image", "draft_id": draft, "revision": 1}}),
        )
        .unwrap_or_else(|e| panic!("generate image: {e}"));
        let deadline = Instant::now() + Duration::from_secs(60);
        let asset = loop {
            let listed = self
                .screen(
                    "app_social_draft_list",
                    json!({"tool_alias": "social.draft"}),
                )
                .unwrap_or_else(|e| panic!("list: {e}"));
            let done = listed["generation_intents"].as_array().and_then(|rows| {
                rows.iter().find(|row| {
                    row["scope"]["draft_id"] == draft
                        && row["scope"]["operation"] == "image"
                        && row["state"] == "completed"
                })
            });
            if let Some(row) = done {
                break row["receipt_id"].as_str().unwrap().to_string();
            }
            assert!(Instant::now() < deadline, "image never completed: {listed}");
            std::thread::sleep(Duration::from_millis(50));
        };
        let attached = self
            .screen(
                "app_social_draft_update",
                json!({"tool_alias": "social.draft", "request_id": format!("attach-{tag}"),
                    "draft_id": draft, "expected_revision": 1, "caption": CAPTION,
                    "asset_id": asset}),
            )
            .unwrap_or_else(|e| panic!("attach image: {e}"));
        (draft, attached["revision"].as_i64().unwrap())
    }

    /// The real stage path (`app_effect_stage` with a social draft proof).
    fn stage_as(
        &self,
        who: Asserted,
        draft: &str,
        revision: i64,
        request: &str,
    ) -> cadence_agent::Result<Value> {
        self.screen_as(
            who,
            "app_effect_stage",
            json!({"tool_alias": "social.draft",
                "proof": {"kind": "social_draft", "draft_id": draft, "revision": revision},
                "request_id": request}),
        )
    }

    fn stage(&self, draft: &str, revision: i64, request: &str) -> cadence_agent::Result<Value> {
        self.stage_as(Asserted::Operator, draft, revision, request)
    }

    fn staged(&self, tag: &str) -> Staged {
        let (draft, revision) = self.draft_with_image(tag);
        let request = format!("stage-{tag}");
        let staged = self
            .stage(&draft, revision, &request)
            .unwrap_or_else(|e| panic!("stage: {e}"));
        let staged = Staged {
            id: staged["effect"]["effect_id"]
                .as_str()
                .unwrap_or_else(|| panic!("staged effect: {staged}"))
                .to_string(),
            digest: staged["effect"]["digest"].as_str().unwrap().to_string(),
            draft,
            revision,
            request,
        };
        assert_eq!(self.state(&staged.id), "waiting");
        staged
    }

    fn records(&self) -> RecordStore {
        RecordStore::open(&self.dir(), &self.install).unwrap()
    }

    fn state(&self, id: &str) -> String {
        self.records().app_social_effect_show(id).unwrap()["effect"]["state"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Every effect row of the context.
    fn effect_rows(&self) -> usize {
        self.records()
            .app_social_effect_list(&self.context)
            .unwrap()["effects"]
            .as_array()
            .unwrap()
            .len()
    }

    fn edit_caption(&self, draft: &str, expected: i64, caption: &str) -> i64 {
        self.screen(
            "app_social_draft_update",
            json!({"tool_alias": "social.draft", "request_id": format!("edit-{draft}-{expected}"),
                "draft_id": draft, "expected_revision": expected, "caption": caption}),
        )
        .unwrap_or_else(|e| panic!("edit caption: {e}"))["revision"]
            .as_i64()
            .unwrap()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.daemon.take() {
            let _ = handle.join();
        }
    }
}

fn refused_with(result: cadence_agent::Result<Value>, reason: &str, what: &str) {
    let refusal = result.expect_err(what).to_string();
    assert!(
        refusal.contains(reason),
        "{what}: refused for another reason: {refusal}"
    );
}

/// (a) (b) (d) (e) through the real daemon stage path.
#[test]
fn a_same_request_retry_returns_the_stored_effect_and_changed_material_is_refused() {
    let fx = Fx::start();
    let first = fx.staged("a");
    assert_eq!(fx.effect_rows(), 1);
    let stored = fx.records().app_social_effect_show(&first.id).unwrap()["effect"].clone();

    // (d) An agent or unproven caller cannot stage, not even a retry.
    for who in [Asserted::Agent("cc13-worker".into()), Asserted::Unproven] {
        refused_with(
            fx.stage_as(who.clone(), &first.draft, first.revision, &first.request),
            "operator",
            &format!("{who:?} staged a retry"),
        );
        refused_with(
            fx.stage_as(who.clone(), &first.draft, first.revision, "stage-agent"),
            "operator",
            &format!("{who:?} staged a new effect"),
        );
    }
    assert_eq!(fx.effect_rows(), 1, "a refused caller wrote a row");

    // (a) The same request_id, the same material: the stored effect, twice.
    for _ in 0..2 {
        let again = fx
            .stage(&first.draft, first.revision, &first.request)
            .unwrap_or_else(|e| panic!("same-request retry refused: {e}"));
        let effect = &again["effect"];
        assert_eq!(effect["effect_id"], first.id.as_str(), "{again}");
        assert_eq!(effect["digest"], first.digest.as_str(), "{again}");
        assert_eq!(effect["approval_id"], stored["approval_id"], "{again}");
        assert_eq!(effect["authority"], stored["authority"], "{again}");
        assert_eq!(effect["state"], "waiting", "{again}");
    }
    assert_eq!(fx.effect_rows(), 1, "a retry wrote a second row");

    // (b) A changed caption is a new revision; under the old request_id it is
    // refused by the stored-effect comparison, not by the stale-revision check.
    let edited = fx.edit_caption(&first.draft, first.revision, "CAD-1332 changed caption");
    assert!(edited > first.revision);
    refused_with(
        fx.stage(&first.draft, edited, &first.request),
        CHANGED,
        "changed revision and caption under one request_id",
    );
    assert_eq!(fx.effect_rows(), 1, "a refused retry wrote a row");

    // (e) Not vacuous: the changed draft stages under a fresh request_id.
    let second_request = "stage-a-edited";
    let second = fx
        .stage(&first.draft, edited, second_request)
        .unwrap_or_else(|e| panic!("fresh request for the edited draft refused: {e}"));
    let second_id = second["effect"]["effect_id"].as_str().unwrap().to_string();
    assert_ne!(second_id, first.id);
    assert_eq!(fx.effect_rows(), 2);

    // (b) A changed binding and destination: the operator points the
    // publication at another destination. The draft is unchanged, so only
    // the stored-effect comparison can refuse the old request_id.
    fx.set_destination(DEST2, "Harbour moving");
    refused_with(
        fx.stage(&first.draft, edited, second_request),
        CHANGED,
        "changed binding and destination under one request_id",
    );
    assert_eq!(fx.effect_rows(), 2, "a refused retry wrote a row");
    // (e) Not vacuous: the new destination stages under a fresh request_id.
    let third = fx
        .stage(&first.draft, edited, "stage-a-moved")
        .unwrap_or_else(|e| panic!("fresh request for the new destination refused: {e}"));
    assert_eq!(third["effect"]["authority"]["destination_id"], DEST2);
    assert_eq!(fx.effect_rows(), 3);
    assert_eq!(fx.door.all_posts(), 0, "staging reached the publish door");
}

/// (b) at the store's own stage guard: each stable field, changed alone
/// under one request_id, is refused; fresh effect, approval and idempotency
/// identifiers alone are not.
#[test]
fn every_stable_field_changed_under_one_request_id_is_refused_at_the_store() {
    let fx = Fx::start();
    let first = fx.staged("f");
    let records = fx.records();
    let stored = records.app_social_effect_show(&first.id).unwrap()["effect"]["authority"].clone();
    let fresh = |n: usize| {
        let mut frozen = stored.clone();
        let tail = format!("{n:032x}");
        frozen["effect_id"] = json!(format!("sfx_retry_{tail}"));
        frozen["approval_id"] = json!(format!("social-approval-{tail}"));
        frozen["idempotency_key"] = json!(format!("social_{tail}"));
        frozen
    };
    let stage = |frozen: &Value| {
        let effect_id = frozen["effect_id"].as_str().unwrap().to_string();
        let approval = frozen["approval_id"].as_str().unwrap().to_string();
        records.app_social_effect_stage(
            &fx.context,
            &EffectStage {
                draft: &first.draft,
                revision: first.revision,
                request: &first.request,
                effect_id: &effect_id,
                frozen,
                approval: &approval,
            },
        )
    };

    // Control: only the fresh identifiers differ, so the stored effect returns.
    let same = stage(&fresh(0)).unwrap_or_else(|e| panic!("same material refused: {e}"));
    assert_eq!(same["effect"]["effect_id"], first.id.as_str());
    assert_eq!(same["effect"]["digest"], first.digest.as_str());

    let bump = |v: &Value| json!(v.as_i64().unwrap() + 1);
    let other = |v: &Value| json!(format!("{}-other", v.as_str().unwrap_or("none")));
    let changes: Vec<(&str, Change<'_>)> = vec![
        (
            "install_id",
            Box::new(|f| f["install_id"] = other(&f["install_id"])),
        ),
        (
            "context_id",
            Box::new(|f| f["context_id"] = other(&f["context_id"])),
        ),
        (
            "draft_id",
            Box::new(|f| f["draft_id"] = other(&f["draft_id"])),
        ),
        (
            "revision",
            Box::new(|f| f["revision"] = bump(&f["revision"])),
        ),
        (
            "caption",
            Box::new(|f| {
                f["caption"] = json!("CAD-1332 another caption");
                f["caption_digest"] = other(&f["caption_digest"]);
            }),
        ),
        (
            "image_digest",
            Box::new(|f| f["image_digest"] = json!("0".repeat(64))),
        ),
        (
            "binding digest",
            Box::new(|f| f["binding"]["digest"] = other(&f["binding"]["digest"])),
        ),
        ("toolkit", Box::new(|f| f["toolkit"] = json!("facebook"))),
        (
            "destination_id",
            Box::new(|f| f["destination_id"] = json!(DEST2)),
        ),
    ];
    for (n, (field, change)) in changes.iter().enumerate() {
        let mut frozen = fresh(n + 1);
        change(&mut frozen);
        refused_with(
            stage(&frozen),
            CHANGED,
            &format!("changed {field} replayed"),
        );
    }
    assert_eq!(fx.effect_rows(), 1, "a refused retry wrote a row");
    assert_eq!(fx.state(&first.id), "waiting");
}

/// (c) A retry after a terminal outcome returns the stored effect as it is.
#[test]
fn a_retry_after_a_terminal_effect_returns_it_and_never_re_arms_or_sends() {
    let fx = Fx::start();
    let posted = fx.staged("p");
    let declined = fx.staged("d");
    let refused = fx.staged("r");

    // Posted through the operator's own confirm.
    fx.op(
        "app_effect_confirm_publish",
        json!({"effect_id": posted.id, "digest": posted.digest}),
    );
    assert_eq!(fx.state(&posted.id), "posted");
    // Declined by the operator.
    fx.op(
        "app_effect_decide",
        json!({"effect_id": declined.id, "digest": declined.digest, "decision": "decline"}),
    );
    assert_eq!(fx.state(&declined.id), "declined");
    // Refused by the provider (store transitions, as the send path records it).
    let records = fx.records();
    records
        .app_social_effect_decide(&refused.id, &refused.digest, true)
        .unwrap();
    records
        .app_social_effect_claim_send(&refused.id, &refused.digest)
        .unwrap()
        .expect("claim");
    records
        .app_social_effect_finish(&refused.id, "refused", &json!({"reason": "provider"}))
        .unwrap();
    assert_eq!(fx.state(&refused.id), "refused");

    let rows = fx.effect_rows();
    let posts = fx.door.all_posts();
    let publishes = fx.door.posts("/publish");
    assert_eq!(publishes, 1);
    let mut before: HashMap<String, Value> = HashMap::new();
    for s in [&posted, &declined, &refused] {
        before.insert(
            s.id.clone(),
            records.app_social_effect_show(&s.id).unwrap()["effect"].clone(),
        );
    }
    for (s, state) in [
        (&posted, "posted"),
        (&declined, "declined"),
        (&refused, "refused"),
    ] {
        let again = fx
            .stage(&s.draft, s.revision, &s.request)
            .unwrap_or_else(|e| panic!("{state} retry refused: {e}"));
        assert_eq!(again["effect"]["effect_id"], s.id.as_str(), "{again}");
        assert_eq!(again["effect"]["state"], state, "{again}");
        assert_eq!(
            again["effect"], before[&s.id],
            "{state} retry changed the effect"
        );
        assert_eq!(fx.state(&s.id), state, "{state} effect re-armed");
    }
    // A declined effect stays unpublishable after the retry.
    refused_with(
        fx.rpc_as(
            Asserted::Operator,
            "app_effect_confirm_publish",
            json!({"effect_id": declined.id, "digest": declined.digest}),
        ),
        "",
        "a declined effect was published after a retry",
    );
    assert_eq!(fx.state(&declined.id), "declined");
    assert_eq!(fx.effect_rows(), rows, "a terminal retry wrote a row");
    assert_eq!(
        fx.door.all_posts(),
        posts,
        "a terminal retry reached the door"
    );
    assert_eq!(fx.door.posts("/publish"), publishes, "posted twice");
}
