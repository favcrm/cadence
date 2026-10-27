//! CAD-1651 independent refusal acceptance. Reviewer-written from the
//! ticket, the approved CAD-1465 step-3 design (H4, §5) and the
//! `agenticos.whatsapp-door.v3` contract (agenticos-v2#816,
//! `packages/contracts/src/whatsapp-door-v3.ts` at 87043b7e) by
//! claude-opus-spec-reviewer, not by the implementer. Implementers must
//! not edit or weaken this file.
//!
//! A real daemon (`Shared` over its own socket) and a real public board,
//! with the hosted door injected through `ServeOptions.hosted_whatsapp`
//! and pointed at a loopback double of door v3: `POST
//! /conversations/:id/handoff {handoff}` stamps or clears the live
//! `handoffAt` that the v1 conversation read serves. FavCRM's `handoffAt`
//! is the one authority; the double records every handoff request, so
//! "refused before the door" is an unchanged list.
//!
//! Refused:
//! (a) an agent or detached caller: native `whatsapp_handoff_set` and
//!     `whatsapp_handoff_release`, the screen `whatsapp.handoff.set` CALL
//!     and `whatsapp.handoff.release` SLOT, and the confirm of an
//!     operator-drawn release card;
//! (b) a member: both screen ops (read-only mount), and both HTTP peers
//!     `/api/whatsapp/handoff/{set,release}` (also with no session and
//!     with an agent-asserted connection), while the operator's own HTTP
//!     set is admitted, so the route is real;
//! (c) a release whose `handoffAt` changed (re-stamped, or cleared) after
//!     the card was drawn, refused at confirm before any door write;
//! (d) a release card the frame tries to steer: a SLOT naming the
//!     takeover time, a confirm carrying a different conversation id or
//!     forged args, a confirm of another shape, a burned card replayed;
//!     and a release of a conversation that is not taken over;
//! (e) a door answer that breaks v3 (another contract, another
//!     conversation, `handoff_at` of the wrong nullness or missing);
//! (f) a self-hosted daemon (no door): both native verbs.
//! Positive: the operator takes over (the conversation reads back with
//! `handoffAt` set) and hands back through one confirmed card (cleared),
//! each a single door request for the card's own conversation.
#![cfg(feature = "test-seam")]

use crate::daemon::{serve, ServeOptions, Shared};
use crate::operator_auth::{BoardUser, Origin, PublicRealm};
use crate::platform::hosted_whatsapp::HostedWhatsapp;
use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const COMPANY: &str = "co_c1651";
const V1: &str = "agenticos.whatsapp-door.v1";
const V3: &str = "agenticos.whatsapp-door.v3";
const CONV: &str = "conv_c1651";
const OTHER: &str = "conv_c1651_other";
const SELF_HOSTED: &str = "runs only on a hosted AgenticOS workspace";
const READ_ONLY: &str = "read-only screen admits only read operations";
const OPERATOR: &str = "operator";
const CHANGED: &str = "confirmation facts changed|not taken over";
const OPS: &[&str] = &[
    "whatsapp.conversations.show",
    "whatsapp.handoff.set",
    "whatsapp.handoff.release",
];

// ---------- a loopback door with the v3 handoff semantics ----------

#[derive(Clone, Copy, PartialEq)]
enum Lie {
    None,
    Contract,
    Conversation,
    Nullness,
    Missing,
}

struct DoorState {
    /// Every handoff request the door received: (conversation, handoff).
    handoffs: Vec<(String, bool)>,
    /// The live FavCRM `handoffAt` per conversation.
    at: HashMap<String, Option<String>>,
    stamps: u32,
    lie: Lie,
}

#[derive(Clone)]
struct Door {
    url: String,
    state: Arc<Mutex<DoorState>>,
}

impl DoorState {
    fn stamp(&mut self) -> String {
        self.stamps += 1;
        format!("2026-10-20T00:00:{:02}.000Z", self.stamps)
    }
}

impl Door {
    fn start() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr().to_ip().unwrap());
        let state = Arc::new(Mutex::new(DoorState {
            handoffs: Vec::new(),
            at: HashMap::from([(CONV.to_string(), None), (OTHER.to_string(), None)]),
            stamps: 0,
            lie: Lie::None,
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
    fn at(&self, id: &str) -> Option<String> {
        self.state.lock().unwrap().at[id].clone()
    }
    /// FavCRM changes the takeover behind the card's back.
    fn restamp(&self, id: &str) {
        let mut door = self.state.lock().unwrap();
        let stamp = door.stamp();
        door.at.insert(id.into(), Some(stamp));
    }
    fn clear(&self, id: &str) {
        self.state.lock().unwrap().at.insert(id.into(), None);
    }
    fn lie(&self, lie: Lie) {
        self.state.lock().unwrap().lie = lie;
    }
}

fn answer(state: &Mutex<DoorState>, path: &str, post: bool, body: &Value) -> (u16, Value) {
    let mut door = state.lock().unwrap();
    let ok = |data: Value| (200, json!({"ok": true, "data": data}));
    let refuse = |code: u16, what: &str| {
        (
            code,
            json!({"ok": false, "error": {"code": what, "message": what}}),
        )
    };
    let rest = path.strip_prefix("/v1/runtime/whatsapp").unwrap_or("");
    let parts: Vec<&str> = rest.trim_start_matches('/').split('/').collect();
    match parts.as_slice() {
        ["conversations", id] if !post && door.at.contains_key(*id) => ok(json!({"contract": V1,
            "conversationId": id, "channelConnectionId": "cc_1", "status": "open",
            "contact": {"id": "ct_1", "name": "Synthetic Diner", "phone": "+85200000000",
                "email": null, "whatsappId": "85200000000"},
            "replyMode": "template_required", "freeformUntil": null, "lastInboundAt": null,
            "lastOutboundAt": null, "unreadCount": 0, "handoffAt": door.at[*id]})),
        ["conversations", id, "handoff"] if post && door.at.contains_key(*id) => {
            // The v3 request schema: exactly `{handoff: boolean}`.
            let Some(handoff) = body
                .as_object()
                .filter(|o| o.len() == 1)
                .and_then(|o| o.get("handoff"))
                .and_then(Value::as_bool)
            else {
                return refuse(400, "invalid_request");
            };
            door.handoffs.push((id.to_string(), handoff));
            let at = if handoff { Some(door.stamp()) } else { None };
            door.at.insert(id.to_string(), at.clone());
            let mut data = json!({"contract": V3, "conversation_id": id, "handoff_at": at});
            match door.lie {
                Lie::None => {}
                Lie::Contract => data["contract"] = json!("agenticos.whatsapp-door.v2"),
                Lie::Conversation => data["conversation_id"] = json!(OTHER),
                Lie::Nullness => {
                    data["handoff_at"] = if handoff {
                        Value::Null
                    } else {
                        json!("2026-10-20T00:00:00.000Z")
                    }
                }
                Lie::Missing => {
                    data.as_object_mut().unwrap().remove("handoff_at");
                }
            }
            ok(data)
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
        let root = tempfile::Builder::new().prefix("c1651").tempdir().unwrap();
        let state = root.path().to_path_buf();
        let pm = state.join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let (port, reservation) = crate::test_ports::reserve_board_port();
        let host = format!("c1651.board.localhost:{port}");
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
        let manifest = format!("---\napp: {app}\ntitle: Synthetic WhatsApp\nversion: '0.1.0'\nsummary: Isolated CAD1651 refusal fixture\nneeds:\n  connections: []\n  requires:\n    contracts:\n      app-screens: [1]\n      screen-records: [1]\n---\n\nSynthetic only.\n");
        std::fs::write(source.join("app.md"), manifest).unwrap();
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(source.join("workflows/synthetic.md"), "---\ntitle: Synthetic refusal fixture\ngoal: Synthetic text only\ninputs:\n  writer: { ask: Synthetic writer }\n  reviewer: { ask: Synthetic reviewer }\ndistinct: [writer, reviewer]\n---\n\n## Synthetic draft\nagent: {{writer}}\nsize: S\naction: local.text.produce\n\nProduce synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n\n## Synthetic review\nagent: {{reviewer}}\nsize: S\ndepends_on: 1\naction: local.text.review\n\nReview the synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n").unwrap();
        let js = "(()=>{document.getElementById('root').textContent='c1651';})();";
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

    /// A real public board session minted by this daemon's own `Auth`.
    fn public(&self, role: &str, n: u32) -> Session {
        let now = self.shared.operator_now();
        let opened = self
            .shared
            .operator_auth()
            .open_public_bound(
                BoardUser {
                    sub: format!("synthetic-c1651-{role}"),
                    email: format!("c1651-{role}@example.test"),
                    name: format!("Synthetic {role}"),
                    role: role.into(),
                    handle: format!("c1651-{role}"),
                },
                &format!("c1651-{role}-jti-{n}"),
                now + 120,
                "c1651-isolated-acceptance",
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

    /// A real mint + consume; returns the action token.
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

    fn confirm_raw(&self, who: Asserted, session: &Session, body: Value) -> crate::Result<Value> {
        self.rpc(
            who,
            "app_screen_service_slot_confirm",
            self.creds(session, body),
        )
    }

    fn confirm(&self, session: &Session, action: &str, token: &str) -> crate::Result<Value> {
        self.confirm_raw(
            Asserted::Operator,
            session,
            json!({"action_token": action, "confirmation_token": token,
                "confirmation": {"confirmed": true}}),
        )
    }

    fn post(&self, session: Option<&Session>, who: &str, path: &str, body: &Value) -> (u16, Value) {
        let mut request = self
            .agent
            .post(format!("{}{path}", self.base))
            .header("Host", &self.host)
            .header("Origin", format!("http://{}", self.host))
            .header("X-Cadence-Board", "1")
            .header(crate::test_seam::AS_HEADER, who)
            .header(crate::test_seam::TOKEN_HEADER, &self.seam)
            .header("Content-Type", "application/json");
        if let Some(session) = session {
            request = request.header(
                "Cookie",
                format!("__Host-aos-board-session={}", session.token),
            );
        }
        let response = request
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

/// (c) only: the confirm-time recompute refusal. CAD-1656 gave the
/// changed-facts refusal its own conflict code, so besides the original
/// guard refusal this accepts exactly kind `conflict` with code
/// `facts_changed` and the recompute's own message, nothing else.
fn refused_changed(result: crate::Result<Value>, case: &str) {
    match result {
        Err(error) if error.kind() == "conflict" => {
            assert_eq!(
                error.code(),
                Some("facts_changed"),
                "{case}: a conflict that is not the facts refusal: {error}"
            );
            assert!(
                error.to_string().contains("confirmation facts changed"),
                "{case}: refused for the wrong reason: {error}"
            );
        }
        result => refused_by(result, CHANGED, case),
    }
}

/// One release SLOT card: (facts, live, confirmation token).
fn card(fx: &Fx, op: &Session, action: &str, conversation: &str) -> (Value, Value, String) {
    let card = fx
        .service(
            Asserted::Operator,
            "app_screen_service_slot_prepare",
            op,
            action,
            "whatsapp.handoff.release",
            json!({"conversation_id": conversation}),
        )
        .unwrap_or_else(|error| panic!("release SLOT prepare: {error}"));
    assert_eq!(
        card["card"]["operation"], "whatsapp.handoff.release",
        "{card}"
    );
    let token = card["confirmation_token"].as_str().unwrap().to_owned();
    (
        card["card"]["facts"].clone(),
        card["card"]["live"].clone(),
        token,
    )
}

#[test]
fn cad1651_handoff_refuses_agent_member_changed_forged_lying_door_and_self_hosted() {
    let door = Door::start();
    let fx = Fx::start(Some(&door));
    let (wa, ctx) = fx.install("wa-c1651", OPS);
    let operator = fx.local_operator();
    let action = fx.mount(&operator, &wa, &ctx, 1);
    let scope = |conversation: &str| json!({"install_id": wa, "context_id": ctx, "conversation_id": conversation});
    let natives = ["whatsapp_handoff_set", "whatsapp_handoff_release"];

    // (a) Agent and detached callers: natively and through the screen.
    for who in [
        Asserted::Agent("synthetic-worker".into()),
        Asserted::Unproven,
    ] {
        for method in natives {
            refused_by(
                fx.rpc(who.clone(), method, scope(CONV)),
                OPERATOR,
                &format!("{} native {method}", who.as_str()),
            );
        }
        refused_by(
            fx.service(
                who.clone(),
                "app_screen_service_call",
                &operator,
                &action,
                "whatsapp.handoff.set",
                json!({"conversation_id": CONV}),
            ),
            OPERATOR,
            &format!("{} screen handoff.set", who.as_str()),
        );
        refused_by(
            fx.service(
                who.clone(),
                "app_screen_service_slot_prepare",
                &operator,
                &action,
                "whatsapp.handoff.release",
                json!({"conversation_id": CONV}),
            ),
            OPERATOR,
            &format!("{} screen handoff.release SLOT", who.as_str()),
        );
    }
    assert!(door.handoffs().is_empty(), "an agent reached the door");

    // (b) A member: both screen ops on a member mount, both HTTP peers.
    let member = fx.public("member", 1);
    let member_action = fx.mount(&member, &wa, &ctx, 2);
    refused_by(
        fx.service(
            Asserted::Operator,
            "app_screen_service_call",
            &member,
            &member_action,
            "whatsapp.handoff.set",
            json!({"conversation_id": CONV}),
        ),
        READ_ONLY,
        "member screen handoff.set",
    );
    refused_by(
        fx.service(
            Asserted::Operator,
            "app_screen_service_slot_prepare",
            &member,
            &member_action,
            "whatsapp.handoff.release",
            json!({"conversation_id": CONV}),
        ),
        READ_ONLY,
        "member screen handoff.release SLOT",
    );
    for path in ["/api/whatsapp/handoff/set", "/api/whatsapp/handoff/release"] {
        // The board's own connection asserts the operator: only the
        // board's caller rule can refuse the member here.
        let (status, body) = fx.post(Some(&member), "operator", path, &scope(CONV));
        assert_eq!(status, 403, "member HTTP {path} must be refused: {body}");
        let (status, body) = fx.post(Some(&member), "agent:synthetic-worker", path, &scope(CONV));
        assert!(
            status == 401 || status == 403,
            "member+agent HTTP {path} must be refused: {status} {body}"
        );
        let (status, body) = fx.post(None, "operator", path, &scope(CONV));
        assert!(
            status == 401 || status == 403,
            "sessionless HTTP {path} must be refused: {status} {body}"
        );
    }
    assert!(door.handoffs().is_empty(), "a member reached the door");

    // The HTTP peer is real: the board owner's own set is admitted, and a
    // forged extra field refuses before the daemon.
    let owner = fx.public("operator", 2);
    let mut forged = scope(CONV);
    forged["handoff"] = json!(false);
    let (status, body) = fx.post(
        Some(&owner),
        "operator",
        "/api/whatsapp/handoff/set",
        &forged,
    );
    assert_eq!(status, 400, "HTTP set with a forged field: {body}");
    assert!(
        door.handoffs().is_empty(),
        "a forged HTTP body reached the door"
    );
    let (status, body) = fx.post(
        Some(&owner),
        "operator",
        "/api/whatsapp/handoff/set",
        &scope(CONV),
    );
    assert_eq!(status, 200, "owner HTTP handoff.set: {body}");
    assert_eq!(door.handoffs(), vec![(CONV.to_string(), true)]);
    // Undo through the door directly so the screen path starts clean.
    door.clear(CONV);

    // Positive set through the screen: one door request, and the
    // conversation reads back with `handoffAt` set.
    let set = fx
        .service(
            Asserted::Operator,
            "app_screen_service_call",
            &operator,
            &action,
            "whatsapp.handoff.set",
            json!({"conversation_id": CONV}),
        )
        .expect("operator screen handoff.set");
    let t1 = door.at(CONV).expect("set stamped handoffAt");
    assert_eq!(set["handoff_at"], json!(t1), "{set}");
    assert_eq!(door.handoffs().len(), 2);
    let shown = fx
        .service(
            Asserted::Operator,
            "app_screen_service_call",
            &operator,
            &action,
            "whatsapp.conversations.show",
            json!({"conversation_id": CONV}),
        )
        .expect("operator conversation read");
    assert_eq!(shown["handoffAt"], json!(t1), "{shown}");

    // (d) A release of a conversation that is not taken over has no card.
    refused_by(
        fx.service(
            Asserted::Operator,
            "app_screen_service_slot_prepare",
            &operator,
            &action,
            "whatsapp.handoff.release",
            json!({"conversation_id": OTHER}),
        ),
        "not taken over",
        "release of a conversation not taken over",
    );
    // The frame cannot name the takeover time or any other field.
    refused_by(
        fx.service(
            Asserted::Operator,
            "app_screen_service_slot_prepare",
            &operator,
            &action,
            "whatsapp.handoff.release",
            json!({"conversation_id": CONV, "handoff_at": t1}),
        ),
        "closed shape",
        "release SLOT naming handoff_at",
    );
    // Release is never a plain CALL.
    refused_by(
        fx.service(
            Asserted::Operator,
            "app_screen_service_call",
            &operator,
            &action,
            "whatsapp.handoff.release",
            json!({"conversation_id": CONV}),
        ),
        "not an implemented non-protected screen CALL",
        "release as a CALL",
    );

    // The card binds exactly the conversation and its takeover time; the
    // contact is live, unbound data.
    let (facts, live, token) = card(&fx, &operator, &action, CONV);
    assert_eq!(facts, json!({"conversation_id": CONV, "handoff_at": t1}));
    assert_eq!(live["contact"]["name"], "Synthetic Diner", "{live}");

    // (a) An agent cannot confirm the operator's card.
    for who in [
        Asserted::Agent("synthetic-worker".into()),
        Asserted::Unproven,
    ] {
        refused_by(
            fx.confirm_raw(
                who.clone(),
                &operator,
                json!({"action_token": action, "confirmation_token": token,
                    "confirmation": {"confirmed": true}}),
            ),
            OPERATOR,
            &format!("{} confirm of the release card", who.as_str()),
        );
    }

    // (c) FavCRM re-stamps the takeover after the card was drawn: the
    // confirm-time recompute refuses before any door write.
    door.restamp(CONV);
    let before = door.handoffs();
    refused_changed(
        fx.confirm(&operator, &action, &token),
        "release after handoffAt was re-stamped",
    );
    // ... and after it was cleared elsewhere.
    let (_, _, token) = card(&fx, &operator, &action, CONV);
    door.clear(CONV);
    refused_changed(
        fx.confirm(&operator, &action, &token),
        "release after handoffAt was cleared",
    );
    assert_eq!(door.handoffs(), before, "a stale release reached the door");
    door.restamp(CONV);

    // (d) Forged confirms: a different conversation, forged args, another
    // confirmation shape; then the burned card is spent.
    let (facts, _, token) = card(&fx, &operator, &action, CONV);
    let t2 = facts["handoff_at"].clone();
    for (case, extra) in [
        (
            "conversation_id in the tap",
            json!({"confirmation": {"confirmed": true, "conversation_id": OTHER}}),
        ),
        (
            "forged args",
            json!({"confirmation": {"confirmed": true}, "args": {"conversation_id": OTHER}}),
        ),
        (
            "forged operation",
            json!({"confirmation": {"confirmed": true}, "operation": "whatsapp.handoff.set"}),
        ),
    ] {
        let mut body = json!({"action_token": action, "confirmation_token": token});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        refused_by(
            fx.confirm_raw(Asserted::Operator, &operator, body),
            "unsupported screen confirmation request",
            &format!("release confirm with {case}"),
        );
    }
    assert!(
        fx.confirm_raw(
            Asserted::Operator,
            &operator,
            json!({"action_token": action, "confirmation_token": token,
                "confirmation": {"confirmed": true, "to_email": "x@example.test"}}),
        )
        .is_err(),
        "a release confirmed with another card's shape was accepted"
    );
    refused_by(
        fx.confirm(&operator, &action, &token),
        "spent or unknown",
        "a burned release card replayed",
    );
    assert_eq!(door.handoffs(), before, "a forged release reached the door");
    assert_eq!(door.at(CONV), t2.as_str().map(str::to_string));

    // Positive release: one confirmed card clears the card's own
    // conversation once.
    let (_, _, token) = card(&fx, &operator, &action, CONV);
    fx.confirm(&operator, &action, &token)
        .expect("confirmed release");
    let mut expected = before.clone();
    expected.push((CONV.to_string(), false));
    assert_eq!(
        door.handoffs(),
        expected,
        "release must reach the door once"
    );
    assert_eq!(door.at(CONV), None, "release must clear handoffAt");
    let shown = fx.op(
        "whatsapp_conversations_show",
        json!({"install_id": wa, "context_id": ctx, "conversation_id": CONV}),
    );
    assert_eq!(shown["handoffAt"], Value::Null, "{shown}");
    assert!(
        door.handoffs().iter().all(|(id, _)| id == CONV),
        "a request reached another conversation"
    );

    // (e) A door answer that breaks v3 is refused, never reported done.
    for lie in [
        Lie::Contract,
        Lie::Conversation,
        Lie::Nullness,
        Lie::Missing,
    ] {
        door.lie(lie);
        for method in natives {
            refused_by(
                fx.rpc(Asserted::Operator, method, scope(CONV)),
                "unpinned contract|not understood",
                &format!("{method} with a lying door"),
            );
        }
    }
    door.lie(Lie::None);
    drop(fx);

    // (f) A self-hosted daemon (no door) refuses both, and no door hears.
    let selfhosted = Fx::start(None);
    let (install, context) = selfhosted.install("wa-c1651", OPS);
    let before = door.handoffs();
    for method in natives {
        refused_by(
            selfhosted.rpc(
                Asserted::Operator,
                method,
                json!({"install_id": install, "context_id": context, "conversation_id": CONV}),
            ),
            SELF_HOSTED,
            &format!("self-hosted {method}"),
        );
    }
    assert_eq!(
        door.handoffs(),
        before,
        "a self-hosted daemon reached a door"
    );
}
