//! CAD-1636 independent refusal acceptance. Reviewer-written from the
//! ticket and the approved CAD-1465 step-3 design (§3 H1-H3, §4, §8
//! slice 1) by claude-opus-spec-reviewer, not by the implementer.
//! Implementers must not edit or weaken this file.
//!
//! A real daemon (`Shared` over its own socket) and a real public board,
//! with the hosted WhatsApp door injected through
//! `ServeOptions.hosted_whatsapp` and pointed at a loopback double of
//! `agenticos.whatsapp-door.v1`. The double serves any conversation id,
//! ignores the page limit and counts every request, so "refused before the
//! door" is an unchanged count and "never sends" is zero `/reply/*` hits.
//!
//! Refused:
//! (a) the master or another agent: every native `whatsapp_*` verb and the
//!     screen `whatsapp.reply.prepare`; a door verb smuggled as an
//!     assistant action id;
//! (b) `whatsapp.conversation.context` / `whatsapp.reply.draft` for a
//!     conversation other than the turn's subject, and from a General chat;
//! (c) an install whose consented descriptor does not declare the action;
//! (d) a member's `chat.whatsapp.brief`, `chat.whatsapp.open` and
//!     `whatsapp.draft.discard`, over RPC and over the board's HTTP peer,
//!     while the member's `whatsapp.draft.show` is allowed;
//! (e) a self-hosted daemon: the subject, every screen op, both actions;
//! (f) a context response over the §4 bound is trimmed on the host;
//! (g) a brief confirm whose guidance differs from the card, or a bare tap;
//! (h) the assistant can never send: `reply.draft` makes no door request
//!     and the door counts zero `/reply/*` requests at the end.
//! Positive: brief SLOT -> the master's subject turn reads the bounded
//! context -> parks one draft -> the operator and a member see it ->
//! the operator discards it.
#![cfg(feature = "test-seam")]

use crate::daemon::{serve, ServeOptions, Shared};
use crate::operator_auth::{BoardUser, Origin, PublicRealm};
use crate::platform::hosted_whatsapp::HostedWhatsapp;
use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const COMPANY: &str = "co_c1636";
const CONTRACT: &str = "agenticos.whatsapp-door.v1";
const CONV: &str = "conv_c1636";
const OTHER: &str = "conv_other1636";
const GENERATION: &str = "0123456789abcdef0123456789abcdef";
const MODEL: &str = "fixture/model";
const GUIDANCE: &str = "Offer a table at 7, keep it short.";
const SELF_HOSTED: &str = "runs only on a hosted AgenticOS workspace";
const READ_ONLY: &str = "read-only screen admits only read operations";
const NOT_SUBJECT: &str = "serve only this chat's own WhatsApp conversation";
const NOT_DECLARED: &str = "not declared by this consented installation";
// `operator_connection` (any agent) or the app-turn verb scope (the master).
const AGENT_REFUSED: &str = "is an operator action|may call only the scoped app verbs";
const COPILOT_OPS: &[&str] = &[
    "chat.whatsapp.open",
    "chat.whatsapp.brief",
    "whatsapp.draft.show",
    "whatsapp.draft.discard",
];
const DOOR_OPS: &[&str] = &[
    "whatsapp.status",
    "whatsapp.conversations.show",
    "whatsapp.messages",
    "whatsapp.reply.prepare",
    "whatsapp.reply.send",
    "whatsapp.reply.status",
];
// Sentinels that must never reach the model.
const TOO_OLD: &str = "SENTINEL-older-than-seven-days";
const UNDATED: &str = "SENTINEL-undated";
const MEDIA_URL: &str = "https://cdn.example.test/SENTINEL-media.jpg";
const TEMPLATE_BODY: &str = "SENTINEL-template-body";
const EMAIL: &str = "sentinel-diner@example.test";
const WA_ID: &str = "85255551234";

// ---------- a loopback door double ----------

#[derive(Clone)]
struct Door {
    url: String,
    hits: Arc<Mutex<Vec<String>>>,
}

impl Door {
    fn start() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr().to_ip().unwrap());
        let hits = Arc::new(Mutex::new(Vec::new()));
        let log = hits.clone();
        std::thread::spawn(move || {
            for mut request in server.incoming_requests() {
                let mut raw = String::new();
                let _ = request.as_reader().read_to_string(&mut raw);
                let path = request.url().split('?').next().unwrap_or("").to_string();
                log.lock().unwrap().push(path.clone());
                let (code, body) = answer(&path);
                let _ = request.respond(
                    tiny_http::Response::from_data(body.to_string().into_bytes())
                        .with_status_code(code),
                );
            }
        });
        Self { url, hits }
    }
    fn count(&self) -> usize {
        self.hits.lock().unwrap().len()
    }
    fn effects(&self) -> usize {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.contains("/reply/"))
            .count()
    }
}

fn iso(ago_minutes: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::minutes(ago_minutes))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// More than the bound, unsorted, in the door's own v1 shape.
fn door_messages() -> Value {
    let mut rows = Vec::new();
    for i in 0..30 {
        rows.push(
            json!({"id": format!("msg_{i:02}"), "providerMessageId": null,
            "direction": if i % 2 == 0 { "inbound" } else { "outbound" }, "type": "text",
            "body": format!("{i:02}-{}", "y".repeat(1400)), "status": "delivered",
            "senderType": "contact", "createdAt": iso(60 + i * 3)}),
        );
    }
    rows.push(
        json!({"id": "msg_old", "providerMessageId": null, "direction": "inbound",
        "type": "text", "body": TOO_OLD, "status": null, "senderType": "contact",
        "createdAt": iso(7 * 24 * 60 + 60)}),
    );
    rows.push(
        json!({"id": "msg_undated", "providerMessageId": null, "direction": "inbound",
        "type": "text", "body": UNDATED, "status": null, "senderType": "contact",
        "createdAt": null}),
    );
    rows.push(
        json!({"id": "msg_pic", "providerMessageId": null, "direction": "inbound",
        "type": "image", "body": MEDIA_URL, "status": null, "senderType": "contact",
        "createdAt": iso(2)}),
    );
    rows.push(
        json!({"id": "msg_tpl", "providerMessageId": null, "direction": "outbound",
        "type": "template", "body": TEMPLATE_BODY, "status": null, "senderType": "agent",
        "createdAt": iso(1)}),
    );
    rows.push(
        json!({"id": "msg_new", "providerMessageId": null, "direction": "inbound",
        "type": "text", "body": "Newest: can we book for four?", "status": null,
        "senderType": "contact", "createdAt": iso(0)}),
    );
    rows.reverse();
    rows.swap(3, 20);
    json!({"contract": CONTRACT, "messages": rows, "cursor": null})
}

fn answer(path: &str) -> (u16, Value) {
    let ok = |data: Value| (200, json!({"ok": true, "data": data}));
    let rest = path.strip_prefix("/v1/runtime/whatsapp").unwrap_or("");
    if let Some(conv) = rest.strip_prefix("/conversations/") {
        if let Some(conv) = conv.strip_suffix("/messages") {
            let _ = conv;
            return ok(door_messages());
        }
        return ok(json!({"contract": CONTRACT, "conversationId": conv,
            "channelConnectionId": "cc_1", "status": "open",
            "contact": {"id": "ct_1", "name": "Synthetic Diner", "phone": "+852 5555 1234",
                "email": EMAIL, "whatsappId": WA_ID},
            "replyMode": "freeform", "freeformUntil": "2030-01-01T00:00:00.000Z",
            "lastInboundAt": null, "lastOutboundAt": null, "unreadCount": 1, "handoffAt": null}));
    }
    if rest == "/status" {
        return ok(json!({"contract": CONTRACT, "connected": true,
            "displayPhoneNumber": "+852 0000 0000", "verifiedName": "Synthetic Co",
            "wabaId": "w_1", "phoneNumberId": "p_1", "channelConnectionId": "cc_1",
            "webhook": {"subscribed": true, "status": null, "error": null, "fields": []},
            "observedAt": "2026-10-20T00:00:00.000Z"}));
    }
    // Any /reply/* would be a send path: answer, but it is counted.
    (
        409,
        json!({"ok": false, "error": {"code": "unexpected", "message": "unexpected"}}),
    )
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
    _servers: Servers,
    shared: Arc<Shared>,
    root: tempfile::TempDir,
    agent: ureq::Agent,
    base: String,
    host: String,
    seam: String,
    issuer: String,
}

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

impl Fx {
    fn start(door: Option<&Door>) -> Self {
        assert!(
            std::env::var_os("CADENCE_SOCKET").is_none(),
            "fixture BLOCKED: inherited socket override"
        );
        let root = tempfile::Builder::new().prefix("c1636").tempdir().unwrap();
        let state = root.path().to_path_buf();
        let pm = state.join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let (port, reservation) = (3110..3200)
            .find_map(|port| {
                std::net::TcpListener::bind(("127.0.0.1", port))
                    .ok()
                    .map(|listener| (port, listener))
            })
            .expect("fixture BLOCKED: no isolated board port in 3110-3199");
        let host = format!("c1636.board.localhost:{port}");
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
            hosted_whatsapp: door.map(|d| HostedWhatsapp::new(&d.url).unwrap()),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = Shared::new(&state, &opts).unwrap();
        let cwd = state.to_str().unwrap();
        for (alias, provider, kind) in [
            ("master", "pi", "managed"),
            ("synthetic-worker", "fake", "fake"),
        ] {
            shared
                .store
                .register_agent(&crate::store::NewAgent {
                    alias,
                    provider,
                    endpoint_kind: kind,
                    role: "worker",
                    cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: (alias == "master").then_some(r#"{"model":"fixture/model"}"#),
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        crate::test_seam::seed_pi_fixture_policy(&shared.store, MODEL).unwrap();
        shared
            .store
            .set_identity_with_quota(
                "master",
                &crate::adapter::Identity {
                    thread_id: "c1636-thread".into(),
                    session_id: "c1636-session".into(),
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
        let mut servers = Servers {
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
        };
        let listener = UnixListener::bind(state.join("cadence.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (stop, dispatch) = (servers.stop.clone(), shared.clone());
        servers.threads.push(std::thread::spawn(move || {
            let mut connections = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let dispatch = dispatch.clone();
                        connections.push(std::thread::spawn(move || {
                            serve::handle_conn(dispatch, stream)
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => panic!("owned RPC listener failed"),
                }
            }
            for connection in connections {
                let _ = connection.join();
            }
        }));
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
        }
    }

    fn rpc(&self, who: Asserted, method: &str, params: Value) -> crate::Result<Value> {
        scoped(who, || {
            crate::client::rpc_timeout(self.root.path(), method, params, Duration::from_secs(20))
        })
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|error| panic!("operator RPC {method} failed: {error}"))
    }

    /// A consented install whose screen declares the door and copilot
    /// operations and whose `app-assistant.json` declares `actions`
    /// (each with the registry's exact schema), plus one context.
    fn install(&self, app: &str, actions: &[&str]) -> (String, String) {
        let source = self.root.path().join(format!("source-{app}"));
        let screen = source.join("screens/main");
        std::fs::create_dir_all(&screen).unwrap();
        let manifest = format!("---\napp: {app}\ntitle: Synthetic Copilot\nversion: '0.1.0'\nsummary: Isolated CAD1636 refusal fixture\nneeds:\n  connections: []\n  requires:\n    contracts:\n      app-screens: [1]\n      screen-records: [1]\n---\n\nSynthetic only.\n");
        std::fs::write(source.join("app.md"), manifest).unwrap();
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(source.join("workflows/synthetic.md"), "---\ntitle: Synthetic refusal fixture\ngoal: Synthetic text only\ninputs:\n  writer: { ask: Synthetic writer }\n  reviewer: { ask: Synthetic reviewer }\ndistinct: [writer, reviewer]\n---\n\n## Synthetic draft\nagent: {{writer}}\nsize: S\naction: local.text.produce\n\nProduce synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n\n## Synthetic review\nagent: {{reviewer}}\nsize: S\ndepends_on: 1\naction: local.text.review\n\nReview the synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n").unwrap();
        let descriptor: Vec<Value> = actions
            .iter()
            .map(|id| {
                let meta = crate::app_assistant::registered_action(id).unwrap();
                json!({"id": id, "description": "Synthetic WhatsApp copilot action.",
                    "input_schema": meta.input_schema, "effect": meta.effect,
                    "confirmation": meta.confirmation, "availability": "requires_context"})
            })
            .collect();
        std::fs::write(
            source.join("app-assistant.json"),
            json!({"contract": "app-assistant/v1", "app": app, "actions": descriptor}).to_string(),
        )
        .unwrap();
        let js = "(()=>{document.getElementById('root').textContent='c1636';})();";
        std::fs::write(screen.join("client.js"), js).unwrap();
        let operations: Vec<&str> = DOOR_OPS.iter().chain(COPILOT_OPS).copied().collect();
        let declaration = json!({
            "contract": "app-screens/v2", "app": app, "entry": "client.js",
            "host_contract": "screen-records.v1", "may": ["records.call", "records.slot"],
            "operations": operations, "tools": {},
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
            "fixture BLOCKED: install consent for {app}: {installed}"
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

    fn member(&self) -> Session {
        let now = self.shared.operator_now();
        let opened = self
            .shared
            .operator_auth()
            .open_public_bound(
                BoardUser {
                    sub: "synthetic-c1636-member".into(),
                    email: "c1636-member@example.test".into(),
                    name: "Synthetic member".into(),
                    role: "member".into(),
                    handle: "c1636-member".into(),
                },
                "c1636-member-jti",
                now + 120,
                "c1636-isolated-acceptance",
                PublicRealm {
                    issuer: self.issuer.clone(),
                    company: COMPANY.into(),
                    host: self.host.clone(),
                },
                now,
            )
            .unwrap()
            .expect("fixture BLOCKED: fresh member jti");
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
            .rpc(
                Asserted::Operator,
                "app_screen_mint",
                self.creds(session, body),
            )
            .unwrap_or_else(|error| panic!("fixture BLOCKED: mint: {error}"));
        let nonce = mint["mount"].as_str().unwrap().rsplit('/').next().unwrap();
        self.op("app_screen_consume", json!({"nonce": nonce}));
        mint["action_token"]
            .as_str()
            .expect("fixture BLOCKED: mount action token")
            .to_owned()
    }

    fn service(
        &self,
        who: Asserted,
        method: &str,
        session: &Session,
        action: &str,
        operation: &str,
        args: Value,
    ) -> crate::Result<Value> {
        self.rpc(
            who,
            method,
            self.creds(
                session,
                json!({"action_token": action, "operation": operation, "args": args}),
            ),
        )
    }

    fn call(
        &self,
        session: &Session,
        action: &str,
        operation: &str,
        args: Value,
    ) -> crate::Result<Value> {
        self.service(
            Asserted::Operator,
            "app_screen_service_call",
            session,
            action,
            operation,
            args,
        )
    }

    fn slot(
        &self,
        session: &Session,
        action: &str,
        operation: &str,
        args: Value,
    ) -> crate::Result<Value> {
        self.service(
            Asserted::Operator,
            "app_screen_service_slot_prepare",
            session,
            action,
            operation,
            args,
        )
    }

    fn confirm(
        &self,
        session: &Session,
        action: &str,
        token: &str,
        confirmation: Value,
    ) -> crate::Result<Value> {
        self.rpc(
            Asserted::Operator,
            "app_screen_service_slot_confirm",
            self.creds(
                session,
                json!({"action_token": action, "confirmation_token": token,
                    "confirmation": confirmation}),
            ),
        )
    }

    fn post(&self, session: &Session, path: &str, body: &Value) -> (u16, Value) {
        let response = self
            .agent
            .post(format!("{}{path}", self.base))
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

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(crate::rollout::db_file(self.root.path())).unwrap()
    }

    /// Make the queued `message` the master's running turn; its token.
    fn run(&self, message: &str) -> String {
        let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(GENERATION);
        self.db()
            .execute(
                "UPDATE messages SET state='submitting', started=1.0 WHERE id=?",
                [message],
            )
            .unwrap();
        self.shared.store.mark_running(message, &token).unwrap();
        token
    }

    fn finish(&self, message: &str) {
        self.db()
            .execute(
                "UPDATE messages SET state='completed' WHERE id=?",
                [message],
            )
            .unwrap();
    }

    /// A General-chat turn of the master in this install and context.
    fn general_turn(&self, install: &str, context: &str, message: &str) -> String {
        self.op(
            "conversation_create",
            json!({"alias": "master", "install_id": install, "context_id": context, "general": true}),
        );
        self.op(
            "thread_send",
            json!({"alias": "master", "text": "General chat", "message": message,
                "app": {"install_id": install, "context_id": context}}),
        );
        self.run(message)
    }

    /// A subject-chat turn (`whatsapp:<conversation>`) of the master.
    fn subject_turn(
        &self,
        install: &str,
        context: &str,
        conversation: &str,
        message: &str,
    ) -> String {
        let made = self.op(
            "conversation_create",
            json!({"alias": "master", "install_id": install, "context_id": context,
                "subject": format!("whatsapp:{conversation}")}),
        );
        self.op(
            "thread_send",
            json!({"alias": "master", "text": "Draft a reply", "message": message,
                "app": {"install_id": install, "context_id": context},
                "conversation": made["conversation"]["id"]}),
        );
        self.run(message)
    }

    #[allow(clippy::too_many_arguments)]
    fn invoke(
        &self,
        who: Asserted,
        install: &str,
        context: &str,
        message: &str,
        token: &str,
        action: &str,
        operation: &str,
        input: Value,
    ) -> crate::Result<Value> {
        self.rpc(
            who,
            "app_assistant_invoke",
            json!({"install_id": install, "context_id": context, "message": message,
                "token": token, "action_id": action, "operation_id": operation, "input": input}),
        )
    }

    fn drafts(&self) -> usize {
        std::fs::read_dir(self.root.path().join("whatsapp-drafts"))
            .map(|dir| dir.flatten().count())
            .unwrap_or(0)
    }
}

/// A refusal counts only as the daemon's own rejection with the reason.
fn refused_by(result: crate::Result<Value>, reason: &str, case: &str) {
    match result {
        Ok(value) => panic!("{case}: daemon accepted a prohibited request: {value}"),
        Err(error) => {
            assert!(
                matches!(error.kind(), "rejected" | "gate"),
                "{case}: not a guard refusal (kind {}): {error}",
                error.kind()
            );
            assert!(
                reason.split('|').any(|r| error.to_string().contains(r)),
                "{case}: refused for the wrong reason: {error}"
            );
        }
    }
}

fn native_verbs(install: &str, context: &str) -> Vec<(&'static str, Value)> {
    let scope = |extra: Value| {
        let mut body = json!({"install_id": install, "context_id": context});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        body
    };
    let effect = format!("wae_{}", "0".repeat(32));
    vec![
        ("whatsapp_status", scope(json!({}))),
        ("whatsapp_connect_session", scope(json!({}))),
        ("whatsapp_conversations_list", scope(json!({"limit": 10}))),
        (
            "whatsapp_conversations_show",
            scope(json!({"conversation_id": CONV})),
        ),
        ("whatsapp_messages", scope(json!({"conversation_id": CONV}))),
        (
            "whatsapp_reply_prepare",
            scope(json!({"conversation_id": CONV, "text": "Hi"})),
        ),
        (
            "whatsapp_reply_send",
            scope(json!({"effect_id": effect, "digest": "a".repeat(64)})),
        ),
        ("whatsapp_reply_status", scope(json!({"effect_id": effect}))),
    ]
}

fn draft_input(conversation: &str) -> Value {
    json!({"conversation_id": conversation, "draft_id": "drf_c1636",
        "text": "Hi! A table for four at 7 works. See you then.",
        "basis_message_id": "msg_new"})
}

#[test]
fn cad1636_whatsapp_copilot_refuses_agents_other_subjects_undeclared_members_and_self_hosted() {
    let door = Door::start();
    let fx = Fx::start(Some(&door));
    let both = ["whatsapp.conversation.context", "whatsapp.reply.draft"];
    let (wa, ctx) = fx.install("wa-c1636", &both);
    let (lite, lite_ctx) = fx.install("wa-lite-c1636", &["whatsapp.conversation.context"]);
    let operator = fx.local_operator();
    let action = fx.mount(&operator, &wa, &ctx, 1);

    // (a) The master and another agent: every native whatsapp_* verb and
    // the screen reply.prepare, before the door.
    let before = door.count();
    for who in ["master", "synthetic-worker"] {
        for (method, params) in native_verbs(&wa, &ctx) {
            refused_by(
                fx.rpc(Asserted::Agent(who.into()), method, params),
                AGENT_REFUSED,
                &format!("agent {who} {method}"),
            );
        }
        assert!(
            fx.service(
                Asserted::Agent(who.into()),
                "app_screen_service_call",
                &operator,
                &action,
                "whatsapp.reply.prepare",
                json!({"conversation_id": CONV, "text": "Hi"}),
            )
            .is_err(),
            "agent {who} screen reply.prepare was accepted"
        );
        for op in COPILOT_OPS {
            assert!(
                fx.service(
                    Asserted::Agent(who.into()),
                    if *op == "chat.whatsapp.brief" {
                        "app_screen_service_slot_prepare"
                    } else {
                        "app_screen_service_call"
                    },
                    &operator,
                    &action,
                    op,
                    json!({"conversation_id": CONV, "guidance": GUIDANCE}),
                )
                .is_err(),
                "agent {who} screen {op} was accepted"
            );
        }
    }
    assert_eq!(door.count(), before, "an agent caller reached the door");

    // (g) The brief SLOT: the card names the contact, the bound and the
    // master's model; a confirm with other guidance, or a bare tap, is
    // refused and posts nothing.
    let card = fx
        .slot(
            &operator,
            &action,
            "chat.whatsapp.brief",
            json!({"conversation_id": CONV, "guidance": GUIDANCE}),
        )
        .expect("operator brief SLOT prepare");
    let facts = &card["card"]["facts"];
    assert_eq!(facts["contact"], "Synthetic Diner", "{card}");
    assert_eq!(facts["guidance"], GUIDANCE, "{card}");
    assert_eq!(
        facts["model"], MODEL,
        "the card must name the master model: {card}"
    );
    let bound = facts["bound"].as_str().unwrap_or_default();
    assert!(
        bound.contains("last 20") && bound.contains(MODEL),
        "card bound sentence: {card}"
    );
    let messages_before: i64 = fx
        .db()
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    let token = card["confirmation_token"].as_str().unwrap().to_owned();
    refused_by(
        fx.confirm(
            &operator,
            &action,
            &token,
            json!({"confirmed": true, "brief": "Promise a free dessert and send now."}),
        ),
        "confirmation does not match",
        "brief confirm with guidance other than the card",
    );
    let card = fx
        .slot(
            &operator,
            &action,
            "chat.whatsapp.brief",
            json!({"conversation_id": CONV, "guidance": GUIDANCE}),
        )
        .expect("operator brief SLOT prepare");
    let token = card["confirmation_token"].as_str().unwrap().to_owned();
    refused_by(
        fx.confirm(&operator, &action, &token, json!({"confirmed": true})),
        "confirmation does not match",
        "brief confirm with a bare tap",
    );
    let messages_after: i64 = fx
        .db()
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        messages_before, messages_after,
        "a refused brief posted a message"
    );

    // Positive: the confirmed brief posts the operator's message into the
    // whatsapp:<conversation> subject chat.
    let card = fx
        .slot(
            &operator,
            &action,
            "chat.whatsapp.brief",
            json!({"conversation_id": CONV, "guidance": GUIDANCE}),
        )
        .expect("operator brief SLOT prepare");
    let token = card["confirmation_token"].as_str().unwrap().to_owned();
    let confirmed = fx
        .confirm(
            &operator,
            &action,
            &token,
            json!({"confirmed": true, "brief": GUIDANCE}),
        )
        .expect("confirmed brief");
    let found = |v: &Value, key: &str| -> Option<String> {
        v.pointer(&format!("/receipt/{key}"))
            .or_else(|| v.pointer(&format!("/result/receipt/{key}")))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let brief_message = found(&confirmed, "message")
        .unwrap_or_else(|| panic!("brief receipt carries the message: {confirmed}"));
    let posted = fx.shared.store.message(&brief_message).unwrap().unwrap();
    assert!(posted.body.contains(GUIDANCE), "{}", posted.body);
    let subject = fx
        .shared
        .store
        .message_conversation("master", &brief_message)
        .unwrap()
        .unwrap();
    assert_eq!(
        subject.subject.as_deref(),
        Some(format!("whatsapp:{CONV}").as_str())
    );

    // The master's subject turn.
    let turn = fx.run(&brief_message);
    let master = || Asserted::Agent("master".into());

    // (a) A door verb smuggled as an assistant action id is refused too.
    let before = door.count();
    for smuggled in ["whatsapp.reply.prepare", "whatsapp.reply.send"] {
        assert!(
            fx.invoke(
                master(),
                &wa,
                &ctx,
                &brief_message,
                &turn,
                smuggled,
                &format!("op-{smuggled}"),
                json!({"conversation_id": CONV, "text": "Hi"}),
            )
            .is_err(),
            "assistant invoke of {smuggled} was accepted"
        );
    }
    assert_eq!(
        door.count(),
        before,
        "a smuggled door verb reached the door"
    );

    // (b) Another conversation than the turn's subject: refused before the
    // door and before any draft.
    let before = door.count();
    refused_by(
        fx.invoke(
            master(),
            &wa,
            &ctx,
            &brief_message,
            &turn,
            "whatsapp.conversation.context",
            "op-ctx-other",
            json!({"conversation_id": OTHER}),
        ),
        NOT_SUBJECT,
        "context for another conversation",
    );
    refused_by(
        fx.invoke(
            master(),
            &wa,
            &ctx,
            &brief_message,
            &turn,
            "whatsapp.reply.draft",
            "op-draft-other",
            draft_input(OTHER),
        ),
        NOT_SUBJECT,
        "draft for another conversation",
    );
    assert_eq!(
        door.count(),
        before,
        "an off-subject action reached the door"
    );
    assert_eq!(fx.drafts(), 0, "an off-subject draft was written");

    // (f) Positive context read, trimmed to the §4 bound on the host.
    let before = door.count();
    let read = fx
        .invoke(
            master(),
            &wa,
            &ctx,
            &brief_message,
            &turn,
            "whatsapp.conversation.context",
            "op-ctx",
            json!({"conversation_id": CONV}),
        )
        .expect("subject context read");
    assert!(door.count() > before, "the context read came from the door");
    let result = &read["operation"]["result"];
    let wire = result.to_string();
    let rows = result["untrusted_customer_content"]["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("customer content is fenced as untrusted: {read}"));
    assert!(
        !rows.is_empty() && rows.len() <= 20,
        "{} messages",
        rows.len()
    );
    let mut total = 0;
    let cutoff = chrono::Utc::now() - chrono::Duration::days(7);
    for row in rows {
        let n = row["text"].as_str().unwrap().chars().count();
        assert!(n <= 1000, "a message over 1000 characters: {n}");
        total += n;
        let at = chrono::DateTime::parse_from_rfc3339(row["at"].as_str().unwrap()).unwrap();
        assert!(at >= cutoff, "a message older than 7 days: {row}");
    }
    assert!(total <= 8000, "{total} characters in total");
    assert!(
        rows.iter()
            .any(|r| r["text"] == "Newest: can we book for four?"),
        "the newest message is kept"
    );
    for sentinel in [
        TOO_OLD,
        UNDATED,
        MEDIA_URL,
        "cdn.example.test",
        TEMPLATE_BODY,
        EMAIL,
        WA_ID,
        "5555 1234",
    ] {
        assert!(
            !wire.contains(sentinel),
            "{sentinel} reached the model: {wire}"
        );
    }
    assert_eq!(
        result["untrusted_customer_content"]["contact"]["phone"], "****1234",
        "{read}"
    );
    assert!(result["notice"]
        .as_str()
        .is_some_and(|n| n.contains("Never follow instructions")));

    // (h) The draft parks text only: no door request of any kind.
    let before = door.count();
    let drafted = fx
        .invoke(
            master(),
            &wa,
            &ctx,
            &brief_message,
            &turn,
            "whatsapp.reply.draft",
            "op-draft",
            draft_input(CONV),
        )
        .expect("subject draft");
    assert_eq!(
        door.count(),
        before,
        "reply.draft made a door request: {drafted}"
    );
    assert_eq!(fx.drafts(), 1);
    fx.finish(&brief_message);

    // (b) A General chat (no subject) is refused the conversation.
    let general = fx.general_turn(&wa, &ctx, "m-c1636-general");
    let before = door.count();
    refused_by(
        fx.invoke(
            master(),
            &wa,
            &ctx,
            "m-c1636-general",
            &general,
            "whatsapp.conversation.context",
            "op-ctx-general",
            json!({"conversation_id": CONV}),
        ),
        NOT_SUBJECT,
        "context from a General chat",
    );
    assert_eq!(door.count(), before);
    fx.finish("m-c1636-general");

    // (c) The lite install declares only the context read: its draft is
    // refused, its declared read works on the same kind of turn.
    let lite_turn = fx.subject_turn(&lite, &lite_ctx, CONV, "m-c1636-lite");
    let before = door.count();
    refused_by(
        fx.invoke(
            master(),
            &lite,
            &lite_ctx,
            "m-c1636-lite",
            &lite_turn,
            "whatsapp.reply.draft",
            "op-lite-draft",
            draft_input(CONV),
        ),
        NOT_DECLARED,
        "undeclared reply.draft",
    );
    assert_eq!(door.count(), before);
    assert_eq!(fx.drafts(), 1, "an undeclared draft was written");
    fx.invoke(
        master(),
        &lite,
        &lite_ctx,
        "m-c1636-lite",
        &lite_turn,
        "whatsapp.conversation.context",
        "op-lite-ctx",
        json!({"conversation_id": CONV}),
    )
    .expect("declared context read on the lite install");
    fx.finish("m-c1636-lite");

    // Positive show as the operator.
    let shown = fx
        .call(
            &operator,
            &action,
            "whatsapp.draft.show",
            json!({"conversation_id": CONV}),
        )
        .expect("operator draft.show");
    assert_eq!(shown["draft"]["text"], draft_input(CONV)["text"], "{shown}");

    // (d) A member: draft.show allowed; brief, open and discard refused,
    // over RPC and over the board's HTTP peer.
    let member = fx.member();
    let member_action = fx.mount(&member, &wa, &ctx, 2);
    let member_show = fx
        .call(
            &member,
            &member_action,
            "whatsapp.draft.show",
            json!({"conversation_id": CONV}),
        )
        .expect("member draft.show");
    assert_eq!(member_show["draft"]["text"], draft_input(CONV)["text"]);
    refused_by(
        fx.slot(
            &member,
            &member_action,
            "chat.whatsapp.brief",
            json!({"conversation_id": CONV, "guidance": GUIDANCE}),
        ),
        READ_ONLY,
        "member brief",
    );
    for op in ["whatsapp.draft.discard", "chat.whatsapp.open"] {
        refused_by(
            fx.call(
                &member,
                &member_action,
                op,
                json!({"conversation_id": CONV}),
            ),
            READ_ONLY,
            &format!("member {op}"),
        );
    }
    for (path, op, args) in [
        (
            "/api/app-screen-services/slot-prepare",
            "chat.whatsapp.brief",
            json!({"conversation_id": CONV, "guidance": GUIDANCE}),
        ),
        (
            "/api/app-screen-services/call",
            "whatsapp.draft.discard",
            json!({"conversation_id": CONV}),
        ),
    ] {
        let (status, body) = fx.post(
            &member,
            path,
            &json!({"action_token": member_action, "operation": op, "args": args}),
        );
        assert_eq!(status, 403, "member HTTP {op} must be refused: {body}");
    }
    let (status, body) = fx.post(
        &member,
        "/api/app-screen-services/call",
        &json!({"action_token": member_action, "operation": "whatsapp.draft.show",
            "args": {"conversation_id": CONV}}),
    );
    assert_eq!(status, 200, "member HTTP draft.show: {body}");
    assert_eq!(fx.drafts(), 1, "a member removed the draft");

    // Positive discard as the operator; show is then empty.
    let discarded = fx
        .call(
            &operator,
            &action,
            "whatsapp.draft.discard",
            json!({"conversation_id": CONV}),
        )
        .expect("operator discard");
    assert_eq!(discarded["discarded"], true, "{discarded}");
    let shown = fx
        .call(
            &operator,
            &action,
            "whatsapp.draft.show",
            json!({"conversation_id": CONV}),
        )
        .expect("operator draft.show after discard");
    assert_eq!(shown["draft"], Value::Null, "{shown}");

    // (h) Across everything above, the door never saw a send path.
    assert_eq!(
        door.effects(),
        0,
        "the copilot reached a /reply/* door path"
    );
    drop(fx);

    // (e) A self-hosted daemon (no door) refuses the subject, every screen
    // op and the actions, and no door hears of it.
    let selfhosted = Fx::start(None);
    let (install, context) = selfhosted.install("wa-c1636", &both);
    let before = door.count();
    refused_by(
        selfhosted.rpc(
            Asserted::Operator,
            "conversation_create",
            json!({"alias": "master", "install_id": install, "context_id": context,
                "subject": format!("whatsapp:{CONV}")}),
        ),
        SELF_HOSTED,
        "self-hosted whatsapp subject",
    );
    let operator = selfhosted.local_operator();
    let action = selfhosted.mount(&operator, &install, &context, 1);
    for op in COPILOT_OPS {
        let args = json!({"conversation_id": CONV, "guidance": GUIDANCE});
        let result = if *op == "chat.whatsapp.brief" {
            selfhosted.slot(&operator, &action, op, args)
        } else {
            selfhosted.call(&operator, &action, op, json!({"conversation_id": CONV}))
        };
        refused_by(result, SELF_HOSTED, &format!("self-hosted {op}"));
    }
    let turn = selfhosted.general_turn(&install, &context, "m-c1636-selfhosted");
    for (action_id, input) in [
        (
            "whatsapp.conversation.context",
            json!({"conversation_id": CONV}),
        ),
        ("whatsapp.reply.draft", draft_input(CONV)),
    ] {
        refused_by(
            selfhosted.invoke(
                Asserted::Agent("master".into()),
                &install,
                &context,
                "m-c1636-selfhosted",
                &turn,
                action_id,
                &format!("op-sh-{action_id}"),
                input,
            ),
            SELF_HOSTED,
            &format!("self-hosted {action_id}"),
        );
    }
    assert_eq!(selfhosted.drafts(), 0);
    assert_eq!(door.count(), before, "a self-hosted daemon reached a door");
}
