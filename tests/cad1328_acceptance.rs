//! CAD-1328 acceptance check — written by the Spec/security reviewer, not
//! the implementer. The implementer may not edit or weaken it.
//!
//! A real in-process daemon and a real board over the `--features test-seam`
//! caller-identity harness; the AgenticOS hosted publish door is a fake
//! behind an HTTP CONNECT proxy (`ALL_PROXY`), as in
//! `cad1304_instagram_image_publish`, that logs every request it sees.
//!
//! The rules it proves, from the ticket ("a human's own action in the app is
//! the approval"):
//! (1) Only the operator can confirm-and-send a staged social draft: an agent
//!     or unproven caller is refused on the daemon RPC
//!     `app_effect_confirm_publish` AND on its board relay
//!     `POST /api/app-effects/<id>/confirm-publish` (with or without riding
//!     an operator session, and an operator session without the board's
//!     request headers), and nothing reaches the door.
//! (2) A forged or another effect's digest is refused.
//! (3) Only a social draft effect id is eligible (allow-list).
//! (4) A replayed confirm on a posted effect sends nothing; a repeated
//!     confirm / Publish now on a `sending` effect never executes again.
//! (5) Only an operator-proven context save re-pins the context's bindings,
//!     and the re-pin changes ONLY the `context` field: a changed connection
//!     still reads drifted after it.
//! (6) A send whose status never turns terminal leaves the effect `sending`
//!     with a plain reason and no second execute.
#![cfg(feature = "test-seam")]

use cadence_agent::platform::deployments::DeploymentMetadata;
use cadence_agent::store::app_records::RecordStore;
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
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
const AOS_CONN: &str = "conn_aos_1328";
const WORKSPACE: &str = "ws_1328";
const DEST: &str = "17841400008461328";
const STANDING: &str = "dpq_standing_cad1328";
const HANDLE: &str = "juicysuite_crm";
const CAPTION: &str = "CAD-1328 caption";

/// A board session: (cookie, `X-Cadence-Session` key).
type Session = (String, String);

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
    fn status_reads(&self) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p)| m == "GET" && p.starts_with(PREFIX) && p.ends_with("/status"))
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
            json!({"ok":true,"data":{"version":"1","destinations":[{"connectionId":AOS_CONN,"toolkit":"instagram","displayName":"Harbour stills","destinationId":DEST,"status":"active","available":true,"publishable":true}]}}),
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
                        data["permalink"] = json!("https://www.instagram.com/p/CAD1328/");
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
                    "status": "posted", "permalink": "https://www.instagram.com/p/CAD1328/",
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

struct Fx {
    _serial: std::sync::MutexGuard<'static, ()>,
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    board: Option<(u16, Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
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
        let root = tempfile::Builder::new().prefix("c1328").tempdir().unwrap();
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
            board: None,
            door,
            install: String::new(),
            context: String::new(),
            session: Value::Null,
            action_token: Value::Null,
        };
        fx.setup();
        fx.start_board();
        fx
    }

    fn start_board(&mut self) {
        let free = |p: &u16| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok();
        let port = (3110..3200)
            .find(free)
            .expect("a free board port in 3110-3199");
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            startup: Some(tx),
            test_seam: true,
            ..Default::default()
        };
        let (dir, pm) = (self.dir(), self.root.path().join("pm"));
        let board = std::thread::spawn(move || {
            let _ = cadence_agent::ui::serve(&dir, &pm, &opts);
        });
        rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        self.board = Some((port, stop, board));
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

    /// `POST <path>` on the board as `who`. `board_headers: false` drops the
    /// board's same-origin request headers (`Origin`, `X-Cadence-Board`)
    /// and sends a foreign `Origin` instead. (status, reply, set-cookie).
    fn http_post_with(
        &self,
        who: &Asserted,
        path: &str,
        body: &Value,
        session: Option<&Session>,
        board_headers: bool,
    ) -> (u16, String, Option<String>) {
        let port = self.board.as_ref().unwrap().0;
        let host = format!("cadence-{port}.localhost:{port}");
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let mut request = agent
            .post(format!("http://127.0.0.1:{port}{path}"))
            .header("Host", &host)
            .header("Content-Type", "application/json")
            .header(AS_HEADER, who.as_str())
            .header(TOKEN_HEADER, Seam::token_at(&self.dir()).unwrap());
        request = if board_headers {
            request
                .header("Origin", format!("http://{host}"))
                .header("X-Cadence-Board", "1")
        } else {
            request.header("Origin", "http://evil.example")
        };
        if let Some((cookie, key)) = session {
            request = request
                .header("Cookie", cookie)
                .header("X-Cadence-Session", key);
        }
        let mut response = request.send(body.to_string()).unwrap();
        let status = response.status().as_u16();
        let cookie = response
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap().to_string());
        (
            status,
            response.body_mut().read_to_string().unwrap(),
            cookie,
        )
    }

    fn http_post(
        &self,
        who: &Asserted,
        path: &str,
        body: &Value,
        session: Option<&Session>,
    ) -> (u16, String) {
        let (status, reply, _) = self.http_post_with(who, path, body, session, true);
        (status, reply)
    }

    /// The operator's real board session, minted from the operator secret.
    fn operator_session(&self) -> Session {
        let secret = operator_auth::read_secret(&self.dir()).unwrap();
        let nonce = self.op(
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )["nonce"]
            .clone();
        let (status, reply, cookie) = self.http_post_with(
            &Asserted::Operator,
            "/api/session",
            &json!({"nonce": nonce}),
            None,
            true,
        );
        assert_eq!(status, 200, "operator session exchange failed: {reply}");
        let key = serde_json::from_str::<Value>(&reply).unwrap()["session_key"]
            .as_str()
            .unwrap()
            .to_string();
        (cookie.unwrap(), key)
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
                "request_id": "ctx-1328"}),
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
            .app_social_sources_save(&context, 0, &[HANDLE.to_owned()], "src-1328")
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

    /// Fetch, draft a caption, generate and attach an image, then stage the
    /// publish effect through the screen (it waits for the operator).
    /// Returns `(effect_id, digest)`.
    fn staged_draft(&self, tag: &str) -> (String, String) {
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
        let revision = attached["revision"].as_i64().unwrap();
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
        assert_eq!(self.state(&id), "waiting");
        (id, digest)
    }

    fn confirm(&self, who: Asserted, id: &str, digest: &str) -> cadence_agent::Result<Value> {
        self.rpc_as(
            who,
            "app_effect_confirm_publish",
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

    /// Every binding of the context as the operator reads it, by slot:
    /// (config, drift state, drift changes).
    fn bindings(&self) -> HashMap<String, (Value, String, Value)> {
        let listed = self.op(
            "app_binding_list",
            json!({"install_id": self.install, "context_id": self.context}),
        );
        listed["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                (
                    b["slot"].as_str().unwrap().to_string(),
                    (
                        b["config"].clone(),
                        b["drift"]["state"].as_str().unwrap_or("").to_string(),
                        b["drift"]["changes"].clone(),
                    ),
                )
            })
            .collect()
    }

    fn context_revision(&self) -> i64 {
        let shown = self.op(
            "app_context_show",
            json!({"install_id": self.install, "context_id": self.context}),
        );
        shown["context"]["revision"]
            .as_i64()
            .or_else(|| shown["revision"].as_i64())
            .unwrap_or_else(|| panic!("no context revision: {shown}"))
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        if let Some((_, stop, board)) = self.board.take() {
            stop.store(true, SeqCst);
            let _ = board.join();
        }
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.daemon.take() {
            let _ = handle.join();
        }
    }
}

fn refused_as_not_operator(result: cadence_agent::Result<Value>, what: &str) {
    let refusal = result.expect_err(what).to_string();
    assert!(
        refusal.contains("operator"),
        "{what}: refused for another reason: {refusal}"
    );
}

/// (1) (2) (3) (4): only the operator confirms, only a social draft effect,
/// only at its own digest, and a replay never sends again.
#[test]
fn only_the_operator_confirms_a_social_draft_at_its_digest_and_a_replay_sends_nothing() {
    let fx = Fx::start();
    let (id, digest) = fx.staged_draft("g");
    let (other_id, other_digest) = fx.staged_draft("o");
    let path = format!("/api/app-effects/{id}/confirm-publish");
    let body = json!({"digest": digest});
    let callers = [Asserted::Agent("cc13-worker".into()), Asserted::Unproven];

    // (1) Agent and unproven callers: refused on the RPC and on the board
    // relay, alone or riding a fresh operator session.
    for who in &callers {
        refused_as_not_operator(
            fx.confirm(who.clone(), &id, &digest),
            &format!("{who:?} confirmed over RPC"),
        );
        for carried in [None, Some(fx.operator_session())] {
            let (status, reply) = fx.http_post(who, &path, &body, carried.as_ref());
            assert_eq!(
                status,
                403,
                "{who:?} confirmed over the board (session: {}): {reply}",
                carried.is_some()
            );
        }
    }
    // The operator's session without the board's own request headers (a
    // cross-site form or fetch) is refused too.
    let (status, reply, _) = fx.http_post_with(
        &Asserted::Operator,
        &path,
        &body,
        Some(&fx.operator_session()),
        false,
    );
    assert!(
        status == 403 || status == 400,
        "a cross-origin confirm reached the daemon: {status} {reply}"
    );

    // (2) A forged digest and another effect's real digest: refused even
    // for the operator, over RPC and HTTP.
    let forged = "0".repeat(64);
    for wrong in [forged.as_str(), other_digest.as_str()] {
        let refusal = fx
            .confirm(Asserted::Operator, &id, wrong)
            .expect_err("operator confirmed at a wrong digest")
            .to_string();
        assert!(
            refusal.contains("digest is stale"),
            "wrong digest refused for another reason: {refusal}"
        );
        let (status, reply) = fx.http_post(
            &Asserted::Operator,
            &path,
            &json!({"digest": wrong}),
            Some(&fx.operator_session()),
        );
        assert_eq!(status, 409, "wrong digest over HTTP: {reply}");
    }

    // (3) Only a social draft effect is eligible for the one-step path.
    for not_social in ["effect-0123456789abcdef", "sfx_zz_short", "run-effect-1"] {
        let refusal = fx
            .confirm(Asserted::Operator, not_social, &digest)
            .expect_err("a non-social effect was confirmed")
            .to_string();
        assert!(
            refusal.contains("only a social draft effect"),
            "{not_social} refused for another reason: {refusal}"
        );
    }

    // Nothing above reached the door or moved either effect.
    assert_eq!(fx.state(&id), "waiting");
    assert_eq!(fx.state(&other_id), "waiting");
    assert_eq!(fx.door.all_posts(), 0, "a refused confirm reached the door");

    // Not vacuous: the operator's own tap over the board posts it, once.
    let (status, reply) = fx.http_post(
        &Asserted::Operator,
        &path,
        &body,
        Some(&fx.operator_session()),
    );
    assert_eq!(status, 200, "the operator could not confirm: {reply}");
    assert_eq!(fx.state(&id), "posted", "{reply}");
    assert_eq!(fx.door.posts("/publish"), 1);
    assert_eq!(fx.door.posts("/media/import"), 1);
    let after = fx.door.all_posts();

    // (4) A replayed confirm, over RPC and HTTP, sends nothing more (a
    // receipt or a refusal are both fine; a second send is not).
    let replay = fx.confirm(Asserted::Operator, &id, &digest);
    assert_eq!(
        fx.door.all_posts(),
        after,
        "a replay reached the door: {replay:?}"
    );
    let (status, reply) = fx.http_post(
        &Asserted::Operator,
        &path,
        &body,
        Some(&fx.operator_session()),
    );
    assert!(status == 200 || status == 409, "{status} {reply}");
    assert_eq!(
        fx.door.all_posts(),
        after,
        "a replay reached the door: {reply}"
    );
    assert_eq!(fx.state(&id), "posted");
    assert_eq!(fx.door.posts("/publish"), 1, "posted twice");
    // The other effect still waits for its own confirmation.
    assert_eq!(fx.state(&other_id), "waiting");
}

/// (5): only an operator-proven save re-pins, and only the `context` field.
#[test]
fn only_an_operator_context_save_re_pins_and_only_the_context_field() {
    let fx = Fx::start();
    let before = fx.bindings();
    assert_eq!(before.len(), 3, "{before:?}");
    assert!(
        before.values().all(|(_, drift, _)| drift == "current"),
        "{before:?}"
    );
    assert_eq!(fx.context_revision(), 1);
    let update_params = json!({"install_id": fx.install, "context_id": fx.context,
        "expected_revision": 1, "label": "EF renamed", "input_defaults": {}});
    let update_path = format!(
        "/api/app-installations/{}/contexts/{}/update",
        fx.install, fx.context
    );
    let update_body = json!({"expected_revision": 1, "label": "EF renamed", "input_defaults": {}});

    // Agent and unproven callers cannot save the context (RPC or board) nor
    // choose the destination, so nothing is saved and nothing re-pinned.
    for who in [Asserted::Agent("cc13-worker".into()), Asserted::Unproven] {
        refused_as_not_operator(
            fx.rpc_as(who.clone(), "app_context_update", update_params.clone()),
            &format!("{who:?} saved the context"),
        );
        refused_as_not_operator(
            fx.rpc_as(
                who.clone(),
                "app_binding_use_destination",
                json!({"install_id": fx.install, "context_id": fx.context,
                    "destination_id": DEST, "request_id": "use-agent"}),
            ),
            &format!("{who:?} chose the destination"),
        );
        for carried in [None, Some(fx.operator_session())] {
            let (status, reply) = fx.http_post(&who, &update_path, &update_body, carried.as_ref());
            assert_eq!(
                status,
                403,
                "{who:?} saved the context over the board (session: {}): {reply}",
                carried.is_some()
            );
        }
    }
    assert_eq!(
        fx.context_revision(),
        1,
        "a refused caller saved the context"
    );
    assert_eq!(
        fx.bindings(),
        before,
        "a refused caller re-pinned a binding"
    );

    // The connection under the source and image bindings is no longer the
    // one they were bound to (their stored connection receipt names an
    // account the connection does not have now): they read drifted. The
    // stored receipt is rewritten with its own valid digest, as the store
    // would have written it.
    let db = rusqlite::Connection::open(fx.dir().join("cadence.sqlite3")).unwrap();
    for slot in ["source", "image"] {
        let mut config = before[slot].0.clone();
        config["account"] = json!("an-account-bound-earlier");
        let digest = cadence_agent::store::app_runs::material_digest(&json!({
            "kind": "app-binding-v1", "install_id": fx.install,
            "context_id": fx.context, "slot": slot, "config": config}));
        let changed = db
            .execute(
                "UPDATE app_bindings SET config=?1, digest=?2 WHERE install_id=?3 AND context_id=?4 AND slot=?5 AND state='configured'",
                rusqlite::params![config.to_string(), digest, fx.install, fx.context, slot],
            )
            .unwrap();
        assert_eq!(changed, 1, "the {slot} binding row");
    }
    drop(db);
    let before = fx.bindings();
    let drifted = &before;
    for slot in ["source", "image"] {
        assert_eq!(
            drifted[slot].1, "needs_confirm",
            "{slot}: {:?}",
            drifted[slot]
        );
    }
    assert_eq!(drifted["publication"].1, "current");

    // The operator's own save re-pins every binding of the context...
    fx.op("app_context_update", update_params.clone());
    assert_eq!(fx.context_revision(), 2);
    let after = fx.bindings();
    let strip = |config: &Value| {
        let mut config = config.clone();
        config.as_object_mut().unwrap().remove("context");
        config
    };
    for (slot, (config, drift, changes)) in &after {
        assert_eq!(
            config["context"]["revision"], 2,
            "{slot} was not re-pinned: {config}"
        );
        // ...and changes ONLY the context field of the receipt.
        assert_eq!(
            strip(config),
            strip(&before[slot].0),
            "{slot}: the re-pin changed more than the context"
        );
        if slot == "publication" {
            assert_eq!(drift, "current", "{slot}: {changes}");
        } else {
            // The changed connection still needs the operator.
            assert_eq!(drift, "needs_confirm", "{slot}: {changes}");
            let fields: Vec<&str> = changes
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|c| c["field"].as_str())
                .collect();
            assert!(fields.contains(&"account"), "{slot}: {changes}");
            assert!(!fields.contains(&"context"), "{slot}: {changes}");
        }
    }
}

/// (4) (6): a send that never settles stays `sending` with a plain reason;
/// repeating the confirm or Publish now never sends again. How and when the
/// daemon re-reads status is not pinned here, only that it never re-sends.
#[test]
fn an_unsettled_send_stays_sending_and_a_repeat_never_sends_again() {
    let fx = Fx::start();
    let (id, digest) = fx.staged_draft("s");
    fx.door.processing.store(true, SeqCst);
    *fx.door.status.lock().unwrap() = "processing".into();

    let started = Instant::now();
    let reply = fx
        .confirm(Asserted::Operator, &id, &digest)
        .unwrap_or_else(|e| panic!("confirm refused: {e}"));
    assert!(
        reply["send_error"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty()),
        "an unsettled send gave no plain reason: {reply}"
    );
    // Past any settle window the daemon runs, AgenticOS never answered: the
    // effect is still `sending`, sent exactly once.
    while started.elapsed() < Duration::from_secs(75) {
        std::thread::sleep(Duration::from_millis(500));
    }
    assert_eq!(fx.state(&id), "sending", "{reply}");
    assert_eq!(fx.door.posts("/publish"), 1);
    let sent = fx.door.all_posts();

    // A repeated confirm on the `sending` effect: no second send.
    let again = fx.confirm(Asserted::Operator, &id, &digest);
    assert_eq!(
        fx.door.all_posts(),
        sent,
        "a repeated confirm reached the door: {again:?}"
    );
    assert_eq!(fx.state(&id), "sending", "{again:?}");

    // AgenticOS settles it: a re-check (Publish now) reads status and the
    // effect settles `posted`, still with no second send of any kind.
    *fx.door.status.lock().unwrap() = "posted".into();
    let reads = fx.door.status_reads();
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut last = None;
    while fx.state(&id) != "posted" {
        assert!(
            Instant::now() < deadline,
            "a settled send never read posted: {last:?}"
        );
        last = Some(fx.rpc_as(
            Asserted::Operator,
            "app_effect_publish_now",
            json!({"effect_id": id, "digest": digest}),
        ));
        assert_eq!(fx.door.all_posts(), sent, "a re-check reached the door");
        let pause = Instant::now() + Duration::from_secs(5);
        while Instant::now() < pause && fx.state(&id) != "posted" {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    assert!(fx.door.status_reads() > reads, "status was not read");
    assert_eq!(fx.door.all_posts(), sent, "a re-check reached the door");

    // And a confirm after that is a replay.
    let replay = fx.confirm(Asserted::Operator, &id, &digest);
    assert_eq!(fx.door.all_posts(), sent, "{replay:?}");
    assert_eq!(fx.state(&id), "posted");
    assert_eq!(fx.door.posts("/publish"), 1, "posted twice");
}
