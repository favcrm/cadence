//! Independent CAD-1482 bad-case guard. Authored from the ticket and
//! the implementation contract before the inspect guard exists.
//! Implementers must not edit this file or weaken its assertions.
//!
//! One retained Library image is produced through the existing host
//! create/approve/dispatch pipeline and the loopback AgenticOS media
//! door. A pi-managed agent then holds a real assigned turn.
//! `app_media_inspect` for that asset while it is not on the turn's
//! durable message must refuse with an ordinary JSON-RPC error, no
//! image octets, and no further provider submit or artifact GET.
//! The same turn, after the operator message durably pins that exact
//! asset (`media_refs`: asset id, revision, original sha256), must
//! succeed on the same method: closed result header plus the exact
//! original octets. A forged turn token and `app_media_original` from
//! the same agent stay refused. `Unknown method` is not a refusal pass.
//!
//! HTTP `POST /api/app-media/inspect` is exercised only when that route
//! is actually dispatched. An unlisted write is `OperatorOnly` before
//! route lookup, so an agent `403` / `operator_only` is safe absence,
//! not an exposed inspector. Presence is an authenticated operator
//! probe: `404` with the write-router's `no such write route` means
//! the path was never handled. Any other operator result means a
//! handler ran, and the agent peer must then refuse the unattached
//! asset and return the attached original. Neither body may carry the
//! image unless that positive attached case is the one being proved,
//! and neither may call the provider again.

#![cfg(all(test, feature = "test-seam"))]

use super::{ServeOptions, Shared};
use crate::adapter::Identity;
use crate::platform::{agenticos_external, Custody, Key};
use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ACCOUNT: &str = "ws_11111111-1111-4111-8111-111111111111";
const AGENT: &str = "image-worker";
const GENERATION: &str = "1482aabbccddeeff1482aabbccddeeff";
const JOB_ID: &str = "med_cad1482inspect";
const PLAIN_MESSAGE: &str = "cad1482-unattached-turn";
const PINNED_MESSAGE: &str = "cad1482-pinned-turn";
const INSPECT: &str = "app_media_inspect";
const HTTP_INSPECT: &str = "/api/app-media/inspect";

const APP_MD: &str = r#"---
app: designer-fixture
title: Designer fixture
version: '1.0.0'
summary: One host-execution image-generation workflow for CAD-1482.
needs:
  connections: []
  capabilities:
    image:
      schema: 1
      capability: media.generate
      version: 1
      action: generate_image
      resource_kind: connection_account
      effect: draft
---

# Designer fixture

One declared host-execution image run.
"#;

const WORKFLOW_MD: &str = r#"---
title: "Create image: {{subject}}"
goal: "Generate one reviewed image draft from frozen facts"
label: Create image
capability_slots: [image]
execution: host
inputs:
  subject: { ask: "Image subject", example: "Customer follow-up" }
  source: { ask: "Source facts", example: "Acme CRM helps teams track customers" }
  image_prompt: { ask: "Image guidance", example: "Use a calm editorial palette" }
---

## Generate image: {{subject}}
size: S
action: local.capability.call

The host executes this step itself: it calls the frozen `image` binding
with an empty input object and retains the actual provider response.

### Acceptance
- [ ] the provider action used only the frozen `image` binding
"#;

fn square_png() -> Vec<u8> {
    use image::ImageEncoder as _;
    let side = 64u32;
    let mut pixels = vec![0u8; (side * side * 3) as usize];
    for (i, px) in pixels.chunks_mut(3).enumerate() {
        px[0] = (i % 251) as u8;
        px[1] = 0x3c;
        px[2] = 0x91;
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&pixels, side, side, image::ExtendedColorType::Rgb8)
        .unwrap();
    png
}

fn media_screen(app_dir: &std::path::Path) {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = manifest_dir.join("tests/fixtures/apps/ig-tools-fixture/screens/feed");
    let dst = app_dir.join("screens/media");
    std::fs::create_dir_all(&dst).unwrap();
    for entry in std::fs::read_dir(&src).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
    }
    let screens = dst.join("screens.json");
    let mut decl: Value =
        serde_json::from_str(&std::fs::read_to_string(&screens).unwrap()).unwrap();
    decl["app"] = json!("designer-fixture");
    decl["host_contract"] = json!("screen-media.v1");
    decl["may"] = json!(["tools.invoke", "media.read"]);
    std::fs::write(&screens, serde_json::to_string_pretty(&decl).unwrap()).unwrap();
}

#[derive(Debug)]
struct DoorRecord {
    method: String,
    url: String,
}

fn media_door(
    original: Vec<u8>,
    stop: Arc<AtomicBool>,
) -> (
    String,
    Arc<Mutex<Vec<DoorRecord>>>,
    std::thread::JoinHandle<()>,
) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    let seen = Arc::new(Mutex::new(Vec::<DoorRecord>::new()));
    let seen_worker = seen.clone();
    let digest = format!("{:x}", Sha256::digest(&original));
    let artifact = json!({"ref": format!("{JOB_ID}.{digest}"), "digest": digest,
        "bytes": original.len(), "mime": "image/png"});
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if stop.load(Ordering::SeqCst) || Instant::now() > deadline {
                break;
            }
            let request = match server.recv_timeout(Duration::from_millis(200)) {
                Ok(Some(request)) => request,
                Ok(None) => continue,
                Err(_) => break,
            };
            let (method, url) = (request.method().to_string(), request.url().to_string());
            let reply = match (method.as_str(), url.as_str()) {
                ("GET", "/v1/runtime/media/price/image") => tiny_http::Response::from_string(
                    json!({"ok":true,"data":{"kind":"image","model":"openai/gpt-image-2.5",
                            "price":{"slug":"generate_image","chargeMinor":31_500,
                            "currency":"USD","version":"2026-09-29T00:00:00.000Z"}}})
                    .to_string(),
                )
                .with_status_code(200),
                ("POST", "/v1/runtime/media/image") => tiny_http::Response::from_string(
                    json!({"ok":true,"data":{"job":{
                            "id":JOB_ID,"kind":"image","status":"succeeded",
                            "model":"openai/gpt-image-2.5","provider":"upstream-fixture",
                            "providerTaskId":null,
                            "artifacts":[artifact],
                            "artifactError":null,"usage":{"providerCredits":null},
                            "price":{"slug":"generate_image","chargeMinor":31_500,
                                "currency":"USD","version":"2026-09-29T00:00:00.000Z"},
                            "repeated":false,"cached":false,"stale":false}}})
                    .to_string(),
                )
                .with_status_code(201),
                ("GET", u) if u.starts_with("/v1/runtime/media/jobs/") => {
                    tiny_http::Response::from_string(
                        json!({"ok":false,"error":{"code":"not_found"}}).to_string(),
                    )
                    .with_status_code(404)
                }
                ("GET", u) if u.starts_with("/v1/runtime/media/artifacts/") => {
                    tiny_http::Response::from_data(original.clone()).with_header(
                        tiny_http::Header::from_bytes("Content-Type", "image/png").unwrap(),
                    )
                }
                (m, u) => panic!("unexpected provider request {m} {u}"),
            };
            seen_worker.lock().unwrap().push(DoorRecord { method, url });
            request.respond(reply).unwrap();
        }
    });
    (base, seen, worker)
}

struct Fx {
    _dir: tempfile::TempDir,
    state: std::path::PathBuf,
    shared: Arc<Shared>,
    asset_id: String,
    revision: i64,
    sha256: String,
    original: Vec<u8>,
    seen: Arc<Mutex<Vec<DoorRecord>>>,
    door: Option<std::thread::JoinHandle<()>>,
    server: Option<std::thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

fn rpc(fx: &Fx, who: Asserted, method: &str, params: Value) -> crate::error::Result<Value> {
    scoped(who, || crate::client::rpc(&fx.state, method, params))
}

fn socket_call(
    fx: &Fx,
    who: Asserted,
    method: &str,
    params: Value,
) -> std::io::Result<(BufReader<UnixStream>, String)> {
    let mut request = crate::proto::request(method, params);
    if let Some(frame) = scoped(who, || crate::test_seam::caller_frame(&fx.state)).unwrap() {
        request[crate::test_seam::FRAME_FIELD] = frame;
    }
    let mut stream = UnixStream::connect(fx.state.join("cadence.sock"))?;
    stream.set_read_timeout(Some(Duration::from_secs(90)))?;
    stream.write_all(format!("{request}\n").as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok((reader, line))
}

impl Fx {
    fn boot() -> Self {
        let original = square_png();
        let dir = tempfile::Builder::new()
            .prefix("c1482inspect")
            .tempdir()
            .unwrap();
        let (state, pm) = (dir.path().join("s"), dir.path().join("pm"));
        std::fs::create_dir_all(&state).unwrap();
        crate::issue::Pm::init(&pm).unwrap();
        crate::board_identity::write_config(
            &state,
            &crate::board_identity::Config {
                host: "cad1482.example.test".to_string(),
                issuer: "https://platform.example.test".to_string(),
                company: "cad1482-test-company".to_string(),
            },
        )
        .unwrap();
        let cwd = state.to_str().unwrap().to_string();
        crate::store::Store::open(&state.join("cadence.sqlite3"))
            .unwrap()
            .register_agent(&crate::store::NewAgent {
                alias: "worker",
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (base, seen, door) = media_door(original.clone(), stop.clone());
        let mut opts = ServeOptions {
            test_seam: true,
            stop: Some(stop.clone()),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        opts.provider_env.set("CADENCE_TEST_MEDIA_POLL_MS", "20");
        opts.provider_env
            .set("CADENCE_TEST_MEDIA_DEADLINE_MS", "8000");
        opts.platforms.insert(
            agenticos_external::PLATFORM.into(),
            Arc::new(
                agenticos_external::AgenticosExternalAdapter::with_deployment_pin(
                    &base,
                    Some(agenticos_external::MANIFEST_PIN),
                )
                .unwrap(),
            ),
        );
        let mut shared = Shared::new(&state, &opts).unwrap();
        Arc::get_mut(&mut shared).unwrap().platform_custody =
            Custody::file(dir.path().join("fixture-custody"));
        let token = b"synthetic-cad1482-inspect-token";
        let key = Key {
            platform: agenticos_external::PLATFORM,
            account: ACCOUNT,
        };
        let custody = shared.platform_custody.put(&key, token).unwrap();
        shared
            .store
            .platform_enroll(
                &crate::store::CredentialRecord {
                    connection_id: "gen1482".into(),
                    credential_revision: 1,
                    platform: agenticos_external::PLATFORM.into(),
                    account: ACCOUNT.into(),
                    scopes: vec!["provider.draft".into()],
                    fingerprint: crate::secret::fingerprint(token),
                    custody: custody.into(),
                    exchange: "token".into(),
                    enrolled_at: 1.0,
                    by: "operator".into(),
                },
                false,
                None,
            )
            .unwrap();
        shared
            .store
            .register_agent(&crate::store::NewAgent {
                alias: AGENT,
                provider: "pi",
                endpoint_kind: "managed",
                role: "worker",
                cwd: &cwd,
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
                AGENT,
                &Identity {
                    thread_id: "cad1482-thread".into(),
                    session_id: "cad1482-session".into(),
                    model: None,
                    effort: None,
                    pid: std::process::id(),
                    endpoint: None,
                    generation: Some(GENERATION.into()),
                    attach: None,
                },
                None,
            )
            .unwrap();
        let listener = std::os::unix::net::UnixListener::bind(state.join("cadence.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let shared_server = shared.clone();
        let stop_server = stop.clone();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            loop {
                if stop_server.load(Ordering::SeqCst) {
                    break;
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        let shared = shared_server.clone();
                        requests.push(std::thread::spawn(move || {
                            super::serve::handle_conn(shared, stream);
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
            for request in requests {
                request.join().expect("fixture RPC thread");
            }
        });
        let mut fx = Self {
            state,
            asset_id: String::new(),
            revision: 0,
            sha256: format!("sha256:{:x}", Sha256::digest(&original)),
            original,
            seen,
            door: Some(door),
            server: Some(server),
            stop,
            shared,
            _dir: dir,
        };
        fx.install_and_produce();
        fx
    }

    fn op(&self, method: &str, params: Value) -> crate::error::Result<Value> {
        rpc(self, Asserted::Operator, method, params)
    }

    fn install_and_produce(&mut self) {
        let app = self._dir.path().join("app");
        std::fs::create_dir_all(app.join("workflows")).unwrap();
        std::fs::write(app.join("app.md"), APP_MD).unwrap();
        std::fs::write(app.join("workflows/create-image.md"), WORKFLOW_MD).unwrap();
        media_screen(&app);
        let installed = self
            .op(
                "app_workspace_install",
                json!({"source": app.to_str().unwrap()}),
            )
            .expect("install");
        let install = installed["install_id"].as_str().unwrap().to_string();
        self.op(
            "app_local_install_approve",
            json!({"install_id": install, "digest": installed["digest"]}),
        )
        .expect("approve");
        self.op(
            "app_binding_create",
            json!({"install_id": install, "slot": "image",
                "connection_id": "gen1482", "request_id": "bind-1482"}),
        )
        .expect("bind");
        let ctx = self
            .op(
                "app_context_create",
                json!({"install_id": install, "label": "media",
                    "input_defaults": {}, "request_id": "ctx-1482"}),
            )
            .expect("context");
        let _context = ctx["context"]["id"]
            .as_str()
            .or_else(|| ctx["id"].as_str())
            .unwrap();
        let (token, key) = self.session();
        let created = self
            .op(
                "app_run_create",
                json!({"install_id": install, "workflow": "create-image",
                    "inputs": {"subject": "Harbour mark",
                        "source": "A red lighthouse marks the harbour",
                        "image_prompt": "Use an editorial palette"},
                    "request_id": "run-1482",
                    "token": token, "key": key, "origin": "loopback"}),
            )
            .expect("run create");
        let run = created["id"].as_str().unwrap().to_string();
        self.op(
            "app_run_approve",
            json!({"run_id": run, "digest": created["snapshot_digest"]}),
        )
        .expect("run approve");
        let done = self
            .op("app_run_dispatch", json!({"run_id": run}))
            .expect("dispatch");
        assert_eq!(done["state"], "succeeded", "{done}");
        let (page, _) = self.shared.store.app_media_catalog(20, None).unwrap();
        assert_eq!(page.len(), 1, "one retained library image");
        self.asset_id = page[0].id.clone();
        self.revision = page[0].revision;
        let receipts = self
            .op("app_run_capability_results", json!({"run_id": run}))
            .expect("receipts");
        let stored = receipts["results"][0]["result"]["original_artifact"]["sha256"]
            .as_str()
            .expect("original digest on the receipt")
            .to_string();
        assert_eq!(
            stored, self.sha256,
            "receipt digest is the produced original"
        );
    }

    fn session(&self) -> (String, String) {
        crate::operator_auth::ensure_secret(&self.state).unwrap();
        let secret = crate::operator_auth::read_secret(&self.state).unwrap();
        let nonce = self
            .op(
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
            .unwrap()["nonce"]
            .clone();
        let opened = self
            .op(
                "operator_session_open",
                json!({"nonce": nonce, "origin": "loopback"}),
            )
            .unwrap();
        (
            opened["token"].as_str().unwrap().to_string(),
            opened["key"].as_str().unwrap().to_string(),
        )
    }

    fn provider_posts(&self) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == "POST")
            .count()
    }

    fn artifact_gets(&self) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == "GET" && r.url.starts_with("/v1/runtime/media/artifacts/"))
            .count()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
        if let Some(door) = self.door.take() {
            let _ = door.join();
        }
    }
}

fn assign_turn(fx: &Fx, message: &str, text: &str, media_refs: Option<Value>) -> String {
    let mut params = json!({
        "alias": AGENT,
        "text": text,
        "message": message,
    });
    if let Some(refs) = media_refs {
        params["media_refs"] = refs;
    }
    let sent = fx.op("thread_send", params).expect("operator send");
    assert_eq!(sent["state"], "queued", "{sent}");
    let db = rusqlite::Connection::open(fx.state.join("cadence.sqlite3")).unwrap();
    db.execute(
        "UPDATE messages SET state='submitting', started=1.0 WHERE id=?1",
        [message],
    )
    .unwrap();
    let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(GENERATION);
    fx.shared.store.mark_running(message, &token).unwrap();
    let stored = fx.shared.store.message(message).unwrap().unwrap();
    assert_eq!(stored.state, "running");
    assert_eq!(stored.alias, AGENT);
    assert_eq!(stored.turn_id.as_deref(), Some(token.as_str()));
    token
}

fn finish_turn(fx: &Fx, message: &str) {
    let db = rusqlite::Connection::open(fx.state.join("cadence.sqlite3")).unwrap();
    db.execute(
        "UPDATE messages SET state='done', completed=2.0 WHERE id=?1",
        [message],
    )
    .unwrap();
}

fn inspect_params(asset_id: &str, message: &str, token: &str) -> Value {
    json!({"asset_id": asset_id, "message": message, "token": token})
}

fn read_reply(reader: BufReader<UnixStream>, line: String) -> (Value, Vec<u8>) {
    let frame: Value = serde_json::from_str(line.trim()).unwrap_or_else(|_| json!({"raw": line}));
    let declared = frame["result"]["bytes"].as_u64().unwrap_or(0) as usize;
    if frame.get("error").is_some() || declared == 0 {
        let mut extra = Vec::new();
        let _ = reader.take(64).read_to_end(&mut extra);
        return (frame, extra);
    }
    assert!(
        declared <= 10 * 1024 * 1024,
        "declared inspect body {declared}"
    );
    let mut body = vec![0u8; declared];
    reader
        .take(declared as u64)
        .read_exact(&mut body)
        .expect("inspect body");
    (frame, body)
}

fn assert_no_image(blob: &[u8], what: &str) {
    assert!(
        !blob.windows(8).any(|w| w == b"\x89PNG\r\n\x1a\n"),
        "{what} carried a PNG signature"
    );
}

fn assert_closed_refusal(frame: &Value, body: &[u8], what: &str) {
    let text = frame.to_string();
    assert!(
        !text.contains("Unknown method"),
        "{what} never reached app_media_inspect: {text}"
    );
    assert!(
        frame.get("error").is_some(),
        "{what}: expected an error frame: {frame}"
    );
    assert!(body.is_empty(), "{what}: refusal carried octets");
    assert_no_image(text.as_bytes(), what);
    assert!(
        !text.contains(ACCOUNT),
        "{what} leaked the provider account"
    );
    assert!(
        !text.contains("fixture-custody"),
        "{what} leaked a custody path"
    );
    assert!(
        !text.contains("synthetic-cad1482"),
        "{what} leaked the synthetic credential"
    );
}

fn board(fx: &Fx) -> (u16, std::thread::JoinHandle<()>) {
    let state = fx.state.clone();
    let pm = state.parent().unwrap().join("pm");
    let stop = fx.stop.clone();
    let first = (std::process::id() % 80) as u16;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut offset = 0u16;
    loop {
        assert!(Instant::now() < deadline, "board startup deadline");
        let port = 3110 + (first + offset) % 80;
        let (startup, ready) = std::sync::mpsc::channel();
        let opts = crate::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            startup: Some(startup),
            test_seam: true,
            ..Default::default()
        };
        let (s, p) = (state.clone(), pm.clone());
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&s, &p, &opts)));
        match ready.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Ok(())) => return (port, thread),
            Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                assert!(thread.join().is_ok());
                offset += 1;
                assert!(offset < 80, "all fixture board candidates busy");
            }
            other => {
                let _ = thread.join();
                panic!("fixture board startup failed on port {port}: {other:?}");
            }
        }
    }
}

fn http_inspect(
    port: u16,
    seam: &str,
    who: &str,
    session: Option<(&str, &str)>,
    body: Value,
) -> (u16, Vec<u8>) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let host = format!("cadence-{port}.localhost:{port}");
    let mut req = agent
        .post(format!("http://127.0.0.1:{port}{HTTP_INSPECT}"))
        .header("Host", &host)
        .header("X-Cadence-Board", "1")
        .header("Origin", format!("http://{host}"))
        .header(crate::test_seam::AS_HEADER, who)
        .header(crate::test_seam::TOKEN_HEADER, seam)
        .header("Content-Type", "application/json");
    if let Some((token, key)) = session {
        req = req
            .header("Cookie", format!("cadence_operator_{port}={token}"))
            .header("X-Cadence-Session", key);
    }
    let mut response = req.send(body.to_string()).unwrap();
    let status = response.status().as_u16();
    let mut buf = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(12 * 1024 * 1024)
        .read_to_end(&mut buf)
        .unwrap();
    (status, buf)
}

/// The write router answers an admitted but unlisted POST with this
/// exact fallback (`write_path::write_route`). A handler that exists
/// does not use that sentence for a missing asset.
fn http_inspect_unrouted(status: u16, body: &[u8]) -> bool {
    status == 404
        && body
            .windows(b"no such write route".len())
            .any(|w| w == b"no such write route")
}

/// Valid assigned turn, retained asset not pinned on that message:
/// native inspect refuses before image bytes or another provider call.
/// The later pinned message is the positive control on the same guard.
/// Operator original download stays closed to this agent.
#[test]
fn cad1482_unattached_library_image_inspect_refuses_without_bytes_or_provider_submit() {
    let fx = Fx::boot();
    let posts = fx.provider_posts();
    let gets = fx.artifact_gets();
    assert!(
        posts >= 1 && gets >= 1,
        "produce must have used the isolated provider"
    );

    let plain = assign_turn(
        &fx,
        PLAIN_MESSAGE,
        "inspect without the library image",
        None,
    );
    let (reader, line) = socket_call(
        &fx,
        Asserted::Agent(AGENT.into()),
        INSPECT,
        inspect_params(&fx.asset_id, PLAIN_MESSAGE, &plain),
    )
    .expect("inspect reaches the daemon");
    let (frame, body) = read_reply(reader, line);
    assert_closed_refusal(&frame, &body, "unattached inspect");
    assert_eq!(
        fx.provider_posts(),
        posts,
        "unattached inspect submitted to the provider"
    );
    assert_eq!(
        fx.artifact_gets(),
        gets,
        "unattached inspect fetched a provider artifact"
    );

    let (reader, line) = socket_call(
        &fx,
        Asserted::Agent(AGENT.into()),
        "app_media_original",
        json!({"asset_id": fx.asset_id, "message": PLAIN_MESSAGE, "token": plain}),
    )
    .expect("original request reaches the daemon");
    let (frame, body) = read_reply(reader, line);
    assert!(
        frame.get("error").is_some(),
        "agent original must stay refused: {frame}"
    );
    assert!(body.is_empty(), "agent original refusal carried octets");
    assert_no_image(&frame.to_string().into_bytes(), "agent original");
    assert_eq!(
        fx.artifact_gets(),
        gets,
        "agent original fetched a provider artifact"
    );

    let forged = socket_call(
        &fx,
        Asserted::Agent(AGENT.into()),
        INSPECT,
        inspect_params(&fx.asset_id, PLAIN_MESSAGE, "pi-forged-turn"),
    )
    .expect("forged inspect reaches the daemon");
    let (frame, body) = read_reply(forged.0, forged.1);
    assert_closed_refusal(&frame, &body, "forged turn token");

    finish_turn(&fx, PLAIN_MESSAGE);
    let pinned_refs = json!([{
        "asset_id": fx.asset_id,
        "revision": fx.revision,
        "sha256": fx.sha256,
    }]);
    let pinned = assign_turn(
        &fx,
        PINNED_MESSAGE,
        "inspect the attached library image",
        Some(pinned_refs),
    );
    let (reader, line) = socket_call(
        &fx,
        Asserted::Agent(AGENT.into()),
        INSPECT,
        inspect_params(&fx.asset_id, PINNED_MESSAGE, &pinned),
    )
    .expect("pinned inspect reaches the daemon");
    let (frame, body) = read_reply(reader, line);
    let result = &frame["result"];
    assert!(
        frame.get("error").is_none() && result.is_object(),
        "attached image must pass the same inspect guard: {frame}"
    );
    assert_eq!(result["asset_id"], fx.asset_id, "{frame}");
    assert_eq!(result["revision"], fx.revision, "{frame}");
    assert_eq!(result["sha256"], fx.sha256, "{frame}");
    assert_eq!(result["media_type"], "image/png", "{frame}");
    assert_eq!(result["bytes"], fx.original.len() as u64, "{frame}");
    assert_eq!(
        body, fx.original,
        "inspect delivers the retained original octets"
    );
    let rendered = frame.to_string();
    assert!(
        !rendered.contains(ACCOUNT),
        "inspect leaked the provider account"
    );
    assert!(
        !rendered.contains("http://"),
        "inspect leaked a provider URL"
    );
    assert!(
        !rendered.contains("fixture-custody"),
        "inspect leaked custody"
    );
    assert!(
        !rendered.contains("synthetic-cad1482"),
        "inspect leaked the credential"
    );
    assert_eq!(
        fx.provider_posts(),
        posts,
        "attached inspect submitted to the provider"
    );
    assert_eq!(
        fx.artifact_gets(),
        gets,
        "attached inspect fetched a provider artifact"
    );

    let (port, board_thread) = board(&fx);
    let seam = Seam::token_at(&fx.state).unwrap();
    let (op_token, op_key) = fx.session();
    let (op_status, op_body) = http_inspect(
        port,
        &seam,
        "operator",
        Some((&op_token, &op_key)),
        json!({}),
    );
    assert_no_image(&op_body, "operator inspect presence probe");
    if http_inspect_unrouted(op_status, &op_body) {
        let (status, http_body) = http_inspect(
            port,
            &seam,
            &format!("agent:{AGENT}"),
            None,
            inspect_params(&fx.asset_id, PLAIN_MESSAGE, &plain),
        );
        assert!(
            !(200..300).contains(&status),
            "unrouted HTTP inspect returned success: {status} {http_body:?}"
        );
        assert_no_image(&http_body, "unrouted HTTP inspect");
    } else {
        assert!(
            op_status != 403,
            "operator probe never passed admit; 403 is not an exposed inspect route: {op_status} {}",
            String::from_utf8_lossy(&op_body)
        );
        let (status, http_body) = http_inspect(
            port,
            &seam,
            &format!("agent:{AGENT}"),
            None,
            inspect_params(&fx.asset_id, PLAIN_MESSAGE, &plain),
        );
        assert!(
            !(200..300).contains(&status),
            "HTTP inspect of an unattached asset succeeded: {status} {http_body:?}"
        );
        assert_no_image(&http_body, "HTTP unattached inspect");
        let (ok, http_body) = http_inspect(
            port,
            &seam,
            &format!("agent:{AGENT}"),
            None,
            inspect_params(&fx.asset_id, PINNED_MESSAGE, &pinned),
        );
        assert_eq!(
            ok,
            200,
            "HTTP inspect of the pinned image: {}",
            String::from_utf8_lossy(&http_body)
        );
        assert!(
            http_body
                .windows(fx.original.len())
                .any(|w| w == fx.original.as_slice())
                || http_body == fx.original,
            "HTTP inspect must return the retained original"
        );
    }
    drop(board_thread);
    assert_eq!(
        fx.provider_posts(),
        posts,
        "HTTP inspect submitted to the provider"
    );
    assert_eq!(
        fx.artifact_gets(),
        gets,
        "HTTP inspect fetched a provider artifact"
    );
}
