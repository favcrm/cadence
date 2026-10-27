//! CAD-1656 independent refusal acceptance. Reviewer-written from the
//! ticket by claude-opus-spec-reviewer, not by the implementer.
//! Implementers must not edit or weaken this file.
//!
//! A real daemon (`Shared` over its own socket) and a real public board,
//! with the hosted WhatsApp door injected through
//! `ServeOptions.hosted_whatsapp` and pointed at a loopback double of door
//! v3 that records every handoff request.
//!
//! Refused, with the distinct code `facts_changed`: a hand-back (release
//! SLOT) confirmed after `handoffAt` changed — re-stamped or cleared —
//! on the daemon RPC and on the board's HTTP peer (409, body code
//! `facts_changed`, which is what the app frame receives). Nothing reaches
//! the door and the card stays burned.
//!
//! The allowlist: the board passes `facts_changed` through only when the
//! daemon's code is exactly that. No screen operation yields another
//! daemon conflict today, so a transparent relay between the board and the
//! daemon socket rewrites the code of a real daemon conflict frame; each
//! other code must still reach the frame as `conflict`. The board relay
//! under test is the real one.
#![cfg(feature = "test-seam")]

use crate::daemon::{serve, ServeOptions, Shared};
use crate::operator_auth::{BoardUser, Origin, PublicRealm};
use crate::platform::hosted_whatsapp::HostedWhatsapp;
use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const COMPANY: &str = "co_c1656";
const V1: &str = "agenticos.whatsapp-door.v1";
const V3: &str = "agenticos.whatsapp-door.v3";
const CONV: &str = "conv_c1656";
const OPS: &[&str] = &[
    "whatsapp.conversations.show",
    "whatsapp.handoff.set",
    "whatsapp.handoff.release",
];
const RELEASE: &str = "whatsapp.handoff.release";

// ---------- a loopback door with the v3 handoff semantics ----------

struct DoorState {
    handoffs: Vec<(String, bool)>,
    at: HashMap<String, Option<String>>,
    stamps: u32,
}

#[derive(Clone)]
struct Door {
    url: String,
    state: Arc<Mutex<DoorState>>,
}

impl Door {
    fn start() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr().to_ip().unwrap());
        let state = Arc::new(Mutex::new(DoorState {
            handoffs: Vec::new(),
            at: HashMap::from([(CONV.to_string(), None)]),
            stamps: 0,
        }));
        let door = Self {
            url,
            state: state.clone(),
        };
        std::thread::spawn(move || {
            for mut request in server.incoming_requests() {
                let mut raw = String::new();
                let _ = request.as_reader().read_to_string(&mut raw);
                let body: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                let path = request.url().split('?').next().unwrap_or("").to_string();
                let post = *request.method() == tiny_http::Method::Post;
                let (code, answer) = answer(&state, &path, post, &body);
                let _ = request.respond(
                    tiny_http::Response::from_data(answer.to_string().into_bytes())
                        .with_status_code(code),
                );
            }
        });
        door
    }
    fn handoffs(&self) -> Vec<(String, bool)> {
        self.state.lock().unwrap().handoffs.clone()
    }
    fn at(&self) -> Option<String> {
        self.state.lock().unwrap().at[CONV].clone()
    }
    /// FavCRM re-stamps the takeover behind the card's back.
    fn restamp(&self) {
        let mut door = self.state.lock().unwrap();
        door.stamps += 1;
        let stamp = format!("2026-10-20T00:01:{:02}.000Z", door.stamps);
        door.at.insert(CONV.into(), Some(stamp));
    }
    /// FavCRM clears the takeover elsewhere.
    fn clear(&self) {
        self.state.lock().unwrap().at.insert(CONV.into(), None);
    }
}

fn answer(state: &Mutex<DoorState>, path: &str, post: bool, body: &Value) -> (u16, Value) {
    let mut door = state.lock().unwrap();
    let rest = path.strip_prefix("/v1/runtime/whatsapp").unwrap_or("");
    let parts: Vec<&str> = rest.trim_start_matches('/').split('/').collect();
    let not_found = (
        404,
        json!({"ok": false, "error": {"code": "not_found", "message": "not_found"}}),
    );
    match parts.as_slice() {
        ["conversations", id] if !post && door.at.contains_key(*id) => (
            200,
            json!({"ok": true, "data": {"contract": V1,
            "conversationId": id, "channelConnectionId": "cc_1", "status": "open",
            "contact": {"id": "ct_1", "name": "Synthetic Diner", "phone": "+85200000000",
                "email": null, "whatsappId": "85200000000"},
            "replyMode": "template_required", "freeformUntil": null, "lastInboundAt": null,
            "lastOutboundAt": null, "unreadCount": 0, "handoffAt": door.at[*id]}}),
        ),
        ["conversations", id, "handoff"] if post && door.at.contains_key(*id) => {
            let Some(handoff) = body
                .as_object()
                .filter(|o| o.len() == 1)
                .and_then(|o| o.get("handoff"))
                .and_then(Value::as_bool)
            else {
                return (
                    400,
                    json!({"ok": false, "error": {"code": "invalid_request", "message": "invalid_request"}}),
                );
            };
            door.handoffs.push((id.to_string(), handoff));
            let at = if handoff {
                door.stamps += 1;
                Some(format!("2026-10-20T00:00:{:02}.000Z", door.stamps))
            } else {
                None
            };
            door.at.insert(id.to_string(), at.clone());
            (
                200,
                json!({"ok": true, "data": {"contract": V3, "conversation_id": id, "handoff_at": at}}),
            )
        }
        _ => not_found,
    }
}

// ---------- a transparent relay in front of the daemon socket ----------

/// Copies frames both ways unchanged. When armed with a code, a daemon
/// `conflict` error frame has its `code` replaced: a real daemon conflict
/// with another code, for the board relay's allowlist.
fn relay(client: UnixStream, daemon: &std::path::Path, armed: Arc<Mutex<Option<&'static str>>>) {
    let Ok(upstream) = UnixStream::connect(daemon) else {
        return;
    };
    let (mut to_daemon, mut from_client) =
        (upstream.try_clone().unwrap(), client.try_clone().unwrap());
    let up = std::thread::spawn(move || {
        let _ = std::io::copy(&mut from_client, &mut to_daemon);
        let _ = to_daemon.shutdown(std::net::Shutdown::Write);
    });
    let mut reader = BufReader::new(upstream);
    let mut out = client;
    let mut line = String::new();
    while matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
        let code = *armed.lock().unwrap();
        let mut bytes = line.clone().into_bytes();
        if let (Some(code), Ok(mut frame)) = (code, serde_json::from_str::<Value>(&line)) {
            if frame["ok"] == false && frame["error"]["kind"] == "conflict" {
                frame["error"]["code"] = json!(code);
                bytes = format!("{frame}\n").into_bytes();
            }
        }
        if out.write_all(&bytes).is_err() {
            break;
        }
        line.clear();
    }
    let _ = out.shutdown(std::net::Shutdown::Both);
    let _ = up.join();
}

// ---------- one daemon + public board ----------

struct Servers {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Servers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

// No Debug: synthetic credentials must not enter failure output.
struct Session {
    token: String,
    key: String,
    origin: &'static str,
}

struct Fx {
    // Field order: stop the servers before the temp state is removed.
    _servers: Servers,
    shared: Arc<Shared>,
    root: tempfile::TempDir,
    agent: ureq::Agent,
    base: String,
    host: String,
    seam: String,
    issuer: String,
    armed: Arc<Mutex<Option<&'static str>>>,
}

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn accept_loop(
    listener: UnixListener,
    stop: Arc<AtomicBool>,
    handle: impl Fn(UnixStream) + Send + Sync + 'static,
) -> std::thread::JoinHandle<()> {
    listener.set_nonblocking(true).unwrap();
    let handle = Arc::new(handle);
    std::thread::spawn(move || {
        let mut connections = Vec::new();
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    let handle = handle.clone();
                    connections.push(std::thread::spawn(move || handle(stream)));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(_) => panic!("owned listener failed"),
            }
        }
        for connection in connections {
            let _ = connection.join();
        }
    })
}

impl Fx {
    fn start(door: &Door) -> Self {
        assert!(
            std::env::var_os("CADENCE_SOCKET").is_none(),
            "fixture BLOCKED: inherited socket override"
        );
        let root = tempfile::Builder::new().prefix("c1656").tempdir().unwrap();
        let state = root.path().to_path_buf();
        let pm = state.join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let (port, reservation) = crate::test_ports::reserve_board_port();
        let host = format!("c1656.board.localhost:{port}");
        let issuer = format!("http://127.0.0.1:{port}");
        crate::board_identity::write_config(
            &state,
            &crate::board_identity::Config {
                host: host.clone(),
                issuer: issuer.clone(),
                company: COMPANY.into(),
            },
        )
        .unwrap();
        let opts = ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            test_seam: true,
            hosted_whatsapp: Some(HostedWhatsapp::new(&door.url).unwrap()),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = Shared::new(&state, &opts).unwrap();
        let mut servers = Servers {
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
        };
        // The daemon answers on its own socket; `cadence.sock` (the one
        // every client, the board included, dials) is the relay.
        let daemon_sock = state.join("d.sock");
        let dispatch = shared.clone();
        servers.threads.push(accept_loop(
            UnixListener::bind(&daemon_sock).unwrap(),
            servers.stop.clone(),
            move |stream| serve::handle_conn(dispatch.clone(), stream),
        ));
        let armed: Arc<Mutex<Option<&'static str>>> = Arc::new(Mutex::new(None));
        let relay_armed = armed.clone();
        servers.threads.push(accept_loop(
            UnixListener::bind(state.join("cadence.sock")).unwrap(),
            servers.stop.clone(),
            move |stream| relay(stream, &daemon_sock, relay_armed.clone()),
        ));
        let seam = Seam::token_at(&state).expect("fixture BLOCKED: seam not armed");
        let (ready, receipt) = std::sync::mpsc::channel();
        let board_opts = crate::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(servers.stop.clone()),
            startup: Some(ready),
            test_seam: true,
            allow_hosts: vec![host.clone()],
            allow_origins: vec![format!("http://{host}")],
            public: Some(crate::ui::PublicBoard {
                host: host.clone(),
                authorize_url: format!("{issuer}/unused-authorize"),
                issuer: issuer.clone(),
                company: COMPANY.into(),
            }),
            ..Default::default()
        };
        drop(reservation);
        let (board_state, board_pm) = (state.clone(), pm.clone());
        servers.threads.push(std::thread::spawn(move || {
            drop(crate::ui::serve(&board_state, &board_pm, &board_opts))
        }));
        let bound = receipt
            .recv_timeout(Duration::from_secs(10))
            .expect("fixture BLOCKED: board startup receipt");
        assert!(bound.is_ok(), "fixture BLOCKED: board could not bind");
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        Self {
            _servers: servers,
            shared,
            root,
            agent,
            base: format!("http://127.0.0.1:{port}"),
            host,
            seam,
            issuer,
            armed,
        }
    }

    fn arm(&self, code: Option<&'static str>) {
        *self.armed.lock().unwrap() = code;
    }

    fn rpc(&self, method: &str, params: Value) -> crate::Result<Value> {
        scoped(Asserted::Operator, || {
            crate::client::rpc_timeout(self.root.path(), method, params, Duration::from_secs(20))
        })
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(method, params)
            .unwrap_or_else(|error| panic!("operator RPC {method} failed: {error}"))
    }

    fn install(&self, app: &str) -> (String, String) {
        let source = self.root.path().join(format!("source-{app}"));
        let screen = source.join("screens/main");
        std::fs::create_dir_all(&screen).unwrap();
        let manifest = format!("---\napp: {app}\ntitle: Synthetic WhatsApp\nversion: '0.1.0'\nsummary: Isolated CAD1656 refusal fixture\nneeds:\n  connections: []\n  requires:\n    contracts:\n      app-screens: [1]\n      screen-records: [1]\n---\n\nSynthetic only.\n");
        std::fs::write(source.join("app.md"), manifest).unwrap();
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(source.join("workflows/synthetic.md"), "---\ntitle: Synthetic refusal fixture\ngoal: Synthetic text only\ninputs:\n  writer: { ask: Synthetic writer }\n  reviewer: { ask: Synthetic reviewer }\ndistinct: [writer, reviewer]\n---\n\n## Synthetic draft\nagent: {{writer}}\nsize: S\naction: local.text.produce\n\nProduce synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n\n## Synthetic review\nagent: {{reviewer}}\nsize: S\ndepends_on: 1\naction: local.text.review\n\nReview the synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n").unwrap();
        let js = "(()=>{document.getElementById('root').textContent='c1656';})();";
        std::fs::write(screen.join("client.js"), js).unwrap();
        let declaration = json!({
            "contract": "app-screens/v2", "app": app, "entry": "client.js",
            "host_contract": "screen-records.v1", "may": ["records.call", "records.slot"],
            "operations": OPS, "tools": {},
            "assets": [{"name": "client.js", "media_type": "text/javascript", "size": js.len(), "sha256": hash(js.as_bytes())}],
            "provenance": {"source_digest": hash(app.as_bytes()), "sdk_digest": hash(b"synthetic-sdk"), "toolchain_digest": hash(b"synthetic-toolchain")}
        });
        std::fs::write(screen.join("screens.json"), declaration.to_string()).unwrap();
        let installed = self.op(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        );
        assert_eq!(
            installed["approved"], true,
            "fixture BLOCKED: install consent"
        );
        let install = installed["install_id"].as_str().unwrap().to_owned();
        let context = self.op(
            "app_context_create",
            json!({"install_id": install, "label": "general",
                "input_defaults": {}, "request_id": format!("{app}-context")}),
        )["context"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        (install, context)
    }

    /// The board owner's real public session, minted by this daemon.
    fn owner(&self) -> Session {
        let now = self.shared.operator_now();
        let opened = self
            .shared
            .operator_auth()
            .open_public_bound(
                BoardUser {
                    sub: "synthetic-c1656-operator".into(),
                    email: "c1656-operator@example.test".into(),
                    name: "Synthetic operator".into(),
                    role: "operator".into(),
                    handle: "c1656-operator".into(),
                },
                "c1656-operator-jti-1",
                now + 120,
                "c1656-isolated-acceptance",
                PublicRealm {
                    issuer: self.issuer.clone(),
                    company: COMPANY.into(),
                    host: self.host.clone(),
                },
                now,
            )
            .unwrap()
            .expect("fixture BLOCKED: fresh public jti");
        Session {
            token: opened.token,
            key: String::new(),
            origin: "public",
        }
    }

    fn local_operator(&self) -> Session {
        let now = self.shared.operator_now();
        let mut auth = self.shared.operator_auth();
        let nonce = auth.mint(Origin::Loopback, now).unwrap();
        let opened = auth
            .open(&nonce, Origin::Loopback, "", now)
            .unwrap()
            .expect("fixture BLOCKED: fresh local nonce");
        Session {
            token: opened.token,
            key: opened.key,
            origin: "loopback",
        }
    }

    fn creds(&self, session: &Session, mut body: Value) -> Value {
        body["token"] = json!(session.token);
        body["key"] = json!(session.key);
        body["origin"] = json!(session.origin);
        body
    }

    fn mount(&self, session: &Session, install: &str, context: &str, generation: u64) -> String {
        let body = json!({"install_id": install, "context_id": context,
            "tag": "main", "generation": generation});
        let mint = self
            .rpc("app_screen_mint", self.creds(session, body))
            .unwrap_or_else(|error| panic!("fixture BLOCKED: mint: {error}"));
        let nonce = mint["mount"].as_str().unwrap().rsplit('/').next().unwrap();
        self.op("app_screen_consume", json!({"nonce": nonce}));
        mint["action_token"]
            .as_str()
            .expect("fixture BLOCKED: mount action token")
            .to_owned()
    }

    /// Daemon RPC: a release card's confirmation token.
    fn rpc_card(&self, session: &Session, action: &str) -> String {
        let card = self
            .rpc(
                "app_screen_service_slot_prepare",
                self.creds(
                    session,
                    json!({"action_token": action, "operation": RELEASE,
                        "args": {"conversation_id": CONV}}),
                ),
            )
            .unwrap_or_else(|error| panic!("release SLOT prepare: {error}"));
        card["confirmation_token"].as_str().unwrap().to_owned()
    }

    fn rpc_confirm(&self, session: &Session, action: &str, token: &str) -> crate::Result<Value> {
        self.rpc(
            "app_screen_service_slot_confirm",
            self.creds(
                session,
                json!({"action_token": action, "confirmation_token": token,
                    "confirmation": {"confirmed": true}}),
            ),
        )
    }

    /// The HTTP peer: exactly what the screen host posts for its frame.
    fn post(&self, session: &Session, path: &str, body: &Value) -> (u16, Value) {
        let response = self
            .agent
            .post(format!("{}/api/app-screen-services/{path}", self.base))
            .header("Host", &self.host)
            .header("Origin", format!("http://{}", self.host))
            .header("X-Cadence-Board", "1")
            .header(crate::test_seam::AS_HEADER, "operator")
            .header(crate::test_seam::TOKEN_HEADER, &self.seam)
            .header("Content-Type", "application/json")
            .header(
                "Cookie",
                format!("__Host-aos-board-session={}", session.token),
            )
            .send(body.to_string())
            .expect("owned HTTP transport failed");
        let status = response.status().as_u16();
        let body = response.into_body().read_json().unwrap_or(Value::Null);
        (status, body)
    }

    fn http_card(&self, session: &Session, action: &str) -> String {
        let (status, card) = self.post(
            session,
            "slot-prepare",
            &json!({"action_token": action, "operation": RELEASE,
                "args": {"conversation_id": CONV}}),
        );
        assert_eq!(status, 200, "HTTP release SLOT prepare: {card}");
        card["confirmation_token"].as_str().unwrap().to_owned()
    }

    fn http_confirm(&self, session: &Session, action: &str, token: &str) -> (u16, Value) {
        self.post(
            session,
            "slot-confirm",
            &json!({"action_token": action, "confirmation_token": token,
                "confirmation": {"confirmed": true}}),
        )
    }
}

/// The daemon's own facts refusal: kind `conflict`, code `facts_changed`.
fn facts_changed(result: crate::Result<Value>, case: &str) {
    match result {
        Ok(value) => panic!("{case}: daemon accepted a stale confirm: {value}"),
        Err(error) => {
            assert_eq!(
                (error.kind(), error.code()),
                ("conflict", Some("facts_changed")),
                "{case}: not the distinct facts refusal: {error}"
            );
            assert!(
                error.to_string().contains("confirmation facts changed"),
                "{case}: refused for the wrong reason: {error}"
            );
        }
    }
}

fn http_facts_changed((status, body): (u16, Value), case: &str) {
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("facts_changed")),
        "{case}: the frame must receive facts_changed: {body}"
    );
}

#[test]
fn cad1656_stale_handback_refused_with_facts_changed_on_rpc_and_http_and_allowlisted() {
    let door = Door::start();
    let fx = Fx::start(&door);
    let (wa, ctx) = fx.install("wa-c1656");
    let local = fx.local_operator();
    let action = fx.mount(&local, &wa, &ctx, 1);
    let owner = fx.owner();
    let owner_action = fx.mount(&owner, &wa, &ctx, 2);
    // Take over through the screen so a release card can be drawn.
    fx.rpc(
        "app_screen_service_call",
        fx.creds(
            &local,
            json!({"action_token": action, "operation": "whatsapp.handoff.set",
                "args": {"conversation_id": CONV}}),
        ),
    )
    .expect("operator screen handoff.set");
    assert!(door.at().is_some(), "fixture BLOCKED: takeover not stamped");
    let before = door.handoffs();

    // Daemon RPC: re-stamped after the card was drawn.
    let token = fx.rpc_card(&local, &action);
    door.restamp();
    facts_changed(
        fx.rpc_confirm(&local, &action, &token),
        "RPC release after handoffAt was re-stamped",
    );
    let replay = fx.rpc_confirm(&local, &action, &token);
    assert!(
        replay
            .as_ref()
            .is_err_and(|e| e.to_string().contains("spent or unknown")),
        "the refused card must stay burned: {replay:?}"
    );

    // HTTP peer: the same refusal reaches the frame as 409 facts_changed.
    let token = fx.http_card(&owner, &owner_action);
    door.restamp();
    http_facts_changed(
        fx.http_confirm(&owner, &owner_action, &token),
        "HTTP release after handoffAt was re-stamped",
    );
    assert_eq!(door.handoffs(), before, "a stale release reached the door");

    // The allowlist: a daemon conflict with any other code reaches the
    // frame as `conflict`, never with its own code.
    for code in [
        "revision_conflict",
        "stale_revision",
        "facts_changed_v2",
        "FACTS_CHANGED",
    ] {
        fx.arm(Some(code));
        let token = fx.rpc_card(&local, &action);
        door.restamp();
        let injected = fx.rpc_confirm(&local, &action, &token);
        assert!(
            injected
                .as_ref()
                .is_err_and(|e| e.kind() == "conflict" && e.code() == Some(code)),
            "fixture BLOCKED: daemon conflict not rewritten to {code}: {injected:?}"
        );
        let token = fx.http_card(&owner, &owner_action);
        door.restamp();
        let (status, body) = fx.http_confirm(&owner, &owner_action, &token);
        assert_eq!(
            (status, body["code"].as_str()),
            (409, Some("conflict")),
            "a daemon conflict coded {code} must reach the frame as conflict: {body}"
        );
        fx.arm(None);
    }
    assert_eq!(door.handoffs(), before, "a stale release reached the door");

    // Cleared elsewhere after the card was drawn: also changed facts.
    let token = fx.rpc_card(&local, &action);
    door.clear();
    facts_changed(
        fx.rpc_confirm(&local, &action, &token),
        "RPC release after handoffAt was cleared",
    );
    door.restamp();
    let token = fx.http_card(&owner, &owner_action);
    door.clear();
    http_facts_changed(
        fx.http_confirm(&owner, &owner_action, &token),
        "HTTP release after handoffAt was cleared",
    );
    assert_eq!(door.handoffs(), before, "a stale release reached the door");
    assert!(
        door.handoffs().iter().all(|(_, handoff)| *handoff),
        "no {{handoff:false}} may reach the door"
    );
}
