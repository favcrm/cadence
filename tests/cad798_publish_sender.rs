//! CAD-798 fake-door end-to-end proofs for the production publish transport.
//!
//! The loopback door below speaks the LANDED device contract shapes
//! (`POST /v1/runtime/connectors/publish/preflight`,
//! `POST /v1/runtime/connectors/publish`,
//! `GET /v1/runtime/connectors/publish/{key}/status` with the
//! `{ok:true,data}` / `{ok:false,error:{code,message}}` envelope and
//! `Authorization: Bearer` credential binding) — never the CAD-771
//! test-fake paths, never a live post, never a real credential. Every
//! provider byte the production [`HttpPublishSender`] sees comes from
//! this door, so the byte-exact evidence, no-second-call recovery and
//! adversarial fail-closed proofs here bind the real wire behavior.
//!
//! Companion policy: no enablement, no live post, no credential in
//! tests — the bearer below is synthetic and the dispatch gate starts
//! closed, mirroring the door's default-off.
#![allow(clippy::disallowed_methods)]
mod common;

use cadence_agent::platform::agenticos_external::publish::{
    caption_digest_of, LedgerOutcome, PublishSender, PublishState, Refusal, SendBinding, Toolkit,
};
use cadence_agent::platform::agenticos_external::publish_sender::{
    DeviceCredential, HttpPublishSender, MaterialResolver, SendMaterial,
    PUBLISH_SEND_CREDENTIAL_FILE_ENV, PUBLISH_SEND_URL_ENV,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const CONN: &str = "con_harbour_fb";
const DEST: &str = "275491372109884";
const GRANT: &str = "dpq_cad798_grant_01";
const CAPTION: &str = "Harbour at dusk. Synthetic CAD-798 caption.";
const BEARER: &str = "cad798-synthetic-credential";
/// CAD-979 v9: the read (`provider.read`) credential for the destinations
/// GET — a separate bearer, never the send one.
const READ_BEARER: &str = "cad798-read-credential";
/// The remote AOS `connectionId` the resolver maps to (wire identity);
/// `CONN` stays the local custody/install id.
const AOS_CONN: &str = "connA_harbour_fb";

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn binding(key: &str, grant: &str) -> SendBinding {
    SendBinding {
        key: key.into(),
        connection_id: CONN.into(),
        destination_id: DEST.into(),
        toolkit: Toolkit::Facebook,
        caption_digest: caption_digest_of(CAPTION),
        image_digest: None,
        cadence_run_id: "cad_run_798_01".into(),
        cadence_effect_id: "cad_fx_798_01".into(),
        grant_id: grant.into(),
    }
}

fn resolver() -> MaterialResolver {
    Arc::new(|_| {
        Ok(SendMaterial {
            caption: CAPTION.into(),
            media_key: None,
        })
    })
}

fn sender(door: &FakeDoor) -> HttpPublishSender {
    HttpPublishSender::new(
        &format!("http://{}", door.addr),
        DeviceCredential::new(BEARER.into()),
        resolver(),
    )
    .expect("loopback sender")
}

// ---------- loopback door speaking the landed contract ----------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Behavior {
    Post,
    Refuse,
    Lose,
    Sleep,
    /// Record the post and consume the grant use, then answer 503 with
    /// a JSON error document: models accept-then-failover at the door.
    AcceptThen503,
}

struct GrantRow {
    connection: String,
    destination: String,
    caption_digest: String,
    image_digest: Option<String>,
    uses: u32,
    revoked: bool,
}

#[derive(Clone)]
struct LedgerRow {
    connection: String,
    destination: String,
    caption_digest: String,
    image_digest: Option<String>,
    state: &'static str,
    permalink: Option<String>,
}

#[derive(Default)]
struct DoorState {
    send_enabled: bool,
    grants: HashMap<String, GrantRow>,
    connections: Vec<String>,
    ledger: HashMap<String, LedgerRow>,
    behaviors: HashMap<String, Behavior>,
    version_override: Option<String>,
    drift_next_exec: HashMap<String, bool>,
    fail_next_preflight: HashMap<String, bool>,
    preflight_down: bool,
    preflight_calls: u64,
    provider_calls: u64,
    http_calls: u64,
    exec_calls: u64,
    bodies: Vec<Value>,
}

struct FakeDoor {
    addr: String,
    state: Arc<Mutex<DoorState>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

type DoorReply = tiny_http::Response<std::io::Cursor<Vec<u8>>>;

fn fail(status: u16, code: &str, message: &str) -> DoorReply {
    let body = json!({"ok": false, "error": {"code": code, "message": message}}).to_string();
    tiny_http::Response::from_string(body).with_status_code(status)
}

fn ok(data: &Value) -> DoorReply {
    tiny_http::Response::from_string(json!({"ok": true, "data": data}).to_string())
}

impl FakeDoor {
    fn start() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap().to_string();
        let state = Arc::new(Mutex::new(DoorState::default()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_state = Arc::clone(&state);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(std::sync::atomic::Ordering::SeqCst) {
                let Ok(Some(mut request)) = server.recv_timeout(Duration::from_millis(50)) else {
                    continue;
                };
                let url = request.url().to_owned();
                let auth = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Authorization"))
                    .map(|h| h.value.to_string())
                    .unwrap_or_default();
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap_or(0);
                let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let (reply, sleep) = Self::route(
                    &worker_state,
                    request.method().as_str(),
                    &url,
                    &auth,
                    &value,
                );
                if let Some(delay) = sleep {
                    thread::sleep(delay);
                }
                let _ = request.respond(reply);
            }
        });
        Self {
            addr,
            state,
            stop,
            worker: Some(worker),
        }
    }

    fn stop(self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        // `worker` joins on drop below; explicit stop is immediate.
        drop(self);
    }
}

impl Drop for FakeDoor {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl FakeDoor {
    fn set_enabled(&self, enabled: bool) {
        self.state.lock().unwrap().send_enabled = enabled;
    }

    fn enroll_connection(&self, connection: &str) {
        self.state
            .lock()
            .unwrap()
            .connections
            .push(connection.into());
    }

    fn issue(&self, id: &str, uses: u32) {
        self.enroll_grant(id, CONN, DEST, &caption_digest_of(CAPTION), None, uses);
    }

    /// Enroll one owner-minted grant row the door authorizes against:
    /// exact connection, destination and content digests plus bounded uses.
    fn enroll_grant(
        &self,
        id: &str,
        connection: &str,
        destination: &str,
        caption_digest: &str,
        image_digest: Option<&str>,
        uses: u32,
    ) {
        self.state.lock().unwrap().grants.insert(
            id.into(),
            GrantRow {
                connection: connection.into(),
                destination: destination.into(),
                caption_digest: caption_digest.into(),
                image_digest: image_digest.map(str::to_owned),
                uses,
                revoked: false,
            },
        );
    }

    fn revoke(&self, id: &str) {
        if let Some(grant) = self.state.lock().unwrap().grants.get_mut(id) {
            grant.revoked = true;
        }
    }

    fn behave(&self, key: &str, behavior: Behavior) {
        self.state
            .lock()
            .unwrap()
            .behaviors
            .insert(key.into(), behavior);
    }

    /// One-shot staging failover: the next preflight for `key` answers
    /// 503, then staging recovers. Proves the retryable pre-send path.
    fn fail_next_preflight(&self, key: &str) {
        self.state
            .lock()
            .unwrap()
            .fail_next_preflight
            .insert(key.into(), true);
    }

    /// Persistent staging outage: every preflight answers 503. Proves
    /// persistent ambiguity fails loud with nothing sent.
    fn set_preflight_down(&self, down: bool) {
        self.state.lock().unwrap().preflight_down = down;
    }

    fn preflight_calls(&self) -> u64 {
        self.state.lock().unwrap().preflight_calls
    }

    /// One-shot contract drift on the next execution answer for `key`:
    /// the row is recorded and the provider is called, but the verdict
    /// document carries a drifted version.
    fn drift_next_exec(&self, key: &str) {
        self.state
            .lock()
            .unwrap()
            .drift_next_exec
            .insert(key.into(), true);
    }

    fn set_version(&self, version: &str) {
        self.state.lock().unwrap().version_override = Some(version.into());
    }

    fn provider_calls(&self) -> u64 {
        self.state.lock().unwrap().provider_calls
    }

    fn http_calls(&self) -> u64 {
        self.state.lock().unwrap().http_calls
    }

    /// Execution POSTs received on the wire (preflight excluded): proves
    /// staging refusals emit zero sends.
    fn exec_calls(&self) -> u64 {
        self.state.lock().unwrap().exec_calls
    }

    fn permalink_for(key: &str, digest: &str) -> String {
        format!("https://www.facebook.com/{key}/{}/", &digest[..11])
    }

    fn payload_for(permalink: &str, digest: &str) -> Value {
        json!({"id": "door-post-1", "permalink": permalink, "caption_digest": digest})
    }

    /// Grant-checked staging shared by preflight and execution: pure
    /// validation, no ledger mutation, no provider call. Returns the
    /// staged digest triple on success.
    fn stage(
        state: &mut DoorState,
        value: &Value,
    ) -> std::result::Result<(String, String, String, Option<String>), DoorReply> {
        let key = value["key"].as_str().unwrap_or("").to_owned();
        let connection = value["connectionId"].as_str().unwrap_or("").to_owned();
        let caption = value["caption"].as_str().unwrap_or("").to_owned();
        let grant = &value["grant"];
        let grant_id = grant["id"].as_str().unwrap_or("");
        if key.len() < 8 || key.len() > 128 {
            return Err(fail(
                400,
                "validation_failed",
                "The idempotency key is invalid.",
            ));
        }
        if connection.is_empty() || connection.len() > 80 {
            return Err(fail(400, "validation_failed", "The connection is invalid."));
        }
        if caption.is_empty() || caption.chars().count() > 8000 {
            return Err(fail(400, "validation_failed", "The caption is invalid."));
        }
        if !state.connections.iter().any(|id| id == &connection) {
            return Err(fail(404, "not_found", "No such connection."));
        }
        let Some(row) = state.grants.get(grant_id) else {
            return Err(fail(403, "grant_required", "No usable send grant."));
        };
        if row.revoked {
            return Err(fail(409, "grant_revoked", "The send grant was revoked."));
        }
        if row.uses == 0 {
            return Err(fail(
                409,
                "grant_exhausted",
                "The send grant has no uses left.",
            ));
        }
        if grant["connectionId"].as_str().unwrap_or("") != row.connection
            || grant["destinationId"].as_str().unwrap_or("") != row.destination
        {
            return Err(fail(
                409,
                "destination_mismatch",
                "The destination differs from the approval.",
            ));
        }
        // The presented caption must hash to the presented digest (what
        // the real door recomputes), and the presented digest must equal
        // the grant row (server-side source of truth).
        if sha_hex(caption.as_bytes()) != grant["captionDigest"].as_str().unwrap_or("") {
            return Err(fail(
                409,
                "content_mismatch",
                "The caption differs from the approval.",
            ));
        }
        if grant["captionDigest"].as_str().unwrap_or("") != row.caption_digest
            || grant["imageDigest"] != json!(row.image_digest)
        {
            return Err(fail(
                409,
                "content_mismatch",
                "The content differs from the approval.",
            ));
        }
        let destination = row.destination.clone();
        let caption_digest = row.caption_digest.clone();
        let image_digest = row.image_digest.clone();
        if let Some(recorded) = state.ledger.get(&key) {
            if recorded.connection != connection
                || recorded.destination != destination
                || recorded.caption_digest != caption_digest
                || recorded.image_digest != image_digest
            {
                return Err(fail(
                    409,
                    "digest_mismatch",
                    "The request key is recorded against different content.",
                ));
            }
        }
        Ok((key, destination, caption_digest, image_digest))
    }

    fn route(
        state: &Mutex<DoorState>,
        method: &str,
        url: &str,
        auth: &str,
        value: &Value,
    ) -> (DoorReply, Option<Duration>) {
        let mut guard = state.lock().unwrap();
        // CAD-979 v9: the destinations read runs under the separate
        // `provider.read` credential — admit that bearer on this GET path
        // only and map `(toolkit, destination_id)` → the remote AOS
        // `connectionId`. Everything else requires the send credential.
        if method == "GET" && url == "/v1/runtime/connectors/destinations" {
            if auth != format!("Bearer {READ_BEARER}") {
                return (
                    fail(
                        401,
                        "unauthorized",
                        "A valid device credential is required.",
                    ),
                    None,
                );
            }
            return (
                ok(&json!([
                    {"connectionId": AOS_CONN, "toolkit": "facebook",
                     "displayName": "fb", "destinationId": DEST,
                     "status": "active", "available": true, "publishable": true},
                    {"connectionId": AOS_CONN, "toolkit": "instagram",
                     "displayName": "ig", "destinationId": DEST,
                     "status": "active", "available": true, "publishable": true},
                ])),
                None,
            );
        }
        guard.http_calls += 1;
        if !value.is_null() {
            guard.bodies.push(value.clone());
        }
        if auth != format!("Bearer {BEARER}") {
            return (
                fail(
                    401,
                    "unauthorized",
                    "A valid device credential is required.",
                ),
                None,
            );
        }
        let version = guard.version_override.clone().unwrap_or_else(|| "1".into());
        if method == "POST" && url == "/v1/runtime/connectors/publish/preflight" {
            guard.preflight_calls += 1;
            if guard.preflight_down
                || guard
                    .fail_next_preflight
                    .remove(value["key"].as_str().unwrap_or(""))
                    .unwrap_or(false)
            {
                return (
                    fail(503, "capability_unavailable", "Staging is unavailable."),
                    None,
                );
            }
            let staged = Self::stage(&mut guard, value);
            match staged {
                Err(reply) => (reply, None),
                Ok((key, destination, caption_digest, image_digest)) => {
                    let repeated = guard.ledger.contains_key(&key);
                    (
                        ok(&json!({
                            "key": key, "decision": "approved", "executable": true,
                            "repeated": repeated, "destinationId": destination,
                            "captionDigest": caption_digest, "imageDigest": image_digest,
                            "reason": Value::Null,
                        })),
                        None,
                    )
                }
            }
        } else if method == "POST" && url == "/v1/runtime/connectors/publish" {
            guard.exec_calls += 1;
            let staged = Self::stage(&mut guard, value);
            let (key, destination, caption_digest, image_digest) = match staged {
                Err(reply) => return (reply, None),
                Ok(staged) => staged,
            };
            // Recorded keys replay without dispatch, without consuming a
            // grant use and without another provider call — even while
            // the dispatch gate is closed.
            if let Some(recorded) = guard.ledger.get(&key) {
                let recorded = recorded.clone();
                return (
                    ok(&json!({
                        "version": version,
                        "result": {"key": key, "decision": "approved",
                            "executed": recorded.state != "failed",
                            "status": recorded.state,
                            "permalink": recorded.permalink, "repeated": true},
                        "destinationId": destination,
                        "captionDigest": caption_digest,
                        "imageDigest": image_digest,
                    })),
                    None,
                );
            }
            if !guard.send_enabled {
                return (
                    fail(409, "send_disabled", "Provider dispatch is not enabled."),
                    None,
                );
            }
            let behavior = guard.behaviors.get(&key).copied().unwrap_or(Behavior::Post);
            let drift = guard.drift_next_exec.remove(&key).unwrap_or(false);
            let version = if drift { "2".into() } else { version };
            let grant_id = value["grant"]["id"].as_str().unwrap_or("").to_owned();
            match behavior {
                Behavior::Refuse => {
                    // The provider attempt refuses: recorded as failed
                    // with no evidence, replayed without another call.
                    guard.provider_calls += 1;
                    if let Some(grant) = guard.grants.get_mut(&grant_id) {
                        grant.uses = grant.uses.saturating_sub(1);
                    }
                    guard.ledger.insert(
                        key.clone(),
                        LedgerRow {
                            connection: value["connectionId"].as_str().unwrap_or("").into(),
                            destination: destination.clone(),
                            caption_digest: caption_digest.clone(),
                            image_digest: image_digest.clone(),
                            state: "failed",
                            permalink: None,
                        },
                    );
                    (
                        ok(&json!({
                            "version": version,
                            "result": {"key": key, "decision": "approved",
                                "executed": false, "status": "failed",
                                "permalink": Value::Null, "repeated": false},
                            "destinationId": destination,
                            "captionDigest": caption_digest,
                            "imageDigest": image_digest,
                        })),
                        None,
                    )
                }
                Behavior::Lose => {
                    // Accepted and recorded, but the response is "lost":
                    // the client sees processing and must reconcile.
                    guard.provider_calls += 1;
                    if let Some(grant) = guard.grants.get_mut(&grant_id) {
                        grant.uses = grant.uses.saturating_sub(1);
                    }
                    let permalink = Self::permalink_for(&key, &caption_digest);
                    guard.ledger.insert(
                        key.clone(),
                        LedgerRow {
                            connection: value["connectionId"].as_str().unwrap_or("").into(),
                            destination: destination.clone(),
                            caption_digest: caption_digest.clone(),
                            image_digest: image_digest.clone(),
                            state: "posted",
                            permalink: Some(permalink),
                        },
                    );
                    (
                        ok(&json!({
                            "version": version,
                            "result": {"key": key, "decision": "approved",
                                "executed": true, "status": "processing",
                                "permalink": Value::Null, "repeated": false},
                            "destinationId": destination,
                            "captionDigest": caption_digest,
                            "imageDigest": image_digest,
                        })),
                        None,
                    )
                }
                Behavior::Post | Behavior::Sleep => {
                    guard.provider_calls += 1;
                    if let Some(grant) = guard.grants.get_mut(&grant_id) {
                        grant.uses = grant.uses.saturating_sub(1);
                    }
                    let permalink = Self::permalink_for(&key, &caption_digest);
                    guard.ledger.insert(
                        key.clone(),
                        LedgerRow {
                            connection: value["connectionId"].as_str().unwrap_or("").into(),
                            destination: destination.clone(),
                            caption_digest: caption_digest.clone(),
                            image_digest: image_digest.clone(),
                            state: "posted",
                            permalink: Some(permalink.clone()),
                        },
                    );
                    let sleep = if behavior == Behavior::Sleep {
                        Some(Duration::from_secs(2))
                    } else {
                        None
                    };
                    (
                        ok(&json!({
                            "version": version,
                            "result": {"key": key, "decision": "approved",
                                "executed": true, "status": "posted",
                                "permalink": permalink, "repeated": false},
                            "destinationId": destination,
                            "captionDigest": caption_digest,
                            "imageDigest": image_digest,
                        })),
                        sleep,
                    )
                }
                Behavior::AcceptThen503 => {
                    // Accepted, recorded and use-consumed — then the door
                    // answers 503 with a JSON error document. The client
                    // must read this as ambiguity (processing), never a
                    // definitive refusal, and recover through status.
                    guard.provider_calls += 1;
                    if let Some(grant) = guard.grants.get_mut(&grant_id) {
                        grant.uses = grant.uses.saturating_sub(1);
                    }
                    let permalink = Self::permalink_for(&key, &caption_digest);
                    guard.ledger.insert(
                        key.clone(),
                        LedgerRow {
                            connection: value["connectionId"].as_str().unwrap_or("").into(),
                            destination: destination.clone(),
                            caption_digest: caption_digest.clone(),
                            image_digest: image_digest.clone(),
                            state: "posted",
                            permalink: Some(permalink),
                        },
                    );
                    (
                        fail(
                            503,
                            "capability_unavailable",
                            "Media signing is not configured.",
                        ),
                        None,
                    )
                }
            }
        } else if method == "GET"
            && url.starts_with("/v1/runtime/connectors/publish/")
            && url.ends_with("/status")
        {
            let key = url
                .strip_prefix("/v1/runtime/connectors/publish/")
                .and_then(|rest| rest.strip_suffix("/status"))
                .unwrap_or("");
            if key.len() < 8 || key.len() > 128 {
                return (fail(404, "not_found", "No such publish."), None);
            }
            let Some(recorded) = guard.ledger.get(key) else {
                return (fail(404, "not_found", "No such publish."), None);
            };
            let recorded = recorded.clone();
            let permalink = recorded.permalink.clone().unwrap_or_default();
            let payload = Self::payload_for(&permalink, &recorded.caption_digest);
            (
                ok(&json!({
                    "version": version, "key": key,
                    "state": recorded.state,
                    "permalink": recorded.permalink,
                    "destinationId": recorded.destination,
                    "captionDigest": recorded.caption_digest,
                    "imageDigest": recorded.image_digest,
                    "providerIds": ["door-post-1"],
                    "providerPayload": payload,
                    "reason": Value::Null,
                })),
                None,
            )
        } else {
            (fail(404, "not_found", "No such route."), None)
        }
    }
}

// ---------- sender-level proofs ----------

#[test]
fn cad798_dispatch_gate_is_closed_by_default() {
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 3);
    let sender = sender(&door);
    let binding = binding("cad798-gate-id-01", GRANT);
    // Staging is unaffected by the closed gate, exactly like the door.
    assert_eq!(
        sender.preflight(&binding, &resolver()(&binding).unwrap()),
        Ok(false)
    );
    // Execution refuses with send_disabled and calls no provider.
    let refusal = sender
        .execute(&binding)
        .expect_err("closed gate must refuse");
    assert_eq!(refusal.code, "send_disabled");
    assert!(refusal.detail.contains("send_disabled"));
    assert_eq!(door.provider_calls(), 0);
    // Explicit enablement posts.
    door.set_enabled(true);
    let outcome = sender.execute(&binding).expect("enabled gate posts");
    assert_eq!(outcome.state, PublishState::Posted);
    assert_eq!(door.provider_calls(), 1);
    door.stop();
}

#[test]
fn cad798_execute_posts_once_with_byte_exact_evidence() {
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 3);
    door.set_enabled(true);
    let sender = sender(&door);
    let binding = binding("cad798-once-id-01", GRANT);
    let outcome = sender.execute(&binding).expect("post");
    assert_eq!(outcome.state, PublishState::Posted);
    assert!(!outcome.repeated);
    assert!(outcome
        .permalink
        .as_deref()
        .unwrap_or("")
        .contains("cad798-once-id-01"));
    assert_eq!(outcome.destination_id, DEST);
    assert_eq!(outcome.caption_digest, caption_digest_of(CAPTION));
    assert_eq!(outcome.provider_ids, vec!["door-post-1".to_owned()]);
    let payload = outcome.provider_payload.clone().expect("provider evidence");
    assert!(payload.contains("door-post-1"));
    // Replay: same key returns the recorded outcome without a second call.
    let replay = sender.execute(&binding).expect("replay");
    assert_eq!(replay.state, PublishState::Posted);
    assert!(replay.repeated);
    assert_eq!(replay.permalink, outcome.permalink);
    assert_eq!(replay.provider_payload, Some(payload));
    assert_eq!(door.provider_calls(), 1);
    door.stop();
}

#[test]
fn cad798_provider_refusal_records_failed_without_evidence() {
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 3);
    door.set_enabled(true);
    door.behave("cad798-refuse-id-01", Behavior::Refuse);
    let sender = sender(&door);
    let binding = binding("cad798-refuse-id-01", GRANT);
    let outcome = sender.execute(&binding).expect("refusal is an outcome");
    assert_eq!(outcome.state, PublishState::Refused);
    assert!(outcome.permalink.is_none());
    assert!(outcome.provider_payload.is_none());
    assert!(!outcome.repeated);
    let replay = sender.execute(&binding).expect("replay");
    assert_eq!(replay.state, PublishState::Refused);
    assert!(replay.repeated);
    assert_eq!(door.provider_calls(), 1);
    door.stop();
}

#[test]
fn cad798_lost_response_reconciles_without_second_call() {
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 3);
    door.set_enabled(true);
    door.behave("cad798-lose-id-01", Behavior::Lose);
    let sender = sender(&door);
    let binding = binding("cad798-lose-id-01", GRANT);
    // The lost response reads as processing with no evidence.
    let outcome = sender.execute(&binding).expect("ambiguous execute");
    assert_eq!(outcome.state, PublishState::Processing);
    assert!(outcome.provider_payload.is_none());
    // Reconcile recovers posted with byte-exact evidence — no new call.
    let recovered = sender.status(&binding.key).expect("reconcile");
    assert_eq!(recovered.state, PublishState::Posted);
    assert!(recovered.repeated);
    assert!(recovered
        .provider_payload
        .as_deref()
        .unwrap_or("")
        .contains("door-post-1"));
    assert_eq!(door.provider_calls(), 1);
    // A later execute replays the recorded post, still one call.
    let replay = sender.execute(&binding).expect("replay");
    assert_eq!(replay.state, PublishState::Posted);
    assert_eq!(door.provider_calls(), 1);
    door.stop();
}

#[test]
fn cad798_restart_recovers_through_status_only() {
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 3);
    door.set_enabled(true);
    let key = "cad798-restart-id-01";
    let first = sender(&door);
    let outcome = first.execute(&binding(key, GRANT)).expect("post");
    assert_eq!(door.provider_calls(), 1);
    // Simulated restart: a fresh sender instance reconciles by key.
    let second = sender(&door);
    let recovered = second.status(key).expect("restart reconcile");
    assert_eq!(recovered.state, PublishState::Posted);
    assert_eq!(recovered.permalink, outcome.permalink);
    assert_eq!(recovered.provider_payload, outcome.provider_payload);
    assert_eq!(door.provider_calls(), 1);
    door.stop();
}

#[test]
fn cad798_timeout_is_processing_and_reconciles() {
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 3);
    door.set_enabled(true);
    door.behave("cad798-slow-id-01", Behavior::Sleep);
    let sender = HttpPublishSender::with_timeout(
        &format!("http://{}", door.addr),
        DeviceCredential::new(BEARER.into()),
        resolver(),
        Duration::from_millis(300),
    )
    .expect("loopback sender");
    let binding = binding("cad798-slow-id-01", GRANT);
    // The door accepts, then sleeps past the client bound: ambiguous,
    // so processing with no evidence — the POST is never retried.
    let outcome = sender.execute(&binding).expect("ambiguous execute");
    assert_eq!(outcome.state, PublishState::Processing);
    assert!(outcome.provider_payload.is_none());
    thread::sleep(Duration::from_secs(3));
    let recovered = sender.status(&binding.key).expect("reconcile");
    assert_eq!(recovered.state, PublishState::Posted);
    assert_eq!(door.provider_calls(), 1);
    door.stop();
}

#[test]
fn cad798_tampered_material_never_reaches_the_door() {
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 3);
    door.set_enabled(true);
    let calls_before = door.http_calls();
    let tampered: MaterialResolver = Arc::new(|_| {
        Ok(SendMaterial {
            caption: "Tampered caption.".into(),
            media_key: None,
        })
    });
    let sender = HttpPublishSender::new(
        &format!("http://{}", door.addr),
        DeviceCredential::new(BEARER.into()),
        tampered,
    )
    .expect("loopback sender");
    let refusal = sender
        .execute(&binding("cad798-tamper-id-01", GRANT))
        .expect_err("tamper refuses");
    assert_eq!(refusal.code, "grant_binding_mismatch");
    assert_eq!(door.http_calls(), calls_before);
    assert_eq!(door.provider_calls(), 0);
    door.stop();
}

#[test]
fn cad798_forged_grant_binding_refuses_without_provider_call() {
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 3);
    door.set_enabled(true);
    let sender = sender(&door);
    // A binding whose digest does not match the enrolled grant: the
    // door refuses content_mismatch and calls no provider.
    let mut forged = binding("cad798-forged-id-01", GRANT);
    forged.caption_digest = sha_hex(b"something-else");
    let refusal = sender.execute(&forged).expect_err("forged binding refuses");
    assert_eq!(refusal.code, "grant_binding_mismatch");
    assert_eq!(door.provider_calls(), 0);
    door.stop();
}

#[test]
fn cad798_revoked_and_exhausted_grants_refuse() {
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 1);
    door.issue("dpq_cad798_revoked_1", 3);
    door.revoke("dpq_cad798_revoked_1");
    door.set_enabled(true);
    let sender = sender(&door);
    let revoked = sender
        .execute(&binding("cad798-revoked-id-01", "dpq_cad798_revoked_1"))
        .expect_err("revoked grant refuses");
    assert_eq!(revoked.code, "grant_revoked");
    let posted = sender
        .execute(&binding("cad798-use-id-01", GRANT))
        .expect("first use posts");
    assert_eq!(posted.state, PublishState::Posted);
    let exhausted = sender
        .execute(&binding("cad798-use-key-02", GRANT))
        .expect_err("exhausted grant refuses");
    assert_eq!(exhausted.code, "grant_exhausted");
    assert_eq!(door.provider_calls(), 1);
    door.stop();
}

#[test]
fn cad798_unknown_key_status_refuses() {
    let door = FakeDoor::start();
    door.set_enabled(true);
    let sender = sender(&door);
    let refusal = sender
        .status("cad798-no-such-id-01")
        .expect_err("unknown key refuses");
    assert_eq!(refusal.code, "unknown_key");
    assert!(sender
        .preflight(
            &binding("cad798-no-such-id-01", GRANT),
            &resolver()(&binding("cad798-no-such-id-01", GRANT)).unwrap()
        )
        .is_err());
    door.stop();
}

#[test]
fn cad798_concurrent_claims_post_exactly_once() {
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 8);
    door.set_enabled(true);
    let sender = Arc::new(sender(&door));
    let binding = binding("cad798-race-id-01", GRANT);
    let outcomes: Vec<_> = (0..8)
        .map(|_| {
            let sender = Arc::clone(&sender);
            let binding = binding.clone();
            thread::spawn(move || sender.execute(&binding))
        })
        .map(|handle| handle.join().expect("claim thread"))
        .collect();
    for outcome in &outcomes {
        let outcome = outcome.as_ref().expect("every claim answers");
        assert_eq!(outcome.state, PublishState::Posted);
    }
    assert_eq!(door.provider_calls(), 1);
    door.stop();
}

#[test]
fn cad798_flaky_preflight_recovers_before_send() {
    // Staging failover is retried pre-send (staging never mutates,
    // never sends): one failed preflight, then staging, then exactly
    // one execution POST and one provider call.
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 3);
    door.set_enabled(true);
    door.fail_next_preflight("cad798-flaky-id-01");
    let sender = sender(&door);
    let outcome = sender
        .execute(&binding("cad798-flaky-id-01", GRANT))
        .expect("retry recovers");
    assert_eq!(outcome.state, PublishState::Posted);
    assert_eq!(door.preflight_calls(), 2);
    assert_eq!(door.exec_calls(), 1);
    assert_eq!(door.provider_calls(), 1);
    door.stop();
}

#[test]
fn cad798_persistent_preflight_ambiguity_is_nothing_sent_refusal() {
    // Persistent staging ambiguity must fail loud with a nothing-sent
    // refusal — never processing, which could only status-reconcile to
    // 404 forever for a key the door never saw.
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 3);
    door.set_enabled(true);
    door.set_preflight_down(true);
    let sender = sender(&door);
    let refusal = sender
        .execute(&binding("cad798-down-id-01", GRANT))
        .expect_err("persistent staging ambiguity refuses");
    assert!(
        refusal.detail.contains("nothing was sent"),
        "unexpected: {refusal}"
    );
    assert_eq!(door.preflight_calls(), 2);
    assert_eq!(door.exec_calls(), 0);
    assert_eq!(door.provider_calls(), 0);
    door.stop();
}

#[test]
fn cad798_preflight_refusal_sends_nothing() {
    // Exact-binding preflight precedes the first POST: a revoked grant
    // refuses at staging, so zero execution POSTs leave the client.
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 3);
    door.issue("dpq_cad798_revoked_1", 3);
    door.revoke("dpq_cad798_revoked_1");
    door.set_enabled(true);
    let sender = sender(&door);
    let refusal = sender
        .execute(&binding("cad798-staged-id-01", "dpq_cad798_revoked_1"))
        .expect_err("staging refusal sends nothing");
    assert_eq!(refusal.code, "grant_revoked");
    assert_eq!(door.exec_calls(), 0);
    assert_eq!(door.provider_calls(), 0);
    // A live grant still stages then posts exactly once.
    let posted = sender
        .execute(&binding("cad798-staged-id-02", GRANT))
        .expect("post");
    assert_eq!(posted.state, PublishState::Posted);
    assert_eq!(door.exec_calls(), 1);
    assert_eq!(door.provider_calls(), 1);
    door.stop();
}

#[test]
fn cad798_accepted_then_503_recovers_through_status() {
    // The door accepts (records, consumes, calls the provider) then
    // answers 503 with a JSON error document: ambiguity, never a
    // definitive refusal — one POST, recovery through status only.
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 3);
    door.set_enabled(true);
    door.behave("cad798-failover-id-01", Behavior::AcceptThen503);
    let sender = sender(&door);
    let outcome = sender
        .execute(&binding("cad798-failover-id-01", GRANT))
        .expect("ambiguous");
    assert_eq!(outcome.state, PublishState::Processing);
    assert!(outcome.provider_payload.is_none());
    assert_eq!(door.exec_calls(), 1);
    assert_eq!(door.provider_calls(), 1);
    let recovered = sender.status("cad798-failover-id-01").expect("reconcile");
    assert_eq!(recovered.state, PublishState::Posted);
    assert!(recovered
        .provider_payload
        .as_deref()
        .unwrap_or("")
        .contains("door-post-1"));
    assert_eq!(door.exec_calls(), 1);
    assert_eq!(door.provider_calls(), 1);
    door.stop();
}

#[test]
fn cad798_exec_drift_is_ambiguous_until_status_proof() {
    // A drifted execution verdict after an accepted POST stays
    // ambiguity (processing) until status proof — never a terminal
    // refusal for a send that may have posted. Status drift itself
    // still refuses loudly on reads.
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 3);
    door.set_enabled(true);
    door.drift_next_exec("cad798-acceptdrift-id-01");
    let sender = sender(&door);
    let outcome = sender
        .execute(&binding("cad798-acceptdrift-id-01", GRANT))
        .expect("ambiguous");
    assert_eq!(outcome.state, PublishState::Processing);
    assert_eq!(door.provider_calls(), 1);
    let recovered = sender
        .status("cad798-acceptdrift-id-01")
        .expect("status proof");
    assert_eq!(recovered.state, PublishState::Posted);
    assert_eq!(door.provider_calls(), 1);
    door.set_version("2");
    let status = sender
        .status("cad798-acceptdrift-id-01")
        .expect_err("status drift refuses");
    assert!(status.detail.contains("drift"), "unexpected: {status}");
    door.stop();
}

#[test]
fn cad798_wrong_credential_fails_closed() {
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 3);
    door.set_enabled(true);
    let sender = HttpPublishSender::new(
        &format!("http://{}", door.addr),
        DeviceCredential::new("wrong-credential".into()),
        resolver(),
    )
    .expect("loopback sender");
    let refusal = sender
        .execute(&binding("cad798-auth-id-01", GRANT))
        .expect_err("bad credential refuses");
    assert_eq!(refusal.code, "unauthorized");
    assert_eq!(door.provider_calls(), 0);
    door.stop();
}

#[test]
fn cad798_wire_carries_no_workspace_and_bears_the_credential() {
    let door = FakeDoor::start();
    door.enroll_connection(CONN);
    door.issue(GRANT, 3);
    door.set_enabled(true);
    let sender = sender(&door);
    let binding = binding("cad798-bind-id-01", GRANT);
    assert_eq!(
        sender.preflight(&binding, &resolver()(&binding).unwrap()),
        Ok(false)
    );
    sender.execute(&binding).expect("post");
    sender.status(&binding.key).expect("status");
    let bodies = door.state.lock().unwrap().bodies.clone();
    assert!(bodies.len() >= 2);
    for body in &bodies {
        assert!(
            body.get("workspaceId").is_none(),
            "wire names a workspace: {body}"
        );
        assert!(
            body.get("workspace_id").is_none(),
            "wire names a workspace: {body}"
        );
        assert!(
            body["grant"].get("workspaceId").is_none(),
            "grant names a workspace: {body}"
        );
        assert_eq!(body["caption"], json!(CAPTION));
    }
    door.stop();
}

// ---------- explicit-config registration (default-off) ----------

static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn clear_env() {
    unsafe {
        std::env::remove_var(PUBLISH_SEND_URL_ENV);
        std::env::remove_var(PUBLISH_SEND_CREDENTIAL_FILE_ENV);
    }
}

fn credential_file(secret: &str) -> tempfile::NamedTempFile {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let mut file = tempfile::NamedTempFile::new().expect("credential file");
    file.write_all(secret.as_bytes()).expect("credential bytes");
    file.flush().expect("credential flush");
    std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o600))
        .expect("credential mode");
    file
}

#[test]
fn cad798_registration_is_default_off() {
    let _guard = ENV_GUARD.lock().unwrap();
    clear_env();
    let state = tempfile::tempdir().expect("state dir");
    let mut opts = cadence_agent::daemon::ServeOptions::default();
    cadence_agent::platform::agenticos_external::publish_sender::attach_publish_sender(
        state.path(),
        &mut opts,
    )
    .expect("default-off registers nothing");
    assert!(opts.social_publish_sender.is_none());
}

#[test]
fn cad798_registration_rejects_one_sided_config() {
    let _guard = ENV_GUARD.lock().unwrap();
    clear_env();
    unsafe {
        std::env::set_var(PUBLISH_SEND_URL_ENV, "http://127.0.0.1:9");
    }
    let state = tempfile::tempdir().expect("state dir");
    let mut opts = cadence_agent::daemon::ServeOptions::default();
    assert!(
        cadence_agent::platform::agenticos_external::publish_sender::attach_publish_sender(
            state.path(),
            &mut opts,
        )
        .is_err()
    );
    assert!(opts.social_publish_sender.is_none());
    clear_env();
}

#[test]
fn cad798_registration_rejects_bad_credential_mode() {
    let _guard = ENV_GUARD.lock().unwrap();
    clear_env();
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let mut file = tempfile::NamedTempFile::new().expect("credential file");
    file.write_all(b"synthetic").expect("bytes");
    file.flush().expect("flush");
    std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o644)).expect("mode");
    unsafe {
        std::env::set_var(PUBLISH_SEND_URL_ENV, "http://127.0.0.1:9");
        std::env::set_var(PUBLISH_SEND_CREDENTIAL_FILE_ENV, file.path());
    }
    let state = tempfile::tempdir().expect("state dir");
    let mut opts = cadence_agent::daemon::ServeOptions::default();
    let err = cadence_agent::platform::agenticos_external::publish_sender::attach_publish_sender(
        state.path(),
        &mut opts,
    )
    .expect_err("group-readable credential refuses");
    assert!(err.to_string().contains("0600"), "unexpected: {err}");
    assert!(opts.social_publish_sender.is_none());
    clear_env();
}

// ---------- daemon-level proofs: approved run to posted report ----------

use common::app_release::{Release, A};

fn epoch_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// A daemon with the PRODUCTION sender registered: store-backed material
/// re-proof, real operator RPCs, fake door on the loopback wire.
fn daemon_release(door: &FakeDoor) -> (Release, Arc<HttpPublishSender>) {
    let base = format!("http://{}", door.addr);
    let sender = Arc::new(
        HttpPublishSender::new(
            &base,
            DeviceCredential::new(BEARER.into()),
            Arc::new(|_| {
                Err(
                    cadence_agent::platform::agenticos_external::publish::Refusal::new(
                        "bad_effect",
                        "unconfigured",
                    ),
                )
            }),
        )
        .expect("loopback sender"),
    );
    let registered = Arc::clone(&sender);
    let base_clone = base.clone();
    let h = Release::with_options(move |opts, state| {
        let _ = &registered;
        opts.social_publish_sender = Some(Arc::new(
            HttpPublishSender::new(
                &base_clone,
                DeviceCredential::new(BEARER.into()),
                cadence_agent::platform::agenticos_external::publish_sender::production_resolver(
                    state,
                ),
            )
            .expect("loopback sender"),
        ));
        // CAD-979 v9: the schedule path resolves the remote AOS
        // `connectionId` via a real `MediaResolver` against the fake door's
        // destinations route (read credential, separate from the send one).
        let base3 = base.clone();
        opts.social_media_resolver = Some(Arc::new(
            cadence_agent::platform::agenticos_external::media_import::MediaResolver::new(
                &base3,
                DeviceCredential::new(READ_BEARER.into()),
            )
            .expect("fake destinations resolver"),
        ));
    });
    (h, sender)
}

fn approved_run(h: &Release, tag: &str) -> (Value, Value, String, String) {
    let context = h.context("Harbour", A, &format!("cad798-{tag}-context"));
    h.bind(&context, &format!("cad798-{tag}-binding"));
    let run = h.complete(&context, &format!("cad798-{tag}-run"));
    let bundle_digest = run["snapshot"]["bundle_digest"]
        .as_str()
        .unwrap()
        .to_owned();
    let install_id = h.install["install_id"].as_str().unwrap().to_owned();
    (context, run, bundle_digest, install_id)
}

fn freeze_params(
    context: &Value,
    run: &Value,
    bundle_digest: &str,
    install_id: &str,
    request: &str,
    grant: &str,
    due: i64,
) -> Value {
    json!({"request_id": request, "install_id": install_id,
        "context_id": context["id"], "run_id": run["id"],
        "artifact_id": run["artifacts"][0]["id"],
        "bundle_digest": bundle_digest,
        "slot": "publication", "effect_id": "cad_fx_798_e2e_01",
        "destination_id": DEST, "toolkit": "facebook",
        "grant_id": grant, "approval_id": approval_for(request),
        "due_epoch": due, "timezone": "Asia/Hong_Kong"})
}

fn recheck_for(intent: &Value) -> Value {
    let frozen = &intent["frozen"];
    // v9: the claim recheck carries the remote AOS wire identity — the field
    // the frozen approval binds (and the door's grant authorizes against).
    json!({"grant_id": frozen["grant_id"],
        "connection_id": frozen["connection_id"],
        "aos_connection_id": frozen["aos_connection_id"],
        "destination_id": frozen["destination_id"],
        "caption_digest": frozen["caption_digest"],
        "image_digest": frozen["image_digest"]})
}

#[test]
fn cad798_daemon_posts_from_approved_run_with_store_backed_material() {
    let door = FakeDoor::start();
    door.set_enabled(true);
    let (h, sender) = daemon_release(&door);
    let (context, run, bundle_digest, install_id) = approved_run(&h, "post");
    let _ = &context;
    // The reviewed caption text is the artifact body; its digest is the
    // frozen approval the door authorizes against.
    let caption = h.artifact(&run)["text"].as_str().unwrap().to_owned();
    assert!(!caption.is_empty());
    let caption_digest = sha_hex(caption.as_bytes());
    let grant = "dpq_cad798_daemon_01";
    let due = epoch_now();
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context,
                &run,
                &bundle_digest,
                &install_id,
                "cad798-daemon-post",
                grant,
                due,
            ),
        )
        .unwrap()["intent"]
        .clone();
    assert_eq!(intent["state"], "queued");
    assert_eq!(intent["frozen"]["caption_digest"], json!(caption_digest));
    // Enroll the owner-minted grant AFTER freeze: connection and content
    // come from the frozen approval, never from caller strings.
    // v9: the door authorizes the wire `connectionId` — the remote AOS id
    // persisted as `aos_connection_id` — not the local custody connection.
    let aos_conn = intent["frozen"]["aos_connection_id"].as_str().unwrap();
    door.enroll_connection(aos_conn);
    door.enroll_grant(grant, aos_conn, DEST, &caption_digest, None, 3);
    // Dispatch through the production sender: the door posts once.
    let claimed = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": due + 5, "recheck": recheck_for(&intent)}),
        )
        .unwrap();
    assert_eq!(claimed["intent"]["state"], "processing");
    let evidence = &claimed["intent"]["upstream"];
    assert_eq!(evidence["state"], "posted");
    assert!(!evidence["permalink"].as_str().unwrap_or("").is_empty());
    // Byte-exact: the daemon-persisted evidence equals a direct status
    // read through the same sender — the report below must replay it.
    let key = claimed["intent"]["request"].as_str().unwrap();
    let direct = sender.status(key).expect("direct status").evidence_json();
    assert_eq!(*evidence, direct);
    assert_eq!(door.provider_calls(), 1);
    let posted = h
        .daemon
        .operator_rpc(
            "social_publish_report",
            json!({"intent_id": claimed["intent"]["intent_id"], "decision": "posted",
                "receipt": {"permalink": evidence["permalink"],
                    "destination_id": DEST, "caption_digest": caption_digest,
                    "image_digest": intent["frozen"]["image_digest"],
                    "provider_ids": evidence["provider_ids"],
                    "provider_payload": evidence["provider_payload"]}}),
        )
        .unwrap()["intent"]
        .clone();
    assert_eq!(posted["state"], "posted");
    // Re-claim finds nothing: no second send, ever.
    let idle = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": due + 5, "recheck": recheck_for(&intent)}),
        )
        .unwrap();
    assert_eq!(idle["claimed"], false);
    assert_eq!(door.provider_calls(), 1);
}

/// A hostile sender: answers `posted` for a FOREIGN binding (different
/// destination and digests) with attacker provider bytes. Models a
/// compromised or confused door row for the same idempotency key.
struct EvilSender;

impl PublishSender for EvilSender {
    fn execute(&self, _binding: &SendBinding) -> std::result::Result<LedgerOutcome, Refusal> {
        Ok(LedgerOutcome {
            state: PublishState::Posted,
            permalink: Some("https://evil.test/p/1".into()),
            destination_id: "999999999999999".into(),
            caption_digest: sha_hex(b"evil-caption"),
            image_digest: None,
            provider_payload: Some(r#"{"id":"evil-post-1"}"#.into()),
            provider_ids: vec!["evil-post-1".into()],
            repeated: false,
        })
    }

    fn status(&self, _key: &str) -> std::result::Result<LedgerOutcome, Refusal> {
        Err(Refusal::new("unknown_key", "no send under this key"))
    }
}

#[test]
fn cad798_foreign_binding_evidence_never_posts() {
    // Cross-binding laundering pinned in 771-canonical terms: a posted
    // outcome for another destination/digest must end refused, with the
    // intent processing and no upstream evidence — even when the receipt
    // copies every frozen field and replays the foreign provider bytes.
    // Behavior assertions only (refusal, processing, null upstream);
    // no sender-side message strings. RED until the 771 lifecycle lands
    // its own binding guard; the transport ships no duplicate of it.
    // v9: schedule resolves the remote AOS `connectionId` via the read
    // credential; a destinations door is needed for resolution even though
    // the SEND path is the hostile `EvilSender` (preserved).
    let door = FakeDoor::start();
    let dest_base = format!("http://{}", door.addr);
    let h = Release::with_options(move |opts, _| {
        opts.social_publish_sender = Some(Arc::new(EvilSender));
        opts.social_media_resolver = Some(Arc::new(
            cadence_agent::platform::agenticos_external::media_import::MediaResolver::new(
                &dest_base,
                DeviceCredential::new(READ_BEARER.into()),
            )
            .expect("fake destinations resolver"),
        ));
    });
    let (context, run, bundle_digest, install_id) = approved_run(&h, "evil");
    let _ = &context;
    let caption = h.artifact(&run)["text"].as_str().unwrap().to_owned();
    let caption_digest = sha_hex(caption.as_bytes());
    let grant = "dpq_cad798_daemon_03";
    let due = epoch_now();
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context,
                &run,
                &bundle_digest,
                &install_id,
                "cad798-daemon-evil",
                grant,
                due,
            ),
        )
        .unwrap()["intent"]
        .clone();
    // Dispatch answers posted-for-elsewhere: the lifecycle must refuse
    // to persist it as upstream evidence.
    h.daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": due + 5, "recheck": recheck_for(&intent)}),
        )
        .expect_err("foreign evidence must not persist");
    let shown = h
        .daemon
        .operator_rpc(
            "social_publish_show",
            json!({"intent_id": intent["intent_id"]}),
        )
        .unwrap()["intent"]
        .clone();
    assert_eq!(shown["state"], "processing");
    assert!(shown["upstream"].is_null());
    // And a forged receipt replaying those bytes must not post either.
    h.daemon
        .operator_rpc(
            "social_publish_report",
            json!({"intent_id": intent["intent_id"], "decision": "posted",
                "receipt": {"permalink": "https://evil.test/p/1",
                    "destination_id": intent["frozen"]["destination_id"],
                    "caption_digest": caption_digest,
                    "image_digest": intent["frozen"]["image_digest"],
                    "provider_ids": ["evil-post-1"],
                    "provider_payload": r#"{"id":"evil-post-1"}"#}}),
        )
        .expect_err("laundered receipt must not post");
    let shown = h
        .daemon
        .operator_rpc(
            "social_publish_show",
            json!({"intent_id": intent["intent_id"]}),
        )
        .unwrap()["intent"]
        .clone();
    assert_eq!(shown["state"], "processing");
}

#[test]
fn cad798_registration_accepts_explicit_config() {
    let _guard = ENV_GUARD.lock().unwrap();
    clear_env();
    let credential = credential_file("cad798-synthetic-credential\n");
    unsafe {
        std::env::set_var(PUBLISH_SEND_URL_ENV, "http://127.0.0.1:9");
        std::env::set_var(PUBLISH_SEND_CREDENTIAL_FILE_ENV, credential.path());
    }
    let state = tempfile::tempdir().expect("state dir");
    let mut opts = cadence_agent::daemon::ServeOptions::default();
    cadence_agent::platform::agenticos_external::publish_sender::attach_publish_sender(
        state.path(),
        &mut opts,
    )
    .expect("explicit config registers");
    assert!(opts.social_publish_sender.is_some());
    clear_env();
}

/// CAD-1027: a daemon-shaped approval id (`apv-` + 32 lowercase hex),
/// distinct per seed — one approval authorizes one intent.
fn approval_for(seed: &str) -> String {
    use sha2::Digest as _;
    let hex = format!("{:x}", sha2::Sha256::digest(seed.as_bytes()));
    format!("apv-{}", &hex[..32])
}
