//! CAD-1063: hosted CRM email goes through the platform's tenant
//! email door (`POST /v1/runtime/email/send` on `api.internal`), not
//! raw SMTP — the tenant container has no internet egress.
//!
//! A loopback fake door models the platform's contract: strict body,
//! `idempotency-key` header, a ledger that binds key to content digest
//! (a replay with different bytes is `key_conflict`), `pending` until
//! the owner (or a standing grant) clears a recipient, at most one
//! provider call per key, and the typed negative outcomes. No live
//! network, no real credential; every address is synthetic.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::daemon::ServeOptions;
use cadence_agent::issue::Pm;
use cadence_agent::platform::hosted_email::{content_digest, HostedEmail, HostedMessage};
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RECIPIENT: &str = "operator@example.com";
const FROM: &str = "tenant@mail.example.test";
const PROFILE_A: &str = r#"{"schema":1,"display_name":"Amina Diallo","email":"amina@example.com","tags":[],"consent":{"email":"granted"}}"#;
const PROFILE_B: &str = r#"{"schema":1,"display_name":"Boris Feld","email":"boris@example.com","tags":[],"consent":{"email":"denied"}}"#;
const PROFILE_C: &str = r#"{"schema":1,"display_name":"Cleo Boone","email":"cleo@example.com","tags":[],"consent":{"email":"granted"}}"#;

// ---------- the fake platform door ----------

#[derive(Clone, Debug)]
struct Seen {
    path: String,
    key: String,
    authorization: Option<String>,
    body: Value,
}

#[derive(Clone, PartialEq)]
enum Policy {
    /// Every key is `approved` on first presentation (a standing grant).
    All,
    /// Nothing is approved: every key stays `pending`.
    Nobody,
    /// Only these recipients clear; the rest stay `pending`.
    Only(HashSet<String>),
}

#[derive(Clone, PartialEq)]
enum Fault {
    None,
    /// Provider definitively refused.
    ProviderFailed,
    /// Provider outcome unknown: the ledger says `uncertain`.
    Uncertain,
    /// The platform proves the provider was never called.
    NotExecuted,
    /// 503 capability_unavailable with no delivery data.
    Unavailable503,
    /// Close the connection after reading the request.
    Reset,
    /// Read the request and never answer.
    Hang,
}

struct Entry {
    digest: String,
    cleared: bool,
    provider_id: Option<String>,
}

struct Door {
    seen: Vec<Seen>,
    ledger: HashMap<String, Entry>,
    policy: Policy,
    fault: Fault,
    /// Remaining requests that answer the fault before behaving.
    fault_budget: usize,
    provider_sends: usize,
}

struct FakeDoor {
    url: String,
    door: Arc<Mutex<Door>>,
}

impl FakeDoor {
    fn start(policy: Policy) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let door = Arc::new(Mutex::new(Door {
            seen: Vec::new(),
            ledger: HashMap::new(),
            policy,
            fault: Fault::None,
            fault_budget: 0,
            provider_sends: 0,
        }));
        let shared = Arc::clone(&door);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || serve_one(stream, shared));
            }
        });
        Self { url, door }
    }

    fn set_policy(&self, policy: Policy) {
        self.door.lock().unwrap().policy = policy;
    }

    fn set_fault(&self, fault: Fault, budget: usize) {
        let mut door = self.door.lock().unwrap();
        door.fault = fault;
        door.fault_budget = budget;
    }

    fn seen(&self) -> Vec<Seen> {
        self.door.lock().unwrap().seen.clone()
    }

    fn provider_sends(&self) -> usize {
        self.door.lock().unwrap().provider_sends
    }

    fn hosted(&self) -> HostedEmail {
        HostedEmail::new(&self.url, FROM, "Tenant Co").unwrap()
    }
}

fn reply(stream: &mut TcpStream, status: u16, body: &Value) {
    let text = body.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
}

fn envelope(code: &str) -> Value {
    json!({"ok": false, "error": {"code": code, "message": code}})
}

fn delivery(
    key: &str,
    decision: &str,
    status: &str,
    provider: Option<&str>,
    error: Option<&str>,
    repeated: bool,
) -> Value {
    json!({
        "key": key, "decision": decision, "status": status,
        "executed": status == "sent", "providerMessageId": provider,
        "errorCode": error, "repeated": repeated,
    })
}

fn serve_one(mut stream: TcpStream, door: Arc<Mutex<Door>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let length = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0);
    while buf.len() < head_end + length {
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body: Value =
        serde_json::from_slice(&buf[head_end..head_end + length]).unwrap_or(Value::Null);
    let header = |name: &str| {
        head.lines().find_map(|l| {
            let lower = l.to_ascii_lowercase();
            lower
                .starts_with(&format!("{name}:"))
                .then(|| l[name.len() + 1..].trim().to_string())
        })
    };
    let path = head
        .lines()
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .unwrap_or("")
        .to_string();
    let key = header("idempotency-key").unwrap_or_default();
    let mut d = door.lock().unwrap();
    d.seen.push(Seen {
        path: path.clone(),
        key: key.clone(),
        authorization: header("authorization"),
        body: body.clone(),
    });
    let fault = if d.fault_budget > 0 {
        d.fault_budget -= 1;
        d.fault.clone()
    } else {
        Fault::None
    };
    match fault {
        Fault::Reset => return,
        Fault::Hang => {
            drop(d);
            std::thread::sleep(Duration::from_secs(5));
            return;
        }
        Fault::Unavailable503 => {
            return reply(&mut stream, 503, &envelope("capability_unavailable"))
        }
        _ => {}
    }
    // The contract: strict body, valid key header.
    let strict = body.as_object().is_some_and(|o| {
        o.keys()
            .all(|k| matches!(k.as_str(), "to" | "subject" | "text" | "html"))
            && o.contains_key("to")
            && o.contains_key("subject")
    });
    let key_ok = (8..=128).contains(&key.len())
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if path != "/v1/runtime/email/send" || !strict || !key_ok {
        return reply(&mut stream, 400, &envelope("invalid_request"));
    }
    let to: Vec<String> = body["to"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_lowercase))
                .collect()
        })
        .unwrap_or_default();
    let message = HostedMessage {
        to: to.first().cloned().unwrap_or_default(),
        subject: body["subject"].as_str().unwrap_or("").to_string(),
        text: body["text"].as_str().unwrap_or("").to_string(),
        html: body["html"].as_str().unwrap_or("").to_string(),
    };
    let digest = content_digest(&message);
    let repeated = d.ledger.contains_key(&key);
    if let Some(entry) = d.ledger.get(&key) {
        if entry.digest != digest {
            return reply(&mut stream, 409, &envelope("key_conflict"));
        }
    }
    let cleared_now = match &d.policy {
        Policy::All => true,
        Policy::Nobody => false,
        Policy::Only(set) => to.iter().all(|t| set.contains(t)),
    };
    let entry = d.ledger.entry(key.clone()).or_insert(Entry {
        digest,
        cleared: false,
        provider_id: None,
    });
    entry.cleared |= cleared_now;
    if !entry.cleared {
        let data = delivery(&key, "pending", "pending", None, None, repeated);
        let envelope = json!({"ok": false, "data": data, "error": {"code": "email_pending", "message": "pending"}});
        return reply(&mut stream, 202, &envelope);
    }
    if let Some(id) = entry.provider_id.clone() {
        let data = delivery(&key, "approved", "sent", Some(&id), None, true);
        return reply(&mut stream, 200, &json!({"ok": true, "data": data}));
    }
    match fault {
        Fault::ProviderFailed => {
            let data = delivery(
                &key,
                "approved",
                "failed",
                None,
                Some("email_provider_failed"),
                repeated,
            );
            reply(
                &mut stream,
                502,
                &json!({"ok": false, "data": data, "error": {"code": "email_provider_failed", "message": "failed"}}),
            )
        }
        Fault::Uncertain => {
            let data = delivery(
                &key,
                "approved",
                "uncertain",
                None,
                Some("email_provider_transport_error"),
                repeated,
            );
            reply(
                &mut stream,
                409,
                &json!({"ok": false, "data": data, "error": {"code": "email_provider_transport_error", "message": "uncertain"}}),
            )
        }
        Fault::NotExecuted => {
            let data = delivery(
                &key,
                "approved",
                "not_executed",
                None,
                Some("staged_body_missing"),
                repeated,
            );
            reply(
                &mut stream,
                409,
                &json!({"ok": false, "data": data, "error": {"code": "staged_body_missing", "message": "not executed"}}),
            )
        }
        _ => {
            d.provider_sends += 1;
            let id = format!("provider-{}", d.provider_sends);
            d.ledger.get_mut(&key).unwrap().provider_id = Some(id.clone());
            let data = delivery(&key, "approved", "sent", Some(&id), None, repeated);
            reply(&mut stream, 200, &json!({"ok": true, "data": data}))
        }
    }
}

// ---------- the CRM fixture ----------

fn blocks() -> Value {
    json!([
        {"type": "heading", "text": "Hello {{first_name|friend}}"},
        {"type": "paragraph", "text": "A calm first line."},
        {"type": "button", "label": "Read more", "url": "https://example.com/posts/welcome"},
    ])
}

struct Crm {
    root: tempfile::TempDir,
    _pm: Pm,
    daemon: TestDaemon,
}

impl Crm {
    /// A hosted daemon (door set) or a self-hosted one (`None`).
    fn start(door: Option<&FakeDoor>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        Self::copy_source(&root.path().join("source"));
        let mut opts: ServeOptions = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        cadence_agent::platform::smtp::attach(&mut opts);
        opts.crm_send_interval_ms = 10;
        opts.crm_send_pending_poll_ms = 150;
        opts.unsubscribe_origin = Some("https://board.example".into());
        if let Some(door) = door {
            cadence_agent::platform::agenticos::register(&mut opts, &door.url).unwrap();
            opts.hosted_email = Some(door.hosted());
        }
        let daemon = TestDaemon::start_opts(opts);
        Self {
            root,
            _pm: pm,
            daemon,
        }
    }

    fn copy_source(into: &Path) {
        for name in [
            "app.md",
            "workflows/blog-post.md",
            "rubrics/blog.md",
            "templates/brief.md",
            "templates/post.md",
        ] {
            let destination = into.join(name);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("apps/blog-post")
                    .join(name),
                &destination,
            )
            .unwrap();
        }
    }

    fn rpc(&self, method: &str, params: Value) -> Value {
        self.daemon.operator_rpc(method, params).unwrap()
    }

    /// install + context + customers + approved content + freeze.
    fn campaign(&self, profiles: &[(&str, &str)]) -> (String, String) {
        let install = self.rpc(
            "app_workspace_install",
            json!({"source": self.root.path().join("source")}),
        )["install_id"]
            .as_str()
            .unwrap()
            .to_string();
        let context = self.rpc(
            "app_context_create",
            json!({"install_id": install, "label": "brand", "input_defaults": {}, "request_id": "ctx-1"}),
        )["context"]["id"].as_str().unwrap().to_string();
        for (id, profile) in profiles {
            self.rpc(
                "app_record_create",
                json!({"install_id": install, "context_id": context, "record_id": id,
                    "profile": serde_json::from_str::<Value>(profile).unwrap()}),
            );
        }
        self.rpc(
            "app_content_save",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "subject": "Spring launch", "preheader": "News", "blocks": blocks()}),
        );
        let revision = self.rpc(
            "app_content_show",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1"}),
        )["content"]["revision"]
            .as_u64()
            .unwrap();
        self.rpc(
            "app_content_approve",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1", "expected_revision": revision}),
        );
        self.rpc(
            "app_audience_prepare",
            json!({"install_id": install, "context_id": context, "freeze_id": "freeze-1",
                "base": {"mode": "all"}, "max_recipients": 50}),
        );
        (install, context)
    }

    /// The hosted built-in sender row the board lists.
    fn hosted_row(&self) -> Option<Value> {
        self.rpc("connection_list", json!({}))["connections"]
            .as_array()?
            .iter()
            .find(|r| r["provider"] == "agenticos" && r["account"] == "hosted")
            .cloned()
    }

    fn bind_hosted(&self, install: &str, context: &str) -> Value {
        let id = self.hosted_row().expect("hosted row")["id"]
            .as_str()
            .unwrap()
            .to_string();
        self.rpc(
            "crm_smtp_bind",
            json!({"install_id": install, "context_id": context, "connection_id": id, "request_id": "bind-1"}),
        )
    }

    fn test_send(&self, install: &str, context: &str) -> Result<Value, String> {
        self.daemon
            .operator_rpc(
                "crm_smtp_test_send",
                json!({"install_id": install, "context_id": context, "campaign_id": "launch-1", "to_email": RECIPIENT}),
            )
            .map_err(|e| e.to_string())
    }

    fn prepare(&self, install: &str, context: &str) -> Result<Value, String> {
        self.daemon
            .operator_rpc(
                "crm_send_prepare",
                json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                    "audience_freeze_id": "freeze-1", "request_id": "send-1"}),
            )
            .map_err(|e| e.to_string())
    }

    /// prepare + approve; returns the send id.
    fn start_send(&self, install: &str, context: &str) -> String {
        let prepared = self.prepare(install, context).unwrap();
        let digest = prepared["send_digest"].as_str().unwrap().to_string();
        let send_id = prepared["send"]["send_id"].as_str().unwrap().to_string();
        self.rpc(
            "crm_send_approve",
            json!({"install_id": install, "context_id": context, "send_id": send_id, "send_digest": digest}),
        );
        send_id
    }

    fn show(&self, install: &str, context: &str, send_id: &str) -> Value {
        self.rpc(
            "crm_send_show",
            json!({"install_id": install, "context_id": context, "send_id": send_id}),
        )
    }
}

fn wait_until(secs: u64, probe: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if probe() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

fn campaign_seen(door: &FakeDoor) -> Vec<Seen> {
    door.seen()
        .into_iter()
        .filter(|s| s.body["to"][0] != RECIPIENT)
        .collect()
}

fn deliveries(shown: &Value) -> HashMap<String, Value> {
    shown["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| (d["customer_id"].as_str().unwrap().to_string(), d.clone()))
        .collect()
}

// ---------- the sender row ----------

#[test]
fn cad1063_hosted_sender_row_shows_the_platform_address_and_no_password() {
    let door = FakeDoor::start(Policy::All);
    let crm = Crm::start(Some(&door));
    let row = crm.hosted_row().expect("hosted row is listed");
    assert_eq!(row["kind"], "builtin");
    // A sender, but never a faulty or rotatable SMTP enrolment.
    assert_eq!(row["smtp_sender"], true, "{row}");
    assert!(row["smtp_error"].is_null(), "{row}");
    // The board's sender list keeps rows with an `smtp` projection: the
    // hosted row carries the from-address and the platform marker, and
    // no credential field of any kind.
    assert_eq!(row["smtp"]["sender"], FROM);
    assert_eq!(row["smtp"]["tls_mode"], "platform");
    assert!(row["smtp"].get("secret").is_none(), "{row}");
    let (install, context) = crm.campaign(&[("customer-a", PROFILE_A)]);
    let bound = crm.bind_hosted(&install, &context);
    assert_eq!(bound["binding"]["sender"]["address"], FROM);
    let shown = crm.rpc(
        "crm_smtp_show",
        json!({"install_id": install, "context_id": context}),
    );
    assert_eq!(shown["binding"]["transport_kind"], "agenticos", "{shown}");
}

#[test]
fn cad1063_self_hosted_keeps_smtp_and_never_contacts_the_door() {
    let door = FakeDoor::start(Policy::All);
    // The daemon has no hosted door: the hosted row is not a sender.
    let crm = Crm::start(None);
    assert!(
        crm.hosted_row().is_none_or(|r| r["smtp"].is_null()),
        "a self-hosted daemon must not offer the hosted sender"
    );
    let (install, context) = crm.campaign(&[("customer-a", PROFILE_A)]);
    // Even naming the hosted row's would-be id refuses: it is not a
    // credential-backed SMTP connection here.
    let forged = "builtin-00000000000000000000000000000000";
    let err = crm
        .daemon
        .operator_rpc(
            "crm_smtp_bind",
            json!({"install_id": install, "context_id": context, "connection_id": forged, "request_id": "bind-1"}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("unavailable or stale"), "{err}");
    // An SMTP test send with nothing bound refuses before any socket.
    assert!(crm.test_send(&install, &context).is_err());
    assert!(
        door.seen().is_empty(),
        "self-hosted touched the door: {:?}",
        door.seen()
    );
}

// ---------- test send ----------

#[test]
fn cad1063_test_send_accepted_uses_the_exact_contract_and_records_evidence() {
    let door = FakeDoor::start(Policy::All);
    let crm = Crm::start(Some(&door));
    let (install, context) = crm.campaign(&[("customer-a", PROFILE_A)]);
    crm.bind_hosted(&install, &context);
    // Evidence first: no test send, no campaign send.
    assert!(crm
        .prepare(&install, &context)
        .unwrap_err()
        .contains("test send"));
    let sent = crm.test_send(&install, &context).unwrap();
    let receipt = &sent["receipt"];
    assert_eq!(receipt["accepted"], true, "{sent}");
    assert_eq!(receipt["pending_approval"], false);
    assert_eq!(receipt["transport_kind"], "agenticos");
    assert_eq!(receipt["delivery_claim"], "smtp-acceptance-only");
    let seen = door.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    let request = &seen[0];
    assert_eq!(request.path, "/v1/runtime/email/send");
    assert!(request.key.starts_with("crm-"), "{}", request.key);
    assert!(
        request.authorization.is_none(),
        "hosted sends carry no credential"
    );
    let fields: HashSet<_> = request.body.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        fields,
        ["to", "subject", "text", "html"].map(String::from).into()
    );
    assert_eq!(request.body["to"], json!([RECIPIENT]));
    assert_eq!(request.body["subject"], "Spring launch");
    assert!(request.body["html"]
        .as_str()
        .unwrap()
        .contains("A calm first line."));
    // The accepted receipt is the evidence a prepare accepts.
    assert!(crm.prepare(&install, &context).is_ok());
}

#[test]
fn cad1063_test_send_pending_is_typed_not_refused_and_replays_the_same_key() {
    let door = FakeDoor::start(Policy::Nobody);
    let crm = Crm::start(Some(&door));
    let (install, context) = crm.campaign(&[("customer-a", PROFILE_A)]);
    crm.bind_hosted(&install, &context);
    let first = crm.test_send(&install, &context).unwrap();
    assert_eq!(first["receipt"]["pending_approval"], true, "{first}");
    assert_eq!(first["receipt"]["accepted"], false);
    assert_eq!(
        first["receipt"]["smtp_message"],
        "waiting for owner approval in AgenticOS"
    );
    // Pending is not evidence.
    assert!(crm
        .prepare(&install, &context)
        .unwrap_err()
        .contains("test send"));
    assert_eq!(door.provider_sends(), 0);
    // The owner decides; the same test, presented again, is the same
    // key and executes exactly once.
    door.set_policy(Policy::All);
    let second = crm.test_send(&install, &context).unwrap();
    assert_eq!(second["receipt"]["accepted"], true, "{second}");
    let again = crm.test_send(&install, &context).unwrap();
    assert_eq!(again["receipt"]["accepted"], true);
    let seen = door.seen();
    assert_eq!(seen.len(), 3);
    assert!(seen.iter().all(|s| s.key == seen[0].key), "{seen:?}");
    assert_eq!(
        door.provider_sends(),
        1,
        "a replayed key must not send twice"
    );
    assert!(crm.prepare(&install, &context).is_ok());
}

#[test]
fn cad1063_test_send_failures_refuse_and_record_no_evidence() {
    for fault in [
        Fault::ProviderFailed,
        Fault::Uncertain,
        Fault::NotExecuted,
        Fault::Reset,
    ] {
        let door = FakeDoor::start(Policy::All);
        door.set_fault(fault, 1);
        let crm = Crm::start(Some(&door));
        let (install, context) = crm.campaign(&[("customer-a", PROFILE_A)]);
        crm.bind_hosted(&install, &context);
        assert!(crm.test_send(&install, &context).is_err());
        assert!(crm
            .prepare(&install, &context)
            .unwrap_err()
            .contains("test send"));
        assert_eq!(door.provider_sends(), 0);
    }
}

#[test]
fn cad1063_a_hanging_door_is_uncertain_and_never_retried() {
    let door = FakeDoor::start(Policy::All);
    door.set_fault(Fault::Hang, 5);
    let hosted = door.hosted().with_timeout(Duration::from_millis(300));
    let message = HostedMessage {
        to: RECIPIENT.into(),
        subject: "s".into(),
        text: "t".into(),
        html: "<p>h</p>".into(),
    };
    let outcome = hosted
        .send_outcome(&message, "crm-aaaaaaaaaaaaaaaa")
        .unwrap();
    assert!(matches!(
        outcome,
        cadence_agent::platform::smtp::SmtpOutcome::Uncertain { .. }
    ));
    assert_eq!(
        door.seen().len(),
        1,
        "one request, no retry inside the client"
    );
}

// ---------- campaign send ----------

#[test]
fn cad1063_campaign_send_per_recipient_keys_unsubscribe_and_acceptance_only() {
    let door = FakeDoor::start(Policy::All);
    let crm = Crm::start(Some(&door));
    let (install, context) = crm.campaign(&[
        ("customer-a", PROFILE_A),
        ("customer-b", PROFILE_B),
        ("customer-c", PROFILE_C),
    ]);
    crm.bind_hosted(&install, &context);
    crm.test_send(&install, &context).unwrap();
    let send_id = crm.start_send(&install, &context);
    assert!(wait_until(30, || crm.show(&install, &context, &send_id)
        ["send"]["state"]
        == "completed"));
    let shown = crm.show(&install, &context, &send_id);
    assert_eq!(shown["counts"]["accepted"], 2, "{shown}");
    assert_eq!(shown["delivery_claim"], "smtp-acceptance-only");
    let seen = campaign_seen(&door);
    assert_eq!(
        seen.len(),
        2,
        "denied consent never reaches the door: {seen:?}"
    );
    let recipients: HashSet<_> = seen
        .iter()
        .map(|s| s.body["to"][0].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        recipients,
        ["amina@example.com", "cleo@example.com"]
            .map(String::from)
            .into()
    );
    assert!(seen
        .iter()
        .all(|s| s.body["to"].as_array().unwrap().len() == 1));
    assert_ne!(seen[0].key, seen[1].key);
    assert!(seen.iter().all(|s| s.authorization.is_none()));
    // Personalised, and each message carries its own redeemable token.
    let amina = seen
        .iter()
        .find(|s| s.body["to"][0] == "amina@example.com")
        .unwrap();
    assert!(amina.body["html"].as_str().unwrap().contains("Hello Amina"));
    let html = amina.body["html"].as_str().unwrap();
    let at = html
        .find("https://board.example/unsubscribe/")
        .expect("unsubscribe link in body");
    let token: String = html[at + "https://board.example/unsubscribe/".len()..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    assert_eq!(token.len(), 43, "{token}");
    crm.rpc("crm_unsubscribe_redeem", json!({"token": token}));
    let suppressions = crm.rpc(
        "app_suppression_list",
        json!({"install_id": install, "context_id": context}),
    );
    assert!(
        suppressions["suppressions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["key"] == "amina@example.com"),
        "{suppressions}"
    );
    // The masked read never leaks an address.
    for d in shown["deliveries"].as_array().unwrap() {
        assert!(d["email"].as_str().unwrap().contains('*'));
    }
}

#[test]
fn cad1063_pending_deliveries_wait_without_blocking_others_and_replay_identical_bytes() {
    // Only cleo is cleared at first; amina waits for the owner.
    let door = FakeDoor::start(Policy::Only(["cleo@example.com".to_string()].into()));
    let crm = Crm::start(Some(&door));
    let (install, context) = crm.campaign(&[("customer-a", PROFILE_A), ("customer-c", PROFILE_C)]);
    crm.bind_hosted(&install, &context);
    door.set_policy(Policy::All);
    crm.test_send(&install, &context).unwrap();
    door.set_policy(Policy::Only(["cleo@example.com".to_string()].into()));
    let send_id = crm.start_send(&install, &context);
    // Cleo goes through while amina's delivery is parked, queued, with
    // the typed reason the board shows.
    assert!(wait_until(30, || {
        let shown = crm.show(&install, &context, &send_id);
        let rows = deliveries(&shown);
        rows["customer-c"]["state"] == "accepted"
            && rows["customer-a"]["state"] == "queued"
            && rows["customer-a"]["reason"] == "waiting for owner approval in AgenticOS"
    }));
    assert_eq!(
        crm.show(&install, &context, &send_id)["send"]["state"],
        "sending"
    );
    // Wait for at least one re-presentation of amina's delivery.
    let amina_requests = || {
        campaign_seen(&door)
            .into_iter()
            .filter(|s| s.body["to"][0] == "amina@example.com")
            .collect::<Vec<_>>()
    };
    assert!(wait_until(30, || amina_requests().len() >= 2));
    let before = amina_requests();
    // Same key and byte-identical content (the unsubscribe token is
    // kept in memory), so the owner's one approval covers every replay.
    assert!(
        before
            .iter()
            .all(|s| s.key == before[0].key && s.body == before[0].body),
        "{before:?}"
    );
    // The owner approves: the next replay executes, once.
    door.set_policy(Policy::All);
    assert!(wait_until(30, || crm.show(&install, &context, &send_id)
        ["send"]["state"]
        == "completed"));
    let rows = deliveries(&crm.show(&install, &context, &send_id));
    assert_eq!(rows["customer-a"]["state"], "accepted");
    assert_eq!(
        door.provider_sends(),
        3,
        "1 test + 2 campaign deliveries, no double send"
    );
}

#[test]
fn cad1063_outcomes_map_to_delivery_states_and_uncertain_is_never_retried() {
    // (fault, expected final state, expected door requests for the campaign row)
    let cases: [(Fault, &str, usize); 4] = [
        (Fault::ProviderFailed, "failed", 1),
        (Fault::Uncertain, "uncertain", 1),
        (Fault::Reset, "uncertain", 1),
        // Never sent: retried at most the bounded 3 attempts, then failed.
        (Fault::NotExecuted, "failed", 3),
    ];
    for (fault, state, requests) in cases {
        let door = FakeDoor::start(Policy::All);
        let crm = Crm::start(Some(&door));
        let (install, context) = crm.campaign(&[("customer-a", PROFILE_A)]);
        crm.bind_hosted(&install, &context);
        crm.test_send(&install, &context).unwrap();
        door.set_fault(fault, 100);
        let send_id = crm.start_send(&install, &context);
        assert!(
            wait_until(30, || crm.show(&install, &context, &send_id)["send"]
                ["state"]
                == "completed"),
            "{state}: send did not finish"
        );
        let rows = deliveries(&crm.show(&install, &context, &send_id));
        assert_eq!(rows["customer-a"]["state"], state);
        assert_eq!(
            campaign_seen(&door).len(),
            requests,
            "{state}: {:?}",
            campaign_seen(&door)
        );
    }
}

#[test]
fn cad1063_deferred_retries_use_a_fresh_key_because_the_bytes_changed() {
    let door = FakeDoor::start(Policy::All);
    let crm = Crm::start(Some(&door));
    let (install, context) = crm.campaign(&[("customer-a", PROFILE_A)]);
    crm.bind_hosted(&install, &context);
    crm.test_send(&install, &context).unwrap();
    // Two 503s then service: the worker retries and succeeds. Each
    // retry mints a new unsubscribe token (new bytes); the key must
    // follow the bytes or a bound ledger answers key_conflict.
    door.set_fault(Fault::Unavailable503, 2);
    let send_id = crm.start_send(&install, &context);
    assert!(wait_until(30, || crm.show(&install, &context, &send_id)
        ["send"]["state"]
        == "completed"));
    let rows = deliveries(&crm.show(&install, &context, &send_id));
    assert_eq!(rows["customer-a"]["state"], "accepted", "{rows:?}");
    let seen = campaign_seen(&door);
    assert_eq!(seen.len(), 3, "{seen:?}");
    let keys: HashSet<_> = seen.iter().map(|s| s.key.clone()).collect();
    assert_eq!(keys.len(), 3, "a retry with new bytes reused a key");
}

// ---------- adversarial callers ----------

fn native(
    lane: &mut LaneShell,
    state: &Path,
    detached: bool,
    method: &str,
    params: Value,
) -> Value {
    let frame = json!({"method": method, "params": params});
    let request = lane.dir.path().join(format!("hosted-{}.json", lane.seq));
    std::fs::write(&request, frame.to_string()).unwrap();
    let prefix = if detached { "setsid " } else { "" };
    let (rc, text) = lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.socket(socket.AF_UNIX);s.settimeout(10);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}", state.join("cadence.sock").display(), request.display()));
    assert_eq!(rc, 0, "native socket process failed: {text}");
    serde_json::from_str(text.trim()).unwrap()
}

#[test]
fn cad1063_agent_and_detached_callers_cannot_send_through_the_hosted_door() {
    let door = FakeDoor::start(Policy::All);
    let crm = Crm::start(Some(&door));
    let (install, context) = crm.campaign(&[("customer-a", PROFILE_A)]);
    let hosted_id = crm.hosted_row().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    crm.bind_hosted(&install, &context);
    crm.test_send(&install, &context).unwrap();
    let baseline = door.seen().len();
    let mut lane = LaneShell::spawn(crm.daemon.dir.path());
    plant_member_pane(&crm.daemon, "hosted-peer", "claude", None, lane.pid());
    let calls = [
        (
            "crm_smtp_bind",
            json!({"install_id": install, "context_id": context, "connection_id": hosted_id, "request_id": "forged"}),
        ),
        (
            "crm_smtp_test_send",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1", "to_email": RECIPIENT}),
        ),
        (
            "crm_send_prepare",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1", "audience_freeze_id": "freeze-1", "request_id": "p-1"}),
        ),
        (
            "crm_send_approve",
            json!({"install_id": install, "context_id": context, "send_id": "send-p-1", "send_digest": "sha256:x"}),
        ),
    ];
    for detached in [false, true] {
        for (method, params) in &calls {
            let frame = native(
                &mut lane,
                &crm.daemon.state,
                detached,
                method,
                params.clone(),
            );
            assert_eq!(frame["ok"], false, "{method} detached={detached}: {frame}");
            assert!(
                frame["error"]["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("operator action"),
                "{method}: {frame}"
            );
        }
    }
    // A forged field over the operator's own connection refuses too.
    assert!(crm
        .daemon
        .operator_rpc(
            "crm_smtp_test_send",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
                "to_email": RECIPIENT, "by": "operator"}),
        )
        .is_err());
    assert_eq!(
        door.seen().len(),
        baseline,
        "a refused caller reached the door"
    );
}
