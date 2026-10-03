//! CAD-1096 ticket results at the real boundary: an in-process daemon, the
//! operator's board HTTP quote route and a loopback hosted door that, like
//! AgenticOS since AOS-103, refuses every slug but `read_instagram_posts`.
use super::*;
use crate::test_seam::{scoped, Asserted, Seam};
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
fn board_quote(answer: fn(&str, &str, &Value) -> Reply) -> (u16, Value, Value) {
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
    let mut adapter = hosted_adapter();
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
    let url = format!("{base}/api/app-installations/{install}/bindings/source/quote");
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
    let (status, body, frozen) = board_quote(generic_door);
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
    let (status, body, _) = board_quote(|_, _, _| refused("not_allowlisted"));
    assert_eq!(status, 409, "{body}");
    let message = "bound capability price discovery refused: not_allowlisted";
    assert_eq!(body["error"], message, "{body}");
}
