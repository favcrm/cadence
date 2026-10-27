//! CAD-1590 independent refusal acceptance. Reviewer-written from the
//! ticket, the operator-approved step-2 design (§3.4, §4) and the
//! `agenticos.whatsapp-door.v2` contract (agenticos-v2#746 at 7e2438c5)
//! by claude-opus-spec-reviewer, not by the implementer. Implementers
//! must not edit or weaken this file.
//!
//! The one refusal check for the WhatsApp templates and broadcasts host
//! verbs. A real daemon (`Shared` over its own socket) and a real public
//! board, with the door injected through `ServeOptions.hosted_whatsapp`
//! and pointed at a loopback stub that keeps the v2 semantics: prepare
//! stages the contract digest it computes itself (independently of the
//! host), every effect executes at most once per key, the test number
//! comes back masked. The stub counts every request per path, so
//! "refused before the door" is proved by an unchanged count.
//!
//! Refused:
//! (a) a member mount and a read-only board mount: every write CALL and
//!     every SLOT (template prepare/send, broadcast draft, test prepare/
//!     send, broadcast prepare/send/stop); member HTTP on every v2 route;
//! (b) template send, test send and broadcast send as a plain CALL, with a
//!     frame-forged digest or key, with altered variables, with an
//!     unconfirmed tap, with a mismatched native digest, with an effect id
//!     of another verb, after the review expired, and replayed (SLOT and
//!     native): none reaches a door send path;
//! (c) `send_at` outside now+5min..now+7d, before the door;
//! (d) an approved install that does not declare `whatsapp.*`;
//! (e) an agent or detached caller, natively and through the screen;
//! (f) a self-hosted daemon (no door).
//! Positive: the operator sends one template, one test and one broadcast
//! and stops one broadcast, each reaching the door exactly once.
#![cfg(feature = "test-seam")]

use crate::daemon::{serve, ServeOptions, Shared};
use crate::operator_auth::{BoardUser, Origin, PublicRealm};
use crate::platform::hosted_whatsapp::{broadcast_digest, template_digest, HostedWhatsapp};
use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const COMPANY: &str = "co_c1590";
const CONTRACT: &str = "agenticos.whatsapp-door.v2";
const CONVERSATION: &str = "conv_c1590";
const TEMPLATE: &str = "tpl_c1590";
const BROADCAST: &str = "cmp_c1590";
const EXPIRING: &str = "cmp_expiring";
const SCHEDULED: &str = "cmp_scheduled";
const TEST_TO: &str = "+85255550999";
const DRAFT_DIGEST: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const NOT_DECLARED: &str = "does not declare WhatsApp messaging";
const SELF_HOSTED: &str = "runs only on a hosted AgenticOS workspace";
const READ_ONLY: &str = "read-only screen admits only read operations";
const SEND_AT: &str = "send_at must be 5 minutes to 7 days ahead|send_at must be an RFC 3339 time";
const WHATSAPP_OPS: &[&str] = &[
    "whatsapp.status",
    "whatsapp.conversations.show",
    "whatsapp.templates.list",
    "whatsapp.template.show",
    "whatsapp.template.prepare",
    "whatsapp.template.send",
    "whatsapp.template.status",
    "whatsapp.broadcasts.list",
    "whatsapp.broadcast.show",
    "whatsapp.broadcast.estimate",
    "whatsapp.broadcast.results",
    "whatsapp.broadcast.draft",
    "whatsapp.broadcast.test.prepare",
    "whatsapp.broadcast.test.send",
    "whatsapp.broadcast.prepare",
    "whatsapp.broadcast.send",
    "whatsapp.broadcast.stop",
];
/// The door paths that execute an external effect.
const SEND_PATHS: &[&str] = &["/template/send", "/test/send", "/broadcasts/send", "/stop"];

// ---------- a loopback door with the v2 contract's semantics ----------

#[derive(Default)]
struct DoorState {
    hits: Vec<String>,
    /// key -> (digest, broadcast id) staged by a prepare.
    staged: HashMap<String, (String, Option<String>)>,
    /// key -> recorded outcome (executed once).
    executed: HashMap<String, Value>,
    /// The live template revision; a change voids a prepared send.
    revision: String,
    snapshots: u32,
}

#[derive(Clone)]
struct Door {
    url: String,
    state: Arc<Mutex<DoorState>>,
}

fn sha(canonical: &str) -> String {
    format!("{:x}", Sha256::digest(canonical.as_bytes()))
}

/// The contract's canonical forms, written from `whatsapp-door-v2.ts`
/// (`JSON.stringify` of the literal, in its key order), not from the host.
fn door_template_digest(body: &Value, revision: &str) -> String {
    let mut vars: Vec<(u64, Value)> = body["variables"]
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.parse().unwrap(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    vars.sort_by_key(|(n, _)| *n);
    let vars: Vec<String> = vars.iter().map(|(n, v)| format!("[{n},{v}]")).collect();
    sha(&format!(
        "{{\"v\":2,\"kind\":\"template\",\"conversationId\":{},\"templateId\":{},\"revision\":{},\"contentDigest\":{},\"variables\":[{}],\"effectId\":{}}}",
        body["conversationId"], body["templateId"], json!(revision), json!("a".repeat(64)), vars.join(","), body["effectId"]
    ))
}

fn door_broadcast_digest(id: &str, snapshot: &str, effect: &Value) -> String {
    sha(&format!(
        "{{\"v\":2,\"kind\":\"broadcast\",\"broadcastId\":{},\"snapshotId\":{},\"snapshotDigest\":{},\"effectId\":{}}}",
        json!(id), json!(snapshot), json!("b".repeat(64)), effect
    ))
}

fn door_test_digest(id: &str, body: &Value) -> String {
    sha(&format!(
        "{{\"v\":2,\"kind\":\"broadcast_test\",\"broadcastId\":{},\"draftDigest\":{},\"to\":{},\"effectId\":{}}}",
        json!(id), json!(DRAFT_DIGEST), body["to"], body["effectId"]
    ))
}

/// The contract's `maskWhatsappPhone`.
fn mask(phone: &str) -> String {
    let visible = &phone[phone.len() - 4..];
    format!("{}{visible}", "•".repeat((phone.len() - 4).max(4)))
}

impl Door {
    fn start() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr().to_ip().unwrap());
        let state = Arc::new(Mutex::new(DoorState {
            revision: "2026-10-19T00:00:00.000Z".into(),
            ..Default::default()
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
    fn hits(&self, suffix: &str) -> usize {
        let state = self.state.lock().unwrap();
        state.hits.iter().filter(|p| p.ends_with(suffix)).count()
    }
    fn sends(&self) -> usize {
        SEND_PATHS.iter().map(|p| self.hits(p)).sum()
    }
    fn set_revision(&self, revision: &str) {
        self.state.lock().unwrap().revision = revision.into();
    }
}

fn template(revision: &str) -> Value {
    json!({"contract": CONTRACT, "templateId": TEMPLATE, "metaTemplateId": "mt_1",
        "name": "synthetic_offer", "language": "en", "category": "MARKETING", "status": "APPROVED",
        "connectionId": "cc_1", "revision": revision, "contentDigest": "a".repeat(64),
        "header": {"format": "NONE", "text": null}, "bodyText": "Hi {{1}}, see you at {{2}}.",
        "footerText": null, "variables": [{"index": 1, "sample": null}, {"index": 2, "sample": null}],
        "sendable": true})
}

fn broadcast(id: &str, revision: &str) -> Value {
    let status = if id == SCHEDULED {
        "SCHEDULED"
    } else {
        "DRAFT"
    };
    json!({"contract": CONTRACT, "broadcastId": id, "name": "Synthetic launch", "status": status,
        "template": {"templateId": TEMPLATE, "name": "synthetic_offer", "language": "en", "revision": revision},
        "bodyParameters": ["Ada", "7pm"], "headerMediaUrl": null,
        "audience": {"source": "segment", "segmentId": "seg_1", "accountIds": []},
        "exclusions": {"accountIds": [], "campaignIds": []},
        "scheduledAt": if id == SCHEDULED { json!("2030-01-01T00:00:00.000Z") } else { Value::Null },
        "sentAt": null, "deliveryStats": {"sent": 3}, "errorMessage": null, "snapshot": null})
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
    let parts: Vec<&str> = rest.trim_start_matches('/').split('/').collect();
    let revision = door.revision.clone();
    // Stage under a key, or refuse a different digest for the same key.
    let stage = |door: &mut DoorState, key: &str, digest: &str, id: Option<&str>| {
        if door.staged.get(key).is_some_and(|(d, _)| d != digest) {
            return false;
        }
        door.staged
            .insert(key.into(), (digest.into(), id.map(str::to_string)));
        true
    };
    // Execute a staged effect at most once per key.
    let execute = |door: &mut DoorState, key: &str, make: &dyn Fn(&str, Option<&str>) -> Value| {
        let Some((staged, id)) = door.staged.get(key).cloned() else {
            return refuse(409, "not_prepared");
        };
        if body["digest"].as_str() != Some(staged.as_str()) {
            return refuse(409, "digest_mismatch");
        }
        let repeated = door.executed.contains_key(key);
        let outcome = door
            .executed
            .entry(key.into())
            .or_insert_with(|| make(key, id.as_deref()))
            .clone();
        let mut outcome = outcome;
        outcome["repeated"] = json!(repeated);
        ok(outcome)
    };
    match parts.as_slice() {
        ["conversations", id] if *id == CONVERSATION => {
            ok(json!({"contract": "agenticos.whatsapp-door.v1",
            "conversationId": CONVERSATION, "channelConnectionId": "cc_1", "status": "open",
            "contact": {"id": "ct_1", "name": "Synthetic Diner", "phone": "+85200000000",
                "email": null, "whatsappId": "85200000000"},
            "replyMode": "template_required", "freeformUntil": null, "lastInboundAt": null,
            "lastOutboundAt": null, "unreadCount": 0, "handoffAt": null}))
        }
        ["templates"] => ok(
            json!({"contract": CONTRACT, "templates": [template(&revision)], "connectionId": "cc_1"}),
        ),
        ["templates", id] if *id == TEMPLATE => ok(template(&revision)),
        ["template", "prepare"] => {
            let Some(key) = key else {
                return refuse(400, "invalid_request");
            };
            let digest = door_template_digest(body, &revision);
            if !stage(&mut door, &key, &digest, None) {
                return refuse(409, "key_conflict");
            }
            ok(
                json!({"contract": CONTRACT, "key": key, "digest": digest, "decision": "granted",
                "renderedBody": "Hi Ada, see you at 7pm.", "templateName": "synthetic_offer",
                "language": "en", "revision": revision, "replyMode": "template_required",
                "expiresAt": "2030-01-01T00:00:00.000Z"}),
            )
        }
        ["template", "send"] => {
            let Some(key) = key else {
                return refuse(400, "invalid_request");
            };
            execute(&mut door, &key, &|key, _| {
                json!({"contract": CONTRACT, "kind": "template",
                "key": key, "decision": "granted", "status": "sent", "executed": true,
                "favcrmMessageId": "fm_c1590", "providerMessageId": "wamid.c1590",
                "deduped": false, "errorCode": null})
            })
        }
        ["template", key] => match door.executed.get(*key) {
            Some(outcome) => {
                let mut outcome = outcome.clone();
                outcome["repeated"] = json!(true);
                ok(outcome)
            }
            None => refuse(404, "not_found"),
        },
        ["broadcasts"] => ok(json!({"contract": CONTRACT, "broadcasts": [], "cursor": null})),
        ["broadcasts", "draft"] => ok(json!({"contract": CONTRACT, "broadcastId": BROADCAST,
            "status": "DRAFT", "replayed": false})),
        ["broadcasts", "send"] => {
            let Some(key) = key else {
                return refuse(400, "invalid_request");
            };
            execute(&mut door, &key, &|key, id| {
                json!({"contract": CONTRACT, "key": key,
                "decision": "granted", "status": "scheduled", "broadcastId": id,
                "scheduledAt": "2030-01-01T00:01:00.000Z", "undoUntil": null, "errorCode": null})
            })
        }
        ["broadcasts", id] => ok(broadcast(id, &revision)),
        ["broadcasts", _, "estimate"] => {
            ok(json!({"contract": CONTRACT, "valid": true, "errors": [],
            "estimate": {"total": 10, "manuallyExcluded": 1, "missingAddress": 1, "missingConsent": 1,
                "optedOut": 1, "eligible": 6},
            "preview": {"renderedBody": "Hi Ada, see you at 7pm.", "templateName": "synthetic_offer", "language": "en"}}))
        }
        ["broadcasts", _, "results"] => {
            ok(json!({"contract": CONTRACT, "recipients": [], "total": 0, "cursor": null}))
        }
        ["broadcasts", id, "test", "prepare"] => {
            let Some(key) = key else {
                return refuse(400, "invalid_request");
            };
            let digest = door_test_digest(id, body);
            if !stage(&mut door, &key, &digest, Some(id)) {
                return refuse(409, "key_conflict");
            }
            ok(
                json!({"contract": CONTRACT, "key": key, "digest": digest, "decision": "granted",
                "to": mask(body["to"].as_str().unwrap_or("0000")), "expiresAt": "2030-01-01T00:00:00.000Z"}),
            )
        }
        ["broadcasts", _, "test", "send"] => {
            let Some(key) = key else {
                return refuse(400, "invalid_request");
            };
            execute(&mut door, &key, &|key, _| {
                json!({"contract": CONTRACT, "key": key,
                "decision": "granted", "status": "queued", "to": mask(TEST_TO), "errorCode": null})
            })
        }
        ["broadcasts", id, "prepare"] => {
            let Some(key) = key else {
                return refuse(400, "invalid_request");
            };
            door.snapshots += 1;
            let snapshot = format!("snap_{}", door.snapshots);
            let digest = door_broadcast_digest(id, &snapshot, &body["effectId"]);
            if !stage(&mut door, &key, &digest, Some(id)) {
                return refuse(409, "key_conflict");
            }
            let expires = if *id == EXPIRING {
                "2020-01-01T00:00:00.000Z"
            } else {
                "2030-01-01T00:00:00.000Z"
            };
            ok(
                json!({"contract": CONTRACT, "key": key, "digest": digest, "decision": "granted",
                "expiresAt": expires, "snapshot": {"snapshotId": snapshot, "snapshotDigest": "b".repeat(64),
                "expiresAt": expires, "connection": {"connectionId": "cc_1", "displayPhoneNumber": "+852 0000 0000"},
                "template": {"templateId": TEMPLATE, "name": "synthetic_offer", "language": "en",
                    "revision": revision, "contentDigest": "a".repeat(64)},
                "variables": {"bodyParameters": ["Ada", "7pm"], "headerMediaUrl": null},
                "audience": {"source": "segment", "segmentId": "seg_1", "accountIds": [],
                    "exclusions": {"accountIds": [], "campaignIds": []}},
                "counts": {"total": 10, "manuallyExcluded": 1, "missingAddress": 1, "missingConsent": 1,
                    "optedOut": 1, "eligible": 6},
                "audienceDigest": "d".repeat(64), "sendAt": body["sendAt"]}}),
            )
        }
        ["broadcasts", id, "stop"] => ok(json!({"contract": CONTRACT, "broadcastId": id,
            "status": "STOPPED", "alreadySent": 3, "cancelled": 7})),
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
        let root = tempfile::Builder::new().prefix("c1590").tempdir().unwrap();
        let state = root.path().to_path_buf();
        let pm = state.join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let (port, reservation) = crate::test_ports::reserve_board_port();
        let host = format!("c1590.board.localhost:{port}");
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
        let manifest = format!("---\napp: {app}\ntitle: Synthetic WhatsApp\nversion: '0.1.0'\nsummary: Isolated CAD1590 refusal fixture\nneeds:\n  connections: []\n  requires:\n    contracts:\n      app-screens: [1]\n      screen-records: [1]\n---\n\nSynthetic only.\n");
        std::fs::write(source.join("app.md"), manifest).unwrap();
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(source.join("workflows/synthetic.md"), "---\ntitle: Synthetic refusal fixture\ngoal: Synthetic text only\ninputs:\n  writer: { ask: Synthetic writer }\n  reviewer: { ask: Synthetic reviewer }\ndistinct: [writer, reviewer]\n---\n\n## Synthetic draft\nagent: {{writer}}\nsize: S\naction: local.text.produce\n\nProduce synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n\n## Synthetic review\nagent: {{reviewer}}\nsize: S\ndepends_on: 1\naction: local.text.review\n\nReview the synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n").unwrap();
        let js = "(()=>{document.getElementById('root').textContent='c1590';})();";
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
                    sub: "synthetic-c1590-member".into(),
                    email: "c1590-member@example.test".into(),
                    name: "Synthetic member".into(),
                    role: "member".into(),
                    handle: "c1590-member".into(),
                },
                "c1590-member-jti",
                now + 120,
                "c1590-isolated-acceptance",
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

fn zero(prefix: &str) -> String {
    format!("{prefix}{}", "0".repeat(32))
}

/// Every native v2 verb with exactly its allowed fields, so only the gate
/// under test can refuse it.
fn native_verbs(install: &str, context: &str) -> Vec<(&'static str, Value)> {
    let scope = |extra: Value| {
        let mut body = json!({"install_id": install, "context_id": context});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        body
    };
    let digest = "a".repeat(64);
    vec![
        (
            "whatsapp_templates_list",
            scope(json!({"status": "approved"})),
        ),
        (
            "whatsapp_template_show",
            scope(json!({"template_id": TEMPLATE})),
        ),
        (
            "whatsapp_template_prepare",
            scope(
                json!({"conversation_id": CONVERSATION, "template_id": TEMPLATE,
                "variables": {"1": "Ada", "2": "7pm"}}),
            ),
        ),
        (
            "whatsapp_template_send",
            scope(json!({"effect_id": zero("wat_"), "digest": digest})),
        ),
        (
            "whatsapp_template_status",
            scope(json!({"effect_id": zero("wat_")})),
        ),
        ("whatsapp_broadcasts_list", scope(json!({"limit": 10}))),
        (
            "whatsapp_broadcast_show",
            scope(json!({"broadcast_id": BROADCAST})),
        ),
        (
            "whatsapp_broadcast_estimate",
            scope(json!({"broadcast_id": BROADCAST})),
        ),
        (
            "whatsapp_broadcast_results",
            scope(json!({"broadcast_id": BROADCAST})),
        ),
        (
            "whatsapp_broadcast_draft",
            scope(json!({"name": "Synthetic launch", "template_id": TEMPLATE,
                "body_parameters": ["Ada", "7pm"],
                "audience": {"source": "segment", "segment_id": "seg_1"}})),
        ),
        (
            "whatsapp_broadcast_test_prepare",
            scope(json!({"broadcast_id": BROADCAST, "to": TEST_TO})),
        ),
        (
            "whatsapp_broadcast_test_send",
            scope(json!({"effect_id": zero("wbt_"), "digest": digest})),
        ),
        (
            "whatsapp_broadcast_prepare",
            scope(json!({"broadcast_id": BROADCAST})),
        ),
        (
            "whatsapp_broadcast_send",
            scope(json!({"effect_id": zero("wab_"), "digest": digest})),
        ),
        (
            "whatsapp_broadcast_stop",
            scope(json!({"broadcast_id": SCHEDULED})),
        ),
    ]
}

/// The screen writes: (method, operation, args), each a well-formed call.
fn screen_writes() -> Vec<(&'static str, &'static str, Value)> {
    let (call, slot) = ("app_screen_service_call", "app_screen_service_slot_prepare");
    vec![
        (
            call,
            "whatsapp.template.prepare",
            json!({"conversation_id": CONVERSATION, "template_id": TEMPLATE,
                "variables": {"1": "Ada", "2": "7pm"}}),
        ),
        (
            call,
            "whatsapp.broadcast.draft",
            json!({"name": "Synthetic launch", "template_id": TEMPLATE,
                "body_parameters": ["Ada"], "audience": {"source": "segment", "segment_id": "seg_1"}}),
        ),
        (
            call,
            "whatsapp.broadcast.test.prepare",
            json!({"broadcast_id": BROADCAST, "to": TEST_TO}),
        ),
        (
            call,
            "whatsapp.broadcast.prepare",
            json!({"broadcast_id": BROADCAST}),
        ),
        (
            slot,
            "whatsapp.template.send",
            json!({"effect_id": zero("wat_"), "conversation_id": CONVERSATION,
                "template_id": TEMPLATE, "variables": {"1": "Ada", "2": "7pm"}}),
        ),
        (
            slot,
            "whatsapp.broadcast.test.send",
            json!({"effect_id": zero("wbt_")}),
        ),
        (
            slot,
            "whatsapp.broadcast.send",
            json!({"effect_id": zero("wab_")}),
        ),
        (
            slot,
            "whatsapp.broadcast.stop",
            json!({"broadcast_id": SCHEDULED}),
        ),
    ]
}

/// One SLOT through the card: (card facts, confirmation token).
fn card(fx: &Fx, op: &Session, action: &str, operation: &str, args: Value) -> (Value, String) {
    let card = fx
        .service(
            Asserted::Operator,
            "app_screen_service_slot_prepare",
            op,
            action,
            operation,
            args,
        )
        .unwrap_or_else(|error| panic!("{operation} SLOT prepare: {error}"));
    let token = card["confirmation_token"].as_str().unwrap().to_owned();
    (card["card"]["facts"].clone(), token)
}

#[test]
fn cad1590_templates_and_broadcasts_refuse_member_agent_forged_replayed_and_self_hosted() {
    // The host digests are #746's golden vectors
    // (packages/contracts/src/whatsapp-door-v2.test.ts at 7e2438c5).
    let vars = BTreeMap::from([
        (10, "ten".to_string()),
        (2, "多謝".to_string()),
        (1, "Ada".to_string()),
    ]);
    assert_eq!(
        template_digest(
            "conv_1",
            "tpl_1",
            "2026-10-19T00:00:00.000Z",
            &"a".repeat(64),
            &vars,
            "eff_1"
        ),
        "54dddc90108370abd0c63850a8f72d26c7451a734e945f778241d07e57f98105"
    );
    assert_eq!(
        broadcast_digest("cmp_1", "snap_1", &"b".repeat(64), "eff_2"),
        "bf36c256b002f0b9933542dbae553084442eb0bf376f9edab92052f933e392e8"
    );

    let door = Door::start();
    let fx = Fx::start(Some(&door));
    let (wa, wa_ctx) = fx.install("wa-c1590", WHATSAPP_OPS);
    let (plain, plain_ctx) = fx.install("plain-c1590", &["records.list"]);
    assert_eq!(fx.door_flag(&wa), json!(true), "declaring receipt");
    assert_eq!(fx.door_flag(&plain), json!(false), "non-declaring receipt");
    let operator = fx.local_operator();

    // (d) A non-declaring approved install: every v2 verb, before the door.
    let before = door.count();
    for (method, params) in native_verbs(&plain, &plain_ctx) {
        refused_by(
            fx.rpc(Asserted::Operator, method, params),
            NOT_DECLARED,
            &format!("non-declaring operator {method}"),
        );
    }
    let plain_action = fx.mount(&operator, &plain, &plain_ctx, 1, false);
    for (method, operation, args) in screen_writes() {
        refused_by(
            fx.service(
                Asserted::Operator,
                method,
                &operator,
                &plain_action,
                operation,
                args,
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

    // (e) Agent and detached callers on the declaring install, natively.
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

    // (a) A member mount and a read-only board mount: reads only.
    let member = fx.member();
    let member_action = fx.mount(&member, &wa, &wa_ctx, 2, false);
    let ro_action = fx.mount(&operator, &wa, &wa_ctx, 3, true);
    let listed = fx
        .service(
            Asserted::Operator,
            "app_screen_service_call",
            &member,
            &member_action,
            "whatsapp.templates.list",
            json!({}),
        )
        .expect("member read of whatsapp.templates.list");
    assert_eq!(listed["templates"][0]["templateId"], TEMPLATE, "{listed}");
    let before = door.count();
    for (who, session, action) in [
        ("member", &member, &member_action),
        ("read-only board", &operator, &ro_action),
    ] {
        for (method, operation, args) in screen_writes() {
            refused_by(
                fx.service(Asserted::Operator, method, session, action, operation, args),
                READ_ONLY,
                &format!("{who} {operation} via {method}"),
            );
        }
    }
    // Over HTTP every v2 route is the operator's: a member is refused.
    for (path, (_, params)) in [
        "/api/whatsapp/templates",
        "/api/whatsapp/template",
        "/api/whatsapp/template/prepare",
        "/api/whatsapp/template/send",
        "/api/whatsapp/template/status",
        "/api/whatsapp/broadcasts",
        "/api/whatsapp/broadcast",
        "/api/whatsapp/broadcast/estimate",
        "/api/whatsapp/broadcast/results",
        "/api/whatsapp/broadcast/draft",
        "/api/whatsapp/broadcast/test/prepare",
        "/api/whatsapp/broadcast/test/send",
        "/api/whatsapp/broadcast/prepare",
        "/api/whatsapp/broadcast/send",
        "/api/whatsapp/broadcast/stop",
    ]
    .into_iter()
    .zip(native_verbs(&wa, &wa_ctx))
    {
        let (status, answer) = fx.post(&member, path, &params);
        assert_eq!(status, 403, "member {path} must be refused: {answer}");
    }
    assert_eq!(
        door.count(),
        before,
        "a member/read-only write reached the door"
    );

    // (c) send_at outside now+5min..now+7d is refused before the door.
    let soon = (chrono::Utc::now() + chrono::Duration::minutes(1)).to_rfc3339();
    let late = (chrono::Utc::now() + chrono::Duration::days(8)).to_rfc3339();
    let action = fx.mount(&operator, &wa, &wa_ctx, 4, false);
    for at in [soon.as_str(), late.as_str(), "next tuesday"] {
        refused_by(
            fx.rpc(
                Asserted::Operator,
                "whatsapp_broadcast_prepare",
                json!({"install_id": wa, "context_id": wa_ctx, "broadcast_id": BROADCAST, "send_at": at}),
            ),
            SEND_AT,
            &format!("native broadcast.prepare send_at {at}"),
        );
        refused_by(
            fx.service(
                Asserted::Operator,
                "app_screen_service_call",
                &operator,
                &action,
                "whatsapp.broadcast.prepare",
                json!({"broadcast_id": BROADCAST, "send_at": at}),
            ),
            SEND_AT,
            &format!("screen broadcast.prepare send_at {at}"),
        );
    }
    assert_eq!(door.hits("/prepare"), 0, "a bad send_at reached the door");

    // (b) Template send: prepared, then refused every way but one tap.
    let prepared = fx
        .service(
            Asserted::Operator,
            "app_screen_service_call",
            &operator,
            &action,
            "whatsapp.template.prepare",
            json!({"conversation_id": CONVERSATION, "template_id": TEMPLATE,
                "variables": {"1": "Ada", "2": "7pm"}}),
        )
        .expect("operator template.prepare");
    let tpl_effect = prepared["effect_id"].as_str().unwrap().to_owned();
    let tpl_digest = prepared["digest"].as_str().unwrap().to_owned();
    assert_eq!(
        tpl_digest,
        door_template_digest(
            &json!({"conversationId": CONVERSATION, "templateId": TEMPLATE,
                "variables": {"1": "Ada", "2": "7pm"}, "effectId": tpl_effect}),
            "2026-10-19T00:00:00.000Z"
        ),
        "host template digest is not the contract digest"
    );
    let tpl_args = json!({"effect_id": tpl_effect, "conversation_id": CONVERSATION,
        "template_id": TEMPLATE, "variables": {"1": "Ada", "2": "7pm"}});

    // (b) Test send and broadcast send, prepared by the operator.
    let tested = fx
        .service(
            Asserted::Operator,
            "app_screen_service_call",
            &operator,
            &action,
            "whatsapp.broadcast.test.prepare",
            json!({"broadcast_id": BROADCAST, "to": TEST_TO}),
        )
        .expect("operator test.prepare");
    let test_effect = tested["effect_id"].as_str().unwrap().to_owned();
    let test_digest = tested["digest"].as_str().unwrap().to_owned();
    assert_eq!(tested["to"], json!(mask(TEST_TO)), "{tested}");
    let reviewed = fx
        .service(
            Asserted::Operator,
            "app_screen_service_call",
            &operator,
            &action,
            "whatsapp.broadcast.prepare",
            json!({"broadcast_id": BROADCAST}),
        )
        .expect("operator broadcast.prepare");
    let bc_effect = reviewed["effect_id"].as_str().unwrap().to_owned();
    let bc_digest = reviewed["digest"].as_str().unwrap().to_owned();
    assert_eq!(
        bc_digest,
        door_broadcast_digest(BROADCAST, "snap_1", &json!(bc_effect)),
        "host broadcast digest is not the contract digest"
    );
    let sends = [
        (
            "whatsapp.template.send",
            "whatsapp_template_send",
            tpl_args.clone(),
            tpl_effect.clone(),
            tpl_digest.clone(),
        ),
        (
            "whatsapp.broadcast.test.send",
            "whatsapp_broadcast_test_send",
            json!({"effect_id": test_effect}),
            test_effect.clone(),
            test_digest.clone(),
        ),
        (
            "whatsapp.broadcast.send",
            "whatsapp_broadcast_send",
            json!({"effect_id": bc_effect}),
            bc_effect.clone(),
            bc_digest.clone(),
        ),
    ];
    for (operation, method, args, effect, digest) in &sends {
        // A plain CALL is never a send.
        refused_by(
            fx.service(
                Asserted::Operator,
                "app_screen_service_call",
                &operator,
                &action,
                operation,
                args.clone(),
            ),
            "not an implemented non-protected screen CALL",
            &format!("{operation} as a CALL"),
        );
        // The frame cannot supply a digest or a key.
        for forged in ["digest", "key"] {
            let mut forged_args = args.clone();
            forged_args[forged] = json!(digest);
            refused_by(
                fx.service(
                    Asserted::Operator,
                    "app_screen_service_slot_prepare",
                    &operator,
                    &action,
                    operation,
                    forged_args,
                ),
                "closed shape",
                &format!("{operation} SLOT with forged {forged}"),
            );
        }
        // Agent and detached callers through the screen.
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
                    operation,
                    args.clone()
                )
                .is_err(),
                "agent/detached {operation} SLOT accepted"
            );
        }
        // An unconfirmed tap does nothing and burns the token.
        let (facts, token) = card(&fx, &operator, &action, operation, args.clone());
        assert_eq!(
            facts["digest"],
            json!(digest),
            "{operation} card digest: {facts}"
        );
        assert!(
            fx.confirm(&operator, &action, &token, false).is_err(),
            "{operation}: an unconfirmed tap was accepted"
        );
        refused_by(
            fx.confirm(&operator, &action, &token, true),
            "spent or unknown",
            &format!("{operation}: a burned confirmation replayed"),
        );
        // A native send with another digest is refused locally.
        refused_by(
            fx.rpc(
                Asserted::Operator,
                method,
                json!({"install_id": wa, "context_id": wa_ctx, "effect_id": effect, "digest": "f".repeat(64)}),
            ),
            "digest does not match the prepared",
            &format!("native {method} with a mismatched digest"),
        );
    }
    // An effect id never crosses verbs.
    for (method, effect, digest) in [
        ("whatsapp_broadcast_send", &test_effect, &test_digest),
        ("whatsapp_broadcast_test_send", &tpl_effect, &tpl_digest),
        ("whatsapp_template_send", &bc_effect, &bc_digest),
    ] {
        refused_by(
            fx.rpc(
                Asserted::Operator,
                method,
                json!({"install_id": wa, "context_id": wa_ctx, "effect_id": effect, "digest": digest}),
            ),
            "effect id is invalid",
            &format!("{method} with another verb's effect"),
        );
    }
    // Altered variables are not the prepared template send.
    let mut altered = tpl_args.clone();
    altered["variables"]["2"] = json!("9pm");
    refused_by(
        fx.service(
            Asserted::Operator,
            "app_screen_service_slot_prepare",
            &operator,
            &action,
            "whatsapp.template.send",
            altered,
        ),
        "changed since it was prepared",
        "template.send SLOT with altered variables",
    );
    // A template edited at FavCRM after prepare voids the prepared send.
    door.set_revision("2026-10-20T00:00:00.000Z");
    refused_by(
        fx.service(
            Asserted::Operator,
            "app_screen_service_slot_prepare",
            &operator,
            &action,
            "whatsapp.template.send",
            tpl_args.clone(),
        ),
        "changed since it was prepared",
        "template.send SLOT after the template changed",
    );
    door.set_revision("2026-10-19T00:00:00.000Z");
    // A review past its stale bound is refused.
    let expiring = fx
        .service(
            Asserted::Operator,
            "app_screen_service_call",
            &operator,
            &action,
            "whatsapp.broadcast.prepare",
            json!({"broadcast_id": EXPIRING}),
        )
        .expect("operator broadcast.prepare (expiring)");
    refused_by(
        fx.service(
            Asserted::Operator,
            "app_screen_service_slot_prepare",
            &operator,
            &action,
            "whatsapp.broadcast.send",
            json!({"effect_id": expiring["effect_id"]}),
        ),
        "review expired",
        "broadcast.send SLOT after its review expired",
    );
    assert_eq!(door.sends(), 0, "a refused send reached a door send path");

    // Positive: one confirmed tap per send reaches the door exactly once.
    for (operation, _, args, _, digest) in &sends {
        let (facts, token) = card(&fx, &operator, &action, operation, args.clone());
        if *operation == "whatsapp.broadcast.send" {
            // The card is the reviewed snapshot: template and revision,
            // variables, eligible and excluded counts, send time.
            assert_eq!(
                facts["snapshot"]["template"]["revision"], "2026-10-19T00:00:00.000Z",
                "{facts}"
            );
            assert_eq!(facts["body_parameters"], json!(["Ada", "7pm"]), "{facts}");
            assert_eq!(facts["eligible"], 6, "{facts}");
            assert_eq!(facts["excluded"], 4, "{facts}");
            assert_eq!(facts["send_at"], Value::Null, "{facts}");
        }
        if *operation == "whatsapp.template.send" {
            assert_eq!(facts["rendered_body"], "Hi Ada, see you at 7pm.", "{facts}");
            assert_eq!(
                facts["template"]["revision"], "2026-10-19T00:00:00.000Z",
                "{facts}"
            );
        }
        if *operation == "whatsapp.broadcast.test.send" {
            assert_eq!(facts["to"], json!(mask(TEST_TO)), "{facts}");
        }
        let before = door.sends();
        let sent = fx
            .confirm(&operator, &action, &token, true)
            .unwrap_or_else(|error| panic!("confirmed {operation}: {error}"));
        let state = &sent["outcome"]["state"];
        assert!(
            state == "sent" || state == "accepted",
            "{operation} confirmed but not sent/accepted: {sent} (digest {digest})"
        );
        assert_eq!(
            door.sends(),
            before + 1,
            "{operation} must reach the door once"
        );
    }
    assert_eq!(door.sends(), 3);

    // Replay: SLOT prepare again and native send again are refused here.
    for (operation, method, args, effect, digest) in &sends {
        refused_by(
            fx.service(
                Asserted::Operator,
                "app_screen_service_slot_prepare",
                &operator,
                &action,
                operation,
                args.clone(),
            ),
            "not awaiting approval",
            &format!("replayed {operation} SLOT"),
        );
        refused_by(
            fx.rpc(
                Asserted::Operator,
                method,
                json!({"install_id": wa, "context_id": wa_ctx, "effect_id": effect, "digest": digest}),
            ),
            "already attempted",
            &format!("replayed native {method}"),
        );
    }
    assert_eq!(door.sends(), 3, "a replay reached a door send path");
    let status = fx.op(
        "whatsapp_template_status",
        json!({"install_id": wa, "context_id": wa_ctx, "effect_id": tpl_effect}),
    );
    assert_eq!(status["state"], "sent", "{status}");
    assert_eq!(door.sends(), 3, "template.status executed a send");

    // Stop: an unconfirmed tap stops nothing; one confirmed tap stops once.
    let stop_args = json!({"broadcast_id": SCHEDULED});
    let (_, token) = card(
        &fx,
        &operator,
        &action,
        "whatsapp.broadcast.stop",
        stop_args.clone(),
    );
    assert!(fx.confirm(&operator, &action, &token, false).is_err());
    assert_eq!(
        door.hits("/stop"),
        0,
        "an unconfirmed stop reached the door"
    );
    refused_by(
        fx.service(
            Asserted::Operator,
            "app_screen_service_slot_prepare",
            &operator,
            &action,
            "whatsapp.broadcast.stop",
            json!({"broadcast_id": BROADCAST}),
        ),
        "nothing to stop",
        "stop of a draft",
    );
    let (_, token) = card(
        &fx,
        &operator,
        &action,
        "whatsapp.broadcast.stop",
        stop_args,
    );
    let stopped = fx
        .confirm(&operator, &action, &token, true)
        .expect("confirmed broadcast.stop");
    assert_eq!(stopped["status"], "STOPPED", "{stopped}");
    assert_eq!(door.hits("/stop"), 1, "the stop must reach the door once");
    drop(fx);

    // (f) A self-hosted daemon (no door) refuses every v2 verb of a
    // declaring, approved install, and no door hears of it.
    let selfhosted = Fx::start(None);
    let (install, context) = selfhosted.install("wa-c1590", WHATSAPP_OPS);
    assert_eq!(selfhosted.door_flag(&install), json!(true));
    let before = door.count();
    for (method, params) in native_verbs(&install, &context) {
        refused_by(
            selfhosted.rpc(Asserted::Operator, method, params),
            SELF_HOSTED,
            &format!("self-hosted {method}"),
        );
    }
    assert_eq!(door.count(), before, "a self-hosted daemon reached a door");
}
