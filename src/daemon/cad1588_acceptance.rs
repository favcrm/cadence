//! CAD-1588 / CAD-1634 independent refusal acceptance. Reviewer-written
//! from the tickets, the operator decision of 2026-10-20 (route-only id
//! check; `proto::identifier` unchanged) and the PR #1189 design note
//! (claude-opus-spec-reviewer), not by the implementer. Implementers must
//! not edit or weaken this file.
//!
//! A real daemon (`serve_with` over its own socket) and a real loopback
//! board, with the hosted WhatsApp door injected through
//! `ServeOptions.hosted_whatsapp` and pointed at a loopback stub that keeps
//! the `agenticos.whatsapp-door.v1` semantics. The stub counts every request,
//! so "refused before the door" is an unchanged count, not an error string.
//!
//! (a) A door envelope `grant_revoked` (no `data`) on `reply.send` is
//!     `refused`, final, and never resent; a `grant_revoked` that carries a
//!     bound `data` object is still classed by `data`.
//! (b) Dot-only conversation, reply-to and template ids are refused before
//!     the door.
//! (c) The screen route selector (`record_id`, `segment_id`) admits exactly
//!     `[a-z0-9_-]{1,64}`: `tpl_welcome` and 64 characters pass; `.`, `..`,
//!     `/`, uppercase, empty, 65 characters and non-strings are refused, over
//!     the RPC and the board HTTP peer alike.
//! (d) `proto::identifier` still refuses `_`, so `provider_identifier`
//!     admits only the exact `agenticos_external` name (CAD-709).
#![cfg(feature = "test-seam")]

use crate::platform::hosted_whatsapp::HostedWhatsapp;
use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CONTRACT: &str = "agenticos.whatsapp-door.v1";
const CONVERSATION: &str = "conv_c1588";
const TEXT: &str = "Synthetic CAD-1588 reply.";
const RPC: &str = "app_screen_service_call";
const HTTP: &str = "/api/app-screen-services/call";
const OPS: &[&str] = &[
    "whatsapp.status",
    "whatsapp.conversations.show",
    "whatsapp.reply.prepare",
    "whatsapp.reply.send",
    "whatsapp.reply.status",
    "whatsapp.template.show",
    "screen.route.patch",
];

// ---------- a loopback door with scripted send answers ----------

#[derive(Clone, Copy)]
enum SendAnswer {
    /// 409 envelope `grant_revoked`, no `data`: the door's local unlink check.
    Envelope,
    /// `grant_revoked` code next to a bound `data` with an unknown status.
    UnknownData,
    /// `grant_revoked` code next to a bound, executed `sent` `data`.
    SentData,
}

#[derive(Default)]
struct DoorState {
    hits: Vec<String>,
    staged: HashMap<String, String>,
    send: Option<SendAnswer>,
}

#[derive(Clone)]
struct Door {
    url: String,
    state: Arc<Mutex<DoorState>>,
}

fn canonical_digest(body: &Value) -> String {
    let canonical = format!(
        "{{\"v\":1,\"conversationId\":{},\"text\":{},\"effectId\":{},\"replyToMessageId\":{}}}",
        body["conversationId"],
        body["text"],
        body["effectId"],
        body.get("replyToMessageId").cloned().unwrap_or(Value::Null),
    );
    format!("{:x}", Sha256::digest(canonical.as_bytes()))
}

impl Door {
    fn start() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr().to_ip().unwrap());
        let state = Arc::new(Mutex::new(DoorState::default()));
        let door = Self {
            url,
            state: state.clone(),
        };
        std::thread::spawn(move || {
            for mut request in server.incoming_requests() {
                let mut raw = String::new();
                let _ = request.as_reader().read_to_string(&mut raw);
                let body: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                let key = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("idempotency-key"))
                    .map(|h| h.value.to_string());
                let path = request.url().split('?').next().unwrap_or("").to_string();
                let (code, answer) = answer(&state, &path, key, &body);
                let _ = request.respond(
                    tiny_http::Response::from_data(answer.to_string().into_bytes())
                        .with_status_code(code),
                );
            }
        });
        door
    }
    fn count(&self) -> usize {
        self.state.lock().unwrap().hits.len()
    }
    fn sends(&self) -> usize {
        let state = self.state.lock().unwrap();
        state
            .hits
            .iter()
            .filter(|p| p.ends_with("/reply/send"))
            .count()
    }
    fn script(&self, send: SendAnswer) {
        self.state.lock().unwrap().send = Some(send);
    }
}

fn answer(state: &Mutex<DoorState>, path: &str, key: Option<String>, body: &Value) -> (u16, Value) {
    let mut door = state.lock().unwrap();
    door.hits.push(path.to_string());
    let ok = |data: Value| (200, json!({"ok": true, "data": data}));
    let refuse = |code: u16, what: &str| {
        (
            code,
            json!({"ok": false, "error": {"code": what, "message": what}}),
        )
    };
    let rest = path.strip_prefix("/v1/runtime/whatsapp").unwrap_or("");
    match rest {
        p if p == format!("/conversations/{CONVERSATION}") => ok(json!({
            "contract": CONTRACT, "conversationId": CONVERSATION, "status": "open"})),
        "/reply/prepare" => {
            let Some(key) = key else {
                return refuse(400, "invalid_request");
            };
            let digest = canonical_digest(body);
            door.staged.insert(key.clone(), digest.clone());
            ok(
                json!({"contract": CONTRACT, "key": key, "digest": digest, "decision": "granted",
                "replyMode": "freeform", "freeformUntil": "2030-01-01T00:00:00.000Z",
                "expiresAt": "2030-01-01T00:00:00.000Z"}),
            )
        }
        "/reply/send" => {
            let Some(key) = key else {
                return refuse(400, "invalid_request");
            };
            let revoked =
                json!({"code": "grant_revoked", "message": "The FavCRM link was revoked."});
            match door.send {
                Some(SendAnswer::Envelope) => (409, json!({"ok": false, "error": revoked})),
                Some(SendAnswer::UnknownData) => (
                    409,
                    json!({"ok": false, "error": revoked, "data": {"contract": CONTRACT,
                        "key": key, "decision": "granted", "status": "in_flight"}}),
                ),
                Some(SendAnswer::SentData) => (
                    409,
                    json!({"ok": false, "error": revoked, "data": {"contract": CONTRACT,
                        "key": key, "decision": "granted", "status": "sent", "executed": true,
                        "providerMessageId": "wamid.c1588", "favcrmMessageId": "fm_c1588"}}),
                ),
                None => refuse(409, "not_prepared"),
            }
        }
        _ => refuse(404, "not_found"),
    }
}

// ---------- one daemon + loopback board ----------

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

// Deliberately no Debug: synthetic credentials must not enter failure output.
struct Session {
    cookie: String,
    key: String,
    token: String,
}

struct Fx {
    // Field order: stop the servers before the temp state is removed.
    _servers: Servers,
    root: tempfile::TempDir,
    agent: ureq::Agent,
    base: String,
    host: String,
    seam: String,
}

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

impl Fx {
    fn start(door: &Door) -> Self {
        assert!(
            std::env::var_os("CADENCE_SOCKET").is_none(),
            "fixture BLOCKED: inherited socket override"
        );
        let root = tempfile::Builder::new().prefix("c1588").tempdir().unwrap();
        let state = root.path().to_path_buf();
        let pm = state.join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let mut servers = Servers {
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
        };
        let opts = crate::daemon::ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            test_seam: true,
            stop: Some(servers.stop.clone()),
            report_router: Some(0),
            checkup: Some(0),
            hosted_whatsapp: Some(HostedWhatsapp::new(&door.url).unwrap()),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let daemon_state = state.clone();
        servers.threads.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap();
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
            assert!(Instant::now() < deadline, "fixture BLOCKED: daemon start");
            assert!(
                !servers.threads[0].is_finished(),
                "fixture BLOCKED: daemon exited"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        // Own bind proof from the startup channel; 3162 is reserved on this host.
        let first = (std::process::id() % 90) as u16;
        let mut port = None;
        for offset in 0..90 {
            let candidate = 3110 + (first + offset) % 90;
            if candidate == 3162 {
                continue;
            }
            let (ready, rx) = std::sync::mpsc::channel();
            let opts = crate::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port: candidate,
                stop: Some(servers.stop.clone()),
                startup: Some(ready),
                test_seam: true,
                ..Default::default()
            };
            let (board_state, board_pm) = (state.clone(), pm.clone());
            let board = std::thread::spawn(move || {
                drop(crate::ui::serve(&board_state, &board_pm, &opts));
            });
            match rx.recv_timeout(Duration::from_secs(20)) {
                Ok(Ok(())) => {
                    servers.threads.push(board);
                    port = Some(candidate);
                    break;
                }
                Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                    let _ = board.join();
                }
                other => {
                    servers.threads.push(board);
                    panic!("fixture BLOCKED: board startup failed: {other:?}");
                }
            }
        }
        let port = port.expect("fixture BLOCKED: no isolated board port in 3110-3199");
        let seam = Seam::token_at(&state).unwrap();
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .build()
            .into();
        Self {
            _servers: servers,
            root,
            agent,
            base: format!("http://127.0.0.1:{port}"),
            host: format!("cadence-{port}.localhost:{port}"),
            seam,
        }
    }

    fn rpc(&self, method: &str, params: Value) -> crate::Result<Value> {
        scoped(Asserted::Operator, || {
            crate::client::rpc_timeout(self.root.path(), method, params, Duration::from_secs(20))
        })
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(method, params)
            .unwrap_or_else(|error| panic!("fixture BLOCKED: operator RPC {method}: {error}"))
    }

    fn post(&self, session: Option<&Session>, path: &str, body: &Value) -> (u16, Value) {
        let mut request = self
            .agent
            .post(format!("{}{path}", self.base))
            .header("Host", &self.host)
            .header("Origin", format!("http://{}", self.host))
            .header("X-Cadence-Board", "1")
            .header(crate::test_seam::AS_HEADER, "operator")
            .header(crate::test_seam::TOKEN_HEADER, &self.seam)
            .header("Content-Type", "application/json");
        if let Some(session) = session {
            request = request
                .header("Cookie", &session.cookie)
                .header("X-Cadence-Session", &session.key);
        }
        let response = request
            .send(body.to_string())
            .expect("owned HTTP transport failed");
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body = response.into_body().read_json().unwrap_or(Value::Null);
        if path == "/api/session" {
            let cookie = headers["set-cookie"]
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned();
            return (status, json!({"cookie": cookie, "body": body}));
        }
        (status, body)
    }

    fn session(&self) -> Session {
        crate::operator_auth::ensure_secret(self.root.path()).unwrap();
        let secret = crate::operator_auth::read_secret(self.root.path()).unwrap();
        let nonce = self.op(
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )["nonce"]
            .clone();
        let (status, opened) = self.post(None, "/api/session", &json!({"nonce": nonce}));
        assert_eq!(status, 200, "fixture BLOCKED: session bootstrap");
        let cookie = opened["cookie"].as_str().unwrap().to_owned();
        let token = cookie.split_once('=').unwrap().1.to_owned();
        let key = opened["body"]["session_key"]
            .as_str()
            .expect("fixture BLOCKED: page key")
            .to_owned();
        Session { cookie, key, token }
    }

    /// The operator's real, consented install with one navigable screen.
    fn install(&self) -> (String, String) {
        let app = "wa-c1588";
        let source = self.root.path().join("source");
        let screen = source.join("screens/main");
        std::fs::create_dir_all(&screen).unwrap();
        let manifest = format!("---\napp: {app}\ntitle: Synthetic WhatsApp\nversion: '0.1.0'\nsummary: Isolated CAD1588 refusal fixture\nneeds:\n  connections: []\n  requires:\n    contracts:\n      app-screens: [1]\n      screen-records: [1]\n---\n\nSynthetic only.\n");
        std::fs::write(source.join("app.md"), manifest).unwrap();
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(source.join("workflows/synthetic.md"), "---\ntitle: Synthetic refusal fixture\ngoal: Synthetic text only\ninputs:\n  writer: { ask: Synthetic writer }\n  reviewer: { ask: Synthetic reviewer }\ndistinct: [writer, reviewer]\n---\n\n## Synthetic draft\nagent: {{writer}}\nsize: S\naction: local.text.produce\n\nProduce synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n\n## Synthetic review\nagent: {{reviewer}}\nsize: S\ndepends_on: 1\naction: local.text.review\n\nReview the synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n").unwrap();
        let js = "(()=>{document.getElementById('root').textContent='c1588';})();";
        std::fs::write(screen.join("client.js"), js).unwrap();
        let declaration = json!({
            "contract": "app-screens/v2", "app": app, "entry": "client.js",
            "host_contract": "screen-records.v1", "may": ["records.call", "records.slot"],
            "operations": OPS, "tools": {},
            "navigation": {"section_query": "tab", "default_section": "inbox",
                "sections": [{"id": "inbox", "label": "Inbox"}]},
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
                "input_defaults": {}, "request_id": "c1588-context"}),
        )["context"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        (install, context)
    }

    fn mount(&self, session: &Session, install: &str, context: &str) -> String {
        let mint = self.op(
            "app_screen_mint",
            json!({"install_id": install, "context_id": context, "tag": "main",
                "generation": 1, "token": session.token, "key": session.key,
                "origin": "loopback"}),
        );
        let nonce = mint["mount"].as_str().unwrap().rsplit('/').next().unwrap();
        self.op("app_screen_consume", json!({"nonce": nonce}));
        mint["action_token"]
            .as_str()
            .expect("fixture BLOCKED: mount action token")
            .to_owned()
    }

    fn route(&self, session: &Session, action: &str, args: Value) -> crate::Result<Value> {
        self.rpc(
            RPC,
            json!({"action_token": action, "operation": "screen.route.patch", "args": args,
                "token": session.token, "key": session.key, "origin": "loopback"}),
        )
    }
}

/// A refusal is guard evidence only when it is the daemon's own rejection
/// carrying the named reason, never a transport or internal failure.
fn refused_by(result: crate::Result<Value>, reason: &str, case: &str) {
    match result {
        Ok(value) => panic!("{case}: daemon accepted a prohibited request: {value}"),
        Err(error) => {
            assert_eq!(
                error.kind(),
                "rejected",
                "{case}: not a guard refusal: {error}"
            );
            assert!(
                error.to_string().contains(reason),
                "{case}: refused for the wrong reason: {error}"
            );
        }
    }
}

#[test]
fn cad1588_revoked_grant_dot_ids_and_route_selector_refusals() {
    // (d) The shared identifier stays strict; only the route check widened.
    assert!(crate::proto::identifier("tpl_welcome", "id").is_err());
    assert!(crate::proto::identifier("tpl-welcome", "id").is_ok());
    let platform = crate::platform::agenticos_external::PLATFORM;
    assert_eq!(
        crate::platform::connections::provider_identifier(platform, "provider").unwrap(),
        platform
    );
    for lookalike in ["agenticos_externa1", "agenticos_internal", "a_b"] {
        assert!(
            crate::platform::connections::provider_identifier(lookalike, "provider").is_err(),
            "provider_identifier admitted an underscore lookalike {lookalike}"
        );
    }

    let door = Door::start();
    let fx = Fx::start(&door);
    let (install, context) = fx.install();
    let scope = |extra: Value| {
        let mut body = json!({"install_id": install, "context_id": context});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        body
    };

    // (b) Dot-only ids never reach the door, on every path-id check.
    let positive = fx.op(
        "whatsapp_conversations_show",
        scope(json!({"conversation_id": CONVERSATION})),
    );
    assert_eq!(positive["conversationId"], CONVERSATION, "{positive}");
    let before = door.count();
    for dots in [".", "..", "...", "...."] {
        refused_by(
            fx.rpc(
                "whatsapp_conversations_show",
                scope(json!({"conversation_id": dots})),
            ),
            "conversation id is invalid",
            &format!("conversation show {dots:?}"),
        );
        refused_by(
            fx.rpc(
                "whatsapp_reply_prepare",
                scope(json!({"conversation_id": dots, "text": TEXT})),
            ),
            "conversation id is invalid",
            &format!("reply prepare to {dots:?}"),
        );
        refused_by(
            fx.rpc(
                "whatsapp_reply_prepare",
                scope(json!({"conversation_id": CONVERSATION, "text": TEXT,
                    "reply_to_message_id": dots})),
            ),
            "reply-to message id is invalid",
            &format!("reply-to {dots:?}"),
        );
        refused_by(
            fx.rpc(
                "whatsapp_template_show",
                scope(json!({"template_id": dots})),
            ),
            "template id is invalid",
            &format!("template show {dots:?}"),
        );
    }
    assert_eq!(door.count(), before, "a dot-only id reached the door");

    // (a) A door envelope `grant_revoked` is refused, final, never resent.
    let prepare = || {
        let prepared = fx.op(
            "whatsapp_reply_prepare",
            scope(json!({"conversation_id": CONVERSATION, "text": TEXT})),
        );
        (
            prepared["effect_id"].as_str().unwrap().to_owned(),
            prepared["digest"].as_str().unwrap().to_owned(),
        )
    };
    let (effect, digest) = prepare();
    door.script(SendAnswer::Envelope);
    let sent = fx.op(
        "whatsapp_reply_send",
        scope(json!({"effect_id": effect, "digest": digest})),
    );
    assert_eq!(sent["reply"]["state"], "refused", "{sent}");
    assert_eq!(sent["reply"]["code"], "grant_revoked", "{sent}");
    assert_eq!(door.sends(), 1);
    let status = fx.op("whatsapp_reply_status", scope(json!({"effect_id": effect})));
    assert_eq!(status["state"], "refused", "{status}");
    refused_by(
        fx.rpc(
            "whatsapp_reply_send",
            scope(json!({"effect_id": effect, "digest": digest})),
        ),
        "already attempted",
        "a revoked-grant reply resent",
    );
    assert_eq!(door.sends(), 1, "a refused reply reached /reply/send again");

    // The code never outranks a bound `data`: an unknown status stays
    // uncertain, an executed `sent` stays sent.
    let (effect, digest) = prepare();
    door.script(SendAnswer::UnknownData);
    let sent = fx.op(
        "whatsapp_reply_send",
        scope(json!({"effect_id": effect, "digest": digest})),
    );
    assert_eq!(sent["reply"]["state"], "uncertain", "{sent}");
    let (effect, digest) = prepare();
    door.script(SendAnswer::SentData);
    let sent = fx.op(
        "whatsapp_reply_send",
        scope(json!({"effect_id": effect, "digest": digest})),
    );
    assert_eq!(sent["reply"]["state"], "sent", "{sent}");
    assert_eq!(door.sends(), 3);

    // (c) The route selector grammar, over RPC and the board HTTP peer.
    let session = fx.session();
    let action = fx.mount(&session, &install, &context);
    let max = "a".repeat(64);
    let before = door.count();
    for key in ["record_id", "segment_id"] {
        for good in ["tpl_welcome", "wab_0123-x", max.as_str()] {
            let patched = fx
                .route(&session, &action, json!({key: good}))
                .unwrap_or_else(|error| panic!("route {key}={good:?} refused: {error}"));
            assert_eq!(
                patched["host_command"]["args"][key],
                json!(good),
                "{patched}"
            );
        }
        let long = "a".repeat(65);
        for bad in [
            ".",
            "..",
            "/",
            "a/b",
            "../x",
            "Tpl_welcome",
            "TPL",
            "",
            long.as_str(),
            "tpl welcome",
            "tpl.welcome",
            "tpl%2f",
        ] {
            refused_by(
                fx.route(&session, &action, json!({key: bad})),
                "invalid screen selector",
                &format!("route {key}={bad:?}"),
            );
        }
        for not_string in [json!(7), json!(true), json!(["tpl"]), json!({"id": "tpl"})] {
            let result = fx.route(&session, &action, json!({key: not_string}));
            match result {
                Ok(value) => panic!("route {key}={not_string} accepted: {value}"),
                Err(error) => assert_eq!(error.kind(), "rejected", "{error}"),
            }
        }
    }
    // The HTTP peer is the same guard: refused with 4xx, admitted with 200.
    let http = |args: Value| {
        fx.post(
            Some(&session),
            HTTP,
            &json!({"action_token": action, "operation": "screen.route.patch", "args": args}),
        )
    };
    let long = "a".repeat(65);
    for bad in ["..", "/", "TPL", "", long.as_str()] {
        let (status, answer) = http(json!({"record_id": bad}));
        assert_eq!(status, 400, "HTTP route record_id={bad:?}: {answer}");
    }
    let (status, answer) = http(json!({"record_id": "tpl_welcome"}));
    assert_eq!(status, 200, "HTTP route tpl_welcome: {answer}");
    assert_eq!(answer["host_command"]["args"]["record_id"], "tpl_welcome");
    assert_eq!(door.count(), before, "a route patch reached the door");
}
