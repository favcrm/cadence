//! CAD-771 slice-3 fake-provider end-to-end proofs.
//!
//! Pinned contract: `agenticos-stack/agenticos-v2` PR #214 @
//! `12953144c50d13075af2323a2e09a70de9f72b87` (device-publish v1).
//! AOS-94 is still unmerged, so every provider byte here comes from the
//! loopback fake door below — no live Meta post, no paid call, no real
//! credential. The exact-destination send adapter registration stays gated
//! until AOS-94 lands and the contract is revalidated.
//!
//! Shape note: the harness produces genuinely approved text-only runs, so
//! the run-freeze path exercises Facebook text-only sends end to end.
//! Instagram-with-image is proven at contract level (slice 1: IG refuses
//! without an image digest) and store level; its E2E with a reviewed asset
//! waits on an asset-bearing harness run. The fake door speaks both
//! destination shapes, and the connection namespace below is the fake
//! provider's own — the local-bundle→provider connection mapping is what
//! the gated adapter registration will own.
//!
//! The driver speaks only operator RPCs (`social_publish_*`) against a real
//! daemon, enforces backend grant liveness (uses/revocation) at dispatch,
//! and reconciles lost responses without a second send. Actor parity:
//! unproven agent-shaped calls are refused, operator calls succeed, forged
//! fields fail closed.
#![allow(clippy::disallowed_methods)]
mod common;

use cadence_agent::platform::agenticos_external::publish::{
    Destination, FakeProviderBehavior, FakePublishLedger, SendBinding, SendGrant, Toolkit,
};
use common::app_release::{Release, A};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const DEST_FB: &str = "275491372109884";
const CONN_FB: &str = "con_harbour_fb";
/// CAD-979 v9 remote AOS `connectionId` (the wire identity the resolver
/// returns; the local `con_harbour_*` stays the custody/install id).
const AOS_IG: &str = "connA_harbour_ig";
const AOS_FB: &str = "connA_harbour_fb";
const GRANT_FB: &str = "dpq_synthetic_grant_fb";
const DEST_IG: &str = "17841400008460056";
const CONN_IG: &str = "con_harbour_ig";

fn discovery(toolkit: Toolkit) -> Destination {
    match toolkit {
        Toolkit::Instagram => Destination {
            connection_id: CONN_IG.into(),
            toolkit: Toolkit::Instagram,
            display_name: "Harbour stills".into(),
            destination_id: DEST_IG.into(),
            status_active: true,
            available: true,
        },
        Toolkit::Facebook => Destination {
            connection_id: CONN_FB.into(),
            toolkit: Toolkit::Facebook,
            display_name: "Harbour page".into(),
            destination_id: DEST_FB.into(),
            status_active: true,
            available: true,
        },
    }
}

/// Backend grant liveness, fake-side: remaining uses plus revocation.
/// Dispatch consults this fresh — never the schedule-time copy.
#[derive(Default)]
struct GrantAuthority {
    grants: HashMap<String, (u32, bool)>,
}

impl GrantAuthority {
    fn issue(&mut self, id: &str, uses: u32) {
        self.grants.insert(id.into(), (uses, false));
    }

    fn revoke(&mut self, id: &str) {
        if let Some(entry) = self.grants.get_mut(id) {
            entry.1 = true;
        }
    }

    fn check(&self, id: &str) -> Result<u32, &'static str> {
        match self.grants.get(id) {
            None => Err("grant_mismatch"),
            Some((_, true)) => Err("grant_revoked"),
            Some((0, false)) => Err("grant_exhausted"),
            Some((uses, false)) => Ok(*uses),
        }
    }

    fn consume(&mut self, id: &str) {
        if let Some(entry) = self.grants.get_mut(id) {
            entry.0 = entry.0.saturating_sub(1);
        }
    }
}

struct FakeDoor {
    addr: String,
    grants: Arc<Mutex<GrantAuthority>>,
    ledger: Arc<FakePublishLedger>,
    calls: Arc<Mutex<u64>>,
    /// CAD-1041: staging (`/preflight`) requests. Staging never reaches
    /// the provider, so it is counted here and never in `calls`.
    stages: Arc<Mutex<u64>>,
    /// Owner-authorized destination the fake discovery returns. The fake
    /// trusts the enrolled connection namespace (operator-side) and
    /// enforces destination-exactness against this value; unset means the
    /// tests drive discovery echo for liveness-only paths.
    expected_destination: Arc<Mutex<Option<String>>>,
    /// Adversarial hook: when set, ok responses omit the binding echo.
    omit_binding: Arc<std::sync::atomic::AtomicBool>,
    /// Adversarial hook: when set, ok responses carry a mismatched
    /// binding (different destination/caption) under the same key.
    corrupt_binding: Arc<std::sync::atomic::AtomicBool>,
    /// Adversarial hook: forged status outcomes keyed by stable key. A
    /// hostile provider answering the same key with a foreign binding.
    status_forgeries: Arc<Mutex<HashMap<String, Value>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl FakeDoor {
    fn start() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap().to_string();
        let grants = Arc::new(Mutex::new(GrantAuthority::default()));
        let ledger = Arc::new(FakePublishLedger::enabled());
        let calls = Arc::new(Mutex::new(0u64));
        let stages = Arc::new(Mutex::new(0u64));
        let expected_destination = Arc::new(Mutex::new(None));
        let omit_binding = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let corrupt_binding = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let status_forgeries = Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_grants = Arc::clone(&grants);
        let worker_ledger = Arc::clone(&ledger);
        let worker_calls = Arc::clone(&calls);
        let worker_stages = Arc::clone(&stages);
        let worker_expected = Arc::clone(&expected_destination);
        let worker_omit = Arc::clone(&omit_binding);
        let worker_forgeries = Arc::clone(&status_forgeries);
        let worker_corrupt = Arc::clone(&corrupt_binding);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(std::sync::atomic::Ordering::SeqCst) {
                let Ok(Some(mut request)) = server.recv_timeout(Duration::from_millis(50)) else {
                    continue;
                };
                // CAD-979 v9: the destinations GET is a read-only lookup,
                // not a provider send — it must NOT count toward the
                // exactly-once send-call assertions.
                let is_destinations_read = request.url().contains("/connectors/destinations");
                if request.url().ends_with("/preflight") {
                    *worker_stages.lock().unwrap() += 1;
                } else if !is_destinations_read {
                    *worker_calls.lock().unwrap() += 1;
                }
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap_or(0);
                let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let shared = DoorShared {
                    grants: &worker_grants,
                    ledger: &worker_ledger,
                    expected: &worker_expected,
                    omit_binding: &worker_omit,
                    corrupt_binding: &worker_corrupt,
                    forgeries: &worker_forgeries,
                };
                let reply = Self::route(&shared, request.url(), &value);
                let _ = request.respond(tiny_http::Response::from_string(reply.to_string()));
            }
        });
        Self {
            addr,
            grants,
            ledger,
            calls,
            stages,
            expected_destination,
            omit_binding,
            corrupt_binding,
            status_forgeries,
            stop,
            worker: Some(worker),
        }
    }

    fn omit_binding_echo(&self, omit: bool) {
        self.omit_binding
            .store(omit, std::sync::atomic::Ordering::SeqCst);
    }

    fn corrupt_binding_echo(&self, corrupt: bool) {
        self.corrupt_binding
            .store(corrupt, std::sync::atomic::Ordering::SeqCst);
    }

    /// Hostile provider: answer one stable key with a forged outcome.
    fn forge_status(&self, key: &str, outcome: Value) {
        self.status_forgeries
            .lock()
            .unwrap()
            .insert(key.into(), outcome);
    }

    fn expect_destination(&self, destination_id: &str) {
        *self.expected_destination.lock().unwrap() = Some(destination_id.into());
    }

    /// Native-vs-HTTP parity core: the HTTP door reaches exactly the verdicts
    /// the native [`FakePublishLedger`] gate computes for the same binding.
    fn route(shared: &DoorShared<'_>, url: &str, value: &Value) -> Value {
        // CAD-979 v9: the destinations read supplies the local→AOS
        // `connectionId` map for `(toolkit, destination_id)` — the wire
        // identity the send binding carries. Answered on a GET path with no
        // `SendBinding` to parse, before the binding decode below.
        if url.contains("/connectors/destinations") {
            return json!({"ok": true, "data": [
                {"connectionId": AOS_IG, "toolkit": "instagram",
                 "displayName": "ig", "destinationId": DEST_IG,
                 "status": "active", "available": true, "publishable": true},
                {"connectionId": AOS_FB, "toolkit": "facebook",
                 "displayName": "fb", "destinationId": DEST_FB,
                 "status": "active", "available": true, "publishable": true},
            ]});
        }
        let toolkit =
            Toolkit::parse(value["toolkit"].as_str().unwrap_or("")).unwrap_or(Toolkit::Facebook);
        let binding = SendBinding {
            key: value["key"].as_str().unwrap_or("").into(),
            connection_id: value["connection_id"].as_str().unwrap_or("").into(),
            destination_id: value["destination_id"].as_str().unwrap_or("").into(),
            toolkit,
            caption_digest: value["caption_digest"].as_str().unwrap_or("").into(),
            image_digest: value["image_digest"].as_str().map(str::to_owned),
            cadence_run_id: value["cadence_run_id"].as_str().unwrap_or("").into(),
            cadence_effect_id: value["cadence_effect_id"].as_str().unwrap_or("").into(),
            grant_id: value["grant_id"].as_str().unwrap_or("").into(),
        };
        let mut dest = discovery(toolkit);
        // The fake trusts the operator-enrolled connection namespace and
        // enforces destination-exactness against owner-authorized
        // discovery (explicit expectation) or echo (liveness-only paths).
        dest.connection_id = binding.connection_id.clone();
        if let Some(expected) = shared.expected.lock().unwrap().clone() {
            dest.destination_id = expected;
        } else {
            dest.destination_id = binding.destination_id.clone();
        }
        let mut grant = SendGrant {
            id: binding.grant_id.clone(),
            workspace_id: "ws_harbour".into(),
            connection_id: binding.connection_id.clone(),
            destination_id: binding.destination_id.clone(),
            toolkit: binding.toolkit,
            caption_digest: binding.caption_digest.clone(),
            image_digest: binding.image_digest.clone(),
            cadence_approval_id: "cad_approval_01".into(),
            max_uses: 3,
            remaining_uses: 3,
            revoked: false,
            not_before_epoch: 1_700_000_000,
            expires_at_epoch: 1_800_000_000,
        };
        // Backend liveness first: the schedule-time copy is never trusted.
        let live = shared.grants.lock().unwrap().check(&binding.grant_id);
        if url.ends_with("/preflight") {
            if let Err(code) = live {
                return json!({"verdict": "refused", "code": code});
            }
            return match shared
                .ledger
                .preflight(&binding, &dest, &grant, "ws_harbour", NOW)
            {
                Ok(staged) => json!({"verdict": "ok", "staged": staged}),
                Err(refusal) => json!({"verdict": "refused", "code": refusal.code}),
            };
        }
        if url.ends_with("/exec") {
            if let Err(code) = live {
                return json!({"verdict": "refused", "code": code});
            }
            let behavior = match value["behavior"].as_str().unwrap_or("post") {
                "refuse" => FakeProviderBehavior::Refuse,
                "lose" => FakeProviderBehavior::LoseResponseAfterAccept,
                _ => FakeProviderBehavior::Post,
            };
            return match shared.ledger.execute(
                &binding,
                &dest,
                &mut grant,
                "ws_harbour",
                NOW,
                behavior,
            ) {
                Ok(outcome) => {
                    shared.grants.lock().unwrap().consume(&binding.grant_id);
                    let mut reply = json!({"verdict": "ok", "state": outcome.state.as_str(),
                        "permalink": outcome.permalink,
                        "provider_ids": outcome.provider_ids,
                        "provider_payload": outcome.provider_payload,
                        "destination_id": outcome.destination_id,
                        "caption_digest": outcome.caption_digest,
                        "image_digest": outcome.image_digest,
                        "repeated": outcome.repeated});
                    if shared
                        .omit_binding
                        .load(std::sync::atomic::Ordering::SeqCst)
                    {
                        // Adversarial hook: upstream echo goes missing.
                        for field in ["destination_id", "caption_digest", "image_digest"] {
                            reply.as_object_mut().unwrap().remove(field);
                        }
                    }
                    if shared
                        .corrupt_binding
                        .load(std::sync::atomic::Ordering::SeqCst)
                    {
                        // Adversarial hook: same key, foreign binding.
                        let forged = reply.as_object_mut().unwrap();
                        forged.insert(
                            "destination_id".into(),
                            Value::String("999999999999999".into()),
                        );
                        forged.insert(
                            "caption_digest".into(),
                            Value::String(
                                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                                    .into(),
                            ),
                        );
                    }
                    reply
                }
                Err(refusal) => json!({"verdict": "refused", "code": refusal.code}),
            };
        }
        if url.starts_with("/v1/device/publish/status") {
            if let Some(forged) = shared.forgeries.lock().unwrap().get(&binding.key) {
                return forged.clone();
            }
            return match shared.ledger.status(&binding.key) {
                Ok(outcome) => json!({"verdict": "ok", "state": outcome.state.as_str(),
                    "permalink": outcome.permalink,
                    "provider_ids": outcome.provider_ids,
                    "provider_payload": outcome.provider_payload,
                    "destination_id": outcome.destination_id,
                    "caption_digest": outcome.caption_digest,
                    "image_digest": outcome.image_digest,
                    "repeated": outcome.repeated}),
                Err(refusal) => json!({"verdict": "refused", "code": refusal.code}),
            };
        }
        json!({"verdict": "refused", "code": "unknown_route"})
    }

    fn post(&self, path: &str, body: &Value) -> Value {
        let agent = ureq::Agent::new_with_defaults();
        let mut response = agent
            .post(format!("http://{}{path}", self.addr))
            .send_json(body)
            .unwrap();
        let text = response.body_mut().read_to_string().unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn provider_calls(&self) -> u64 {
        *self.calls.lock().unwrap()
    }

    fn stages(&self) -> u64 {
        *self.stages.lock().unwrap()
    }
}

/// Shared fake state bundled so the route stays under the argument
/// limit: six handles, one struct.
struct DoorShared<'a> {
    grants: &'a Mutex<GrantAuthority>,
    ledger: &'a FakePublishLedger,
    expected: &'a Mutex<Option<String>>,
    omit_binding: &'a std::sync::atomic::AtomicBool,
    corrupt_binding: &'a std::sync::atomic::AtomicBool,
    forgeries: &'a Mutex<HashMap<String, Value>>,
}

impl Drop for FakeDoor {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

const NOW: i64 = 1_750_000_000;

/// Daemon-side test sender: speaks the fake door over loopback HTTP and
/// maps its verdicts to ledger outcomes. Grant liveness, key folding and
/// no-second-call semantics stay door-side; the daemon only observes.
struct HttpSender {
    base: String,
    behaviors: Mutex<HashMap<String, FakeProviderBehavior>>,
    /// Test-only forged status outcomes keyed by stable key. When present
    /// for a key, status() returns the forgery without touching the wire —
    /// a second, sender-side injection point beside the door hook.
    status_forgeries:
        Mutex<HashMap<String, cadence_agent::platform::agenticos_external::publish::LedgerOutcome>>,
    /// CAD-1041: scripted staging verdicts by key. An unscripted key
    /// stages through the door's `/preflight` route, as the production
    /// sender does.
    preflights: Mutex<HashMap<String, Staging>>,
    /// CAD-1041: scripted execute refusals by key — the code returns
    /// before any wire call (e.g. `nothing_sent`).
    execute_refusals: Mutex<HashMap<String, &'static str>>,
}

/// A scripted staging verdict for one key.
#[derive(Clone, Copy)]
enum Staging {
    Uncertain,
    Refused(&'static str),
}

impl HttpSender {
    fn new(base: String) -> Self {
        Self {
            base,
            behaviors: Mutex::new(HashMap::new()),
            status_forgeries: Mutex::new(HashMap::new()),
            preflights: Mutex::new(HashMap::new()),
            execute_refusals: Mutex::new(HashMap::new()),
        }
    }

    /// Script the staging verdict for `key`; `None` stages at the door.
    fn script_preflight(&self, key: &str, staging: Option<Staging>) {
        let mut scripts = self.preflights.lock().unwrap();
        match staging {
            Some(staging) => scripts.insert(key.into(), staging),
            None => scripts.remove(key),
        };
    }

    fn script_execute_refusal(&self, key: &str, code: &'static str) {
        self.execute_refusals
            .lock()
            .unwrap()
            .insert(key.into(), code);
    }

    /// The exact binding as the fake door reads it.
    fn wire(binding: &SendBinding) -> Value {
        json!({"key": binding.key,
            "connection_id": binding.connection_id,
            "toolkit": binding.toolkit.as_str(),
            "destination_id": binding.destination_id,
            "caption_digest": binding.caption_digest,
            "image_digest": binding.image_digest,
            "cadence_run_id": binding.cadence_run_id,
            "cadence_effect_id": binding.cadence_effect_id,
            "grant_id": binding.grant_id})
    }

    fn forge_status(
        &self,
        key: &str,
        outcome: cadence_agent::platform::agenticos_external::publish::LedgerOutcome,
    ) {
        self.status_forgeries
            .lock()
            .unwrap()
            .insert(key.into(), outcome);
    }

    fn set_behavior(&self, key: &str, behavior: FakeProviderBehavior) {
        self.behaviors.lock().unwrap().insert(key.into(), behavior);
    }

    fn post(&self, path: &str, body: &Value) -> Value {
        let agent = ureq::Agent::new_with_defaults();
        let mut response = agent
            .post(format!("{}{path}", self.base))
            .send_json(body)
            .unwrap();
        let text = response.body_mut().read_to_string().unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn outcome_of(
        binding: &SendBinding,
        verdict: &Value,
    ) -> Result<
        cadence_agent::platform::agenticos_external::publish::LedgerOutcome,
        cadence_agent::platform::agenticos_external::publish::Refusal,
    > {
        use cadence_agent::platform::agenticos_external::publish::{
            LedgerOutcome, PublishState, Refusal,
        };
        if verdict["verdict"] != "ok" {
            return Err(Refusal::new(
                Refusal::code_for(verdict["code"].as_str().unwrap_or("")),
                "fake door refused dispatch",
            ));
        }
        // Strict echo: every binding field must arrive from upstream.
        // Nothing is ever filled from the request — a missing echo
        // refuses instead of masking a broken read-back.
        let destination_id = verdict["destination_id"].as_str().ok_or_else(|| {
            Refusal::new(
                "bad_destination",
                "fake door response omits destination echo",
            )
        })?;
        let caption_digest = verdict["caption_digest"].as_str().ok_or_else(|| {
            Refusal::new(
                "bad_caption_digest",
                "fake door response omits caption echo",
            )
        })?;
        let image_digest = verdict.get("image_digest").and_then(|digest| {
            if digest.is_null() {
                None
            } else {
                digest.as_str().map(str::to_owned)
            }
        });
        if image_digest.as_deref() != binding.image_digest.as_deref() {
            return Err(Refusal::new(
                "bad_image_digest",
                "fake door response image differs from the dispatched binding",
            ));
        }
        Ok(LedgerOutcome {
            state: match verdict["state"].as_str().unwrap_or("") {
                "posted" => PublishState::Posted,
                "processing" => PublishState::Processing,
                "refused" => PublishState::Refused,
                _ => PublishState::ReconnectNeeded,
            },
            permalink: verdict["permalink"].as_str().map(str::to_owned),
            destination_id: destination_id.to_owned(),
            caption_digest: caption_digest.to_owned(),
            image_digest,
            provider_payload: verdict["provider_payload"].as_str().map(str::to_owned),
            provider_ids: verdict["provider_ids"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            repeated: verdict["repeated"].as_bool().unwrap_or(false),
        })
    }
}

impl cadence_agent::platform::agenticos_external::publish::PublishSender for HttpSender {
    fn execute(
        &self,
        binding: &SendBinding,
    ) -> Result<
        cadence_agent::platform::agenticos_external::publish::LedgerOutcome,
        cadence_agent::platform::agenticos_external::publish::Refusal,
    > {
        if let Some(code) = self.execute_refusals.lock().unwrap().get(&binding.key) {
            return Err(
                cadence_agent::platform::agenticos_external::publish::Refusal::new(
                    *code,
                    "scripted: nothing left this client",
                ),
            );
        }
        let behavior = self
            .behaviors
            .lock()
            .unwrap()
            .get(&binding.key)
            .copied()
            .unwrap_or(FakeProviderBehavior::Post);
        let mut wire = Self::wire(binding);
        wire["behavior"] = json!(match behavior {
            FakeProviderBehavior::Post => "post",
            FakeProviderBehavior::Refuse => "refuse",
            FakeProviderBehavior::LoseResponseAfterAccept => "lose",
        });
        let verdict = self.post("/v1/device/publish/exec", &wire);
        Self::outcome_of(binding, &verdict)
    }

    /// CAD-1041: stage at the door (grant liveness + exact binding), as
    /// the production sender does, unless the key is scripted.
    fn preflight(
        &self,
        binding: &SendBinding,
    ) -> cadence_agent::platform::agenticos_external::publish::Preflight {
        use cadence_agent::platform::agenticos_external::publish::{Preflight, Refusal};
        match self.preflights.lock().unwrap().get(&binding.key).copied() {
            Some(Staging::Uncertain) => {
                return Preflight::Uncertain(Refusal::new("refused", "scripted: staging timed out"))
            }
            Some(Staging::Refused(code)) => {
                return Preflight::Refused(Refusal::new(code, "scripted staging refusal"))
            }
            None => {}
        }
        let verdict = self.post("/v1/device/publish/preflight", &Self::wire(binding));
        if verdict["verdict"] == "ok" {
            return Preflight::Approved;
        }
        Preflight::Refused(Refusal::new(
            Refusal::code_for(verdict["code"].as_str().unwrap_or("")),
            "fake door refused staging",
        ))
    }

    fn status(
        &self,
        key: &str,
    ) -> Result<
        cadence_agent::platform::agenticos_external::publish::LedgerOutcome,
        cadence_agent::platform::agenticos_external::publish::Refusal,
    > {
        if let Some(forged) = self.status_forgeries.lock().unwrap().get(key) {
            return Ok(forged.clone());
        }
        let verdict = self.post("/v1/device/publish/status", &json!({"key": key}));
        let empty = SendBinding {
            key: key.into(),
            connection_id: String::new(),
            destination_id: String::new(),
            toolkit: Toolkit::Facebook,
            caption_digest: String::new(),
            image_digest: None,
            cadence_run_id: String::new(),
            cadence_effect_id: String::new(),
            grant_id: String::new(),
        };
        Self::outcome_of(&empty, &verdict)
    }
}

fn door_binding(intent: &Value, behavior: &str) -> Value {
    let frozen = &intent["frozen"];
    // v9: the wire `connection_id` is the resolved remote AOS
    // `connectionId` persisted as `frozen["aos_connection_id"]` — the local
    // `con_*` custody id is never sent on the wire.
    json!({"key": intent["request"],
        "connection_id": frozen["aos_connection_id"], "toolkit": frozen["toolkit"],
        "destination_id": frozen["destination_id"],
        "caption_digest": frozen["caption_digest"],
        "image_digest": frozen["image_digest"],
        "cadence_run_id": frozen["run_id"],
        "cadence_effect_id": frozen["effect_id"],
        "grant_id": frozen["grant_id"],
        "behavior": behavior})
}

fn recheck_for(intent: &Value) -> Value {
    let frozen = &intent["frozen"];
    // v9: the claim recheck compares the remote AOS wire identity
    // (`aos_connection_id`) — the field the frozen approval binds. The
    // local `connection_id` stays custody, not the rechecked wire field.
    json!({"grant_id": frozen["grant_id"],
        "connection_id": frozen["connection_id"],
        "aos_connection_id": frozen["aos_connection_id"],
        "destination_id": frozen["destination_id"],
        "caption_digest": frozen["caption_digest"],
        "image_digest": frozen["image_digest"]})
}

fn epoch_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Real cross-layer setup for the browser-surface test: the board must
/// serve the actual SPA build. If `ui/dist` is absent (fresh checkout,
/// CI), build it here with the repo toolchain — never a placeholder
/// shell, never a skip. Fails loudly when the toolchain is missing.
fn ensure_spa_dist() -> std::path::PathBuf {
    let ui = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
    let dist = ui.join("dist");
    if dist.join("index.html").is_file() {
        return dist;
    }
    let status = std::process::Command::new("pnpm")
        .arg("--dir")
        .arg(&ui)
        .arg("build")
        .status()
        .expect("node/pnpm toolchain is required to build the SPA for this test");
    assert!(
        status.success() && dist.join("index.html").is_file(),
        "building the actual SPA failed; run `pnpm --dir ui install && pnpm --dir ui build`"
    );
    dist
}

fn approved_run(h: &Release, tag: &str) -> (Value, Value, String, String) {
    let context = h.context("Harbour", A, &format!("cad771-e2e-{tag}-context"));
    h.bind(&context, &format!("cad771-e2e-{tag}-binding"));
    let mut run = h.complete(&context, &format!("cad771-e2e-{tag}-run"));
    // CAD-1027: freeze proves the effect belongs to this run+artifact, so
    // every fixture schedules against a real staged app effect.
    let effect = h.stage(&run, &format!("cad771-e2e-{tag}-effect"));
    run["staged_effect_id"] = effect["effect_id"].clone();
    let bundle_digest = run["snapshot"]["bundle_digest"]
        .as_str()
        .unwrap()
        .to_owned();
    let install_id = h.install["install_id"].as_str().unwrap().to_owned();
    (context, run, bundle_digest, install_id)
}

/// A daemon with the fake dispatch sender registered: claims execute
/// daemon-side and persist provider evidence, exactly the path posted
/// reports verify against.
/// A daemon with ONLY the read-credential destinations resolver configured
/// (no publish sender). The schedule path still resolves the remote AOS
/// `connectionId` via the fake door; dispatch/sender is absent so the
/// cancel/revoke/parity assertions keep their no-sender semantics.
fn resolver_only_release(door: &FakeDoor) -> Release {
    let dest_base = format!("http://{}", door.addr);
    Release::with_options(move |opts, _| {
        opts.social_media_resolver = Some(std::sync::Arc::new(
            cadence_agent::platform::agenticos_external::media_import::MediaResolver::new(
                &dest_base,
                cadence_agent::platform::agenticos_external::publish_sender::DeviceCredential::new(
                    "cad-test-read".to_owned(),
                ),
            )
            .expect("fake destinations resolver"),
        ));
    })
}

fn e2e_release(door: &FakeDoor) -> (Release, Arc<HttpSender>) {
    let sender = Arc::new(HttpSender::new(format!("http://{}", door.addr)));
    let registered = Arc::clone(&sender);
    let dest_base = format!("http://{}", door.addr);
    let h = Release::with_options(move |opts, _| {
        opts.social_publish_sender = Some(registered);
        // CAD-979 v9: the schedule/import path resolves the remote AOS
        // `connectionId` under a read credential — here a real
        // `MediaResolver` pointed at the fake door's destinations route.
        opts.social_media_resolver = Some(std::sync::Arc::new(
            cadence_agent::platform::agenticos_external::media_import::MediaResolver::new(
                &dest_base,
                cadence_agent::platform::agenticos_external::publish_sender::DeviceCredential::new(
                    "cad-test-read".to_owned(),
                ),
            )
            .expect("fake destinations resolver"),
        ));
    });
    (h, sender)
}

fn freeze_params(
    context: &Value,
    run: &Value,
    bundle_digest: &str,
    install_id: &str,
    request: &str,
    due: i64,
) -> Value {
    json!({"request_id": request, "install_id": install_id,
        "context_id": context["id"], "run_id": run["id"],
        "artifact_id": run["artifacts"][0]["id"],
        "bundle_digest": bundle_digest,
        "slot": "publication", "effect_id": run["staged_effect_id"],
        "destination_id": DEST_FB, "toolkit": "facebook",
        // CAD-1027: one approval authorizes one intent — each request
        // carries its own approval identity.
        "grant_id": GRANT_FB, "approval_id": approval_for(request),
        "due_epoch": due, "timezone": "Asia/Hong_Kong"})
}

#[test]
fn cad771_e2e_post_now_from_approved_run_with_grant_liveness() {
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, _sender) = e2e_release(&door);
    let (context, run, bundle_digest, install_id) = approved_run(&h, "now");
    let _ = &context;

    // Freeze from the genuinely approved run: digests are derived, and the
    // reviewed binding is re-proven current.
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
                "cad771-e2e-now",
                due,
            ),
        )
        .unwrap()["intent"]
        .clone();
    assert_eq!(intent["state"], "queued");
    assert_eq!(intent["frozen"]["destination_id"], DEST_FB);
    // The frozen caption digest is the reviewed artifact's — never a
    // caller-supplied string.
    let artifact = h.artifact(&run);
    let reviewed_hex = artifact["digest"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    assert_eq!(intent["frozen"]["caption_digest"], reviewed_hex);

    // Dispatch: recheck matches frozen, claim wins, fake door posts once.
    let recheck = recheck_for(&intent);
    let claimed = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": due + 5, "recheck": recheck}),
        )
        .unwrap();
    assert_eq!(claimed["intent"]["state"], "processing");
    // The daemon executed the exact binding itself and persisted the
    // provider's evidence; the report below must replay those bytes.
    let evidence = &claimed["intent"]["upstream"];
    assert_eq!(evidence["state"], "posted");
    assert!(!evidence["permalink"].as_str().unwrap_or("").is_empty());
    let posted = h
        .daemon
        .operator_rpc(
            "social_publish_report",
            json!({"intent_id": claimed["intent"]["intent_id"], "decision": "posted",
                "receipt": {"permalink": evidence["permalink"],
                    "destination_id": DEST_FB, "caption_digest": reviewed_hex, "image_digest": intent["frozen"]["image_digest"],
                    "provider_ids": evidence["provider_ids"],
                    "provider_payload": evidence["provider_payload"]}}),
        )
        .unwrap()["intent"]
        .clone();
    assert_eq!(posted["state"], "posted");
    assert!(posted["receipt"]["permalink"].is_string());
    // Re-claim finds nothing: no second send, ever.
    let idle = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": due + 5, "recheck": recheck}),
        )
        .unwrap();
    assert_eq!(idle["claimed"], false);
    assert_eq!(door.ledger.provider_calls(), 1);
}

#[test]
fn cad771_e2e_forged_matching_receipt_fails_closed_without_trusted_evidence() {
    // Field-equality is not trust: a receipt copying every frozen field
    // but fabricating permalink and payload must not post. Only the exact
    // daemon-observed evidence bytes satisfy a posted report; anything
    // else retains processing (uncertain).
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, _sender) = e2e_release(&door);
    let (context, run, bundle_digest, install_id) = approved_run(&h, "forged-receipt");
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context,
                &run,
                &bundle_digest,
                &install_id,
                "cad771-e2e-forged-receipt",
                epoch_now(),
            ),
        )
        .unwrap()["intent"]
        .clone();
    let recheck = recheck_for(&intent);
    let claimed = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": epoch_now() + 5, "recheck": recheck}),
        )
        .unwrap();
    assert_eq!(claimed["intent"]["state"], "processing");
    // Every frozen field copied exactly; permalink and payload fabricated.
    let forged = h
        .daemon
        .operator_rpc(
            "social_publish_report",
            json!({"intent_id": claimed["intent"]["intent_id"], "decision": "posted",
                "receipt": {"permalink": "https://www.instagram.com/p/FORGED/",
                    "destination_id": intent["frozen"]["destination_id"],
                    "caption_digest": intent["frozen"]["caption_digest"],
                    "image_digest": intent["frozen"]["image_digest"],
                    "provider_ids": ["provider-post-1"],
                    "provider_payload": "{\"id\":\"forged\"}"}}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        forged.contains("does not match trusted upstream evidence"),
        "{forged}"
    );
    assert_eq!(
        h.daemon
            .operator_rpc(
                "social_publish_show",
                json!({"intent_id": claimed["intent"]["intent_id"]}),
            )
            .unwrap()["intent"]["state"],
        "processing"
    );
    assert_eq!(door.ledger.provider_calls(), 1);
    // Copied evidence bytes with forged permalink and IDs still fail:
    // full receipt-to-outcome equality against daemon-observed evidence.
    let copied = h
        .daemon
        .operator_rpc(
            "social_publish_report",
            json!({"intent_id": claimed["intent"]["intent_id"], "decision": "posted",
                "receipt": {"permalink": "https://www.instagram.com/p/FORGED/",
                    "destination_id": intent["frozen"]["destination_id"],
                    "caption_digest": intent["frozen"]["caption_digest"],
                    "image_digest": intent["frozen"]["image_digest"],
                    "provider_ids": ["provider-post-9"],
                    "provider_payload": claimed["intent"]["upstream"]["provider_payload"]}}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        copied.contains("does not match trusted upstream evidence"),
        "{copied}"
    );
    // The true evidence bytes post.
    let evidence = &claimed["intent"]["upstream"];
    h.daemon
        .operator_rpc(
            "social_publish_report",
            json!({"intent_id": claimed["intent"]["intent_id"], "decision": "posted",
                "receipt": {"permalink": evidence["permalink"],
                    "destination_id": intent["frozen"]["destination_id"],
                    "caption_digest": intent["frozen"]["caption_digest"],
                    "image_digest": intent["frozen"]["image_digest"],
                    "provider_ids": evidence["provider_ids"],
                    "provider_payload": evidence["provider_payload"]}}),
        )
        .unwrap();
    assert_eq!(
        h.daemon
            .operator_rpc(
                "social_publish_show",
                json!({"intent_id": claimed["intent"]["intent_id"]}),
            )
            .unwrap()["intent"]["state"],
        "posted"
    );
}

#[test]
fn cad771_e2e_forged_freeze_inputs_fail_closed() {
    let h = Release::new();
    let (context, run, bundle_digest, install_id) = approved_run(&h, "forged");
    let base = freeze_params(
        &context,
        &run,
        &bundle_digest,
        &install_id,
        "cad771-e2e-forged",
        epoch_now(),
    );
    // Forged artifact, bundle, slot, and run each refuse.
    for (field, value) in [
        ("artifact_id", "artifact-forged"),
        ("bundle_digest", "sha256:forge"),
        ("slot", "other-slot"),
        ("run_id", "run-forged"),
    ] {
        let mut forged = base.clone();
        forged[field] = json!(value);
        assert!(
            h.daemon
                .operator_rpc("social_publish_schedule", forged)
                .is_err(),
            "{field}"
        );
    }
    // Explicit caller-frozen digests are refused at the boundary: schedule
    // takes artifact-freeze fields only, never caller strings.
    for field in ["caption_digest", "image_digest", "connection_id"] {
        let mut explicit = base.clone();
        explicit[field] = json!("attacker-string");
        let err = h
            .daemon
            .operator_rpc("social_publish_schedule", explicit)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unsupported fields"), "{field}: {err}");
    }
    // Unproven agent-shaped callers cannot reach any of the six methods:
    // the operator gate refuses before any field is read.
    for method in [
        "social_publish_schedule",
        "social_publish_cancel",
        "social_publish_show",
        "social_publish_list",
        "social_publish_claim_due",
        "social_publish_reconcile",
        "social_publish_report",
    ] {
        assert!(
            h.daemon
                .unproven_rpc(method, json!({}))
                .unwrap_err()
                .to_string()
                .contains("operator"),
            "{method}"
        );
    }
}

#[test]
fn cad771_e2e_revoked_grant_holds_for_new_decision_and_exhaustion_refuses() {
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 1);
    let (h, _sender) = e2e_release(&door);
    let (context, run, bundle_digest, install_id) = approved_run(&h, "revoke");

    // Revocation between schedule and dispatch: the door refuses liveness,
    // and a stale recheck claims-then-holds for a new human decision.
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context,
                &run,
                &bundle_digest,
                &install_id,
                "cad771-e2e-revoke-1",
                epoch_now(),
            ),
        )
        .unwrap()["intent"]
        .clone();
    door.grants.lock().unwrap().revoke(GRANT_FB);
    // Revocation between schedule and dispatch: the daemon-side exec hits
    // the liveness gate and the intent auto-reports refused with the
    // provider's verdict — no provider call, no human decision consumed.
    let refused = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": epoch_now() + 5, "recheck": recheck_for(&intent)}),
        )
        .unwrap();
    assert_eq!(refused["intent"]["state"], "refused");
    assert!(refused["intent"]["receipt"]["error"]
        .as_str()
        .unwrap_or("")
        .contains("grant_revoked"));
    assert_eq!(door.ledger.provider_calls(), 0);
    // A stale recheck (rotated destination) claims-then-holds for a new
    // human decision instead of publishing: fresh intent, same flow.
    let stale_intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context,
                &run,
                &bundle_digest,
                &install_id,
                "cad771-e2e-revoke-stale",
                epoch_now(),
            ),
        )
        .unwrap()["intent"]
        .clone();
    let mut stale = recheck_for(&stale_intent);
    stale["destination_id"] = json!("999999999999999");
    let held = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": epoch_now() + 5, "recheck": stale}),
        )
        .unwrap();
    assert_eq!(held["intent"]["state"], "held");
    assert_eq!(door.ledger.provider_calls(), 0);

    // Exhaustion: one use posts once; the second intent's exec is refused
    // and reports refused without a provider call.
    door.grants.lock().unwrap().issue(GRANT_FB, 1);
    let intent2 = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context,
                &run,
                &bundle_digest,
                &install_id,
                "cad771-e2e-exhaust-2",
                epoch_now(),
            ),
        )
        .unwrap()["intent"]
        .clone();
    let recheck2 = recheck_for(&intent2);
    let claimed2 = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": epoch_now() + 5, "recheck": recheck2}),
        )
        .unwrap();
    assert_eq!(claimed2["intent"]["state"], "processing");
    let evidence2 = &claimed2["intent"]["upstream"];
    assert_eq!(evidence2["state"], "posted");
    h.daemon
        .operator_rpc(
            "social_publish_report",
            json!({"intent_id": claimed2["intent"]["intent_id"], "decision": "posted",
                "receipt": {"permalink": evidence2["permalink"], "destination_id": DEST_FB,
                    "caption_digest": intent2["frozen"]["caption_digest"],
                    "image_digest": intent2["frozen"]["image_digest"],
                    "provider_ids": evidence2["provider_ids"],
                    "provider_payload": evidence2["provider_payload"]}}),
        )
        .unwrap();
    let intent3 = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context,
                &run,
                &bundle_digest,
                &install_id,
                "cad771-e2e-exhaust-3",
                epoch_now(),
            ),
        )
        .unwrap()["intent"]
        .clone();
    let recheck3 = recheck_for(&intent3);
    let refused3 = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": epoch_now() + 5, "recheck": recheck3}),
        )
        .unwrap();
    assert_eq!(refused3["intent"]["state"], "refused");
    assert!(refused3["intent"]["receipt"]["error"]
        .as_str()
        .unwrap_or("")
        .contains("grant_exhausted"));
    assert_eq!(door.ledger.provider_calls(), 1);
}

#[test]
fn cad771_e2e_lost_response_reconciles_without_second_send() {
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, sender) = e2e_release(&door);
    let (context, run, bundle_digest, install_id) = approved_run(&h, "lost");
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context,
                &run,
                &bundle_digest,
                &install_id,
                "cad771-e2e-lost",
                epoch_now(),
            ),
        )
        .unwrap()["intent"]
        .clone();
    // The provider will accept but the response will be lost: arm the
    // lose behavior before the claim executes daemon-side at once.
    sender.set_behavior(
        intent["request"].as_str().unwrap(),
        FakeProviderBehavior::LoseResponseAfterAccept,
    );
    let recheck = recheck_for(&intent);
    let claimed = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": epoch_now() + 5, "recheck": recheck}),
        )
        .unwrap();
    assert_eq!(claimed["intent"]["state"], "processing");
    assert_eq!(claimed["intent"]["upstream"]["state"], "processing");
    assert!(claimed["intent"]["upstream"]["provider_payload"].is_null());
    // After restart, reconcile upstream status before any retry.
    let reconciled = h
        .daemon
        .operator_rpc(
            "social_publish_reconcile",
            json!({"intent_id": claimed["intent"]["intent_id"]}),
        )
        .unwrap()["intent"]
        .clone();
    assert_eq!(reconciled["state"], "processing");
    let evidence = &reconciled["upstream"];
    assert_eq!(evidence["state"], "posted");
    // Byte-exact provider evidence: the payload travels as an opaque
    // string and parses to the recorded provider document — never a
    // re-serialized approximation, never a bare success string.
    let payload: Value =
        serde_json::from_str(evidence["provider_payload"].as_str().unwrap()).unwrap();
    assert_eq!(payload["id"], "provider-post-1");
    h.daemon
        .operator_rpc(
            "social_publish_report",
            json!({"intent_id": claimed["intent"]["intent_id"], "decision": "posted",
                "receipt": {"permalink": evidence["permalink"], "destination_id": DEST_FB,
                    "caption_digest": intent["frozen"]["caption_digest"],
                    "image_digest": intent["frozen"]["image_digest"],
                    "provider_ids": evidence["provider_ids"],
                    "provider_payload": evidence["provider_payload"]}}),
        )
        .unwrap();
    let shown = h
        .daemon
        .operator_rpc(
            "social_publish_show",
            json!({"intent_id": claimed["intent"]["intent_id"]}),
        )
        .unwrap()["intent"]
        .clone();
    assert_eq!(shown["state"], "posted");
    assert_eq!(door.ledger.provider_calls(), 1);
    assert_eq!(door.provider_calls(), 2);
}

#[test]
fn cad771_e2e_schedule_cancel_and_native_http_parity() {
    let door = FakeDoor::start();
    let h = resolver_only_release(&door);
    let (context, run, bundle_digest, install_id) = approved_run(&h, "cancel");
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    // Future-dated schedule is not yet due; operator cancellation closes it.
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context,
                &run,
                &bundle_digest,
                &install_id,
                "cad771-e2e-cancel",
                epoch_now() + 3600,
            ),
        )
        .unwrap()["intent"]
        .clone();
    let recheck = recheck_for(&intent);
    let idle = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": epoch_now(), "recheck": recheck}),
        )
        .unwrap();
    assert_eq!(idle["claimed"], false);
    let cancelled = h
        .daemon
        .operator_rpc(
            "social_publish_cancel",
            json!({"intent_id": intent["intent_id"], "install_id": install_id,
                "context_id": context["id"]}),
        )
        .unwrap()["intent"]
        .clone();
    assert_eq!(cancelled["state"], "cancelled");
    // Native-vs-HTTP parity on the fake door: the same forged binding is
    // refused with the same code through the HTTP JSON door that the native
    // gate computes.
    let mut forged = door_binding(&intent, "post");
    forged["destination_id"] = json!("999999999999999");
    // Retarget the forged binding at the IG discovery shape to prove the
    // wrong-destination verdict is destination-exact, not toolkit luck.
    forged["toolkit"] = json!("instagram");
    forged["connection_id"] = json!(CONN_IG);
    // Owner-authorized discovery returns the true destination: the forged
    // binding mismatches it through the HTTP door exactly as natively.
    door.expect_destination(DEST_IG);
    // Instagram shape needs an image digest before the destination gate;
    // the point under test is destination-exactness, so supply one.
    forged["image_digest"] =
        json!("9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08");
    let http_verdict = door.post("/v1/device/publish/preflight", &forged);
    assert_eq!(
        http_verdict,
        json!({"verdict": "refused", "code": "wrong_destination"})
    );
    let native = door.ledger.preflight(
        &SendBinding {
            key: forged["key"].as_str().unwrap().into(),
            connection_id: CONN_IG.into(),
            destination_id: "999999999999999".into(),
            toolkit: Toolkit::Instagram,
            caption_digest: forged["caption_digest"].as_str().unwrap().into(),
            image_digest: Some(
                "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into(),
            ),
            cadence_run_id: forged["cadence_run_id"].as_str().unwrap().into(),
            cadence_effect_id: forged["cadence_effect_id"].as_str().unwrap().into(),
            grant_id: GRANT_FB.into(),
        },
        &discovery(Toolkit::Instagram),
        &SendGrant {
            id: GRANT_FB.into(),
            workspace_id: "ws_harbour".into(),
            connection_id: CONN_IG.into(),
            destination_id: DEST_IG.into(),
            toolkit: Toolkit::Instagram,
            caption_digest: forged["caption_digest"].as_str().unwrap().into(),
            image_digest: None,
            cadence_approval_id: "cad_approval_01".into(),
            max_uses: 3,
            remaining_uses: 3,
            revoked: false,
            not_before_epoch: 1_700_000_000,
            expires_at_epoch: 1_800_000_000,
        },
        "ws_harbour",
        NOW,
    );
    assert_eq!(native.unwrap_err().code, "wrong_destination");
    assert_eq!(door.ledger.provider_calls(), 0);
}

#[test]
fn cad771_browser_board_serves_social_content_app_surface() {
    use std::path::Path;
    let h = Release::new();
    // The operator installs and approves the real Social Content bundle
    // through the exact RPCs the board's Apps screen drives.
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("workspace-apps/social-content");
    let install = h
        .daemon
        .operator_rpc("app_workspace_install", json!({"source": source}))
        .unwrap();
    let install_id = install["install_id"].as_str().unwrap().to_owned();
    h.daemon
        .operator_rpc(
            "app_local_install_approve",
            json!({"install_id": install_id, "digest": install["digest"]}),
        )
        .unwrap();
    // The real board HTTP stack over the live daemon state serves a
    // browser-shaped client with zero provider involvement. The shared
    // harness board carries no SPA build, so this test serves the
    // checked-in web build explicitly (the lane touches no UI file). If the
    // build is absent (a fresh checkout, CI), build the actual SPA here -
    // never a placeholder shell, never a skip.
    let dist = ensure_spa_dist();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (state, pm) = (h.daemon.state.clone(), h.root.path().join("pm"));
    let serve_dist = dist.clone();
    thread::spawn(move || {
        let _ = cadence_agent::ui::serve(
            &state,
            &pm,
            &cadence_agent::ui::ServeOpts {
                host: "127.0.0.1".to_string(),
                port,
                dist: Some(serve_dist),
                ..Default::default()
            },
        );
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            let (status, _) = common::board_get(port, "/api/health");
            if status == 200 {
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "board did not become healthy"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let (status, body) = common::board_get(port, "/");
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("<!doctype html>") || body.contains("<html"),
        "board serves the SPA shell"
    );
    // The served bundle is the real build and carries the workspace-app
    // surface the Social Content install renders through: the
    // `/app-installations/` route marker is a network contract the
    // minifier cannot rename.
    let bundle = std::fs::read_dir(dist.join("assets"))
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "js"))
        .map(|path| std::fs::read_to_string(path).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        bundle.contains("/app-installations/"),
        "built bundle carries the workspace-app routes"
    );
    // Client-side app routes serve the same shell (deep links work),
    // including the workspace-app screen for this exact install.
    let (status, _) = common::board_get(port, "/apps");
    assert_eq!(status, 200);
    let (status, _) = common::board_get(port, &format!("/app-installations/{install_id}"));
    assert_eq!(status, 200);
    // The data the Apps screen renders is present: the approved
    // Social Content installation with its publication workflows.
    let listed = h
        .daemon
        .operator_rpc("app_workspace_list", json!({}))
        .unwrap();
    let apps = listed.as_array().unwrap();
    let app = apps
        .iter()
        .find(|app| app["install_id"] == install_id)
        .expect("installed app is listed");
    assert_eq!(app["name"], "social-content");
    assert_eq!(app["approved"], true);
    let workflows: Vec<&str> = app["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|workflow| workflow["name"].as_str())
        .collect();
    assert!(
        workflows.contains(&"instagram") && workflows.contains(&"facebook"),
        "{workflows:?}"
    );
    // Real-browser proof where a browser exists (CI runners ship one):
    // the bundle executes against this board and renders client-side
    // chrome ("Sign in" appears only after JS runs — the static shell
    // carries no text). Where no browser exists the HTTP proofs above
    // stand; nothing is asserted from a placeholder.
    if let Some(chrome) = find_chrome() {
        use std::os::unix::process::CommandExt;
        let dom_path = h.root.path().join("cad771-browser-dom.html");
        let error_path = h.root.path().join("cad771-browser-stderr.txt");
        let mut browser = std::process::Command::new(chrome)
            .args([
                "--headless",
                "--no-sandbox",
                "--disable-gpu",
                "--disable-dev-shm-usage",
                "--timeout=20000",
                "--dump-dom",
                &format!("http://127.0.0.1:{port}/"),
            ])
            .stdout(std::fs::File::create(&dom_path).unwrap())
            .stderr(std::fs::File::create(&error_path).unwrap())
            .process_group(0)
            .spawn()
            .expect("headless browser failed to start");
        let browser_deadline = std::time::Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = browser.try_wait().expect("headless browser wait failed") {
                break status;
            }
            if std::time::Instant::now() >= browser_deadline {
                // The browser's children inherit this dedicated process group.
                // Reap the whole tree before failing, so one hung render cannot
                // strand the test worker or a later run.
                unsafe { libc::killpg(browser.id() as i32, libc::SIGKILL) };
                let _ = browser.wait();
                panic!("headless browser timed out after 30 seconds");
            }
            thread::sleep(Duration::from_millis(50));
        };
        assert!(
            status.success(),
            "headless browser exited {status}: {}",
            std::fs::read_to_string(&error_path).unwrap_or_default()
        );
        let dom = std::fs::read_to_string(&dom_path).expect("headless browser DOM missing");
        // Client-rendered board chrome: nav labels and the read-only
        // session chip appear only after the bundle executes against
        // this board (the static shell carries no text).
        for marker in ["Projects", "Agents", "Read-only"] {
            assert!(
                dom.contains(marker),
                "real browser renders the board chrome"
            );
        }
    }
}

/// A real browser binary when one is installed; none is ever synthesized.
fn find_chrome() -> Option<String> {
    ["google-chrome", "chromium", "chromium-browser"]
        .into_iter()
        .find(|binary| {
            std::process::Command::new(binary)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .map(str::to_owned)
}

#[test]
fn cad771_e2e_revoked_binding_holds_despite_matching_recheck() {
    // Daemon-side re-proof: the operator recheck still matches frozen, but
    // the approved material is stale (binding revoked after schedule), so
    // dispatch holds instead of trusting the stale attestation.
    let door = FakeDoor::start();
    let h = resolver_only_release(&door);
    let (context, run, bundle_digest, install_id) = approved_run(&h, "stale");
    let binding = h.bind(&context, "cad771-e2e-stale-binding");
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context,
                &run,
                &bundle_digest,
                &install_id,
                "cad771-e2e-stale",
                epoch_now(),
            ),
        )
        .unwrap()["intent"]
        .clone();
    h.daemon
        .operator_rpc(
            "app_binding_revoke",
            json!({"install_id": install_id, "binding_id": binding["id"],
                "expected_revision": binding["revision"]}),
        )
        .unwrap();
    let recheck = recheck_for(&intent);
    let held = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": epoch_now() + 5, "recheck": recheck}),
        )
        .unwrap();
    assert_eq!(held["intent"]["state"], "held");
    assert_eq!(
        held["intent"]["receipt"]["reason"],
        "approved material changed since freeze"
    );
    assert_eq!(door.ledger.provider_calls(), 0);
}

#[test]
fn cad771_e2e_missing_upstream_echo_fails_closed_never_filled() {
    // A provider response that omits the binding echo must refuse, never
    // fill evidence from the request: missing upstream fields fail closed.
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, _sender) = e2e_release(&door);
    let (context, run, bundle_digest, install_id) = approved_run(&h, "noecho");
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context,
                &run,
                &bundle_digest,
                &install_id,
                "cad771-e2e-noecho",
                epoch_now(),
            ),
        )
        .unwrap()["intent"]
        .clone();
    door.omit_binding_echo(true);
    let refused = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": epoch_now() + 5, "recheck": recheck_for(&intent)}),
        )
        .unwrap();
    assert_eq!(refused["intent"]["state"], "refused");
    assert!(refused["intent"]["receipt"]["error"]
        .as_str()
        .unwrap_or("")
        .contains("bad_destination"));
    // Nothing was filled from the request: no upstream evidence recorded.
    assert!(refused["intent"]["upstream"].is_null());
}

#[test]
fn cad771_e2e_cross_key_evidence_confusion_fails_closed() {
    // Two intents, two provider evidences: reporting A with B's payload —
    // even alongside A's own binding fields — refuses and retains
    // processing. Evidence is key-bound, never interchangeable.
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 4);
    let (h, _sender) = e2e_release(&door);
    let (context_a, run_a, bundle_a, install_a) = approved_run(&h, "xkey-a");
    let (context_b, run_b, bundle_b, install_b) = approved_run(&h, "xkey-b");
    // Staggered dues make dispatch order deterministic: A then B.
    let due_a = epoch_now();
    let due_b = due_a + 3600;
    let intent_a = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context_a,
                &run_a,
                &bundle_a,
                &install_a,
                "cad771-e2e-xkey-a",
                due_a,
            ),
        )
        .unwrap()["intent"]
        .clone();
    let intent_b = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context_b,
                &run_b,
                &bundle_b,
                &install_b,
                "cad771-e2e-xkey-b",
                due_b,
            ),
        )
        .unwrap()["intent"]
        .clone();
    for (intent, at) in [(&intent_a, due_a + 5), (&intent_b, due_b + 5)] {
        let claimed = h
            .daemon
            .operator_rpc(
                "social_publish_claim_due",
                json!({"now_epoch": at, "recheck": recheck_for(intent)}),
            )
            .unwrap();
        assert_eq!(claimed["intent"]["intent_id"], intent["intent_id"]);
        assert_eq!(claimed["intent"]["state"], "processing");
    }
    // A's binding fields with B's evidence bytes: refused, A retained.
    let confused = h
        .daemon
        .operator_rpc(
            "social_publish_report",
            json!({"intent_id": intent_a["intent_id"], "decision": "posted",
                "receipt": {"permalink": intent_b["frozen"]["destination_id"],
                    "destination_id": intent_a["frozen"]["destination_id"],
                    "caption_digest": intent_a["frozen"]["caption_digest"],
                    "image_digest": intent_a["frozen"]["image_digest"],
                    "provider_ids": ["provider-post-1"],
                    "provider_payload": "copied-bytes"}}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        confused.contains("does not match trusted upstream evidence"),
        "{confused}"
    );
    assert_eq!(
        h.daemon
            .operator_rpc(
                "social_publish_show",
                json!({"intent_id": intent_a["intent_id"]}),
            )
            .unwrap()["intent"]["state"],
        "processing"
    );
    assert_eq!(door.ledger.provider_calls(), 2);
}

#[test]
fn cad771_e2e_corrupt_status_binding_fails_closed_at_claim() {
    // RPC/HTTP level: a status/exec reply for the same key carrying a
    // foreign destination/caption must fail the dispatch claim itself —
    // nothing persists, processing is retained, no silent fill.
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, _sender) = e2e_release(&door);
    let (context, run, bundle_digest, install_id) = approved_run(&h, "corrupt");
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context,
                &run,
                &bundle_digest,
                &install_id,
                "cad771-e2e-corrupt",
                epoch_now(),
            ),
        )
        .unwrap()["intent"]
        .clone();
    door.corrupt_binding_echo(true);
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": epoch_now() + 5, "recheck": recheck_for(&intent)}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not match the frozen intent"), "{err}");
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
}

#[test]
fn cad771_e2e_hostile_status_binding_fails_closed_at_reconcile() {
    // Same key, foreign binding at status time: the hostile outcome is
    // visible on the HTTP wire, but the daemon reconcile refuses to
    // persist it — processing retained, honest evidence intact.
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, _sender) = e2e_release(&door);
    let (context, run, bundle_digest, install_id) = approved_run(&h, "hostile");
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context,
                &run,
                &bundle_digest,
                &install_id,
                "cad771-e2e-hostile",
                epoch_now(),
            ),
        )
        .unwrap()["intent"]
        .clone();
    let key = intent["request"].as_str().unwrap().to_owned();
    let recheck = recheck_for(&intent);
    let claimed = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": epoch_now() + 5, "recheck": recheck}),
        )
        .unwrap();
    assert_eq!(claimed["intent"]["state"], "processing");
    // Hostile provider: same stable key, B destination and digest.
    door.forge_status(
        &key,
        json!({"verdict": "ok", "state": "posted",
            "permalink": "https://www.instagram.com/p/HOSTILE/",
            "provider_ids": ["provider-post-9"],
            "provider_payload": "{\"id\":\"hostile\"}",
            "destination_id": "222222222222222",
            "caption_digest": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "image_digest": null,
            "repeated": false}),
    );
    // HTTP peer shows the hostile bytes on the wire.
    let raw = door.post("/v1/device/publish/status", &json!({"key": key}));
    assert_eq!(raw["destination_id"], "222222222222222");
    // Daemon reconcile refuses to persist them.
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_reconcile",
            json!({"intent_id": intent["intent_id"]}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not match the frozen intent"), "{err}");
    let shown = h
        .daemon
        .operator_rpc(
            "social_publish_show",
            json!({"intent_id": intent["intent_id"]}),
        )
        .unwrap()["intent"]
        .clone();
    assert_eq!(shown["state"], "processing");
    assert_eq!(shown["upstream"]["destination_id"], DEST_FB);
    assert_eq!(door.ledger.provider_calls(), 1);
}

#[test]
fn cad771_e2e_sender_forged_status_fails_closed_at_reconcile() {
    // Second injection point beside the door hook: the sender itself
    // returns a foreign binding for A's key. The HTTP peer still shows
    // honest bytes while the daemon refuses to persist the forgery —
    // processing retained, honest evidence intact, nothing recorded.
    use cadence_agent::platform::agenticos_external::publish::{LedgerOutcome, PublishState};
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, sender) = e2e_release(&door);
    let (context, run, bundle_digest, install_id) = approved_run(&h, "senderforge");
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context,
                &run,
                &bundle_digest,
                &install_id,
                "cad771-e2e-senderforge",
                epoch_now(),
            ),
        )
        .unwrap()["intent"]
        .clone();
    let key = intent["request"].as_str().unwrap().to_owned();
    let claimed = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": epoch_now() + 5, "recheck": recheck_for(&intent)}),
        )
        .unwrap();
    assert_eq!(claimed["intent"]["state"], "processing");
    sender.forge_status(
        &key,
        LedgerOutcome {
            state: PublishState::Posted,
            permalink: Some("https://www.instagram.com/p/SENDERFORGED/".into()),
            destination_id: "333333333333333".into(),
            caption_digest: "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                .into(),
            image_digest: None,
            provider_payload: Some("{\"id\":\"sender-forged\"}".into()),
            provider_ids: vec!["provider-post-9".into()],
            repeated: false,
        },
    );
    // HTTP peer shows honest bytes; only the sender-observed path is hostile.
    let raw = door.post("/v1/device/publish/status", &json!({"key": key}));
    assert_eq!(raw["destination_id"], DEST_FB);
    // Daemon reconcile refuses the forgery.
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_reconcile",
            json!({"intent_id": intent["intent_id"]}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not match the frozen intent"), "{err}");
    let shown = h
        .daemon
        .operator_rpc(
            "social_publish_show",
            json!({"intent_id": intent["intent_id"]}),
        )
        .unwrap()["intent"]
        .clone();
    assert_eq!(shown["state"], "processing");
    assert_eq!(shown["upstream"]["destination_id"], DEST_FB);
    assert_eq!(door.ledger.provider_calls(), 1);
}

#[test]
fn cad771_e2e_hostile_reconcile_with_no_prior_evidence_stays_null() {
    // Null-upstream variant: the claim itself records nothing (corrupt
    // exec refused at note), then a forged status for the same key must
    // still refuse at reconcile — processing retained, upstream null.
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, _sender) = e2e_release(&door);
    let (context, run, bundle_digest, install_id) = approved_run(&h, "nullup");
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &context,
                &run,
                &bundle_digest,
                &install_id,
                "cad771-e2e-nullup",
                epoch_now(),
            ),
        )
        .unwrap()["intent"]
        .clone();
    let key = intent["request"].as_str().unwrap().to_owned();
    // Corrupt exec: claim records nothing, processing with null upstream.
    door.corrupt_binding_echo(true);
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": epoch_now() + 5, "recheck": recheck_for(&intent)}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not match the frozen intent"), "{err}");
    door.corrupt_binding_echo(false);
    // Hostile status for the same key: reconcile refuses, null retained.
    door.forge_status(
        &key,
        json!({"verdict": "ok", "state": "posted",
            "permalink": "https://www.instagram.com/p/NULLUP/",
            "provider_ids": ["provider-post-9"],
            "provider_payload": "{\"id\":\"nullup\"}",
            "destination_id": "444444444444444",
            "caption_digest": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            "image_digest": null,
            "repeated": false}),
    );
    let raw = door.post("/v1/device/publish/status", &json!({"key": key}));
    assert_eq!(raw["destination_id"], "444444444444444");
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_reconcile",
            json!({"intent_id": intent["intent_id"]}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not match the frozen intent"), "{err}");
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
}

/// An in-process board on the daemon's own state dir, for HTTP parity.
struct Board {
    port: u16,
    _lease: common::PortLease,
    stop: Arc<std::sync::atomic::AtomicBool>,
    join: Option<thread::JoinHandle<cadence_agent::Result<()>>>,
    _pm: tempfile::TempDir,
}
impl Board {
    fn serve(h: &Release) -> Self {
        let lease = common::test_port();
        let port = lease.port;
        let (state, pm) = (h.daemon.state.clone(), tempfile::tempdir().unwrap());
        let pm_dir = pm.path().to_path_buf();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let bstop = stop.clone();
        let join = thread::spawn(move || {
            cadence_agent::ui::serve(
                &state,
                &pm_dir,
                &cadence_agent::ui::ServeOpts {
                    host: "127.0.0.1".into(),
                    port,
                    stop: Some(bstop),
                    startup: Some(tx),
                    test_seam: true,
                    ..Default::default()
                },
            )
        });
        rx.recv_timeout(Duration::from_secs(10))
            .expect("board up")
            .expect("board started");
        Self {
            port,
            _lease: lease,
            stop,
            join: Some(join),
            _pm: pm,
        }
    }
    /// Operator POST; returns `(status, body)`.
    fn post(&self, h: &Release, path: &str, body: &Value) -> (u16, String) {
        let session =
            common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &h.daemon.state, self.port);
        let (code, _, text) =
            common::op::raw(self.port, &session.request("POST", path, &body.to_string()));
        (code, text)
    }
}
impl Drop for Board {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn intent_count(h: &Release, install: &str) -> usize {
    h.daemon
        .operator_rpc("social_publish_list", json!({"install_id": install}))
        .unwrap()["intents"]
        .as_array()
        .unwrap()
        .len()
}

/// CAD-1027 adversarial (written before the guard): one operator approval
/// authorizes exactly one intent. Concurrent double submits that share the
/// approval but carry fresh request ids yield one intent; a replay of the
/// approval refuses through the RPC and the HTTP relay alike; a forged
/// (non-`dpq_`) grant refuses through both doors; an agent caller and a
/// detached unproven peer never reach schedule.
#[test]
fn cad1027_one_approval_one_intent_rpc_and_http() {
    let door = FakeDoor::start();
    let h = resolver_only_release(&door);
    let (context, run, bundle, install) = approved_run(&h, "apv");
    let body = |request: &str| {
        let mut b = freeze_params(
            &context,
            &run,
            &bundle,
            &install,
            request,
            epoch_now() + 3600,
        );
        b["approval_id"] = json!(approval_for("cad1027-double"));
        b
    };
    // Four concurrent submits of one approval with distinct request ids.
    let wins = thread::scope(|scope| {
        let calls: Vec<_> = (0..4)
            .map(|i| {
                let (state, params) = (h.daemon.state.clone(), body(&format!("cad1027-dbl-{i}")));
                scope.spawn(move || {
                    cadence_agent::test_seam::scoped(
                        cadence_agent::test_seam::Asserted::Operator,
                        || cadence_agent::client::rpc(&state, "social_publish_schedule", params),
                    )
                })
            })
            .collect();
        calls
            .into_iter()
            .map(|call| call.join().unwrap())
            .filter(Result::is_ok)
            .count()
    });
    assert_eq!(wins, 1, "a double submit must produce exactly one intent");
    assert_eq!(intent_count(&h, &install), 1);
    // RPC replay of the consumed approval under a fresh request refuses.
    let err = h
        .daemon
        .operator_rpc("social_publish_schedule", body("cad1027-replay-rpc"))
        .unwrap_err();
    assert!(err.to_string().contains("approval_replay"), "{err}");
    // An agent caller and an unproven (detached) peer never reach schedule.
    for err in [
        h.daemon
            .agent_rpc("worker-0", "social_publish_schedule", body("cad1027-agent"))
            .unwrap_err(),
        h.daemon
            .unproven_rpc("social_publish_schedule", body("cad1027-unproven"))
            .unwrap_err(),
    ] {
        assert!(err.to_string().contains("operator"), "{err}");
    }
    // The HTTP relay is at least as strict: replay and forged grant refuse.
    let board = Board::serve(&h);
    let (code, text) = board.post(&h, "/api/social-publishes", &body("cad1027-replay-http"));
    // The relay maps the store's "already" refusal to 409 Conflict.
    assert_eq!(code, 409, "HTTP replay: {text}");
    assert!(text.contains("approval_replay"), "{text}");
    for grant in ["grant-a", "dpq_short", "DPQ_synthetic_grant_fb"] {
        let mut forged = freeze_params(
            &context,
            &run,
            &bundle,
            &install,
            "cad1027-forged",
            epoch_now() + 3600,
        );
        forged["grant_id"] = json!(grant);
        let (code, text) = board.post(&h, "/api/social-publishes", &forged);
        assert_eq!(code, 400, "HTTP forged grant {grant} accepted: {text}");
        let err = h
            .daemon
            .operator_rpc("social_publish_schedule", forged)
            .unwrap_err();
        assert!(
            err.to_string().contains("grant is invalid"),
            "{grant}: {err}"
        );
    }
    assert_eq!(
        intent_count(&h, &install),
        1,
        "a refused call stored an intent"
    );
}

/// CAD-1027 (d) adversarial: freeze proves the effect belongs to this
/// run+artifact and that the request's install/context are the run's own.
/// A cross-context effect, a forged effect, another context, a dropped
/// context and another install each refuse through the RPC and the HTTP
/// relay, storing nothing; the exact scope then freezes.
#[test]
fn cad1027_freeze_refuses_foreign_effect_and_scope() {
    let door = FakeDoor::start();
    let h = resolver_only_release(&door);
    let (ctx_a, run_a, bundle_a, install) = approved_run(&h, "scope-a");
    let (ctx_b, run_b, _bundle_b, _) = approved_run(&h, "scope-b");
    let base = freeze_params(
        &ctx_a,
        &run_a,
        &bundle_a,
        &install,
        "cad1027-scope",
        epoch_now() + 3600,
    );
    let board = Board::serve(&h);
    let mut cases: Vec<(&str, &str, Value)> = Vec::new();
    let mut forged = base.clone();
    forged["effect_id"] = run_b["staged_effect_id"].clone();
    cases.push(("cross-context effect", "bad_effect", forged));
    // Another approved run in the SAME context: its effect matches the
    // scope, so only the run/artifact comparison refuses it.
    let sibling = h.complete(&ctx_a, "cad1027-scope-sibling-run");
    let sibling_effect = h.stage(&sibling, "cad1027-scope-sibling-effect");
    let mut forged = base.clone();
    forged["effect_id"] = sibling_effect["effect_id"].clone();
    cases.push(("same-scope other-run effect", "bad_effect", forged));
    // Same run, different artifact: the effect's authorization names
    // another artifact of the run (rewritten in place, as a two-step run
    // would carry), so only the artifact comparison refuses it.
    let mut rewritten = h.complete(&ctx_a, "cad1027-scope-artifact-run");
    let effect = h.stage(&rewritten, "cad1027-scope-artifact-effect");
    rewritten["staged_effect_id"] = effect["effect_id"].clone();
    rusqlite::Connection::open(h.daemon.state.join("cadence.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE app_effect_authorizations SET artifact_id='artifact-cad1027-other' WHERE effect_id=?",
            [effect["effect_id"].as_str().unwrap()],
        )
        .unwrap();
    let forged = freeze_params(
        &ctx_a,
        &rewritten,
        &bundle_a,
        &install,
        "cad1027-scope-artifact",
        epoch_now() + 3600,
    );
    cases.push(("same-run other-artifact effect", "bad_effect", forged));
    // A declined effect is no longer live authority: it never backs a post.
    let mut declined = h.complete(&ctx_a, "cad1027-scope-declined-run");
    let effect = h.stage(&declined, "cad1027-scope-declined-effect");
    h.daemon
        .operator_rpc(
            "app_effect_decide",
            json!({"effect_id": effect["effect_id"], "digest": effect["digest"], "decision": "decline"}),
        )
        .unwrap();
    declined["staged_effect_id"] = effect["effect_id"].clone();
    let forged = freeze_params(
        &ctx_a,
        &declined,
        &bundle_a,
        &install,
        "cad1027-scope-declined",
        epoch_now() + 3600,
    );
    cases.push(("declined effect", "bad_effect", forged));
    let mut forged = base.clone();
    forged["effect_id"] = json!("fx-forged-cad1027");
    cases.push(("forged effect", "bad_effect", forged));
    let mut forged = base.clone();
    forged["context_id"] = ctx_b["id"].clone();
    cases.push(("other context", "grant_binding_mismatch", forged));
    let mut forged = base.clone();
    forged.as_object_mut().unwrap().remove("context_id");
    cases.push(("dropped context", "grant_binding_mismatch", forged));
    let mut forged = base.clone();
    forged["install_id"] = json!("install-forged");
    cases.push(("other install", "grant_binding_mismatch", forged));
    // Each case names the guard that must refuse it, so removing either
    // guard alone fails here (the effect guard would otherwise also catch
    // the scope cases).
    for (case, refusal, params) in cases {
        let err = h
            .daemon
            .operator_rpc("social_publish_schedule", params.clone())
            .expect_err(case)
            .to_string();
        assert!(err.contains(refusal), "RPC {case}: {err}");
        let (code, text) = board.post(&h, "/api/social-publishes", &params);
        assert!(
            (400..500).contains(&code),
            "HTTP froze a {case}: {code} {text}"
        );
        assert!(text.contains(refusal), "HTTP {case}: {text}");
    }
    assert_eq!(
        intent_count(&h, &install),
        0,
        "a refused freeze stored an intent"
    );
    assert_eq!(intent_count(&h, "install-forged"), 0);
    let intent = h
        .daemon
        .operator_rpc("social_publish_schedule", base)
        .unwrap();
    assert_eq!(intent["intent"]["state"], "queued");
}

/// CAD-1027 (d) adversarial: cancel is scoped by install and context. A
/// cancel naming another install, another context or no context refuses
/// through the RPC and the HTTP relay and leaves the intent queued; only
/// the intent's own scope cancels it.
#[test]
fn cad1027_cancel_is_scoped_to_install_and_context() {
    let door = FakeDoor::start();
    let h = resolver_only_release(&door);
    let (ctx_a, run_a, bundle_a, install) = approved_run(&h, "cscope-a");
    let (ctx_b, _run_b, _bundle_b, _) = approved_run(&h, "cscope-b");
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(
                &ctx_a,
                &run_a,
                &bundle_a,
                &install,
                "cad1027-cscope",
                epoch_now() + 3600,
            ),
        )
        .unwrap()["intent"]
        .clone();
    let id = intent["intent_id"].as_str().unwrap();
    let board = Board::serve(&h);
    let path = format!("/api/social-publishes/{id}/cancel");
    for scope in [
        json!({"install_id": "install-forged", "context_id": ctx_a["id"]}),
        json!({"install_id": install, "context_id": ctx_b["id"]}),
        json!({"install_id": install}),
    ] {
        let mut params = scope.clone();
        params["intent_id"] = json!(id);
        let err = h
            .daemon
            .operator_rpc("social_publish_cancel", params)
            .unwrap_err();
        assert!(
            err.to_string().contains("in this install and context"),
            "{scope}: {err}"
        );
        let (code, text) = board.post(&h, &path, &scope);
        assert!(
            (400..500).contains(&code),
            "HTTP cancelled outside scope {scope}: {code} {text}"
        );
    }
    let shown = h
        .daemon
        .operator_rpc("social_publish_show", json!({"intent_id": id}))
        .unwrap();
    assert_eq!(
        shown["intent"]["state"], "queued",
        "an out-of-scope cancel changed the intent"
    );
    let (code, text) = board.post(
        &h,
        &path,
        &json!({"install_id": install, "context_id": ctx_a["id"]}),
    );
    assert_eq!(code, 200, "in-scope cancel refused: {text}");
    assert!(text.contains("\"cancelled\""), "{text}");
}

/// CAD-1027: a daemon-shaped approval id (`apv-` + 32 lowercase hex),
/// distinct per seed — one approval authorizes one intent.
fn approval_for(seed: &str) -> String {
    use sha2::Digest as _;
    let hex = format!("{:x}", sha2::Sha256::digest(seed.as_bytes()));
    format!("apv-{}", &hex[..32])
}

/// CAD-1027 adversarial: the daemon accepts only the minted approval shape
/// (`apv-` + 32 lowercase hex), so a non-UI operator client cannot choose a
/// guessable approval id. Every other shape refuses `bad_approval` through
/// the RPC and the HTTP relay and stores nothing.
#[test]
fn cad1027_daemon_refuses_unminted_approval_shape() {
    let door = FakeDoor::start();
    let h = resolver_only_release(&door);
    let (context, run, bundle, install) = approved_run(&h, "apvshape");
    let board = Board::serve(&h);
    let hex = "0123456789abcdef0123456789abcdef";
    for (i, approval) in [
        "op-a".to_owned(),
        "cad_approval_01".to_owned(),
        format!("apv-{}", hex.to_uppercase()),
        format!("apv-{}", &hex[..31]),
        format!("apv-{hex}0"),
        format!("apv-{}g", &hex[..31]),
        format!("APV-{hex}"),
        format!(" apv-{hex}"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut params = freeze_params(
            &context,
            &run,
            &bundle,
            &install,
            &format!("cad1027-shape-{i}"),
            epoch_now() + 3600,
        );
        params["approval_id"] = json!(approval);
        let err = h
            .daemon
            .operator_rpc("social_publish_schedule", params.clone())
            .expect_err(&approval)
            .to_string();
        assert!(err.contains("bad_approval"), "RPC {approval:?}: {err}");
        let (code, text) = board.post(&h, "/api/social-publishes", &params);
        assert!(
            (400..500).contains(&code),
            "HTTP accepted {approval:?}: {code} {text}"
        );
        assert!(text.contains("bad_approval"), "HTTP {approval:?}: {text}");
    }
    assert_eq!(
        intent_count(&h, &install),
        0,
        "an unminted approval stored an intent"
    );
    let mut ok = freeze_params(
        &context,
        &run,
        &bundle,
        &install,
        "cad1027-shape-ok",
        epoch_now() + 3600,
    );
    ok["approval_id"] = json!(format!("apv-{hex}"));
    assert_eq!(
        h.daemon
            .operator_rpc("social_publish_schedule", ok)
            .unwrap()["intent"]["state"],
        "queued"
    );
}

/// CAD-1041 adversarial (written before the guard): the operator's
/// send-now claims ONE named queued intent by identity, stages it,
/// dispatches once through the shared path, then reconciles via status
/// — never a re-send, never another intent, and only an operator may
/// invoke it. Each refusal keeps the provider-call count unchanged.
///
/// Schedule one queued intent and return `(intent_id, request_key)`.
#[allow(clippy::too_many_arguments)]
fn send_now_fixture(
    h: &Release,
    context: &Value,
    run: &Value,
    bundle: &str,
    install: &str,
    request: &str,
    due: i64,
) -> (String, String) {
    let intent = h
        .daemon
        .operator_rpc(
            "social_publish_schedule",
            freeze_params(context, run, bundle, install, request, due),
        )
        .unwrap()["intent"]
        .clone();
    assert_eq!(intent["state"], "queued");
    (
        intent["intent_id"].as_str().unwrap().to_owned(),
        intent["request"].as_str().unwrap().to_owned(),
    )
}

#[test]
fn cad1041_send_now_posts_the_named_intent_once() {
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, _s) = e2e_release(&door);
    let (context, run, bundle, install) = approved_run(&h, "sn");
    let (id, _key) = send_now_fixture(
        &h,
        &context,
        &run,
        &bundle,
        &install,
        "cad1041-send-ok",
        epoch_now() + 3600, // future due — the click is the trigger
    );
    let out = h
        .daemon
        .operator_rpc("social_publish_send_now", json!({"intent_id": id}))
        .unwrap()["intent"]
        .clone();
    assert_eq!(out["state"], "posted", "{out}");
    // One staging request at the door, then the single exec POST.
    assert_eq!(door.stages(), 1, "send-now stages once before the claim");
    assert_eq!(
        *door.calls.lock().unwrap(),
        1,
        "the single exec POST — exactly once"
    );
    // A second send-now on a posted row refuses without a provider call.
    let err = h
        .daemon
        .operator_rpc("social_publish_send_now", json!({"intent_id": id}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("queued"), "{err}");
    assert_eq!(*door.calls.lock().unwrap(), 1);
}

#[test]
fn cad1041_agent_and_detached_child_never_send() {
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, _s) = e2e_release(&door);
    let (context, run, bundle, install) = approved_run(&h, "sngate");
    let (id, _key) = send_now_fixture(
        &h,
        &context,
        &run,
        &bundle,
        &install,
        "cad1041-gate",
        epoch_now(),
    );
    // Agent caller: refused.
    let err = h
        .daemon
        .agent_rpc(
            "cc13-pw",
            "social_publish_send_now",
            json!({"intent_id": id}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("authority") || err.contains("operator"),
        "{err}"
    );
    // Detached setsid child (unproven peer): refused.
    let err = h
        .daemon
        .unproven_rpc("social_publish_send_now", json!({"intent_id": id}))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("authority") || err.contains("operator"),
        "{err}"
    );
    assert_eq!(*door.calls.lock().unwrap(), 0);
    // The row is still queued — refusals never mutate.
    let shown = h
        .daemon
        .operator_rpc("social_publish_show", json!({"intent_id": id}))
        .unwrap();
    assert_eq!(shown["intent"]["state"], "queued");
}

#[test]
fn cad1041_forged_id_never_claims_a_different_intent() {
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, _s) = e2e_release(&door);
    let (context, run, bundle, install) = approved_run(&h, "snid");
    let (id_a, key_a) = send_now_fixture(
        &h,
        &context,
        &run,
        &bundle,
        &install,
        "cad1041-id-a",
        epoch_now(),
    );
    let (id_b, _key_b) = send_now_fixture(
        &h,
        &context,
        &run,
        &bundle,
        &install,
        "cad1041-id-b",
        epoch_now(),
    );
    // A forged id (valid shape, never scheduled) claims nothing.
    let err = h
        .daemon
        .operator_rpc(
            "social_publish_send_now",
            json!({"intent_id": "sp-forged-never-scheduled"}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not exist"), "{err}");
    // Send A by name: A posts on its own request key; B stays queued.
    let out = h
        .daemon
        .operator_rpc("social_publish_send_now", json!({"intent_id": id_a}))
        .unwrap()["intent"]
        .clone();
    assert_eq!(out["state"], "posted");
    assert_eq!(out["request"].as_str().unwrap(), key_a);
    let shown_b = h
        .daemon
        .operator_rpc("social_publish_show", json!({"intent_id": id_b}))
        .unwrap();
    assert_eq!(shown_b["intent"]["state"], "queued", "B must not move");
    // One provider call, keyed to A's request — a head-of-queue claim
    // would send B's key instead.
    assert_eq!(*door.calls.lock().unwrap(), 1);
    let keys = door.ledger.keys();
    assert!(
        !keys.is_empty() && keys.iter().all(|k| k == &key_a),
        "keys: {keys:?} want only {key_a}"
    );
}

#[test]
fn cad1041_concurrent_double_click_one_provider_call() {
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, _s) = e2e_release(&door);
    let (context, run, bundle, install) = approved_run(&h, "sndbl");
    let (id, _key) = send_now_fixture(
        &h,
        &context,
        &run,
        &bundle,
        &install,
        "cad1041-dbl",
        epoch_now(),
    );
    // Two operator clicks race on the same intent id; the single CAS
    // inside claim_id means only one wins the queued→processing move.
    let state = h.daemon.state.clone();
    let id2 = id.clone();
    let winner = thread::spawn(move || {
        cadence_agent::test_seam::scoped(cadence_agent::test_seam::Asserted::Operator, || {
            cadence_agent::client::rpc(&state, "social_publish_send_now", json!({"intent_id": id2}))
        })
    });
    let second = h
        .daemon
        .operator_rpc("social_publish_send_now", json!({"intent_id": id}));
    let first = winner.join().unwrap();
    // Exactly one posts; the other either posts the same claimed row
    // (idempotent claim) or refuses "no longer queued" — never two
    // provider sends.
    let posted = [&first, &second]
        .iter()
        .filter(|r| {
            r.as_ref()
                .map(|v| v["intent"]["state"] == "posted")
                .unwrap_or(false)
        })
        .count();
    assert_eq!(posted, 1, "first={first:?} second={second:?}");
    assert_eq!(
        *door.calls.lock().unwrap(),
        1,
        "exactly one provider call across the racing clicks"
    );
    let shown = h
        .daemon
        .operator_rpc("social_publish_show", json!({"intent_id": id}))
        .unwrap();
    assert_eq!(shown["intent"]["state"], "posted");
}

#[test]
fn cad1041_revoked_grant_sends_nothing() {
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, _s) = e2e_release(&door);
    let (context, run, bundle, install) = approved_run(&h, "snrev");
    let (id, _key) = send_now_fixture(
        &h,
        &context,
        &run,
        &bundle,
        &install,
        "cad1041-rev",
        epoch_now(),
    );
    // Revoke after schedule: staging refuses at the door, so the row is
    // claimed only to be reported refused and no exec POST ever leaves.
    door.grants.lock().unwrap().revoke(GRANT_FB);
    let out = h
        .daemon
        .operator_rpc("social_publish_send_now", json!({"intent_id": id}))
        .unwrap()["intent"]
        .clone();
    assert_eq!(out["state"], "refused", "revoked grant: {out}");
    let error = out["receipt"]["error"].as_str().unwrap_or("");
    assert!(error.contains("grant_revoked"), "{out}");
    assert_eq!(door.stages(), 1, "the refusing stage ran once");
    assert_eq!(*door.calls.lock().unwrap(), 0, "no exec POST, no status");
}

/// CAD-1041: an uncertain stage leaves the row queued with nothing sent;
/// once staging answers, the same click sends.
#[test]
fn cad1041_uncertain_staging_leaves_the_row_queued() {
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, sender) = e2e_release(&door);
    let (context, run, bundle, install) = approved_run(&h, "snunc");
    let (id, key) = send_now_fixture(
        &h,
        &context,
        &run,
        &bundle,
        &install,
        "cad1041-unc",
        epoch_now(),
    );
    sender.script_preflight(&key, Some(Staging::Uncertain));
    let err = h
        .daemon
        .operator_rpc("social_publish_send_now", json!({"intent_id": id}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("stays queued"), "{err}");
    let shown = h
        .daemon
        .operator_rpc("social_publish_show", json!({"intent_id": id}))
        .unwrap();
    assert_eq!(shown["intent"]["state"], "queued");
    assert_eq!(*door.calls.lock().unwrap(), 0, "nothing reached the door");
    sender.script_preflight(&key, None);
    let out = h
        .daemon
        .operator_rpc("social_publish_send_now", json!({"intent_id": id}))
        .unwrap();
    assert_eq!(out["intent"]["state"], "posted", "{out}");
    assert_eq!(*door.calls.lock().unwrap(), 1);
}

/// CAD-1041: a definitive staging refusal claims the row and reports it
/// refused with the door's code; execute never runs.
#[test]
fn cad1041_refused_staging_reports_refused_without_execute() {
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, sender) = e2e_release(&door);
    let (context, run, bundle, install) = approved_run(&h, "snref");
    let (id, key) = send_now_fixture(
        &h,
        &context,
        &run,
        &bundle,
        &install,
        "cad1041-ref",
        epoch_now(),
    );
    sender.script_preflight(&key, Some(Staging::Refused("not_publishable")));
    let out = h
        .daemon
        .operator_rpc("social_publish_send_now", json!({"intent_id": id}))
        .unwrap()["intent"]
        .clone();
    assert_eq!(out["state"], "refused", "{out}");
    let error = out["receipt"]["error"].as_str().unwrap_or("");
    assert!(error.contains("not_publishable"), "{out}");
    assert_eq!(*door.calls.lock().unwrap(), 0, "execute never ran");
    assert_eq!(door.stages(), 0);
}

/// CAD-1041: `nothing_sent` from execute (staging stayed ambiguous inside
/// the send) holds the row for a human; it is never burned `refused`.
#[test]
fn cad1041_nothing_sent_holds_the_row() {
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, sender) = e2e_release(&door);
    let (context, run, bundle, install) = approved_run(&h, "snheld");
    let (id, key) = send_now_fixture(
        &h,
        &context,
        &run,
        &bundle,
        &install,
        "cad1041-held",
        epoch_now(),
    );
    sender.script_execute_refusal(&key, "nothing_sent");
    let out = h
        .daemon
        .operator_rpc("social_publish_send_now", json!({"intent_id": id}))
        .unwrap()["intent"]
        .clone();
    assert_eq!(out["state"], "held", "{out}");
    let reason = out["receipt"]["reason"].as_str().unwrap_or("");
    assert!(reason.contains("nothing was sent"), "{out}");
    assert_eq!(*door.calls.lock().unwrap(), 0, "no exec POST left");
}

#[test]
fn cad1041_overdue_intent_refuses_until_rescheduled() {
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, _s) = e2e_release(&door);
    let (context, run, bundle, install) = approved_run(&h, "snlate");
    let (id, _key) = send_now_fixture(
        &h,
        &context,
        &run,
        &bundle,
        &install,
        "cad1041-late",
        epoch_now() - 901, // > MAX_LATENESS
    );
    let err = h
        .daemon
        .operator_rpc("social_publish_send_now", json!({"intent_id": id}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("overdue"), "{err}");
    assert_eq!(*door.calls.lock().unwrap(), 0);
    let shown = h
        .daemon
        .operator_rpc("social_publish_show", json!({"intent_id": id}))
        .unwrap();
    assert_eq!(shown["intent"]["state"], "queued");
}

#[test]
fn cad1041_send_now_board_route_reaches_the_same_gate() {
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, _s) = e2e_release(&door);
    let (context, run, bundle, install) = approved_run(&h, "snboard");
    let (id, _key) = send_now_fixture(
        &h,
        &context,
        &run,
        &bundle,
        &install,
        "cad1041-board",
        epoch_now() + 3600,
    );
    let board = Board::serve(&h);
    // The operator's click through the HTTP peer posts the named row.
    let (code, text) = board.post(
        &h,
        &format!("/api/social-publishes/{id}/send-now"),
        &json!({}),
    );
    assert_eq!(code, 200, "{code} {text}");
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["intent"]["state"], "posted", "{text}");
    assert_eq!(*door.calls.lock().unwrap(), 1);
    assert_eq!(door.stages(), 1);
    // A second click on the posted row is refused, provider count flat.
    let (code2, _text2) = board.post(
        &h,
        &format!("/api/social-publishes/{id}/send-now"),
        &json!({}),
    );
    assert!((400..500).contains(&code2), "{code2}");
    assert_eq!(*door.calls.lock().unwrap(), 1);
    assert_eq!(door.stages(), 1, "a refused replay never stages");
    // An unsigned session cannot reach the write: sign-in is the gate.
}

#[test]
fn cad1041_crash_after_door_accept_reconciles_never_resends() {
    let door = FakeDoor::start();
    door.grants.lock().unwrap().issue(GRANT_FB, 3);
    let (h, sender) = e2e_release(&door);
    let (context, run, bundle, install) = approved_run(&h, "sncrash");
    let (id, key) = send_now_fixture(
        &h,
        &context,
        &run,
        &bundle,
        &install,
        "cad1041-crash",
        epoch_now(),
    );
    // The door accepts the POST but the response is lost — the
    // in-RPC status reconcile recovers it to posted inside the same
    // call, never a second send.
    sender.set_behavior(&key, FakeProviderBehavior::LoseResponseAfterAccept);
    let out = h
        .daemon
        .operator_rpc("social_publish_send_now", json!({"intent_id": id}))
        .unwrap()["intent"]
        .clone();
    assert_eq!(out["state"], "posted", "{out}");
    // exec POST + one status GET — reconcile never re-sends.
    assert_eq!(*door.calls.lock().unwrap(), 2);
    assert_eq!(door.stages(), 1);
    // A send-now on the settled row refuses; no third provider call.
    let err = h
        .daemon
        .operator_rpc("social_publish_send_now", json!({"intent_id": id}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("queued"), "{err}");
    assert_eq!(*door.calls.lock().unwrap(), 2);
}
