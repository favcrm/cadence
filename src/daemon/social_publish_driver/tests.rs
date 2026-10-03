//! CAD-1020 ticket-result checks. Each test runs the daemon's own driver
//! thread on a real store holding an approved, artifact-frozen intent, and
//! observes the outcome where the ticket promises it: the intent's
//! persisted state and the publish requests that reach the provider door.
//! No operator verb is called anywhere: the driver is the only trigger.
use super::*;
use crate::platform::agenticos_external::publish::{LedgerOutcome, PublishSender, SendBinding};
use crate::store::app_runs::{LocalRunRequest, LocalWorkflow};
use std::collections::BTreeMap;
use std::sync::atomic::AtomicI64;
use std::sync::Condvar;

const INSTALL: &str = "install-1";
const BUNDLE: &str = "sha256:bundle";

/// The provider door. `sends` is every publish request that reached it;
/// a refused or uncertain key is answered at staging (preflight).
#[derive(Default)]
struct Door {
    sends: Mutex<Vec<String>>,
    accepted: Mutex<HashMap<String, SendBinding>>,
    preflights: Mutex<Vec<String>>,
    refused: Mutex<HashMap<String, Refusal>>,
    uncertain: Mutex<Vec<String>>,
    /// The door accepts the post but the reply never arrives.
    lose_reply: AtomicBool,
    /// Runs once the door has accepted a post (the crash point).
    after_accept: Mutex<Option<Box<dyn Fn() + Send>>>,
    /// Runs once, inside the next status read (a slow door).
    on_status: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Staging waits until this many callers stage at once (or 5s).
    meet: (Mutex<(usize, usize)>, Condvar),
}

impl Door {
    fn sent(&self) -> Vec<String> {
        self.sends.lock().unwrap().clone()
    }

    fn outcome(state: PublishState, binding: &SendBinding) -> LedgerOutcome {
        let posted = state == PublishState::Posted;
        LedgerOutcome {
            state,
            permalink: posted.then(|| "https://example.test/p/1".into()),
            destination_id: binding.destination_id.clone(),
            caption_digest: binding.caption_digest.clone(),
            image_digest: binding.image_digest.clone(),
            provider_payload: posted.then(|| r#"{"id":"post-1"}"#.into()),
            provider_ids: if posted {
                vec!["post-1".into()]
            } else {
                vec![]
            },
            repeated: false,
        }
    }
}

impl PublishSender for Door {
    fn execute(&self, binding: &SendBinding) -> std::result::Result<LedgerOutcome, Refusal> {
        self.sends.lock().unwrap().push(binding.key.clone());
        self.accepted
            .lock()
            .unwrap()
            .insert(binding.key.clone(), binding.clone());
        if let Some(crash) = self.after_accept.lock().unwrap().as_ref() {
            crash();
        }
        let state = if self.lose_reply.load(Ordering::SeqCst) {
            PublishState::Processing
        } else {
            PublishState::Posted
        };
        Ok(Self::outcome(state, binding))
    }

    fn status(&self, key: &str) -> std::result::Result<LedgerOutcome, Refusal> {
        if let Some(slow) = self.on_status.lock().unwrap().take() {
            slow();
        }
        match self.accepted.lock().unwrap().get(key) {
            Some(binding) => Ok(Self::outcome(PublishState::Posted, binding)),
            None => Err(Refusal::new("unknown_key", "the door has no record")),
        }
    }

    fn preflight(&self, binding: &SendBinding) -> Preflight {
        self.preflights.lock().unwrap().push(binding.key.clone());
        let (lock, cvar) = &self.meet;
        let mut meet = lock.lock().unwrap();
        meet.1 += 1;
        cvar.notify_all();
        let until = Instant::now() + Duration::from_secs(5);
        while meet.1 < meet.0 && Instant::now() < until {
            meet = cvar
                .wait_timeout(meet, Duration::from_millis(50))
                .unwrap()
                .0;
        }
        drop(meet);
        if let Some(refusal) = self.refused.lock().unwrap().get(&binding.key) {
            return Preflight::Refused(refusal.clone());
        }
        if self.uncertain.lock().unwrap().contains(&binding.key) {
            return Preflight::Uncertain(Refusal::new("refused", "staging timed out"));
        }
        Preflight::Approved
    }
}

/// One daemon on `dir` with its driver thread running hot on `clock`.
struct Daemon {
    shared: Arc<Shared>,
    driver: Option<std::thread::JoinHandle<()>>,
}

impl Daemon {
    /// A daemon on `dir` with the driver forced on, hot, on `clock`.
    fn start(dir: &Path, door: &Arc<Door>, clock: &Arc<AtomicI64>) -> Self {
        Self::open(dir, door, clock, |_| {}).run()
    }

    /// The daemon without its driver thread yet; `configure` adjusts the
    /// options after the defaults below.
    fn open(
        dir: &Path,
        door: &Arc<Door>,
        clock: &Arc<AtomicI64>,
        configure: impl FnOnce(&mut ServeOptions),
    ) -> Self {
        let clock = clock.clone();
        let mut opts = ServeOptions {
            social_publish_sender: Some(door.clone()),
            // Explicitly unleased; never read a pm.yaml.
            lease: Some(crate::lease::Hosted::default()),
            social_publish_driver_off: Some(false),
            social_publish_driver_ms: Some(20),
            social_publish_driver_clock: Some(Arc::new(move || clock.load(Ordering::SeqCst))),
            ..ServeOptions::default()
        };
        configure(&mut opts);
        Self {
            shared: Shared::new(dir, &opts).unwrap(),
            driver: None,
        }
    }

    fn run(mut self) -> Self {
        let shared = self.shared.clone();
        self.driver = Some(std::thread::spawn(move || {
            shared.run_social_publish_driver()
        }));
        self
    }

    /// Wait until the driver has finished `n` more ticks, so a whole tick
    /// started after this call. Absence checks follow this, not a sleep.
    fn ticks(&self, n: usize) {
        let tick = || self.shared.social_publish_driver.status_json()["last_tick"].clone();
        let until = Instant::now() + Duration::from_secs(20);
        let mut seen = tick();
        for _ in 0..n {
            while tick() == seen {
                assert!(Instant::now() < until, "the driver stopped ticking");
                std::thread::sleep(Duration::from_millis(5));
            }
            seen = tick();
        }
    }

    /// The state the intent settles in once it leaves `queued` and
    /// `processing`.
    fn settled(&self, intent: &Value) -> String {
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            let state = self.state(intent);
            if state != "queued" && state != "processing" {
                return state;
            }
            assert!(Instant::now() < until, "intent never settled");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn stop(&mut self) {
        self.shared.closing.store(true, Ordering::SeqCst);
        if let Some(driver) = self.driver.take() {
            driver.join().unwrap();
        }
    }

    fn state(&self, intent: &Value) -> String {
        let shown = self.shared.store.social_publish_show(id(intent)).unwrap();
        shown["intent"]["state"].as_str().unwrap().to_owned()
    }

    fn wait_state(&self, intent: &Value, want: &str) -> Value {
        let until = Instant::now() + Duration::from_secs(20);
        while self.state(intent) != want {
            assert!(Instant::now() < until, "intent never reached {want}");
            std::thread::sleep(Duration::from_millis(20));
        }
        self.shared.store.social_publish_show(id(intent)).unwrap()["intent"].clone()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

fn id(intent: &Value) -> &str {
    intent["intent_id"].as_str().unwrap()
}

fn key(intent: &Value) -> String {
    intent["request"].as_str().unwrap().to_owned()
}

fn wall() -> i64 {
    crate::issue::time::now_epoch()
}

/// An approved run's reviewed artifact, frozen as an intent due at `due`:
/// a writer and an independent reviewer turn complete on a publication-
/// bound run, a live app effect authorizes it, and freeze re-proves it all.
fn approved_intent(store: &Store, tag: &str, due: i64) -> Value {
    let workspace = store.connection_workspace_id().unwrap();
    if store.agent_opt("writer").unwrap().is_none() {
        for (alias, role) in [("lead", "pm"), ("writer", "worker"), ("reviewer", "worker")] {
            store
                .register_agent(&crate::store::NewAgent {
                    alias,
                    provider: "claude",
                    endpoint_kind: "managed",
                    role,
                    cwd: "/tmp",
                    sandbox: "read-only",
                    instructions: None,
                    params: Some("{\"upstream\":\"lead\"}"),
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
            let identity = crate::adapter::Identity {
                thread_id: "t".into(),
                session_id: "s".into(),
                model: None,
                effort: None,
                pid: 4242,
                endpoint: None,
                generation: Some("g1".into()),
                attach: None,
            };
            store.set_identity(alias, &identity).unwrap();
        }
        store.app_capability_decide(INSTALL, BUNDLE, true).unwrap();
        let connection = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!("{workspace}:local:local").as_bytes(),
        );
        let config = json!({"schema":1,"install_id":INSTALL,"context":null,"bundle_digest":BUNDLE,
            "connection_kind":"builtin","provider":"local","account":"local",
            "connection_id":format!("builtin-{}", connection.simple()),"workspace_id":workspace});
        store
            .app_binding_create(INSTALL, None, "publication", &config, "bind-1")
            .unwrap();
    }
    let proof = store
        .app_binding_for_slot(INSTALL, None, "publication", BUNDLE)
        .unwrap()
        .unwrap();
    let text = "---\ntitle: Local\ngoal: Reviewed text\npublication_slot: publication\n---\n## Write\nagent: writer\naction: local.text.produce\n\nWrite Markdown.\n\n### Acceptance\n- [ ] Markdown artifact exists\n\n## Review\nagent: reviewer\ndepends_on: 1\naction: local.text.review\n\nReview the exact artifact.\n\n### Acceptance\n- [ ] Exact artifact reviewed\n";
    let inputs = BTreeMap::new();
    let workflow = LocalWorkflow::parse(text, &inputs).unwrap();
    let request = LocalRunRequest {
        install_id: INSTALL,
        bundle_digest: BUNDLE,
        workflow: &workflow,
        inputs: &inputs,
        request_id: &format!("run-{tag}"),
        owner_pm: "lead",
        project_link: None,
    };
    let run = store
        .app_run_create_with_publication(request, None, Some(&proof))
        .unwrap();
    let run_id = run["id"].as_str().unwrap().to_owned();
    store
        .app_run_decide(
            &run_id,
            run["snapshot_digest"].as_str(),
            false,
            Some(BUNDLE),
        )
        .unwrap();
    let body = format!("# Post {tag}\nReviewed copy.");
    let turn = |run: &Value, step: usize, alias: &str, reply: Value| {
        let message = run["steps"][step]["message_id"].as_str().unwrap();
        let crate::store::Take::Message(taken) = store
            .take_queued_app_proven(alias, Some((message, BUNDLE)))
            .unwrap()
        else {
            panic!("app turn must be claimed")
        };
        let token = crate::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
        store.mark_running(&taken.id, &token).unwrap();
        let taken = store.message(&taken.id).unwrap().unwrap();
        let reply = json!({"turn_id":taken.turn_id,"text":reply.to_string()});
        store.finish(&taken, "completed", &reply, None).unwrap();
    };
    let run = store.app_run_dispatch(&run_id, BUNDLE).unwrap();
    turn(
        &run,
        0,
        "writer",
        json!({"schema":1,"kind":"produce_text","run_id":run_id,"step_id":"s1","revision":1,"outcome":"succeeded","artifacts":[{"media_type":"text/markdown","text":body}]}),
    );
    let run = store.app_run_dispatch(&run_id, BUNDLE).unwrap();
    let artifact = run["artifacts"][0]["id"].as_str().unwrap().to_owned();
    let digest = crate::store::app_runs::artifact_digest(body.as_bytes());
    turn(
        &run,
        1,
        "reviewer",
        json!({"schema":1,"kind":"review_text","run_id":run_id,"step_id":"s2","revision":1,"producer_step_id":"s1","producer_revision":1,"artifact_sha256":digest,"decision":"approve","rationale":"Checked the exact artifact."}),
    );
    // CAD-1027: a live app effect authorized by this run's artifact.
    let effect = format!("effect-{tag}");
    let mut provenance = json!({"install_id":INSTALL,"context_id":null,"run_id":run_id,
        "artifact_id":artifact,"artifact_digest":digest,"binding_id":proof.id,
        "binding_revision":proof.revision,"binding_digest":proof.digest,"sink_registration":"sink-one"});
    let mut authority = json!({"schema":1,"install_id":INSTALL,"context":null,"run_id":run_id,
        "artifact_id":artifact,"artifact_digest":digest,"provenance":provenance});
    provenance["effect_id"] = json!(effect);
    provenance["authorization_kind"] = json!("app_artifact");
    provenance["authority_digest"] = json!(crate::store::app_effects::authority_digest(&authority));
    authority["provenance"] = provenance.clone();
    let row = crate::store::EffectRow {
        effect_id: effect.clone(),
        request: format!("effect-request-{tag}"),
        agent: "lead".into(),
        platform: "local".into(),
        account: "local".into(),
        tool: "publish_app_text".into(),
        label: None,
        input: json!({"schema":1,"title":"Post","body":body,"provenance":provenance}),
        input_summary: "Post".into(),
        preview: body.clone(),
        source_name: None,
        source_hash: Some(digest),
        scopes: vec!["publish".into()],
        task: None,
        state: "waiting".into(),
        close_reason: None,
        decision: None,
        outcome: None,
        needs_you: false,
        staged_at: 0.0,
        updated_at: 0.0,
    };
    store.app_effect_stage(&row, &authority).unwrap();
    use sha2::Digest as _;
    let approval = format!("apv-{:x}", sha2::Sha256::digest(tag.as_bytes()))[..36].to_owned();
    store
        .social_publish_freeze_from_artifact(&crate::store::social_publish::FreezeFromArtifact {
            request_id: &format!("publish-{tag}"),
            install_id: INSTALL,
            context_id: None,
            run_id: &run_id,
            artifact_id: &artifact,
            bundle_digest: BUNDLE,
            slot: "publication",
            effect_id: &effect,
            destination_id: "dest-fb",
            toolkit: "facebook",
            aos_connection_id: "connA_fake_wire",
            media_key: None,
            grant_id: "dpq_fake_grant",
            approval_id: &approval,
            due_epoch: due,
            timezone: "UTC",
        })
        .unwrap()["intent"]
        .clone()
}

fn rig() -> (tempfile::TempDir, Arc<Door>, Arc<AtomicI64>) {
    let dir = tempfile::Builder::new().prefix("c1020-").tempdir().unwrap();
    (
        dir,
        Arc::new(Door::default()),
        Arc::new(AtomicI64::new(wall())),
    )
}

/// Outcome: a due, approved intent is published by the daemon on its own —
/// no operator press — exactly once, and reads posted.
#[test]
fn cad1020_due_approved_intent_publishes_once_with_no_operator() {
    let (dir, door, clock) = rig();
    let daemon = Daemon::start(dir.path(), &door, &clock);
    let intent = approved_intent(
        &daemon.shared.store,
        "due",
        clock.load(Ordering::SeqCst) - 1,
    );
    daemon.wait_state(&intent, "posted");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(door.sent(), vec![key(&intent)], "published exactly once");
}

/// Forbidden harm: two daemons on one store racing the same due intent
/// publish it twice. Both stage it together; one claim wins.
#[test]
fn cad1020_two_daemons_racing_publish_once() {
    let (dir, door, clock) = rig();
    *door.meet.0.lock().unwrap() = (2, 0);
    let first = Daemon::start(dir.path(), &door, &clock);
    let second = Daemon::start(dir.path(), &door, &clock);
    let intent = approved_intent(
        &first.shared.store,
        "race",
        clock.load(Ordering::SeqCst) - 1,
    );
    let until = Instant::now() + Duration::from_secs(20);
    while door.sent().is_empty() {
        assert!(Instant::now() < until, "nothing was published");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        door.preflights.lock().unwrap().len() >= 2,
        "both daemons staged it"
    );
    assert_eq!(door.sent(), vec![key(&intent)], "published exactly once");
    first.wait_state(&intent, "posted");
    assert_eq!(second.state(&intent), "posted");
}

/// Forbidden harm: the daemon dies after the door accepted the post but
/// before it learned so; the next daemon re-sends. It must reconcile.
#[test]
fn cad1020_crash_after_door_accepts_reconciles_without_resend() {
    let (dir, door, clock) = rig();
    door.lose_reply.store(true, Ordering::SeqCst);
    let mut crashed = Daemon::start(dir.path(), &door, &clock);
    let closing = crashed.shared.clone();
    *door.after_accept.lock().unwrap() = Some(Box::new(move || {
        closing.closing.store(true, Ordering::SeqCst);
    }));
    let intent = approved_intent(
        &crashed.shared.store,
        "crash",
        clock.load(Ordering::SeqCst) - 1,
    );
    crashed.wait_state(&intent, "processing");
    crashed.stop();
    door.lose_reply.store(false, Ordering::SeqCst);
    *door.after_accept.lock().unwrap() = None;
    let restarted = Daemon::start(dir.path(), &door, &clock);
    restarted.wait_state(&intent, "posted");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(door.sent(), vec![key(&intent)], "never re-sent");
}

/// Outcome: a revoked grant (a refused staging) publishes nothing and the
/// intent says why; an uncertain staging publishes nothing and the
/// intent stays queued for a later try.
#[test]
fn cad1020_refused_or_uncertain_preflight_publishes_nothing() {
    let (dir, door, clock) = rig();
    let daemon = Daemon::start(dir.path(), &door, &clock);
    let store = &daemon.shared.store;
    // Scheduled ahead, scripted at the door, then the clock reaches them.
    let now = clock.load(Ordering::SeqCst);
    let revoked = approved_intent(store, "revoked", now + 10);
    let flaky = approved_intent(store, "flaky", now + 10);
    door.refused.lock().unwrap().insert(
        key(&revoked),
        Refusal::new("grant_revoked", "the send grant was revoked"),
    );
    door.uncertain.lock().unwrap().push(key(&flaky));
    clock.store(now + 20, Ordering::SeqCst);
    let ended = daemon.wait_state(&revoked, "refused");
    let why = ended["receipt"]["error"].as_str().unwrap_or("");
    assert!(
        why.contains("grant_revoked"),
        "the intent names the refusal: {ended}"
    );
    let until = Instant::now() + Duration::from_secs(20);
    while !door.preflights.lock().unwrap().contains(&key(&flaky)) {
        assert!(Instant::now() < until, "the flaky intent was never staged");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(daemon.state(&flaky), "queued");
    assert!(door.sent().is_empty(), "nothing was published");
}

/// Forbidden harm: an intent sent before it is due, or after the operator
/// cancelled it. Later intents publishing prove the driver kept ticking.
#[test]
fn cad1020_not_due_or_cancelled_intent_is_never_sent() {
    let (dir, door, clock) = rig();
    let daemon = Daemon::start(dir.path(), &door, &clock);
    let store = &daemon.shared.store;
    let now = clock.load(Ordering::SeqCst);
    let later = approved_intent(store, "later", now + 600);
    let cancelled = approved_intent(store, "cancelled", now + 30);
    store
        .social_publish_cancel(id(&cancelled), INSTALL, None)
        .unwrap();
    let witness = approved_intent(store, "witness", now - 1);
    let next = approved_intent(store, "next", now + 40);
    daemon.wait_state(&witness, "posted");
    clock.store(now + 60, Ordering::SeqCst);
    daemon.wait_state(&next, "posted");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(daemon.state(&later), "queued");
    assert_eq!(daemon.state(&cancelled), "cancelled");
    assert_eq!(door.sent(), vec![key(&witness), key(&next)]);
}

/// Forbidden harm: a post published well after its window. The bound is
/// judged when the row is reached, after slow door calls earlier in the
/// same tick, not when the tick started.
#[test]
fn cad1020_overdue_intent_is_held_not_sent() {
    let (dir, door, clock) = rig();
    let daemon = Daemon::open(dir.path(), &door, &clock, |_| {});
    let store = &daemon.shared.store;
    let now = clock.load(Ordering::SeqCst);
    // A predecessor claimed this one and died before sending; reading its
    // status from the door takes 800 s.
    let stuck = approved_intent(store, "stuck", now - 1);
    store
        .social_publish_claim_id(id(&stuck), INSTALL, None)
        .unwrap()
        .unwrap();
    let late = approved_intent(store, "late", now - 200);
    let slow = clock.clone();
    *door.on_status.lock().unwrap() = Some(Box::new(move || {
        slow.fetch_add(800, Ordering::SeqCst);
    }));
    let daemon = daemon.run();
    assert_eq!(daemon.settled(&late), "held", "1000 s late: held, not sent");
    daemon.ticks(2);
    assert!(door.sent().is_empty(), "nothing was published");
}
