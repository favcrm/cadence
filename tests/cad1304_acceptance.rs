//! CAD-1304 acceptance check — written by the Spec/security reviewer
//! (opus-rev-spec-905), not the implementer. The harness (door, app setup,
//! screen session) is the PR's `cad1304_instagram_image_publish` one.
//!
//! Every effect starts from the real producer: a fetched source receipt, a
//! draft, a generated image retained in custody, and `app_effect_stage`.
//! Where a case needs an old or mismatched frozen digest, the real staged
//! authority is copied and only `image_digest` (plus the per-effect ids) is
//! changed, then frozen through the store exactly as staging does.
//!
//! (a) An approved, unchanged effect publishes and the door sees only the
//!     bare 64-hex digest. (b) A swapped image, an edited caption, a moved
//!     destination, or a frozen digest that differs from custody in any way
//!     sends nothing; each case pins exactly which door requests happened
//!     before the refusal (never a media import). (c) An effect frozen in
//!     the pre-CAD-1304 `sha256:<hex>` form with an unchanged image still
//!     publishes; the same old form over different bytes does not.
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
use cadence_agent::store::app_social_drafts::EffectStage;
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
    /// When set, media import answers with a receipt for other bytes.
    lie: AtomicBool,
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
                    "digest": if door.lie.load(SeqCst) { hex(b"other bytes") } else { digest.clone() },
                    "mime": mime, "sizeBytes": body.len(),
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

struct Fx {
    root: tempfile::TempDir,
    daemon: Option<(
        Arc<AtomicBool>,
        std::thread::JoinHandle<cadence_agent::Result<()>>,
    )>,
    seen: Seen,
    door: Arc<Door>,
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
        let root = tempfile::Builder::new().prefix("c1304").tempdir().unwrap();
        cadence_agent::issue::Pm::init(&root.path().join("pm")).unwrap();
        std::fs::create_dir_all(root.path().join("s")).unwrap();
        let (door_addr, seen, door) = fake_door();
        let mut fx = Self {
            root,
            daemon: None,
            seen,
            door,
            install: String::new(),
            context: String::new(),
            session: Value::Null,
            action_token: Value::Null,
        };
        let (dir, stop) = (fx.dir(), Arc::new(AtomicBool::new(false)));
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set(
            "CADENCE_PM_DIR",
            fx.root.path().join("pm").to_str().unwrap(),
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
        let handle = std::thread::spawn(move || daemon::serve_with(&dir, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&fx.dir(), "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&fx.dir()).is_none()
        {
            assert!(
                !handle.is_finished() && std::time::Instant::now() < deadline,
                "daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        std::env::remove_var("ALL_PROXY");
        fx.daemon = Some((stop, handle));
        fx.setup();
        fx
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

    /// Door requests after the first `from`, as `METHOD route` (no query).
    fn since(&self, from: usize) -> Vec<String> {
        self.seen.lock().unwrap()[from..]
            .iter()
            .map(|(m, p)| format!("{m} {}", p.split('?').next().unwrap_or("")))
            .collect()
    }

    fn mark(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    /// Copy a real staged authority onto a new effect for the same draft,
    /// with only `image_digest` (and the per-effect ids) changed; freeze it
    /// as staging does and approve it in the store, so the send-time guard
    /// is the one under test. Returns `(effect_id, digest)`.
    fn restage(&self, base: &Value, tag: &str, image_digest: &str) -> (String, String) {
        let records = RecordStore::open(&self.dir(), &self.install).unwrap();
        let hexed: String = self.install.bytes().map(|b| format!("{b:02x}")).collect();
        let n = self.jobs_tag(tag);
        let id = format!("sfx_{hexed}_{n:032x}");
        let mut frozen = base.clone();
        frozen["image_digest"] = json!(image_digest);
        frozen["effect_id"] = json!(id);
        frozen["idempotency_key"] = json!(format!("social_{n:032x}"));
        frozen["approval_id"] = json!(format!("social-approval-{tag}"));
        let staged = records
            .app_social_effect_stage(
                &self.context,
                &EffectStage {
                    draft: base["draft_id"].as_str().unwrap(),
                    revision: base["revision"].as_i64().unwrap(),
                    request: &format!("restage-{tag}"),
                    effect_id: &id,
                    frozen: &frozen,
                    approval: &format!("social-approval-{tag}"),
                },
            )
            .unwrap();
        let digest = staged["effect"]["digest"].as_str().unwrap().to_string();
        records
            .app_social_effect_decide(&id, &digest, true)
            .unwrap()
            .unwrap();
        (id, digest)
    }

    /// A distinct effect-id suffix per tag.
    fn jobs_tag(&self, tag: &str) -> u128 {
        tag.bytes().fold(0xc1304_u128, |a, b| a * 131 + b as u128)
    }

    fn authority(&self, id: &str) -> Value {
        RecordStore::open(&self.dir(), &self.install)
            .unwrap()
            .app_social_effect_show(id)
            .unwrap()["effect"]["authority"]
            .clone()
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
fn image_publish_sends_only_the_approved_unchanged_bytes_under_one_bare_digest() {
    let fx = Fx::start();
    let source = fx.fetch();
    let import = format!("{PREFIX}/media/import");
    let lookups = vec![
        format!("GET {PREFIX}/destinations"),
        format!("GET {PREFIX}/publish/grants"),
    ];

    // (a) Approved, unchanged: import, preflight, publish, posted; the door
    // only ever sees the bare lowercase 64-hex digest of the image bytes.
    let (draft, revision, _) = fx.draft_with_image("a", &source);
    let (id, digest) = fx.staged(&draft, revision, "a");
    let base = fx.authority(&id);
    let bare = base["image_digest"].as_str().unwrap().to_string();
    assert!(
        bare.len() == 64
            && bare
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "staging froze a non-canonical digest: {bare}"
    );
    let from = fx.mark();
    fx.publish_now(&id, &digest)
        .unwrap_or_else(|e| panic!("(a) image publish refused: {e}"));
    assert_eq!(fx.state(&id), "posted");
    let sent = fx.since(from);
    assert!(sent.contains(&format!("POST {import}")), "(a) {sent:?}");
    assert!(
        sent.contains(&format!("POST {PREFIX}/publish")),
        "(a) {sent:?}"
    );
    let imports = fx.door.imports.lock().unwrap().clone();
    assert_eq!(
        imports,
        vec![(bare.clone(), bare.clone())],
        "(a) import digest"
    );
    let sends = fx.door.sends.lock().unwrap().clone();
    assert!(!sends.is_empty());
    assert!(
        sends
            .iter()
            .all(|s| s["grant"]["imageDigest"] == json!(bare)),
        "(a) the door saw another image digest form: {sends:?}"
    );
    let imports_after_a = imports.len();

    // (c) An effect frozen before CAD-1304 (`sha256:<hex>`) over the same
    // retained bytes reads as the same digest and publishes, presenting
    // the bare form; the same old form over different bytes does not.
    let (draft_c, revision_c, _) = fx.draft_with_image("c", &source);
    let (staged_c, _) = fx.staged(&draft_c, revision_c, "c-base");
    let base_c = fx.authority(&staged_c);
    let bare_c = base_c["image_digest"].as_str().unwrap().to_string();
    let (old, old_digest) = fx.restage(&base_c, "c-old", &format!("sha256:{bare_c}"));
    let from = fx.mark();
    let sends_before = fx.door.sends.lock().unwrap().len();
    let old_sent = fx.publish_now(&old, &old_digest);
    assert!(
        old_sent.is_ok(),
        "(c) old-form effect over unchanged bytes refused: {:?}; door saw {:?}",
        old_sent.err().map(|e| e.to_string()),
        fx.since(from)
    );
    assert_eq!(fx.state(&old), "posted");
    let new_sends = fx.door.sends.lock().unwrap()[sends_before..].to_vec();
    assert!(
        !new_sends.is_empty()
            && new_sends
                .iter()
                .all(|s| s["grant"]["imageDigest"] == json!(bare_c)),
        "(c) old-form effect presented a non-bare digest: {new_sends:?}"
    );

    // (b) The frozen digest differs from custody in any way: the retained
    // bytes are draft c's, the frozen digest is not exactly theirs. Only the
    // destination and grant lookups may happen first; never an import.
    let (draft_o, revision_o, _) = fx.draft_with_image("other", &source);
    let (staged_o, _) = fx.staged(&draft_o, revision_o, "other");
    let other = fx.authority(&staged_o)["image_digest"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(other, bare_c);
    let forged = [
        ("another image's bare digest", other.clone()),
        (
            "another image's digest, old prefix",
            format!("sha256:{other}"),
        ),
        ("the same hex in uppercase", bare_c.to_uppercase()),
        (
            "the same hex, prefix doubled",
            format!("sha256:sha256:{bare_c}"),
        ),
        (
            "the same hex, other algorithm prefix",
            format!("sha512:{bare_c}"),
        ),
        ("the same hex, truncated", bare_c[..63].to_string()),
        ("the same hex, one extra char", format!("{bare_c}0")),
        ("the same hex with whitespace", format!(" {bare_c}")),
    ];
    for (n, (case, frozen_digest)) in forged.into_iter().enumerate() {
        let (id, digest) = fx.restage(&base_c, &format!("forged-{n}"), &frozen_digest);
        let from = fx.mark();
        let refused = fx.publish_now(&id, &digest);
        let sent = fx.since(from);
        assert_eq!(sent, lookups, "(b) {case}: door saw {sent:?}");
        let refused = refused
            .err()
            .unwrap_or_else(|| panic!("(b) {case}: published"))
            .to_string();
        assert!(
            refused.contains("image changed since approval"),
            "(b) {case}: {refused}"
        );
        assert_eq!(fx.state(&id), "approved", "(b) {case}");
    }
    // The decide-time check refuses the same mismatch before approval.
    {
        let records = RecordStore::open(&fx.dir(), &fx.install).unwrap();
        let hexed: String = fx.install.bytes().map(|b| format!("{b:02x}")).collect();
        let id = format!("sfx_{hexed}_{:032x}", 0xdec1de_u64);
        let mut frozen = base_c.clone();
        frozen["image_digest"] = json!(other);
        frozen["effect_id"] = json!(id);
        frozen["idempotency_key"] = json!("social_decide_forged");
        let staged = records
            .app_social_effect_stage(
                &fx.context,
                &EffectStage {
                    draft: base_c["draft_id"].as_str().unwrap(),
                    revision: base_c["revision"].as_i64().unwrap(),
                    request: "restage-decide",
                    effect_id: &id,
                    frozen: &frozen,
                    approval: "social-approval-decide",
                },
            )
            .unwrap();
        let from = fx.mark();
        let decided = fx.rpc_as(
            Asserted::Operator,
            "app_effect_decide",
            json!({"effect_id": id, "digest": staged["effect"]["digest"], "decision": "accept"}),
        );
        let decided = decided
            .expect_err("decide accepted a mismatched image")
            .to_string();
        assert!(decided.contains("image changed since staging"), "{decided}");
        assert!(fx.since(from).is_empty());
    }

    // (b) The import receipt names other bytes than custody holds: the
    // import itself is the only send, never a preflight or publish.
    let (draft_l, revision_l, _) = fx.draft_with_image("lie", &source);
    let (id_l, digest_l) = fx.staged(&draft_l, revision_l, "lie");
    fx.door.lie.store(true, SeqCst);
    let from = fx.mark();
    let refused = fx.publish_now(&id_l, &digest_l);
    fx.door.lie.store(false, SeqCst);
    let mut expect = lookups.clone();
    expect.push(format!("POST {import}"));
    assert_eq!(fx.since(from), expect, "(b) lying receipt");
    let refused = refused
        .expect_err("(b) lying receipt published")
        .to_string();
    assert!(
        refused.contains("media receipt digest differs from the uploaded bytes")
            || refused.contains("does not match approved image"),
        "{refused}"
    );
    assert_eq!(fx.state(&id_l), "approved");

    // (b) Image swapped after approval: refused before any door request.
    let (draft_b, revision_b, _) = fx.draft_with_image("b", &source);
    let (id_b, digest_b) = fx.staged(&draft_b, revision_b, "b");
    let (_, _, swap) = fx.draft_with_image("swap", &source);
    fx.screen(
        "app_social_draft_update",
        json!({"tool_alias": "social.draft", "request_id": "swap-b", "draft_id": draft_b,
            "expected_revision": revision_b, "caption": CAPTION, "asset_id": swap}),
    )
    .unwrap();
    let from = fx.mark();
    let refused = fx.publish_now(&id_b, &digest_b).unwrap_err().to_string();
    assert!(
        fx.since(from).is_empty(),
        "(b) swapped image: {:?}",
        fx.since(from)
    );
    assert!(
        refused.contains("social draft changed since approval"),
        "{refused}"
    );
    assert_eq!(fx.state(&id_b), "approved");

    // (b) Caption edited after approval (same image): same.
    let (draft_d, revision_d, asset_d) = fx.draft_with_image("d", &source);
    let (id_d, digest_d) = fx.staged(&draft_d, revision_d, "d");
    fx.screen(
        "app_social_draft_update",
        json!({"tool_alias": "social.draft", "request_id": "edit-d", "draft_id": draft_d,
            "expected_revision": revision_d, "caption": "Edited after approval",
            "asset_id": asset_d}),
    )
    .unwrap();
    let from = fx.mark();
    let refused = fx.publish_now(&id_d, &digest_d).unwrap_err().to_string();
    assert!(
        fx.since(from).is_empty(),
        "(b) caption: {:?}",
        fx.since(from)
    );
    assert!(
        refused.contains("social draft changed since approval"),
        "{refused}"
    );
    assert_eq!(fx.state(&id_d), "approved");

    // (b) Destination moved after approval: the operator re-points the
    // publication binding; refused before any door request.
    let (draft_e, revision_e, _) = fx.draft_with_image("e", &source);
    let (id_e, digest_e) = fx.staged(&draft_e, revision_e, "e");
    let live = fx.op(
        "app_binding_list",
        json!({"install_id": fx.install, "context_id": fx.context}),
    )["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["slot"] == "publication" && b["state"] == "configured")
        .unwrap()
        .clone();
    fx.op(
        "app_binding_publish_set",
        json!({"install_id": fx.install, "binding_id": live["id"],
            "expected_revision": live["revision"], "destination_id": "17841400000000001",
            "destination_label": "Elsewhere", "toolkit": "instagram",
            "timezone": "Asia/Hong_Kong", "grant_id": "dpq_typed_on_binding"}),
    );
    let from = fx.mark();
    let refused = fx.publish_now(&id_e, &digest_e).unwrap_err().to_string();
    assert!(
        fx.since(from).is_empty(),
        "(b) destination: {:?}",
        fx.since(from)
    );
    assert!(
        refused.contains("binding changed since approval"),
        "{refused}"
    );
    assert_eq!(fx.state(&id_e), "approved");

    // Only (a), the old-form (c) effect and the lying-receipt case (whose
    // refusal can only follow the import) ever imported an image.
    assert_eq!(
        fx.door.imports.lock().unwrap().len(),
        imports_after_a + 2,
        "an image was imported for a refused effect"
    );
}
