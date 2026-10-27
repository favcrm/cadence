//! Independent CAD-1485 repair acceptance. NOT a production UI module.
//! Run as the integration target in the separately handed-off scratch manifest:
//! cadence_agent is a NORMAL library dependency (NOT cfg(test)), deliberately
//! retaining production issuer transport defaults. No test TLS/resolver override,
//! asserted caller, cookie-row insertion, model call or alternate guard exists.
//! Startup/provisioning follows the active team_assistants_http_refusal Rig and
//! the existing public-library browser driver. Only an authorized QA executor
//! may execute this target. Synthetic signed issuer responses are NOT real AOS
//! custody/provider/product/browser evidence. Implementers must not edit it.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use cadence_agent as native;
use native::{
    adapter::{Identity, ProviderEnv},
    daemon::{self, ServeOptions, Shared},
    issue::Pm,
    store::NewAgent,
    test_seam::Seam,
    ui::{self, PublicBoard, ServeOpts},
};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tiny_http::{Header, Response, Server, StatusCode};

const ROOT: &str = "/tmp/c1485-r2q-99da1/native";
const ORG: &str = "c1485-r2-native-org";
const KID: &str = "c1485-r2-native-key";
const VERSION: &str = "hosted-cadence-board-assistant.v1";
const OTHER_ORIGIN: &str = "http://other.board.localhost";

fn preflight() {
    let root = Path::new(ROOT);
    assert_eq!(
        root.canonicalize().unwrap(),
        root,
        "owned root is a symlink"
    );
    assert!(root.join("OWNER.md").is_file());
    for name in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "TMPDIR",
        "CARGO_TARGET_DIR",
    ] {
        let path = PathBuf::from(std::env::var_os(name).expect("isolated path missing"));
        let real = path.canonicalize().expect("isolated path missing");
        assert!(
            real.starts_with(root) && real != root,
            "path escapes owned root: {name}"
        );
    }
    assert!(std::env::var_os("CADENCE_SUITE_LOCK").is_some());
    assert_eq!(
        std::env::var("HOSTED_CADENCE_ASSISTANT_ENABLED").as_deref(),
        Ok("true")
    );
    // Refuse ambient configuration; never clear identity or borrow a session.
    for (name, value) in std::env::vars_os() {
        let name = name.to_string_lossy();
        assert!(
            !(name.starts_with("GIT_")
                || name.starts_with("CADENCE_TEST_")
                || name.starts_with("CADENCE_AGENTICOS_")
                || name.starts_with("AGENTICOS_BOARD_")
                || name.starts_with("CADENCE_DEVICE_LOGIN_")
                || name.starts_with("CADENCE_PUBLISH_")
                || name.starts_with("CADENCE_SMTP_")
                || name == "CADENCE_PM_DIR"),
            "ambient integration/Git/assertion configuration; stop for parent inspection"
        );
        if [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ]
        .contains(&name.as_ref())
        {
            assert!(
                value.is_empty(),
                "ambient proxy; stop, do not alter environment"
            );
        }
    }
}

fn server() -> Server {
    (3110..=3199)
        .filter(|p| ![3176, 3187].contains(p))
        .find_map(|p| Server::http(("127.0.0.1", p)).ok())
        .expect("no owned free loopback fixture port")
}
fn port(server: &Server) -> u16 {
    match server.server_addr() {
        tiny_http::ListenAddr::IP(a) => a.port(),
        _ => panic!("not a loopback TCP fixture"),
    }
}
fn signed(key: &Ed25519KeyPair, header: Value, claims: Value) -> String {
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    );
    format!(
        "{input}.{}",
        URL_SAFE_NO_PAD.encode(key.sign(input.as_bytes()).as_ref())
    )
}
fn options(pm: &Path, stop: Arc<AtomicBool>) -> ServeOptions {
    let env = ProviderEnv::refusing_providers();
    env.set("CADENCE_PM_DIR", pm.to_string_lossy().into_owned());
    ServeOptions {
        provider_env: env,
        test_seam: true,
        stop: Some(stop),
        lease: Some(native::lease::Hosted::default()),
        report_router: Some(0),
        checkup: Some(0),
        social_publish_driver_off: true,
        ..ServeOptions::default()
    }
}

#[derive(Default)]
struct Issuer {
    sub: String,
    owner: bool,
    redirect: bool,
    authorize: usize,
    revalidate: usize,
}
struct Rig {
    // Keep root/state alive until all owned threads have stopped.
    dir: tempfile::TempDir,
    pm: PathBuf,
    state: PathBuf,
    board: u16,
    host: String,
    issuer: String,
    key: Arc<Ed25519KeyPair>,
    authority: Arc<Mutex<Issuer>>,
    second: Arc<AtomicUsize>,
    second_url: String,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    shared: Option<Arc<Shared>>,
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for handle in self.threads.drain(..) {
            assert!(handle.join().is_ok(), "owned fixture thread failed");
        }
    }
}
impl Rig {
    fn start(separate_audience: bool) -> Self {
        preflight();
        let dir = tempfile::Builder::new().prefix("r2-").tempdir().unwrap();
        let state = dir.path().join("s");
        let pm = dir.path().join("pm");
        Pm::init(&pm).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let first = server();
        let first_port = port(&first);
        let second_server = server();
        let second_port = port(&second_server);
        let issuer = format!("http://localhost:{first_port}");
        // Both possible OLD destinations are ours; never dial an unowned host.
        let host = format!(
            "localhost:{}",
            if separate_audience {
                second_port
            } else {
                first_port
            }
        );
        let second_url = format!("http://127.0.0.1:{second_port}/redirect-counter");
        let key = Arc::new(Ed25519KeyPair::from_seed_unchecked(&[148; 32]).unwrap());
        let authority = Arc::new(Mutex::new(Issuer {
            sub: "alice".into(),
            ..Issuer::default()
        }));
        let second = Arc::new(AtomicUsize::new(0));
        let mut rig = Self {
            dir,
            pm,
            state,
            board: 0,
            host,
            issuer,
            key,
            authority,
            second,
            second_url,
            stop,
            threads: Vec::new(),
            shared: None,
        };
        {
            let (stop, hits) = (rig.stop.clone(), rig.second.clone());
            rig.threads.push(thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    if let Some(request) = second_server
                        .recv_timeout(Duration::from_millis(100))
                        .unwrap()
                    {
                        hits.fetch_add(1, Ordering::SeqCst);
                        request
                            .respond(Response::from_string("{\"counter\":true}").with_header(
                                Header::from_bytes("Content-Type", "application/json").unwrap(),
                            ))
                            .unwrap();
                    }
                }
            }));
        }
        {
            let (stop, authority, key) = (rig.stop.clone(), rig.authority.clone(), rig.key.clone());
            let (issuer, host, target) =
                (rig.issuer.clone(), rig.host.clone(), rig.second_url.clone());
            rig.threads.push(thread::spawn(move || while !stop.load(Ordering::SeqCst) {
                if let Some(mut request) = first.recv_timeout(Duration::from_millis(100)).unwrap() {
                    let path = request.url().to_string();
                    let mut data = Vec::new();
                    request.as_reader().take(65536).read_to_end(&mut data).unwrap();
                    let mut status = 200;
                    let mut location = None;
                    let body = match path.as_str() {
                        "/.well-known/agenticos-board-jwks.json" => json!({"keys":[{"kty":"OKP","crv":"Ed25519","kid":KID,"x":URL_SAFE_NO_PAD.encode(key.public_key().as_ref())}]}),
                        "/__platform/assistant/board-authorize" => {
                            let mut a = authority.lock().unwrap();
                            a.authorize += 1;
                            assert_eq!(request.method().as_str(), "POST");
                            assert!(request.headers().iter().any(|h| h.field.equiv("Authorization") && h.value.as_str().starts_with("BoardSession ")), "missing possession proof (redacted)");
                            assert!(!request.headers().iter().any(|h| h.field.equiv("Cookie")), "browser Cookie forwarded");
                            let input: Value = serde_json::from_slice(&data).unwrap();
                            assert_eq!(input["version"], VERSION);
                            assert!(matches!(input["verb"].as_str(), Some("state" | "history" | "send")));
                            assert_eq!(input.as_object().unwrap().len(), 2);
                            if a.redirect { status = 302; location = Some(target.clone()); json!({"redirect":true}) }
                            else {
                                let now = native::issue::time::now_epoch();
                                let envelope = signed(&key, json!({"alg":"EdDSA","typ":"agenticos-assistant-actor/1","kid":KID}), json!({
                                    "iss":issuer,"aud":format!("http://{host}"),"sub":a.sub,
                                    "actor":format!("users/{}",a.sub),"principal_kind":"user",
                                    "role_at_issue":if a.owner {"owner"} else {"member"},"organization_id":ORG,
                                    "assistant_id":format!("assist_{}",a.sub),"scope":["assist.read","assist.write","assist.jobs"],
                                    "credential_id":format!("cred-{}",a.sub),"iat":now,"exp":now+120,"jti":uuid::Uuid::new_v4().to_string()
                                }));
                                json!({"version":VERSION,"envelope":envelope,"expires_at":now+120})
                            }
                        }
                        "/__platform/assistant/revalidate" => {
                            authority.lock().unwrap().revalidate += 1;
                            // Synthetic provider decision, NOT the native guard under test.
                            let input: Value = serde_json::from_slice(&data).unwrap();
                            assert_eq!(request.method().as_str(), "POST");
                            assert_eq!(input["version"], "hosted-cadence-assistant.v1");
                            assert_eq!(input["organization_id"], ORG);
                            assert!(request.headers().iter().any(|h| h.field.equiv("Authorization") && h.value.as_str().starts_with("Bearer wikienv_")));
                            json!({"allow":true})
                        }
                        _ => { status = 404; json!({"error":"unknown fixture path"}) }
                    };
                    let mut response = Response::from_string(body.to_string()).with_status_code(StatusCode(status))
                        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                    if let Some(location) = location { response.add_header(Header::from_bytes("Location", location).unwrap()); }
                    request.respond(response).unwrap();
                }
            }));
        }
        {
            let state = rig.state.clone();
            let opts = options(&rig.pm, rig.stop.clone());
            rig.threads.push(thread::spawn(move || {
                daemon::serve_with(&state, opts).expect("isolated daemon failed")
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        while !rig.state.join("cadence.sock").exists() || Seam::token_at(&rig.state).is_none() {
            assert!(Instant::now() < deadline, "daemon readiness failed");
            thread::sleep(Duration::from_millis(50));
        }
        let shared = Shared::new(&rig.state, &options(&rig.pm, rig.stop.clone())).unwrap();
        for sub in ["alice", "owner"] {
            shared
                .store
                .assistant_bind(
                    ORG,
                    sub,
                    &format!("assist_{sub}"),
                    &format!("assist-agent-{sub}"),
                    Some("rev0"),
                )
                .unwrap();
        }
        rig.shared = Some(shared);
        for p in (3110..=3199).filter(|p| ![first_port, second_port, 3176, 3187].contains(p)) {
            let (startup, ready) = mpsc::channel();
            let opts = ServeOpts {
                host: "127.0.0.1".into(),
                port: p,
                stop: Some(rig.stop.clone()),
                startup: Some(startup),
                test_seam: true,
                gh: Some("/bin/false".into()),
                allow_hosts: vec![rig.host.clone()],
                allow_origins: vec![OTHER_ORIGIN.into()],
                public: Some(PublicBoard {
                    host: rig.host.clone(),
                    issuer: rig.issuer.clone(),
                    company: ORG.into(),
                    authorize_url: format!("{}/authorize", rig.issuer),
                }),
                ..ServeOpts::default()
            };
            let (state, pm) = (rig.state.clone(), rig.pm.clone());
            let handle = thread::spawn(move || ui::serve(&state, &pm, &opts));
            match ready
                .recv_timeout(Duration::from_secs(30))
                .expect("board readiness disconnected")
            {
                Ok(()) => {
                    rig.board = p;
                    // Retain the top-level owner thread and its result.
                    rig.threads.push(thread::spawn(move || {
                        handle.join().unwrap().expect("board failed");
                    }));
                    break;
                }
                Err(_) => {
                    let _ = handle.join().unwrap();
                }
            }
        }
        assert_ne!(rig.board, 0, "no board port");
        rig
    }
    fn shared(&self) -> &Shared {
        self.shared.as_ref().unwrap()
    }
    fn for_sub(&self, sub: &str) {
        let mut a = self.authority.lock().unwrap();
        a.sub = sub.into();
        a.owner = sub == "owner";
    }
    fn counts(&self) -> (usize, usize) {
        let a = self.authority.lock().unwrap();
        (a.authorize, a.revalidate)
    }
    fn login(&self, sub: &str) -> String {
        let now = native::issue::time::now_epoch();
        let assertion = signed(
            &self.key,
            json!({"alg":"EdDSA","typ":"JWT","kid":KID}),
            json!({
                "iss":self.issuer,"aud":self.host,"sub":sub,"email":format!("{sub}@c1485.example.test"),"name":sub,
                "company":ORG,"role":if sub == "owner" {"owner"} else {"member"},"iat":now,"exp":now+60,"jti":uuid::Uuid::new_v4().to_string()
            }),
        );
        let mut response = post(
            self.board,
            &self.host,
            "/__platform/session",
            None,
            Some(&format!("http://{}", self.host)),
            &json!({"assertion":assertion}),
        );
        assert_eq!(
            response.status().as_u16(),
            200,
            "genuine signed SSO failed (body redacted)"
        );
        let cookie = response
            .headers()
            .get("set-cookie")
            .expect("missing actual native cookie")
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        assert!(cookie.starts_with("__Host-aos-board-session="));
        let body: Value = response.body_mut().read_json().unwrap();
        assert!(body["expires_at"].as_i64().is_some());
        let checked = native::client::rpc(
            &self.state,
            "board_session_check",
            json!({"token":cookie.split_once('=').unwrap().1}),
        )
        .unwrap();
        assert_eq!(checked["valid"], true);
        assert_eq!(checked["session"]["origin"], "public");
        assert_eq!(checked["session"]["user"]["sub"], sub);
        assert_eq!(
            checked["session"]["user"]["role"],
            if sub == "owner" { "operator" } else { "member" }
        );
        cookie
    }
    fn own_control(&self, sub: &str, cookie: &str) {
        self.for_sub(sub);
        let before = self.counts();
        let mut response = post(
            self.board,
            &self.host,
            "/api/member-assistant/state",
            Some(cookie),
            Some(&format!("http://{}", self.host)),
            &json!({}),
        );
        assert_eq!(
            response.status().as_u16(),
            200,
            "genuine named/owner control failed (redacted)"
        );
        let body: Value = response.body_mut().read_json().unwrap();
        assert_eq!(body["assistant"]["alias"], format!("assist-agent-{sub}"));
        let after = self.counts();
        assert_eq!(after.0, before.0 + 1);
        assert!(
            after.1 > before.1,
            "control did not traverse live daemon revalidation"
        );
        for agent in self.shared().store.agents().unwrap() {
            self.shared()
                .store
                .set_enabled(&agent.alias, false)
                .unwrap();
            self.shared()
                .store
                .event_public(&agent.alias, "stop_requested", json!({}))
                .unwrap();
        }
    }
}

fn post(
    board: u16,
    host: &str,
    path: &str,
    cookie: Option<&str>,
    origin: Option<&str>,
    body: &Value,
) -> ureq::http::Response<ureq::Body> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .build(),
    );
    let mut request = agent
        .post(format!("http://127.0.0.1:{board}{path}"))
        .header("Host", host)
        .header("Content-Type", "application/json")
        .header("X-Cadence-Board", "1")
        .header("Sec-Fetch-Site", "same-origin");
    if let Some(cookie) = cookie {
        request = request.header("Cookie", cookie);
    }
    if let Some(origin) = origin {
        request = request.header("Origin", origin);
    }
    request
        .send_json(body)
        .expect("board HTTP transport failed (redacted)")
}
fn effects(rig: &Rig) -> BTreeMap<String, Vec<Vec<rusqlite::types::Value>>> {
    // External acceptance must not need crate-private Store::try_conn visibility.
    // Read ONLY our own live fixture database, with SQLite read-only admission.
    let conn = rusqlite::Connection::open_with_flags(
        rig.state.join("cadence.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    ["messages", "assistants", "assist_requests", "agents"]
        .into_iter()
        .map(|table| {
            let mut query = conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let count = query.column_count();
            let rows = query
                .query_map([], |row| {
                    (0..count)
                        .map(|i| row.get(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (table.into(), rows)
        })
        .collect()
}
fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            if path.is_dir() {
                out.insert(path.strip_prefix(root).unwrap().into(), Vec::new());
                walk(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().into(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}
fn refused(mut response: ureq::http::Response<ureq::Body>, check: &str, cookie: &str) {
    assert_eq!(
        response.status().as_u16(),
        403,
        "not the actual HTTP guard refusal"
    );
    let text = response.body_mut().read_to_string().unwrap();
    assert!(
        !text.contains(cookie.split_once('=').unwrap().1),
        "session disclosure (redacted)"
    );
    let body: Value = serde_json::from_str(&text).expect("not JSON guard response");
    assert_eq!(body["check"], check, "wrong guard, body redacted");
}

#[test]
fn cad1485_origin_missing_and_other_configured_refused_before_issuer() {
    let rig = Rig::start(false);
    let alice = rig.login("alice");
    rig.own_control("alice", &alice);
    let owner = rig.login("owner");
    rig.own_control("owner", &owner);
    rig.for_sub("alice");
    // Explicitly configured above, so write_guard alone admits this origin;
    // it is NOT this request Host's public own-Origin. Only an HTTP header,
    // never a network destination or a fake caller identity.
    let other = OTHER_ORIGIN.to_string();
    assert_ne!(other, format!("http://{}", rig.host));
    for origin in [None, Some(other.as_str())] {
        let before = effects(&rig);
        let files = tree(&rig.pm);
        let counts = rig.counts();
        let response = post(
            rig.board,
            &rig.host,
            "/api/member-assistant/send",
            Some(&alice),
            origin,
            &json!({"text":"r2-origin-refusal-marker"}),
        );
        refused(response, "origin", &alice);
        assert_eq!(
            rig.counts(),
            counts,
            "refusal reached issuer/private revalidation"
        );
        assert_eq!(
            effects(&rig),
            before,
            "refusal changed private/Master state"
        );
        assert_eq!(tree(&rig.pm), files, "refusal changed private/Master files");
    }
    rig.own_control("alice", &alice);
    rig.own_control("owner", &owner);
}

// A real socket-owning child, NOT an asserted test identity. The ignored test
// entry is invoked only by its parent below; stdin carries the ephemeral cookie
// (never argv/env/file/log). Native /proc attribution uses its live registered
// PID plus start time. No model/provider executable is involved.
#[test]
#[ignore]
fn cad1485_agent_peer_child() {
    println!("CAD1485_PEER_READY");
    std::io::stdout().flush().unwrap();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
    let request: Value = serde_json::from_str(&input).expect("missing owner-pipe request");
    let cookie = request["cookie"].as_str().unwrap();
    let response = post(
        request["board"].as_u64().unwrap() as u16,
        request["host"].as_str().unwrap(),
        "/api/member-assistant/send",
        Some(cookie),
        request["origin"].as_str(),
        &json!({"text":"r2-stolen-refusal-marker"}),
    );
    refused(response, "session_from_agent", cookie);
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        let _ = self.0.wait();
    }
}
#[test]
fn cad1485_real_agent_stolen_cookie_refused_before_issuer() {
    let rig = Rig::start(false);
    let cookie = rig.login("alice");
    rig.own_control("alice", &cookie);
    let owner = rig.login("owner");
    rig.own_control("owner", &owner);
    rig.for_sub("alice");
    let mut child = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cad1485_agent_peer_child",
                "--ignored",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut output = BufReader::new(child.0.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert!(
            output.read_line(&mut line).unwrap() > 0,
            "child stopped before socket request"
        );
        if line.contains("CAD1485_PEER_READY") {
            break;
        }
    }
    let store = &rig.shared().store;
    store
        .register_agent(&NewAgent {
            alias: "r2-real-peer",
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: rig.dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    store.set_enabled("r2-real-peer", false).unwrap();
    store
        .set_identity(
            "r2-real-peer",
            &Identity {
                thread_id: "r2-fixture-peer".into(),
                session_id: "r2-fixture-peer".into(),
                model: None,
                effort: None,
                pid: child.0.id(),
                endpoint: None,
                generation: Some("r2-peer-generation".into()),
                attach: None,
            },
        )
        .unwrap();
    store
        .event_public("r2-real-peer", "stop_requested", json!({}))
        .unwrap();
    let registered = store.agent("r2-real-peer").unwrap();
    assert_eq!(registered.pid, Some(child.0.id() as i64));
    assert!(registered.pid_start.is_some());
    let before = effects(&rig);
    let files = tree(&rig.pm);
    let counts = rig.counts();
    let mut pipe = child.0.stdin.take().unwrap();
    writeln!(pipe,"{}",json!({"board":rig.board,"host":rig.host,"origin":format!("http://{}",rig.host),"cookie":cookie})).unwrap();
    drop(pipe);
    // Child emits only fixed harness diagnostics. Never echo raw response/input.
    let mut discarded = String::new();
    output.read_to_string(&mut discarded).unwrap();
    let status = child.0.wait().unwrap();
    assert_eq!(
        rig.counts(),
        counts,
        "stolen-cookie request reached issuer/private revalidation"
    );
    assert_eq!(
        effects(&rig),
        before,
        "stolen-cookie request changed private/Master state"
    );
    assert_eq!(
        tree(&rig.pm),
        files,
        "stolen-cookie request changed private/Master files"
    );
    assert!(
        status.success(),
        "real attributed child did not receive session_from_agent refusal"
    );
    // Revocation is EXPECTED guard bookkeeping, deliberately not an effects
    // snapshot of the auth session store.
    let checked = native::client::rpc(
        &rig.state,
        "board_session_check",
        json!({"token":cookie.split_once('=').unwrap().1}),
    )
    .unwrap();
    assert_eq!(
        checked["valid"], false,
        "stolen native session was not revoked"
    );
    rig.own_control("owner", &owner);
    let renewed = rig.login("alice");
    rig.own_control("alice", &renewed);
}

#[test]
fn cad1485_production_transport_never_follows_authorize_redirect() {
    let rig = Rig::start(false);
    let cookie = rig.login("alice");
    rig.own_control("alice", &cookie);
    // Prove the independent SECOND destination is live before the bad case.
    let agent = ureq::Agent::new_with_config(ureq::Agent::config_builder().proxy(None).build());
    assert_eq!(
        agent.get(&rig.second_url).call().unwrap().status().as_u16(),
        200
    );
    let hits = rig.second.load(Ordering::SeqCst);
    assert_eq!(hits, 1);
    rig.authority.lock().unwrap().redirect = true;
    let counts = rig.counts();
    let before = effects(&rig);
    let files = tree(&rig.pm);
    let mut response = post(
        rig.board,
        &rig.host,
        "/api/member-assistant/send",
        Some(&cookie),
        Some(&format!("http://{}", rig.host)),
        &json!({"text":"r2-no-redirect-marker"}),
    );
    let status = response.status().as_u16();
    let body: Value = response.body_mut().read_json().unwrap();
    assert_eq!(
        rig.counts(),
        (counts.0 + 1, counts.1),
        "redirect path bypassed pinned issuer or reached private revalidation"
    );
    assert_eq!(
        rig.second.load(Ordering::SeqCst),
        hits,
        "PRODUCTION transport followed credential-bearing authorization redirect"
    );
    assert_eq!(status, 503, "issuer redirect must fail closed");
    assert_eq!(body["error"]["code"], "authority_refused");
    assert_eq!(
        effects(&rig),
        before,
        "redirect refusal changed private/Master state"
    );
    assert_eq!(tree(&rig.pm), files);
    rig.authority.lock().unwrap().redirect = false;
    rig.own_control("alice", &cookie);
}

#[test]
fn cad1485_authorize_and_commit_use_pinned_issuer_not_audience() {
    let rig = Rig::start(true);
    let cookie = rig.login("alice");
    // Audience listener returns only a counter, never authority. The real
    // authorize AND daemon live commit revalidation must reach pinned issuer.
    rig.own_control("alice", &cookie);
    assert_eq!(
        rig.second.load(Ordering::SeqCst),
        0,
        "public audience was used as transport destination"
    );
}
