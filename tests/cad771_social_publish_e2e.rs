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

fn connection_for(toolkit: Toolkit) -> &'static str {
    match toolkit {
        Toolkit::Instagram => CONN_IG,
        Toolkit::Facebook => CONN_FB,
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
        let expected_destination = Arc::new(Mutex::new(None));
        let omit_binding = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let corrupt_binding = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let status_forgeries = Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_grants = Arc::clone(&grants);
        let worker_ledger = Arc::clone(&ledger);
        let worker_calls = Arc::clone(&calls);
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
                *worker_calls.lock().unwrap() += 1;
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
}

impl HttpSender {
    fn new(base: String) -> Self {
        Self {
            base,
            behaviors: Mutex::new(HashMap::new()),
            status_forgeries: Mutex::new(HashMap::new()),
        }
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
        let behavior = self
            .behaviors
            .lock()
            .unwrap()
            .get(&binding.key)
            .copied()
            .unwrap_or(FakeProviderBehavior::Post);
        let verdict = self.post(
            "/v1/device/publish/exec",
            &json!({"key": binding.key,
            "connection_id": binding.connection_id,
            "toolkit": binding.toolkit.as_str(),
            "destination_id": binding.destination_id,
            "caption_digest": binding.caption_digest,
            "image_digest": binding.image_digest,
            "cadence_run_id": binding.cadence_run_id,
            "cadence_effect_id": binding.cadence_effect_id,
            "grant_id": binding.grant_id,
            "behavior": match behavior {
                FakeProviderBehavior::Post => "post",
                FakeProviderBehavior::Refuse => "refuse",
                FakeProviderBehavior::LoseResponseAfterAccept => "lose",
            }}),
        );
        Self::outcome_of(binding, &verdict)
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
    let toolkit =
        Toolkit::parse(frozen["toolkit"].as_str().unwrap_or("")).unwrap_or(Toolkit::Facebook);
    json!({"key": intent["request"],
        "connection_id": connection_for(toolkit), "toolkit": frozen["toolkit"],
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
    json!({"grant_id": frozen["grant_id"],
        "connection_id": frozen["connection_id"],
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
    let run = h.complete(&context, &format!("cad771-e2e-{tag}-run"));
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
fn e2e_release(door: &FakeDoor) -> (Release, Arc<HttpSender>) {
    let sender = Arc::new(HttpSender::new(format!("http://{}", door.addr)));
    let registered = Arc::clone(&sender);
    let h = Release::with_options(move |opts, _| {
        opts.social_publish_sender = Some(registered);
    });
    (h, sender)
}

fn freeze_params(
    context: &Value,
    run: &Value,
    bundle_digest: &str,
    install_id: &str,
    request: &str,
    effect: &str,
    due: i64,
) -> Value {
    json!({"request_id": request, "install_id": install_id,
        "context_id": context["id"], "run_id": run["id"],
        "artifact_id": run["artifacts"][0]["id"],
        "bundle_digest": bundle_digest,
        "slot": "publication", "effect_id": effect,
        "destination_id": DEST_FB, "toolkit": "facebook",
        "grant_id": GRANT_FB, "approval_id": "cad_approval_e2e_01",
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
                "cad_fx_e2e_01",
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
                "cad_fx_forged_receipt_01",
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
        "cad_fx_forged_01",
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
                "cad_fx_revoke_01",
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
                "cad_fx_revoke_stale_01",
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
                "cad_fx_exhaust_02",
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
                "cad_fx_exhaust_03",
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
                "cad_fx_lost_01",
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
    let h = Release::new();
    let (context, run, bundle_digest, install_id) = approved_run(&h, "cancel");
    let door = FakeDoor::start();
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
                "cad_fx_cancel_01",
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
            json!({"intent_id": intent["intent_id"]}),
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
    let h = Release::new();
    let (context, run, bundle_digest, install_id) = approved_run(&h, "stale");
    let binding = h.bind(&context, "cad771-e2e-stale-binding");
    let door = FakeDoor::start();
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
                "cad_fx_stale_01",
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
                "cad_fx_noecho_01",
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
                "cad_fx_xkey_a_01",
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
                "cad_fx_xkey_b_01",
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
                "cad_fx_corrupt_01",
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
                "cad_fx_hostile_01",
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
                "cad_fx_senderforge_01",
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
