//! CAD-1939: one draft to an Instagram account and a Facebook Page — a
//! publication binding per destination.
//!
//! A real in-process daemon (`--features test-seam`) and the AgenticOS hosted
//! publish door as a fake behind an HTTP CONNECT proxy (`ALL_PROXY`), as in
//! `cad1328_acceptance`. The door lists one Instagram account and one
//! Facebook Page and echoes each send's own destination.
//!
//! Positive behaviour only (the refusal acceptance check is the reviewer's):
//! - the destination list carries both rows with their toolkit;
//! - `app_binding_use_destination` ADDS the Page's binding next to the
//!   account's instead of replacing it;
//! - one draft revision staged twice (IG, FB) is two effects, and each
//!   confirms and sends on its own;
//! - a text-only Facebook effect stages and sends; Instagram still needs an
//!   image;
//! - changing one destination's binding stops only that destination's
//!   effect;
//! - with one destination, a stage that names none is today's behaviour.
#![cfg(feature = "test-seam")]

#[path = "fixtures/aos_media_door.rs"]
mod aos_media_door;

use cadence_agent::platform::deployments::DeploymentMetadata;
use cadence_agent::store::app_records::RecordStore;
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon, operator_auth};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const HOSTED: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"}]}"#;
const PREFIX: &str = "/v1/runtime/connectors/hosted-publish";
const WORKSPACE: &str = "ws_1939";
const IG_CONN: &str = "conn_aos_ig_1939";
const IG: &str = "17841400008461939";
const FB_CONN: &str = "conn_aos_fb_1939";
/// A second Instagram account: a context may publish to any number per toolkit.
const IG2_CONN: &str = "conn_aos_ig2_1939";
const IG2: &str = "17841400008462939";
const FB: &str = "104471939000001";
const HANDLE: &str = "juicysuite_crm";
const CAPTION: &str = "CAD-1939 caption";

struct Door {
    media: aos_media_door::MediaDoor,
    /// Every `POST {PREFIX}/publish` body, in order.
    published: Mutex<Vec<Value>>,
}

fn fake_door() -> (String, Arc<Door>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let door = Arc::new(Door {
        media: aos_media_door::MediaDoor::new_small(),
        published: Mutex::new(Vec::new()),
    });
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
    // Each send's own destination is echoed back.
    let echo = |sent: &Value| {
        json!({
            "key": sent["key"],
            "destinationId": sent["grant"]["destinationId"],
            "captionDigest": sent["grant"]["captionDigest"],
            "imageDigest": sent["grant"]["imageDigest"],
        })
    };
    let hex = |bytes: &[u8]| {
        use sha2::Digest as _;
        format!("{:x}", sha2::Sha256::digest(bytes))
    };
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
        ("GET", r) if r == format!("{PREFIX}/destinations") => {
            json_reply(json!({"ok":true,"data":{"version":"1","destinations":[
                {"connectionId":IG_CONN,"toolkit":"instagram","displayName":"Harbour stills","destinationId":IG,"status":"active","available":true,"publishable":true},
                {"connectionId":FB_CONN,"toolkit":"facebook","displayName":"Harbour Page","destinationId":FB,"status":"active","available":true,"publishable":true},
                {"connectionId":IG2_CONN,"toolkit":"instagram","displayName":"Kowloon stills","destinationId":IG2,"status":"active","available":true,"publishable":true}]}}))
        }
        ("GET", r) if r == format!("{PREFIX}/publish/grants") => {
            json_reply(json!({"ok":true,"data":{"grants":[
                {"kind":"standing","id":"dpq_standing_ig_1939","workspaceId":WORKSPACE,"connectionId":IG_CONN,"destinationId":IG,"toolkit":"instagram","expiresAt":null,"revokedAt":null},
                {"kind":"standing","id":"dpq_standing_fb_1939","workspaceId":WORKSPACE,"connectionId":FB_CONN,"destinationId":FB,"toolkit":"facebook","expiresAt":null,"revokedAt":null},
                {"kind":"standing","id":"dpq_standing_ig2_1939","workspaceId":WORKSPACE,"connectionId":IG2_CONN,"destinationId":IG2,"toolkit":"instagram","expiresAt":null,"revokedAt":null}]}}))
        }
        ("POST", r) if r == format!("{PREFIX}/media/import") => {
            let digest = query_value("digest").unwrap_or_default();
            let connection = query_value("connectionId").unwrap_or_default();
            json_reply(json!({"ok":true,"data":{
                "mediaKey": format!("dp1.{WORKSPACE}.{connection}.{}", &digest[..32]),
                "connectionId": connection,
                "digest": hex(&body), "mime": header("content-type").unwrap_or_default(),
                "sizeBytes": body.len(),
                "readBack": {"bytes": body.len(), "digest": hex(&body)},
            }}))
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
            door.published.lock().unwrap().push(sent.clone());
            let permalink = if sent["grant"]["destinationId"] == FB {
                "https://www.facebook.com/104471939000001/posts/1939"
            } else {
                "https://www.instagram.com/p/CAD1939/"
            };
            let mut data = echo(&sent);
            data["version"] = json!("1");
            data["result"] = json!({"key": sent["key"], "decision": "approved", "executed": true,
                "status": "posted", "permalink": permalink, "repeated": false});
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
    stop: Arc<AtomicBool>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
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
        let root = tempfile::Builder::new().prefix("c1939").tempdir().unwrap();
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

    fn op_result(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(Asserted::Operator, || {
            client::rpc_timeout(&self.dir(), method, params, Duration::from_secs(60))
        })
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.op_result(method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }

    /// A screen action: the session credentials plus the action token.
    fn screen(&self, method: &str, mut params: Value) -> cadence_agent::Result<Value> {
        params["action_token"] = self.action_token.clone();
        params["token"] = self.session["token"].clone();
        params["key"] = self.session["key"].clone();
        params["origin"] = json!("loopback");
        self.op_result(method, params)
    }

    /// Social Content with its source and image slots bound to the hosted
    /// account, its publication slot not yet pointed at any destination, and
    /// a mounted screen session.
    fn setup(&mut self) {
        let app = self.root.path().join("app");
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        copy_dir(
            &std::path::Path::new(manifest_dir).join("tests/fixtures/apps/social-content"),
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
        let hosted = connections
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["account"] == "hosted")
            .expect("hosted connection")["id"]
            .clone();
        let context = self.op(
            "app_context_create",
            json!({"install_id": install, "label": "EF", "input_defaults": {},
                "request_id": "ctx-1939"}),
        );
        let context = context["context"]["id"]
            .as_str()
            .or_else(|| context["id"].as_str())
            .unwrap()
            .to_string();
        for slot in ["source", "image"] {
            self.op(
                "app_binding_create",
                json!({"install_id": install, "context_id": context, "slot": slot,
                    "connection_id": hosted, "request_id": format!("bind-{slot}")}),
            );
        }
        RecordStore::open(&self.dir(), &install)
            .unwrap()
            .app_social_sources_save(&context, 0, &[HANDLE.to_owned()], "src-1939")
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
        self.install = install;
        self.context = context;
        self.mount();
    }

    /// Mint and consume a screen mount.
    fn mount(&mut self) {
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

    /// The operator chooses a destination: add (or, with a revision, update)
    /// that destination's own binding.
    fn use_destination(&self, toolkit: &str, destination: &str, extra: Value) -> Value {
        let mut params = json!({"install_id": self.install, "context_id": self.context,
            "destination_id": destination, "toolkit": toolkit});
        for (key, value) in extra.as_object().unwrap() {
            params[key] = value.clone();
        }
        self.op("app_binding_use_destination", params)
    }

    /// A text-only draft from a fetched source post. Returns (draft, revision).
    fn draft(&self, tag: &str) -> (String, i64) {
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
        (created["draft_id"].as_str().unwrap().to_string(), 1)
    }

    /// Generate and attach an image to revision 1 of `draft`. Returns the
    /// new revision.
    fn attach_image(&self, tag: &str, draft: &str) -> i64 {
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
        attached["revision"].as_i64().unwrap()
    }

    /// Stage `draft@revision`, naming `destination` when given.
    fn stage(
        &self,
        tag: &str,
        draft: &str,
        revision: i64,
        destination: Option<&str>,
    ) -> cadence_agent::Result<(String, String, Value)> {
        let mut params = json!({"tool_alias": "social.draft",
            "proof": {"kind": "social_draft", "draft_id": draft, "revision": revision},
            "request_id": format!("stage-{tag}")});
        if let Some(destination) = destination {
            params["destination_id"] = json!(destination);
        }
        let staged = self.screen("app_effect_stage", params)?;
        let effect = staged["effect"].clone();
        Ok((
            effect["effect_id"].as_str().unwrap().to_string(),
            effect["digest"].as_str().unwrap().to_string(),
            effect,
        ))
    }

    fn confirm(&self, id: &str, digest: &str) -> cadence_agent::Result<Value> {
        self.op_result(
            "app_effect_confirm_publish",
            json!({"effect_id": id, "digest": digest}),
        )
    }

    fn effect(&self, id: &str) -> Value {
        RecordStore::open(&self.dir(), &self.install)
            .unwrap()
            .app_social_effect_show(id)
            .unwrap()["effect"]
            .clone()
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

#[test]
fn cad1939_one_draft_posts_to_an_instagram_account_and_a_facebook_page() {
    let fx = Fx::start();

    // The destination list carries both rows with their toolkit.
    let listed = fx.op("social_destinations", json!({}));
    let rows: Vec<(String, String)> = listed["destinations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["toolkit"].as_str().unwrap().to_string(),
                row["destination_id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("instagram".to_string(), IG.to_string()),
            ("facebook".to_string(), FB.to_string()),
            ("instagram".to_string(), IG2.to_string())
        ]
    );

    // One destination: the stage names none, exactly as before.
    let ig_binding = fx.use_destination("instagram", IG, json!({"request_id": "use-ig"}));
    let ig_id = ig_binding["binding"]["id"].as_str().unwrap().to_string();
    let (draft, text_revision) = fx.draft("one");
    let revision = fx.attach_image("one", &draft);
    let (single, single_digest, effect) = fx.stage("single", &draft, revision, None).unwrap();
    assert_eq!(effect["authority"]["destination_id"], IG);
    assert_eq!(effect["authority"]["binding"]["id"], ig_id.as_str());

    // Choosing the Page ADDS its own binding; the account's is untouched.
    let fb_binding = fx.use_destination("facebook", FB, json!({"request_id": "use-fb"}));
    let fb_id = fb_binding["binding"]["id"].as_str().unwrap().to_string();
    assert_ne!(fb_id, ig_id);
    let bindings = fx.op(
        "app_binding_list",
        json!({"install_id": fx.install, "context_id": fx.context}),
    );
    let live: Vec<(String, i64)> = bindings["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["slot"] == "publication" && b["state"] == "configured")
        .map(|b| {
            (
                b["config"]["publish"]["toolkit"]
                    .as_str()
                    .unwrap()
                    .to_string(),
                b["revision"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(live.len(), 2, "two live publication bindings: {bindings}");
    // A second Instagram account adds a third binding: not one per toolkit.
    let ig2_binding = fx.use_destination("instagram", IG2, json!({"request_id": "use-ig2"}));
    let ig2_id = ig2_binding["binding"]["id"].as_str().unwrap().to_string();
    assert!(ig2_id != ig_id && ig2_id != fb_id);
    assert!(live.contains(&("instagram".to_string(), 1)));
    assert!(live.contains(&("facebook".to_string(), 1)));
    // The single-destination effect staged before still confirms and posts.
    let posted = fx.confirm(&single, &single_digest).unwrap();
    assert_eq!(posted["effect"]["state"], "posted", "{posted}");

    // One revision staged twice: two effects, each to its own destination.
    let (to_ig, ig_digest, ig_effect) = fx.stage("ig", &draft, revision, Some(IG)).unwrap();
    let (to_fb, fb_digest, fb_effect) = fx.stage("fb", &draft, revision, Some(FB)).unwrap();
    assert_ne!(to_ig, to_fb);
    assert_eq!(
        (
            &ig_effect["authority"]["toolkit"],
            &ig_effect["authority"]["binding"]["id"]
        ),
        (&json!("instagram"), &json!(ig_id))
    );
    assert_eq!(
        (
            &fb_effect["authority"]["toolkit"],
            &fb_effect["authority"]["binding"]["id"],
            &fb_effect["authority"]["aos_connection_id"]
        ),
        (&json!("facebook"), &json!(fb_id), &json!(FB_CONN))
    );

    // Changing the Page's binding stops only the Page's effect.
    fx.use_destination(
        "facebook",
        FB,
        json!({"expected_revision": 1, "timezone": "Asia/Tokyo"}),
    );
    let refused = fx.confirm(&to_fb, &fb_digest).unwrap_err().to_string();
    assert!(
        refused.contains("social publish binding changed since staging"),
        "{refused}"
    );
    assert_eq!(fx.effect(&to_fb)["state"], "waiting");
    let sent = fx.confirm(&to_ig, &ig_digest).unwrap();
    assert_eq!(sent["effect"]["state"], "posted", "{sent}");

    // A fresh Page effect at the new binding confirms and posts on its own,
    // with the Facebook permalink in its outcome.
    let (to_fb, fb_digest, _) = fx.stage("fb2", &draft, revision, Some(FB)).unwrap();
    let sent = fx.confirm(&to_fb, &fb_digest).unwrap();
    assert_eq!(sent["effect"]["state"], "posted", "{sent}");
    assert_eq!(
        fx.effect(&to_fb)["outcome"]["permalink"],
        "https://www.facebook.com/104471939000001/posts/1939"
    );

    // Facebook without an image: a text-only draft stages and sends to the
    // Page; Instagram still refuses it.
    let (text_draft, _) = fx.draft("text");
    let ig_text = fx
        .stage("ig-text", &text_draft, text_revision, Some(IG))
        .unwrap_err()
        .to_string();
    assert!(
        ig_text.contains("Instagram publish needs a retained JPEG or PNG image"),
        "{ig_text}"
    );
    let (fb_text, fb_text_digest, fb_text_effect) = fx
        .stage("fb-text", &text_draft, text_revision, Some(FB))
        .unwrap();
    assert!(fb_text_effect["authority"]["asset_id"].is_null());
    let sent = fx.confirm(&fb_text, &fb_text_digest).unwrap();
    assert_eq!(sent["effect"]["state"], "posted", "{sent}");

    // The second Instagram account posts on its own binding.
    let (to_ig2, ig2_digest, ig2_effect) = fx.stage("ig2", &draft, revision, Some(IG2)).unwrap();
    assert_eq!(ig2_effect["authority"]["binding"]["id"], ig2_id.as_str());
    let sent = fx.confirm(&to_ig2, &ig2_digest).unwrap();
    assert_eq!(sent["effect"]["state"], "posted", "{sent}");

    // Each confirmed effect reached the door once, to its own destination.
    let destinations: Vec<Value> = fx
        .door
        .published
        .lock()
        .unwrap()
        .iter()
        .map(|body| body["grant"]["destinationId"].clone())
        .collect();
    assert_eq!(
        destinations,
        vec![json!(IG), json!(IG), json!(FB), json!(FB), json!(IG2)]
    );
    let text_send = fx.door.published.lock().unwrap()[3].clone();
    assert!(
        text_send.get("mediaKey").is_none(),
        "the text-only Page post carries no media"
    );
}
