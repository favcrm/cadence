//! CAD-1939 refusal acceptance check — written by the Spec/security reviewer,
//! not the implementer. The implementer may not edit or weaken it.
//!
//! A real in-process daemon and a real board (`--features test-seam`), with
//! the AgenticOS hosted publish door as a fake behind an HTTP CONNECT proxy
//! (`ALL_PROXY`), as in `cad1939_destinations`. The door lists two Instagram
//! accounts and one Facebook Page of this company.
//!
//! The rule it proves, from the ticket: each destination publishes only
//! through its OWN live publication binding in the staging context, and only
//! the operator chooses those bindings, only among the company's live
//! destinations. Refused, with nothing staged, written or sent:
//! (a) a stage naming a destination with no live binding in this context;
//! (b) a stage naming a destination bound only in ANOTHER context of the same
//!     install, or only in another install;
//! (c) a stage naming no destination while the context has two live ones;
//! (d) confirm and schedule (RPC and board) of an effect after THAT effect's
//!     binding was changed to a new revision, or revoked — while the other
//!     destination's effect still confirms and sends;
//! (e) `app_binding_use_destination` (RPC and its board relay) by an agent or
//!     unproven caller, to create or to update a destination binding;
//! (f) `app_binding_use_destination` naming a destination that is not in the
//!     company's live list (a forged id, a listed id under the wrong toolkit,
//!     an unknown toolkit), even for the operator, over RPC and HTTP.
#![cfg(feature = "test-seam")]

#[path = "fixtures/aos_media_door.rs"]
mod aos_media_door;

use cadence_agent::platform::deployments::DeploymentMetadata;
use cadence_agent::store::app_records::RecordStore;
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon, operator_auth};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const HOSTED: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"}]}"#;
const PREFIX: &str = "/v1/runtime/connectors/hosted-publish";
const WORKSPACE: &str = "ws_1939r";
const IG_CONN: &str = "conn_aos_ig_1939r";
const IG: &str = "17841400008471939";
const FB_CONN: &str = "conn_aos_fb_1939r";
const FB: &str = "104471939000077";
const IG2_CONN: &str = "conn_aos_ig2_1939r";
const IG2: &str = "17841400008472939";
/// Not this company's: the door never lists it.
const FORGED: &str = "17841499999991939";
const HANDLE: &str = "juicysuite_crm";
const CAPTION: &str = "CAD-1939 refusal caption";

/// A board session: (cookie, `X-Cadence-Session` key).
type Session = (String, String);

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
        let root = tempfile::Builder::new().prefix("c1939r").tempdir().unwrap();
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
        fx.start_board();
        fx.setup();
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

    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let dir = self.dir();
        scoped(who, || {
            client::rpc_timeout(&dir, method, params, Duration::from_secs(60))
        })
    }

    fn op_result(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        self.rpc(Asserted::Operator, method, params)
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.op_result(method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }

    /// `POST <path>` on the board as `who` (seam headers), optionally with a
    /// board session. (status, reply).
    fn http_post(
        &self,
        who: &Asserted,
        path: &str,
        body: &Value,
        session: Option<&Session>,
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
            .header("Origin", format!("http://{host}"))
            .header("X-Cadence-Board", "1")
            .header("Content-Type", "application/json")
            .header(AS_HEADER, who.as_str())
            .header(TOKEN_HEADER, Seam::token_at(&self.dir()).unwrap());
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

    /// The operator's real board session, exchanged at `/api/session`.
    fn board_session(&self) -> Session {
        let secret = operator_auth::read_secret(&self.dir()).unwrap();
        let nonce = self.op(
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )["nonce"]
            .clone();
        let (status, reply, cookie) = self.http_post(
            &Asserted::Operator,
            "/api/session",
            &json!({"nonce": nonce}),
            None,
        );
        assert_eq!(status, 200, "operator session exchange failed: {reply}");
        let key = serde_json::from_str::<Value>(&reply).unwrap()["session_key"]
            .as_str()
            .unwrap()
            .to_string();
        (cookie.unwrap(), key)
    }

    /// A screen action of context A: the session credentials plus the
    /// action token.
    fn screen(&self, method: &str, mut params: Value) -> cadence_agent::Result<Value> {
        params["action_token"] = self.action_token.clone();
        params["token"] = self.session["token"].clone();
        params["key"] = self.session["key"].clone();
        params["origin"] = json!("loopback");
        self.op_result(method, params)
    }

    /// Social Content (context A) with its source and image slots bound to
    /// the hosted account and a mounted screen session; no destination yet.
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
        let context = self.context_create(&install, "A");
        for slot in ["source", "image"] {
            self.op(
                "app_binding_create",
                json!({"install_id": install, "context_id": context, "slot": slot,
                    "connection_id": hosted, "request_id": format!("bind-{slot}")}),
            );
        }
        RecordStore::open(&self.dir(), &install)
            .unwrap()
            .app_social_sources_save(&context, 0, &[HANDLE.to_owned()], "src-1939r")
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

    fn context_create(&self, install: &str, label: &str) -> String {
        let context = self.op(
            "app_context_create",
            json!({"install_id": install, "label": label, "input_defaults": {},
                "request_id": format!("ctx-1939r-{}", label.to_lowercase())}),
        );
        context["context"]["id"]
            .as_str()
            .or_else(|| context["id"].as_str())
            .unwrap()
            .to_string()
    }

    /// A second installation with one declared send slot (the CAD-1290
    /// acceptance app).
    fn other_install(&self) -> String {
        let source = self.root.path().join("other-src");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(
            source.join("app.md"),
            "---\napp: c1939other\ntitle: C1939 other install\nversion: '0.1.0'\n\
             summary: Publication-slot fixture.\nneeds:\n  connections: []\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send\n---\n\n# C1939 other install\n",
        )
        .unwrap();
        std::fs::write(
            source.join("workflows/post.md"),
            "---\ntitle: \"Post\"\ngoal: \"One post\"\npublication_slot: publication\ninputs:\n  writer: { ask: \"writer\" }\n---\n\n## Write\nagent: {{writer}}\nsize: S\naction: local.text.produce\n\nWrite one post.\n\n### Acceptance\n- [ ] post exists\n",
        )
        .unwrap();
        self.op(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        )["install_id"]
            .as_str()
            .unwrap()
            .into()
    }

    /// The operator adds (or, with `expected_revision`, updates) one
    /// destination's binding. Returns the binding row.
    fn use_destination(
        &self,
        install: &str,
        context: Option<&str>,
        toolkit: &str,
        destination: &str,
        extra: Value,
    ) -> Value {
        let mut params = json!({"install_id": install, "destination_id": destination,
            "toolkit": toolkit});
        if let Some(context) = context {
            params["context_id"] = json!(context);
        }
        for (key, value) in extra.as_object().unwrap() {
            params[key] = value.clone();
        }
        self.op("app_binding_use_destination", params)["binding"].clone()
    }

    /// Every binding row of an install as (id, revision, state, toolkit,
    /// destination), as the operator lists it.
    fn bindings(&self, install: &str) -> Vec<(String, i64, String, Value, Value)> {
        self.op("app_binding_list", json!({"install_id": install}))["bindings"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|b| {
                (
                    b["id"].as_str().unwrap_or_default().to_string(),
                    b["revision"].as_i64().unwrap_or_default(),
                    b["state"].as_str().unwrap_or_default().to_string(),
                    b["config"]["publish"]["toolkit"].clone(),
                    b["config"]["publish"]["destination_id"].clone(),
                )
            })
            .collect()
    }

    /// Every social effect of the main install, as (id, state).
    fn effects(&self) -> Vec<(String, String)> {
        RecordStore::open(&self.dir(), &self.install)
            .unwrap()
            .app_social_effect_list_install()
            .unwrap()["effects"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|e| {
                (
                    e["effect_id"].as_str().unwrap_or_default().to_string(),
                    e["state"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    fn sent(&self) -> Vec<Value> {
        self.door
            .published
            .lock()
            .unwrap()
            .iter()
            .map(|body| body["grant"]["destinationId"].clone())
            .collect()
    }

    fn effect(&self, id: &str) -> Value {
        RecordStore::open(&self.dir(), &self.install)
            .unwrap()
            .app_social_effect_show(id)
            .unwrap()["effect"]
            .clone()
    }

    /// A draft of context A from a fetched source post with an image
    /// attached. Returns (draft, revision with the image).
    fn image_draft(&self) -> (String, i64) {
        let fetched = self
            .screen(
                "app_tool_invoke",
                json!({"tool_alias": "instagram.read", "input": {"handle": HANDLE},
                    "request_id": "fetch-1939r"}),
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
                json!({"tool_alias": "social.draft", "request_id": "draft-1939r",
                    "caption": CAPTION,
                    "source": {"kind": "tool_receipt", "receipt_id": receipt, "post_id": post}}),
            )
            .unwrap_or_else(|e| panic!("draft create: {e}"));
        let draft = created["draft_id"].as_str().unwrap().to_string();
        self.screen(
            "app_tool_invoke",
            json!({"tool_alias": "social.draft", "input": {},
                "request_id": "image-1939r",
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
                json!({"tool_alias": "social.draft", "request_id": "attach-1939r",
                    "draft_id": draft, "expected_revision": 1, "caption": CAPTION,
                    "asset_id": asset}),
            )
            .unwrap_or_else(|e| panic!("attach image: {e}"));
        (draft, attached["revision"].as_i64().unwrap())
    }

    /// Stage `draft@revision` from context A's screen, naming `destination`
    /// when given. Ok((effect id, digest)).
    fn stage(
        &self,
        tag: &str,
        draft: &str,
        revision: i64,
        destination: Option<&str>,
    ) -> cadence_agent::Result<(String, String)> {
        let mut params = json!({"tool_alias": "social.draft",
            "proof": {"kind": "social_draft", "draft_id": draft, "revision": revision},
            "request_id": format!("stage-{tag}")});
        if let Some(destination) = destination {
            params["destination_id"] = json!(destination);
        }
        let staged = self.screen("app_effect_stage", params)?;
        Ok((
            staged["effect"]["effect_id"].as_str().unwrap().to_string(),
            staged["effect"]["digest"].as_str().unwrap().to_string(),
        ))
    }

    /// A stage that must be refused with `reason`, staging and sending
    /// nothing.
    fn stage_refused(
        &self,
        tag: &str,
        draft: &str,
        revision: i64,
        destination: Option<&str>,
        reason: &str,
    ) {
        let (effects, sent) = (self.effects(), self.sent());
        let refusal = match self.stage(tag, draft, revision, destination) {
            Ok(staged) => panic!("{tag}: staged {destination:?}: {staged:?}"),
            Err(e) => e.to_string(),
        };
        assert!(
            refusal.contains(reason),
            "{tag}: refused for another reason: {refusal}"
        );
        assert_eq!(self.effects(), effects, "{tag}: a refused stage staged");
        assert_eq!(self.sent(), sent, "{tag}: a refused stage sent");
    }

    /// Confirm and schedule of `effect` refused over RPC and over the board,
    /// leaving it waiting and sending nothing.
    fn send_refused(&self, what: &str, effect: &str, digest: &str) {
        let sent = self.sent();
        let due = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        let changed = "social publish binding changed since";
        let confirm = self
            .op_result(
                "app_effect_confirm_publish",
                json!({"effect_id": effect, "digest": digest}),
            )
            .expect_err(&format!("{what}: confirm sent"))
            .to_string();
        assert!(confirm.contains(changed), "{what}: confirm: {confirm}");
        let schedule = self
            .op_result(
                "app_effect_schedule",
                json!({"effect_id": effect, "digest": digest, "due_epoch": due}),
            )
            .expect_err(&format!("{what}: schedule accepted"))
            .to_string();
        assert!(schedule.contains(changed), "{what}: schedule: {schedule}");
        let session = self.board_session();
        for (verb, body) in [
            ("confirm-publish", json!({"digest": digest})),
            ("schedule", json!({"digest": digest, "due_epoch": due})),
        ] {
            let (status, reply, _) = self.http_post(
                &Asserted::Operator,
                &format!("/api/app-effects/{effect}/{verb}"),
                &body,
                Some(&session),
            );
            // The board hides a refusal's text (409 is a daemon rejection);
            // the same board confirm of the other destination's effect
            // succeeds below, so this 409 is the binding refusal.
            assert_eq!(status, 409, "{what}: board {verb}: {reply}");
        }
        assert_eq!(self.effect(effect)["state"], "waiting", "{what}");
        assert_eq!(
            self.sent(),
            sent,
            "{what}: a refused effect reached the door"
        );
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

#[test]
fn cad1939_refusal_each_destination_publishes_only_through_its_own_binding() {
    let fx = Fx::start();
    let (install, ctx_a) = (fx.install.clone(), fx.context.clone());
    let ctx_b = fx.context_create(&install, "B");
    let other = fx.other_install();

    // Context A publishes to IG; FB is bound only in context B of the same
    // install; IG2 only in another install.
    let ig = fx.use_destination(
        &install,
        Some(&ctx_a),
        "instagram",
        IG,
        json!({"request_id": "use-a-ig"}),
    );
    fx.use_destination(
        &install,
        Some(&ctx_b),
        "facebook",
        FB,
        json!({"request_id": "use-b-fb"}),
    );
    fx.use_destination(
        &other,
        None,
        "instagram",
        IG2,
        json!({"request_id": "use-o-ig2"}),
    );
    let (draft, revision) = fx.image_draft();
    let unbound = "no live publication binding of this context publishes to that destination";

    // (a) A destination with no live binding in this context.
    fx.stage_refused("a-forged", &draft, revision, Some(FORGED), unbound);
    // (b) Bound only in another context of this install; only in another install.
    fx.stage_refused("b-context", &draft, revision, Some(FB), unbound);
    fx.stage_refused("b-install", &draft, revision, Some(IG2), unbound);
    // Not vacuous: the same mount stages to its own destination.
    let (to_ig, ig_digest) = fx.stage("a-own", &draft, revision, Some(IG)).unwrap();

    // (c) Two live destinations in this context: a stage must name one.
    let fb = fx.use_destination(
        &install,
        Some(&ctx_a),
        "facebook",
        FB,
        json!({"request_id": "use-a-fb"}),
    );
    fx.stage_refused(
        "c-omitted",
        &draft,
        revision,
        None,
        "this context publishes to several destinations; name the destination_id",
    );

    // (d) Changing the Page's binding stops only the Page's effect.
    let (to_fb, fb_digest) = fx.stage("d-fb", &draft, revision, Some(FB)).unwrap();
    let fb_changed = fx.use_destination(
        &install,
        Some(&ctx_a),
        "facebook",
        FB,
        json!({"expected_revision": fb["revision"], "timezone": "Asia/Tokyo"}),
    );
    assert_ne!(fb_changed["revision"], fb["revision"], "{fb_changed}");
    fx.send_refused("changed FB binding", &to_fb, &fb_digest);
    // Over the board relay: the same route that refused the Page's effect.
    let (status, reply, _) = fx.http_post(
        &Asserted::Operator,
        &format!("/api/app-effects/{to_ig}/confirm-publish"),
        &json!({"digest": ig_digest}),
        Some(&fx.board_session()),
    );
    assert_eq!(status, 200, "the IG effect must still send: {reply}");
    assert_eq!(fx.effect(&to_ig)["state"], "posted", "{reply}");
    assert_eq!(fx.sent(), vec![json!(IG)]);

    // (d) Revoking the account's binding (the plain-scope row) stops only the
    // account's effect; the Page's effect at its new binding still sends.
    let (to_ig, ig_digest) = fx.stage("d-ig2", &draft, revision, Some(IG)).unwrap();
    let (to_fb, fb_digest) = fx.stage("d-fb2", &draft, revision, Some(FB)).unwrap();
    fx.op(
        "app_binding_revoke",
        json!({"install_id": install, "binding_id": ig["id"],
            "expected_revision": ig["revision"]}),
    );
    fx.send_refused("revoked IG binding", &to_ig, &ig_digest);
    assert_eq!(fx.confirm_ok(&to_fb, &fb_digest), "posted");
    assert_eq!(fx.sent(), vec![json!(IG), json!(FB)]);

    // (e) Only the operator chooses a destination: an agent or an unproven
    // caller can neither add one nor update one, over RPC or the board relay.
    let path = format!("/api/app-installations/{install}/publishing/use");
    let before = (fx.bindings(&install), fx.bindings(&other));
    for who in [Asserted::Agent("cc13-pw".into()), Asserted::Unproven] {
        for (n, body) in [
            json!({"destination_id": IG2, "toolkit": "instagram", "context_id": ctx_a,
                "request_id": "e-add"}),
            json!({"destination_id": FB, "toolkit": "facebook", "context_id": ctx_a,
                "expected_revision": fb_changed["revision"], "timezone": "Europe/Paris"}),
        ]
        .into_iter()
        .enumerate()
        {
            let mut rpc = body.clone();
            rpc["install_id"] = json!(install);
            let refusal = fx
                .rpc(who.clone(), "app_binding_use_destination", rpc)
                .expect_err(&format!("{who:?} use #{n} over RPC"))
                .to_string();
            assert!(
                refusal.contains("operator"),
                "{who:?} use #{n}: refused for another reason: {refusal}"
            );
            for carried in [None, Some(fx.board_session())] {
                let (status, reply, _) = fx.http_post(&who, &path, &body, carried.as_ref());
                assert_eq!(
                    status,
                    403,
                    "{who:?} use #{n} over the board (session: {}): {reply}",
                    carried.is_some()
                );
            }
        }
    }
    assert_eq!(
        (fx.bindings(&install), fx.bindings(&other)),
        before,
        "a refused caller changed a binding"
    );

    // (f) Not in the company's live list, even for the operator.
    for (n, (toolkit, destination, reason)) in [
        ("instagram", FORGED, "grant_binding_mismatch"),
        ("facebook", FORGED, "grant_binding_mismatch"),
        // A listed id under the wrong toolkit is not a listed destination.
        ("instagram", FB, "grant_binding_mismatch"),
        ("facebook", IG2, "grant_binding_mismatch"),
        ("tiktok", IG2, "toolkit must be instagram or facebook"),
    ]
    .into_iter()
    .enumerate()
    {
        let body = json!({"destination_id": destination, "toolkit": toolkit,
            "context_id": ctx_a, "request_id": format!("f-{n}")});
        let mut rpc = body.clone();
        rpc["install_id"] = json!(install);
        let refusal = fx
            .op_result("app_binding_use_destination", rpc)
            .expect_err(&format!("operator used {toolkit}:{destination}"))
            .to_string();
        assert!(
            refusal.contains(reason),
            "operator used {toolkit}:{destination}: refused for another reason: {refusal}"
        );
        let (status, reply, _) =
            fx.http_post(&Asserted::Operator, &path, &body, Some(&fx.board_session()));
        assert_eq!(
            status, 409,
            "operator used {toolkit}:{destination} over HTTP: {reply}"
        );
    }
    assert_eq!(
        (fx.bindings(&install), fx.bindings(&other)),
        before,
        "a refused destination changed a binding"
    );
    assert_eq!(fx.sent(), vec![json!(IG), json!(FB)]);
}

impl Fx {
    /// Confirm `effect` as the operator; its resulting state.
    fn confirm_ok(&self, effect: &str, digest: &str) -> String {
        let sent = self
            .op_result(
                "app_effect_confirm_publish",
                json!({"effect_id": effect, "digest": digest}),
            )
            .unwrap_or_else(|e| panic!("confirm {effect}: {e}"));
        sent["effect"]["state"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
}
