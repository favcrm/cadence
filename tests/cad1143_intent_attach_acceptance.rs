//! CAD-1143 prepared-intent attach guard acceptance — written by the
//! INDEPENDENT acceptance author (cc13-pi-acc793), never the host
//! implementer (social-host-ca8ff), who may not edit or weaken this file.
//!
//! Source-verified against the current host handlers: queue verifier
//! `src/daemon/social_publish_queue.rs` (typed Ed25519
//! `social.queue-validation.v1` receipt), prepare/attach handlers in
//! `src/daemon/social_publish_start.rs`, strict fields and
//! `operator_connection` gate in `src/daemon/social_publish_rpc.rs`, the
//! configured owner URL in `src/ui/social_publish.rs`, and atomic
//! `social_publish_attach_intent` / `SCHEMA_PREPARED` in
//! `src/store/social_publish.rs`. PREPARED rows are separate from
//! `social_publish_intents`, so only an authorized handoff enters the
//! queued-only due claim. The owner-read purpose has its own acceptance in
//! `tests/cad1143_owner_intent_read_acceptance.rs`; a READ assertion is not
//! attach authority.
//!
//! The policy-seam case is deliberately NOT described as real operator
//! proof: this in-band test explicitly asserts `Operator` only to exercise
//! the handler after its caller gate. It also asserts a named `Agent` is
//! refused. The separate `examples/cad1143_operator_control.rs` is the only
//! genuine process ancestry control; it calls prepare/attach unscoped from
//! an attached operator-origin process, without claiming a successful attach.
//!
//! Cases:
//!   1. A completed, independently reviewed run is prepared under the
//!      explicit operator policy seam; the response is `prepared`, while the
//!      dispatch queue and authorized-event stream remain empty.
//!   2. A named Agent policy assertion cannot prepare or attach.
//!   3. Same request/run retries the same prepared identity; request reuse
//!      for another run refuses.
//!   4. A caller-supplied grant or read-purpose field is refused by the
//!      closed attach schema. The cryptographic path refuses forged-signature,
//!      READ-purpose, replayed, wrong-scope/material, expired and
//!      upstream-uncertain receipts without changing PREPARED/queue state.
//!   5. A locally signed exact 26-claim queue receipt attaches a scheduled
//!      intent once; a retry returns the same queued row, while a future due
//!      time is not claimed early.
//!   6. The prepare/attach RPCs and HTTP relays refuse omitted required
//!      fields; HTTP also refuses forged frame fields and non-operator callers,
//!      then attaches a valid signed receipt exactly once. Prepare requires
//!      explicit `PublicBoard.company_slug`, validates it with the AOS slug
//!      grammar, and crosschecks the exact configured production host; missing
//!      or invalid slugs never fall back to host, workspace id, or frame. It
//!      uses the configured authorize/app origin, stripping path/query.
//!      Invalid host/slug/origin refuses before prepare.
//!      The real driver, claim RPC (even with a forged future now_epoch) and
//!      send-now path refuse dispatch
//!      until five seconds after queue commit; cancel remains available before then.
//!   7. Account-affinity settings require a fresh destination row, derive its
//!      label/connection, drop the prior account's grant, preserve only the
//!      selected account's grant on a timezone refresh, reject stale binding
//!      revisions, and refuse prepare after the fresh row's connection changes.
//!      These are selector/CAS checks only: READ-visible discovery does not
//!      prove authenticated AOS read/send authority, local binding eligibility,
//!      or a shared workspace, and the seeded grant is not send authority.
//!      All resolver and sender peers are local fixtures: no AOS server,
//!      provider, or external dispatch is invoked. Direct prepared-row reads
//!      use SQLite read-only mode because no public prepared-state RPC exists.
//!      The literal operator control separately proves genuine caller ancestry
//!      and is not replaced by this policy seam.
//!   8. Owner status/cancel RPC and HTTP routes require explicit null context,
//!      closed request fields and the operator gate. Status performs a fresh
//!      advisory inspection without consuming its JTI or returning receipt /
//!      grant material; attach then performs its own inspection and consumes
//!      its own JTI. Pending/not-found/ambiguous/definite-refusal cases retain
//!      their separate status classifications. Cancellation stays local and
//!      terminal; it does not imply remote deletion.
//!   9. A test-only synchronization seam holds the real attach transaction
//!      before commit and again after commit but before arm. The due driver and
//!      both claim paths refuse the persisted unarmed row; cancellation wins
//!      over late arm/attach. On a separate row the post-commit trusted-clock
//!      sample establishes the exact +5 deadline, +4 loses, +5 wins once, and
//!      immutable frozen material is unchanged. The fake sender never reaches
//!      an AOS server or provider.
//!
//! SOURCE ONLY: this acceptance has not been built or run. The sole validator
//! owns compile/check execution. The in-band policy seam is not genuine
//! operator ancestry, the attach-boundary hook only synchronizes test threads,
//! and the fake sender is not evidence of AOS runtime or provider behavior.
#![cfg(feature = "test-seam")]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cadence_agent::platform::agenticos_external::media_import::MediaResolver;
use cadence_agent::platform::agenticos_external::publish::{
    LedgerOutcome, Preflight, PublishSender, QueueValidationRequest, QueueValidationResponse,
    Refusal, SendBinding,
};
use cadence_agent::platform::agenticos_external::publish_sender::DeviceCredential;
use cadence_agent::store::{Store, PLATFORM_STREAM};
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon};
use ring::signature::{Ed25519KeyPair, KeyPair};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

const NOW: i64 = 1_800_000_000;
const COMPANY: &str = "workspace_cad1143_attach";
const HOST: &str = "native-owner-company.cadencecloud.app";
const KID: &str = "cad1143-queue-acceptance";
const QUEUE_PURPOSE: &str = "social.queue-validation.v1";
const READ_PURPOSE: &str = "social.intent.read.v1";
const OWNER_APP_ORIGIN: &str = "https://portal.cad1143.example";
const OWNER_AUTHORIZE_URL: &str =
    "https://portal.cad1143.example/v2/board/authorize?return=discard-me";
const OWNER_COMPANY_SLUG: &str = "native-owner-company";

#[derive(Clone, Copy)]
enum ReceiptMode {
    Valid,
    ForgedSignature,
    ReadPurpose,
    Replayed,
    WrongScope,
    WrongMaterial,
    Expired,
    UpstreamUncertain,
    OwnerActionPending,
    OwnerActionNotFound,
    OwnerActionAmbiguous,
}

/// A local signer and response peer exercise the real host parser, JWKS
/// fetch, Ed25519 verifier, exact-scope comparison and JTI ledger. It is not
/// an AOS server and it never calls a provider.
struct QueueHarness {
    signer: Mutex<Ed25519KeyPair>,
    issuer: String,
    host: String,
    workspace: String,
    replay_jti: String,
    clock: Arc<AtomicI64>,
    owner_intent: Mutex<Option<Value>>,
    mode: Mutex<ReceiptMode>,
    requests: Mutex<Vec<QueueValidationRequest>>,
    signed_claims: Mutex<Vec<Value>>,
    executed_bindings: Mutex<Vec<SendBinding>>,
    inspect_calls: AtomicUsize,
    preflight_calls: AtomicUsize,
    execute_calls: AtomicUsize,
    status_calls: AtomicUsize,
}

impl QueueHarness {
    fn set_case(&self, owner_intent: Value, mode: ReceiptMode) {
        *self.owner_intent.lock().unwrap() = Some(owner_intent);
        *self.mode.lock().unwrap() = mode;
    }

    fn compact(&self, claims: &Value, forge_signature: bool) -> String {
        let header = json!({"alg":"EdDSA", "typ":"JWT", "kid":KID});
        let encoded_header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
        let encoded_claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
        let signing_input = format!("{encoded_header}.{encoded_claims}");
        let mut signature = self
            .signer
            .lock()
            .unwrap()
            .sign(signing_input.as_bytes())
            .as_ref()
            .to_vec();
        if forge_signature {
            signature[0] ^= 1;
        }
        format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature))
    }
}

impl PublishSender for QueueHarness {
    fn inspect_queue(
        &self,
        request: &QueueValidationRequest,
    ) -> std::result::Result<QueueValidationResponse, Refusal> {
        self.inspect_calls.fetch_add(1, SeqCst);
        self.requests.lock().unwrap().push(request.clone());
        let mode = *self.mode.lock().unwrap();
        match mode {
            ReceiptMode::OwnerActionPending => {
                return Err(Refusal::new(
                    "owner_action_pending",
                    "the exact owner action is still pending",
                ));
            }
            ReceiptMode::OwnerActionNotFound => {
                return Err(Refusal::new(
                    "owner_action_not_found",
                    "the exact owner action is absent",
                ));
            }
            ReceiptMode::OwnerActionAmbiguous => {
                return Err(Refusal::new(
                    "owner_action_ambiguous",
                    "the exact owner action is ambiguous",
                ));
            }
            ReceiptMode::UpstreamUncertain => {
                return Err(Refusal::new(
                    "queue_validation_uncertain",
                    "local acceptance models an uncertain read; no receipt exists",
                ));
            }
            _ => {}
        }
        let owner = self
            .owner_intent
            .lock()
            .unwrap()
            .clone()
            .expect("test prepared its exact owner-intent descriptor");
        let jti = if matches!(mode, ReceiptMode::Replayed) {
            self.replay_jti.clone()
        } else {
            Uuid::new_v4().to_string()
        };
        let now = self.clock.load(SeqCst);
        let mut claims = json!({
            "iss": self.issuer,
            "aud": self.host,
            "purpose": QUEUE_PURPOSE,
            "workspace": self.workspace,
            "key": request.key,
            "action_id": "act_cad1143_attach",
            "grant_id": "dpq_cad1143_attach_acceptance",
            "cadence_run_id": request.cadence_run_id,
            "cadence_effect_id": request.cadence_effect_id,
            "intent_id": request.expected_intent_id,
            "intent_digest": request.expected_intent_digest,
            "connection_id": request.connection_id,
            "destination_id": owner["destination_id"],
            "toolkit": owner["toolkit"],
            "caption_digest": owner["caption_digest"],
            "image_digest": owner["image_digest"],
            "media_key": request.media_key,
            "cadence_approval_id": owner["cadence_approval_id"],
            "due_epoch": owner["due_epoch"],
            "not_before_ms": owner["not_before"].as_i64().unwrap() * 1000,
            "expires_at_ms": owner["expires_at"].as_i64().unwrap() * 1000,
            "grant_max_uses": 1,
            "grant_remaining_uses": 1,
            "iat": now,
            "exp": now + 15,
            "jti": jti,
        });
        assert_eq!(
            claims.as_object().unwrap().len(),
            26,
            "closed AOS queue receipt has exactly 26 claims"
        );
        assert!(claims.get("sub").is_none() && claims.get("checked_at").is_none());
        match mode {
            ReceiptMode::Valid
            | ReceiptMode::Replayed
            | ReceiptMode::UpstreamUncertain
            | ReceiptMode::OwnerActionPending
            | ReceiptMode::OwnerActionNotFound
            | ReceiptMode::OwnerActionAmbiguous => {}
            ReceiptMode::ForgedSignature => {}
            ReceiptMode::ReadPurpose => claims["purpose"] = json!(READ_PURPOSE),
            ReceiptMode::WrongScope => claims["connection_id"] = json!("connA_wrong_scope"),
            ReceiptMode::WrongMaterial => {
                let original = claims["caption_digest"].as_str().unwrap().to_owned();
                let replacement = if original == "f".repeat(64) {
                    "e".repeat(64)
                } else {
                    "f".repeat(64)
                };
                claims["caption_digest"] = json!(replacement);
            }
            ReceiptMode::Expired => {
                claims["iat"] = json!(now - 30);
                claims["exp"] = json!(now - 15);
            }
        }
        self.signed_claims.lock().unwrap().push(claims.clone());
        Ok(QueueValidationResponse {
            version: QUEUE_PURPOSE.to_owned(),
            receipt: self.compact(&claims, matches!(mode, ReceiptMode::ForgedSignature)),
        })
    }

    fn execute(&self, binding: &SendBinding) -> std::result::Result<LedgerOutcome, Refusal> {
        self.executed_bindings.lock().unwrap().push(binding.clone());
        self.execute_calls.fetch_add(1, SeqCst);
        Err(Refusal::new(
            "acceptance_no_provider",
            "provider calls are forbidden in this fixture",
        ))
    }

    fn status(&self, _key: &str) -> std::result::Result<LedgerOutcome, Refusal> {
        self.status_calls.fetch_add(1, SeqCst);
        Err(Refusal::new(
            "acceptance_no_provider",
            "provider calls are forbidden in this fixture",
        ))
    }

    fn preflight(&self, _binding: &SendBinding) -> Preflight {
        self.preflight_calls.fetch_add(1, SeqCst);
        Preflight::Approved
    }
}

/// A loopback JWKS endpoint for the daemon's real configured-key fetch.
struct JwksStub {
    addr: SocketAddr,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl JwksStub {
    fn start(body: String) -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let thread = std::thread::spawn(move || {
            for request in server.incoming_requests() {
                if request.url() == "/__stop" {
                    let _ = request.respond(tiny_http::Response::from_string("stopping"));
                    break;
                }
                let response = if request.url() == "/.well-known/agenticos-board-jwks.json" {
                    tiny_http::Response::from_string(body.clone())
                } else {
                    tiny_http::Response::from_string("{}").with_status_code(404)
                };
                let _ = request.respond(response);
            }
        });
        Self {
            addr,
            thread: Some(thread),
        }
    }

    fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for JwksStub {
    fn drop(&mut self) {
        if let Ok(mut stream) = TcpStream::connect(self.addr) {
            let _ = stream
                .write_all(b"GET /__stop HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One seam-armed daemon on a temp dir; the local resolver provides only the
/// fixture destination, while the separate queue peer signs test receipts.
struct Fx {
    root: tempfile::TempDir,
    clock: Arc<AtomicI64>,
    driver_clock: Arc<AtomicI64>,
    daemon: Option<(
        Arc<AtomicBool>,
        std::thread::JoinHandle<cadence_agent::Result<()>>,
    )>,
    store: Store,
    _jwks: JwksStub,
    sender: Arc<QueueHarness>,
    destinations: Arc<Mutex<Vec<Value>>>,
}

impl Fx {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("c1143attach")
            .tempdir()
            .unwrap();
        cadence_agent::issue::Pm::init(&root.path().join("pm")).unwrap();
        let clock = Arc::new(AtomicI64::new(NOW));
        let driver_clock = Arc::new(AtomicI64::new(NOW));
        let state_dir = root.path().join("s");
        std::fs::create_dir_all(&state_dir).unwrap();
        let store = Store::open(&state_dir.join("cadence.sqlite3")).unwrap();
        let signer = Ed25519KeyPair::from_seed_unchecked(&[0x42; 32]).unwrap();
        let jwks_body = json!({"keys":[{
            "kty":"OKP", "crv":"Ed25519", "kid":KID,
            "x":URL_SAFE_NO_PAD.encode(signer.public_key().as_ref()),
            "use":"sig", "alg":"EdDSA"
        }]})
        .to_string();
        let jwks = JwksStub::start(jwks_body);
        let issuer = jwks.origin();
        let replay_jti = Uuid::new_v4().to_string();
        let sender = Arc::new(QueueHarness {
            signer: Mutex::new(signer),
            issuer: issuer.clone(),
            host: HOST.to_owned(),
            workspace: COMPANY.to_owned(),
            replay_jti: replay_jti.clone(),
            clock: Arc::clone(&clock),
            owner_intent: Mutex::new(None),
            mode: Mutex::new(ReceiptMode::Valid),
            requests: Mutex::new(Vec::new()),
            signed_claims: Mutex::new(Vec::new()),
            executed_bindings: Mutex::new(Vec::new()),
            inspect_calls: AtomicUsize::new(0),
            preflight_calls: AtomicUsize::new(0),
            execute_calls: AtomicUsize::new(0),
            status_calls: AtomicUsize::new(0),
        });
        cadence_agent::board_identity::write_config(
            &state_dir,
            &cadence_agent::board_identity::Config {
                host: HOST.to_owned(),
                issuer,
                company: COMPANY.to_owned(),
            },
        )
        .unwrap();
        // Load one JTI before the daemon starts, just as if this already-seen
        // queue receipt had been consumed on an earlier request.
        let mut replay_ledger = cadence_agent::operator_auth::Auth::load(&state_dir);
        assert!(replay_ledger
            .consume_assertion_jti(&replay_jti, NOW + 15, NOW)
            .unwrap());
        let destinations = Arc::new(Mutex::new(vec![json!({
            "connectionId": "connA_harbour", "toolkit": "facebook",
            "displayName": "Harbour", "destinationId": "dest-01",
            "status": "active", "available": true, "publishable": true,
        })]));
        Self {
            root,
            clock,
            driver_clock,
            daemon: None,
            store,
            _jwks: jwks,
            sender,
            destinations,
        }
    }
    fn dir(&self) -> std::path::PathBuf {
        self.root.path().join("s")
    }
    fn auth_ledger(&self) -> String {
        std::fs::read_to_string(self.dir().join("operator/sessions.json"))
            .expect("fixture operator-auth ledger exists")
    }
    fn set_destinations(&self, rows: Vec<Value>) {
        *self.destinations.lock().unwrap() = rows;
    }
    fn pm(&self) -> std::path::PathBuf {
        self.root.path().join("pm")
    }
    fn start(&mut self) {
        self.start_with_sender(true, true);
    }
    fn start_driver(&mut self) {
        self.start_with_sender(true, false);
    }
    fn start_without_sender(&mut self) {
        self.start_with_sender(false, true);
    }
    fn start_with_sender(&mut self, configure_sender: bool, driver_off: bool) {
        let (dir, stop) = (self.dir(), Arc::new(AtomicBool::new(false)));
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", self.pm().to_str().unwrap());
        let operator_clock = Arc::clone(&self.clock);
        let driver_clock = Arc::clone(&self.driver_clock);
        let stub = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = stub.server_addr().to_string();
        let destinations = Arc::clone(&self.destinations);
        std::thread::spawn(move || {
            for request in stub.incoming_requests() {
                let rows = destinations.lock().unwrap().clone();
                let body =
                    json!({"ok": true, "data": {"version": "1", "destinations": rows}}).to_string();
                let _ = request.respond(tiny_http::Response::from_string(body));
            }
        });
        let resolver = MediaResolver::new(
            &format!("http://{addr}"),
            DeviceCredential::new("read-cred".into()),
        )
        .unwrap();
        let mut opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(Arc::clone(&stop)),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            operator_clock: Some(Arc::new(move || operator_clock.load(SeqCst))),
            social_media_resolver: Some(Arc::new(resolver)),
            social_publish_sender: configure_sender
                .then(|| self.sender.clone() as Arc<dyn PublishSender>),
            social_publish_driver_ms: Some(1),
            social_publish_driver_off: Some(driver_off),
            social_publish_driver_clock: Some(Arc::new(move || driver_clock.load(SeqCst))),
            ..Default::default()
        };
        cadence_agent::platform::local::register_at(
            &dir,
            &mut opts,
            dir.join("outbox"),
            "http://127.0.0.1:3119".into(),
        );
        let handle = std::thread::spawn(move || daemon::serve_with(&dir, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&self.dir(), "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&self.dir()).is_none()
        {
            assert!(
                !handle.is_finished() && std::time::Instant::now() < deadline,
                "daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        self.daemon = Some((stop, handle));
    }
    fn setup_rpc(&self, method: &str, params: Value) -> Value {
        scoped(Asserted::Operator, || {
            client::rpc(&self.dir(), method, params)
        })
        .unwrap_or_else(|e| panic!("setup {method}: {e}"))
    }
    /// Explicit policy-seam call. This is never a claim about real
    /// operator ancestry; positive actual-caller proof lives in the
    /// separate operator-origin example.
    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(who, || client::rpc(&self.dir(), method, params))
    }
    /// Install an app and produce one approved run (the real
    /// install→bind→approve chain) so `prepare` has live approved runs
    /// + a binding to derive from. All setup under one seam scope, dropped
    ///   before any guarded call.
    fn setup(&self) -> (String, String, String) {
        scoped(Asserted::Operator, || self.install_and_approve_scoped())
    }
    fn install_and_approve_scoped(&self) -> (String, String, String) {
        let store = &self.store;
        for (alias, role) in [("lead", "pm"), ("writer", "worker"), ("reviewer", "worker")] {
            if store.agent_opt(alias).unwrap().is_none() {
                store
                    .register_agent(&cadence_agent::store::NewAgent {
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
                store
                    .set_identity(
                        alias,
                        &cadence_agent::adapter::Identity {
                            thread_id: "t".into(),
                            session_id: "s".into(),
                            model: None,
                            effort: None,
                            pid: std::process::id(),
                            endpoint: None,
                            generation: Some("g1".into()),
                            attach: None,
                        },
                    )
                    .unwrap();
            }
        }
        let source = self.root.path().join("app-src-attach");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(
            source.join("app.md"),
            "---\napp: attach-accept\ntitle: Attach accept\nversion: '0.1.0'\n\
             summary: Prepared-intent fixture.\nneeds:\n  connections: []\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send\n---\n\n# Attach accept\n",
        )
        .unwrap();
        std::fs::write(source.join("workflows/brief.md"), WORKFLOW).unwrap();
        let installed = self.setup_rpc(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        );
        let install = installed["install_id"].as_str().unwrap().to_string();
        let bundle = installed["digest"].as_str().unwrap().to_string();
        self.setup_rpc(
            "app_install_team_set",
            json!({"install_id": install, "owner_pm": "lead",
                "roles": {"writer": "writer", "reviewer": "reviewer"},
                "expected_revision": 0}),
        );
        let bound = self.setup_rpc(
            "app_binding_create",
            json!({"install_id": install, "slot": "publication",
                "connection_id": self.local_connection(), "request_id": "bind-attach"}),
        );
        let binding = &bound["binding"];
        self.setup_rpc(
            "app_binding_publish_set",
            json!({"install_id": install, "binding_id": binding["id"],
                "expected_revision": binding["revision"],
                "destination_id": "dest-01", "destination_label": "Harbour",
                "toolkit": "facebook", "timezone": "Asia/Hong_Kong",
                "grant_id": "dpq_acceptance_fixture_grant"}),
        );
        let run_id = self.complete_approved_run(&install, &bundle, "run-attach-01");
        let other_run_id = self.complete_approved_run(&install, &bundle, "run-attach-02");
        (install, run_id, other_run_id)
    }
    /// Complete a reviewed source run through the store's real turn path;
    /// the deterministic fixture reply exercises approval without a provider.
    fn complete_approved_run(&self, install: &str, bundle: &str, request_id: &str) -> String {
        let store = &self.store;
        let started = self.setup_rpc(
            "app_run_start",
            json!({"install_id": install, "workflow": "brief", "request_id": request_id,
                "expected_quotes": {},
                "inputs": {"subject": "Reviewed update", "source": "Source facts."}}),
        );
        let run_id = started["id"].as_str().unwrap().to_string();
        let finish_step = |run: &Value, step: usize, reply: Value| {
            let message = run["steps"][step]["message_id"]
                .as_str()
                .unwrap()
                .to_string();
            let token = cadence_agent::adapter::registry::CLAUDE_MANAGED_TURN_TOKENS.mint("g1");
            store.mark_running(&message, &token).unwrap();
            let msg = store.message(&message).unwrap().unwrap();
            let reply = json!({"turn_id": msg.turn_id, "text": reply.to_string()});
            store.finish(&msg, "completed", &reply, None).unwrap();
        };
        let run = store.app_run_dispatch(&run_id, bundle).unwrap();
        finish_step(
            &run,
            0,
            json!({"schema":1,"kind":"produce_text","run_id":run_id,"step_id":"s1","revision":1,
                "outcome":"succeeded","artifacts":[{"media_type":"text/markdown","text":CAPTION}]}),
        );
        let run = store.app_run_dispatch(&run_id, bundle).unwrap();
        let artifact_digest = cadence_agent::store::app_runs::artifact_digest(CAPTION.as_bytes());
        finish_step(
            &run,
            1,
            json!({"schema":1,"kind":"review_text","run_id":run_id,"step_id":"s2","revision":1,
                "producer_step_id":"s1","producer_revision":1,"artifact_sha256":artifact_digest,
                "decision":"approve","rationale":"Reviewed the exact artifact."}),
        );
        let shown = self.setup_rpc("app_run_show", json!({"run_id": run_id}));
        assert_eq!(shown["state"], "succeeded", "approved fixture run: {shown}");
        run_id
    }
    fn local_connection(&self) -> String {
        let rows = self.setup_rpc("connection_list", json!({}))["connections"].clone();
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["provider"] == "local" && row["account"] == "local")
            .expect("local builtin connection")["id"]
            .as_str()
            .unwrap()
            .into()
    }
    /// Count queued send intents under this install — the queue a
    /// PREPARED row must never land in. Reads the real public
    /// `social_publish_list` (the `social_publish_intents` table); the
    /// prepared table is a separate store the queue never selects from.
    fn publish_queue(&self, install: &str) -> Value {
        self.store
            .social_publish_list(Some(install), None)
            .unwrap_or_else(|error| panic!("read publish queue: {error}"))
    }
    fn queued_intent_count(&self, install: &str) -> usize {
        self.publish_queue(install)["intents"]
            .as_array()
            .map(|intents| {
                intents
                    .iter()
                    .filter(|intent| intent["state"] == json!("queued"))
                    .count()
            })
            .unwrap_or(0)
    }
    fn social_publish_event_count(&self) -> usize {
        self.store
            .events_tail(PLATFORM_STREAM, 500)
            .unwrap_or_default()
            .iter()
            .filter(|event| event.kind.starts_with("social_publish_"))
            .count()
    }
    /// Whether the prepared-intent lifecycle emitted an AUTHORIZED event
    /// on the platform stream — `social_publish_prepared` is written at
    /// prepare, `social_publish_authorized` ONLY on the real CAS. An
    /// unverified grant must never produce the authorized event.
    fn authorized_event_count(&self) -> usize {
        self.store
            .events_tail(PLATFORM_STREAM, 200)
            .unwrap_or_default()
            .iter()
            .filter(|e| {
                e.kind == cadence_agent::store::social_publish::SOCIAL_PUBLISH_AUTHORIZED_EVENT
            })
            .count()
    }
    /// The prepared table is private and has no public show/list RPC. Read it
    /// directly in SQLite read-only mode to prove failed receipts left the
    /// lifecycle state and grant column unchanged.
    fn prepared_rows(&self, install: &str) -> Vec<(String, String, Option<String>, String)> {
        let conn = Connection::open_with_flags(
            self.dir().join("cadence.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut statement = conn
            .prepare("SELECT prepared_id,state,grant_id,descriptor_digest FROM social_publish_prepared WHERE install_id=?1 ORDER BY prepared_id")
            .unwrap();
        statement
            .query_map([install], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        if let Some((stop, handle)) = self.daemon.take() {
            stop.store(true, SeqCst);
            let _ = handle.join();
        }
    }
}

fn owner_public_board(
    state_dir: &std::path::Path,
    company_slug: Option<&str>,
    authorize_url: &str,
) -> cadence_agent::ui::PublicBoard {
    let identity = cadence_agent::board_identity::read_config(state_dir)
        .expect("fixture board identity is configured");
    cadence_agent::ui::PublicBoard {
        host: identity.host,
        issuer: identity.issuer,
        company: identity.company,
        company_slug: company_slug.map(str::to_owned),
        authorize_url: authorize_url.to_owned(),
    }
}

fn owner_public_board_at(
    state_dir: &std::path::Path,
    host: &str,
    company_slug: Option<&str>,
    authorize_url: &str,
) -> cadence_agent::ui::PublicBoard {
    let mut public = owner_public_board(state_dir, company_slug, authorize_url);
    public.host = host.to_owned();
    public
}

fn board_http_post(
    dir: &std::path::Path,
    pm: &std::path::Path,
    who: &str,
    path: &str,
    body: &str,
) -> (u16, String) {
    board_http_post_with_public(
        dir,
        pm,
        who,
        path,
        body,
        Some(owner_public_board(
            dir,
            Some(OWNER_COMPANY_SLUG),
            OWNER_AUTHORIZE_URL,
        )),
    )
}

fn board_http_post_with_public(
    dir: &std::path::Path,
    pm: &std::path::Path,
    who: &str,
    path: &str,
    body: &str,
    public: Option<cadence_agent::ui::PublicBoard>,
) -> (u16, String) {
    let free = |port: &u16| std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok();
    let port = (3110..3200).find(free).expect("isolated board port");
    let host = format!("cadence-{port}.localhost:{port}");
    let (startup, ready) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let opts = cadence_agent::ui::ServeOpts {
        host: "127.0.0.1".into(),
        port,
        stop: Some(Arc::clone(&stop)),
        startup: Some(startup),
        public,
        test_seam: true,
        ..Default::default()
    };
    let (dir, pm) = (dir.to_path_buf(), pm.to_path_buf());
    let board_dir = dir.clone();
    let board = std::thread::spawn(move || cadence_agent::ui::serve(&board_dir, &pm, &opts));
    ready
        .recv_timeout(Duration::from_secs(10))
        .expect("board startup signal")
        .expect("board startup");
    let token = Seam::token_at(dir.as_path()).expect("fixture daemon minted its seam token");
    let request = format!(
        "POST {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\n\
         X-Cadence-Board: 1\r\nSec-Fetch-Site: same-origin\r\nOrigin: http://{host}\r\n\
         {}: {who}\r\n{}: {token}\r\nContent-Length: {}\r\n\r\n{body}",
        cadence_agent::test_seam::AS_HEADER,
        cadence_agent::test_seam::TOKEN_HEADER,
        body.len()
    );
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect fixture board");
    stream
        .write_all(request.as_bytes())
        .expect("write fixture request");
    let mut response = String::new();
    std::io::Read::read_to_string(&mut stream, &mut response).expect("read fixture response");
    stop.store(true, SeqCst);
    let _ = board.join();
    let status = response
        .split_whitespace()
        .nth(1)
        .expect("HTTP status")
        .parse()
        .expect("numeric HTTP status");
    (status, response)
}

fn http_response_body(response: &str) -> &str {
    response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("HTTP response body separator")
}

fn assert_no_authority_fields(value: &Value) {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                let key = key.to_ascii_lowercase();
                assert!(
                    ![
                        "receipt",
                        "grant",
                        "jws",
                        "nonce",
                        "token",
                        "descriptor",
                        "signature",
                        "jti",
                        "assertion",
                    ]
                    .iter()
                    .any(|forbidden| key.contains(*forbidden)),
                    "owner status/cancel leaked authority field {key}"
                );
                assert_no_authority_fields(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                assert_no_authority_fields(item);
            }
        }
        _ => {}
    }
}

fn queue_row_for_request(fx: &Fx, install: &str, request: &str) -> Value {
    fx.publish_queue(install)["intents"]
        .as_array()
        .expect("publish queue intents")
        .iter()
        .find(|row| row["request"] == request)
        .cloned()
        .unwrap_or_else(|| panic!("no publish queue row for {request}"))
}

fn driver_last_tick(fx: &Fx, install: &str) -> f64 {
    let list = fx
        .rpc(
            Asserted::Operator,
            "social_publish_list",
            json!({"install_id":install}),
        )
        .unwrap_or_else(|error| panic!("read social-publish driver status: {error}"));
    list["driver"]["last_tick"].as_f64().unwrap_or(0.0)
}

fn wait_for_driver_tick_after(fx: &Fx, install: &str, previous: f64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        if driver_last_tick(fx, install) > previous {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("social-publish driver did not complete a tick while attach was held");
}

const WORKFLOW: &str = r#"---
title: "Attach brief"
goal: "One reviewed brief"
inputs:
  writer: { ask: "writer" }
  reviewer: { ask: "reviewer" }
  subject: { ask: "subject" }
  source: { ask: "facts" }
---

## Write
agent: {{writer}}
size: S
action: local.text.produce

Write one brief about {{subject}} grounded only in {{source}}.

### Acceptance
- [ ] brief exists

## Review
agent: {{reviewer}}
size: S
depends_on: 1
action: local.text.review

Review the artifact.

### Acceptance
- [ ] reviewed
"#;

const CAPTION: &str = "# Acceptance brief\nReviewed copy.";

fn refuse_receipt_case(
    fx: &Fx,
    install: &str,
    run_id: &str,
    request_id: &str,
    mode: ReceiptMode,
    expected_error: &str,
) {
    let prepared = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_prepare",
            json!({"request_id":request_id,"run_id":run_id,"mode":"now"}),
        )
        .unwrap_or_else(|error| panic!("prepare {request_id}: {error}"));
    let prepared_id = prepared["prepared"]["prepared_id"].as_str().unwrap();
    let owner_intent = prepared["prepared"]["owner_intent"].clone();
    fx.sender.set_case(owner_intent, mode);
    let rows_before = fx.prepared_rows(install);
    let queue_before = fx.publish_queue(install);
    let social_events_before = fx.social_publish_event_count();
    let authorized_before = fx.authorized_event_count();
    let inspect_before = fx.sender.inspect_calls.load(SeqCst);
    let preflight_before = fx.sender.preflight_calls.load(SeqCst);
    let execute_before = fx.sender.execute_calls.load(SeqCst);
    let status_before = fx.sender.status_calls.load(SeqCst);

    let result = fx.rpc(
        Asserted::Operator,
        "app_publish_intent_attach",
        json!({"prepared_id":prepared_id,"install_id":install}),
    );
    let error = format!(
        "{:?}",
        result.expect_err("invalid queue receipt unexpectedly attached")
    );
    assert!(
        error.contains(expected_error),
        "expected queue receipt refusal {expected_error:?}, got {error}"
    );
    assert_eq!(
        fx.prepared_rows(install),
        rows_before,
        "refusal changed PREPARED state or grant"
    );
    assert_eq!(
        fx.publish_queue(install),
        queue_before,
        "refusal changed the dispatchable queue"
    );
    assert_eq!(
        fx.social_publish_event_count(),
        social_events_before,
        "refusal emitted a publish event"
    );
    assert_eq!(
        fx.authorized_event_count(),
        authorized_before,
        "refusal emitted an authorized event"
    );
    assert_eq!(fx.sender.inspect_calls.load(SeqCst), inspect_before + 1);
    assert_eq!(fx.sender.preflight_calls.load(SeqCst), preflight_before);
    assert_eq!(fx.sender.execute_calls.load(SeqCst), execute_before);
    assert_eq!(fx.sender.status_calls.load(SeqCst), status_before);
}

/// Missing upstream configuration must fail closed before attach effects.
#[test]
fn attach_without_configured_aos_sender_refuses_without_effects() {
    let mut fx = Fx::new();
    fx.start_without_sender();
    let (install, run_id, _) = fx.setup();
    let prepared = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_prepare",
            json!({"request_id":"prep-no-sender-01","run_id":run_id,"mode":"now"}),
        )
        .expect("fixture prepare must remain available without an AOS publish sender");
    let prepared_id = prepared["prepared"]["prepared_id"].as_str().unwrap();
    assert_eq!(prepared["prepared"]["state"], json!("prepared"));
    let prepared_before = fx.prepared_rows(&install);
    let queue_before = fx.publish_queue(&install);
    let events_before = fx.social_publish_event_count();
    let authorized_before = fx.authorized_event_count();

    let result = fx.rpc(
        Asserted::Operator,
        "app_publish_intent_attach",
        json!({"prepared_id":prepared_id,"install_id":install}),
    );
    let error = format!(
        "{:?}",
        result.expect_err("attach succeeded without an AOS sender")
    );
    assert!(
        error.contains("capability_unavailable: no AOS publish sender is configured"),
        "unexpected no-sender refusal: {error}"
    );
    assert_eq!(
        fx.prepared_rows(&install),
        prepared_before,
        "no-sender refusal changed PREPARED state"
    );
    assert_eq!(
        fx.publish_queue(&install),
        queue_before,
        "no-sender refusal queued an intent"
    );
    assert_eq!(
        fx.social_publish_event_count(),
        events_before,
        "no-sender refusal emitted a publish event"
    );
    assert_eq!(
        fx.authorized_event_count(),
        authorized_before,
        "no-sender refusal authorized a send"
    );
    assert_eq!(
        fx.sender.inspect_calls.load(SeqCst),
        0,
        "missing sender cannot inspect upstream"
    );
    assert_eq!(fx.sender.preflight_calls.load(SeqCst), 0);
    assert_eq!(fx.sender.execute_calls.load(SeqCst), 0);
    assert_eq!(fx.sender.status_calls.load(SeqCst), 0);
}

/// CAD-1143 policy-seam acceptance: explicit seam identities exercise the
/// real handler after its caller gate, independent of ambient CI/pane
/// ancestry. The separate literal operator-origin control remains the only
/// process-ancestry proof and is not replaced by this test.
#[test]
fn prepared_intent_requires_a_current_exact_queue_receipt_and_attaches_once() {
    let mut fx = Fx::new();
    fx.start();
    let (install, run_id, other_run_id) = fx.setup();
    let authorized0 = fx.authorized_event_count();
    assert_eq!(
        fx.queued_intent_count(&install),
        0,
        "fixture must start send-free"
    );

    let prepare = json!({"request_id":"prep-guard-01","run_id":run_id,"mode":"now"});
    let rows0 = fx.prepared_rows(&install);
    let agent_prepare = fx.rpc(
        Asserted::Agent("writer".into()),
        "app_publish_intent_prepare",
        prepare.clone(),
    );
    let agent_prepare_error = format!("{:?}", agent_prepare.unwrap_err());
    assert!(
        agent_prepare_error.contains("operator action"),
        "prepare must refuse at the operator-connection guard: {agent_prepare_error}"
    );
    assert_eq!(
        fx.prepared_rows(&install),
        rows0,
        "denied Agent prepare wrote a row"
    );
    assert_eq!(fx.queued_intent_count(&install), 0);
    assert_eq!(fx.authorized_event_count(), authorized0);

    // POLICY-SEAM CONTROL ONLY: explicit Operator asserts the test seam; this
    // is not evidence of actual process ancestry or a real operator action.
    let prepared = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_prepare",
            prepare.clone(),
        )
        .unwrap_or_else(|error| panic!("policy-seam prepare refused: {error}"));
    let prepared_id = prepared["prepared"]["prepared_id"].as_str().unwrap();
    assert_eq!(prepared["prepared"]["state"], json!("prepared"));
    assert_eq!(
        fx.queued_intent_count(&install),
        0,
        "prepare must never queue a send"
    );
    assert_eq!(
        fx.authorized_event_count(),
        authorized0,
        "prepare must not authorize a send"
    );
    let retry = fx
        .rpc(Asserted::Operator, "app_publish_intent_prepare", prepare)
        .unwrap();
    assert_eq!(retry["prepared"]["prepared_id"], json!(prepared_id));
    let reused = fx.rpc(
        Asserted::Operator,
        "app_publish_intent_prepare",
        json!({"request_id":"prep-guard-01","run_id":other_run_id,"mode":"now"}),
    );
    let reused_error = format!("{:?}", reused.unwrap_err());
    assert!(reused_error.contains("publish prepare request already names different"));

    let attach_params = json!({"prepared_id":prepared_id,"install_id":install});
    let agent_attach = fx.rpc(
        Asserted::Agent("writer".into()),
        "app_publish_intent_attach",
        attach_params.clone(),
    );
    let agent_attach_error = format!("{:?}", agent_attach.unwrap_err());
    assert!(
        agent_attach_error.contains("operator action"),
        "attach must refuse at the operator-connection guard: {agent_attach_error}"
    );
    let inspect_before_forged_body = fx.sender.inspect_calls.load(SeqCst);
    for forged in [
        json!({"prepared_id":prepared_id,"install_id":install,"grant_id":"dpq_forged_wellformed"}),
        json!({"prepared_id":prepared_id,"install_id":install,"assertion":"social.intent.read.v1","purpose":"social.intent.read.v1"}),
    ] {
        let error = format!(
            "{:?}",
            fx.rpc(Asserted::Operator, "app_publish_intent_attach", forged)
                .unwrap_err()
        );
        assert!(
            error.contains("unsupported fields"),
            "closed attach schema: {error}"
        );
    }
    assert_eq!(
        fx.sender.inspect_calls.load(SeqCst),
        inspect_before_forged_body,
        "caller-supplied grant/READ fields must be rejected before upstream inspection"
    );

    // Correctly signed but invalid receipts reach the cryptographic verifier;
    // every refusal leaves the row PREPARED, grant-free and absent from the
    // due queue. The JTI replay case is pre-seeded through Auth before daemon
    // startup, so this is the exact real durable consume guard.
    for (index, mode, expected) in [
        (
            "forged",
            ReceiptMode::ForgedSignature,
            "Ed25519 signature or configured kid does not verify",
        ),
        (
            "read-purpose",
            ReceiptMode::ReadPurpose,
            "issuer, audience, workspace, or purpose mismatch",
        ),
        (
            "replayed",
            ReceiptMode::Replayed,
            "social queue-validation receipt jti was already used",
        ),
        (
            "wrong-scope",
            ReceiptMode::WrongScope,
            "signed receipt does not match the exact prepared scope/window",
        ),
        (
            "wrong-material",
            ReceiptMode::WrongMaterial,
            "signed receipt does not match the exact prepared scope/window",
        ),
        (
            "expired",
            ReceiptMode::Expired,
            "iat/exp are outside the 15-second receipt window",
        ),
        (
            "upstream-uncertain",
            ReceiptMode::UpstreamUncertain,
            "queue_validation_uncertain",
        ),
    ] {
        refuse_receipt_case(
            &fx,
            &install,
            &run_id,
            &format!("prep-{index}"),
            mode,
            expected,
        );
    }
    assert_eq!(
        fx.queued_intent_count(&install),
        0,
        "bad receipts never queue"
    );
    assert_eq!(
        fx.authorized_event_count(),
        authorized0,
        "bad receipts never authorize"
    );

    // A separate future scheduled intent can be inspected and attached, but
    // attach is not dispatch. The host's queued-only due query returns no
    // claim before not_before; the AOS device preflight/server behavior is a
    // separate contract and is not inferred from this local sender.
    let future_due = NOW + 60;
    let scheduled_request = json!({
        "request_id":"prep-scheduled-01",
        "run_id":run_id,
        "mode":"schedule",
        "due_epoch":future_due
    });
    let scheduled = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_prepare",
            scheduled_request.clone(),
        )
        .unwrap_or_else(|error| panic!("scheduled prepare refused: {error}"));
    let scheduled_id = scheduled["prepared"]["prepared_id"].as_str().unwrap();
    assert_eq!(
        scheduled["prepared"]["owner_intent"]["not_before"],
        json!(future_due)
    );
    assert_eq!(scheduled["prepared"]["state"], json!("prepared"));
    let scheduled_retry = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_prepare",
            scheduled_request,
        )
        .unwrap();
    assert_eq!(
        scheduled_retry["prepared"]["prepared_id"],
        json!(scheduled_id)
    );
    fx.sender.set_case(
        scheduled["prepared"]["owner_intent"].clone(),
        ReceiptMode::Valid,
    );
    let attach = json!({"prepared_id":scheduled_id,"install_id":install,"context_id":null});
    let attached = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_attach",
            attach.clone(),
        )
        .unwrap_or_else(|error| panic!("valid signed queue receipt refused: {error}"));
    assert_eq!(attached["prepared"]["state"], json!("authorized"));
    assert_eq!(attached["queued"]["state"], json!("queued"));
    assert_eq!(attached["queued"]["due_epoch"], json!(future_due));
    let queued_id = attached["queued"]["intent_id"].as_str().unwrap();
    let rows_after_attach = fx.prepared_rows(&install);
    assert_eq!(
        rows_after_attach
            .iter()
            .find(|row| row.0.as_str() == scheduled_id)
            .unwrap()
            .1
            .as_str(),
        "authorized"
    );
    assert_eq!(
        rows_after_attach
            .iter()
            .find(|row| row.0.as_str() == scheduled_id)
            .unwrap()
            .2
            .as_deref(),
        Some("dpq_cad1143_attach_acceptance")
    );
    assert_eq!(
        fx.queued_intent_count(&install),
        1,
        "one verified receipt queues once"
    );
    assert_eq!(fx.authorized_event_count(), authorized0 + 1);
    let inspected = fx.sender.requests.lock().unwrap();
    let selector = inspected
        .last()
        .expect("valid attach performed one queue inspection");
    assert_eq!(selector.expected_intent_id.as_str(), scheduled_id);
    assert_eq!(
        selector.expected_intent_digest.as_str(),
        scheduled["prepared"]["owner_intent"]["intent_digest"]
            .as_str()
            .unwrap()
    );
    assert_eq!(
        selector.cadence_run_id.as_str(),
        scheduled["prepared"]["owner_intent"]["run_id"]
            .as_str()
            .unwrap()
    );
    assert_eq!(
        selector.cadence_effect_id.as_str(),
        scheduled["prepared"]["owner_intent"]["effect_id"]
            .as_str()
            .unwrap()
    );
    assert_eq!(
        selector.connection_id.as_str(),
        scheduled["prepared"]["owner_intent"]["connection_id"]
            .as_str()
            .unwrap()
    );
    assert_eq!(selector.caption, CAPTION);
    drop(inspected);

    // An exact attach retry is served from the atomic committed row: it
    // returns the same queued intent without re-inspecting or adding a row.
    let inspect_after_first = fx.sender.inspect_calls.load(SeqCst);
    let retry_attached = fx
        .rpc(Asserted::Operator, "app_publish_intent_attach", attach)
        .unwrap();
    assert_eq!(retry_attached["queued"]["intent_id"], json!(queued_id));
    assert_eq!(fx.sender.inspect_calls.load(SeqCst), inspect_after_first);
    assert_eq!(fx.prepared_rows(&install), rows_after_attach);
    assert_eq!(fx.queued_intent_count(&install), 1);
    assert_eq!(fx.authorized_event_count(), authorized0 + 1);

    let early_claim = fx
        .rpc(
            Asserted::Operator,
            "social_publish_claim_due",
            json!({"now_epoch":NOW,"recheck":{}}),
        )
        .unwrap_or_else(|error| panic!("future due claim failed: {error}"));
    assert_eq!(early_claim["claimed"], json!(false));
    assert_eq!(
        fx.sender.preflight_calls.load(SeqCst),
        0,
        "notBefore was not sent early"
    );
    assert_eq!(
        fx.sender.execute_calls.load(SeqCst),
        0,
        "attach never dispatches"
    );
    assert_eq!(fx.sender.status_calls.load(SeqCst), 0);
    assert_eq!(fx.queued_intent_count(&install), 1);
}

/// Status is an advisory read over the existing queue-validation inspection;
/// it never consumes that inspection's JTI or creates attach authority. The
/// canonical RPC and HTTP scopes require explicit `context_id: null`, closed
/// schemas and the operator caller gate. Cancellation is exact-scope and
/// terminal, but does not claim remote deletion or revoke AOS state.
#[test]
fn owner_status_and_cancel_are_scoped_non_consuming_operator_relays() {
    const STATUS_PATH: &str = "/api/social-publish-intents/status";
    const CANCEL_PATH: &str = "/api/social-publish-intents/cancel";

    let mut fx = Fx::new();
    fx.start();
    let (install, run_id, _) = fx.setup();
    let prepared = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_prepare",
            json!({"request_id":"prep-status-cancel-01","run_id":run_id,"mode":"now"}),
        )
        .unwrap();
    let prepared_id = prepared["prepared"]["prepared_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let owner_intent = prepared["prepared"]["owner_intent"].clone();
    let scope = json!({"prepared_id":prepared_id,"install_id":install,"context_id":null});
    fx.sender.set_case(owner_intent.clone(), ReceiptMode::Valid);

    let prepared_before = fx.prepared_rows(&install);
    let queue_before = fx.publish_queue(&install);
    assert!(queue_before["intents"].as_array().unwrap().is_empty());
    let events_before = fx.social_publish_event_count();
    let ledger_before = fx.auth_ledger();
    let inspect_before = fx.sender.inspect_calls.load(SeqCst);
    let denied = fx.rpc(
        Asserted::Agent("writer".into()),
        "app_publish_intent_status",
        scope.clone(),
    );
    assert!(format!("{:?}", denied.unwrap_err()).contains("operator action"));
    assert!(fx
        .rpc(
            Asserted::Agent("writer".into()),
            "app_publish_intent_cancel",
            scope.clone(),
        )
        .is_err());
    for (method, forged) in [
        (
            "app_publish_intent_status",
            json!({"prepared_id":prepared_id,"install_id":install}),
        ),
        (
            "app_publish_intent_status",
            json!({"prepared_id":prepared_id,"install_id":install,"context_id":null,"grant_id":"dpq_forged"}),
        ),
        (
            "app_publish_intent_cancel",
            json!({"prepared_id":prepared_id,"install_id":install}),
        ),
        (
            "app_publish_intent_cancel",
            json!({"prepared_id":prepared_id,"install_id":install,"context_id":null,"receipt":"forged"}),
        ),
    ] {
        assert!(fx.rpc(Asserted::Operator, method, forged).is_err());
    }
    assert_eq!(fx.sender.inspect_calls.load(SeqCst), inspect_before);

    let ready = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_status",
            scope.clone(),
        )
        .unwrap();
    assert_eq!(ready["state"], json!("prepared"));
    assert_eq!(ready["owner_status"], json!("ready"));
    assert_no_authority_fields(&ready);
    assert_eq!(fx.auth_ledger(), ledger_before);
    let selector = fx.sender.requests.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        selector.expected_intent_id.as_str(),
        owner_intent["intent_id"].as_str().unwrap()
    );
    assert_eq!(
        selector.expected_intent_digest.as_str(),
        owner_intent["intent_digest"].as_str().unwrap()
    );

    for (mode, expected) in [
        (ReceiptMode::OwnerActionPending, "pending"),
        (ReceiptMode::OwnerActionNotFound, "unknown"),
        (ReceiptMode::OwnerActionAmbiguous, "refused"),
        (ReceiptMode::WrongScope, "refused"),
    ] {
        fx.sender.set_case(owner_intent.clone(), mode);
        let observed = fx
            .rpc(
                Asserted::Operator,
                "app_publish_intent_status",
                scope.clone(),
            )
            .unwrap();
        assert_eq!(observed["state"], json!("prepared"));
        assert_eq!(observed["owner_status"], json!(expected));
        assert_no_authority_fields(&observed);
        assert_eq!(fx.auth_ledger(), ledger_before);
    }

    fx.sender.set_case(owner_intent, ReceiptMode::Valid);
    let (agent_status, _) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "agent:writer",
        STATUS_PATH,
        &scope.to_string(),
    );
    assert_eq!(agent_status, 403, "HTTP status has the same operator gate");
    assert_eq!(fx.auth_ledger(), ledger_before);
    let (missing_scope_status, _) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        STATUS_PATH,
        &json!({"prepared_id":prepared_id,"install_id":install}).to_string(),
    );
    assert_eq!(
        missing_scope_status, 400,
        "HTTP status requires explicit null scope"
    );
    let (forged_scope_status, _) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        STATUS_PATH,
        &json!({"prepared_id":prepared_id,"install_id":install,"context_id":null,"grant_id":"dpq_forged"}).to_string(),
    );
    assert_eq!(forged_scope_status, 400, "HTTP status has a closed schema");
    let (http_status, http_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        STATUS_PATH,
        &scope.to_string(),
    );
    assert_eq!(http_status, 200, "HTTP status relays the canonical RPC");
    let http_status: Value = serde_json::from_str(http_response_body(&http_response)).unwrap();
    assert_eq!(http_status["owner_status"], json!("ready"));
    assert_no_authority_fields(&http_status);
    assert_eq!(fx.auth_ledger(), ledger_before);
    let status_claims = fx.sender.signed_claims.lock().unwrap().clone();
    for claims in &status_claims {
        let jti = claims["jti"].as_str().unwrap();
        assert!(
            !fx.auth_ledger()
                .contains(&cadence_agent::operator_auth::digest(jti)),
            "read-only status consumed inspection JTI {jti}"
        );
    }
    assert_eq!(fx.prepared_rows(&install), prepared_before);
    assert_eq!(
        fx.publish_queue(&install)["intents"],
        queue_before["intents"]
    );
    assert_eq!(fx.social_publish_event_count(), events_before);

    let inspect_before_attach = fx.sender.inspect_calls.load(SeqCst);
    let attached = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_attach",
            scope.clone(),
        )
        .unwrap();
    assert_eq!(attached["prepared"]["state"], json!("authorized"));
    assert_eq!(attached["queued"]["state"], json!("queued"));
    assert_eq!(attached["queued"]["claim_armed"], json!(true));
    assert_eq!(
        fx.sender.inspect_calls.load(SeqCst),
        inspect_before_attach + 1
    );
    let attach_jti = fx
        .sender
        .signed_claims
        .lock()
        .unwrap()
        .last()
        .expect("attach performs its own fresh queue inspection")["jti"]
        .as_str()
        .unwrap()
        .to_owned();
    let ledger_after_attach = fx.auth_ledger();
    assert!(ledger_after_attach.contains(&cadence_agent::operator_auth::digest(&attach_jti)));
    for claims in &status_claims {
        let jti = claims["jti"].as_str().unwrap();
        assert!(!ledger_after_attach.contains(&cadence_agent::operator_auth::digest(jti)));
    }

    let (missing_cancel_status, _) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        CANCEL_PATH,
        &json!({"prepared_id":prepared_id,"install_id":install}).to_string(),
    );
    assert_eq!(
        missing_cancel_status, 400,
        "HTTP cancel requires explicit null scope"
    );
    let (forged_cancel_status, _) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        CANCEL_PATH,
        &json!({"prepared_id":prepared_id,"install_id":install,"context_id":null,"receipt":"forged"}).to_string(),
    );
    assert_eq!(forged_cancel_status, 400, "HTTP cancel has a closed schema");
    let inspect_before_cancel = fx.sender.inspect_calls.load(SeqCst);
    let (cancel_http_status, cancel_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        CANCEL_PATH,
        &scope.to_string(),
    );
    assert_eq!(
        cancel_http_status, 200,
        "HTTP cancel relays the canonical RPC"
    );
    let cancelled: Value = serde_json::from_str(http_response_body(&cancel_response)).unwrap();
    assert_eq!(cancelled["state"], json!("cancelled"));
    assert_eq!(cancelled["queued"]["state"], json!("cancelled"));
    assert_no_authority_fields(&cancelled);
    assert_eq!(fx.sender.inspect_calls.load(SeqCst), inspect_before_cancel);
    let cancel_retry = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_cancel",
            scope.clone(),
        )
        .unwrap();
    assert_eq!(cancel_retry["state"], json!("cancelled"));
    assert_eq!(fx.social_publish_event_count(), events_before + 4);
    let mut expected_prepared = prepared_before;
    let prepared_row = expected_prepared
        .iter_mut()
        .find(|row| row.0 == prepared_id)
        .expect("prepared fixture row");
    prepared_row.1 = "cancelled".to_owned();
    prepared_row.2 = Some("dpq_cad1143_attach_acceptance".to_owned());
    assert_eq!(fx.prepared_rows(&install), expected_prepared);
    let cancelled_queue = fx.publish_queue(&install)["intents"].clone();
    assert_eq!(cancelled_queue.as_array().unwrap().len(), 1);
    assert_eq!(cancelled_queue[0]["state"], json!("cancelled"));
    let inspect_before_cancelled_status = fx.sender.inspect_calls.load(SeqCst);
    let cancelled_status = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_status",
            scope.clone(),
        )
        .unwrap();
    assert_eq!(cancelled_status["state"], json!("cancelled"));
    assert_eq!(cancelled_status["owner_status"], json!("refused"));
    assert_no_authority_fields(&cancelled_status);
    assert_eq!(
        fx.sender.inspect_calls.load(SeqCst),
        inspect_before_cancelled_status,
        "cancelled local state does not poll or claim AOS authority"
    );
    assert_eq!(fx.auth_ledger(), ledger_after_attach);
    assert_eq!(fx.sender.preflight_calls.load(SeqCst), 0);
    assert_eq!(fx.sender.execute_calls.load(SeqCst), 0);
    assert_eq!(fx.sender.status_calls.load(SeqCst), 0);
}

/// CAD-1143's post-commit arm guard is exercised through the real attach,
/// cancel, due-claim, named-claim and background-driver paths. Channels hold
/// the store at both transaction boundaries; the hook cannot access or mutate
/// the transaction. The sender refuses before any provider call. Explicit
/// `Operator` assertions exercise handler policy only, not real ancestry.
#[test]
fn postcommit_arm_uses_fresh_clock_and_cancellation_cannot_be_resurrected() {
    use cadence_agent::store::social_publish::SocialPublishAttachBoundary as Boundary;

    let mut fx = Fx::new();
    fx.start_driver();
    let (install, run_id, second_run_id) = fx.setup();

    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<Boundary>();
    let (before_release_tx, before_release_rx) = std::sync::mpsc::channel::<()>();
    let (after_release_tx, after_release_rx) = std::sync::mpsc::channel::<()>();
    let before_release_rx = Arc::new(Mutex::new(before_release_rx));
    let after_release_rx = Arc::new(Mutex::new(after_release_rx));
    let hook_before = Arc::clone(&before_release_rx);
    let hook_after = Arc::clone(&after_release_rx);
    fx.store
        .set_social_publish_attach_test_hook(Some(Arc::new(move |boundary| {
            entered_tx
                .send(boundary)
                .expect("acceptance observes attach boundary");
            let release = match boundary {
                Boundary::BeforeAuthorizationCommit => &hook_before,
                Boundary::AfterAuthorizationCommitBeforeArm => &hook_after,
            };
            release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .expect("acceptance releases attach boundary");
        })));

    let cancelled_prepare = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_prepare",
            json!({"request_id":"prep-unarmed-cancel-01","run_id":run_id,"mode":"now"}),
        )
        .unwrap();
    let cancelled_prepared_id = cancelled_prepare["prepared"]["prepared_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let cancelled_request = cancelled_prepare["prepared"]["request"]
        .as_str()
        .unwrap()
        .to_owned();
    fx.sender.set_case(
        cancelled_prepare["prepared"]["owner_intent"].clone(),
        ReceiptMode::Valid,
    );
    let cancelled_scope = json!({
        "prepared_id":cancelled_prepared_id,
        "install_id":install,
        "context_id":null,
    });
    let cancelled_attach = cancelled_scope.clone();
    let cancelled_dir = fx.dir();
    let (cancelled_result_tx, cancelled_result_rx) = std::sync::mpsc::channel();
    let cancelled_thread = std::thread::spawn(move || {
        let result = scoped(Asserted::Operator, || {
            client::rpc(
                &cancelled_dir,
                "app_publish_intent_attach",
                cancelled_attach,
            )
        })
        .map_err(|error| format!("{error:?}"));
        cancelled_result_tx.send(result).unwrap();
    });
    assert_eq!(
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        Boundary::BeforeAuthorizationCommit
    );
    // This exceeds the old sample+5 window before the first transaction has
    // committed. It must not age the later post-commit Undo interval.
    fx.clock.store(NOW + 6, SeqCst);
    fx.driver_clock.store(NOW + 6, SeqCst);
    before_release_tx.send(()).unwrap();
    assert_eq!(
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        Boundary::AfterAuthorizationCommitBeforeArm
    );

    let unarmed_status = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_status",
            cancelled_scope.clone(),
        )
        .unwrap();
    assert_eq!(unarmed_status["state"], json!("authorized"));
    assert_eq!(unarmed_status["owner_status"], json!("unknown"));
    assert_eq!(unarmed_status["queued"]["claim_armed"], json!(false));
    assert_eq!(unarmed_status["queued"]["claim_after_epoch"], Value::Null);
    assert_no_authority_fields(&unarmed_status);
    let unarmed_row = queue_row_for_request(&fx, &install, &cancelled_request);
    assert_eq!(unarmed_row["state"], json!("queued"));
    assert_eq!(unarmed_row["claim_armed"], json!(false));
    assert_eq!(unarmed_row["claim_after_epoch"], json!(0));
    assert_eq!(
        fx.publish_queue(&install)["intents"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let tick_before_unarmed = driver_last_tick(&fx, &install);
    wait_for_driver_tick_after(&fx, &install, tick_before_unarmed);
    assert_eq!(
        fx.sender.preflight_calls.load(SeqCst),
        0,
        "the real due driver must not preflight an unarmed row"
    );
    let forged_due = fx
        .rpc(
            Asserted::Operator,
            "social_publish_claim_due",
            json!({"now_epoch":NOW+3600,"recheck":{}}),
        )
        .unwrap();
    assert_eq!(forged_due["claimed"], json!(false));
    let named_unarmed = fx
        .rpc(
            Asserted::Operator,
            "social_publish_send_now",
            json!({
                "intent_id":unarmed_row["intent_id"],
                "install_id":install,
                "context_id":null,
            }),
        )
        .unwrap();
    assert_eq!(named_unarmed["sent"], json!(false));
    assert_eq!(named_unarmed["intent"]["state"], json!("queued"));
    assert_eq!(fx.sender.preflight_calls.load(SeqCst), 1);
    assert_eq!(fx.sender.execute_calls.load(SeqCst), 0);
    assert_eq!(fx.sender.status_calls.load(SeqCst), 0);

    let cancelled = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_cancel",
            cancelled_scope.clone(),
        )
        .unwrap();
    assert_eq!(cancelled["state"], json!("cancelled"));
    assert_eq!(cancelled["queued"]["state"], json!("cancelled"));
    assert_eq!(cancelled["queued"]["claim_armed"], json!(false));
    let tick_before_cancelled_driver = driver_last_tick(&fx, &install);
    wait_for_driver_tick_after(&fx, &install, tick_before_cancelled_driver);
    assert_eq!(fx.sender.preflight_calls.load(SeqCst), 1);
    after_release_tx.send(()).unwrap();
    let cancelled_attach_result = cancelled_result_rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    assert!(cancelled_attach_result
        .unwrap_err()
        .contains("only an authorized owner attachment can be armed"));
    cancelled_thread.join().unwrap();
    let late_attach_inspections = fx.sender.inspect_calls.load(SeqCst);
    assert!(fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_attach",
            cancelled_scope.clone(),
        )
        .is_err());
    assert_eq!(
        fx.sender.inspect_calls.load(SeqCst),
        late_attach_inspections
    );
    let still_cancelled = queue_row_for_request(&fx, &install, &cancelled_request);
    assert_eq!(still_cancelled["state"], json!("cancelled"));
    assert_eq!(still_cancelled["claim_armed"], json!(false));
    assert_eq!(still_cancelled["claim_after_epoch"], json!(0));
    assert_eq!(fx.queued_intent_count(&install), 0);

    let armed_prepare = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_prepare",
            json!({"request_id":"prep-postcommit-arm-01","run_id":second_run_id,"mode":"now"}),
        )
        .unwrap();
    let armed_prepared_id = armed_prepare["prepared"]["prepared_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let armed_request = armed_prepare["prepared"]["request"]
        .as_str()
        .unwrap()
        .to_owned();
    fx.sender.set_case(
        armed_prepare["prepared"]["owner_intent"].clone(),
        ReceiptMode::Valid,
    );
    let armed_scope = json!({
        "prepared_id":armed_prepared_id,
        "install_id":install,
        "context_id":null,
    });
    let armed_attach = armed_scope.clone();
    let armed_dir = fx.dir();
    let (armed_result_tx, armed_result_rx) = std::sync::mpsc::channel();
    let armed_thread = std::thread::spawn(move || {
        let result = scoped(Asserted::Operator, || {
            client::rpc(&armed_dir, "app_publish_intent_attach", armed_attach)
        })
        .map_err(|error| format!("{error:?}"));
        armed_result_tx.send(result).unwrap();
    });
    assert_eq!(
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        Boundary::BeforeAuthorizationCommit
    );
    fx.clock.store(NOW + 12, SeqCst);
    fx.driver_clock.store(NOW + 12, SeqCst);
    before_release_tx.send(()).unwrap();
    assert_eq!(
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        Boundary::AfterAuthorizationCommitBeforeArm
    );
    let s = NOW + 14;
    fx.clock.store(s, SeqCst);
    fx.driver_clock.store(s, SeqCst);

    let unarmed_success = queue_row_for_request(&fx, &install, &armed_request);
    assert_eq!(unarmed_success["state"], json!("queued"));
    assert_eq!(unarmed_success["claim_armed"], json!(false));
    assert_eq!(unarmed_success["claim_after_epoch"], json!(0));
    let frozen_before_arm = unarmed_success["frozen"].clone();
    let intent_id_before_arm = unarmed_success["intent_id"].clone();
    let due_before_arm = unarmed_success["due_epoch"].clone();
    let tick_before_second_driver = driver_last_tick(&fx, &install);
    wait_for_driver_tick_after(&fx, &install, tick_before_second_driver);
    assert_eq!(fx.sender.preflight_calls.load(SeqCst), 1);
    let unarmed_due = fx
        .rpc(
            Asserted::Operator,
            "social_publish_claim_due",
            json!({"now_epoch":s+3600,"recheck":{}}),
        )
        .unwrap();
    assert_eq!(unarmed_due["claimed"], json!(false));
    let unarmed_named = fx
        .rpc(
            Asserted::Operator,
            "social_publish_send_now",
            json!({
                "intent_id":unarmed_success["intent_id"],
                "install_id":install,
                "context_id":null,
            }),
        )
        .unwrap();
    assert_eq!(unarmed_named["sent"], json!(false));
    assert_eq!(unarmed_named["intent"]["state"], json!("queued"));
    assert_eq!(fx.sender.preflight_calls.load(SeqCst), 2);
    assert_eq!(fx.sender.execute_calls.load(SeqCst), 0);

    let status_inspections = fx.sender.inspect_calls.load(SeqCst);
    let authorized_status = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_status",
            armed_scope.clone(),
        )
        .unwrap();
    assert_eq!(authorized_status["state"], json!("authorized"));
    assert_eq!(authorized_status["owner_status"], json!("unknown"));
    assert_eq!(fx.sender.inspect_calls.load(SeqCst), status_inspections);
    after_release_tx.send(()).unwrap();
    let attached = armed_result_rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    armed_thread.join().unwrap();
    assert_eq!(attached["prepared"]["state"], json!("authorized"));
    assert_eq!(attached["queued"]["claim_armed"], json!(true));
    assert_eq!(attached["queued"]["claim_after_epoch"], json!(s + 5));
    let armed_row = queue_row_for_request(&fx, &install, &armed_request);
    assert_eq!(armed_row["intent_id"], intent_id_before_arm);
    assert_eq!(armed_row["due_epoch"], due_before_arm);
    assert_eq!(armed_row["claim_armed"], json!(true));
    assert_eq!(armed_row["claim_after_epoch"], json!(s + 5));
    assert_eq!(armed_row["frozen"], frozen_before_arm);
    assert_eq!(fx.queued_intent_count(&install), 1);

    let inspect_before_retry = fx.sender.inspect_calls.load(SeqCst);
    let retry = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_attach",
            armed_scope.clone(),
        )
        .unwrap();
    assert_eq!(retry["queued"]["intent_id"], intent_id_before_arm);
    assert_eq!(retry["queued"]["claim_after_epoch"], json!(s + 5));
    assert_eq!(fx.sender.inspect_calls.load(SeqCst), inspect_before_retry);

    fx.clock.store(s + 4, SeqCst);
    fx.driver_clock.store(s + 4, SeqCst);
    let tick_before_plus_four = driver_last_tick(&fx, &install);
    wait_for_driver_tick_after(&fx, &install, tick_before_plus_four);
    let plus_four_due = fx
        .rpc(
            Asserted::Operator,
            "social_publish_claim_due",
            json!({"now_epoch":s+3600,"recheck":{}}),
        )
        .unwrap();
    assert_eq!(plus_four_due["claimed"], json!(false));
    let plus_four_named = fx
        .rpc(
            Asserted::Operator,
            "social_publish_send_now",
            json!({
                "intent_id":intent_id_before_arm,
                "install_id":install,
                "context_id":null,
            }),
        )
        .unwrap();
    assert_eq!(plus_four_named["sent"], json!(false));
    assert_eq!(plus_four_named["intent"]["state"], json!("queued"));
    assert_eq!(fx.sender.preflight_calls.load(SeqCst), 3);
    assert_eq!(fx.sender.execute_calls.load(SeqCst), 0);
    let plus_four_row = queue_row_for_request(&fx, &install, &armed_request);
    assert_eq!(plus_four_row["state"], json!("queued"));
    assert_eq!(plus_four_row["claim_after_epoch"], json!(s + 5));
    assert_eq!(plus_four_row["frozen"], frozen_before_arm);

    let frozen = &armed_row["frozen"];
    let recheck = json!({
        "grant_id":frozen["grant_id"],
        "aos_connection_id":frozen["aos_connection_id"],
        "destination_id":frozen["destination_id"],
        "caption_digest":frozen["caption_digest"],
        "image_digest":frozen["image_digest"],
    });
    fx.clock.store(s + 5, SeqCst);
    // Hold the background loop at +4 so this operator route is the one
    // maturity-boundary claimant; it still uses the real SQLite claim CAS.
    let mature_claim = fx
        .rpc(
            Asserted::Operator,
            "social_publish_claim_due",
            json!({"now_epoch":s+3600,"recheck":recheck}),
        )
        .unwrap();
    assert_ne!(mature_claim["claimed"], json!(false));
    assert_eq!(fx.sender.execute_calls.load(SeqCst), 1);
    let repeated_claim = fx
        .rpc(
            Asserted::Operator,
            "social_publish_claim_due",
            json!({"now_epoch":s+3600,"recheck":{}}),
        )
        .unwrap();
    assert_eq!(repeated_claim["claimed"], json!(false));
    assert!(fx
        .rpc(
            Asserted::Operator,
            "social_publish_send_now",
            json!({
                "intent_id":intent_id_before_arm,
                "install_id":install,
                "context_id":null,
            }),
        )
        .is_err());
    assert_eq!(fx.sender.execute_calls.load(SeqCst), 1);
    fx.driver_clock.store(s + 5, SeqCst);
    let tick_before_maturity = driver_last_tick(&fx, &install);
    wait_for_driver_tick_after(&fx, &install, tick_before_maturity);
    assert_eq!(fx.sender.execute_calls.load(SeqCst), 1);
    assert_eq!(fx.sender.status_calls.load(SeqCst), 0);
    fx.store.set_social_publish_attach_test_hook(None);
}

#[test]
fn committed_unarmed_recovery_revalidates_and_arms_the_same_row_once() {
    use cadence_agent::store::social_publish::SocialPublishAttachBoundary as Boundary;

    let mut fx = Fx::new();
    fx.start();
    let (install, run_id, _) = fx.setup();
    let prepared = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_prepare",
            json!({"request_id":"prep-recovery-fresh-proof-01","run_id":run_id,"mode":"now"}),
        )
        .unwrap();
    let prepared_id = prepared["prepared"]["prepared_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let request = prepared["prepared"]["request"].as_str().unwrap().to_owned();
    let owner_intent = prepared["prepared"]["owner_intent"].clone();
    let scope = json!({
        "prepared_id":prepared_id,
        "install_id":install,
        "context_id":null,
    });
    fx.sender.set_case(owner_intent.clone(), ReceiptMode::Valid);

    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<Boundary>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let hook_release = Arc::clone(&release_rx);
    fx.store
        .set_social_publish_attach_test_hook(Some(Arc::new(move |boundary| {
            entered_tx
                .send(boundary)
                .expect("acceptance observes attach boundary");
            if boundary == Boundary::AfterAuthorizationCommitBeforeArm {
                hook_release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(30))
                    .expect("acceptance releases post-commit attach");
            }
        })));

    let initial_dir = fx.dir();
    let initial_scope = scope.clone();
    let (initial_result_tx, initial_result_rx) = std::sync::mpsc::channel();
    let initial_thread = std::thread::spawn(move || {
        let result = scoped(Asserted::Operator, || {
            client::rpc(&initial_dir, "app_publish_intent_attach", initial_scope)
        })
        .map_err(|error| format!("{error:?}"));
        initial_result_tx.send(result).unwrap();
    });
    assert_eq!(
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        Boundary::BeforeAuthorizationCommit
    );
    assert_eq!(
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        Boundary::AfterAuthorizationCommitBeforeArm
    );

    let unarmed = queue_row_for_request(&fx, &install, &request);
    assert_eq!(unarmed["state"], json!("queued"));
    assert_eq!(unarmed["claim_armed"], json!(false));
    assert_eq!(unarmed["claim_after_epoch"], json!(0));
    let original_intent_id = unarmed["intent_id"].clone();
    let original_due = unarmed["due_epoch"].clone();
    let original_frozen = unarmed["frozen"].clone();
    let prepared_after_commit = fx.prepared_rows(&install);
    let auth_after_initial = fx.auth_ledger();
    let initial_claims = fx.sender.signed_claims.lock().unwrap().clone();
    let initial_jti = initial_claims
        .last()
        .expect("first attach inspected a current queue receipt")["jti"]
        .as_str()
        .unwrap()
        .to_owned();
    let grant_id = initial_claims.last().unwrap()["grant_id"].clone();
    assert!(auth_after_initial.contains(&cadence_agent::operator_auth::digest(&initial_jti)));
    let events_after_initial = fx.social_publish_event_count();
    let authorized_after_initial = fx.authorized_event_count();
    let effect_id = owner_intent["effect_id"].as_str().unwrap();
    let effect_after_initial = fx.store.app_effect_show(effect_id).unwrap();
    let preflight_after_initial = fx.sender.preflight_calls.load(SeqCst);
    let execute_after_initial = fx.sender.execute_calls.load(SeqCst);
    let status_after_initial = fx.sender.status_calls.load(SeqCst);
    let assert_unarmed_unchanged = || {
        assert_eq!(fx.prepared_rows(&install), prepared_after_commit);
        assert_eq!(
            queue_row_for_request(&fx, &install, &request),
            unarmed,
            "failed recovery changed the committed unarmed row"
        );
        assert_eq!(fx.queued_intent_count(&install), 1);
        assert_eq!(fx.social_publish_event_count(), events_after_initial);
        assert_eq!(fx.authorized_event_count(), authorized_after_initial);
        assert_eq!(fx.auth_ledger(), auth_after_initial);
        assert_eq!(
            fx.store.app_effect_show(effect_id).unwrap(),
            effect_after_initial
        );
        assert_eq!(
            fx.sender.preflight_calls.load(SeqCst),
            preflight_after_initial
        );
        assert_eq!(fx.sender.execute_calls.load(SeqCst), execute_after_initial);
        assert_eq!(fx.sender.status_calls.load(SeqCst), status_after_initial);
    };

    let inspect_before_expired = fx.sender.inspect_calls.load(SeqCst);
    let claims_before_expired = fx.sender.signed_claims.lock().unwrap().len();
    fx.sender
        .set_case(owner_intent.clone(), ReceiptMode::Expired);
    let expired_error = format!(
        "{:?}",
        fx.rpc(
            Asserted::Operator,
            "app_publish_intent_attach",
            scope.clone(),
        )
        .expect_err("expired fresh recovery receipt unexpectedly armed the row")
    );
    assert!(
        expired_error.contains("15-second receipt window"),
        "recovery did not refuse the expired receipt: {expired_error}"
    );
    assert_eq!(
        fx.sender.inspect_calls.load(SeqCst),
        inspect_before_expired + 1
    );
    let expired_claims = fx.sender.signed_claims.lock().unwrap().clone();
    assert_eq!(expired_claims.len(), claims_before_expired + 1);
    let expired_jti = expired_claims.last().unwrap()["jti"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!auth_after_initial.contains(&cadence_agent::operator_auth::digest(&expired_jti)));
    assert_unarmed_unchanged();

    let recovery_epoch = NOW + 2;
    fx.clock.store(recovery_epoch, SeqCst);
    fx.sender.set_case(owner_intent.clone(), ReceiptMode::Valid);
    let inspect_before_recovery = fx.sender.inspect_calls.load(SeqCst);
    let claims_before_recovery = fx.sender.signed_claims.lock().unwrap().len();
    let recovered = fx
        .rpc(
            Asserted::Operator,
            "app_publish_intent_attach",
            scope.clone(),
        )
        .unwrap_or_else(|error| panic!("fresh authorized recovery refused: {error}"));
    assert_eq!(
        fx.sender.inspect_calls.load(SeqCst),
        inspect_before_recovery + 1
    );
    let recovered_claims = fx.sender.signed_claims.lock().unwrap().clone();
    assert_eq!(recovered_claims.len(), claims_before_recovery + 1);
    let recovery_claim = recovered_claims.last().unwrap();
    assert_eq!(recovery_claim["iat"], json!(recovery_epoch));
    assert_eq!(recovery_claim["exp"], json!(recovery_epoch + 15));
    assert_eq!(recovery_claim["grant_id"], grant_id);
    let recovery_jti = recovery_claim["jti"].as_str().unwrap();
    assert_ne!(recovery_jti, initial_jti);
    let auth_after_recovery = fx.auth_ledger();
    assert!(auth_after_recovery.contains(&cadence_agent::operator_auth::digest(recovery_jti)));
    assert!(auth_after_recovery.contains(&cadence_agent::operator_auth::digest(&initial_jti)));
    assert!(!auth_after_recovery.contains(&cadence_agent::operator_auth::digest(&expired_jti)));
    assert_eq!(recovered["prepared"]["state"], json!("authorized"));
    assert_eq!(recovered["queued"]["state"], json!("queued"));
    assert_eq!(recovered["queued"]["intent_id"], original_intent_id);
    assert_eq!(recovered["queued"]["claim_armed"], json!(true));
    assert_eq!(
        recovered["queued"]["claim_after_epoch"],
        json!(recovery_epoch + 5)
    );
    let armed = queue_row_for_request(&fx, &install, &request);
    assert_eq!(armed["intent_id"], original_intent_id);
    assert_eq!(armed["state"], json!("queued"));
    assert_eq!(armed["due_epoch"], original_due);
    assert_eq!(armed["frozen"], original_frozen);
    assert_eq!(armed["claim_armed"], json!(true));
    assert_eq!(armed["claim_after_epoch"], json!(recovery_epoch + 5));
    assert_eq!(fx.prepared_rows(&install), prepared_after_commit);
    assert_eq!(fx.queued_intent_count(&install), 1);
    assert_eq!(fx.social_publish_event_count(), events_after_initial);
    assert_eq!(fx.authorized_event_count(), authorized_after_initial);
    assert_eq!(
        fx.store.app_effect_show(effect_id).unwrap(),
        effect_after_initial
    );
    assert_eq!(
        fx.sender.preflight_calls.load(SeqCst),
        preflight_after_initial
    );
    assert_eq!(fx.sender.execute_calls.load(SeqCst), execute_after_initial);
    assert_eq!(fx.sender.status_calls.load(SeqCst), status_after_initial);

    release_tx.send(()).unwrap();
    let first_result = initial_result_rx
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    initial_thread.join().unwrap();
    assert_eq!(first_result["queued"]["intent_id"], original_intent_id);
    assert_eq!(
        first_result["queued"]["claim_after_epoch"],
        json!(recovery_epoch + 5)
    );

    fx.sender
        .set_case(owner_intent.clone(), ReceiptMode::UpstreamUncertain);
    let inspect_before_armed_retry = fx.sender.inspect_calls.load(SeqCst);
    let claims_before_armed_retry = fx.sender.signed_claims.lock().unwrap().len();
    let auth_before_armed_retry = fx.auth_ledger();
    let armed_retry = fx
        .rpc(Asserted::Operator, "app_publish_intent_attach", scope)
        .unwrap_or_else(|error| panic!("already-armed retry was not read-only: {error}"));
    assert_eq!(armed_retry["queued"]["intent_id"], original_intent_id);
    assert_eq!(
        armed_retry["queued"]["claim_after_epoch"],
        json!(recovery_epoch + 5)
    );
    assert_eq!(
        fx.sender.inspect_calls.load(SeqCst),
        inspect_before_armed_retry
    );
    assert_eq!(
        fx.sender.signed_claims.lock().unwrap().len(),
        claims_before_armed_retry
    );
    assert_eq!(fx.auth_ledger(), auth_before_armed_retry);
    assert_eq!(queue_row_for_request(&fx, &install, &request), armed);
    assert_eq!(fx.social_publish_event_count(), events_after_initial);
    assert_eq!(fx.authorized_event_count(), authorized_after_initial);
    assert_eq!(
        fx.store.app_effect_show(effect_id).unwrap(),
        effect_after_initial
    );
    fx.store.set_social_publish_attach_test_hook(None);
}

/// CAD-1143 native owner-action boundary acceptance. The test-seam Operator
/// identity exercises the real board relay after its operator gate; it is not
/// genuine process-ancestry proof. A signed local queue peer supplies the
/// exact receipt shape, and the fake sender makes any early dispatch visible
/// without reaching AOS or a provider. A second immutable `now` intent
/// proves queue-time gating, the maturity CAS winner and losing retries using
/// local policy-seam evidence only.
#[test]
fn native_owner_http_rejects_frame_forgery_attaches_once_and_preserves_undo_window() {
    const PREPARE_PATH: &str = "/api/social-publish-intents/prepare";
    const ATTACH_PATH: &str = "/api/social-publish-intents/attach";

    let mut fx = Fx::new();
    fx.start_driver();
    let (install, run_id, second_run_id) = fx.setup();
    let prepared_before = fx.prepared_rows(&install);
    let queue_before = fx.publish_queue(&install);
    let events_before = fx.social_publish_event_count();
    let platform_events_before = fx.store.events_tail(PLATFORM_STREAM, 500).unwrap().len();
    let inspect_before = fx.sender.inspect_calls.load(SeqCst);
    let preflight_before = fx.sender.preflight_calls.load(SeqCst);
    let execute_before = fx.sender.execute_calls.load(SeqCst);
    let status_before = fx.sender.status_calls.load(SeqCst);
    let assert_prepare_unchanged = || {
        assert_eq!(fx.prepared_rows(&install), prepared_before);
        assert_eq!(fx.publish_queue(&install), queue_before);
        assert_eq!(fx.social_publish_event_count(), events_before);
        assert_eq!(
            fx.store.events_tail(PLATFORM_STREAM, 500).unwrap().len(),
            platform_events_before
        );
        assert_eq!(fx.sender.inspect_calls.load(SeqCst), inspect_before);
        assert_eq!(fx.sender.preflight_calls.load(SeqCst), preflight_before);
        assert_eq!(fx.sender.execute_calls.load(SeqCst), execute_before);
        assert_eq!(fx.sender.status_calls.load(SeqCst), status_before);
    };

    // Presence is semantically meaningful: explicit null is not omission for
    // `now`, and malformed schedule values must refuse before any prepare
    // mutation at either the real daemon RPC or its HTTP relay.
    let null_now_rpc = fx.rpc(
        Asserted::Operator,
        "app_publish_intent_prepare",
        json!({
            "request_id":"native-owner-rpc-now-null",
            "run_id":run_id,
            "mode":"now",
            "due_epoch":null,
        }),
    );
    assert!(
        null_now_rpc.is_err(),
        "RPC prepare accepted present due_epoch:null in now mode"
    );
    assert_prepare_unchanged();

    let malformed_schedule_rpc = fx.rpc(
        Asserted::Operator,
        "app_publish_intent_prepare",
        json!({
            "request_id":"native-owner-rpc-schedule-malformed",
            "run_id":run_id,
            "mode":"schedule",
            "due_epoch":"not-an-epoch",
        }),
    );
    assert!(
        malformed_schedule_rpc.is_err(),
        "RPC prepare accepted malformed schedule time"
    );
    assert_prepare_unchanged();

    for (field, params) in [
        ("request_id", json!({"run_id":run_id,"mode":"now"})),
        (
            "run_id",
            json!({"request_id":"native-owner-rpc-missing-run","mode":"now"}),
        ),
        (
            "mode",
            json!({"request_id":"native-owner-rpc-missing-mode","run_id":run_id}),
        ),
    ] {
        let result = fx.rpc(Asserted::Operator, "app_publish_intent_prepare", params);
        assert!(
            result.is_err(),
            "RPC prepare accepted missing required {field}"
        );
        assert_prepare_unchanged();
    }

    let (null_now_status, null_now_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        PREPARE_PATH,
        &json!({
            "request_id":"native-owner-http-now-null",
            "run_id":run_id,
            "mode":"now",
            "due_epoch":null,
        })
        .to_string(),
    );
    assert_eq!(
        null_now_status, 400,
        "HTTP prepare erased explicit due_epoch:null: {null_now_response}"
    );
    assert_prepare_unchanged();

    let (malformed_schedule_status, malformed_schedule_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        PREPARE_PATH,
        &json!({
            "request_id":"native-owner-http-schedule-malformed",
            "run_id":run_id,
            "mode":"schedule",
            "due_epoch":"not-an-epoch",
        })
        .to_string(),
    );
    assert_eq!(
        malformed_schedule_status, 400,
        "HTTP prepare accepted malformed schedule time: {malformed_schedule_response}"
    );
    assert_prepare_unchanged();

    for (field, body) in [
        ("request_id", json!({"run_id":run_id,"mode":"now"})),
        (
            "run_id",
            json!({"request_id":"native-owner-http-missing-run","mode":"now"}),
        ),
        (
            "mode",
            json!({"request_id":"native-owner-http-missing-mode","run_id":run_id}),
        ),
    ] {
        let (status, response) = board_http_post(
            &fx.dir(),
            &fx.pm(),
            "operator",
            PREPARE_PATH,
            &body.to_string(),
        );
        assert_eq!(
            status, 400,
            "HTTP prepare accepted missing required {field}: {response}"
        );
        assert_prepare_unchanged();
    }

    // A frame/agent cannot turn caller-supplied identity, grant or material
    // fields into a mutation through the real board HTTP route.
    let forged_prepare = json!({
        "request_id":"native-owner-http-forged",
        "run_id":run_id,
        "mode":"now",
        "actor":"operator",
        "company_slug":"frame-controlled-slug",
        "authorize_url":"https://frame-controlled.invalid/owner-action",
        "grant_id":"dpq_forged_frame_grant_01",
        "owner_id":"owner_forged",
        "destination_id":"dest_forged",
        "aos_connection_id":"conn_forged",
        "owner_intent":{"intent_id":"intent_forged"},
        "assertion":"social.intent.read.v1",
    });
    let (agent_status, agent_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "agent:writer",
        PREPARE_PATH,
        &forged_prepare.to_string(),
    );
    assert_eq!(
        agent_status, 403,
        "agent/frame caller reached prepare: {agent_response}"
    );
    let (forged_status, forged_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        PREPARE_PATH,
        &forged_prepare.to_string(),
    );
    assert_eq!(
        forged_status, 400,
        "HTTP prepare accepted forged fields: {forged_response}"
    );
    assert_eq!(fx.prepared_rows(&install), prepared_before);
    assert_eq!(fx.publish_queue(&install), queue_before);
    assert_eq!(fx.social_publish_event_count(), events_before);
    assert_eq!(fx.sender.inspect_calls.load(SeqCst), inspect_before);
    assert_eq!(fx.sender.execute_calls.load(SeqCst), 0);

    // The exact strict HTTP grammar contains only the host-minted request id,
    // approved run, mode and optional schedule time. The board relays this to
    // the guarded daemon prepare without putting the prepared row in the queue.
    let prepare_body = json!({
        "request_id":"native-owner-http-valid",
        "run_id":run_id,
        "mode":"now",
    });
    let (prepare_status, prepare_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        PREPARE_PATH,
        &prepare_body.to_string(),
    );
    assert_eq!(
        prepare_status, 200,
        "exact prepare relay refused: {prepare_response}"
    );
    let prepared: Value = serde_json::from_str(http_response_body(&prepare_response))
        .expect("prepare HTTP response is JSON");
    let prepared_id = prepared["prepared"]["prepared_id"]
        .as_str()
        .expect("HTTP prepare returns its prepared selector")
        .to_owned();
    let owner_intent = prepared["prepared"]["owner_intent"].clone();
    assert_eq!(prepared["prepared"]["state"], json!("prepared"));
    assert_eq!(
        fx.queued_intent_count(&install),
        0,
        "prepare must not queue"
    );
    let configured_identity = cadence_agent::board_identity::read_config(&fx.dir()).unwrap();
    assert!(configured_identity.issuer.starts_with("http://127.0.0.1:"));
    assert!(OWNER_APP_ORIGIN.starts_with("https://"));
    assert!(OWNER_AUTHORIZE_URL
        .strip_prefix(OWNER_APP_ORIGIN)
        .is_some_and(|suffix| suffix.starts_with('/')));
    assert_eq!(owner_intent["intent_id"], json!(prepared_id));
    let intent_digest = owner_intent["intent_digest"].as_str().unwrap();
    let expected_owner_action_url = format!(
        "{OWNER_APP_ORIGIN}/social-owner-action?company_slug={OWNER_COMPANY_SLUG}&intent_id={prepared_id}&intent_digest={intent_digest}"
    );
    assert_eq!(
        prepared["owner_action_url"],
        json!(expected_owner_action_url)
    );
    assert!(!expected_owner_action_url.contains(configured_identity.issuer.as_str()));
    assert!(!expected_owner_action_url.contains("frame-controlled"));

    assert_eq!(
        configured_identity.host,
        format!("{OWNER_COMPANY_SLUG}.cadencecloud.app"),
        "the explicit AOS slug must match the exact configured production host"
    );
    let attach_body = json!({"prepared_id":prepared_id,"install_id":install,"context_id":null});
    let prepared_after_prepare = fx.prepared_rows(&install);
    let queue_after_prepare = fx.publish_queue(&install);
    let events_after_prepare = fx.social_publish_event_count();
    let platform_events_after_prepare = fx.store.events_tail(PLATFORM_STREAM, 500).unwrap().len();
    let authorized_after_prepare = fx.authorized_event_count();
    let inspect_after_prepare = fx.sender.inspect_calls.load(SeqCst);
    let preflight_after_prepare = fx.sender.preflight_calls.load(SeqCst);
    let execute_after_prepare = fx.sender.execute_calls.load(SeqCst);
    let status_after_prepare = fx.sender.status_calls.load(SeqCst);
    let assert_attach_omission_unchanged = || {
        assert_eq!(fx.prepared_rows(&install), prepared_after_prepare);
        assert_eq!(fx.publish_queue(&install), queue_after_prepare);
        assert_eq!(fx.social_publish_event_count(), events_after_prepare);
        assert_eq!(
            fx.store.events_tail(PLATFORM_STREAM, 500).unwrap().len(),
            platform_events_after_prepare
        );
        assert_eq!(fx.authorized_event_count(), authorized_after_prepare);
        assert_eq!(fx.sender.inspect_calls.load(SeqCst), inspect_after_prepare);
        assert_eq!(
            fx.sender.preflight_calls.load(SeqCst),
            preflight_after_prepare
        );
        assert_eq!(fx.sender.execute_calls.load(SeqCst), execute_after_prepare);
        assert_eq!(fx.sender.status_calls.load(SeqCst), status_after_prepare);
    };
    for (field, params) in [
        (
            "prepared_id",
            json!({"install_id":install,"context_id":null}),
        ),
        (
            "install_id",
            json!({"prepared_id":prepared_id,"context_id":null}),
        ),
    ] {
        let result = fx.rpc(Asserted::Operator, "app_publish_intent_attach", params);
        assert!(
            result.is_err(),
            "RPC attach accepted missing required {field}"
        );
        assert_attach_omission_unchanged();
    }
    for (field, body) in [
        (
            "prepared_id",
            json!({"install_id":install,"context_id":null}),
        ),
        (
            "install_id",
            json!({"prepared_id":prepared_id,"context_id":null}),
        ),
    ] {
        let (status, response) = board_http_post(
            &fx.dir(),
            &fx.pm(),
            "operator",
            ATTACH_PATH,
            &body.to_string(),
        );
        assert_eq!(
            status, 400,
            "HTTP attach accepted missing required {field}: {response}"
        );
        assert_attach_omission_unchanged();
    }
    drop(assert_attach_omission_unchanged);
    let mut forged_attach = attach_body.clone();
    forged_attach["actor"] = json!("operator");
    forged_attach["grant_id"] = json!("dpq_forged_frame_grant_02");
    forged_attach["owner_intent"] = json!(owner_intent.clone());
    forged_attach["purpose"] = json!(READ_PURPOSE);
    let (agent_attach_status, agent_attach_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "agent:writer",
        ATTACH_PATH,
        &forged_attach.to_string(),
    );
    assert_eq!(
        agent_attach_status, 403,
        "agent/frame caller reached attach: {agent_attach_response}"
    );
    let (forged_attach_status, forged_attach_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        ATTACH_PATH,
        &forged_attach.to_string(),
    );
    assert_eq!(
        forged_attach_status, 400,
        "HTTP attach accepted caller authority: {forged_attach_response}"
    );
    assert_eq!(fx.prepared_rows(&install), prepared_after_prepare);
    assert_eq!(fx.social_publish_event_count(), events_after_prepare);
    assert_eq!(fx.authorized_event_count(), authorized_after_prepare);
    assert_eq!(
        fx.queued_intent_count(&install),
        0,
        "forged attach queued a send"
    );
    assert_eq!(fx.sender.inspect_calls.load(SeqCst), inspect_before);
    assert_eq!(fx.sender.execute_calls.load(SeqCst), 0);

    // Model a long owner-exchange delay: the signed descriptor/grant was
    // prepared at NOW, but the trusted host attaches at this later clock.
    // The five-second Undo origin is the atomic queue commit, not prepare or
    // the browser's unverifiable window-message time.
    let attach_at = NOW + 120;
    fx.clock.store(attach_at, SeqCst);
    fx.driver_clock.store(attach_at, SeqCst);

    // A byte-identical prepare retry after the trusted clock advances must
    // reuse the immutable prepared window, not recompute due/notBefore/expiry
    // and accidentally invalidate the still-live owner intent.
    let queue_before_delayed_retry = fx.publish_queue(&install);
    let inspect_before_delayed_retry = fx.sender.inspect_calls.load(SeqCst);
    let preflight_before_delayed_retry = fx.sender.preflight_calls.load(SeqCst);
    let execute_before_delayed_retry = fx.sender.execute_calls.load(SeqCst);
    let status_before_delayed_retry = fx.sender.status_calls.load(SeqCst);
    assert!(
        prepared_after_prepare
            .iter()
            .find(|row| row.0 == prepared_id)
            .expect("initial prepared row exists")
            .2
            .is_none(),
        "prepare must not mint or persist a grant"
    );
    let (delayed_prepare_status, delayed_prepare_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        PREPARE_PATH,
        &prepare_body.to_string(),
    );
    assert_eq!(
        delayed_prepare_status, 200,
        "delayed exact prepare retry refused: {delayed_prepare_response}"
    );
    let delayed_retry: Value = serde_json::from_str(http_response_body(&delayed_prepare_response))
        .expect("delayed prepare retry response is JSON");
    assert_eq!(delayed_retry["prepared"]["prepared_id"], json!(prepared_id));
    assert_eq!(delayed_retry["prepared"]["state"], json!("prepared"));
    assert_eq!(delayed_retry["prepared"]["owner_intent"], owner_intent);
    assert_eq!(
        delayed_retry["prepared"]["owner_intent"]["intent_digest"],
        owner_intent["intent_digest"]
    );
    assert_eq!(fx.prepared_rows(&install), prepared_after_prepare);
    assert_eq!(fx.publish_queue(&install), queue_before_delayed_retry);
    assert_eq!(fx.queued_intent_count(&install), 0);
    assert_eq!(fx.social_publish_event_count(), events_after_prepare);
    assert_eq!(fx.authorized_event_count(), authorized_after_prepare);
    assert_eq!(
        fx.sender.inspect_calls.load(SeqCst),
        inspect_before_delayed_retry
    );
    assert_eq!(
        fx.sender.preflight_calls.load(SeqCst),
        preflight_before_delayed_retry
    );
    assert_eq!(
        fx.sender.execute_calls.load(SeqCst),
        execute_before_delayed_retry
    );
    assert_eq!(
        fx.sender.status_calls.load(SeqCst),
        status_before_delayed_retry
    );

    fx.sender.set_case(owner_intent, ReceiptMode::Valid);
    let authorized_before = fx.authorized_event_count();
    let inspect_before_attach = fx.sender.inspect_calls.load(SeqCst);
    let (attach_status, attach_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        ATTACH_PATH,
        &attach_body.to_string(),
    );
    assert_eq!(
        attach_status, 200,
        "signed HTTP attach refused: {attach_response}"
    );
    let attached: Value = serde_json::from_str(http_response_body(&attach_response))
        .expect("attach HTTP response is JSON");
    assert_eq!(attached["prepared"]["state"], json!("authorized"));
    assert_eq!(attached["queued"]["state"], json!("queued"));
    let queued_id = attached["queued"]["intent_id"]
        .as_str()
        .expect("attach returns its queued id")
        .to_owned();
    assert_eq!(
        fx.queued_intent_count(&install),
        1,
        "one valid attach queues once"
    );
    assert_eq!(fx.authorized_event_count(), authorized_before + 1);
    assert_eq!(
        fx.sender.inspect_calls.load(SeqCst),
        inspect_before_attach + 1
    );
    assert_eq!(fx.sender.execute_calls.load(SeqCst), 0);

    // The HTTP retry reads the atomic committed handoff. It cannot mint or
    // inspect another grant and cannot add a second queued row.
    let inspect_after_attach = fx.sender.inspect_calls.load(SeqCst);
    let (retry_status, retry_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        ATTACH_PATH,
        &attach_body.to_string(),
    );
    assert_eq!(
        retry_status, 200,
        "idempotent HTTP attach retry refused: {retry_response}"
    );
    let retried: Value = serde_json::from_str(http_response_body(&retry_response))
        .expect("attach retry response is JSON");
    assert_eq!(retried["queued"]["intent_id"], json!(queued_id));
    assert_eq!(fx.queued_intent_count(&install), 1);
    assert_eq!(fx.authorized_event_count(), authorized_before + 1);
    assert_eq!(fx.sender.inspect_calls.load(SeqCst), inspect_after_attach);

    let frozen = fx.store.social_publish_show(&queued_id).unwrap()["intent"]["frozen"].clone();
    let recheck = json!({
        "grant_id":frozen["grant_id"],
        "aos_connection_id":frozen["aos_connection_id"],
        "destination_id":frozen["destination_id"],
        "caption_digest":frozen["caption_digest"],
        "image_digest":frozen["image_digest"],
    });
    let early_epoch = attach_at + 4;
    fx.clock.store(early_epoch, SeqCst);
    fx.driver_clock.store(early_epoch, SeqCst);
    std::thread::sleep(Duration::from_millis(25));
    let early_claim = fx
        .rpc(
            Asserted::Operator,
            "social_publish_claim_due",
            json!({"now_epoch":early_epoch + 3600,"recheck":recheck}),
        )
        .unwrap_or_else(|error| panic!("early claim should refuse without dispatch: {error}"));
    assert_eq!(
        early_claim["claimed"],
        json!(false),
        "caller-forged future time accelerated claim"
    );
    assert_eq!(
        fx.store.social_publish_show(&queued_id).unwrap()["intent"]["state"],
        json!("queued"),
        "the early claim must leave the attached intent cancellable"
    );
    let early_send = fx.rpc(
        Asserted::Operator,
        "social_publish_send_now",
        json!({"intent_id":queued_id,"install_id":install}),
    );
    assert!(
        early_send.is_err(),
        "send-now bypassed the five-second Undo floor"
    );
    assert_eq!(
        fx.sender.execute_calls.load(SeqCst),
        0,
        "early claim/send-now dispatched"
    );

    let cancelled = fx
        .rpc(
            Asserted::Operator,
            "social_publish_cancel",
            json!({"intent_id":queued_id,"install_id":install}),
        )
        .unwrap_or_else(|error| panic!("Undo should still cancel before five seconds: {error}"));
    assert_eq!(cancelled["intent"]["state"], json!("cancelled"));
    fx.clock.store(attach_at + 5, SeqCst);
    fx.driver_clock.store(attach_at + 5, SeqCst);
    std::thread::sleep(Duration::from_millis(25));
    assert_eq!(
        fx.sender.execute_calls.load(SeqCst),
        0,
        "cancelled intent dispatched after Undo"
    );
    assert_eq!(fx.sender.status_calls.load(SeqCst), 0);

    // A second immutable `now` intent is prepared first and attached later.
    // Its signed due/notBefore therefore predates queue commit; only a
    // persisted queue-maturity guard can keep it cancellable through +4.
    let second_prepare_at = attach_at + 5;
    fx.clock.store(second_prepare_at, SeqCst);
    fx.driver_clock.store(second_prepare_at, SeqCst);
    let (second_prepare_status, second_prepare_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        PREPARE_PATH,
        &json!({
            "request_id":"native-owner-http-maturity",
            "run_id":second_run_id,
            "mode":"now",
        })
        .to_string(),
    );
    assert_eq!(
        second_prepare_status, 200,
        "second now prepare refused: {second_prepare_response}"
    );
    let second_prepared: Value = serde_json::from_str(http_response_body(&second_prepare_response))
        .expect("second prepare HTTP response is JSON");
    let second_prepared_id = second_prepared["prepared"]["prepared_id"]
        .as_str()
        .expect("second prepare returns its selector")
        .to_owned();
    let second_owner_intent = second_prepared["prepared"]["owner_intent"].clone();
    assert_eq!(second_prepared["prepared"]["state"], json!("prepared"));
    assert_eq!(second_owner_intent["due_epoch"], json!(second_prepare_at));
    assert_eq!(second_owner_intent["not_before"], json!(second_prepare_at));
    let second_descriptor_digest = fx
        .prepared_rows(&install)
        .into_iter()
        .find(|row| row.0 == second_prepared_id)
        .expect("second descriptor persisted")
        .3;
    let signed_claims_before = fx.sender.signed_claims.lock().unwrap().len();
    let second_attach_body = json!({
        "prepared_id":second_prepared_id,
        "install_id":install,
    });
    let second_attach_at = second_prepare_at + 2;
    fx.clock.store(second_attach_at, SeqCst);
    fx.driver_clock.store(second_attach_at, SeqCst);
    fx.sender
        .set_case(second_owner_intent.clone(), ReceiptMode::Valid);
    let authorized_before_second = fx.authorized_event_count();
    let (second_attach_status, second_attach_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        ATTACH_PATH,
        &second_attach_body.to_string(),
    );
    assert_eq!(
        second_attach_status, 200,
        "second signed attach refused: {second_attach_response}"
    );
    let second_attached: Value = serde_json::from_str(http_response_body(&second_attach_response))
        .expect("second attach HTTP response is JSON");
    assert_eq!(second_attached["prepared"]["state"], json!("authorized"));
    assert_eq!(second_attached["queued"]["state"], json!("queued"));
    let second_queued_id = second_attached["queued"]["intent_id"]
        .as_str()
        .expect("second attach returns queued id")
        .to_owned();
    assert_eq!(fx.queued_intent_count(&install), 1);
    assert_eq!(fx.authorized_event_count(), authorized_before_second + 1);
    assert_eq!(
        fx.sender.signed_claims.lock().unwrap().len(),
        signed_claims_before + 1,
        "one queue inspection signed the attached descriptor"
    );

    // The driver sees an immediately due `now` row whose signed due/notBefore
    // predates queue commit, but at queue+0 and queue+4 it cannot claim or
    // dispatch. The due RPC rejects a forged future clock, while the named
    // send-now path cannot
    // bypass Undo. Cancellation was independently proven above on the first
    // intent; this second one remains live to prove the maturity winner.
    let second_before = fx.store.social_publish_show(&second_queued_id).unwrap();
    let second_frozen_before = second_before["intent"]["frozen"].clone();
    let second_frozen_digest = second_before["intent"]["frozen_digest"].clone();
    std::thread::sleep(Duration::from_millis(25));
    assert_eq!(
        fx.store.social_publish_show(&second_queued_id).unwrap()["intent"]["state"],
        json!("queued"),
        "background driver claimed before the post-queue floor"
    );
    assert_eq!(fx.sender.execute_calls.load(SeqCst), 0);

    let second_recheck = json!({
        "grant_id":second_frozen_before["grant_id"],
        "aos_connection_id":second_frozen_before["aos_connection_id"],
        "destination_id":second_frozen_before["destination_id"],
        "caption_digest":second_frozen_before["caption_digest"],
        "image_digest":second_frozen_before["image_digest"],
    });
    let second_early_epoch = second_attach_at + 4;
    fx.clock.store(second_early_epoch, SeqCst);
    fx.driver_clock.store(second_early_epoch, SeqCst);
    std::thread::sleep(Duration::from_millis(25));
    let second_early_claim = fx
        .rpc(
            Asserted::Operator,
            "social_publish_claim_due",
            json!({"now_epoch":second_early_epoch + 3600,"recheck":second_recheck}),
        )
        .unwrap_or_else(|error| {
            panic!("early second claim should refuse without dispatch: {error}")
        });
    assert_eq!(second_early_claim["claimed"], json!(false));
    assert_eq!(
        fx.store.social_publish_show(&second_queued_id).unwrap()["intent"]["state"],
        json!("queued"),
        "early driver/due claim must leave the second intent queued"
    );
    let second_early_send = fx.rpc(
        Asserted::Operator,
        "social_publish_send_now",
        json!({"intent_id":second_queued_id,"install_id":install}),
    );
    assert!(
        second_early_send.is_err(),
        "named send-now bypassed the second Undo floor"
    );
    assert_eq!(
        fx.sender.execute_calls.load(SeqCst),
        0,
        "a pre-maturity path dispatched"
    );

    // Hold only the driver clock at +4 while the real daemon/operator clock
    // reaches +5, so this RPC wins the same atomic claim race deterministically.
    // The sender is an explicit no-provider fake: one execute call for this
    // winning intent is local claim/dispatch-seam evidence, not provider or
    // process-ancestry proof.
    let mature_epoch = second_attach_at + 5;
    fx.clock.store(mature_epoch, SeqCst);
    let mature_claim = fx
        .rpc(
            Asserted::Operator,
            "social_publish_claim_due",
            json!({"now_epoch":mature_epoch,"recheck":second_recheck}),
        )
        .unwrap_or_else(|error| panic!("eligible maturity claim failed: {error}"));
    // The +5 CAS wins this exact intent; only afterward does the local fake
    // sender refuse, without contacting or executing any real provider.
    assert_eq!(mature_claim["intent"]["intent_id"], json!(second_queued_id));
    assert_eq!(mature_claim["intent"]["state"], json!("refused"));
    assert_eq!(
        fx.sender.execute_calls.load(SeqCst),
        1,
        "maturity claim did not reach the fake exactly once"
    );

    let repeated_claim = fx
        .rpc(
            Asserted::Operator,
            "social_publish_claim_due",
            json!({"now_epoch":mature_epoch,"recheck":second_recheck}),
        )
        .unwrap_or_else(|error| {
            panic!("repeated maturity claim should lose without error: {error}")
        });
    assert_eq!(repeated_claim["claimed"], json!(false));
    let competing_send = fx.rpc(
        Asserted::Operator,
        "social_publish_send_now",
        json!({"intent_id":second_queued_id,"install_id":install}),
    );
    assert!(
        competing_send.is_err(),
        "competing named claim won after maturity claim"
    );
    fx.driver_clock.store(mature_epoch, SeqCst);
    std::thread::sleep(Duration::from_millis(25));
    assert_eq!(
        fx.sender.execute_calls.load(SeqCst),
        1,
        "driver or retry dispatched a second time"
    );

    let second_after = fx.store.social_publish_show(&second_queued_id).unwrap();
    assert_eq!(
        second_after["intent"]["frozen"], second_frozen_before,
        "dispatch changed frozen payload"
    );
    assert_eq!(
        second_after["intent"]["frozen_digest"],
        second_frozen_digest
    );
    let descriptor_digest_after = fx
        .prepared_rows(&install)
        .into_iter()
        .find(|row| row.0 == second_prepared_id)
        .expect("authorized descriptor remains persisted")
        .3;
    assert_eq!(
        descriptor_digest_after, second_descriptor_digest,
        "attach/dispatch changed the descriptor"
    );

    let signed_claims = fx.sender.signed_claims.lock().unwrap();
    let signed = &signed_claims[signed_claims_before];
    assert_eq!(signed["intent_id"], second_owner_intent["intent_id"]);
    assert_eq!(
        signed["intent_digest"],
        second_owner_intent["intent_digest"]
    );
    assert_eq!(signed["due_epoch"], second_owner_intent["due_epoch"]);
    assert_eq!(
        signed["not_before_ms"],
        json!(second_owner_intent["not_before"].as_i64().unwrap() * 1000),
        "signed notBefore changed across attach and dispatch"
    );
    assert_eq!(
        signed["caption_digest"],
        second_frozen_before["caption_digest"]
    );
    assert_eq!(signed["image_digest"], second_frozen_before["image_digest"]);
    drop(signed_claims);

    // Give the named claim_id/send_now CAS a separate now intent so its
    // positive +5 boundary is covered independently of the due-claim winner.
    let third_prepare_at = mature_epoch + 1;
    fx.clock.store(third_prepare_at, SeqCst);
    fx.driver_clock.store(third_prepare_at, SeqCst);
    let (third_prepare_status, third_prepare_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        PREPARE_PATH,
        &json!({
            "request_id":"native-owner-http-send-now-maturity",
            "run_id":second_run_id,
            "mode":"now",
        })
        .to_string(),
    );
    assert_eq!(
        third_prepare_status, 200,
        "named-path maturity prepare refused: {third_prepare_response}"
    );
    let third_prepared: Value = serde_json::from_str(http_response_body(&third_prepare_response))
        .expect("named-path prepare response is JSON");
    let third_prepared_id = third_prepared["prepared"]["prepared_id"]
        .as_str()
        .expect("named-path prepare returns selector")
        .to_owned();
    let third_owner_intent = third_prepared["prepared"]["owner_intent"].clone();
    assert_eq!(third_owner_intent["due_epoch"], json!(third_prepare_at));
    assert_eq!(third_owner_intent["not_before"], json!(third_prepare_at));
    let third_attach_at = third_prepare_at + 2;
    fx.clock.store(third_attach_at, SeqCst);
    fx.driver_clock.store(third_attach_at, SeqCst);
    fx.sender.set_case(third_owner_intent, ReceiptMode::Valid);
    let third_attach_body = json!({
        "prepared_id":third_prepared_id,
        "install_id":install,
    });
    let (third_attach_status, third_attach_response) = board_http_post(
        &fx.dir(),
        &fx.pm(),
        "operator",
        ATTACH_PATH,
        &third_attach_body.to_string(),
    );
    assert_eq!(
        third_attach_status, 200,
        "named-path maturity attach refused: {third_attach_response}"
    );
    let third_attached: Value = serde_json::from_str(http_response_body(&third_attach_response))
        .expect("named-path attach response is JSON");
    let third_queued_id = third_attached["queued"]["intent_id"]
        .as_str()
        .expect("named-path attach returns queued id")
        .to_owned();
    let third_before = fx.store.social_publish_show(&third_queued_id).unwrap();
    let third_frozen = third_before["intent"]["frozen"].clone();
    let third_frozen_digest = third_before["intent"]["frozen_digest"].clone();
    let third_recheck = json!({
        "grant_id":third_frozen["grant_id"],
        "aos_connection_id":third_frozen["aos_connection_id"],
        "destination_id":third_frozen["destination_id"],
        "caption_digest":third_frozen["caption_digest"],
        "image_digest":third_frozen["image_digest"],
    });
    let third_early_epoch = third_attach_at + 4;
    fx.clock.store(third_early_epoch, SeqCst);
    fx.driver_clock.store(third_early_epoch, SeqCst);
    std::thread::sleep(Duration::from_millis(25));
    assert_eq!(
        fx.store.social_publish_show(&third_queued_id).unwrap()["intent"]["state"],
        json!("queued"),
        "driver claimed the named-path intent before queue+5"
    );
    let third_early_send = fx.rpc(
        Asserted::Operator,
        "social_publish_send_now",
        json!({"intent_id":third_queued_id,"install_id":install}),
    );
    assert!(
        third_early_send.is_err(),
        "claim_id/send_now bypassed queue+4"
    );
    assert_eq!(fx.sender.execute_calls.load(SeqCst), 1);
    assert_eq!(
        fx.store.social_publish_show(&third_queued_id).unwrap()["intent"]["state"],
        json!("queued")
    );

    let third_mature_epoch = third_attach_at + 5;
    fx.clock.store(third_mature_epoch, SeqCst);
    let named_winner = fx
        .rpc(
            Asserted::Operator,
            "social_publish_send_now",
            json!({"intent_id":third_queued_id,"install_id":install}),
        )
        .unwrap_or_else(|error| panic!("mature claim_id/send_now should win once: {error}"));
    assert_eq!(named_winner["intent"]["intent_id"], json!(third_queued_id));
    assert_eq!(named_winner["intent"]["state"], json!("refused"));
    assert_eq!(fx.sender.execute_calls.load(SeqCst), 2);
    let repeated_named = fx.rpc(
        Asserted::Operator,
        "social_publish_send_now",
        json!({"intent_id":third_queued_id,"install_id":install}),
    );
    assert!(
        repeated_named.is_err(),
        "repeated named claim dispatched twice"
    );
    let due_competitor = fx
        .rpc(
            Asserted::Operator,
            "social_publish_claim_due",
            json!({"now_epoch":third_mature_epoch,"recheck":third_recheck}),
        )
        .unwrap_or_else(|error| panic!("competing due claim should lose: {error}"));
    assert_eq!(due_competitor["claimed"], json!(false));
    fx.driver_clock.store(third_mature_epoch, SeqCst);
    std::thread::sleep(Duration::from_millis(25));
    assert_eq!(fx.sender.execute_calls.load(SeqCst), 2);
    let third_after = fx.store.social_publish_show(&third_queued_id).unwrap();
    assert_eq!(third_after["intent"]["frozen"], third_frozen);
    assert_eq!(third_after["intent"]["frozen_digest"], third_frozen_digest);

    let executed = fx.sender.executed_bindings.lock().unwrap();
    assert_eq!(executed.len(), 2);
    let binding = &executed[0];
    assert_eq!(
        binding.key,
        second_after["intent"]["request"].as_str().unwrap()
    );
    assert_eq!(
        binding.connection_id,
        second_frozen_before["aos_connection_id"].as_str().unwrap()
    );
    assert_eq!(
        binding.destination_id,
        second_frozen_before["destination_id"].as_str().unwrap()
    );
    assert_eq!(
        binding.toolkit.as_str(),
        second_frozen_before["toolkit"].as_str().unwrap()
    );
    assert_eq!(
        binding.caption_digest,
        second_frozen_before["caption_digest"].as_str().unwrap()
    );
    assert_eq!(
        binding.image_digest.as_deref(),
        second_frozen_before["image_digest"].as_str()
    );
    assert_eq!(
        binding.cadence_run_id,
        second_frozen_before["run_id"].as_str().unwrap()
    );
    assert_eq!(
        binding.cadence_effect_id,
        second_frozen_before["effect_id"].as_str().unwrap()
    );
    assert_eq!(
        binding.grant_id,
        second_frozen_before["grant_id"].as_str().unwrap()
    );

    let named_binding = &executed[1];
    assert_eq!(
        named_binding.key,
        third_after["intent"]["request"].as_str().unwrap()
    );
    assert_eq!(
        named_binding.destination_id,
        third_frozen["destination_id"].as_str().unwrap()
    );
    assert_eq!(
        named_binding.caption_digest,
        third_frozen["caption_digest"].as_str().unwrap()
    );
    assert_eq!(
        named_binding.image_digest.as_deref(),
        third_frozen["image_digest"].as_str()
    );
    assert_eq!(
        named_binding.grant_id,
        third_frozen["grant_id"].as_str().unwrap()
    );
    drop(executed);

    // Account-affinity settings are checked against a fresh local resolver
    // response, not a provider. These assertions cover setting selection/CAS
    // only: READ-visible discovery is not authenticated AOS read/send authority,
    // local binding eligibility, or proof the local binding shares a workspace
    // with the AOS connection. The starting row is legacy account A plus grant;
    // selecting B without a fresh row must leave settings unchanged.
    const ACCOUNT_B_CONNECTION: &str = "connA_river_fb";
    const STALE_ACCOUNT_B_CONNECTION: &str = "connA_old_river_fb";
    const ACCOUNT_B_DESTINATION: &str = "dest-02";
    const ACCOUNT_B_LABEL: &str = "River Page";
    const PRIOR_ACCOUNT_GRANT: &str = "dpq_acceptance_fixture_grant";
    const ACCOUNT_B_GRANT: &str = "dpq_river_account_grant";
    let account_b_row = json!({
        "connectionId": ACCOUNT_B_CONNECTION,
        "toolkit": "facebook",
        "displayName": ACCOUNT_B_LABEL,
        "destinationId": ACCOUNT_B_DESTINATION,
        "status": "active",
        "available": true,
        "publishable": true,
    });
    let bindings = fx
        .rpc(
            Asserted::Operator,
            "app_binding_list",
            json!({"install_id":install}),
        )
        .unwrap_or_else(|error| panic!("read publication binding: {error}"));
    let initial_binding = bindings["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|binding| binding["slot"] == "publication")
        .expect("configured publication binding");
    let binding_id = initial_binding["id"].as_str().unwrap().to_owned();
    let initial_revision = initial_binding["revision"].as_i64().unwrap();
    let prior_publish = initial_binding["config"]["publish"].clone();
    assert_eq!(prior_publish["grant_id"], json!(PRIOR_ACCOUNT_GRANT));
    assert!(
        prior_publish.get("aos_connection_id").is_none()
            || prior_publish["aos_connection_id"].is_null()
    );

    fx.set_destinations(Vec::new());
    let missing_fresh_row = fx.rpc(
        Asserted::Operator,
        "app_binding_publish_set",
        json!({
            "install_id":install,
            "binding_id":binding_id,
            "expected_revision":initial_revision,
            "destination_id":ACCOUNT_B_DESTINATION,
            "destination_label":ACCOUNT_B_LABEL,
            "toolkit":"facebook",
            "timezone":"Europe/London",
            "grant_id":PRIOR_ACCOUNT_GRANT,
            "aos_connection_id":ACCOUNT_B_CONNECTION,
        }),
    );
    assert!(
        missing_fresh_row.is_err(),
        "account selection succeeded without a fresh destination row"
    );
    let unchanged_binding = fx
        .rpc(
            Asserted::Operator,
            "app_binding_show",
            json!({"install_id":install,"binding_id":binding_id}),
        )
        .unwrap();
    assert_eq!(
        unchanged_binding["binding"]["revision"],
        json!(initial_revision)
    );
    assert_eq!(
        unchanged_binding["binding"]["config"]["publish"],
        prior_publish
    );

    // Discovery exposes a READ-visible row only; it does not establish
    // authenticated read/send authority, workspace affinity, or binding
    // eligibility. The setting write below checks this fresh row's current
    // label/connection selector and grant carry rules only; it is not a SEND
    // authorization. The old account grant is not copied, even if repeated.
    fx.set_destinations(vec![account_b_row.clone()]);
    let fresh_rows = fx
        .rpc(
            Asserted::Operator,
            "app_publish_destinations_list",
            json!({"install_id":install}),
        )
        .unwrap_or_else(|error| panic!("read fresh destinations: {error}"));
    assert_eq!(fresh_rows["destinations"].as_array().unwrap().len(), 1);
    assert_eq!(
        fresh_rows["destinations"][0]["destination_id"],
        json!(ACCOUNT_B_DESTINATION)
    );
    assert_eq!(
        fresh_rows["destinations"][0]["connection_id"],
        json!(ACCOUNT_B_CONNECTION)
    );
    assert_eq!(
        fresh_rows["destinations"][0]["display_name"],
        json!(ACCOUNT_B_LABEL)
    );

    let stale_label_set = fx.rpc(
        Asserted::Operator,
        "app_binding_publish_set",
        json!({
            "install_id":install,
            "binding_id":binding_id,
            "expected_revision":initial_revision,
            "destination_id":ACCOUNT_B_DESTINATION,
            "destination_label":"Harbour",
            "toolkit":"facebook",
            "timezone":"Europe/London",
            "aos_connection_id":ACCOUNT_B_CONNECTION,
        }),
    );
    assert!(
        stale_label_set.is_err(),
        "setter accepted the previous account's label for the fresh row"
    );
    let stale_selector_set = fx.rpc(
        Asserted::Operator,
        "app_binding_publish_set",
        json!({
            "install_id":install,
            "binding_id":binding_id,
            "expected_revision":initial_revision,
            "destination_id":ACCOUNT_B_DESTINATION,
            "destination_label":ACCOUNT_B_LABEL,
            "toolkit":"facebook",
            "timezone":"Europe/London",
            "aos_connection_id":STALE_ACCOUNT_B_CONNECTION,
        }),
    );
    assert!(
        stale_selector_set.is_err(),
        "setter accepted a connection selector not present in the fresh row"
    );
    let unchanged_after_stale_selection = fx
        .rpc(
            Asserted::Operator,
            "app_binding_show",
            json!({"install_id":install,"binding_id":binding_id}),
        )
        .unwrap();
    assert_eq!(
        unchanged_after_stale_selection["binding"]["revision"],
        json!(initial_revision)
    );
    assert_eq!(
        unchanged_after_stale_selection["binding"]["config"]["publish"],
        prior_publish
    );

    let switched = fx
        .rpc(
            Asserted::Operator,
            "app_binding_publish_set",
            json!({
                "install_id":install,
                "binding_id":binding_id,
                "expected_revision":initial_revision,
                "destination_id":ACCOUNT_B_DESTINATION,
                "destination_label":ACCOUNT_B_LABEL,
                "toolkit":"facebook",
                "timezone":"Europe/London",
                "grant_id":PRIOR_ACCOUNT_GRANT,
                "aos_connection_id":ACCOUNT_B_CONNECTION,
            }),
        )
        .unwrap_or_else(|error| panic!("select fresh account-B settings: {error}"));
    let switched_publish = &switched["binding"]["config"]["publish"];
    assert_eq!(
        switched_publish["destination_id"],
        json!(ACCOUNT_B_DESTINATION)
    );
    assert_eq!(
        switched_publish["destination_label"],
        json!(ACCOUNT_B_LABEL)
    );
    assert_eq!(switched_publish["toolkit"], json!("facebook"));
    assert_eq!(
        switched_publish["aos_connection_id"],
        json!(ACCOUNT_B_CONNECTION)
    );
    assert!(switched_publish.get("grant_id").is_none() || switched_publish["grant_id"].is_null());
    assert_ne!(switched_publish["grant_id"], json!(PRIOR_ACCOUNT_GRANT));
    let switched_revision = switched["binding"]["revision"].as_i64().unwrap();

    // Seed persisted B-grant fixture data directly only to check same-account
    // carry behavior. This does not mint or validate a grant, prove B is an
    // eligible binding, or authorize a send. Only the same-connection/account
    // timezone refresh may carry B's stored value; account A's grant cannot.
    let mut account_b_config = switched["binding"]["config"].clone();
    account_b_config["publish"]["grant_id"] = json!(ACCOUNT_B_GRANT);
    let seeded_account_b = fx
        .store
        .app_binding_update(&install, &binding_id, switched_revision, &account_b_config)
        .expect("seed persisted account-B grant for refresh acceptance");
    let seeded_revision = seeded_account_b["binding"]["revision"].as_i64().unwrap();
    let refreshed = fx
        .rpc(
            Asserted::Operator,
            "app_binding_publish_set",
            json!({
                "install_id":install,
                "binding_id":binding_id,
                "expected_revision":seeded_revision,
                "destination_id":ACCOUNT_B_DESTINATION,
                "destination_label":ACCOUNT_B_LABEL,
                "toolkit":"facebook",
                "timezone":"Asia/Tokyo",
                "grant_id":PRIOR_ACCOUNT_GRANT,
                "aos_connection_id":ACCOUNT_B_CONNECTION,
            }),
        )
        .unwrap_or_else(|error| panic!("same-account timezone refresh: {error}"));
    let refreshed_publish = &refreshed["binding"]["config"]["publish"];
    assert_eq!(refreshed_publish["timezone"], json!("Asia/Tokyo"));
    assert_eq!(
        refreshed_publish["aos_connection_id"],
        json!(ACCOUNT_B_CONNECTION)
    );
    assert_eq!(
        refreshed_publish["destination_label"],
        json!(ACCOUNT_B_LABEL)
    );
    assert_eq!(refreshed_publish["grant_id"], json!(ACCOUNT_B_GRANT));
    assert_ne!(refreshed_publish["grant_id"], json!(PRIOR_ACCOUNT_GRANT));
    let refreshed_revision = refreshed["binding"]["revision"].as_i64().unwrap();

    let stale_revision_write = fx.rpc(
        Asserted::Operator,
        "app_binding_publish_set",
        json!({
            "install_id":install,
            "binding_id":binding_id,
            "expected_revision":seeded_revision,
            "destination_id":ACCOUNT_B_DESTINATION,
            "destination_label":ACCOUNT_B_LABEL,
            "toolkit":"facebook",
            "timezone":"Asia/Hong_Kong",
            "aos_connection_id":ACCOUNT_B_CONNECTION,
        }),
    );
    assert!(
        stale_revision_write.is_err(),
        "stale binding revision overwrote current settings"
    );
    let after_stale = fx
        .rpc(
            Asserted::Operator,
            "app_binding_show",
            json!({"install_id":install,"binding_id":binding_id}),
        )
        .unwrap();
    assert_eq!(
        after_stale["binding"]["revision"],
        json!(refreshed_revision)
    );
    assert_eq!(
        after_stale["binding"]["config"],
        refreshed["binding"]["config"]
    );

    // Prepare a run against the current binding, then let the fresh row keep
    // the same toolkit/destination but move to another AOS connection. The
    // stored selector mismatch must refuse before PREPARED/effect/queue or
    // sender side effects.
    let bundle = refreshed["binding"]["config"]["bundle_digest"]
        .as_str()
        .expect("binding pins the installed bundle")
        .to_owned();
    let affinity_run = fx.complete_approved_run(&install, &bundle, "run-account-affinity-01");
    let prepared_before_affinity_refusal = fx.prepared_rows(&install);
    let queue_before_affinity_refusal = fx.publish_queue(&install);
    let social_events_before_affinity_refusal = fx.social_publish_event_count();
    let platform_events_before_affinity_refusal =
        fx.store.events_tail(PLATFORM_STREAM, 500).unwrap().len();
    let inspect_before_affinity_refusal = fx.sender.inspect_calls.load(SeqCst);
    let preflight_before_affinity_refusal = fx.sender.preflight_calls.load(SeqCst);
    let execute_before_affinity_refusal = fx.sender.execute_calls.load(SeqCst);
    let status_before_affinity_refusal = fx.sender.status_calls.load(SeqCst);
    fx.set_destinations(vec![json!({
        "connectionId":"connA_reconnected_fb",
        "toolkit":"facebook",
        "displayName":ACCOUNT_B_LABEL,
        "destinationId":ACCOUNT_B_DESTINATION,
        "status":"active",
        "available":true,
        "publishable":true,
    })]);
    let selector_mismatch = fx.rpc(
        Asserted::Operator,
        "app_publish_intent_prepare",
        json!({
            "request_id":"account-affinity-selector-drift",
            "run_id":affinity_run,
            "mode":"now",
        }),
    );
    let selector_error =
        selector_mismatch.expect_err("prepare ignored the changed fresh AOS connection");
    assert!(
        selector_error.to_string().contains("configured account connection changed"),
        "prepare refused for an unrelated reason instead of the stale AOS selector: {selector_error}"
    );
    assert_eq!(fx.prepared_rows(&install), prepared_before_affinity_refusal);
    assert_eq!(fx.publish_queue(&install), queue_before_affinity_refusal);
    assert_eq!(
        fx.social_publish_event_count(),
        social_events_before_affinity_refusal
    );
    assert_eq!(
        fx.store.events_tail(PLATFORM_STREAM, 500).unwrap().len(),
        platform_events_before_affinity_refusal
    );
    assert_eq!(
        fx.sender.inspect_calls.load(SeqCst),
        inspect_before_affinity_refusal
    );
    assert_eq!(
        fx.sender.preflight_calls.load(SeqCst),
        preflight_before_affinity_refusal
    );
    assert_eq!(
        fx.sender.execute_calls.load(SeqCst),
        execute_before_affinity_refusal
    );
    assert_eq!(
        fx.sender.status_calls.load(SeqCst),
        status_before_affinity_refusal
    );

    // Owner launch needs an explicit AOS company slug, valid under the AOS
    // grammar and equal to the single production host label. Missing/invalid
    // slugs never fall back to host, workspace id or frame input; all failures
    // below must precede row/event/queue/sender mutation.
    let portal_prepared_before = fx.prepared_rows(&install);
    let portal_queue_before = fx.publish_queue(&install);
    let portal_events_before = fx.social_publish_event_count();
    let portal_platform_events_before = fx.store.events_tail(PLATFORM_STREAM, 500).unwrap().len();
    let portal_inspect_before = fx.sender.inspect_calls.load(SeqCst);
    let portal_preflight_before = fx.sender.preflight_calls.load(SeqCst);
    let portal_execute_before = fx.sender.execute_calls.load(SeqCst);
    let portal_status_before = fx.sender.status_calls.load(SeqCst);
    let assert_portal_refusal_unchanged = || {
        assert_eq!(fx.prepared_rows(&install), portal_prepared_before);
        assert_eq!(fx.publish_queue(&install), portal_queue_before);
        assert_eq!(fx.social_publish_event_count(), portal_events_before);
        assert_eq!(
            fx.store.events_tail(PLATFORM_STREAM, 500).unwrap().len(),
            portal_platform_events_before
        );
        assert_eq!(fx.sender.inspect_calls.load(SeqCst), portal_inspect_before);
        assert_eq!(
            fx.sender.preflight_calls.load(SeqCst),
            portal_preflight_before
        );
        assert_eq!(fx.sender.execute_calls.load(SeqCst), portal_execute_before);
        assert_eq!(fx.sender.status_calls.load(SeqCst), portal_status_before);
    };
    for (case, host, company_slug, authorize_url) in [
        (
            "host-port",
            "native-owner-company.cadencecloud.app:443",
            Some(OWNER_COMPANY_SLUG),
            OWNER_AUTHORIZE_URL,
        ),
        (
            "custom-host",
            "native-owner-company.example",
            Some(OWNER_COMPANY_SLUG),
            OWNER_AUTHORIZE_URL,
        ),
        (
            "staging-host",
            "native-owner-company-staging.cadencecloud.app",
            Some(OWNER_COMPANY_SLUG),
            OWNER_AUTHORIZE_URL,
        ),
        (
            "slug-cross-check",
            HOST,
            Some("different-company"),
            OWNER_AUTHORIZE_URL,
        ),
        (
            "invalid-explicit-slug",
            HOST,
            Some("Bad_slug"),
            OWNER_AUTHORIZE_URL,
        ),
        (
            "reserved-explicit-slug",
            HOST,
            Some("native-owner-company-staging"),
            OWNER_AUTHORIZE_URL,
        ),
        ("missing-explicit-slug", HOST, None, OWNER_AUTHORIZE_URL),
        ("missing-app-origin", HOST, Some(OWNER_COMPANY_SLUG), ""),
    ] {
        let body = json!({
            "request_id":format!("owner-portal-refusal-{case}"),
            "run_id":run_id,
            "mode":"now",
            "company_slug":"frame-controlled-slug",
            "authorize_url":"https://frame-controlled.invalid/owner-action",
        });
        let (status, response) = board_http_post_with_public(
            &fx.dir(),
            &fx.pm(),
            "operator",
            PREPARE_PATH,
            &body.to_string(),
            Some(owner_public_board_at(
                &fx.dir(),
                host,
                company_slug,
                authorize_url,
            )),
        );
        assert_eq!(
            status, 503,
            "invalid authoritative portal config {case} passed: {response}"
        );
        assert_portal_refusal_unchanged();
    }
}
