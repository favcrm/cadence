//! CAD-1304: an approved Instagram draft WITH a generated image publishes
//! through the hosted door, and a changed image or caption after approval
//! sends nothing.
//!
//! Everything runs through the daemon's public RPCs, the way Social Content
//! does: fetch a saved handle's posts, create a draft from that receipt,
//! generate the image (the hosted media door returns the artifact, the
//! daemon retains it in custody), attach it, stage the publish effect with
//! the typed draft proof, approve, and `app_effect_publish_now`. The
//! `api.internal` door sits behind an HTTP CONNECT proxy (`ALL_PROXY`), as in
//! `cad1291_acceptance`, and answers with the AgenticOS contract shapes
//! (`packages/contracts/src/device-publish.ts`): media import verifies the
//! body against the `digest` query and mints
//! `dp1.<workspace>.<connection>.<digest[..32]>` (32 hex characters; the
//! contract fixture's 31 is a typo), preflight and publish echo the binding.
#![cfg(feature = "test-seam")]

use cadence_agent::platform::deployments::DeploymentMetadata;
use cadence_agent::store::app_records::RecordStore;
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[path = "fixtures/aos_media_door.rs"]
mod aos_media_door;

const HOSTED: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"}]}"#;
const PREFIX: &str = "/v1/runtime/connectors/hosted-publish";
const AOS_CONN: &str = "conn_aos_1304";
const WORKSPACE: &str = "ws_1304";
const DEST: &str = "17841400008460056";
const STANDING: &str = "dpq_standing_cad1304";
const HANDLE: &str = "juicysuite_crm";
const CAPTION: &str = "Instagram image caption";

/// One request the door saw: method, path and body length.
type Seen = Arc<Mutex<Vec<(String, String)>>>;

#[derive(Default)]
struct Door {
    /// The AgenticOS media door in its real job shape (CAD-1315).
    media: aos_media_door::MediaDoor,
    /// Digest query and body hash of every media import, in order.
    imports: Mutex<Vec<(String, String)>>,
    /// Publish and preflight bodies, in order.
    sends: Mutex<Vec<Value>>,
}

/// A distinct 1x1 PNG per `n`.
fn png(n: u8) -> Vec<u8> {
    use image::ImageEncoder as _;
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(&[n], 1, 1, image::ExtendedColorType::L8)
        .unwrap();
    bytes
}

fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn fake_door() -> (String, Seen, Arc<Door>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let seen: Seen = Arc::default();
    let state = Arc::new(Door::default());
    let (log, door) = (seen.clone(), state.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (log, door) = (log.clone(), door.clone());
            std::thread::spawn(move || serve(stream, &log, &door));
        }
    });
    (addr, seen, state)
}

fn serve(stream: std::net::TcpStream, log: &Seen, door: &Door) {
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
    log.lock().unwrap().push((method.clone(), path.clone()));
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
    let reply = match (method.as_str(), route) {
        // ---- source read (generic tool door) ----
        ("GET", "/v1/runtime/tools/read_instagram_posts") => json_reply(
            json!({"ok":true,"data":{"slug":"read_instagram_posts","displayName":"Read Instagram posts","effect":"read","chargePrecondition":"max_charge_minor@1","price":price,"unitPrice":null}}),
        ),
        ("POST", "/v1/runtime/tools/call") if sent["slug"] == "read_instagram_posts" => json_reply(
            json!({"ok":true,"data":{"slug":"read_instagram_posts","repeated":false,"price":price,"result":{"success":true,"status":"ok","user":{"username":HANDLE,"is_private":false},"items":[{"id":"post-1","code":"AbCd123","created_at":"2026-09-27T00:00:00Z","caption":{"text":"Door caption"}}]}}}),
        ),
        // ---- image generation (media door) ----
        (m, r) if r.starts_with("/v1/runtime/media/") => {
            match door
                .media
                .reply(m, r, header("idempotency-key").as_deref(), &body)
            {
                // CAD-1315: full Content-Length, body cut short, then close.
                Some((200, content_type, bytes))
                    if door.media.truncating() && r.contains("/artifacts/") =>
                {
                    let mut out = raw(200, content_type, &bytes);
                    out.truncate(out.len() - bytes.len() + 1024);
                    out
                }
                Some((status, content_type, bytes)) => raw(status, content_type, &bytes),
                None => raw(404, "application/json", b"{}"),
            }
        }
        // ---- hosted publish door ----
        ("GET", r) if r == format!("{PREFIX}/destinations") => json_reply(
            json!({"ok":true,"data":{"version":"1","destinations":[{"connectionId":AOS_CONN,"toolkit":"instagram","displayName":"Harbour stills","destinationId":DEST,"status":"active","available":true,"publishable":true}]}}),
        ),
        ("GET", r) if r == format!("{PREFIX}/publish/grants") => json_reply(
            json!({"ok":true,"data":{"grants":[{"kind":"standing","id":STANDING,"workspaceId":WORKSPACE,"connectionId":AOS_CONN,"destinationId":DEST,"toolkit":"instagram","dailyCap":20,"remainingToday":20,"expiresAt":null,"revokedAt":null}]}}),
        ),
        ("POST", r) if r == format!("{PREFIX}/media/import") => {
            let digest = query_value("digest").unwrap_or_default();
            let mime = header("content-type").unwrap_or_default();
            door.imports
                .lock()
                .unwrap()
                .push((digest.clone(), hex(&body)));
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
            door.sends.lock().unwrap().push(sent.clone());
            let mut data = echo(&sent);
            data["decision"] = json!("approved");
            data["executable"] = json!(true);
            data["repeated"] = json!(false);
            data["reason"] = Value::Null;
            json_reply(json!({"ok":true,"data":data}))
        }
        ("POST", r) if r == format!("{PREFIX}/publish") => {
            door.sends.lock().unwrap().push(sent.clone());
            let mut data = echo(&sent);
            data["version"] = json!("1");
            data["result"] = json!({"key": sent["key"], "decision": "approved", "executed": true,
                "status": "posted", "permalink": "https://www.instagram.com/p/CAD1304/", "repeated": false});
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

struct Fx {
    _serial: std::sync::MutexGuard<'static, ()>,
    root: tempfile::TempDir,
    daemon: Option<(
        Arc<AtomicBool>,
        std::thread::JoinHandle<cadence_agent::Result<()>>,
    )>,
    seen: Seen,
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

    /// A screen action: the session credentials plus the action token.
    fn screen(&self, method: &str, mut params: Value) -> cadence_agent::Result<Value> {
        params["action_token"] = self.action_token.clone();
        params["token"] = self.session["token"].clone();
        params["key"] = self.session["key"].clone();
        params["origin"] = json!("loopback");
        self.rpc_as(Asserted::Operator, method, params)
    }

    fn start() -> Self {
        Self::start_with(0, 0)
    }

    /// `window_ms` / `backoff_ms` shorten the image job's window and retry
    /// pause (`0` keeps the production values).
    fn start_with(window_ms: u64, backoff_ms: u64) -> Self {
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
        let root = tempfile::Builder::new().prefix("c1304").tempdir().unwrap();
        cadence_agent::issue::Pm::init(&root.path().join("pm")).unwrap();
        std::fs::create_dir_all(root.path().join("s")).unwrap();
        let (door_addr, seen, door) = fake_door();
        let mut fx = Self {
            _serial: serial,
            root,
            daemon: None,
            seen,
            door,
            door_addr,
            install: String::new(),
            context: String::new(),
            session: Value::Null,
            action_token: Value::Null,
        };
        fx.launch(window_ms, backoff_ms);
        fx.setup();
        fx
    }

    /// Start (or restart) the daemon on this fixture's state dir.
    fn launch(&mut self, window_ms: u64, backoff_ms: u64) {
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
            image_job_window_ms: window_ms,
            image_job_backoff_ms: backoff_ms,
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
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&self.dir(), "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&self.dir()).is_none()
        {
            assert!(
                !handle.is_finished() && std::time::Instant::now() < deadline,
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

    /// Social Content with a screen that may call the source read and the
    /// draft actions, bound to the hosted account (source, image) and to the
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
                "request_id": "ctx-1304"}),
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
            .app_social_sources_save(&context, 0, &[HANDLE.to_owned()], "src-1304")
            .unwrap();
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
        self.install = install;
        self.context = context;
    }

    /// Fetch the saved handle's posts. Returns `(receipt_id, post_id)`.
    fn fetch(&self) -> (String, String) {
        let fetched = self
            .screen(
                "app_tool_invoke",
                json!({"tool_alias": "instagram.read", "input": {"handle": HANDLE},
                    "request_id": "fetch-1304"}),
            )
            .unwrap_or_else(|e| panic!("fetch: {e}"));
        let receipt = fetched["receipt"]["id"].as_str().unwrap().to_string();
        let post = fetched["receipt"]["result"]["posts"][0]["id"]
            .as_str()
            .unwrap_or_else(|| panic!("no normalized post: {fetched}"))
            .to_string();
        (receipt, post)
    }

    /// Poll the draft listing until the image intent completes; returns its
    /// receipt id.
    fn image_receipt(&self, draft: &str) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
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
                return row["receipt_id"].as_str().unwrap().to_string();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "image never completed: {listed}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Draft a caption from that post, generate the image and attach it.
    /// Returns `(draft_id, revision, asset_id)`.
    fn draft_with_image(&self, tag: &str, source: &(String, String)) -> (String, i64, String) {
        let (receipt, post) = source;
        let created = self
            .screen(
                "app_social_draft_create",
                json!({"tool_alias": "social.draft", "request_id": format!("draft-{tag}"),
                    "caption": CAPTION,
                    "source": {"kind": "tool_receipt", "receipt_id": receipt, "post_id": post}}),
            )
            .unwrap_or_else(|e| panic!("draft create: {e}"));
        let draft = created["draft_id"].as_str().unwrap().to_string();
        let generated = self
            .screen(
                "app_tool_invoke",
                json!({"tool_alias": "social.draft", "input": {},
                    "request_id": format!("image-{tag}"),
                    "generation_scope": {"operation": "image", "draft_id": draft, "revision": 1}}),
            )
            .unwrap_or_else(|e| panic!("generate image: {e}"));
        // CAD-1315: the call returns at once with the intent pending; the
        // host worker settles it against the media door's real job shape.
        assert_eq!(
            generated["generation_intent"]["state"], "pending",
            "image generation must queue, not block: {generated}"
        );
        let asset = self.image_receipt(&draft);
        let attached = self
            .screen(
                "app_social_draft_update",
                json!({"tool_alias": "social.draft", "request_id": format!("attach-{tag}"),
                    "draft_id": draft, "expected_revision": 1, "caption": CAPTION,
                    "asset_id": asset}),
            )
            .unwrap_or_else(|e| panic!("attach image: {e}"));
        (draft, attached["revision"].as_i64().unwrap(), asset)
    }

    /// Stage the publish effect through the real RPC and approve it.
    fn staged(&self, draft: &str, revision: i64, tag: &str) -> (String, String) {
        let staged = self
            .screen(
                "app_effect_stage",
                json!({"tool_alias": "social.draft",
                    "proof": {"kind": "social_draft", "draft_id": draft, "revision": revision},
                    "request_id": format!("stage-{tag}")}),
            )
            .unwrap_or_else(|e| panic!("stage: {e}"));
        let id = staged["effect"]["effect_id"]
            .as_str()
            .unwrap_or_else(|| panic!("staged effect: {staged}"))
            .to_string();
        let digest = staged["effect"]["digest"].as_str().unwrap().to_string();
        self.op(
            "app_effect_decide",
            json!({"effect_id": id, "digest": digest, "decision": "accept"}),
        );
        (id, digest)
    }

    /// An effect staged the way main did before CAD-1304: the real staged
    /// authority with the image digest re-frozen as `sha256:<hex>` (or the
    /// digest of `forged` bytes), waiting. Returns `(effect_id, digest)`.
    fn legacy(
        &self,
        draft: &str,
        revision: i64,
        tag: &str,
        forged: Option<&[u8]>,
    ) -> (String, String) {
        let real = self
            .screen(
                "app_effect_stage",
                json!({"tool_alias": "social.draft",
                    "proof": {"kind": "social_draft", "draft_id": draft, "revision": revision},
                    "request_id": format!("stage-real-{tag}")}),
            )
            .unwrap_or_else(|e| panic!("stage: {e}"));
        let mut frozen = real["effect"]["authority"].clone();
        let bare = forged.map_or_else(|| frozen["image_digest"].as_str().unwrap().to_string(), hex);
        let hexed: String = self.install.bytes().map(|b| format!("{b:02x}")).collect();
        let id = format!("sfx_{hexed}_{:032x}", 0xdead_u32 + tag.len() as u32);
        frozen["image_digest"] = json!(format!("sha256:{bare}"));
        frozen["effect_id"] = json!(id);
        frozen["idempotency_key"] = json!(format!("social_{:032x}", 0xbeef_u32 + tag.len() as u32));
        frozen["approval_id"] = json!(format!("social-approval-legacy-{tag}"));
        let staged = RecordStore::open(&self.dir(), &self.install)
            .unwrap()
            .app_social_effect_stage(
                &self.context,
                &cadence_agent::store::app_social_drafts::EffectStage {
                    draft,
                    revision,
                    request: &format!("stage-legacy-{tag}"),
                    effect_id: &id,
                    frozen: &frozen,
                    approval: &format!("social-approval-legacy-{tag}"),
                },
            )
            .unwrap();
        let digest = staged["effect"]["digest"].as_str().unwrap().to_string();
        (id, digest)
    }

    fn accept(&self, id: &str, digest: &str) {
        self.op(
            "app_effect_decide",
            json!({"effect_id": id, "digest": digest, "decision": "accept"}),
        );
    }

    fn publish_now(&self, id: &str, digest: &str) -> cadence_agent::Result<Value> {
        self.rpc_as(
            Asserted::Operator,
            "app_effect_publish_now",
            json!({"effect_id": id, "digest": digest}),
        )
    }

    fn state(&self, id: &str) -> String {
        RecordStore::open(&self.dir(), &self.install)
            .unwrap()
            .app_social_effect_show(id)
            .unwrap()["effect"]["state"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn count(&self, path: &str) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p)| m == "POST" && p.split('?').next() == Some(path))
            .count()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        if let Some((stop, handle)) = self.daemon.take() {
            stop.store(true, SeqCst);
            let _ = handle.join();
        }
    }
}

#[test]
fn an_approved_instagram_draft_with_a_generated_image_publishes_and_a_changed_one_sends_nothing() {
    let fx = Fx::start();
    let source = fx.fetch();
    let (draft, revision, asset) = fx.draft_with_image("a", &source);
    let (id, digest) = fx.staged(&draft, revision, "a");

    // Positive: media import, preflight and publish, ends posted.
    let posted = fx
        .publish_now(&id, &digest)
        .unwrap_or_else(|e| panic!("image publish refused: {e}"));
    assert_eq!(fx.count(&format!("{PREFIX}/media/import")), 1);
    assert!(fx.count(&format!("{PREFIX}/publish/preflight")) >= 1);
    assert_eq!(fx.count(&format!("{PREFIX}/publish")), 1);
    assert_eq!(fx.state(&id), "posted", "{posted}");
    // The staged digest is the canonical bare 64-hex the door speaks.
    let frozen = RecordStore::open(&fx.dir(), &fx.install)
        .unwrap()
        .app_social_effect_show(&id)
        .unwrap();
    let frozen_digest = frozen["effect"]["authority"]["image_digest"]
        .as_str()
        .unwrap_or_else(|| panic!("no frozen image digest: {frozen}"))
        .to_string();
    assert_eq!(
        frozen_digest.len(),
        64,
        "frozen image digest {frozen_digest}"
    );
    assert!(frozen_digest.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(!asset.is_empty());

    let sends = fx.door.sends.lock().unwrap().clone();
    assert!(
        sends
            .iter()
            .all(|s| s["grant"]["imageDigest"] == json!(frozen_digest)),
        "the door was presented a different image digest: {sends:?}"
    );
    let imports = fx.door.imports.lock().unwrap().clone();
    assert_eq!(
        imports,
        vec![(frozen_digest.clone(), frozen_digest.clone())]
    );

    // Changed image after approval: a second generated image is swapped in,
    // the approved effect is refused and nothing more reaches the door.
    let (draft_b, revision_b, _) = fx.draft_with_image("b", &source);
    let (id_b, digest_b) = fx.staged(&draft_b, revision_b, "b");
    let (_, _, other_asset) = fx.draft_with_image("c", &source);
    fx.screen(
        "app_social_draft_update",
        json!({"tool_alias": "social.draft", "request_id": "swap-b", "draft_id": draft_b,
            "expected_revision": revision_b, "caption": CAPTION, "asset_id": other_asset}),
    )
    .unwrap();
    let before = fx.seen.lock().unwrap().len();
    let refused = fx.publish_now(&id_b, &digest_b).unwrap_err().to_string();
    assert!(
        refused.contains("social draft changed since approval"),
        "{refused}"
    );
    assert_eq!(fx.state(&id_b), "approved");

    // Changed caption after approval: same.
    let (draft_d, revision_d, asset_d) = fx.draft_with_image("d", &source);
    let (id_d, digest_d) = fx.staged(&draft_d, revision_d, "d");
    fx.screen(
        "app_social_draft_update",
        json!({"tool_alias": "social.draft", "request_id": "edit-d", "draft_id": draft_d,
            "expected_revision": revision_d, "caption": "Edited after approval",
            "asset_id": asset_d}),
    )
    .unwrap();
    let refused = fx.publish_now(&id_d, &digest_d).unwrap_err().to_string();
    assert!(
        refused.contains("social draft changed since approval"),
        "{refused}"
    );
    assert_eq!(fx.state(&id_d), "approved");
    let after = fx.seen.lock().unwrap()[before..].to_vec();
    assert!(
        after.iter().all(|(m, p)| m != "POST"
            || !(p.starts_with(&format!("{PREFIX}/media/import"))
                || p.starts_with(&format!("{PREFIX}/publish")))),
        "a changed draft reached the door: {after:?}"
    );
}

#[test]
fn a_legacy_sha256_frozen_effect_publishes_unchanged_and_a_changed_image_sends_nothing() {
    let fx = Fx::start();
    let source = fx.fetch();
    let (draft, revision, _) = fx.draft_with_image("l", &source);
    let (id, digest) = fx.legacy(&draft, revision, "ok", None);
    fx.accept(&id, &digest);
    let posted = fx
        .publish_now(&id, &digest)
        .unwrap_or_else(|e| panic!("legacy image publish refused: {e}"));
    assert_eq!(fx.state(&id), "posted", "{posted}");
    assert_eq!(fx.count(&format!("{PREFIX}/publish")), 1);

    // The frozen image is not the one in custody: refused before any import.
    let (draft, revision, _) = fx.draft_with_image("m", &source);
    let (id, digest) = fx.legacy(&draft, revision, "chg", Some(&png(200)));
    let imports = fx.count(&format!("{PREFIX}/media/import"));
    let refused = fx
        .rpc_as(
            Asserted::Operator,
            "app_effect_decide",
            json!({"effect_id": id, "digest": digest, "decision": "accept"}),
        )
        .unwrap_err()
        .to_string();
    assert!(refused.contains("image changed since staging"), "{refused}");
    assert!(fx.publish_now(&id, &digest).is_err());
    assert_eq!(fx.state(&id), "waiting");
    assert_eq!(fx.count(&format!("{PREFIX}/media/import")), imports);
    assert_eq!(fx.count(&format!("{PREFIX}/publish")), 1);
}

// ---- CAD-1315: image generation as a queued job -------------------------
//
// The media door above answers in AgenticOS's real job shape (see
// `fixtures/aos_media_door.rs`): 201 `submitted`, then `running`, then
// `succeeded` with a ~3 MiB artifact.

use aos_media_door::Scenario;

impl Fx {
    /// Create a draft from the fetched source; returns its id.
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

    fn image(&self, draft: &str, request: &str) -> cadence_agent::Result<Value> {
        self.screen(
            "app_tool_invoke",
            json!({"tool_alias": "social.draft", "input": {}, "request_id": request,
                "generation_scope": {"operation": "image", "draft_id": draft, "revision": 1}}),
        )
    }

    /// The draft's image intent, read from the record file (no live session
    /// needed, so it also works across a daemon restart).
    fn intent(&self, draft: &str) -> Option<Value> {
        RecordStore::open(&self.dir(), &self.install)
            .unwrap()
            .app_social_generation_intents(&self.context)
            .unwrap()
            .into_iter()
            .find(|row| row["scope"]["draft_id"] == draft && row["scope"]["operation"] == "image")
    }

    fn wait_intent(&self, draft: &str, state: &str) -> Value {
        let deadline = std::time::Instant::now() + Duration::from_secs(40);
        loop {
            let row = self.intent(draft);
            if let Some(row) = row.filter(|row| row["state"] == state) {
                return row;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "intent never reached {state}: {:?}",
                self.intent(draft)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_door(&self, what: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !self.door.media.log().iter().any(|l| l.starts_with(what)) {
            assert!(
                std::time::Instant::now() < deadline,
                "door never saw {what}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn keys(&self) -> std::collections::BTreeSet<String> {
        self.door.media.submit_keys().into_iter().collect()
    }
}

#[test]
fn an_image_job_returns_at_once_leaves_the_board_free_and_retains_a_custody_sized_jpeg() {
    let fx = Fx::start();
    let source = fx.fetch();
    // One attempt may poll for 30 s: a board write that waits on the worker
    // cannot finish inside this test's bound.
    std::env::set_var("CADENCE_TEST_MEDIA_DEADLINE_MS", "30000");
    fx.door.media.set(Scenario::Hold);
    let draft = fx.new_draft("q1", &source);

    let started = std::time::Instant::now();
    let reply = fx.image(&draft, "image-q1").unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the call blocked"
    );
    // The reply is the shape an older app bundle already validates.
    assert_eq!(reply["replayed"], true, "{reply}");
    let intent = reply["generation_intent"].as_object().unwrap();
    let mut keys: Vec<&str> = intent.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "draft_id",
            "input",
            "input_digest",
            "operation",
            "request_id",
            "revision",
            "state",
            "updated_at"
        ],
        "{reply}"
    );
    assert_eq!(intent["state"], "pending");

    // The job is in flight at the door; another board write completes now.
    fx.wait_door("GET job");
    let other = std::time::Instant::now();
    let second = fx.new_draft("q1b", &source);
    fx.screen(
        "app_social_draft_update",
        json!({"tool_alias": "social.draft", "request_id": "edit-q1b", "draft_id": second,
            "expected_revision": 1, "caption": "A second caption"}),
    )
    .unwrap_or_else(|e| panic!("a board write stalled behind the image job: {e}"));
    assert!(other.elapsed() < Duration::from_secs(10));
    assert_eq!(fx.intent(&draft).unwrap()["state"], "pending");

    // The door finishes the job; the 3 MiB PNG is retained within custody.
    fx.door.media.set(Scenario::Normal);
    let done = fx.wait_intent(&draft, "completed");
    let receipt = fx.op("app_tool_result", json!({"receipt_id": done["receipt_id"]}));
    let asset = &receipt["asset"];
    assert_eq!(asset["media_type"], "image/jpeg", "{receipt}");
    assert!(
        asset["size"].as_u64().unwrap() <= 2 * 1024 * 1024,
        "{receipt}"
    );
    assert_eq!(receipt["result"]["asset_media_type"], "image/jpeg");
    assert_eq!(fx.keys().len(), 1, "one key, one job");
    assert_eq!(fx.door.media.job_count(), 1);
}

#[test]
fn a_restart_replays_the_same_key_and_leaves_no_image_intent_pending() {
    let mut fx = Fx::start();
    let source = fx.fetch();
    fx.door.media.set(Scenario::Hold);
    let draft = fx.new_draft("r1", &source);
    fx.image(&draft, "image-r1").unwrap();
    fx.wait_door("GET job");

    // The daemon dies mid-job. Its worker gives up within one attempt
    // (1.5 s here); the intent stays pending and the job stays held at
    // AgenticOS, so only the next boot can settle it.
    fx.stop_daemon();
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(fx.intent(&draft).unwrap()["state"], "pending");
    let key = fx.keys().into_iter().next().unwrap();
    let mark = fx.door.media.log().len();
    fx.door.media.set(Scenario::Normal);
    fx.launch(0, 0);

    // Boot reconciliation replays the key; no second job is ever created.
    let done = fx.wait_intent(&draft, "completed");
    assert!(done["receipt_id"].is_string(), "{done}");
    let replays = fx.door.media.log()[mark..].to_vec();
    assert!(
        replays.contains(&format!("POST image key={key}")),
        "boot did not replay the key: {replays:?}"
    );
    assert_eq!(fx.keys().len(), 1, "a restart must replay the same key");
    assert_eq!(fx.door.media.job_count(), 1);
}

#[test]
fn a_new_key_is_minted_only_after_the_old_job_is_terminal() {
    let fx = Fx::start();
    let source = fx.fetch();

    // An unresolved job (AgenticOS answers `uncertain`) records its reason
    // and is re-checked under the SAME key; no request id starts a second one.
    fx.door.media.set(Scenario::Uncertain);
    let stuck = fx.new_draft("k1", &source);
    fx.image(&stuck, "image-k1").unwrap();
    let row = fx.wait_intent(&stuck, "uncertain");
    assert_eq!(row["outcome"], "provider_uncertain", "{row}");
    let submits = fx.door.media.submit_keys().len();
    fx.image(&stuck, "image-k1").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while fx.door.media.submit_keys().len() == submits
        || fx.intent(&stuck).unwrap()["state"] != "uncertain"
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the re-check never ran"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let traffic = fx.door.media.log().len();
    let other = fx.image(&stuck, "image-k1-other").unwrap();
    assert_eq!(other["generation_intent"]["state"], "uncertain", "{other}");
    assert_eq!(other["generation_intent"]["request_id"], "image-k1");
    assert_eq!(
        fx.door.media.log().len(),
        traffic,
        "another request id reached the door"
    );
    assert_eq!(fx.keys().len(), 1);
    assert_eq!(fx.door.media.job_count(), 1);

    // A job AgenticOS closed as failed is terminal: it records why, and the
    // next request id is a new job under a new key. The old job stays.
    fx.door.media.set(Scenario::Fail);
    let failed = fx.new_draft("k2", &source);
    fx.image(&failed, "image-k2").unwrap();
    let row = fx.wait_intent(&failed, "refused");
    assert_eq!(row["outcome"], "job_failed", "{row}");
    assert_eq!(fx.keys().len(), 2);
    fx.door.media.set(Scenario::Normal);
    fx.image(&failed, "image-k2-again").unwrap();
    fx.wait_intent(&failed, "completed");
    assert_eq!(fx.keys().len(), 3, "the retry is a new key");
    let records = RecordStore::open(&fx.dir(), &fx.install).unwrap();
    let old = records
        .app_social_image_job_for_request("image-k2")
        .unwrap()
        .unwrap();
    assert_eq!(
        (old.state.as_str(), old.reason.as_deref()),
        ("failed", Some("job_failed"))
    );
}

#[test]
fn an_unreachable_door_keeps_the_intent_pending_then_settles_uncertain_and_recovers_on_the_same_key(
) {
    let fx = Fx::start_with(2500, 100);
    let source = fx.fetch();
    fx.door.media.set(Scenario::Hold);
    let draft = fx.new_draft("d1", &source);
    fx.image(&draft, "image-d1").unwrap();
    fx.wait_door("GET job");

    // AgenticOS goes away: no fake terminal state while the window is open.
    fx.door.media.set(Scenario::Down);
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(fx.intent(&draft).unwrap()["state"], "pending");

    // The window is bounded: the intent settles with a reason.
    let row = fx.wait_intent(&draft, "uncertain");
    let reason = row["outcome"].as_str().unwrap();
    assert!(matches!(reason, "submit_error" | "poll_timeout"), "{row}");

    // The door returns; re-checking replays the same key and completes.
    fx.door.media.set(Scenario::Normal);
    fx.image(&draft, "image-d1").unwrap();
    fx.wait_intent(&draft, "completed");
    assert_eq!(fx.keys().len(), 1);
    assert_eq!(fx.door.media.job_count(), 1);
}

#[test]
fn a_download_that_breaks_after_the_charge_retries_the_same_key_and_keeps_the_image() {
    let fx = Fx::start();
    let source = fx.fetch();
    let draft = fx.new_draft("t1", &source);
    // The job succeeds at the door, but every artifact body is cut short.
    fx.door.media.truncate_artifacts(true);
    fx.image(&draft, "image-t1").unwrap();
    fx.wait_door("GET job");
    // Let several fetches break: still pending, never a verdict on the job.
    std::thread::sleep(Duration::from_secs(8));
    let row = fx.intent(&draft).unwrap();
    assert_eq!(row["state"], "pending", "{row}");
    assert_eq!(row["outcome"], Value::Null, "{row}");
    // The body arrives whole: the same key completes, with no second job.
    fx.door.media.truncate_artifacts(false);
    fx.wait_intent(&draft, "completed");
    assert_eq!(fx.keys().len(), 1);
    assert_eq!(fx.door.media.job_count(), 1);
}

// ---- CAD-1315: the worker never loops --------------------------------------
//
// A worker run that leaves its job `active` on purpose (parked fence, missing
// spec, an error, a panic) is NOT restarted by itself; only a run that
// settled can race a re-check, and that case restarts exactly once. The
// worker-start count bounds any spin.

use cadence_agent::daemon::image_job_probe as probe;

impl Fx {
    /// Worker starts over the next two seconds, with nothing else going on.
    fn starts_over_two_seconds(&self, before: usize) -> usize {
        std::thread::sleep(Duration::from_secs(2));
        probe::starts() - before
    }
}

#[test]
fn a_parked_fence_runs_one_worker_and_does_not_respawn_until_asked() {
    let fx = Fx::start();
    let source = fx.fetch();
    let draft = fx.new_draft("p1", &source);
    let before = probe::starts();
    probe::trip_fence(true);
    fx.image(&draft, "image-p1").unwrap();
    assert_eq!(
        fx.starts_over_two_seconds(before),
        1,
        "the parked worker spun"
    );
    assert_eq!(fx.intent(&draft).unwrap()["state"], "pending");
    // The fence clears and a same-id re-check restarts it: it completes.
    probe::trip_fence(false);
    fx.image(&draft, "image-p1").unwrap();
    fx.wait_intent(&draft, "completed");
    assert_eq!(probe::starts() - before, 2);
}

#[test]
fn an_errored_or_panicked_worker_frees_its_slot_without_respawning() {
    let fx = Fx::start();
    let source = fx.fetch();
    for (tag, fault) in [("e1", 2u8), ("e2", 1u8)] {
        let draft = fx.new_draft(tag, &source);
        let before = probe::starts();
        probe::fault_next(fault);
        fx.image(&draft, &format!("image-{tag}")).unwrap();
        assert_eq!(fx.starts_over_two_seconds(before), 1, "{tag} respawned");
        assert_eq!(fx.intent(&draft).unwrap()["state"], "pending");
        // The slot is free: a same-id re-check starts a worker that completes.
        fx.image(&draft, &format!("image-{tag}")).unwrap();
        fx.wait_intent(&draft, "completed");
        assert_eq!(probe::starts() - before, 2, "{tag}");
    }
}

#[test]
fn a_job_with_no_spec_runs_one_worker_at_boot_and_does_not_respawn() {
    let mut fx = Fx::start();
    let source = fx.fetch();
    fx.door.media.set(Scenario::Hold);
    let draft = fx.new_draft("m1", &source);
    fx.image(&draft, "image-m1").unwrap();
    fx.wait_door("GET job");
    fx.stop_daemon();
    std::thread::sleep(Duration::from_millis(2500));
    let path = cadence_agent::store::app_records::record_db_path(&fx.dir(), &fx.install).unwrap();
    rusqlite::Connection::open(path)
        .unwrap()
        .execute("UPDATE app_social_image_jobs SET spec_json=NULL", [])
        .unwrap();
    let before = probe::starts();
    fx.launch(0, 0);
    assert_eq!(
        fx.starts_over_two_seconds(before),
        1,
        "the held worker spun"
    );
    assert_eq!(fx.intent(&draft).unwrap()["state"], "pending");
}

#[test]
fn a_re_check_that_lands_before_the_slot_is_freed_is_restarted_exactly_once() {
    // The job settles `uncertain` (poll_timeout) at the end of a 1.2 s window;
    // the worker then sits 3 s before freeing its slot. A same-id re-check in
    // that gap finds the slot taken and spawns nothing: the worker itself must
    // restart the job, once, and it then completes.
    let fx = Fx::start_with(1200, 100);
    let source = fx.fetch();
    fx.door.media.set(Scenario::Hold);
    let draft = fx.new_draft("c1", &source);
    let before = probe::starts();
    probe::pause_after_settle_ms(3000);
    fx.image(&draft, "image-c1").unwrap();
    fx.wait_intent(&draft, "uncertain");
    fx.door.media.set(Scenario::Normal);
    fx.image(&draft, "image-c1").unwrap();
    assert_eq!(fx.intent(&draft).unwrap()["state"], "pending");
    fx.wait_intent(&draft, "completed");
    probe::pause_after_settle_ms(0);
    assert_eq!(probe::starts() - before, 2, "one restart, no loop");
    assert_eq!(fx.keys().len(), 1);
    assert_eq!(fx.door.media.job_count(), 1);
}

/// CAD-1328: the operator's own context save re-pins the context's bindings,
/// and one confirm on the staged effect approves and sends it exactly once.
#[test]
fn an_operator_context_save_re_pins_and_one_confirm_posts_the_staged_draft_once() {
    let fx = Fx::start();
    let pins = || {
        let listed = fx.op(
            "app_binding_list",
            json!({"install_id": fx.install, "context_id": fx.context}),
        );
        listed["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                (
                    b["drift"]["state"].clone(),
                    b["config"]["context"]["revision"].clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(pins().len(), 3);
    assert!(pins().iter().all(|(s, r)| s == "current" && *r == json!(1)));
    let update = |who: Asserted| {
        fx.rpc_as(
            who,
            "app_context_update",
            json!({"install_id": fx.install, "context_id": fx.context, "expected_revision": 1,
                "label": "EF renamed", "input_defaults": {}}),
        )
    };
    // No operator proof: nothing is saved and nothing is re-pinned.
    assert!(update(Asserted::Agent("worker".into())).is_err());
    assert!(update(Asserted::Unproven).is_err());
    assert!(pins().iter().all(|(s, r)| s == "current" && *r == json!(1)));
    // The operator's save moves every binding to revision 2 in one step.
    update(Asserted::Operator).unwrap();
    let after = pins();
    assert_eq!(after.len(), 3);
    assert!(
        after.iter().all(|(s, r)| s == "current" && *r == json!(2)),
        "{after:?}"
    );

    // Publish after the save: staged by the screen, confirmed with one call.
    let source = fx.fetch();
    let (draft, revision, _) = fx.draft_with_image("one", &source);
    let staged = fx
        .screen(
            "app_effect_stage",
            json!({"tool_alias": "social.draft",
                "proof": {"kind": "social_draft", "draft_id": draft, "revision": revision},
                "request_id": "stage-one"}),
        )
        .unwrap_or_else(|e| panic!("stage after a context save: {e}"));
    let id = staged["effect"]["effect_id"].as_str().unwrap().to_string();
    let digest = staged["effect"]["digest"].as_str().unwrap().to_string();
    let confirm = |who: Asserted, digest: &str| {
        fx.rpc_as(
            who,
            "app_effect_confirm_publish",
            json!({"effect_id": id, "digest": digest}),
        )
    };
    // An agent, an unproven caller and a stale digest send nothing.
    assert!(confirm(Asserted::Agent("worker".into()), &digest).is_err());
    assert!(confirm(Asserted::Unproven, &digest).is_err());
    assert!(confirm(Asserted::Operator, "sha256:stale").is_err());
    assert_eq!(fx.state(&id), "waiting");
    assert_eq!(fx.count(&format!("{PREFIX}/publish")), 0);
    // A run artifact effect is not eligible for the one-step path.
    assert!(fx
        .rpc_as(
            Asserted::Operator,
            "app_effect_confirm_publish",
            json!({"effect_id": "effect-0123", "digest": digest}),
        )
        .is_err());

    confirm(Asserted::Operator, &digest).unwrap_or_else(|e| panic!("confirm refused: {e}"));
    assert_eq!(fx.state(&id), "posted");
    assert_eq!(fx.count(&format!("{PREFIX}/publish")), 1);
    // A replayed tap returns the receipt and posts nothing more.
    let replay = confirm(Asserted::Operator, &digest).unwrap();
    assert_eq!(replay["effect"]["state"], "posted");
    assert_eq!(fx.count(&format!("{PREFIX}/publish")), 1);
    assert_eq!(fx.count(&format!("{PREFIX}/media/import")), 1);
}
