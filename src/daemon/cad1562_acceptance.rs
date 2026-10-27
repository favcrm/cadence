//! CAD-1562 independent refusal acceptance. Reviewer-written from the
//! ticket and the operator-approved AOS-287 design (§3, §4, §5)
//! (claude-opus-spec-reviewer), not by the implementer. Implementers must
//! not edit or weaken this file.
//!
//! The one refusal check for the hosted WhatsApp door. A real daemon
//! (`Shared` over its own socket) and a real public board, with the door
//! injected through `ServeOptions.hosted_whatsapp` and pointed at a
//! loopback stub that keeps the `agenticos.whatsapp-door.v1` semantics
//! (prepare echoes key and digest, send executes once per key). The stub
//! counts every request it receives, so "refused before the door" is
//! proved by an unchanged count, not by an error string alone.
//!
//! Refused:
//! (a) an approved install that does not declare `whatsapp.*`: every
//!     verb, reads included, with no door request;
//! (b) a member mount and a read-only board mount: `connect_session`,
//!     `reply.prepare` and the `reply.send` SLOT; member HTTP routes;
//! (c) `reply.send` as a plain CALL, with a forged frame digest, with
//!     altered text, with an unconfirmed tap, with a wrong digest, and a
//!     replay (SLOT and native): none reaches `/reply/send`;
//! (d) a self-hosted daemon (no door): every verb, no request;
//! (e) an agent or detached caller, natively and through the screen.
//! Positive: the operator reads status and conversations and sends one
//! SLOT-approved reply, which reaches the door exactly once.
#![cfg(feature = "test-seam")]

use crate::daemon::{serve, ServeOptions, Shared};
use crate::operator_auth::{BoardUser, Origin, PublicRealm};
use crate::platform::hosted_whatsapp::{reply_digest, HostedWhatsapp, Reply};
use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const COMPANY: &str = "co_c1562";
const CONTRACT: &str = "agenticos.whatsapp-door.v1";
const CONVERSATION: &str = "conv_c1562";
const TEXT: &str = "多謝！\nSee you at 7.";
const NOT_DECLARED: &str = "does not declare WhatsApp messaging";
const SELF_HOSTED: &str = "runs only on a hosted AgenticOS workspace";
const READ_ONLY: &str = "read-only screen admits only read operations";
const WHATSAPP_OPS: &[&str] = &[
    "whatsapp.status",
    "whatsapp.connect_session",
    "whatsapp.conversations.list",
    "whatsapp.conversations.show",
    "whatsapp.messages",
    "whatsapp.reply.prepare",
    "whatsapp.reply.send",
    "whatsapp.reply.status",
];

// ---------- a loopback door with the contract's semantics ----------

#[derive(Default)]
struct DoorState {
    /// Every request path the door received, in order.
    hits: Vec<String>,
    /// key -> digest staged by prepare.
    staged: HashMap<String, String>,
    /// key -> recorded outcome (executed once).
    executed: HashMap<String, Value>,
}

#[derive(Clone)]
struct Door {
    url: String,
    state: Arc<Mutex<DoorState>>,
}

fn canonical_digest(body: &Value) -> String {
    // The pinned §3.6 canonical form, built independently of the host:
    // JavaScript `JSON.stringify` of the literal, in this key order.
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
    fn hits(&self) -> Vec<String> {
        self.state.lock().unwrap().hits.clone()
    }
    fn count(&self) -> usize {
        self.state.lock().unwrap().hits.len()
    }
    fn sends(&self) -> usize {
        self.hits()
            .iter()
            .filter(|p| p.ends_with("/reply/send"))
            .count()
    }
}

fn conversation() -> Value {
    json!({"contract": CONTRACT, "conversationId": CONVERSATION, "channelConnectionId": "cc_1",
        "status": "open", "contact": {"id": "ct_1", "name": "Synthetic Diner", "phone": "+85200000000",
        "email": null, "whatsappId": "85200000000"}, "replyMode": "freeform",
        "freeformUntil": "2030-01-01T00:00:00.000Z", "lastInboundAt": null, "lastOutboundAt": null,
        "unreadCount": 0, "handoffAt": null})
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
        "/status" => ok(json!({"contract": CONTRACT, "connected": true,
            "displayPhoneNumber": "+852 0000 0000", "verifiedName": "Synthetic Co",
            "wabaId": "w_1", "phoneNumberId": "p_1", "channelConnectionId": "cc_1",
            "webhook": {"subscribed": true, "status": null, "error": null, "fields": []},
            "observedAt": "2026-10-18T00:00:00.000Z"})),
        "/connect-session" => ok(json!({"contract": CONTRACT,
            "url": "https://portal.example/connect/synthetic", "expiresAt": "2030-01-01T00:00:00.000Z"})),
        "/conversations" => ok(json!({"contract": CONTRACT, "conversations": [], "cursor": null})),
        p if p == format!("/conversations/{CONVERSATION}") => ok(conversation()),
        p if p == format!("/conversations/{CONVERSATION}/messages") => {
            ok(json!({"contract": CONTRACT, "messages": [], "cursor": null}))
        }
        "/reply/prepare" => {
            let Some(key) = key else {
                return refuse(400, "invalid_request");
            };
            let digest = canonical_digest(body);
            if door.staged.get(&key).is_some_and(|d| d != &digest) {
                return refuse(409, "key_conflict");
            }
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
            let Some(staged) = door.staged.get(&key).cloned() else {
                return refuse(409, "not_prepared");
            };
            if body["digest"].as_str() != Some(staged.as_str()) {
                return refuse(409, "digest_mismatch");
            }
            let repeated = door.executed.contains_key(&key);
            let outcome = door.executed.entry(key.clone()).or_insert_with(|| {
                json!({"contract": CONTRACT, "key": key, "decision": "granted", "status": "sent",
                    "executed": true, "favcrmMessageId": "fm_c1562", "providerMessageId": "wamid.c1562",
                    "deduped": false, "errorCode": null})
            });
            let mut outcome = outcome.clone();
            outcome["repeated"] = json!(repeated);
            ok(outcome)
        }
        p if p.starts_with("/reply/") => {
            let key = p.trim_start_matches("/reply/");
            match door.executed.get(key) {
                Some(outcome) => {
                    let mut outcome = outcome.clone();
                    outcome["repeated"] = json!(true);
                    ok(outcome)
                }
                None => refuse(404, "not_found"),
            }
        }
        _ => refuse(404, "not_found"),
    }
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
}

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

impl Fx {
    /// `door: None` is a self-hosted daemon: no AgenticOS WhatsApp door.
    fn start(door: Option<&Door>) -> Self {
        assert!(
            std::env::var_os("CADENCE_SOCKET").is_none(),
            "fixture BLOCKED: inherited socket override"
        );
        let root = tempfile::Builder::new().prefix("c1562").tempdir().unwrap();
        let state = root.path().to_path_buf();
        let pm = state.join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let (port, reservation) = crate::test_ports::reserve_board_port();
        let host = format!("c1562.board.localhost:{port}");
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
        shared
            .store
            .register_agent(&crate::store::NewAgent {
                alias: "synthetic-worker",
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: state.to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
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

    /// The operator's real, consented install of `app` whose one screen
    /// package declares `operations`, plus one active context.
    fn install(&self, app: &str, operations: &[&str]) -> (String, String) {
        let source = self.root.path().join(format!("source-{app}"));
        let screen = source.join("screens/main");
        std::fs::create_dir_all(&screen).unwrap();
        let manifest = format!("---\napp: {app}\ntitle: Synthetic WhatsApp\nversion: '0.1.0'\nsummary: Isolated CAD1562 refusal fixture\nneeds:\n  connections: []\n  requires:\n    contracts:\n      app-screens: [1]\n      screen-records: [1]\n---\n\nSynthetic only.\n");
        std::fs::write(source.join("app.md"), manifest).unwrap();
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(source.join("workflows/synthetic.md"), "---\ntitle: Synthetic refusal fixture\ngoal: Synthetic text only\ninputs:\n  writer: { ask: Synthetic writer }\n  reviewer: { ask: Synthetic reviewer }\ndistinct: [writer, reviewer]\n---\n\n## Synthetic draft\nagent: {{writer}}\nsize: S\naction: local.text.produce\n\nProduce synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n\n## Synthetic review\nagent: {{reviewer}}\nsize: S\ndepends_on: 1\naction: local.text.review\n\nReview the synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n").unwrap();
        let js = "(()=>{document.getElementById('root').textContent='c1562';})();";
        std::fs::write(screen.join("client.js"), js).unwrap();
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
            "fixture BLOCKED: install consent for {app}"
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

    fn door_flag(&self, install: &str) -> Value {
        self.op("app_workspace_show", json!({"install_id": install}))["whatsapp_door"].clone()
    }

    fn member(&self) -> Session {
        let now = self.shared.operator_now();
        let opened = self
            .shared
            .operator_auth()
            .open_public_bound(
                BoardUser {
                    sub: "synthetic-c1562-member".into(),
                    email: "c1562-member@example.test".into(),
                    name: "Synthetic member".into(),
                    role: "member".into(),
                    handle: "c1562-member".into(),
                },
                "c1562-member-jti",
                now + 120,
                "c1562-isolated-acceptance",
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

    /// A real mint + consume; returns the action token.
    fn mount(
        &self,
        session: &Session,
        install: &str,
        context: &str,
        generation: u64,
        read_only: bool,
    ) -> String {
        let mut body = json!({"install_id": install, "context_id": context,
            "tag": "main", "generation": generation});
        if read_only {
            body["read_only"] = json!(true);
        }
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

    fn confirm(
        &self,
        session: &Session,
        action: &str,
        token: &str,
        confirmed: bool,
    ) -> crate::Result<Value> {
        self.rpc(
            Asserted::Operator,
            "app_screen_service_slot_confirm",
            self.creds(
                session,
                json!({"action_token": action, "confirmation_token": token,
                    "confirmation": {"confirmed": confirmed}}),
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
}

/// A refusal is guard evidence only when it is the daemon's own rejection
/// carrying the named reason — never a transport or internal failure.
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

/// Every native verb with exactly its allowed fields, so only the gate
/// under test can refuse it.
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
            scope(json!({"conversation_id": CONVERSATION})),
        ),
        (
            "whatsapp_messages",
            scope(json!({"conversation_id": CONVERSATION})),
        ),
        (
            "whatsapp_reply_prepare",
            scope(json!({"conversation_id": CONVERSATION, "text": TEXT})),
        ),
        (
            "whatsapp_reply_send",
            scope(json!({"effect_id": effect, "digest": "a".repeat(64)})),
        ),
        ("whatsapp_reply_status", scope(json!({"effect_id": effect}))),
    ]
}

#[test]
fn cad1562_whatsapp_door_refuses_undeclared_member_unapproved_replayed_and_self_hosted() {
    // The host digest is the AOS-287 golden vector (agenticos-v2#731,
    // packages/contracts/src/whatsapp-door.test.ts), byte for byte.
    assert_eq!(
        reply_digest(&Reply {
            conversation_id: "conv_1".into(),
            text: "多謝！\nSee you at 7.".into(),
            effect_id: "eff_1".into(),
            reply_to_message_id: None,
        }),
        "809b9737285f7374d0feabe76e8178305af9c99ed869a59168a8d3261a9e0ba2",
        "host digest diverges from the pinned contract vector"
    );

    let door = Door::start();
    let fx = Fx::start(Some(&door));
    let (wa, wa_ctx) = fx.install("wa-c1562", WHATSAPP_OPS);
    let (plain, plain_ctx) = fx.install("plain-c1562", &["records.list"]);
    assert_eq!(fx.door_flag(&wa), json!(true), "declaring receipt");
    assert_eq!(fx.door_flag(&plain), json!(false), "non-declaring receipt");

    // (a) An approved install that declares no whatsapp.* operation is
    // refused every verb, reads included, before the door.
    let before = door.count();
    for (method, params) in native_verbs(&plain, &plain_ctx) {
        refused_by(
            fx.rpc(Asserted::Operator, method, params),
            NOT_DECLARED,
            &format!("non-declaring operator {method}"),
        );
    }
    let operator = fx.local_operator();
    let plain_action = fx.mount(&operator, &plain, &plain_ctx, 1, false);
    for operation in ["whatsapp.status", "whatsapp.conversations.list"] {
        refused_by(
            fx.service(
                Asserted::Operator,
                "app_screen_service_call",
                &operator,
                &plain_action,
                operation,
                json!({}),
            ),
            "not declared by current package",
            &format!("non-declaring screen {operation}"),
        );
    }
    assert_eq!(
        door.count(),
        before,
        "a non-declaring install reached the door"
    );

    // (e) Agent and detached callers, on the declaring install.
    let before = door.count();
    for who in [
        Asserted::Agent("synthetic-worker".into()),
        Asserted::Unproven,
    ] {
        for (method, params) in native_verbs(&wa, &wa_ctx) {
            assert!(
                fx.rpc(who.clone(), method, params).is_err(),
                "{method} must refuse {}",
                who.as_str()
            );
        }
    }
    assert_eq!(
        door.count(),
        before,
        "an agent/detached caller reached the door"
    );

    // Positive operator reads through the declaring install.
    let status = fx.op("whatsapp_status", native_verbs(&wa, &wa_ctx)[0].1.clone());
    assert_eq!(status["connected"], true, "{status}");
    let shown = fx.op(
        "whatsapp_conversations_show",
        native_verbs(&wa, &wa_ctx)[3].1.clone(),
    );
    assert_eq!(shown["conversationId"], CONVERSATION, "{shown}");

    // (b) A member mount and a read-only board mount: reads only.
    let member = fx.member();
    let member_action = fx.mount(&member, &wa, &wa_ctx, 2, false);
    let ro_action = fx.mount(&operator, &wa, &wa_ctx, 3, true);
    let member_read = fx
        .service(
            Asserted::Operator,
            "app_screen_service_call",
            &member,
            &member_action,
            "whatsapp.status",
            json!({}),
        )
        .expect("member read of whatsapp.status");
    assert_eq!(member_read["connected"], true, "{member_read}");
    let before = door.count();
    for (who, session, action) in [
        ("member", &member, &member_action),
        ("read-only board", &operator, &ro_action),
    ] {
        for (method, operation, args) in [
            (
                "app_screen_service_call",
                "whatsapp.connect_session",
                json!({}),
            ),
            (
                "app_screen_service_call",
                "whatsapp.reply.prepare",
                json!({"conversation_id": CONVERSATION, "text": TEXT}),
            ),
            (
                "app_screen_service_slot_prepare",
                "whatsapp.reply.send",
                json!({"conversation_id": CONVERSATION, "text": TEXT,
                    "effect_id": format!("wae_{}", "0".repeat(32))}),
            ),
        ] {
            refused_by(
                fx.service(Asserted::Operator, method, session, action, operation, args),
                READ_ONLY,
                &format!("{who} {operation} via {method}"),
            );
        }
    }
    // Over HTTP the WhatsApp routes are the operator's: a member session
    // is refused before the daemon or the door.
    for (path, body) in [
        (
            "/api/whatsapp/status",
            json!({"install_id": wa, "context_id": wa_ctx}),
        ),
        (
            "/api/whatsapp/connect-session",
            json!({"install_id": wa, "context_id": wa_ctx}),
        ),
        (
            "/api/whatsapp/reply/prepare",
            json!({"install_id": wa, "context_id": wa_ctx, "conversation_id": CONVERSATION, "text": TEXT}),
        ),
        (
            "/api/whatsapp/reply/send",
            json!({"install_id": wa, "context_id": wa_ctx,
                "effect_id": format!("wae_{}", "0".repeat(32)), "digest": "a".repeat(64)}),
        ),
    ] {
        let (status, answer) = fx.post(&member, path, &body);
        assert_eq!(status, 403, "member {path} must be refused: {answer}");
    }
    assert_eq!(
        door.count(),
        before,
        "a member/read-only write reached the door"
    );

    // (c) The operator's reply: prepared, then SLOT-approved exactly once.
    let action = fx.mount(&operator, &wa, &wa_ctx, 4, false);
    let prepared = fx
        .service(
            Asserted::Operator,
            "app_screen_service_call",
            &operator,
            &action,
            "whatsapp.reply.prepare",
            json!({"conversation_id": CONVERSATION, "text": TEXT}),
        )
        .expect("operator reply.prepare");
    let effect = prepared["effect_id"].as_str().unwrap().to_owned();
    let digest = prepared["digest"].as_str().unwrap().to_owned();
    assert_eq!(
        digest,
        canonical_digest(
            &json!({"conversationId": CONVERSATION, "text": TEXT, "effectId": effect})
        ),
        "host digest is not the contract canonical digest"
    );
    let send_args = json!({"conversation_id": CONVERSATION, "text": TEXT, "effect_id": effect});
    assert_eq!(door.sends(), 0);

    // A plain CALL is never a send.
    refused_by(
        fx.service(
            Asserted::Operator,
            "app_screen_service_call",
            &operator,
            &action,
            "whatsapp.reply.send",
            send_args.clone(),
        ),
        "not an implemented non-protected screen CALL",
        "reply.send as a CALL",
    );
    // The frame cannot supply a digest, key or recipient.
    for forged in ["digest", "key", "recipient"] {
        let mut args = send_args.clone();
        args[forged] = json!(digest);
        refused_by(
            fx.service(
                Asserted::Operator,
                "app_screen_service_slot_prepare",
                &operator,
                &action,
                "whatsapp.reply.send",
                args,
            ),
            "closed shape",
            &format!("reply.send SLOT with forged {forged}"),
        );
    }
    // Text other than what was prepared is not the approved reply.
    let mut altered = send_args.clone();
    altered["text"] = json!("Something else entirely.");
    refused_by(
        fx.service(
            Asserted::Operator,
            "app_screen_service_slot_prepare",
            &operator,
            &action,
            "whatsapp.reply.send",
            altered,
        ),
        "differs from the prepared reply",
        "reply.send SLOT with altered text",
    );
    // An unconfirmed tap does nothing (and burns the token).
    let card = fx
        .service(
            Asserted::Operator,
            "app_screen_service_slot_prepare",
            &operator,
            &action,
            "whatsapp.reply.send",
            send_args.clone(),
        )
        .expect("reply.send SLOT prepare");
    assert_eq!(card["card"]["facts"]["digest"], json!(digest), "{card}");
    assert_eq!(card["card"]["facts"]["text"], json!(TEXT), "{card}");
    let token = card["confirmation_token"].as_str().unwrap().to_owned();
    assert!(
        fx.confirm(&operator, &action, &token, false).is_err(),
        "an unconfirmed tap was accepted"
    );
    refused_by(
        fx.confirm(&operator, &action, &token, true),
        "spent or unknown",
        "a burned confirmation replayed",
    );
    // A native send with any other digest is refused locally.
    refused_by(
        fx.rpc(
            Asserted::Operator,
            "whatsapp_reply_send",
            json!({"install_id": wa, "context_id": wa_ctx, "effect_id": effect, "digest": "b".repeat(64)}),
        ),
        "digest does not match the prepared",
        "native send with a mismatched digest",
    );
    // Agent / detached callers through the screen confirm path.
    for who in [
        Asserted::Agent("synthetic-worker".into()),
        Asserted::Unproven,
    ] {
        assert!(
            fx.service(
                who,
                "app_screen_service_slot_prepare",
                &operator,
                &action,
                "whatsapp.reply.send",
                send_args.clone(),
            )
            .is_err(),
            "agent/detached SLOT prepare accepted"
        );
    }
    assert_eq!(door.sends(), 0, "a refused reply reached /reply/send");

    // Positive: one confirmed tap sends exactly once.
    let card = fx
        .service(
            Asserted::Operator,
            "app_screen_service_slot_prepare",
            &operator,
            &action,
            "whatsapp.reply.send",
            send_args.clone(),
        )
        .expect("reply.send SLOT prepare");
    let token = card["confirmation_token"].as_str().unwrap().to_owned();
    let sent = fx
        .confirm(&operator, &action, &token, true)
        .expect("confirmed reply.send");
    assert_eq!(sent["reply"]["state"], "sent", "{sent}");
    assert_eq!(
        door.sends(),
        1,
        "the approved reply must reach the door once"
    );

    // Replay: SLOT prepare again, native send again — both refused here.
    refused_by(
        fx.service(
            Asserted::Operator,
            "app_screen_service_slot_prepare",
            &operator,
            &action,
            "whatsapp.reply.send",
            send_args.clone(),
        ),
        "not awaiting approval",
        "replayed reply.send SLOT",
    );
    refused_by(
        fx.rpc(
            Asserted::Operator,
            "whatsapp_reply_send",
            json!({"install_id": wa, "context_id": wa_ctx, "effect_id": effect, "digest": digest}),
        ),
        "already attempted",
        "replayed native send",
    );
    assert_eq!(door.sends(), 1, "a replay reached /reply/send");
    let status = fx.op(
        "whatsapp_reply_status",
        json!({"install_id": wa, "context_id": wa_ctx, "effect_id": effect}),
    );
    assert_eq!(status["state"], "sent", "{status}");
    assert_eq!(door.sends(), 1, "reply.status executed a send");
    drop(fx);

    // (d) A self-hosted daemon (no door) refuses every verb of a declaring,
    // approved install, and the door never hears of it.
    let selfhosted = Fx::start(None);
    let (install, context) = selfhosted.install("wa-c1562", WHATSAPP_OPS);
    assert_eq!(selfhosted.door_flag(&install), json!(true));
    let before = door.hits();
    for (method, params) in native_verbs(&install, &context) {
        refused_by(
            selfhosted.rpc(Asserted::Operator, method, params),
            SELF_HOSTED,
            &format!("self-hosted {method}"),
        );
    }
    assert_eq!(door.hits(), before, "a self-hosted daemon reached a door");
}
