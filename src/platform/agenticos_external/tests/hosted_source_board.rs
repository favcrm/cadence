//! CAD-1096 ticket results at the real boundary: an in-process daemon, the
//! operator's board HTTP quote route and a loopback hosted door that, like
//! AgenticOS since AOS-103, refuses every slug but `read_instagram_posts`.
use super::*;
use crate::test_seam::{scoped, Asserted, Seam};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

type Reply = tiny_http::Response<std::io::Cursor<Vec<u8>>>;

fn refused(code: &str) -> Reply {
    json_response(404, json!({"ok":false,"error":{"code":code}}))
}

/// The door AOS-140 promises: quote and call answer only the generic slug.
fn generic_door(method: &str, url: &str, body: &Value) -> Reply {
    let price = json!({"currency":"USD","scale":6,"amount":"0.002000"});
    match (method, url) {
        ("GET", "/v1/runtime/tools/read_instagram_posts") => json_response(
            200,
            json!({"ok":true,"data":{"slug":"read_instagram_posts","displayName":"Read Instagram posts","effect":"read","chargePrecondition":"max_charge_minor@1","price":price,"unitPrice":null}}),
        ),
        ("POST", CALL_PATH) if body["slug"] == "read_instagram_posts" => json_response(
            200,
            json!({"ok":true,"data":{"slug":"read_instagram_posts","repeated":false,"price":price,"result":{"success":true,"status":"ok","user":{"username":"juicysuite_crm","is_private":false},"items":[{"id":"post-1","code":"AbCd123","created_at":"2026-09-27T00:00:00Z","caption":{"text":"Door caption"}}]}}}),
        ),
        _ => refused("not_allowlisted"),
    }
}

/// Stops the door, daemon and board threads even when an assertion fails.
struct Stop(Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>);

impl Drop for Stop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
        for thread in self.1.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

fn operator(state: &std::path::Path, method: &str, params: Value) -> Value {
    scoped(Asserted::Operator, || {
        crate::client::rpc(state, method, params)
    })
    .unwrap()
}

/// A door answering with `answer`, a daemon whose hosted source points at
/// it, Social Content installed with its source slot bound to the builtin
/// `hosted` account, and a board on 3110-3199. Answers the board's quote
/// `(status, body)` and the frozen source binding.
fn boot_daemon(answer: fn(&str, &str, &Value) -> Reply) -> (Stop, tempfile::TempDir, PathBuf) {
    boot_daemon_pinned(answer, MANIFEST_PIN)
}

fn boot_daemon_pinned(
    answer: fn(&str, &str, &Value) -> Reply,
    pin: &str,
) -> (Stop, tempfile::TempDir, PathBuf) {
    let mut stop = Stop(Arc::new(AtomicBool::new(false)), Vec::new());
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    let halt = stop.0.clone();
    stop.1.push(std::thread::spawn(move || {
        while !halt.load(Ordering::SeqCst) {
            let Ok(Some(mut request)) = server.recv_timeout(Duration::from_millis(100)) else {
                continue;
            };
            let mut text = String::new();
            request.as_reader().read_to_string(&mut text).unwrap();
            let body = serde_json::from_str(&text).unwrap_or(Value::Null);
            let reply = answer(request.method().as_str(), request.url(), &body);
            let _ = request.respond(reply);
        }
    }));
    let root = tempfile::Builder::new().prefix("c1096").tempdir().unwrap();
    let (state, pm) = (root.path().join("s"), root.path().join("pm"));
    crate::issue::Pm::init(&pm).unwrap();
    let mut opts = crate::daemon::ServeOptions {
        test_seam: true,
        stop: Some(stop.0.clone()),
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let mut adapter = hosted_with_pin(pin).unwrap();
    adapter.base = base;
    opts.platforms.insert(PLATFORM.into(), Arc::new(adapter));
    let daemon_state = state.clone();
    stop.1.push(std::thread::spawn(move || {
        crate::daemon::serve_with(&daemon_state, opts).unwrap()
    }));
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
        assert!(std::time::Instant::now() < deadline, "daemon never started");
        std::thread::sleep(Duration::from_millis(50));
    }
    (stop, root, state)
}

fn board_quote(answer: fn(&str, &str, &Value) -> Reply, slot: &str) -> (u16, Value, Value) {
    let (mut stop, root, state) = boot_daemon(answer);
    let pm = root.path().join("pm");
    let source = concat!(env!("CARGO_MANIFEST_DIR"), "/workspace-apps/social-content");
    let install = operator(&state, "app_workspace_install", json!({"source":source}));
    let approve = json!({"install_id":install["install_id"],"digest":install["digest"]});
    operator(&state, "app_local_install_approve", approve);
    let rows = operator(&state, "connection_list", json!({}))["connections"].clone();
    let hosted = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["account"] == "hosted");
    let bind = json!({"install_id":install["install_id"],"slot":"source","connection_id":hosted.unwrap()["id"],"request_id":"bind-1096"});
    let binding = operator(&state, "app_binding_create", bind)["binding"].clone();
    let mut port = 3110 + (std::process::id() % 80) as u16;
    loop {
        let (startup, ready) = std::sync::mpsc::channel();
        let board = crate::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.0.clone()),
            startup: Some(startup),
            test_seam: true,
            ..Default::default()
        };
        let (state, pm) = (state.clone(), pm.clone());
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &board)));
        match ready.recv_timeout(Duration::from_secs(20)).unwrap() {
            Ok(()) => break stop.1.push(thread),
            Err(_) if port < 3199 => port += 1,
            Err(kind) => panic!("board could not bind: {kind:?}"),
        }
        thread.join().unwrap();
    }
    // The operator's session comes from the real login-link exchange.
    let token = Seam::token_at(&state).unwrap();
    let host = format!("cadence-{port}.localhost:{port}");
    crate::operator_auth::ensure_secret(&state).unwrap();
    let secret = crate::operator_auth::read_secret(&state).unwrap();
    let mint = json!({"secret":secret,"origin":"loopback"});
    let nonce = operator(&state, "operator_link_mint", mint)["nonce"].clone();
    let config = ureq::Agent::config_builder().http_status_as_error(false);
    let agent: ureq::Agent = config.build().into();
    let base = format!("http://127.0.0.1:{port}");
    let session = board(agent.post(format!("{base}/api/session")), &host, &token)
        .header("Content-Type", "application/json")
        .send(json!({"nonce":nonce}).to_string())
        .unwrap();
    let set = session.headers()["set-cookie"].to_str().unwrap();
    let cookie = set[..set.find(';').unwrap()].to_owned();
    let key: Value = session.into_body().read_json().unwrap();
    let install = install["install_id"].as_str().unwrap();
    let url = format!("{base}/api/app-installations/{install}/bindings/{slot}/quote");
    let mut response = board(agent.get(&url), &host, &token)
        .header("Cookie", &cookie)
        .header("X-Cadence-Session", key["session_key"].as_str().unwrap())
        .call()
        .unwrap();
    let status = response.status().as_u16();
    let body = response.body_mut().read_json().unwrap();
    (
        status,
        body,
        json!({"install_id":install,"binding":binding}),
    )
}

/// A loopback board request: its own Host and Origin, asserted operator.
fn board<B>(request: ureq::RequestBuilder<B>, host: &str, token: &str) -> ureq::RequestBuilder<B> {
    let request = request.header("Host", host).header("X-Cadence-Board", "1");
    let request = request.header("Origin", format!("http://{host}"));
    let request = request.header(crate::test_seam::AS_HEADER, "operator");
    request.header(crate::test_seam::TOKEN_HEADER, token)
}

/// R1: the Source quote returns a frozen price and the source read under
/// that binding and quote returns the door's posts.
#[test]
fn cad1096_source_quote_and_read_succeed_against_the_generic_tool() {
    let (status, body, frozen) = board_quote(generic_door, "source");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["quote"]["total_price_micros"], 2000, "{body}");
    assert!(body["quote_digest"].as_str().is_some_and(|d| !d.is_empty()));
    // The broker's read under the daemon's frozen binding and that quote.
    let authority = json!({"schema":1,"slot":"source","install_id":frozen["install_id"],
        "binding":frozen["binding"],"inputs":{"profile_handle":"juicysuite_crm"},"quote":body["quote"]});
    let (base, _seen, door) = media_door(|r| generic_door(&r.method, &r.url, &r.body));
    let mut host = hosted_adapter();
    host.base = base;
    let read = host.execute_app_capability(b"", &authority, &json!({}), "app-call-1096");
    assert_eq!(read.unwrap().result["posts"][0]["caption"], "Door caption");
    door.join().unwrap();
}

/// R2: when the door refuses, the operator's board response names the code.
#[test]
fn cad1096_board_quote_names_the_door_refusal_code() {
    let (status, body, _) = board_quote(|_, _, _| refused("not_allowlisted"), "source");
    assert_eq!(status, 409, "{body}");
    let message = "bound capability price discovery refused: not_allowlisted";
    assert_eq!(body["error"], message, "{body}");
}

/// Forbidden harm: any other daemon refusal (here the unbound image slot's
/// "capability binding is absent") never reaches the browser verbatim.
#[test]
fn cad1096_board_keeps_non_price_refusals_generic() {
    let (status, body, _) = board_quote(generic_door, "image");
    assert_eq!(status, 409, "{body}");
    let generic = "app release management refused or unavailable";
    assert_eq!(body["error"], generic, "{body}");
}

static CALLS: std::sync::Mutex<Vec<Value>> = std::sync::Mutex::new(Vec::new());

fn recording_door(method: &str, url: &str, body: &Value) -> Reply {
    if method == "POST" && url == CALL_PATH {
        CALLS.lock().unwrap().push(body.clone());
    }
    generic_door(method, url, body)
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

/// CAD-1294: the hosted Fetch posts screen call, through the real daemon
/// path (session, mounted screen, saved context handle, bound source),
/// reaches the provider door with the saved handle. A handle that is not
/// saved is refused before any provider traffic.
#[test]
fn cad1294_screen_fetch_of_a_saved_handle_reaches_the_provider_door() {
    let (_stop, root, state) = boot_daemon(recording_door);
    // Social Content (declares `list_posts`) plus the tools screen fixture.
    let fixture = root.path().join("app");
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    copy_dir(
        &std::path::Path::new(manifest_dir).join("workspace-apps/social-content"),
        &fixture,
    );
    let screen =
        std::path::Path::new(manifest_dir).join("tests/fixtures/apps/ig-tools-fixture/screens");
    copy_dir(&screen, &fixture.join("screens"));
    let screens = fixture.join("screens/feed/screens.json");
    let text = std::fs::read_to_string(&screens).unwrap();
    std::fs::write(&screens, text.replace("ig-tools-fixture", "social-content")).unwrap();
    let install = operator(
        &state,
        "app_workspace_install",
        json!({"source":fixture.to_str().unwrap()}),
    );
    let (install_id, digest) = (install["install_id"].clone(), install["digest"].clone());
    operator(
        &state,
        "app_local_install_approve",
        json!({"install_id":install_id,"digest":digest}),
    );
    let rows = operator(&state, "connection_list", json!({}))["connections"].clone();
    let hosted = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account"] == "hosted");
    let context = operator(
        &state,
        "app_context_create",
        json!({"install_id":install_id,"label":"EF","input_defaults":{},"request_id":"ctx-1294"}),
    );
    let context = context["context"]["id"]
        .as_str()
        .or_else(|| context["id"].as_str())
        .unwrap()
        .to_owned();
    let bind = json!({"install_id":install_id,"context_id":context,"slot":"source","connection_id":hosted.unwrap()["id"],"request_id":"bind-1294"});
    operator(&state, "app_binding_create", bind);
    crate::store::app_records::RecordStore::open(&state, install_id.as_str().unwrap())
        .unwrap()
        .app_social_sources_save(&context, 0, &["juicysuite_crm".to_owned()], "src-1294")
        .unwrap();
    crate::operator_auth::ensure_secret(&state).unwrap();
    let secret = crate::operator_auth::read_secret(&state).unwrap();
    let nonce = operator(
        &state,
        "operator_link_mint",
        json!({"secret":secret,"origin":"loopback"}),
    )["nonce"]
        .clone();
    let session = operator(
        &state,
        "operator_session_open",
        json!({"nonce":nonce,"origin":"loopback"}),
    );
    let (token, key) = (session["token"].clone(), session["key"].clone());
    let mint = operator(
        &state,
        "app_screen_mint",
        json!({"install_id":install_id,"context_id":context,"tag":"feed","token":token,"key":key,"origin":"loopback","generation":1}),
    );
    let nonce = mint["mount"].as_str().unwrap().rsplit('/').next().unwrap();
    operator(&state, "app_screen_consume", json!({"nonce":nonce}));
    let fetch = |handle: &str, request: &str| {
        scoped(Asserted::Operator, || {
            crate::client::rpc(
                &state,
                "app_tool_invoke",
                json!({"action_token":mint["action_token"],"tool_alias":"instagram.read","input":{"handle":handle},"request_id":request,"token":token,"key":key,"origin":"loopback"}),
            )
        })
    };
    let receipt = fetch("juicysuite_crm", "fetch-1294").unwrap();
    assert_eq!(
        receipt["receipt"]["result"]["posts"][0]["caption"], "Door caption",
        "{receipt}"
    );
    let calls = CALLS.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0]["query"]["handle"], "juicysuite_crm");
    // An unsaved handle is refused with no further provider call.
    assert!(fetch("someone_else", "fetch-1294-b").is_err());
    assert_eq!(CALLS.lock().unwrap().len(), 1);
}

static TEXT_CALLS: std::sync::Mutex<Vec<Value>> = std::sync::Mutex::new(Vec::new());

/// The AgenticOS door for `generate_text` (manifest @4): the quote view and
/// the call reply in the shape the host's text contract validates.
fn text_door(method: &str, url: &str, body: &Value) -> Reply {
    let price = json!({"currency":"USD","scale":6,"amount":"0.002000"});
    match (method, url) {
        ("GET", "/v1/runtime/tools/generate_text") => json_response(
            200,
            json!({"ok":true,"data":{"slug":"generate_text","displayName":"Generate text","effect":"draft","chargePrecondition":"max_charge_minor@1","price":price,"unitPrice":null}}),
        ),
        ("POST", CALL_PATH) if body["slug"] == "generate_text" => {
            TEXT_CALLS.lock().unwrap().push(body.clone());
            json_response(
                200,
                json!({"ok":true,"data":{"slug":"generate_text","repeated":false,"price":price,"result":{"text":"Door caption from the writer slot.","finishReason":"stop","usage":{"inputTokens":9,"outputTokens":5,"totalTokens":14,"cachedInputTokens":null,"reasoningOutputTokens":null}}}}),
            )
        }
        _ => refused("not_allowlisted"),
    }
}

/// CAD-1302: Social Content declares its text slot as `writer`
/// (`tests/fixtures/apps/social-content-writer/app.md` copies the real
/// declarations verbatim). A caption request through the real screen path
/// must dispatch on the bound capability and reach the lease door.
#[test]
fn cad1302_caption_through_a_slot_named_writer_reaches_the_lease_door() {
    let (_stop, root, state) = boot_daemon_pinned(text_door, TEXT_MANIFEST_PIN);
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = root.path().join("app");
    // The host's Social Content workflows satisfy the local-workflow
    // check; the manifest is the real app's declarations, `writer` included.
    copy_dir(
        &manifest_dir.join("workspace-apps/social-content"),
        &fixture,
    );
    std::fs::copy(
        manifest_dir.join("tests/fixtures/apps/social-content-writer/app.md"),
        fixture.join("app.md"),
    )
    .unwrap();
    copy_dir(
        &manifest_dir.join("tests/fixtures/apps/ig-tools-fixture/screens"),
        &fixture.join("screens"),
    );
    let screens = fixture.join("screens/feed/screens.json");
    let text = std::fs::read_to_string(&screens).unwrap();
    let text = text.replace("ig-tools-fixture", "social-content").replace(
        "\"instagram.read\": \"source\"",
        "\"caption.generate\": \"writer\"",
    );
    assert!(text.contains("caption.generate"), "{text}");
    std::fs::write(&screens, text).unwrap();
    let install = operator(
        &state,
        "app_workspace_install",
        json!({"source":fixture.to_str().unwrap()}),
    );
    let (install_id, digest) = (install["install_id"].clone(), install["digest"].clone());
    operator(
        &state,
        "app_local_install_approve",
        json!({"install_id":install_id,"digest":digest}),
    );
    let rows = operator(&state, "connection_list", json!({}))["connections"].clone();
    let hosted = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account"] == "hosted")
        .unwrap()["id"]
        .clone();
    let context = operator(
        &state,
        "app_context_create",
        json!({"install_id":install_id,"label":"EF","input_defaults":{},"request_id":"ctx-1302"}),
    );
    let context = context["context"]["id"]
        .as_str()
        .or_else(|| context["id"].as_str())
        .unwrap()
        .to_owned();
    let bind = json!({"install_id":install_id,"context_id":context,"slot":"writer","connection_id":hosted,"request_id":"bind-1302"});
    operator(&state, "app_binding_create", bind);
    let draft = crate::store::app_records::RecordStore::open(&state, install_id.as_str().unwrap())
        .unwrap()
        .app_social_draft_create(
            &context,
            "Draft caption",
            &crate::store::app_social_drafts::DraftSource::ToolReceipt {
                receipt_id: "receipt-1302".into(),
                post_id: None,
            },
            None,
            "draft-1302",
            "session:test",
        )
        .unwrap();
    crate::operator_auth::ensure_secret(&state).unwrap();
    let secret = crate::operator_auth::read_secret(&state).unwrap();
    let nonce = operator(
        &state,
        "operator_link_mint",
        json!({"secret":secret,"origin":"loopback"}),
    )["nonce"]
        .clone();
    let session = operator(
        &state,
        "operator_session_open",
        json!({"nonce":nonce,"origin":"loopback"}),
    );
    let (token, key) = (session["token"].clone(), session["key"].clone());
    let mint = operator(
        &state,
        "app_screen_mint",
        json!({"install_id":install_id,"context_id":context,"tag":"feed","token":token,"key":key,"origin":"loopback","generation":1}),
    );
    let nonce = mint["mount"].as_str().unwrap().rsplit('/').next().unwrap();
    operator(&state, "app_screen_consume", json!({"nonce":nonce}));
    let receipt = operator(
        &state,
        "app_tool_invoke",
        json!({"action_token":mint["action_token"],"tool_alias":"caption.generate",
            "input":{"messages":[{"role":"user","content":"Draft a caption."}]},
            "generation_scope":{"operation":"caption","draft_id":draft["draft_id"],"revision":1},
            "request_id":"caption-1302","token":token,"key":key,"origin":"loopback"}),
    );
    assert_eq!(
        receipt["receipt"]["result"]["text"], "Door caption from the writer slot.",
        "{receipt}"
    );
    let calls = TEXT_CALLS.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0]["slug"], "generate_text");
}

// ---------------------------------------------------------------------------
// CAD-1302 acceptance check — written by the Spec/security reviewer
// (opus-rev-spec-904), not the implementer.
//
// Ticket outcome: the AgenticOS adapter is chosen by the binding's frozen
// capability, never by the app-chosen slot name, and no forged binding
// reaches the door. Two layers refuse a forgery, and this check exercises
// both:
// - the screen path (session, mounted screen, `app_tool_invoke`): the
//   manifest declaration, the provider descriptor's `resolve_action` at bind,
//   `app_binding_live`'s drift check and the read/draft classification all
//   run before the quote and the adapter, so a forged declaration never gets
//   a binding at all;
// - the adapter boundary: a frozen authority whose binding config was forged
//   after the fact (the stored digest can only be recomputed in-store, so it
//   is forged here at the adapter's input) must be refused by the adapter's
//   own validator (`validate_text_authority` / `validate_source_authority`)
//   with no call to the door.
// "Nothing sent" means no POST to the tools-call route.
// ---------------------------------------------------------------------------

static DOOR_1302: std::sync::Mutex<Vec<(String, String, Value)>> =
    std::sync::Mutex::new(Vec::new());

/// The lease door for both reviewed tools. Quote and call replies follow the
/// AgenticOS source: `generateTextResultSchema` (strict `text`,
/// `finishReason` in stop|length, nullable `usage` with five nullable token
/// counts) inside the tools-call `{slug,repeated,price,result}` envelope.
fn door_1302(method: &str, url: &str, body: &Value) -> Reply {
    DOOR_1302
        .lock()
        .unwrap()
        .push((method.to_owned(), url.to_owned(), body["slug"].clone()));
    let price = json!({"currency":"USD","scale":6,"amount":"0.002000"});
    match (method, url) {
        ("GET", "/v1/runtime/tools/generate_text") => json_response(
            200,
            json!({"ok":true,"data":{"slug":"generate_text","displayName":"Generate text","effect":"draft","chargePrecondition":"max_charge_minor@1","price":price,"unitPrice":null}}),
        ),
        ("POST", CALL_PATH) if body["slug"] == "generate_text" => json_response(
            200,
            json!({"ok":true,"data":{"slug":"generate_text","repeated":false,"price":price,"result":{"text":"Caption 1302.","finishReason":"stop","usage":{"inputTokens":9,"outputTokens":5,"totalTokens":14,"cachedInputTokens":null,"reasoningOutputTokens":null}}}}),
        ),
        _ => generic_door(method, url, body),
    }
}

fn posts_1302(slug: &str) -> usize {
    DOOR_1302
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, u, s)| m == "POST" && u == CALL_PATH && s == slug)
        .count()
}

fn all_posts_1302() -> usize {
    DOOR_1302
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, u, _)| m == "POST" && u == CALL_PATH)
        .count()
}

/// The real Social Content declaration with the `writer` slot.
fn writer_manifest() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/apps/social-content-writer/app.md"
    );
    std::fs::read_to_string(path).unwrap()
}

/// On a fresh daemon, install `manifest` with one screen whose `alias` invokes `slot`, bind
/// `slot` in a fresh context to the builtin hosted account, then invoke the
/// alias through the mounted screen. Any refusal along the way is returned.
fn invoke_1302(
    tag: &str,
    manifest: &str,
    (alias, slot): (&str, &str),
    input: Value,
    scope: Option<Value>,
) -> std::result::Result<Value, String> {
    // A fresh daemon per case: one workspace holds one Social Content.
    let (_stop, root, state) = boot_daemon_pinned(door_1302, TEXT_MANIFEST_PIN);
    let (root, state) = (root.path(), state.as_path());
    let rpc = |method: &str, params: Value| {
        scoped(Asserted::Operator, || {
            crate::client::rpc(state, method, params)
        })
        .map_err(|e| format!("{method}: {e}"))
    };
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = root.join(format!("app-{tag}"));
    copy_dir(
        &manifest_dir.join("workspace-apps/social-content"),
        &fixture,
    );
    std::fs::write(fixture.join("app.md"), manifest).unwrap();
    copy_dir(
        &manifest_dir.join("tests/fixtures/apps/ig-tools-fixture/screens"),
        &fixture.join("screens"),
    );
    let screens = fixture.join("screens/feed/screens.json");
    let text = std::fs::read_to_string(&screens).unwrap();
    let text = text.replace("ig-tools-fixture", "social-content").replace(
        "\"instagram.read\": \"source\"",
        &format!("\"{alias}\": \"{slot}\""),
    );
    std::fs::write(&screens, text).unwrap();
    let install = rpc(
        "app_workspace_install",
        json!({"source":fixture.to_str().unwrap()}),
    )?;
    let (install_id, digest) = (install["install_id"].clone(), install["digest"].clone());
    rpc(
        "app_local_install_approve",
        json!({"install_id":install_id,"digest":digest}),
    )?;
    let rows = rpc("connection_list", json!({}))?["connections"].clone();
    let hosted = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account"] == "hosted")
        .unwrap()["id"]
        .clone();
    let context = rpc(
        "app_context_create",
        json!({"install_id":install_id,"label":"EF","input_defaults":{},"request_id":format!("ctx-{tag}")}),
    )?;
    let context = context["context"]["id"]
        .as_str()
        .or_else(|| context["id"].as_str())
        .unwrap()
        .to_owned();
    rpc(
        "app_binding_create",
        json!({"install_id":install_id,"context_id":context,"slot":slot,"connection_id":hosted,"request_id":format!("bind-{tag}")}),
    )?;
    let records =
        crate::store::app_records::RecordStore::open(state, install_id.as_str().unwrap()).unwrap();
    records
        .app_social_sources_save(
            &context,
            0,
            &["juicysuite_crm".to_owned()],
            &format!("src-{tag}"),
        )
        .unwrap();
    let draft = records
        .app_social_draft_create(
            &context,
            "Draft caption",
            &crate::store::app_social_drafts::DraftSource::ToolReceipt {
                receipt_id: format!("receipt-{tag}"),
                post_id: None,
            },
            None,
            &format!("draft-{tag}"),
            "session:test",
        )
        .unwrap();
    crate::operator_auth::ensure_secret(state).unwrap();
    let secret = crate::operator_auth::read_secret(state).unwrap();
    let nonce = rpc(
        "operator_link_mint",
        json!({"secret":secret,"origin":"loopback"}),
    )?["nonce"]
        .clone();
    let session = rpc(
        "operator_session_open",
        json!({"nonce":nonce,"origin":"loopback"}),
    )?;
    let (token, key) = (session["token"].clone(), session["key"].clone());
    let mint = rpc(
        "app_screen_mint",
        json!({"install_id":install_id,"context_id":context,"tag":"feed","token":token,"key":key,"origin":"loopback","generation":1}),
    )?;
    let nonce = mint["mount"].as_str().unwrap().rsplit('/').next().unwrap();
    rpc("app_screen_consume", json!({"nonce":nonce}))?;
    let mut params = json!({"action_token":mint["action_token"],"tool_alias":alias,"input":input,
        "request_id":format!("call-{tag}"),"token":token,"key":key,"origin":"loopback"});
    if let Some(scope) = scope {
        let mut scope = scope;
        scope["draft_id"] = draft["draft_id"].clone();
        params["generation_scope"] = scope;
    }
    let mut out = rpc("app_tool_invoke", params)?;
    // Carry the live binding so the adapter-boundary leg starts from the
    // real frozen config.
    let bindings = rpc(
        "app_binding_list",
        json!({"install_id":install_id,"context_id":context}),
    )?;
    out["binding_1302"] = bindings["bindings"]
        .as_array()
        .and_then(|rows| rows.iter().find(|b| b["slot"] == slot).cloned())
        .unwrap_or(Value::Null);
    out["install_1302"] = install_id;
    Ok(out)
}

#[test]
fn cad1302_adapter_follows_the_bound_capability_and_refuses_forged_bindings() {
    let caption_in = json!({"messages":[{"role":"user","content":"Draft a caption."}]});
    let caption_scope = Some(json!({"operation":"caption","draft_id":null,"revision":1}));
    let real = writer_manifest();
    let writer_block = "    writer:\n      schema: 1\n      capability: text.generate\n      version: 1\n      action: generate_text\n      resource_kind: connection_account\n      effect: draft\n";
    assert!(
        real.contains(writer_block),
        "fixture drifted from the real app"
    );

    // (a) `writer` with the app's real declaration reaches generate_text.
    let out = invoke_1302(
        "a",
        &real,
        ("caption.generate", "writer"),
        caption_in.clone(),
        caption_scope.clone(),
    )
    .unwrap_or_else(|e| panic!("(a) writer caption refused: {e}"));
    assert_eq!(out["receipt"]["result"]["text"], "Caption 1302.", "{out}");
    assert_eq!(posts_1302("generate_text"), 1, "(a) one generate_text call");
    let binding = out["binding_1302"].clone();
    let install_id = out["install_1302"].clone();
    assert_eq!(binding["config"]["mapping"]["capability"], "text.generate");

    // (b, screen path) The `writer` slot forged in the app's own declaration:
    // each is refused before any call reaches the door.
    for (field, forged) in [
        ("action: generate_text", "action: generate_image"),
        ("effect: draft", "effect: send"),
        ("version: 1", "version: 2"),
        (
            "resource_kind: connection_account",
            "resource_kind: workspace",
        ),
    ] {
        let block = writer_block.replacen(field, forged, 1);
        let manifest = real.replacen(writer_block, &block, 1);
        let before = all_posts_1302();
        let result = invoke_1302(
            &format!("b-{}", forged.split(':').next().unwrap()),
            &manifest,
            ("caption.generate", "writer"),
            caption_in.clone(),
            caption_scope.clone(),
        );
        let refused = result.expect_err("forged declaration accepted");
        assert!(
            !refused.contains("already has workspace installation"),
            "{refused}"
        );
        assert_eq!(all_posts_1302(), before, "forged {forged} reached the door");
    }

    // (b, adapter boundary) The real frozen `writer` authority, then each
    // field forged after binding. The unforged authority is the control.
    let mut adapter = hosted_with_pin(TEXT_MANIFEST_PIN).unwrap();
    let door = tiny_http::Server::http("127.0.0.1:0").unwrap();
    adapter.base = format!("http://{}", door.server_addr().to_ip().unwrap());
    let door = std::sync::Arc::new(door);
    let serving = door.clone();
    std::thread::spawn(move || {
        for mut request in serving.incoming_requests() {
            let mut text = String::new();
            request.as_reader().read_to_string(&mut text).unwrap();
            let body = serde_json::from_str(&text).unwrap_or(Value::Null);
            let reply = door_1302(request.method().as_str(), request.url(), &body);
            let _ = request.respond(reply);
        }
    });
    let authority = |slot: &str, binding: &Value| {
        json!({"schema":1,"install_id":install_id,"context_id":null,"slot":slot,
            "binding":binding,"inputs":caption_in,"source":null,"quote":media_quote(),
            "call_id":"app-tool-1302","invocation_id":"app-tool-1302"})
    };
    let call = |proof: &Value| {
        adapter.execute_app_capability_outcome(b"", proof, &caption_in, "app-tool-1302")
    };
    let before = posts_1302("generate_text");
    let control = call(&authority("writer", &binding)).map(|out| out.result);
    assert!(control.is_ok(), "adapter control refused: {control:?}");
    assert_eq!(
        posts_1302("generate_text"),
        before + 1,
        "control never sent"
    );
    let forgeries: Vec<(&str, Value)> = vec![
        ("mapping.tool", json!(IMAGE_TOOL)),
        ("mapping.action", json!("generate_image")),
        ("mapping.effect", json!("send")),
        ("mapping.version", json!(2)),
        ("mapping.resource_kind", json!("workspace")),
        ("connection_id", json!("")),
        ("account", json!("")),
        ("install_id", json!("another-install")),
        ("provider", json!("agenticos")),
    ];
    for (field, value) in forgeries {
        let mut forged = binding.clone();
        match field.strip_prefix("mapping.") {
            Some(sub) => forged["config"]["mapping"][sub] = value,
            None => forged["config"][field] = value,
        }
        let before = all_posts_1302();
        let result = call(&authority("writer", &forged)).map(|out| out.result);
        assert!(
            matches!(result, Err(AppCapabilityError::Refused(_))),
            "forged {field} was not refused: {result:?}"
        );
        assert_eq!(all_posts_1302(), before, "forged {field} reached the door");
    }

    // (c) A slot NAMED `text` bound to a non-text capability never reaches
    // the text door, through the screen path and at the adapter.
    for (capability, action, effect) in [
        ("text.publish", "publish", "send"),
        ("social.read", "list_posts", "read"),
        ("media.generate", "generate_image", "draft"),
        ("", "generate_text", "draft"),
    ] {
        let block = format!(
            "    text:\n      schema: 1\n      capability: {}\n      version: 1\n      action: {action}\n      resource_kind: connection_account\n      effect: {effect}\n",
            if capability.is_empty() { "''" } else { capability }
        );
        let manifest = real.replacen(writer_block, &block, 1);
        let before = posts_1302("generate_text");
        let result = invoke_1302(
            &format!("c-{}", capability.replace('.', "-")),
            &manifest,
            ("caption.generate", "text"),
            caption_in.clone(),
            caption_scope.clone(),
        );
        assert_eq!(
            posts_1302("generate_text"),
            before,
            "slot `text` bound to {capability:?} reached generate_text: {result:?}"
        );
        let mut forged = binding.clone();
        forged["config"]["mapping"]["capability"] = json!(capability);
        let result = call(&authority("text", &forged)).map(|out| out.result);
        assert!(result.is_err(), "{capability:?} at the adapter: {result:?}");
        assert_eq!(
            posts_1302("generate_text"),
            before,
            "{capability:?} at the adapter"
        );
    }

    // (d) A source capability under an unusual slot name goes only to the
    // source adapter and passes its validation.
    let source_block = "    source:\n      schema: 1\n      capability: social.read\n";
    assert!(real.contains(source_block));
    let manifest = real.replacen(
        source_block,
        "    library:\n      schema: 1\n      capability: social.read\n",
        1,
    );
    let (text_before, source_before) = (
        posts_1302("generate_text"),
        posts_1302("read_instagram_posts"),
    );
    let out = invoke_1302(
        "d",
        &manifest,
        ("instagram.read", "library"),
        json!({"handle":"juicysuite_crm"}),
        None,
    )
    .unwrap_or_else(|e| panic!("(d) source under `library` refused: {e}"));
    assert_eq!(
        out["receipt"]["result"]["posts"][0]["caption"], "Door caption",
        "{out}"
    );
    assert_eq!(posts_1302("read_instagram_posts"), source_before + 1);
    assert_eq!(posts_1302("generate_text"), text_before);
}

/// CAD-1303 door: the source read as above, plus the image tool's quote. An
/// image call is refused, so a generation never settles; the test reads the
/// frozen plan instead. Every POST is recorded.
static DOOR_1303: std::sync::Mutex<Vec<Value>> = std::sync::Mutex::new(Vec::new());

fn door_1303(method: &str, url: &str, body: &Value) -> Reply {
    if method == "POST" {
        DOOR_1303.lock().unwrap().push(body.clone());
    }
    match (method, url) {
        ("GET", MEDIA_PRICE_PATH) => json_response(200, price_view(40, "2026-09-29T00:00:00.000Z")),
        ("POST", CALL_PATH) if body["slug"] != "read_instagram_posts" => refused("not_allowlisted"),
        _ => generic_door(method, url, body),
    }
}

/// Social Content with a screen declaring the draft alias (`drafts`, the
/// draft-effect `image` slot) and the source read alias.
fn social_fixture(root: &std::path::Path) -> PathBuf {
    let fixture = root.join("app");
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    copy_dir(
        &manifest_dir.join("workspace-apps/social-content"),
        &fixture,
    );
    let screens = manifest_dir.join("tests/fixtures/apps/ig-tools-fixture/screens");
    copy_dir(&screens, &fixture.join("screens"));
    let path = fixture.join("screens/feed/screens.json");
    let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    doc["app"] = json!("social-content");
    doc["tools"] = json!({"instagram.read":"source","drafts":"image"});
    std::fs::write(&path, doc.to_string()).unwrap();
    fixture
}

/// Install, approve and mount Social Content in one context through `op`
/// (an operator RPC). Answers (install_id, context_id).
fn social_install(
    op: &dyn Fn(&str, Value) -> Value,
    fixture: &std::path::Path,
    bind_hosted: bool,
) -> (String, String) {
    let install = op(
        "app_workspace_install",
        json!({"source":fixture.to_str().unwrap()}),
    );
    let install_id = install["install_id"].as_str().unwrap().to_owned();
    op(
        "app_local_install_approve",
        json!({"install_id":install_id,"digest":install["digest"]}),
    );
    let context = op(
        "app_context_create",
        json!({"install_id":install_id,"label":"EF","input_defaults":{},"request_id":"ctx-1303"}),
    );
    let context = context["context"]["id"]
        .as_str()
        .or_else(|| context["id"].as_str())
        .unwrap()
        .to_owned();
    if bind_hosted {
        let rows = op("connection_list", json!({}))["connections"].clone();
        let hosted = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["account"] == "hosted")
            .unwrap()["id"]
            .clone();
        for slot in ["source", "image"] {
            op(
                "app_binding_create",
                json!({"install_id":install_id,"context_id":context,"slot":slot,"connection_id":hosted,"request_id":format!("bind-1303-{slot}")}),
            );
        }
    }
    (install_id, context)
}

/// CAD-1303 acceptance (reviewer-written; the implementer must not edit or
/// weaken it). Soft discard of a social draft through the real daemon RPC
/// and the board relay, and the image idea bound on the image tool.
#[test]
fn cad1303_discard_is_operator_only_refuses_in_use_and_stale_and_hides_for_good() {
    use crate::store::app_social_drafts::{DraftSource, EffectStage};
    let (mut stop, root, state) = boot_daemon(door_1303);
    let op = |method: &str, params: Value| operator(&state, method, params);
    let fixture = social_fixture(root.path());
    let (install, context) = social_install(&op, &fixture, true);
    crate::store::Store::open(&state.join("cadence.sqlite3"))
        .unwrap()
        .register_agent(&crate::store::NewAgent {
            alias: "worker",
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: root.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let records = crate::store::app_records::RecordStore::open(&state, &install).unwrap();
    records
        .app_social_sources_save(&context, 0, &["juicysuite_crm".to_owned()], "src-1303")
        .unwrap();

    // The board, and the operator's session from the real login-link
    // exchange. Its cookie token and key serve the RPC calls as well.
    let mut port = 3110 + (std::process::id() % 80) as u16;
    loop {
        let (startup, ready) = std::sync::mpsc::channel();
        let board_opts = crate::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.0.clone()),
            startup: Some(startup),
            test_seam: true,
            ..Default::default()
        };
        let (state, pm) = (state.clone(), root.path().join("pm"));
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &board_opts)));
        match ready.recv_timeout(Duration::from_secs(20)).unwrap() {
            Ok(()) => break stop.1.push(thread),
            Err(_) if port < 3199 => port += 1,
            Err(kind) => panic!("board could not bind: {kind:?}"),
        }
        thread.join().unwrap();
    }
    let seam = Seam::token_at(&state).unwrap();
    let host = format!("cadence-{port}.localhost:{port}");
    crate::operator_auth::ensure_secret(&state).unwrap();
    let secret = crate::operator_auth::read_secret(&state).unwrap();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let base = format!("http://127.0.0.1:{port}");
    // (cookie, token, key, action token) of a fresh operator board session
    // with Social Content mounted. The board revokes a session an agent
    // presents, so the test signs in again after such a probe.
    let login = || -> [String; 4] {
        let nonce = op(
            "operator_link_mint",
            json!({"secret":secret,"origin":"loopback"}),
        )["nonce"]
            .clone();
        let session = board(agent.post(format!("{base}/api/session")), &host, &seam)
            .header("Content-Type", "application/json")
            .send(json!({"nonce":nonce}).to_string())
            .unwrap();
        let set = session.headers()["set-cookie"].to_str().unwrap();
        let cookie = set[..set.find(';').unwrap()].to_owned();
        let token = cookie.split_once('=').unwrap().1.to_owned();
        let key: Value = session.into_body().read_json().unwrap();
        let key = key["session_key"].as_str().unwrap().to_owned();
        let mint = op(
            "app_screen_mint",
            json!({"install_id":install,"context_id":context,"tag":"feed","token":token,"key":key,"origin":"loopback","generation":1}),
        );
        let nonce = mint["mount"].as_str().unwrap().rsplit('/').next().unwrap();
        op("app_screen_consume", json!({"nonce":nonce}));
        let action = mint["action_token"].as_str().unwrap().to_owned();
        [cookie, token, key, action]
    };
    let creds = std::cell::RefCell::new(login());
    let relogin = || *creds.borrow_mut() = login();
    let session = |extra: Value| {
        let [_, token, key, action] = creds.borrow().clone();
        let mut params = json!({"action_token":action,"token":token,"key":key,"origin":"loopback"});
        for (k, v) in extra.as_object().unwrap() {
            params[k] = v.clone();
        }
        params
    };

    // Real RPC as `who`, with the operator's valid session and mount, so
    // only the caller differs.
    let rpc = |who: Asserted, method: &str, extra: Value| {
        let mut params = session(extra);
        params["tool_alias"] = json!("drafts");
        scoped(who, || crate::client::rpc(&state, method, params))
    };
    // The board relay as `who` (the seam header), with the operator's cookie.
    let http = |who: &Asserted, route: &str, body: Value| -> (u16, Value) {
        let [cookie, _, key, action] = creds.borrow().clone();
        let mut body = body;
        body["action_token"] = json!(action);
        body["tool_alias"] = json!("drafts");
        let request = agent
            .post(format!("{base}/api/app-social-drafts/{route}"))
            .header("Host", &host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{host}"))
            .header(crate::test_seam::AS_HEADER, who.as_str())
            .header(crate::test_seam::TOKEN_HEADER, &seam)
            .header("Cookie", &cookie)
            .header("X-Cadence-Session", &key)
            .header("Content-Type", "application/json");
        let mut response = request.send(body.to_string()).unwrap();
        let status = response.status().as_u16();
        (
            status,
            response.body_mut().read_json().unwrap_or(Value::Null),
        )
    };
    let listed = || -> Vec<(String, i64)> {
        rpc(Asserted::Operator, "app_social_draft_list", json!({})).unwrap()["drafts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| {
                (
                    d["draft_id"].as_str().unwrap().to_owned(),
                    d["revision"].as_i64().unwrap(),
                )
            })
            .collect()
    };
    let effect_state = |id: &str| {
        op("app_effect_show", json!({"effect_id":id}))["effect"]["state"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    // A retained source receipt through the real Fetch path, for the drafts.
    let fetched = scoped(Asserted::Operator, || {
        crate::client::rpc(
            &state,
            "app_tool_invoke",
            session(json!({"tool_alias":"instagram.read","input":{"handle":"juicysuite_crm"},"request_id":"fetch-1303"})),
        )
    })
    .unwrap();
    let receipt_id = fetched["receipt"]["id"].as_str().unwrap().to_owned();
    let source = DraftSource::ToolReceipt {
        receipt_id,
        post_id: Some("post-1".into()),
    };
    let draft = |request: &str| {
        records
            .app_social_draft_create(
                &context,
                "A caption",
                &source,
                None,
                request,
                "session:test",
            )
            .unwrap()["draft_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let effect_seq = std::cell::Cell::new(0u32);
    let stage = |draft: &str, revision: i64| {
        effect_seq.set(effect_seq.get() + 1);
        let id = format!(
            "sfx_{}_{:032x}",
            install
                .as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            effect_seq.get()
        );
        let frozen = json!({"draft_id":draft,"revision":revision,"context_id":context});
        let staged = records
            .app_social_effect_stage(
                &context,
                &EffectStage {
                    draft,
                    revision,
                    request: &format!("stage-{}", effect_seq.get()),
                    effect_id: &id,
                    frozen: &frozen,
                    approval: "approval-1303",
                },
            )
            .unwrap();
        (id, staged["effect"]["digest"].as_str().unwrap().to_owned())
    };

    let open = draft("d-open");
    let (waiting, waiting_digest) = stage(&open, 1);
    // A draft with an effect approved at revision 1 and edited to revision 2.
    let approved = draft("d-approved");
    let (fx, digest) = stage(&approved, 1);
    records
        .app_social_effect_decide(&fx, &digest, true)
        .unwrap();
    rpc(
        Asserted::Operator,
        "app_social_draft_update",
        json!({"draft_id":approved,"expected_revision":1,"caption":"Edited","request_id":"edit-1303"}),
    )
    .unwrap();
    let sending = draft("d-sending");
    let (fx, digest) = stage(&sending, 1);
    records
        .app_social_effect_decide(&fx, &digest, true)
        .unwrap();
    records.app_social_effect_claim_send(&fx, &digest).unwrap();
    let posted = draft("d-posted");
    let (fx, digest) = stage(&posted, 1);
    records
        .app_social_effect_decide(&fx, &digest, true)
        .unwrap();
    records.app_social_effect_claim_send(&fx, &digest).unwrap();
    records
        .app_social_effect_finish(&fx, "posted", &json!({}))
        .unwrap();
    let mut before = listed();
    before.sort();
    let unchanged = |why: &str| {
        let mut now = listed();
        now.sort();
        assert_eq!(now, before, "{why}");
        assert_eq!(effect_state(&waiting), "waiting", "{why}");
    };

    // (a) An agent and an unproven caller are refused at the operator gate,
    // over RPC and over the board relay, and nothing changes.
    let worker = Asserted::Agent("worker".into());
    for who in [worker.clone(), Asserted::Unproven] {
        let error = rpc(
            who.clone(),
            "app_social_draft_discard",
            json!({"draft_id":open,"revision":1}),
        )
        .expect_err("a non-operator caller must not discard")
        .to_string();
        assert!(
            error.contains("operator action") || error.contains("not provably the operator"),
            "{who:?} over RPC: {error}"
        );
        unchanged("non-operator RPC");
        let (status, body) = http(&who, "discard", json!({"draft_id":open,"revision":1}));
        assert!(status >= 400, "{who:?} over HTTP: {status} {body}");
        relogin();
        unchanged("non-operator HTTP");
    }

    // (b) Any approved, sending or posted effect (of any revision) keeps the
    // draft, over RPC and HTTP; nothing changes.
    for (id, revision) in [(&approved, 2), (&sending, 1), (&posted, 1)] {
        let error = rpc(
            Asserted::Operator,
            "app_social_draft_discard",
            json!({"draft_id":id,"revision":revision}),
        )
        .unwrap_err();
        assert_eq!(error.code(), Some("draft_in_use"), "{id}: {error}");
        let (status, body) = http(
            &Asserted::Operator,
            "discard",
            json!({"draft_id":id,"revision":revision}),
        );
        assert!(status >= 400, "{id} over HTTP: {status} {body}");
        unchanged("in-use draft");
    }

    // (c) A stale revision and an unknown draft are refused; nothing changes.
    let stale = rpc(
        Asserted::Operator,
        "app_social_draft_discard",
        json!({"draft_id":open,"revision":2}),
    )
    .unwrap_err();
    assert_eq!(stale.code(), Some("stale_revision"), "{stale}");
    unchanged("stale revision");
    let unknown = rpc(
        Asserted::Operator,
        "app_social_draft_discard",
        json!({"draft_id":"sdr-unknown","revision":1}),
    )
    .unwrap_err();
    assert_eq!(unknown.code(), Some("draft_not_found"), "{unknown}");
    unchanged("unknown draft");

    // (d) The operator's discard over the board relay: gone from list, show
    // and update; its waiting effect declined and never approvable.
    let (status, body) = http(
        &Asserted::Operator,
        "discard",
        json!({"draft_id":open,"revision":1}),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        json!({"draft_id":open,"state":"discarded","revision":1})
    );
    assert!(listed().iter().all(|(id, _)| id != &open));
    assert!(rpc(
        Asserted::Operator,
        "app_social_draft_show",
        json!({"draft_id":open})
    )
    .is_err());
    assert!(rpc(
        Asserted::Operator,
        "app_social_draft_update",
        json!({"draft_id":open,"expected_revision":1,"caption":"Back","request_id":"edit-1303-b"}),
    )
    .is_err());
    assert_eq!(effect_state(&waiting), "declined");
    let decide = op_err(
        &state,
        "app_effect_decide",
        json!({"effect_id":waiting,"digest":waiting_digest,"decision":"accept"}),
    );
    assert!(decide.contains("no longer waiting"), "{decide}");
    assert!(op_err(
        &state,
        "app_effect_publish_now",
        json!({"effect_id":waiting,"digest":waiting_digest}),
    )
    .contains("approved"));
    // An already discarded draft is refused and stays discarded.
    let again = rpc(
        Asserted::Operator,
        "app_social_draft_discard",
        json!({"draft_id":open,"revision":1}),
    )
    .unwrap_err();
    assert_eq!(again.code(), Some("draft_not_found"), "{again}");
    assert!(listed().iter().all(|(id, _)| id != &open));

    // (e) The image idea: anything but {} or exactly {image_prompt} of 1-512
    // plain characters is refused before any provider call; 512 characters
    // are carried into the frozen plan whole.
    let target = draft("d-image");
    let image = |input: Value, request: &str| {
        scoped(Asserted::Operator, || {
            crate::client::rpc(
                &state,
                "app_tool_invoke",
                session(
                    json!({"tool_alias":"drafts","input":input,"request_id":request,"generation_scope":{"operation":"image","draft_id":target,"revision":1}}),
                ),
            )
        })
    };
    let posts_before = DOOR_1303.lock().unwrap().len();
    for (n, bad) in [
        json!({"image_prompt":"x".repeat(513)}),
        json!({"image_prompt":""}),
        json!({"image_prompt":"   "}),
        json!({"image_prompt":"line\nbreak"}),
        json!({"image_prompt":"tab\there"}),
        json!({"image_prompt":"bell\u{7}"}),
        json!({"image_prompt":"ok","subject":"forged"}),
        json!({"image_prompt":7}),
    ]
    .into_iter()
    .enumerate()
    {
        let error = image(bad.clone(), &format!("img-bad-{n}"))
            .expect_err("a bad image idea must be refused")
            .to_string();
        assert!(
            error.contains("image idea") || error.contains("image input"),
            "{bad}: {error}"
        );
    }
    assert_eq!(
        DOOR_1303.lock().unwrap().len(),
        posts_before,
        "no provider call"
    );
    let idea = "a".repeat(511) + "z";
    let _ = image(json!({"image_prompt":idea}), "img-good");
    let intents = rpc(Asserted::Operator, "app_social_draft_list", json!({})).unwrap()
        ["generation_intents"]
        .clone();
    let plan = intents
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["request_id"] == "img-good")
        .unwrap_or_else(|| panic!("no frozen plan: {intents}"));
    assert_eq!(plan["input"]["image_prompt"], json!(idea), "{plan}");
    drop(stop);
}

fn op_err(state: &std::path::Path, method: &str, params: Value) -> String {
    scoped(Asserted::Operator, || {
        crate::client::rpc(state, method, params)
    })
    .expect_err("must be refused")
    .to_string()
}
