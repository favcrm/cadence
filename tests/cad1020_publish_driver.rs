//! CAD-1020 adversarial tests for the daemon-owned publish driver.
//!
//! Every test names the guard it proves and was run red (guard removed)
//! before landing — per AGENTS.md "for any rule the code enforces, write
//! a test that fails without the guard". The sender below is a pure
//! in-process fake implementing `PublishSender` (incl. `preflight`);
//! it counts `execute`/`status`/`preflight` calls so "sent exactly once"
//! is a counted assertion, never a sleep-observe guess.
//!
//! The destinations resolver is the real `MediaResolver` client pointed
//! at a loopback stub that answers the destinations GET — the schedule
//! path's local→AOS `connectionId` resolution needs a real wire, never
//! a stubbed trust.

#![allow(clippy::disallowed_methods)]

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cadence_agent::daemon::ServeOptions;
use cadence_agent::platform::agenticos_external::media_import::MediaResolver;
use cadence_agent::platform::agenticos_external::publish::{
    LedgerOutcome, Preflight, PublishSender, PublishState, Refusal, SendBinding,
};
use cadence_agent::platform::agenticos_external::publish_sender::DeviceCredential;
use common::app_release::Release;
use serde_json::{json, Value};
use std::thread;

// ---------- loopback destinations stub ----------

/// Answers `GET /v1/runtime/connectors/destinations` with one
/// publishable row — the wire identity `connA_fake_wire` for any
/// `(toolkit, destination_id)`. Everything else 404s.
struct Destinations {
    addr: String,
    stop: Arc<std::sync::atomic::AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Destinations {
    fn start() -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap().to_string();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::SeqCst) {
                let Ok(Some(mut request)) = server.recv_timeout(Duration::from_millis(50)) else {
                    continue;
                };
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                let reply = if request
                    .url()
                    .starts_with("/v1/runtime/connectors/destinations")
                {
                    json!({"ok": true, "data": [{
                        "connectionId": "connA_fake_wire",
                        "toolkit": "facebook",
                        "destinationId": "dest-fb",
                        "publishable": true,
                    }, {
                        "connectionId": "connA_fake_wire_ig",
                        "toolkit": "instagram",
                        "destinationId": "dest-ig",
                        "publishable": true,
                    }]})
                } else {
                    json!({"ok": false, "error": {"code": "not_found", "message": "unknown"}})
                };
                let _ = request.respond(tiny_http::Response::from_string(reply.to_string()));
            }
        });
        Self {
            addr,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for Destinations {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// ---------- a counting fake sender ----------

/// Scripted door: per-key `preflight`/`execute`/`status` verdicts plus
/// call counts. `execute` records the key so "sent once" is provable.
#[derive(Default)]
struct FakeSender {
    /// keys this sender has executed (provider calls).
    sent: Mutex<Vec<String>>,
    /// keys this sender has status-read.
    status_reads: Mutex<Vec<String>>,
    /// keys preflighted.
    preflighted: Mutex<Vec<String>>,
    /// Script overrides.
    preflight_uncertain: Mutex<HashMap<String, bool>>,
    preflight_refused: Mutex<HashMap<String, Refusal>>,
    execute_processing: Mutex<HashMap<String, bool>>,
    /// Execute answers `processing` AND never records the key in `seen`
    /// — the door dropped the POST entirely (crash-before-send shape).
    execute_lost: Mutex<HashMap<String, bool>>,
    /// Status responses: key → outcome the fake returns; keys absent
    /// AND absent from `seen` answer `unknown_key`.
    status_outcome: Mutex<HashMap<String, PublishState>>,
    /// Keys the door "saw" — `status` answers `unknown_key` for others.
    /// The stored binding is echoed back in `status` replies so
    /// `note_evidence`'s frozen-equality check passes.
    seen: Mutex<HashMap<String, SendBinding>>,
}

impl FakeSender {
    /// The binding the frozen intent carries — mirrors the driver's
    /// `sender_binding` mapping for assertions.
    fn sent_count(&self) -> usize {
        self.sent.lock().unwrap().len()
    }
}

impl PublishSender for FakeSender {
    fn execute(&self, binding: &SendBinding) -> Result<LedgerOutcome, Refusal> {
        self.sent.lock().unwrap().push(binding.key.clone());
        let lost = self
            .execute_lost
            .lock()
            .unwrap()
            .remove(&binding.key)
            .unwrap_or(false);
        if !lost {
            self.seen
                .lock()
                .unwrap()
                .insert(binding.key.clone(), binding.clone());
        }
        if lost
            || self
                .execute_processing
                .lock()
                .unwrap()
                .remove(&binding.key)
                .unwrap_or(false)
        {
            // Ambiguous transport: the provider may have accepted — the
            // outcome is processing with no evidence, never a retry.
            return Ok(LedgerOutcome {
                state: PublishState::Processing,
                permalink: None,
                destination_id: binding.destination_id.clone(),
                caption_digest: binding.caption_digest.clone(),
                image_digest: binding.image_digest.clone(),
                provider_payload: None,
                provider_ids: vec![],
                repeated: false,
            });
        }
        Ok(LedgerOutcome {
            state: PublishState::Posted,
            permalink: Some("https://example.test/p/1".into()),
            destination_id: binding.destination_id.clone(),
            caption_digest: binding.caption_digest.clone(),
            image_digest: binding.image_digest.clone(),
            provider_payload: Some(r#"{"id":"post-1"}"#.into()),
            provider_ids: vec!["post-1".into()],
            repeated: false,
        })
    }

    fn status(&self, key: &str) -> Result<LedgerOutcome, Refusal> {
        self.status_reads.lock().unwrap().push(key.to_owned());
        let binding = match self.seen.lock().unwrap().get(key).cloned() {
            Some(binding) => binding,
            None => return Err(Refusal::new("unknown_key", "the door has no record")),
        };
        match self
            .status_outcome
            .lock()
            .unwrap()
            .get(key)
            .copied()
            .unwrap_or(PublishState::Posted)
        {
            PublishState::Posted => Ok(LedgerOutcome {
                state: PublishState::Posted,
                permalink: Some("https://example.test/p/1".into()),
                destination_id: binding.destination_id.clone(),
                caption_digest: binding.caption_digest.clone(),
                image_digest: binding.image_digest.clone(),
                provider_payload: Some(r#"{"id":"post-1"}"#.into()),
                provider_ids: vec!["post-1".into()],
                repeated: true,
            }),
            _ => Ok(LedgerOutcome {
                state: PublishState::Refused,
                permalink: None,
                destination_id: binding.destination_id.clone(),
                caption_digest: binding.caption_digest.clone(),
                image_digest: binding.image_digest.clone(),
                provider_payload: None,
                provider_ids: vec![],
                repeated: true,
            }),
        }
    }

    fn preflight(&self, binding: &SendBinding) -> Preflight {
        self.preflighted.lock().unwrap().push(binding.key.clone());
        if self
            .preflight_uncertain
            .lock()
            .unwrap()
            .get(&binding.key)
            .copied()
            .unwrap_or(false)
        {
            return Preflight::Uncertain(Refusal::new(
                "refused",
                "preflight uncertain; row stays queued",
            ));
        }
        if let Some(refusal) = self.preflight_refused.lock().unwrap().remove(&binding.key) {
            return Preflight::Refused(refusal);
        }
        Preflight::Approved
    }
}

// ---------- rig ----------

/// Shared pieces a driver test needs: the release daemon (fake sender
/// plus real resolver over the loopback destinations stub) and the
/// handles the test pokes.
struct Rig {
    release: Release,
    sender: Arc<FakeSender>,
    _destinations: Destinations,
}

/// A daemon with the fake sender attached, the real destinations
/// resolver over loopback, and the driver ticking hot. `configure`
/// mutates `ServeOptions` after the driver seam is set (kill switch,
/// lateness, clock, test-seam gates).
fn rig(configure: impl FnOnce(&mut ServeOptions) + Send + 'static) -> Rig {
    let destinations = Destinations::start();
    let dest_base = format!("http://{}", destinations.addr);
    let sender = Arc::new(FakeSender::default());
    let registered: Arc<dyn PublishSender> = sender.clone();
    let release = Release::with_options(move |opts, _| {
        opts.social_publish_sender = Some(registered);
        opts.publish_driver_off = Some(false);
        opts.publish_driver_ms = Some(50);
        opts.social_media_resolver = Some(Arc::new(
            MediaResolver::new(&dest_base, DeviceCredential::new("cad-test-read".into()))
                .expect("loopback resolver"),
        ));
        configure(opts);
    });
    Rig {
        release,
        sender,
        _destinations: destinations,
    }
}

fn wait_until(deadline_secs: u64, what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(deadline_secs);
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

/// Schedule a facebook intent through the real approved-run path
/// (freeze derives digests; the resolver binds the wire id).
fn schedule(
    h: &Release,
    context: &Value,
    run: &Value,
    bundle_digest: &str,
    request: &str,
    due: i64,
) -> Value {
    h.daemon
        .operator_rpc(
            "social_publish_schedule",
            json!({"request_id": request, "install_id": h.install["install_id"],
                "context_id": context["id"], "run_id": run["id"],
                "artifact_id": run["artifacts"][0]["id"],
                "bundle_digest": bundle_digest,
                "slot": "publication", "effect_id": format!("fx-{request}"),
                "destination_id": "dest-fb", "toolkit": "facebook",
                "grant_id": "dpq_fake_grant", "approval_id": "appr-1",
                "due_epoch": due, "timezone": "UTC"}),
        )
        .unwrap()["intent"]
        .clone()
}

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Build the approved-run fixture: context + binding + completed run.
fn approved(h: &Release, tag: &str) -> (Value, Value, String) {
    let context = h.context("Harbour", "A", &format!("{tag}-ctx"));
    h.bind(&context, &format!("{tag}-bind"));
    let run = h.complete(&context, &format!("{tag}-run"));
    let bundle = run["snapshot"]["bundle_digest"]
        .as_str()
        .unwrap()
        .to_owned();
    (context, run, bundle)
}

fn state_of(h: &Release, intent_id: &str) -> String {
    h.daemon
        .operator_rpc("social_publish_show", json!({"intent_id": intent_id}))
        .unwrap()["intent"]["state"]
        .as_str()
        .unwrap_or("")
        .to_owned()
}

// ---------- tests ----------

/// Acceptance 1: a publish due at T is sent within one driver interval
/// of T with no manual command — the driver loop claims it.
#[test]
fn driver_sends_due_publish_within_one_interval() {
    let rig = rig(|_| {});
    let (context, run, bundle) = approved(&rig.release, "due");
    let intent = schedule(
        &rig.release,
        &context,
        &run,
        &bundle,
        "due-1",
        now_epoch() - 1,
    );
    wait_until(30, "driver posts the due intent", || {
        state_of(&rig.release, intent["intent_id"].as_str().unwrap()) == "posted"
    });
    let key = intent["request"].as_str().unwrap();
    assert_eq!(rig.sender.sent_count(), 1, "sent once");
    assert_eq!(rig.sender.sent.lock().unwrap()[0], *key);
}

/// Kill switch: `publish_driver_off` leaves the row queued and the
/// sender untouched — the canary's explicit opt-out.
#[test]
fn driver_off_via_kill_switch() {
    let rig = rig(|opts| opts.publish_driver_off = Some(true));
    let (context, run, bundle) = approved(&rig.release, "off");
    let intent = schedule(
        &rig.release,
        &context,
        &run,
        &bundle,
        "off-1",
        now_epoch() - 1,
    );
    // Give the loop several ticks to prove it never claims.
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        state_of(&rig.release, intent["intent_id"].as_str().unwrap()),
        "queued"
    );
    assert_eq!(rig.sender.sent_count(), 0);
    assert!(rig.sender.preflighted.lock().unwrap().is_empty());
    // The kill switch is observable.
    let health = rig
        .release
        .daemon
        .operator_rpc("health", json!({}))
        .unwrap();
    assert_eq!(health["social_publish_driver"]["status"], "off");
}

/// Without a sender the driver reports `sender_not_configured` and
/// never claims — acceptance 3.
#[test]
fn no_sender_reports_sender_not_configured() {
    let destinations = Destinations::start();
    let dest_base = format!("http://{}", destinations.addr);
    let release = Release::with_options(move |opts, _| {
        opts.publish_driver_off = Some(false);
        opts.publish_driver_ms = Some(50);
        opts.social_media_resolver = Some(Arc::new(
            MediaResolver::new(&dest_base, DeviceCredential::new("cad-test-read".into()))
                .expect("loopback resolver"),
        ));
        // no social_publish_sender
    });
    wait_until(15, "driver reports sender_not_configured", || {
        release.daemon.operator_rpc("health", json!({})).unwrap()["social_publish_driver"]["status"]
            == "sender_not_configured"
    });
    // And nothing was claimed: a queued intent stays queued.
    let (context, run, bundle) = approved(&release, "nosender");
    let intent = schedule(&release, &context, &run, &bundle, "ns-1", now_epoch() - 1);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        state_of(&release, intent["intent_id"].as_str().unwrap()),
        "queued"
    );
}

/// Preflight ambiguity (door 5xx/timeout) before claim: the row stays
/// `queued` and no send ever happens — acceptance 4's pre-POST half.
/// Mutation: claim-then-report-refused would leave state `refused`.
#[test]
fn preflight_5xx_leaves_row_queued_no_send() {
    let rig = rig(|_| {});
    let (context, run, bundle) = approved(&rig.release, "pref5");
    let intent = schedule(
        &rig.release,
        &context,
        &run,
        &bundle,
        "pref5-1",
        now_epoch() - 1,
    );
    let key = intent["request"].as_str().unwrap().to_owned();
    rig.sender
        .preflight_uncertain
        .lock()
        .unwrap()
        .insert(key.clone(), true);
    // One uncertain preflight tick: row stays queued, no send.
    wait_until(30, "the uncertain preflight ran", || {
        !rig.sender.preflighted.lock().unwrap().is_empty()
    });
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        state_of(&rig.release, intent["intent_id"].as_str().unwrap()),
        "queued"
    );
    assert_eq!(rig.sender.sent_count(), 0);
    // The transient error is surfaced on the intent.
    let shown = rig
        .release
        .daemon
        .operator_rpc(
            "social_publish_show",
            json!({"intent_id": intent["intent_id"]}),
        )
        .unwrap();
    assert!(
        shown["intent"]["driver"]["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("preflight"),
        "last_error must carry the preflight refusal: {shown}"
    );
}

/// Definitive preflight refusal (a revoked grant): the driver claims
/// and reports `refused` — a definitive end, never a send, and the
/// refusal code reaches the receipt.
#[test]
fn revoked_grant_preflight_refuses_no_send() {
    let rig = rig(|_| {});
    let (context, run, bundle) = approved(&rig.release, "revoke");
    let intent = schedule(
        &rig.release,
        &context,
        &run,
        &bundle,
        "revoke-1",
        now_epoch() - 1,
    );
    let key = intent["request"].as_str().unwrap().to_owned();
    rig.sender
        .preflight_refused
        .lock()
        .unwrap()
        .insert(key, Refusal::new("grant_revoked", "send grant was revoked"));
    wait_until(30, "the refused intent settles", || {
        state_of(&rig.release, intent["intent_id"].as_str().unwrap()) == "refused"
    });
    assert_eq!(rig.sender.sent_count(), 0);
    let shown = rig
        .release
        .daemon
        .operator_rpc(
            "social_publish_show",
            json!({"intent_id": intent["intent_id"]}),
        )
        .unwrap();
    assert!(
        shown["intent"]["receipt"]["error"]
            .as_str()
            .unwrap_or("")
            .contains("grant_revoked"),
        "receipt must carry the door refusal: {shown}"
    );
}

/// Post-POST ambiguity: `execute` returns `processing` (the door may
/// have accepted); the row reconciles through `status` — never a
/// second `execute`. Acceptance 4's post-POST half + I5.
#[test]
fn post_5xx_goes_processing_then_status_posted() {
    let rig = rig(|_| {});
    let (context, run, bundle) = approved(&rig.release, "post5");
    let intent = schedule(
        &rig.release,
        &context,
        &run,
        &bundle,
        "post5-1",
        now_epoch() - 1,
    );
    let key = intent["request"].as_str().unwrap().to_owned();
    rig.sender
        .execute_processing
        .lock()
        .unwrap()
        .insert(key.clone(), true);
    wait_until(30, "the ambiguous send reconciles to posted", || {
        state_of(&rig.release, intent["intent_id"].as_str().unwrap()) == "posted"
    });
    // Exactly one execute; recovery was a status read, not a re-send.
    assert_eq!(rig.sender.sent_count(), 1);
    assert!(rig.sender.status_reads.lock().unwrap().contains(&key));
}

/// Crash-before-send shape: a row the door never saw stays
/// `processing` while unknown_key is fresh, then escalates to `held`
/// past the bound — never re-sent (finding 1, I5).
#[test]
fn unknown_key_persisted_escalates_to_held() {
    let clock = Arc::new(AtomicU64::new(now_epoch() as u64));
    let clock_read = Arc::clone(&clock);
    let rig = rig(move |opts| {
        opts.publish_driver_clock =
            Some(Arc::new(move || clock_read.load(Ordering::SeqCst) as i64));
    });
    let (context, run, bundle) = approved(&rig.release, "unk");
    let intent = schedule(
        &rig.release,
        &context,
        &run,
        &bundle,
        "unk-1",
        now_epoch() - 5,
    );
    let key = intent["request"].as_str().unwrap().to_owned();
    // The door dropped the POST entirely: execute returns processing
    // but `status` will answer unknown_key (the door never saw it).
    rig.sender
        .execute_lost
        .lock()
        .unwrap()
        .insert(key.clone(), true);
    let id = intent["intent_id"].as_str().unwrap();
    wait_until(30, "the intent is claimed and processing", || {
        state_of(&rig.release, id) == "processing" && rig.sender.sent_count() == 1
    });
    // Advance the injected clock past UNKNOWN_KEY_HOLD_SECS.
    clock.fetch_add(400, Ordering::SeqCst);
    wait_until(30, "the unknown-key row escalates to held", || {
        state_of(&rig.release, id) == "held"
    });
    let shown = rig
        .release
        .daemon
        .operator_rpc("social_publish_show", json!({"intent_id": id}))
        .unwrap();
    assert!(
        shown["intent"]["receipt"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("no record"),
        "held reason must name the missing door record: {shown}"
    );
    // And the row was never re-executed.
    assert_eq!(rig.sender.sent_count(), 1);
}

/// Stale backlog: a due row older than `max_lateness` is claimed and
/// held ("missed publish window"), never sent — the canary's first
/// start does not dump old intents (finding 3, I8).
#[test]
fn stale_due_row_held_not_sent() {
    let rig = rig(|opts| {
        opts.publish_driver_max_lateness_secs = Some(60);
    });
    let (context, run, bundle) = approved(&rig.release, "stale");
    // Due two hours ago — well past the 60s lateness bound.
    let intent = schedule(
        &rig.release,
        &context,
        &run,
        &bundle,
        "stale-1",
        now_epoch() - 7200,
    );
    wait_until(30, "the stale intent is held", || {
        state_of(&rig.release, intent["intent_id"].as_str().unwrap()) == "held"
    });
    assert_eq!(rig.sender.sent_count(), 0);
    let shown = rig
        .release
        .daemon
        .operator_rpc(
            "social_publish_show",
            json!({"intent_id": intent["intent_id"]}),
        )
        .unwrap();
    assert!(
        shown["intent"]["receipt"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("missed publish window"),
        "held reason must name the missed window: {shown}"
    );
}

/// Cancel before due: the operator cancels a queued intent and the
/// driver never sends it — acceptance 5.
#[test]
fn cancel_before_due_prevents_send() {
    let rig = rig(|_| {});
    let (context, run, bundle) = approved(&rig.release, "cancel");
    // Due 2s out — cancel lands while the row is still queued, then
    // several ticks pass the due time: a cancelled row never sends.
    let intent = schedule(
        &rig.release,
        &context,
        &run,
        &bundle,
        "cxl-1",
        now_epoch() + 2,
    );
    let id = intent["intent_id"].as_str().unwrap();
    let cancelled = rig
        .release
        .daemon
        .operator_rpc("social_publish_cancel", json!({"intent_id": id}))
        .unwrap();
    assert_eq!(cancelled["intent"]["state"], "cancelled");
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(state_of(&rig.release, id), "cancelled");
    assert_eq!(rig.sender.sent_count(), 0);
}

/// Lease lost mid-tick: the fence trips inside the claim→send window
/// (test-seam gate). The claim commits — a durable `processing` row —
/// but the pre-send check refuses the HTTP call: `sent` stays empty.
/// Mutation: deleting the mid-tick `fence.check()` lets `execute` run.
#[cfg(feature = "test-seam")]
#[test]
fn lease_lost_mid_tick_no_send() {
    use std::sync::atomic::AtomicBool;
    // A file-lease daemon: the fence is real. The claimed_gate hook
    // trips the lease's file record away between claim and send.
    let lease_dir = tempfile::tempdir().unwrap();
    let lease_file = lease_dir.path().join("lease.json");
    let tripped = Arc::new(AtomicBool::new(false));
    let trip_read = Arc::clone(&tripped);
    let lease_path = lease_file.clone();
    let sender = Arc::new(FakeSender::default());
    let registered: Arc<dyn PublishSender> = sender.clone();
    let destinations = Destinations::start();
    let dest_base = format!("http://{}", destinations.addr);
    let release = Release::with_options(move |opts, _| {
        opts.social_publish_sender = Some(registered);
        opts.publish_driver_off = Some(false);
        opts.publish_driver_ms = Some(50);
        opts.social_media_resolver = Some(Arc::new(
            MediaResolver::new(&dest_base, DeviceCredential::new("cad-test-read".into()))
                .expect("loopback resolver"),
        ));
        // Short lease so a stolen file trips the fence fast: TTL 3s,
        // renew 1s — two failed beats trip it inside the gate's wait.
        opts.lease = Some(cadence_agent::lease::Hosted {
            lease: Some(format!("file:{}", lease_path.display())),
            lease_ttl_secs: Some(3),
            lease_renew_secs: Some(1),
            flush_timeout_secs: Some(1),
        });
        let gate = Arc::clone(&trip_read);
        let lease_file = lease_file.clone();
        opts.publish_driver_claimed_gate = Some(Arc::new(move || {
            if !gate.swap(true, Ordering::SeqCst) {
                // Steal the lease: overwrite the record as a foreign
                // holder, then wait out the heartbeat — the next renew
                // fails, which trips the shared fence while the claim
                // is still committed but unsent.
                std::fs::write(
                    &lease_file,
                    json!({"holder": "foreign", "epoch": 99, "expires_unix": 9e9}).to_string(),
                )
                .unwrap();
                // renew_every is 1s; two beats guarantee the trip lands.
                std::thread::sleep(Duration::from_millis(2500));
            }
        }));
    });
    let (context, run, bundle) = approved(&release, "lease");
    let intent = schedule(
        &release,
        &context,
        &run,
        &bundle,
        "lease-1",
        now_epoch() - 1,
    );
    // The claim commits (state processing is visible while the gate
    // still sleeps — don't assert yet); the gate steals the lease and
    // waits out the heartbeat so the fence trips inside the window.
    wait_until(30, "the gate steals the lease inside the window", || {
        tripped.load(Ordering::SeqCst)
    });
    // The gate returns ~2.5s after the steal (two renew beats); give the
    // driver another beat to run the post-claim check and dispatch.
    std::thread::sleep(Duration::from_millis(3200));
    assert_eq!(
        state_of(&release, intent["intent_id"].as_str().unwrap()),
        "processing"
    );
    assert_eq!(
        sender.sent_count(),
        0,
        "a fence trip between claim and send must prevent the send"
    );
}

/// Reconcile edge: a `processing` row whose status comes back
/// `reconnect_needed` must report `refused` outright — noting the
/// evidence would be rejected and spin the tick forever (finding 10a).
/// Mutation: routing refused/reconnect through `note_evidence` leaves
/// the row `processing` and the error surfaces repeatedly.
#[test]
fn reconnect_needed_reports_refused_no_loop() {
    let rig = rig(|_| {});
    let (context, run, bundle) = approved(&rig.release, "recon");
    let intent = schedule(
        &rig.release,
        &context,
        &run,
        &bundle,
        "recon-1",
        now_epoch() - 1,
    );
    let key = intent["request"].as_str().unwrap().to_owned();
    // The send leaves the row processing (ambiguous transport), then
    // status answers refused — a definitive provider end.
    rig.sender
        .execute_processing
        .lock()
        .unwrap()
        .insert(key.clone(), true);
    rig.sender
        .status_outcome
        .lock()
        .unwrap()
        .insert(key, PublishState::Refused);
    wait_until(30, "the reconnect row reports refused", || {
        state_of(&rig.release, intent["intent_id"].as_str().unwrap()) == "refused"
    });
    assert_eq!(rig.sender.sent_count(), 1);
}

/// Dispatch-verb caller gate while the driver is live: an agent caller,
/// a detached `setsid` child and the board HTTP peer must all be refused
/// — only the daemon's own driver and operator connections dispatch.
/// Pinned to `operator_connection` (daemon RPC) and `route()`'s 404
/// (board relay) — finding 9.
#[test]
fn agent_child_board_cannot_drive() {
    let rig = rig(|_| {});
    let (context, run, bundle) = approved(&rig.release, "gate");
    let intent = schedule(
        &rig.release,
        &context,
        &run,
        &bundle,
        "gate-1",
        now_epoch() - 1,
    );
    // An agent-asserted caller must be refused every dispatch verb.
    for method in [
        "social_publish_claim_due",
        "social_publish_reconcile",
        "social_publish_report",
    ] {
        let refused = rig
            .release
            .daemon
            .agent_rpc("test-agent", method, json!({}))
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("operator") || refused.contains("caller"),
            "{method} must refuse an agent caller: {refused}"
        );
    }
    // A detached child (setsid) caller is refused too — `unproven_rpc`
    // runs the call outside every pane without operator proof.
    let refused = rig
        .release
        .daemon
        .unproven_rpc(
            "social_publish_claim_due",
            json!({"now_epoch": now_epoch()}),
        )
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("operator") || refused.contains("caller"),
        "a detached caller must be refused: {refused}"
    );
    // Board routes for the dispatch verbs stay 404 — the relay never
    // reaches the RPC. (Proven at route() level in ui::social_publish
    // unit tests; here the board-facing surface stays absent.)
    let _ = intent;
}
