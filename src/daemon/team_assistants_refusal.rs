//! CAD-1380 independent refusal acceptance — authored by a worker that is NOT
//! the implementer, from the ticket acceptance items and the frozen contract
//! (`design-plans/team-assistants-contract-v1.md` §5 R1–R10 and
//! `.tmp/team-assistant-research/implementation-interface.md`). The implementer
//! must not edit or weaken this file (AGENTS.md "Gates and security work");
//! `mod team_assistants_refusal;` is registered by the implementer without
//! changing a check.
//!
//! What is exercised (the REAL guards, never a mirrored fake):
//!   * the daemon member plane — `Shared::dispatch` on the `member_*` verbs
//!     (`member_assist_state`, `member_assist_history`, `member_assist_send`,
//!     `member_job_request`, `member_job_list`, `member_job_evidence`,
//!     `member_job_events`), each of which re-verifies the SIGNED
//!     `member_envelope` param (Ed25519 + iss/aud/org/actor/assistant/scope
//!     pins against the daemon's own `operator/board-identity.json` + the
//!     platform JWKS) and re-binds the verified `(org, sub, assistant_id)`
//!     to the stored `assistants` row, admitted only on a proven
//!     operator/board connection;
//!   * effect-time LIVE authority revalidation — `member_authority_live`
//!     pulls `POST {scheme}://{board_host}/__platform/assistant/revalidate`
//!     at every member effect. This fixture serves that endpoint as the
//!     authoritative provider: it decides `allow` from CURRENT member
//!     state (live membership row + member-facing role, active assistant
//!     binding, the recorded issuing credential's liveness + grant
//!     ceiling, the operation/scope binding) — the same decision the real
//!     v2 endpoint computes against D1, exercised there by
//!     `team-assistants.refusal.test.ts`. Here the daemon's obligation is
//!     bound: honor `allow`/`deny`, and fail closed on unreachable /
//!     non-200 / malformed answers and on configured-but-unloadable
//!     custody;
//!   * bearer selection — the daemon prefers an operator-enrolled
//!     `hosted-board` service credential from `platform_credentials`
//!     custody (the least-authority `assist.revalidate` transport for
//!     deferred commits); absent that record the member's signed
//!     envelope is the bearer. Under service transport the member's
//!     provenance stays bound to `(member, assistant, operation)` — the
//!     signed envelope is still re-verified and the service grant can
//!     never substitute for the member's own permission;
//!   * effect-time revocation — a member REMOVED AT THE PLATFORM (the
//!     authority table flips; nothing pokes the local `assistants.state`
//!     flag) is refused at the next commit through the live pull, and
//!     `assistant_revoke` on the stored binding refuses as the local
//!     half of the same guarantee;
//!   * the canonical-job idempotency of `member_job_request` keyed on
//!     `(org, sub, request_key)` (R6).
//!
//! A refused case is counted only when the error names the guard under test
//! AND a whole-tracker byte snapshot did not move — "no effects on refusal",
//! not merely an error label. Each refused case is paired with a positive
//! control in the same fixture so a blanket-fail cannot satisfy it.
//!
//! The assistant-envelope guards (`src/assistant_actor.rs` — the real
//! `verify`, `scope_covers` and `assist-jtis.json` `Spent` set the
//! `/api/assist/<verb>` route calls) are exercised in the `envelope`
//! submodule: real Ed25519 signature + iss/aud/org/time/actor/assistant/scope
//! pin checks plus single-use replay. The socket-level `/api/assist/*`
//! admission is covered end-to-end in
//! `src/ui/team_assistants_http_refusal.rs` (real daemon socket + real board
//! + real platform authority server).
//!
//! CAD-2207 extension (independently authored checks:
//! `operator_cannot_read_or_list_a_member_private_assistant`,
//! `profile_or_alias_label_never_manufactures_member_owner_authority`,
//! `cli_owner_cannot_inspect_a_member_private_assistant_mailbox`,
//! `cad2207_dispatch_send_cannot_queue_for_a_member_assistant_or_route_its_reply`,
//! `cad2207_private_assistant_room_posts_stay_hidden_after_agent_row_removal`,
//! `cad2207_room_membership_and_human_agent_list_refuse_a_private_assistant`,
//! `cad2207_private_effect_input_and_decisions_stay_private`,
//! `cad2207_private_effect_decline_does_not_route_to_a_nonowner_upstream`,
//! `cad2207_room_paging_and_unread_exclude_hidden_posts_without_stranding`,
//! `cad2207_operator_chat_verbs_refuse_private_app_and_thread_surfaces`,
//! `cad2207_unstamped_operator_turn_cannot_reprove_into_a_bound_assistant`,
//! `cad2207_private_message_reconcile_and_cancel_refuse_without_mutation`,
//! `cad2207_task_kickoff_provenance_requires_member_owned_job`,
//! `cad2207_agent_unfence_refuses_private_unknown_recovery`,
//! `cad2207_assist_notice_requires_delivery_receipt_and_live_share`,
//! `cad2207_effect_notice_receipt_is_owner_bound_and_enforced`,
//! `cad2207_effect_alias_reuse_cannot_inherit_the_old_owner`,
//! `cad2207_retained_event_alias_reuse_cannot_read_the_old_owner_stream`,
//! `cad2207_retained_mail_alias_reuse_cannot_read_the_old_owner_mail`,
//! `cad2207_public_update_health_and_profile_lists_withhold_private_runtime`,
//! `cad2207_public_waiting_shape_is_identical_for_private_and_ordinary_work`): the
//! member's personal assistant — its alias, private bootstrap profile,
//! conversation, events and mail — belongs to the VERIFIED (workspace,
//! member). A proven operator connection, an unproven external harness and a
//! DIFFERENT member's bound assistant must not read that alias, its thread,
//! its events or its mail, the operator's generic lists must not carry its
//! row, and a different workspace owner's remote-CLI envelope must not inspect
//! its mailbox. The positive controls are the two verified routes the ticket
//! names: the owner's SIGNED `member_envelope` (the board relay), the owner's
//! own bound assistant connection, the bound principal's own `cli_envelope`,
//! plus the operator's unrelated and explicitly non-member targets. No check
//! asserts refusal wording: a refusal is paired with a positive control that
//! proves the same verb/envelope works for the authorized caller or target.
//! Job/task authorization is deliberately NOT re-tested here: it predates this
//! slice and keeps its own policy, so these checks encode no blanket job/task
//! restriction. `human_agents_list`'s CAD-2207 row filter IS exercised here:
//! the fixture serves the live roster endpoint those checks need. The generic
//! HTTP agent routes are still unexercised (the HTTP fixture drives
//! `/api/assist/*` only) and are reported as unexercised rather than
//! simulated.

#![cfg(all(test, feature = "test-seam"))]

use super::*;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::signature::{Ed25519KeyPair, KeyPair};

/// The ONE process-wide lock arming `HOSTED_CADENCE_ASSISTANT_ENABLED` for
/// the enabled-path fixtures in BOTH independent refusal modules
/// (`daemon::team_assistants_refusal` and `ui::team_assistants_http_refusal`
/// — the UI module imports this guard; two module-local locks would restore
/// the var while the other module's positive case still runs). The env gate
/// in `members_rpc::member_caller` / `assist_route::assistant_enabled` reads
/// the PROCESS env, which `scripts/run-result-tests` scrubs — so every
/// positive case holds this guard for its whole body: the flag is exactly
/// `"true"` while held and the previous value is restored on drop (panic
/// included). Flag-off child probes run in spawned processes with the var
/// absent/`"false"`/garbage and never take this lock. Edition 2021 keeps
/// `set_var`/`remove_var` safe; production default OFF is untouched — this
/// is test-only process env, never a config or seam bypass.
static ASSISTANT_FLAG_LOCK: Mutex<()> = Mutex::new(());

/// Holds `HOSTED_CADENCE_ASSISTANT_ENABLED="true"` under
/// [`ASSISTANT_FLAG_LOCK`] for the guard's lifetime, restoring the prior
/// value (or absence) on drop.
pub(crate) struct AssistantFlagOn {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

impl AssistantFlagOn {
    /// Arms the rollout flag for ONE enabled-path test. Panics on poison
    /// would hide a failing sibling's report — recover and still fail.
    pub(crate) fn arm() -> Self {
        let lock = ASSISTANT_FLAG_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("HOSTED_CADENCE_ASSISTANT_ENABLED");
        std::env::set_var("HOSTED_CADENCE_ASSISTANT_ENABLED", "true");
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for AssistantFlagOn {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(v) => std::env::set_var("HOSTED_CADENCE_ASSISTANT_ENABLED", v),
            None => std::env::remove_var("HOSTED_CADENCE_ASSISTANT_ENABLED"),
        }
    }
}

/// The org (workspace) id this fixture's bindings live under — the
/// `organization_id` pin of every minted envelope and the `company` of the
/// fixture's `board-identity.json`.
const ORG: &str = "c1380-org";
/// The kid every fixture envelope signs under; the platform's JWKS answers it.
const KID: &str = "c1380-daemon-fixture";

/// A loopback port in the isolated fixture range.
fn loopback_server() -> tiny_http::Server {
    (3110..=3199)
        .find_map(|port| tiny_http::Server::http(("127.0.0.1", port)).ok())
        .expect("no free isolated fixture port in 3110..3199")
}

/// The contract's revalidate request fields — parsed strictly like the real
/// v2 `hostedCadenceAssistantRevalidateRequestSchema` so a daemon that sends
/// a wrong-shaped pull is refused, not silently allowed.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RevalidateRequest {
    version: String,
    organization_id: String,
    sub: String,
    assistant_id: String,
    operation: String,
    scope: String,
    resource: RevalidateResource,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RevalidateResource {
    kind: String,
    job_id: Option<String>,
    /// The queued `job_dispatch` effect's OWN recorded provenance
    /// (`assist_requests.credential_id`) — the platform re-proves THIS
    /// credential live for `job_dispatch`, never the binding's latest
    /// pin, so a stale queued effect denied after revocation does NOT
    /// ride a global assistant renewal.
    credential_id: Option<String>,
}

/// The authoritative member record the fixture platform holds — the
/// `workspace_members` + `personalAssistants` + issuing-credential state
/// the real endpoint re-reads. `role` of `None` means the membership row
/// is gone (removal); `credential_live`/`credential_caps` are the recorded
/// issuing bridge credential's CURRENT state and grant ceiling.
#[derive(Clone)]
struct MemberAuthority {
    /// `Some("member")`/`Some("owner")` while the membership row lives.
    role: Option<&'static str>,
    /// The `personalAssistants` binding state for `assist_<sub>` —
    /// active and naming this assistant.
    binding_active: bool,
    /// The binding's recorded issuing credential (`credential_id` — the
    /// durable provenance the real schema's migration 0055 records).
    recorded_credential: String,
    /// The recorded issuing credential is non-revoked, non-expired,
    /// under a live session+membership.
    credential_live: bool,
    /// The recorded issuing credential's CURRENT grant ceiling.
    credential_caps: Vec<String>,
    /// Per-credential live state keyed by credential id — the
    /// `hosted_bridges` rows the real `liveBridge` re-reads. Models BOTH
    /// an original (revoked/expired) op credential and a reconciled
    /// (live) one so a `job_dispatch` re-prove names its own recorded
    /// provenance, never the binding's latest. `(live, grant_caps)`.
    live_credentials: BTreeMap<String, (bool, Vec<String>)>,
}

impl MemberAuthority {
    fn live(sub: &str) -> Self {
        Self {
            role: Some("member"),
            binding_active: true,
            recorded_credential: format!("cred-{sub}"),
            credential_live: true,
            credential_caps: vec![
                "assist.read".into(),
                "assist.write".into(),
                "assist.jobs".into(),
            ],
            live_credentials: {
                let mut m = BTreeMap::new();
                m.insert(
                    format!("cred-{sub}"),
                    (
                        true,
                        vec![
                            "assist.read".to_string(),
                            "assist.write".to_string(),
                            "assist.jobs".to_string(),
                        ],
                    ),
                );
                m
            },
        }
    }
}

/// The dedicated `assist.revalidate` service credential the daemon may
/// hold in `platform_credentials` custody — the platform's record of it.
#[derive(Clone)]
struct ServiceAuthority {
    /// The bearer token the daemon would send (the custody bytes).
    token: String,
    /// The org the service credential is pinned to.
    org: String,
    /// The board audience the service credential is pinned to.
    audience: String,
    /// The service credential's capabilities (`assist.revalidate` required).
    caps: Vec<String>,
    /// Revoked/expired → live=false.
    live: bool,
}

/// The authoritative state the fixture platform answers from. Tests flip
/// fields to move authority — the daemon observes the change ONLY through
/// the revalidate pull, never through a local poke.
#[derive(Default)]
struct Authority {
    members: BTreeMap<String, MemberAuthority>,
    services: Vec<ServiceAuthority>,
    /// The current roster the fixture platform answers for the human-room
    /// guard — `[{subject_id, name, role}]` (CAD-2207 acceptance). Empty by
    /// default: the other fixtures never read the roster endpoint.
    roster: Vec<Value>,
    /// Serve a malformed body instead of the contract shape.
    malformed_next: bool,
    /// Answer a non-200 status.
    status: u16,
    /// Hold the next revalidate request until released (deterministic
    /// barrier for a commit racing an in-flight pull). HTTP side only.
    hold_next: bool,
    /// Set by the server when a held request has arrived.
    held: bool,
    /// Released by the test; the held request then answers from state.
    released: bool,
}

/// The claims the fixture platform recovers from a `wikienv_` member
/// envelope bearer — it re-verifies the signature itself (the real
/// endpoint's member-bearer path), never trusting unsigned fields.
#[derive(serde::Deserialize)]
struct EnvelopeClaims {
    iss: String,
    aud: String,
    sub: String,
    actor: String,
    principal_kind: String,
    role_at_issue: String,
    organization_id: String,
    assistant_id: String,
    scope: Vec<String>,
    credential_id: String,
    iat: i64,
    exp: i64,
    #[allow(dead_code)]
    jti: String,
}

/// A fixture daemon on an isolated state dir + isolated PM, a real
/// `operator/board-identity.json` trust root, and ONE loopback platform
/// server answering both the JWKS fetch and the
/// `/__platform/assistant/revalidate` authority pull — the authoritative
/// provider the contract's live-revalidation speaks to. `Shared::new`
/// seals `provider_env` (`isolate_for_test`), so no provider or ambient
/// credential can leak into an assistant turn.
struct Fx {
    dir: tempfile::TempDir,
    shared: Arc<Shared>,
    key: Ed25519KeyPair,
    /// The platform issuer AND the board host are this one loopback
    /// authority (`localhost:<port>` — `localhost` keeps the contract's
    /// http scheme derivation while staying reachable).
    issuer: String,
    host: String,
    authority: Arc<Mutex<Authority>>,
    /// Wakes a held revalidate pull when the test releases the barrier.
    barrier: Arc<std::sync::Condvar>,
    stop: Arc<AtomicBool>,
    /// CAD-2207: serve each platform request on its own worker thread, so a
    /// test can hold ONE revalidate pull while an independent pull (the
    /// racing commit's own re-prove) is still answered. Off by default: every
    /// other fixture keeps the single-threaded accept loop.
    concurrent: Arc<AtomicBool>,
    /// The workers `concurrent` mode spawned — joined at teardown.
    workers: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
    /// The accept loop, taken and joined at teardown AFTER `stop` so it cannot
    /// enqueue a worker once the handle drain begins.
    platform: Option<std::thread::JoinHandle<()>>,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("c1380-").tempdir().unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();

        let key = Ed25519KeyPair::from_seed_unchecked(&[140; 32]).unwrap();
        let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
        let platform = loopback_server();
        // The platform's address as a `localhost` host — `public_scheme`
        // keeps `http` and the daemon can dial it.
        let host = match platform.server_addr() {
            tiny_http::ListenAddr::IP(a) => format!("localhost:{}", a.port()),
            _ => panic!("fixture platform bound an unexpected address"),
        };
        let issuer = format!("http://{host}");
        let authority: Arc<Mutex<Authority>> = Arc::new(Mutex::new(Authority {
            status: 200,
            ..Authority::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let barrier = Arc::new(std::sync::Condvar::new());
        let concurrent = Arc::new(AtomicBool::new(false));
        let workers: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let platform_thread = {
            let stop = stop.clone();
            let authority = authority.clone();
            let barrier = barrier.clone();
            let issuer = issuer.clone();
            let host = host.clone();
            let concurrent = concurrent.clone();
            let workers = workers.clone();
            std::thread::spawn(move || {
                // The verification key the platform publishes — used to
                // re-verify member-envelope bearers on the revalidate path.
                let verify_key = Ed25519KeyPair::from_seed_unchecked(&[140; 32]).unwrap();
                let verify_public = verify_key.public_key().as_ref().to_vec();
                loop {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(Some(request)) = platform.recv_timeout(Duration::from_millis(200)) {
                        if concurrent.load(Ordering::SeqCst) {
                            // Off the accept loop: the handler may park this
                            // request on the barrier, and the platform must
                            // still answer an INDEPENDENT pull meanwhile
                            // (CAD-2207 share-revocation ordering).
                            let authority = authority.clone();
                            let barrier = barrier.clone();
                            let issuer = issuer.clone();
                            let host = host.clone();
                            let public_key = public_key.clone();
                            let verify_public = verify_public.clone();
                            let handle = std::thread::spawn(move || {
                                Self::answer_platform(
                                    request,
                                    &authority,
                                    &barrier,
                                    &issuer,
                                    &host,
                                    &public_key,
                                    &verify_public,
                                );
                            });
                            workers.lock().unwrap().push(handle);
                        } else {
                            Self::answer_platform(
                                request,
                                &authority,
                                &barrier,
                                &issuer,
                                &host,
                                &public_key,
                                &verify_public,
                            );
                        }
                    }
                }
            })
        };
        crate::board_identity::write_config(
            dir.path(),
            &crate::board_identity::Config {
                host: host.clone(),
                issuer: issuer.clone(),
                company: ORG.to_string(),
            },
        )
        .unwrap();
        Self {
            dir,
            shared,
            key,
            issuer,
            host,
            authority,
            barrier,
            stop,
            concurrent,
            workers,
            platform: Some(platform_thread),
        }
    }

    /// Release the held pull and wake the server thread.
    fn release_barrier(&self) {
        self.authority
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .released = true;
        self.barrier.notify_all();
    }

    /// The fixture platform: `GET /.well-known/agenticos-board-jwks.json`
    /// answers the signing JWKS; `POST /__platform/assistant/revalidate`
    /// answers `{allow}` from CURRENT authoritative state — membership
    /// live + member-facing role, binding active naming the assistant,
    /// the RECORDED issuing credential live with a grant ceiling covering
    /// the requested scope, and (member envelope path) the re-verified
    /// claims' credential provenance + scope. Caller authentication
    /// mirrors the real endpoint: a live `assist.revalidate` service
    /// credential OR the member's own re-verified signed envelope. Fail
    /// closed on anything else.
    fn answer_platform(
        mut request: tiny_http::Request,
        authority: &Mutex<Authority>,
        barrier: &std::sync::Condvar,
        issuer: &str,
        host: &str,
        jwks_public: &str,
        verify_public: &[u8],
    ) {
        let url = request.url().to_string();
        let path = url.split('?').next().unwrap_or("/");
        fn respond(request: tiny_http::Request, status: u16, body: Value) {
            let response = tiny_http::Response::from_string(body.to_string())
                .with_status_code(tiny_http::StatusCode(status))
                .with_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                );
            let _ = request.respond(response);
        }
        if path == "/.well-known/agenticos-board-jwks.json" {
            respond(
                request,
                200,
                json!({"keys":[{"kty":"OKP","crv":"Ed25519","kid":KID,"x":jwks_public}]}),
            );
            return;
        }
        // CAD-2207 acceptance: the human-room guard reads the live roster
        // from this configured public host. The fixture answers the exact
        // `Roster` DTO; membership/role come from `Authority.roster`.
        if path == "/__platform/threads/roster" {
            let roster = authority.lock().unwrap().roster.clone();
            respond(
                request,
                200,
                json!({
                    "issuer": issuer,
                    "organization_id": ORG,
                    "members": roster,
                }),
            );
            return;
        }
        if path != "/__platform/assistant/revalidate" {
            respond(request, 404, json!({"ok":false,"code":"not_found"}));
            return;
        }
        // Deterministic barrier: park this pull until the test releases it,
        // so a commit can land while the HTTP answer is still in flight.
        {
            let mut guard = authority.lock().unwrap();
            if guard.hold_next {
                guard.hold_next = false;
                guard.held = true;
                // Event-driven: wait on the release signal, no polling.
                guard = barrier.wait_while(guard, |g| !g.released).unwrap();
                guard.released = false;
            }
        }
        // Forced failure modes — the daemon must fail closed on each.
        {
            let guard = authority.lock().unwrap();
            if guard.malformed_next {
                drop(guard);
                let response = tiny_http::Response::from_string("this is not json")
                    .with_status_code(tiny_http::StatusCode(200));
                let _ = request.respond(response);
                return;
            }
            if guard.status != 200 {
                let status = guard.status;
                drop(guard);
                respond(request, status, json!({"ok":false,"code":"unavailable"}));
                return;
            }
        }
        // Read the bounded body.
        let mut body = Vec::new();
        if request
            .as_reader()
            .take(64 * 1024)
            .read_to_end(&mut body)
            .is_err()
        {
            respond(request, 400, json!({"ok":false,"code":"invalid_request"}));
            return;
        }
        let input: RevalidateRequest = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => {
                respond(request, 400, json!({"ok":false,"code":"invalid_request"}));
                return;
            }
        };
        // The strict contract: exact version, this org, an allowlisted
        // operation, its required scope (the operation→scope refine), the
        // member's own assistant resource. The operation union mirrors the
        // real schema's `HOSTED_CADENCE_ASSIST_REVALIDATE_OPERATIONS` — the
        // member-facing verbs PLUS the deferred-commit-only `job_dispatch`
        // (`assist.jobs` scope), which `/call` can never name but the
        // daemon's queued-effect re-prove carries.
        let scope_ok = if input.operation == "job_dispatch" {
            input.scope == "assist.jobs"
        } else {
            crate::assistant_actor::AssistVerb::of(&input.operation).is_some_and(|v| {
                let want = match v.scope() {
                    crate::assistant_actor::AssistScope::Read => "assist.read",
                    crate::assistant_actor::AssistScope::Write => "assist.write",
                    crate::assistant_actor::AssistScope::Jobs => "assist.jobs",
                };
                input.scope == want
            })
        };
        if input.version != "hosted-cadence-assistant.v1"
            || input.organization_id != ORG
            || !scope_ok
            || input.resource.kind != "assistant"
        {
            respond(request, 400, json!({"ok":false,"code":"invalid_request"}));
            return;
        }
        // `job_id` is admitted under the job-evidence verbs AND the
        // deferred `job_dispatch`/`job_subscribe` re-prove. The platform
        // answers the member's LIVE authority; the per-job entitlement
        // (owner-share / subscriber) is the daemon-local ACL the handlers
        // check after the pull — the platform `job_id` is a resource
        // pin, not the ACL itself.
        let job_ok = input.resource.job_id.is_none()
            || matches!(
                input.operation.as_str(),
                "job_evidence"
                    | "job_events"
                    | "job_dispatch"
                    | "job_subscribe"
                    | "job_share"
                    | "job_reconcile"
            );
        // Op-bound provenance: when the request names an op's own credential
        // (the queued dispatch's `assist_requests` row, or the verified incoming
        // envelope credential on a member verb), the platform re-proves THAT
        // credential live with the requested scope in its grant ceiling. It is
        // never the binding's latest pin and never a fallback, so a revoked C1
        // stays denied after a renewal re-pins the binding to C2. `job_dispatch`
        // MUST name one (fail closed). A no-CID native background check keeps
        // the current-binding rule below.
        let op_provenance_ok = match input.resource.credential_id.as_deref() {
            None => input.operation != "job_dispatch",
            Some(cred) => {
                let guard = authority.lock().unwrap();
                guard
                    .members
                    .get(&input.sub)
                    .and_then(|m| m.live_credentials.get(cred))
                    .is_some_and(|(live, caps)| *live && caps.iter().any(|c| c == &input.scope))
            }
        };

        let bearer = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Authorization"))
            .map(|h| h.value.as_str().to_string())
            .unwrap_or_default();

        let member = {
            let guard = authority.lock().unwrap();
            guard.members.get(&input.sub).cloned()
        };

        // Caller authentication: the member's own re-verified signed
        // envelope (member re-proving self) OR a live `assist.revalidate`
        // service credential (the daemon's custody bearer). The service
        // path authenticates the TRANSPORT only — the target member's
        // permission is still computed from the member's own live state
        // and recorded issuing credential below, never the service's
        // grant.
        let (envelope_credential, envelope_scopes): (Option<String>, Vec<String>) =
            if let Some(env) = bearer.strip_prefix("Bearer wikienv_") {
                match Self::verify_envelope(env, verify_public, issuer, host) {
                    Some(claims)
                        if claims.sub == input.sub
                            && claims.assistant_id == input.assistant_id
                            && claims.organization_id == input.organization_id =>
                    {
                        (Some(claims.credential_id), claims.scope)
                    }
                    _ => {
                        respond(request, 401, json!({"ok":false,"code":"unauthorized"}));
                        return;
                    }
                }
            } else {
                let token = bearer.strip_prefix("Bearer ").unwrap_or("");
                let guard = authority.lock().unwrap();
                let service = guard.services.iter().find(|s| s.token == token).cloned();
                drop(guard);
                match service {
                    Some(service)
                        if service.live
                            && service.org == input.organization_id
                            && service.audience == format!("http://{host}")
                            && service.caps.iter().any(|c| c == "assist.revalidate") =>
                    {
                        (None, Vec::new())
                    }
                    _ => {
                        respond(request, 401, json!({"ok":false,"code":"unauthorized"}));
                        return;
                    }
                }
            };

        // `allow` from CURRENT member state — never a stale signed
        // assertion or membership-existence alone:
        //   live membership row AND current role still member-facing
        //   (member|owner — the mint admits both, so both revalidate);
        //   active binding naming this assistant_id;
        //   the binding's RECORDED issuing credential still live AND its
        //   CURRENT grant ceiling covers the requested scope — under
        //   EITHER caller path (a service credential proves the
        //   transport, never the member's permission);
        //   member-envelope path: the envelope's credential_id IS the
        //   binding's recorded provenance AND its scope also covers.
        let allow = member.is_some_and(|m| {
            let role_ok = matches!(m.role, Some("member") | Some("owner"));
            let credential_ok =
                m.credential_live && m.credential_caps.iter().any(|c| c == &input.scope);
            let provenance_ok = match &envelope_credential {
                Some(cred) => {
                    cred == &m.recorded_credential
                        && envelope_scopes.iter().any(|s| s == &input.scope)
                }
                None => true,
            };
            role_ok
                && m.binding_active
                && input.assistant_id == format!("assist_{}", input.sub)
                && credential_ok
                && provenance_ok
                && job_ok
                && op_provenance_ok
        });
        respond(request, 200, json!({"allow": allow}));
    }

    /// Re-verify a member envelope's signature + the pins the platform
    /// pins (iss/aud/org/actor/assistant/kind/scope vocabulary/time). The
    /// authoritative provider's own verify — the real endpoint's member
    /// path, not a field trust.
    fn verify_envelope(
        envelope: &str,
        verify_public: &[u8],
        issuer: &str,
        host: &str,
    ) -> Option<EnvelopeClaims> {
        let parts: Vec<&str> = envelope.split('.').collect();
        if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
            return None;
        }
        let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).ok()?).ok()?;
        if header["alg"] != "EdDSA"
            || header["typ"] != "agenticos-assistant-actor/1"
            || header["kid"] != KID
        {
            return None;
        }
        let claims: EnvelopeClaims =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).ok()?).ok()?;
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).ok()?;
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, verify_public)
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .ok()?;
        let now = crate::issue::time::now_epoch();
        if claims.iss != issuer
            || claims.aud != format!("http://{host}")
            || claims.organization_id != ORG
            || claims.iat > now + 30
            || claims.exp <= now
            || claims.exp - claims.iat > 300
            || claims.principal_kind != "user"
            || claims.actor != format!("users/{}", claims.sub)
            || claims.assistant_id != format!("assist_{}", claims.sub)
            || !matches!(claims.role_at_issue.as_str(), "member" | "owner")
            || claims
                .scope
                .iter()
                .any(|s| !matches!(s.as_str(), "assist.read" | "assist.write" | "assist.jobs"))
        {
            return None;
        }
        Some(claims)
    }

    fn pm(&self) -> PathBuf {
        self.dir.path().join("pm")
    }
    fn call(&self, who: Asserted, method: &str, params: Value) -> Result<Value> {
        scoped(who, || {
            self.shared.dispatch(method, &params, std::process::id())
        })
    }
    /// A proven operator/board connection — the only channel
    /// `member_envelope` is honored on.
    fn op(&self, method: &str, params: Value) -> Value {
        self.call(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }
    fn op_err(&self, method: &str, params: Value) -> Result<Value> {
        self.call(Asserted::Operator, method, params)
    }

    /// Flip the member's authoritative platform state — a removal, a
    /// demotion, a credential revocation or a grant narrowing the real
    /// `workspace_members`/`personalAssistants`/`hosted_bridges` rows
    /// would carry. The daemon sees it only through the revalidate pull.
    fn set_member<F: FnOnce(&mut MemberAuthority)>(&self, sub: &str, f: F) {
        let mut guard = self.authority.lock().unwrap();
        let member = guard
            .members
            .entry(sub.to_string())
            .or_insert_with(|| MemberAuthority::live(sub));
        f(member);
        // Keep the per-credential live map in sync with the recorded
        // credential's flipped state — `live_credentials[recorded]` is
        // what a `job_dispatch` re-prove reads.
        let rec = member.recorded_credential.clone();
        member.live_credentials.insert(
            rec,
            (member.credential_live, member.credential_caps.clone()),
        );
    }
    /// Remove the member's authoritative platform row — the removal.
    fn remove_member(&self, sub: &str) {
        let mut guard = self.authority.lock().unwrap();
        if let Some(member) = guard.members.get_mut(sub) {
            member.role = None;
            member.binding_active = false;
        }
    }
    /// Enroll an `assist.revalidate` service credential the way the
    /// operator's `platform enroll` would land it in daemon custody —
    /// the `platform_credentials` row plus the custody bytes the real
    /// `member_authority_live` loads. `platform_enroll`'s account
    /// identifier grammar cannot carry a `host:port`, so the fixture
    /// writes the same two artifacts the enroll path commits (row +
    /// custody file) — the read path under test is unchanged.
    fn enroll_service(&self, service: ServiceAuthority) {
        self.authority
            .lock()
            .unwrap()
            .services
            .push(service.clone());
        let key = crate::platform::custody::Key {
            platform: "hosted-board",
            account: &self.host,
        };
        crate::platform::custody::Custody::open(self.dir.path())
            .unwrap()
            .put(&key, service.token.as_bytes())
            .unwrap();
        let fingerprint = crate::secret::fingerprint(service.token.as_bytes());
        let host = self.host.clone();
        self.shared
            .store
            .fixture_write(|tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO platform_credentials
                     (platform,account,scopes,fingerprint,custody,exchange,enrolled_at,by,connection_id,credential_revision)
                     VALUES('hosted-board',?1,'[\"assist.revalidate\"]',?2,'file','token',1.0,'operator','conn-hosted-board',1)",
                    rusqlite::params![host, fingerprint],
                )?;
                Ok(())
            })
            .unwrap();
    }

    /// Mint a signed `agenticos-assistant-actor/1` envelope for `sub`.
    /// `claims`/`header` overrides forge exactly one field; everything else
    /// is the accepted shape a member envelope carries. The daemon
    /// re-verifies signature + every pin against its own config + JWKS.
    fn envelope(&self, sub: &str, header: Value, claims: Value) -> String {
        let now = crate::issue::time::now_epoch();
        let mut h = json!({"alg":"EdDSA","typ":"agenticos-assistant-actor/1","kid":KID});
        if let Some(o) = header.as_object() {
            for (k, v) in o {
                h[k] = v.clone();
            }
        }
        let mut c = json!({
            "iss": self.issuer, "aud": format!("http://{}", self.host),
            "sub": sub, "actor": format!("users/{sub}"),
            "principal_kind": "user", "role_at_issue": "member",
            "organization_id": ORG, "assistant_id": format!("assist_{sub}"),
            "scope": ["assist.read","assist.write","assist.jobs"],
            "credential_id": format!("cred-{sub}"),
            "iat": now, "exp": now + 120,
            "jti": format!("jti-{sub}-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
        });
        if let Some(o) = claims.as_object() {
            for (k, v) in o {
                c[k] = v.clone();
            }
        }
        let signed = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&h).unwrap()),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&c).unwrap())
        );
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(self.key.sign(signed.as_bytes()).as_ref())
        )
    }

    /// Provision a real assistant binding for member `sub` and register the
    /// member's authoritative platform state as live — the member's
    /// recorded issuing credential is `cred-<sub>`, matching the envelope
    /// `credential_id` the fixture mints.
    fn provision(&self, sub: &str) -> String {
        let assistant_id = format!("assist_{sub}");
        let alias = format!("assist-agent-{sub}");
        self.shared
            .store
            .assistant_bind(ORG, sub, &assistant_id, &alias, Some("rev0"))
            .unwrap_or_else(|e| panic!("provision {sub}: {e}"));
        self.authority
            .lock()
            .unwrap()
            .members
            .insert(sub.to_string(), MemberAuthority::live(sub));
        self.envelope(sub, json!({}), json!({}))
    }
    /// A fresh signed envelope for an already-provisioned member.
    fn member_envelope(&self, sub: &str) -> String {
        self.envelope(sub, json!({}), json!({}))
    }
    /// Revoke the member's LOCAL binding — the stored-authority half of a
    /// removal (the platform half is `remove_member`).
    fn revoke(&self, sub: &str) {
        self.shared
            .store
            .assistant_revoke(ORG, sub)
            .unwrap_or_else(|e| panic!("revoke {sub}: {e}"));
    }
    /// Whole-tracker byte snapshot — the "no effect" probe.
    fn tree(&self) -> BTreeMap<String, Vec<u8>> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                if rel == ".git" {
                    continue;
                }
                if p.is_dir() {
                    out.insert(format!("{rel}/"), vec![]);
                    walk(base, &p, out);
                } else {
                    out.insert(rel, std::fs::read(&p).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.pm(), &self.pm(), &mut out);
        out
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        // Order matters. Stop the accept loop and release any parked pull
        // FIRST, then join the loop, and only then drain the worker handles: a
        // drain that ran before the loop stopped could miss a worker the loop
        // enqueued in between, leaking that thread. Nothing is held while
        // joining, and a lock poisoned by a panicking test is RECOVERED rather
        // than panicked on again — a second panic during unwinding would abort
        // the whole test binary instead of reporting one failure.
        self.stop.store(true, Ordering::SeqCst);
        {
            let mut guard = self
                .authority
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            guard.released = true;
        }
        self.barrier.notify_all();
        if let Some(platform) = self.platform.take() {
            let _ = platform.join();
        }
        let workers: Vec<_> = {
            let mut guard = self
                .workers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *guard)
        };
        for worker in workers {
            let _ = worker.join();
        }
    }
}

fn err_text(r: Result<Value>, case: &str) -> String {
    match r {
        Err(e) => e.to_string(),
        Ok(v) => panic!("{case} was accepted: {v}"),
    }
}

// ============================================================================
// Rollout gate (CON-002 / R6-1) — `HOSTED_CADENCE_ASSISTANT_ENABLED` must be
// exactly `"true"`; absent/false/unknown is OFF. `member_caller` refuses
// EVERY `member_*` verb BEFORE the envelope is read — a stale envelope, a
// cached service bearer or a replayed member op can never reach a member
// effect while the feature is off. The flag is PROCESS env: each enabled-
// path test arms it through [`AssistantFlagOn::arm`] (the shared lock + the
// prior value restored on drop — `scripts/run-result-tests` scrubs the var,
// so the suite must arm it itself). The OFF cases therefore run in
// ISOLATED CHILD PROCESSES (the coordinator's scoped-isolation
// requirement): this binary respawned with the flag absent/false/garbage,
// unaffected by an armed parent because each child's env is its own.
// ============================================================================

/// Child-side probe, `#[ignore]`d so it never runs in the main suite; the
/// parent driver respawns it with the flag absent, `"false"` and garbage.
/// The availability gate is retired (CAD-1420), so the flag value is not an
/// expectation. The AUTHORITY guard must refuse an envelope whose sub has no
/// stored binding, on every `member_*` verb including `member_job_reconcile`,
/// with the real binding refusal reason and no tracker effect. The bound
/// member's own envelope is the positive control and must resolve to its own
/// private binding.
#[test]
#[ignore]
fn member_verbs_refuse_unbound_envelope_child() {
    let fx = Fx::new();
    let env = fx.provision("sub-alice");
    let ghost = fx.envelope("sub-ghost", json!({}), json!({}));
    let before = fx.tree();
    for (method, params) in member_verbs(&ghost) {
        let e = err_text(
            fx.op_err(method, params),
            &format!("{method} with an unbound member envelope"),
        );
        assert!(
            e.contains("no assistant binding for this member"),
            "{method} unbound envelope refused for another reason: {e}"
        );
    }
    let e = err_text(
        fx.op_err(
            "member_job_reconcile",
            json!({"member_envelope": ghost, "job": "j1", "expected_authority_rev": "1.0"}),
        ),
        "member_job_reconcile with an unbound member envelope",
    );
    assert!(
        e.contains("no assistant binding for this member"),
        "member_job_reconcile unbound envelope refused for another reason: {e}"
    );
    assert_eq!(
        fx.tree(),
        before,
        "an unbound member envelope had an effect"
    );
    let ok = fx
        .op_err("member_assist_state", json!({"member_envelope": env}))
        .unwrap_or_else(|e| panic!("bound member refused: {e}"));
    assert_eq!(
        ok["assistant"]["sub"].as_str(),
        Some("sub-alice"),
        "bound member did not resolve its own binding: {ok}"
    );
    assert_eq!(
        ok["assistant"]["assistant_id"].as_str(),
        Some("assist_sub-alice"),
        "bound member resolved the wrong assistant: {ok}"
    );
    // Legacy non-assistant operator surface unaffected by the flag —
    // `daemon_info` is a proven-operator call that must still answer.
    let info = fx.op("daemon_info", json!({}));
    assert!(info.is_object(), "daemon_info broke under the flag: {info}");
}

/// Parent driver: respawns THIS test binary with the flag absent / `"false"`
/// / garbage and runs the ignored child probe in each; the child's exit code
/// is the verdict. No in-process env mutation, so the enabled suite can never
/// observe a torn flag.
#[test]
fn member_verbs_refuse_unbound_envelope_any_flag() {
    for (label, flag) in [
        ("absent", None),
        ("false", Some("false")),
        ("garbage", Some("not-a-bool")),
    ] {
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "daemon::team_assistants_refusal::member_verbs_refuse_unbound_envelope_child",
            "--exact",
            "--include-ignored",
            "--test-threads",
            "1",
            "--nocapture",
        ]);
        match flag {
            Some(v) => {
                cmd.env("HOSTED_CADENCE_ASSISTANT_ENABLED", v);
            }
            None => {
                cmd.env_remove("HOSTED_CADENCE_ASSISTANT_ENABLED");
            }
        }
        let out = cmd.output().expect("flag-off child never ran");
        assert!(
            out.status.success(),
            "flag {label}: child probe failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The member verbs and a parameter set that is valid in shape, so the
/// caller/binding guard — not the parameter allowlist — decides.
fn member_verbs(env: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("member_assist_state", json!({"member_envelope": env})),
        ("member_assist_history", json!({"member_envelope": env})),
        (
            "member_assist_send",
            json!({"member_envelope": env, "text": "hi"}),
        ),
        (
            "member_job_request",
            json!({"member_envelope": env, "title": "t", "request_key": "k", "body": "b"}),
        ),
        ("member_job_list", json!({"member_envelope": env})),
        (
            "member_job_evidence",
            json!({"member_envelope": env, "job": "j1"}),
        ),
        (
            "member_job_events",
            json!({"member_envelope": env, "job": "j1"}),
        ),
    ]
}

// ============================================================================
// Member-plane refusals
// ============================================================================

/// An agent or unproven caller can never reach a member effect, even carrying
/// a well-formed signed `member_envelope` — `member_caller` proves the
/// operator/board relay channel FIRST, so the member scope cannot widen a
/// non-operator caller.
#[test]
fn member_verbs_refuse_agent_and_unproven_callers() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let env = fx.provision("sub-alice");
    let before = fx.tree();
    for (method, params) in member_verbs(&env) {
        for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
            let e = err_text(
                fx.call(who.clone(), method, params.clone()),
                &format!("{who:?} {method}"),
            );
            assert!(
                e.contains("operator")
                    || e.contains("member")
                    || e.contains("refused")
                    || e.contains("unauthorized")
                    || e.contains("prove"),
                "{who:?} {method} refused for an unrecognised reason: {e}"
            );
        }
    }
    assert_eq!(fx.tree(), before, "a non-operator caller moved the tracker");
}

/// A `member_envelope` whose verified claims do not re-bind to a real stored
/// row — no such `(org, sub)` binding, a mismatched assistant, a mismatched
/// org, an unsigned/wrong-family/operator-spelling envelope — must never
/// resolve on a member verb. Positive control: the provisioned member's own
/// envelope resolves.
#[test]
fn forged_member_envelope_never_resolves_a_member_effect() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.provision("sub-alice");
    let _bob = fx.provision("sub-bob");
    let before = fx.tree();
    for forged in [
        // A valid envelope for a member with no stored binding.
        fx.envelope("sub-ghost", json!({}), json!({})),
        // alice's sub under bob's assistant id — `assistant_id` ≠ `assist_<sub>`.
        fx.envelope(
            "sub-alice",
            json!({}),
            json!({"assistant_id":"assist_sub-bob"}),
        ),
        // alice's envelope naming a foreign org.
        fx.envelope(
            "sub-alice",
            json!({}),
            json!({"organization_id":"other-org"}),
        ),
        // assistant_id without the assist_ derivation.
        fx.envelope(
            "sub-alice",
            json!({}),
            json!({"assistant_id":"not_an_assistant"}),
        ),
        // Not a JWS at all.
        "not-an-envelope".to_string(),
        // An operator-spelling actor on the member family — never mintable.
        fx.envelope(
            "sub-alice",
            json!({}),
            json!({"actor":"operator","assistant_id":"assist_sub-alice"}),
        ),
        String::new(),
    ] {
        let e = err_text(
            fx.op_err("member_assist_state", json!({"member_envelope": forged})),
            "member_assist_state forged member_envelope",
        );
        assert!(
            e.contains("member_envelope")
                || e.contains("envelope")
                || e.contains("binding")
                || e.contains("assistant")
                || e.contains("member")
                || e.contains("refused"),
            "forged member_envelope refused for another reason: {e}"
        );
    }
    assert_eq!(
        fx.tree(),
        before,
        "a forged member_envelope changed the tracker"
    );
    // Positive control — alice's own provisioned binding resolves on a read.
    let ok = fx.op_err(
        "member_assist_state",
        json!({"member_envelope": fx.member_envelope("sub-alice")}),
    );
    assert!(ok.is_ok(), "alice's own bound assistant refused: {ok:?}");
}

/// Member A must not read or steer member B's assistant, nor read a job A
/// neither owns nor subscribes — the claims-pin and the owner/subscriber
/// ACL are the cross-principal guards.
#[test]
fn member_a_cannot_read_member_b_assistant_or_conversation() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.provision("sub-alice");
    fx.provision("sub-bob");
    let before = fx.tree();
    // Case 1: a member_envelope whose sub/assistant pair is inconsistent —
    // refused at verify, before any binding lookup.
    let mismatched = fx.envelope(
        "sub-alice",
        json!({}),
        json!({"assistant_id":"assist_sub-bob"}),
    );
    for method in [
        "member_assist_state",
        "member_assist_history",
        "member_assist_send",
    ] {
        let mut params = json!({"member_envelope": mismatched});
        if method == "member_assist_send" {
            params["text"] = json!("alice steering bob");
        }
        let e = err_text(fx.op_err(method, params), &format!("cross-member {method}"));
        assert!(
            e.contains("member_envelope")
                || e.contains("envelope")
                || e.contains("assistant")
                || e.contains("binding")
                || e.contains("member")
                || e.contains("refused"),
            "cross-member {method} refused for another reason: {e}"
        );
    }
    // Case 2: B's own bound assistant can request a job; A cannot read its
    // evidence or event feed.
    let bob = fx.member_envelope("sub-bob");
    let job = fx.op(
        "member_job_request",
        json!({"member_envelope": bob, "title": "b job", "request_key": "b-k1", "body": "x"}),
    );
    let job_id = job["job"].as_str().unwrap().to_string();
    let alice = fx.member_envelope("sub-alice");
    for method in ["member_job_evidence", "member_job_events"] {
        let e = err_text(
            fx.op_err(method, json!({"member_envelope": alice, "job": job_id})),
            &format!("alice reads bob's {method}"),
        );
        assert!(
            e.contains("owns")
                || e.contains("subscribe")
                || e.contains("refused")
                || e.contains("member"),
            "alice on bob's job {method} refused for another reason: {e}"
        );
    }
    assert_eq!(
        fx.tree(),
        before,
        "a cross-member access changed the tracker"
    );
}

/// A member-bound `member_envelope` must never satisfy an operator-only
/// daemon verb — member scope cannot widen into operator authority.
#[test]
fn member_scope_never_satisfies_an_operator_only_verb() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let bound = fx.provision("sub-alice");
    let before = fx.tree();
    for method in [
        "approval_record_shown", // operator merge approval (CAD-1218)
        "agent_unfence",         // operator agent control
        "models_default_set",    // operator model config
    ] {
        let e = err_text(
            fx.op_err(method, json!({"member_envelope": bound})),
            &format!("member-scoped {method}"),
        );
        assert!(
            !e.is_empty(),
            "member-scoped {method} produced an empty refusal"
        );
    }
    assert_eq!(fx.tree(), before);
}

/// Effect-time revocation (acceptance 7 / R4 / RISK-001): once the member's
/// authority is gone, every member effect refuses at commit — including a
/// request that was admitted while the member was live.
///
/// Two removal mechanisms are exercised independently:
///   (a) the AUTHORITATIVE removal — the platform's `workspace_members`
///       row goes away (`remove_member` flips the fixture authority; the
///       local `assistants.state` stays `active`), and the daemon's live
///       pull must answer `allow:false` and refuse the next commit. This
///       is the removed-member-after-enqueue guarantee proven at the real
///       authority surface, not by injecting a local revoke;
///   (b) the stored-binding removal — `assistant_revoke` flips
///       `assistants.state` (the daemon-side record a platform revocation
///       delivery would land), and every effect refuses on `is_active`.
#[test]
fn removed_member_queued_effect_refused_at_commit() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.provision("sub-carol");
    // Positive control: while the member is live at the platform the member
    // read resolves and a job request is admitted.
    assert!(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-carol")})
        )
        .is_ok(),
        "live member refused before removal"
    );
    let admitted = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-carol"), "title": "c job", "request_key": "c-k1", "body": "x"}),
    );
    assert!(
        admitted["job"].is_string(),
        "live member job_request: {admitted}"
    );

    // (a) The authoritative removal: the platform's membership row goes —
    // the daemon's own `assistants.state` stays `active`, so ONLY the live
    // pull can refuse the queued/late effect.
    fx.remove_member("sub-carol");
    let before = fx.tree();
    for (method, params) in [
        ("member_assist_state", json!({})),
        ("member_assist_history", json!({})),
        ("member_assist_send", json!({"text": "late"})),
        (
            "member_job_request",
            json!({"title": "late", "request_key": "c-k2", "body": "x"}),
        ),
        ("member_job_list", json!({})),
    ] {
        let mut p = params.clone();
        p["member_envelope"] = json!(fx.member_envelope("sub-carol"));
        let e = err_text(
            fx.op_err(method, p),
            &format!("platform-removed member {method}"),
        );
        assert!(
            e.contains("revalidation")
                || e.contains("authority")
                || e.contains("allow")
                || e.contains("member")
                || e.contains("refused"),
            "platform-removed member {method} refused for another reason: {e}"
        );
    }
    assert_eq!(
        fx.tree(),
        before,
        "a platform-removed member's effect was committed"
    );

    // (b) The stored-binding removal — re-register the member live at the
    // platform so ONLY the local binding gate stands between the effect
    // and commit.
    fx.provision("sub-dana");
    fx.revoke("sub-dana");
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-dana")}),
        ),
        "locally-revoked member state",
    );
    assert!(
        e.contains("revoked") || e.contains("binding") || e.contains("refused"),
        "locally-revoked member refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);

    // A revoked binding is NOT revived by re-provisioning.
    let revive = fx.shared.store.assistant_bind(
        ORG,
        "sub-dana",
        "assist_sub-dana",
        "assist-agent-sub-dana",
        Some("rev1"),
    );
    assert!(
        revive.is_err(),
        "a revoked binding was silently revived by assistant_bind"
    );
}

/// A member whose ISSUING CREDENTIAL is revoked or whose grant is narrowed
/// after the envelope was minted must be refused at commit — a ≤300 s
/// signed proof never substitutes for the member's current permitted
/// provenance. The endpoint re-reads the recorded issuing credential's
/// live state and grant ceiling; the daemon must honor `allow:false`.
#[test]
fn withdrawn_credential_or_grant_refuses_at_commit() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.provision("sub-erin");
    assert!(
        fx.op_err(
            "member_job_request",
            json!({"member_envelope": fx.member_envelope("sub-erin"), "title": "t", "request_key": "e-k1", "body": "b"}),
        )
        .is_ok(),
        "live member job_request refused before the credential withdrawal"
    );

    // The recorded issuing credential is revoked at the platform — the
    // member is still listed, the envelope still verifies, but the live
    // pull must answer deny.
    fx.set_member("sub-erin", |m| m.credential_live = false);
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-erin")}),
        ),
        "credential-revoked member state",
    );
    assert!(
        e.contains("revalidation")
            || e.contains("authority")
            || e.contains("refused")
            || e.contains("member"),
        "credential-revoked member refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);

    // Restore the credential but narrow its grant — `assist.jobs`
    // withdrawn post-mint. The read verb still passes (positive control);
    // the jobs verb must refuse.
    fx.set_member("sub-erin", |m| {
        m.credential_live = true;
        m.credential_caps = vec!["assist.read".into(), "assist.write".into()];
    });
    assert!(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-erin")}),
        )
        .is_ok(),
        "narrowed member's read still refused — the withdrawn grant was jobs"
    );
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "member_job_request",
            json!({"member_envelope": fx.member_envelope("sub-erin"), "title": "t", "request_key": "e-k2", "body": "b"}),
        ),
        "grant-narrowed member job_request",
    );
    assert!(
        e.contains("revalidation")
            || e.contains("authority")
            || e.contains("refused")
            || e.contains("member"),
        "grant-narrowed member job_request refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);
}

/// The dedicated `assist.revalidate` service credential the daemon holds in
/// `platform_credentials` custody authenticates the TRANSPORT of the live
/// pull — it never substitutes for the member's own authority. With the
/// service enrolled:
///   * a live member's effect still commits (authorized service + target);
///   * a member whose recorded credential was revoked, grant narrowed, or
///     membership removed is refused — the service grant does not rescue
///     withdrawn member provenance;
///   * a member envelope minted under a credential the binding does NOT
///     record as provenance is refused even under the service transport —
///     a queued effect keeps its original authorization provenance and
///     cannot silently adopt a different credential's mint.
#[test]
fn service_credential_transport_keeps_member_provenance() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.provision("sub-fran");
    fx.enroll_service(ServiceAuthority {
        token: format!("hcs_{}", "a".repeat(43)),
        org: ORG.to_string(),
        audience: format!("http://{}", fx.host),
        caps: vec!["assist.revalidate".into()],
        live: true,
    });

    // Positive control: authorized service transport + live member.
    assert!(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-fran")}),
        )
        .is_ok(),
        "service-auth live member refused assist_state"
    );

    // Member's recorded issuing credential revoked — service transport is
    // still fine but the member's provenance is dead: refuse.
    fx.set_member("sub-fran", |m| m.credential_live = false);
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-fran")}),
        ),
        "service-auth credential-revoked member",
    );
    assert!(
        e.contains("revalidation")
            || e.contains("authority")
            || e.contains("refused")
            || e.contains("member"),
        "service-auth credential-revoked member refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);

    // Member grant narrowed under the same service transport: read still
    // passes, jobs refuses.
    fx.set_member("sub-fran", |m| {
        m.credential_live = true;
        m.credential_caps = vec!["assist.read".into()];
    });
    assert!(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-fran")}),
        )
        .is_ok(),
        "service-auth narrowed member's read refused — the withdrawn grant was jobs"
    );
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "member_job_request",
            json!({"member_envelope": fx.member_envelope("sub-fran"), "title": "t", "request_key": "f-k1", "body": "b"}),
        ),
        "service-auth narrowed member job_request",
    );
    assert!(
        e.contains("revalidation")
            || e.contains("authority")
            || e.contains("refused")
            || e.contains("member"),
        "service-auth narrowed member job_request refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);

    // Member removed at the platform under the service transport.
    fx.remove_member("sub-fran");
    let e = err_text(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-fran")}),
        ),
        "service-auth removed member",
    );
    assert!(
        e.contains("revalidation")
            || e.contains("authority")
            || e.contains("refused")
            || e.contains("member"),
        "service-auth removed member refused for another reason: {e}"
    );

    // Provenance durability: a fresh member whose envelope names a
    // credential that is NOT the binding's recorded issuing credential —
    // e.g. a job enqueued under the ORIGINAL credential and a later mint
    // under a replacement — must be refused. The daemon still sends the
    // member envelope (the platform's member path checks provenance) only
    // when no service record exists, so exercise it on the member path.
    let fx2 = Fx::new();
    fx2.provision("sub-iris");
    // The platform's recorded provenance is cred-sub-iris; an envelope
    // minted under a DIFFERENT (replacement) credential claims
    // cred-sub-iris-replacement — provenance mismatch must deny.
    let replacement = fx2.envelope(
        "sub-iris",
        json!({}),
        json!({"credential_id": "cred-sub-iris-replacement"}),
    );
    let before = fx2.tree();
    let e = err_text(
        fx2.op_err(
            "member_job_request",
            json!({"member_envelope": replacement, "title": "t", "request_key": "i-k1", "body": "b"}),
        ),
        "replacement-credential member job_request",
    );
    assert!(
        e.contains("revalidation")
            || e.contains("authority")
            || e.contains("refused")
            || e.contains("member"),
        "replacement-credential member refused for another reason: {e}"
    );
    assert_eq!(fx2.tree(), before);
}

/// A service credential that is revoked, names another org/audience, or
/// lacks `assist.revalidate` must never authenticate the revalidation
/// transport — the member effect fails closed. An unreachable or non-200 /
/// malformed platform answer fails closed too, and a `hosted-board` record
/// whose custody bytes cannot be loaded is a refusal (never a silent
/// degrade to the member path).
#[test]
fn wrong_or_dead_service_credential_fails_closed() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.provision("sub-gwen");
    let good_token = format!("hcs_{}", "b".repeat(43));
    fx.enroll_service(ServiceAuthority {
        token: good_token.clone(),
        org: ORG.to_string(),
        audience: format!("http://{}", fx.host),
        caps: vec!["assist.revalidate".into()],
        live: true,
    });
    assert!(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-gwen")}),
        )
        .is_ok(),
        "good service transport refused a live member"
    );

    let revoke_service = |live: bool| {
        let mut guard = fx.authority.lock().unwrap();
        for s in guard.services.iter_mut() {
            if s.token == good_token {
                s.live = live;
            }
        }
    };

    // Service credential revoked at the platform.
    revoke_service(false);
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-gwen")}),
        ),
        "revoked service credential",
    );
    assert!(
        e.contains("revalidation")
            || e.contains("authority")
            || e.contains("refused")
            || e.contains("HTTP"),
        "revoked service credential refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);
    revoke_service(true);

    // Service capability narrowed — `assist.revalidate` withdrawn.
    {
        let mut guard = fx.authority.lock().unwrap();
        for s in guard.services.iter_mut() {
            if s.token == good_token {
                s.caps = vec!["results.submit".into()];
            }
        }
    }
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-gwen")}),
        ),
        "capability-narrowed service credential",
    );
    assert!(
        e.contains("revalidation")
            || e.contains("authority")
            || e.contains("refused")
            || e.contains("HTTP"),
        "capability-narrowed service refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);
    // Restore the capability for the platform-failure legs.
    {
        let mut guard = fx.authority.lock().unwrap();
        for s in guard.services.iter_mut() {
            if s.token == good_token {
                s.caps = vec!["assist.revalidate".into()];
            }
        }
    }

    // Platform unreachable-equivalents: non-200 then malformed JSON. Both
    // fail closed.
    {
        fx.authority.lock().unwrap().status = 503;
    }
    let e = err_text(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-gwen")}),
        ),
        "platform non-200",
    );
    assert!(
        e.contains("revalidation") || e.contains("refused") || e.contains("HTTP"),
        "platform non-200 refused for another reason: {e}"
    );
    {
        let mut guard = fx.authority.lock().unwrap();
        guard.status = 200;
        guard.malformed_next = true;
    }
    let e = err_text(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-gwen")}),
        ),
        "platform malformed body",
    );
    assert!(
        e.contains("revalidation")
            || e.contains("refused")
            || e.contains("shape")
            || e.contains("contract"),
        "platform malformed body refused for another reason: {e}"
    );
    fx.authority.lock().unwrap().malformed_next = false;
}

/// A configured-but-unloadable `hosted-board` custody record must refuse —
/// never silently degrade to the member-envelope path. The fixture lands
/// the `platform_credentials` row with NO custody bytes behind it; the
/// daemon's load fails and the member effect is refused closed.
#[test]
fn configured_but_unloadable_custody_fails_closed() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.provision("sub-ivy");
    // The record exists but no custody file backs it — the fail-closed
    // arm: a half-configured service binding never falls back to member
    // auth (that would let a custody corruption widen the pull path).
    let host = fx.host.clone();
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT OR REPLACE INTO platform_credentials
                 (platform,account,scopes,fingerprint,custody,exchange,enrolled_at,by,connection_id,credential_revision)
                 VALUES('hosted-board',?1,'[\"assist.revalidate\"]','fingerprint','file','token',1.0,'operator','conn-hosted-board',1)",
                rusqlite::params![host],
            )?;
            Ok(())
        })
        .unwrap();
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-ivy")}),
        ),
        "unloadable custody member state",
    );
    assert!(
        e.contains("custody")
            || e.contains("credential")
            || e.contains("revalidation")
            || e.contains("refused"),
        "unloadable custody refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);
}

/// An envelope that has expired can never be renewed into authority by a
/// live service credential — the daemon re-verifies the signed proof
/// itself before the pull, so a stale proof fails at the member-envelope
/// gate regardless of which transport the revalidation would ride.
#[test]
fn expired_proof_is_not_renewed_by_service_transport() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.provision("sub-hank");
    fx.enroll_service(ServiceAuthority {
        token: format!("hcs_{}", "c".repeat(43)),
        org: ORG.to_string(),
        audience: format!("http://{}", fx.host),
        caps: vec!["assist.revalidate".into()],
        live: true,
    });
    let now = crate::issue::time::now_epoch();
    let stale = fx.envelope(
        "sub-hank",
        json!({}),
        json!({"iat": now - 400, "exp": now - 100}),
    );
    let before = fx.tree();
    let e = err_text(
        fx.op_err("member_assist_state", json!({"member_envelope": stale})),
        "expired member envelope under service transport",
    );
    assert!(
        e.contains("envelope") || e.contains("expired") || e.contains("member"),
        "expired envelope refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);
}

/// The mint admits both member-facing roles (`owner` and `member` carry
/// `role_at_issue`); the platform's authority check must admit the same
/// set — an authorized owner is a member-facing principal too. The daemon
/// side is exercised here: an owner-role member with a live platform row
/// resolves its own assistant surface.
#[test]
fn owner_role_member_effect_resolves() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.shared
        .store
        .assistant_bind(
            ORG,
            "sub-owner",
            "assist_sub-owner",
            "assist-agent-sub-owner",
            Some("rev0"),
        )
        .unwrap();
    fx.set_member("sub-owner", |m| m.role = Some("owner"));
    let env = fx.envelope("sub-owner", json!({}), json!({"role_at_issue":"owner"}));
    let ok = fx.op_err("member_assist_state", json!({"member_envelope": env}));
    assert!(
        ok.is_ok(),
        "an authorized owner-role member was refused its own assistant: {ok:?}"
    );
}

/// Idempotent canonical job (acceptance 2 / R6): same `(org, sub,
/// request_key)` resolves to ONE canonical job; a different principal's
/// same key and the same principal's different key are distinct jobs.
#[test]
fn duplicate_request_key_yields_one_canonical_job() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.provision("sub-alice");
    fx.provision("sub-bob");

    let first = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-alice"), "title": "ship it", "request_key": "k-dup", "body": "b"}),
    );
    let job_id = first["job"].as_str().unwrap().to_string();
    assert!(
        !job_id.is_empty(),
        "job_request returned no job id: {first}"
    );

    let replay = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-alice"), "title": "ship it", "request_key": "k-dup", "body": "retry"}),
    );
    assert_eq!(
        replay["job"].as_str().unwrap(),
        job_id,
        "a replayed request_key minted a second job: {first} vs {replay}"
    );

    let other = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-bob"), "title": "ship it", "request_key": "k-dup", "body": "b"}),
    );
    let other_id = other["job"].as_str().unwrap();
    assert_ne!(
        other_id, job_id,
        "a different principal collapsed onto alice's canonical job"
    );

    let third = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-alice"), "title": "ship it", "request_key": "k-other", "body": "b"}),
    );
    assert_ne!(
        third["job"].as_str().unwrap(),
        job_id,
        "same free text deduped across a different request_key"
    );

    let alice_jobs = fx.op(
        "member_job_list",
        json!({"member_envelope": fx.member_envelope("sub-alice")}),
    );
    let listed: Vec<String> = alice_jobs["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j.as_str().unwrap().to_string())
        .collect();
    assert!(listed.contains(&job_id), "alice's job missing: {listed:?}");
    assert!(
        !listed.iter().any(|j| j == other_id),
        "bob's job leaked into alice's job_list: {listed:?}"
    );
}

/// A member who neither owns nor subscribes to a job cannot read its
/// evidence or event feed — and a member's own job IS readable.
#[test]
fn job_evidence_acl_owner_and_subscriber_only() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.provision("sub-alice");
    fx.provision("sub-bob");
    let job = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-alice"), "title": "a job", "request_key": "a-k1", "body": "x"}),
    );
    let job_id = job["job"].as_str().unwrap().to_string();

    let ev = fx.op(
        "member_job_evidence",
        json!({"member_envelope": fx.member_envelope("sub-alice"), "job": job_id}),
    );
    assert!(ev["job"].is_string(), "owner evidence refused: {ev}");
    assert_eq!(
        ev["evidence_state"], "reported",
        "fresh job over-claims: {ev}"
    );

    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "member_job_evidence",
            json!({"member_envelope": fx.member_envelope("sub-bob"), "job": job_id}),
        ),
        "bob reads alice's job evidence",
    );
    assert!(
        e.contains("owns") || e.contains("subscribe") || e.contains("refused"),
        "bob's foreign evidence read refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);
}

/// Direct daemon-side read-scope: an envelope carrying only `assist.read`
/// must refuse the write/jobs member verbs at the daemon boundary itself —
/// the daemon is no weaker than the `/api/assist/*` route's scope gate.
#[test]
fn member_envelope_scope_gate_refuses_write_and_jobs_at_daemon() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.provision("sub-alice");
    let read_only = fx.envelope("sub-alice", json!({}), json!({"scope":["assist.read"]}));

    // Positive control: assist.read covers the read verb — the scope gate
    // passes it and the live pull confirms it.
    assert!(
        fx.op_err("member_assist_state", json!({"member_envelope": read_only}))
            .is_ok(),
        "read-scope member refused assist_state"
    );

    let before = fx.tree();
    for (method, params) in [
        ("member_assist_send", json!({"text": "x"})),
        (
            "member_job_request",
            json!({"title": "t", "request_key": "k", "body": "b"}),
        ),
        ("member_job_list", json!({})),
        ("member_job_evidence", json!({"job": "j1"})),
        ("member_job_events", json!({"job": "j1"})),
    ] {
        let mut p = params.clone();
        p["member_envelope"] = json!(read_only);
        let e = err_text(fx.op_err(method, p), &format!("read-scope member {method}"));
        assert!(
            e.contains("scope") || e.contains("refused") || e.contains("assist."),
            "read-scope member {method} refused for another reason: {e}"
        );
    }
    assert_eq!(fx.tree(), before, "a read-scope member moved the tracker");
}

/// The other direction too: an `assist.jobs`-only envelope must not
/// satisfy an `assist.write` effect — the per-verb ceiling binds each
/// verb to its own scope, never to "any assist scope".
#[test]
fn member_envelope_jobs_scope_does_not_cover_write() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.provision("sub-alice");
    let jobs_only = fx.envelope("sub-alice", json!({}), json!({"scope":["assist.jobs"]}));
    // Positive control: jobs scope covers a jobs verb.
    assert!(
        fx.op_err("member_job_list", json!({"member_envelope": jobs_only}))
            .is_ok(),
        "jobs-scope member refused job_list"
    );
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "member_assist_send",
            json!({"member_envelope": jobs_only, "text": "x"}),
        ),
        "jobs-scope member assist_send",
    );
    assert!(
        e.contains("scope") || e.contains("refused") || e.contains("assist."),
        "jobs-scope member send refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);
}

// ============================================================================
// Member-boundary runtime gate (`member_assist_policy`) — the agent-class
// half of the member surface. An active bound assistant's registered agent
// is scoped to its own principal; the gate runs under `Shared::dispatch`
// BEFORE the method's own caller rule, so every case here is the real
// dispatch path with the daemon-derived `Asserted::Agent` caller — no
// cleared identity, no operator bypass, the native caller proof.
// ============================================================================

/// The bound assistant's registered agent alias (`assist-agent-<sub>`), its
/// private thread and one ordinary worker + foreign message seeded for the
/// cross-resource probes.
struct MemberRuntime {
    alias: String,
    thread_id: String,
    worker: String,
    foreign_msg: String,
    foreign_job: String,
    foreign_task: String,
}

impl Fx {
    /// Provision `sub` + lazy-register its assistant runtime, and seed one
    /// ordinary worker with its own thread, message, job and task — the
    /// foreign surface the member boundary must refuse.
    fn member_runtime(&self, sub: &str) -> MemberRuntime {
        self.provision(sub);
        // First member call lazily registers the agent + binds the thread.
        let env = self.member_envelope(sub);
        let state = self.op("member_assist_state", json!({"member_envelope": env}));
        let conv = state["conversation_id"]
            .as_str()
            .expect("member_assist_state bound no conversation")
            .to_string();
        let alias = state["assistant"]["alias"]
            .as_str()
            .expect("member_assist_state bound no alias")
            .to_string();
        // The ordinary worker — a NON-assist registered agent the
        // cross-resource checks name. Idempotent across members in one
        // fixture so `member_runtime` can be called per member.
        let worker_dir = self.dir.path().join("worker-cwd");
        std::fs::create_dir_all(&worker_dir).unwrap();
        if self
            .shared
            .store
            .agent_opt("ordinary-worker")
            .unwrap()
            .is_none()
        {
            self.shared
                .store
                .register_agent(&crate::store::NewAgent {
                    alias: "ordinary-worker",
                    provider: "pi",
                    endpoint_kind: "managed",
                    role: "worker",
                    cwd: worker_dir.to_str().unwrap(),
                    sandbox: "workspace-write",
                    instructions: None,
                    params: None,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        let wthread = self.shared.store.ensure_thread("ordinary-worker").unwrap();
        let foreign_msg = {
            let msg_id = format!("foreign-msg-{sub}");
            let (dup, _state) = self
                .shared
                .store
                .enqueue(
                    "ordinary-worker",
                    "foreign body: not the member's",
                    None,
                    &msg_id,
                    "test",
                )
                .unwrap();
            assert!(!dup, "foreign message id collided");
            assert!(
                self.shared
                    .store
                    .message(&msg_id)
                    .unwrap()
                    .is_some_and(|m| m.alias == "ordinary-worker"),
                "foreign message {msg_id} was not stored for ordinary-worker"
            );
            let _ = wthread;
            msg_id
        };
        // A foreign job + task the member neither owns nor subscribes.
        let spec = self.dir.path().join(format!("foreign-spec-{sub}.md"));
        std::fs::write(&spec, "# foreign\n").unwrap();
        let foreign_sha = sha256_file(&spec).unwrap();
        let (_d, job) = self
            .shared
            .store
            .create_job(
                &format!("job-foreign-{sub}"),
                Some("foreign"),
                spec.to_str().unwrap(),
                &foreign_sha,
                "ordinary-worker",
                None,
                None,
                None,
                2,
                None,
                Some("foreign task"),
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let foreign_task = format!("{}-t1", job.id);
        MemberRuntime {
            alias,
            thread_id: conv,
            worker: "ordinary-worker".to_string(),
            foreign_msg,
            foreign_job: job.id,
            foreign_task,
        }
    }

    /// A member-verb call under the assistant's OWN daemon-derived agent
    /// identity — the seam's `Asserted::Agent` IS the registered caller
    /// proof (`caller_identity` resolves it to the live agent row; no
    /// envelope, no operator, no cleared identity).
    fn as_assistant(&self, alias: &str, method: &str, params: Value) -> Result<Value> {
        self.call(Asserted::Agent(alias.to_string()), method, params)
    }
}

/// The member boundary refuses everything outside the assist surface — and
/// the cross-resource checks inside it never leak another principal's state.
/// Every refused case is paired with an own-surface positive so a blanket
/// `refuse everything` cannot pass, and the tracker snapshot proves a
/// refused read/steer/dispatch left no effect.
#[test]
fn assist_agent_boundary_refuses_foreign_fleet_and_leaks() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let rt = fx.member_runtime("sub-alice");
    let own = rt.alias.clone();

    // ---- positive controls: the member's OWN surface resolves ----
    assert!(
        fx.as_assistant(&own, "agent_show", json!({"alias": own}))
            .is_ok(),
        "assist alias could not read its own agent row"
    );
    assert!(
        fx.as_assistant(&own, "thread_read", json!({"alias": own}))
            .is_ok(),
        "assist alias could not read its own conversation"
    );

    let before = fx.tree();

    // ---- foreign-agent narrowing on the allowed verbs ----
    for (method, params) in [
        ("agent_send", json!({"to": rt.worker, "text": "steer"})),
        ("agent_ask", json!({"to": rt.worker, "text": "steer"})),
        ("agent_show", json!({"alias": rt.worker})),
        ("agent_events", json!({"alias": rt.worker})),
    ] {
        let e = err_text(
            fx.as_assistant(&own, method, params),
            &format!("assist {method} foreign agent"),
        );
        assert!(
            e.contains("assist agent") || e.contains("principal") || e.contains("refused"),
            "assist {method} to a foreign agent refused for another reason: {e}"
        );
    }

    // ---- foreign message/job/task metadata leaks ----
    for (method, params) in [
        ("message_read", json!({"message": rt.foreign_msg})),
        (
            "message_report",
            json!({"message": rt.foreign_msg, "result": "x"}),
        ),
        ("job_show", json!({"job": rt.foreign_job})),
        ("job_events", json!({"job": rt.foreign_job})),
        ("task_show", json!({"task": rt.foreign_task})),
    ] {
        let e = err_text(
            fx.as_assistant(&own, method, params),
            &format!("assist {method} foreign resource"),
        );
        assert!(
            e.contains("assist agent")
                || e.contains("principal")
                || e.contains("not bound")
                || e.contains("refused")
                || e.contains("not its own")
                || e.contains("not a job this member"),
            "assist {method} on a foreign resource refused for another reason: {e}"
        );
    }

    // ---- off-surface verbs: fleet/issue/wiki/dispatch/operator effects ----
    for (method, params) in [
        (
            "job_new",
            json!({"pm": own, "spec": "s", "spec_sha256": "x"}),
        ),
        (
            "task_new",
            json!({"job": rt.foreign_job, "task": "t", "title": "x"}),
        ),
        (
            "task_dispatch",
            json!({"task": rt.foreign_task, "to": rt.worker, "by": "op"}),
        ),
        (
            "agent_register",
            json!({"alias": "a2", "provider": "pi", "endpoint_kind": "managed", "cwd": "/tmp"}),
        ),
        ("plan_propose", json!({"spec": "x"})),
        ("issue_list", json!({})),
        ("agent_stop", json!({"alias": rt.worker})),
        ("agent_set", json!({"alias": rt.worker, "model": "x"})),
    ] {
        let e = err_text(
            fx.as_assistant(&own, method, params),
            &format!("assist off-surface {method}"),
        );
        assert!(
            e.contains("assist agent")
                || e.contains("may not call")
                || e.contains("refused")
                || e.contains("operator"),
            "assist off-surface {method} refused for another reason: {e}"
        );
    }

    // ---- a member assistant never reaches the member_* verbs itself ----
    let e = err_text(
        fx.as_assistant(
            &own,
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-alice")}),
        ),
        "assist alias calling member_assist_state",
    );
    assert!(
        e.contains("assist agent") || e.contains("operator") || e.contains("refused"),
        "assist alias reaching a member_* verb refused for another reason: {e}"
    );

    assert_eq!(
        fx.tree(),
        before,
        "a refused foreign read/steer/dispatch changed the tracker"
    );

    // ---- still-positive: own-surface reads still resolve after the refusals ----
    assert!(
        fx.as_assistant(&own, "agent_show", json!({"alias": own}))
            .is_ok(),
        "assist alias's own surface broke after refusals"
    );
}

/// A revoked/archived binding keeps the alias inside the member boundary —
/// it is still a member assistant (the dead one), NOT an ordinary worker.
/// The binding-state check must never silently drop the alias into the
/// generic worker verb surface; a binding the daemon cannot read fails
/// closed rather than ungating the worker verbs.
#[test]
fn assist_binding_dead_state_never_widens_to_ordinary_worker() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let rt = fx.member_runtime("sub-bob");
    let own = rt.alias.clone();
    // Positive control: while the binding is active the own-surface read works.
    assert!(
        fx.as_assistant(&own, "agent_show", json!({"alias": own}))
            .is_ok(),
        "active assist alias could not read its own row"
    );

    // Revoke the binding — the member's authority is dead.
    fx.revoke("sub-bob");
    let before = fx.tree();
    // The dead binding must NOT widen the alias to ordinary worker verbs:
    // every member-boundary check still refuses it — both the off-surface
    // verbs AND the cross-resource narrows on the allowed ones.
    for (method, params) in [
        (
            "job_new",
            json!({"pm": own, "spec": "s", "spec_sha256": "x"}),
        ),
        (
            "task_dispatch",
            json!({"task": rt.foreign_task, "to": rt.worker, "by": "op"}),
        ),
        ("agent_send", json!({"to": rt.worker, "text": "x"})),
        ("job_show", json!({"job": rt.foreign_job})),
    ] {
        let e = err_text(
            fx.as_assistant(&own, method, params),
            &format!("revoked-binding assist {method}"),
        );
        assert!(
            e.contains("assist agent")
                || e.contains("refused")
                || e.contains("binding")
                || e.contains("operator"),
            "revoked-binding assist {method} refused for another reason: {e}"
        );
    }
    assert_eq!(
        fx.tree(),
        before,
        "a dead binding let an assist alias act as an ordinary worker"
    );

    // An ORDINARY registered worker (no assistants row at all) is NOT
    // member-bound: `member_assist_policy` must not touch it — its own
    // caller rule decides. Positive control proving the boundary is
    // scoped, not a blanket agent refusal: the worker can reach a worker
    // verb the boundary would refuse an assistant for.
    let ordinary = rt.worker.clone();
    let r = fx.call(Asserted::Agent(ordinary), "agent_list", json!({}));
    assert!(
        r.is_ok(),
        "an ordinary worker was refused agent_list by the member boundary: {r:?}"
    );
}

/// The member-assistant runtime record is the launch policy's binding: the
/// registered agent must carry `role=worker`, `sandbox=read-only`,
/// `params.confine=true` and `params.assist=<sub>` — the selector
/// `pi_assistant_confinement` keys the member-boundary policy on — and its
/// `cwd` must be the dedicated principal subtree, never the daemon state
/// root. A request field can never inject these; the daemon writes them.
#[test]
fn assist_runtime_profile_is_recorded_member_scoped() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-carol");
    let agent = fx
        .shared
        .store
        .agent(&rt.alias)
        .unwrap_or_else(|e| panic!("registered assistant agent missing: {e}"));
    assert_eq!(agent.provider, "pi");
    assert_eq!(agent.endpoint_kind, "managed");
    assert_eq!(agent.role, "worker", "assistant must not hold pm authority");
    assert_eq!(agent.sandbox, "read-only");
    let params = agent
        .params
        .clone()
        .expect("assistant agent carries no params");
    assert_eq!(params["confine"], json!(true));
    assert_eq!(
        params["assist"],
        json!("sub-carol"),
        "the member-boundary launch selector is not the member's sub"
    );
    // The principal-owned subtree, never the state root.
    let cwd = std::path::PathBuf::from(&agent.cwd);
    assert!(
        cwd.starts_with(fx.dir.path().join("assistants")),
        "assistant cwd is not the dedicated principal subtree: {}",
        agent.cwd
    );
    assert_ne!(
        cwd.canonicalize().ok(),
        fx.dir.path().canonicalize().ok(),
        "assistant cwd is the daemon state root"
    );
    // The member's private conversation is bound and owned by this alias.
    let binding = fx
        .shared
        .store
        .assistant_binding(ORG, "sub-carol")
        .unwrap()
        .expect("no binding");
    assert_eq!(
        binding.conversation_id.as_deref(),
        Some(rt.thread_id.as_str())
    );
    let thread = fx
        .shared
        .store
        .thread_by_id(&rt.thread_id)
        .unwrap()
        .expect("bound conversation missing");
    assert_eq!(
        thread.alias, rt.alias,
        "the private thread is not the assistant's"
    );
}

// ============================================================================
// Deferred-commit revalidation on `task_dispatch` (kickoff §4 / REQ-008):
// an approved assist-bound job's dispatch must re-prove the member's LIVE
// platform authority at consume time — the ≤300 s envelope is not
// revocation proof, and a queued effect for a removed/credential-dead
// member must fail closed with no lane effect.
// ============================================================================

/// Seed the `hosted-board` `assist.revalidate` service credential into the
/// daemon's `platform_credentials` custody + the fixture platform — the
/// deferred-commit bearer path (the member envelope cannot survive to
/// consume time).
/// CAD-1380 acceptance 1 (target-side isolation). A registered ORDINARY
/// worker (a non-assist agent, asserted through the real caller seam) must
/// not read a member's private assistant thread, agent row, agent events,
/// job or task records. The owner's own thread read stays positive and an
/// ordinary nonmember control on its own thread stays allowed.
#[test]
fn ordinary_worker_cannot_read_member_private_thread_job_or_task() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let rt = fx.member_runtime("sub-alice");
    let own = rt.alias.clone();
    let worker = Asserted::Agent("ordinary-worker".to_string());

    // A REAL member-owned job: the canonical request binds it to the member's
    // assistant as PM and records the assist-request owner row.
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-alice"), "title": "private boundary", "request_key": "ordinary-private-boundary", "body": "private request"}),
    );
    let private_job = req["job"].as_str().unwrap().to_string();
    let private_task = fx
        .shared
        .store
        .tasks_for_job(&private_job)
        .unwrap()
        .first()
        .expect("member job created no task")
        .id
        .clone();
    assert_eq!(
        req["job_record"]["pm"].as_str(),
        Some(own.as_str()),
        "member job PM is not the member's assistant"
    );
    assert!(
        fx.shared
            .store
            .assist_request_owns(ORG, "sub-alice", &private_job)
            .unwrap(),
        "member job has no member-owned assist request row"
    );
    assert_ne!(
        req["job_record"]["pm"].as_str(),
        Some("ordinary-worker"),
        "ordinary worker is the member job's PM"
    );
    assert!(
        !fx.shared
            .store
            .job_is_subscriber(&private_job, ORG, "ordinary-worker")
            .unwrap(),
        "ordinary worker is a subscriber of the member job"
    );

    // Positive control: the member's own assistant reads its own thread.
    assert!(
        fx.as_assistant(&own, "thread_read", json!({"alias": own}))
            .is_ok(),
        "owner could not read its own private thread"
    );
    // Control: the ordinary worker reads its OWN nonmember thread.
    assert!(
        fx.call(
            worker.clone(),
            "thread_read",
            json!({"alias": "ordinary-worker"})
        )
        .is_ok(),
        "ordinary worker could not read its own thread"
    );
    // Control: the ordinary worker is the PM of the ORIGINAL foreign job, so
    // its job/task reads stay allowed (normal nonmember behaviour).
    for (method, params) in [
        ("job_show", json!({"job": rt.foreign_job})),
        ("task_show", json!({"task": rt.foreign_task})),
    ] {
        assert!(
            fx.call(worker.clone(), method, params).is_ok(),
            "ordinary worker PM could not {method} its own foreign job"
        );
    }

    let before = fx.tree();
    let probes: [(&str, Value); 5] = [
        ("thread_read", json!({"alias": own})),
        ("agent_show", json!({"alias": own})),
        ("agent_events", json!({"alias": own})),
        ("job_show", json!({"job": private_job})),
        ("task_show", json!({"task": private_task})),
    ];
    for (method, params) in probes {
        let r = fx.call(worker.clone(), method, params);
        assert!(
            r.is_err(),
            "ordinary worker read member-private {method} of '{own}' (job {private_job}): {r:?}"
        );
    }
    assert_eq!(
        fx.tree(),
        before,
        "an ordinary worker read moved the tracker"
    );
}

/// CAD-1380 privacy (list filter). A member-private assist job must stay out of
/// an ordinary worker's generic `job_list`. The owner member's share and the
/// subscribing member's live grant keep the job visible to THAT member.
#[test]
fn ordinary_job_list_hides_member_private_job_until_shared() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    fx.provision("sub-ada");
    fx.provision("sub-ben");
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-ada"), "title": "private list", "request_key": "list-private-1", "body": "private"}),
    );
    let private_job = req["job"].as_str().unwrap().to_string();
    let worker = Asserted::Agent("ordinary-worker".to_string());
    let generic_lists_job = |who: &Asserted| -> bool {
        let v = fx
            .call(who.clone(), "job_list", json!({"all": true}))
            .unwrap_or_else(|e| panic!("job_list refused: {e}"));
        v["jobs"].as_array().is_some_and(|a| {
            a.iter()
                .any(|j| j["id"].as_str() == Some(private_job.as_str()))
        })
    };
    // Unshared: the ordinary worker's generic list must not contain it.
    assert!(
        !generic_lists_job(&worker),
        "ordinary worker's job_list exposed a member-private job before any share"
    );
    // The owner's share plus the subscriber's live grant make it visible to B.
    fx.op(
        "member_job_share",
        json!({"member_envelope": fx.member_envelope("sub-ada"), "job": private_job}),
    );
    fx.op(
        "member_job_subscribe",
        json!({"member_envelope": fx.member_envelope("sub-ben"), "job": private_job}),
    );
    let ben_jobs = fx.op(
        "member_job_list",
        json!({"member_envelope": fx.member_envelope("sub-ben")}),
    );
    assert!(
        ben_jobs["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j.as_str() == Some(private_job.as_str())),
        "subscribed member lost its shared job from member_job_list"
    );
    // Positive: the subscribed assistant's own generic job read stays allowed.
    assert!(
        fx.as_assistant(
            "assist-agent-sub-ben",
            "job_show",
            json!({"job": private_job})
        )
        .is_ok(),
        "subscribed assistant's generic job_show was refused under a live grant"
    );
    // Withdrawal: the owner withdraws the share; a fresh subscribe is refused.
    fx.op(
        "member_job_share",
        json!({"member_envelope": fx.member_envelope("sub-ada"), "job": private_job, "enabled": false}),
    );
    fx.provision("sub-cid");
    assert!(
        fx.op_err(
            "member_job_subscribe",
            json!({"member_envelope": fx.member_envelope("sub-cid"), "job": private_job}),
        )
        .is_err(),
        "a subscribe after share withdrawal was accepted"
    );
}

// ============================================================================
// Job-share withdrawal on a member-owned job (independent acceptance).
//
// The owner shares with the real `member_job_share` RPC and a second member
// subscribes with the real `member_job_subscribe` RPC. Before withdrawal the
// subscriber reads job evidence/events through its SIGNED member envelope and
// the job/task through its bound assistant's generic verbs. The owner then
// disables the share; afterwards the subscriber's ASSISTANT reads are
// refused (the subscriber path requires a live share), no private payload is
// returned, the owner keeps both surfaces, and an unrelated ordinary job is
// untouched.
//
// Required contract (reviewer-confirmed): withdrawal revokes a NEW/existing
// subscriber's member-envelope reads too. `member_job_evidence`,
// `member_job_events` and `member_job_list` must refuse/omit the job after
// `member_job_share(enabled:false)`, even though the subscription row was not
// retro-removed. The current source gates those on `job_is_subscriber` alone,
// so the final block below is the RED acceptance for that fix — it is
// intentionally expected to fail until the handlers consult the shared flag.
// The assistant-process (generic) reads ARE share-guarded today and are
// asserted as regressions above.
// ============================================================================

#[test]
fn job_share_withdrawal_revokes_the_subscriber_assistant_reads() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let ben = fx.member_runtime("sub-ben");

    let req = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope("sub-alice"),
            "title": "a shared job",
            "request_key": "cad2207-share-withdrawal-1",
            "body": "private share body",
        }),
    );
    let job = req["job"]
        .as_str()
        .expect("member_job_request returned no job")
        .to_string();
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .expect("the job's tasks could not be read")
        .first()
        .expect("the member job created no task")
        .id
        .clone();

    fx.op(
        "member_job_share",
        json!({"member_envelope": fx.member_envelope("sub-alice"), "job": job}),
    );
    fx.op(
        "member_job_subscribe",
        json!({"member_envelope": fx.member_envelope("sub-ben"), "job": job}),
    );

    // ---- authorised before withdrawal: BOTH surfaces --------------------
    let ben_evidence = fx.op(
        "member_job_evidence",
        json!({"member_envelope": fx.member_envelope("sub-ben"), "job": job}),
    );
    assert_eq!(
        ben_evidence["job"].as_str(),
        Some(job.as_str()),
        "the live subscriber's member evidence did not name the job: {ben_evidence}"
    );
    fx.op(
        "member_job_events",
        json!({"member_envelope": fx.member_envelope("sub-ben"), "job": job}),
    );
    let ben_show = fx
        .as_assistant(&ben.alias, "job_show", json!({"job": job}))
        .unwrap_or_else(|e| {
            panic!("the subscriber's assistant could not read the shared job: {e}")
        });
    assert_eq!(
        ben_show["job"]["id"].as_str(),
        Some(job.as_str()),
        "the subscriber's assistant read a different job record: {ben_show}"
    );
    for (method, params) in [
        ("job_events", json!({"job": job})),
        ("task_show", json!({"task": task})),
    ] {
        assert!(
            fx.as_assistant(&ben.alias, method, params).is_ok(),
            "the subscriber's assistant lost {method} while the job was shared"
        );
    }
    // Owner control before withdrawal.
    assert!(
        fx.as_assistant(&alice.alias, "job_show", json!({"job": job}))
            .is_ok(),
        "the owning assistant could not read its own job before withdrawal"
    );

    // ---- the owner withdraws through the real member RPC ----------------
    let withdrawn = fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope("sub-alice"),
            "job": job,
            "enabled": false,
        }),
    );
    assert_eq!(
        withdrawn["shared"].as_bool(),
        Some(false),
        "the share withdrawal did not report the disabled state: {withdrawn}"
    );

    // ---- the subscriber's ASSISTANT reads are refused -------------------
    let before = fx.tree();
    for (method, params) in [
        ("job_show", json!({"job": job})),
        ("job_events", json!({"job": job})),
        ("task_show", json!({"task": task})),
    ] {
        let refused = fx.as_assistant(&ben.alias, method, params.clone());
        assert!(
            refused.is_err(),
            "the subscriber's assistant kept {method} after share withdrawal: {refused:?}"
        );
    }
    assert_eq!(
        fx.tree(),
        before,
        "a refused post-withdrawal read changed the tracker"
    );

    // ---- the owner keeps both surfaces; an ordinary job is untouched ----
    assert!(
        fx.as_assistant(&alice.alias, "job_show", json!({"job": job}))
            .is_ok(),
        "the owning assistant lost its own job after withdrawal"
    );
    assert!(
        fx.as_assistant(&alice.alias, "task_show", json!({"task": task}))
            .is_ok(),
        "the owning assistant lost its own task after withdrawal"
    );
    let alice_evidence = fx.op(
        "member_job_evidence",
        json!({"member_envelope": fx.member_envelope("sub-alice"), "job": job}),
    );
    assert_eq!(
        alice_evidence["job"].as_str(),
        Some(job.as_str()),
        "the owner lost its own member evidence after withdrawal: {alice_evidence}"
    );
    assert!(
        fx.call(
            Asserted::Operator,
            "job_show",
            json!({"job": alice.foreign_job})
        )
        .is_ok(),
        "an unrelated ordinary job read broke after the share withdrawal"
    );
    assert!(
        fx.call(
            Asserted::Agent("ordinary-worker".to_string()),
            "job_show",
            json!({"job": alice.foreign_job})
        )
        .is_ok(),
        "the ordinary worker lost its own unrelated job after the withdrawal"
    );

    // ---- signed member route: TARGET contract (RED until the source fix) --
    // The reviewer-confirmed contract is that withdrawal revokes the
    // subscriber's member-envelope reads and list entry too. The current
    // handlers gate on `job_is_subscriber` alone, so these assertions
    // intentionally fail today.
    let before_member = fx.tree();
    assert!(
        fx.op_err(
            "member_job_evidence",
            json!({"member_envelope": fx.member_envelope("sub-ben"), "job": job}),
        )
        .is_err(),
        "the withdrawn subscriber still read member_job_evidence"
    );
    assert!(
        fx.op_err(
            "member_job_events",
            json!({"member_envelope": fx.member_envelope("sub-ben"), "job": job}),
        )
        .is_err(),
        "the withdrawn subscriber still read member_job_events"
    );
    let ben_list = fx.op(
        "member_job_list",
        json!({"member_envelope": fx.member_envelope("sub-ben")}),
    );
    assert!(
        !ben_list["jobs"].as_array().is_some_and(|jobs| jobs
            .iter()
            .any(|listed| listed.as_str() == Some(job.as_str()))),
        "the withdrawn subscriber's member_job_list still carries the job: {ben_list}"
    );
    assert_eq!(
        fx.tree(),
        before_member,
        "a refused withdrawn member read changed the tracker"
    );

    // Owner positives survive the withdrawal on both member verbs.
    let alice_list = fx.op(
        "member_job_list",
        json!({"member_envelope": fx.member_envelope("sub-alice")}),
    );
    assert!(
        alice_list["jobs"].as_array().is_some_and(|jobs| jobs
            .iter()
            .any(|listed| listed.as_str() == Some(job.as_str()))),
        "the owner lost the job from its own member_job_list: {alice_list}"
    );
    fx.op(
        "member_job_events",
        json!({"member_envelope": fx.member_envelope("sub-alice"), "job": job}),
    );

    // A FRESH subscribe after withdrawal is refused (no new subscribers).
    fx.provision("sub-cid");
    assert!(
        fx.op_err(
            "member_job_subscribe",
            json!({"member_envelope": fx.member_envelope("sub-cid"), "job": job}),
        )
        .is_err(),
        "a fresh subscribe was admitted after the share was withdrawn"
    );
}

/// Share withdrawal does not erase an INDEPENDENT assigned role. The
/// subscriber's assistant is also the real assignee of one task on the
/// owner's job (`task_new` is `Rule::OnJob` and writes the `tasks.assignee`
/// row the target guard reads). After the owner withdraws the share, that
/// task read stays authorized — pre-existing target policy — while the job
/// itself and the UNASSIGNED sibling task refuse. Subscription alone is
/// therefore not a grant.
#[test]
fn assigned_task_author_keeps_its_read_after_share_withdrawal() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let ben = fx.member_runtime("sub-ben");

    let req = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope("sub-alice"),
            "title": "an assigned-role job",
            "request_key": "cad2207-assigned-withdrawal-1",
            "body": "private body",
        }),
    );
    let job = req["job"]
        .as_str()
        .expect("member_job_request returned no job")
        .to_string();
    // The draft task the member request created: deliberately unassigned.
    let unassigned = fx
        .shared
        .store
        .tasks_for_job(&job)
        .expect("the job's tasks could not be read")
        .first()
        .expect("the member job created no task")
        .id
        .clone();
    // A REAL second task assigned to the subscribing member's assistant.
    // The assignment path requires the worker to be in the job PM's group;
    // record exactly what a join writes (`params.upstream = <job PM>`).
    // The privacy guard under test reads `tasks.assignee`, never `upstream`.
    fx.shared
        .store
        .set_params(&ben.alias, &json!({"upstream": alice.alias}))
        .expect("the fixture could not join the assistant to the job group");
    let assigned = fx
        .call(
            Asserted::Operator,
            "task_new",
            json!({
                "job": job,
                "title": "assigned to the subscriber",
                "assignee": ben.alias,
            }),
        )
        .unwrap_or_else(|e| panic!("the operator could not add an assigned task: {e}"));
    let assigned_task = assigned["task"]["id"]
        .as_str()
        .expect("task_new returned no task id")
        .to_string();
    assert_eq!(
        fx.shared
            .store
            .task(&assigned_task)
            .expect("the assigned task could not be read")
            .assignee
            .as_deref(),
        Some(ben.alias.as_str()),
        "the fixture's assigned role was not recorded on the task row"
    );

    fx.op(
        "member_job_share",
        json!({"member_envelope": fx.member_envelope("sub-alice"), "job": job}),
    );
    fx.op(
        "member_job_subscribe",
        json!({"member_envelope": fx.member_envelope("sub-ben"), "job": job}),
    );
    // Both tasks are readable while the share is live.
    for task in [&assigned_task, &unassigned] {
        assert!(
            fx.as_assistant(&ben.alias, "task_show", json!({"task": task}))
                .is_ok(),
            "the subscriber's assistant could not read task '{task}' while the job was shared"
        );
    }

    fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope("sub-alice"),
            "job": job,
            "enabled": false,
        }),
    );

    // The independent assigned role survives the withdrawal.
    assert!(
        fx.as_assistant(&ben.alias, "task_show", json!({"task": assigned_task}))
            .is_ok(),
        "the assigned task author lost its own task after the owner withdrew the share"
    );
    // Subscription alone is not a grant: the job and the unassigned sibling
    // refuse, and the refusals leave no effect.
    let before = fx.tree();
    assert!(
        fx.as_assistant(&ben.alias, "job_show", json!({"job": job}))
            .is_err(),
        "the withdrawn subscriber still read the job record"
    );
    assert!(
        fx.as_assistant(&ben.alias, "job_events", json!({"job": job}))
            .is_err(),
        "the withdrawn subscriber still read the job events"
    );
    assert!(
        fx.as_assistant(&ben.alias, "task_show", json!({"task": unassigned}))
            .is_err(),
        "the withdrawn subscriber still read an unassigned sibling task"
    );
    assert_eq!(fx.tree(), before, "a refused read changed the tracker");

    // Owner controls.
    assert!(
        fx.as_assistant(&alice.alias, "job_show", json!({"job": job}))
            .is_ok(),
        "the owner lost its job while the share was withdrawn"
    );
    assert!(
        fx.as_assistant(&alice.alias, "task_show", json!({"task": unassigned}))
            .is_ok(),
        "the owner lost its unassigned task while the share was withdrawn"
    );
}

// CAD-2207 independent negative acceptance — a member's personal assistant
// (its alias, profile, conversation and event stream) belongs to the VERIFIED
// (workspace, member), not to whoever holds a generic operator connection.
// Authored by the acceptance maintainer from the ticket's privacy clause; the
// guard implementer must not edit or weaken these checks.
//
// Everything below is the real daemon: `Shared::dispatch` under the real
// `test-seam` caller identities, the real stored `assistants` binding, the
// real bound thread and the real private bootstrap profile files. Nothing
// mirrors a guard.
//
// Bad cases (against today's `Who::Operator => Ok(())` bypass): a proven
// operator, an unproven external harness and a DIFFERENT member's bound
// assistant each name the owner's private alias and are refused on every
// generic read; the operator's `agent_list` no longer carries the private
// row; no path returns the private profile value.
//
// Controls in the same fixture so a blanket refusal cannot pass: the
// verified member's signed envelope still reads its own profile and
// conversation, the owner's own assistant still reads its own row/thread/
// mail, and the operator keeps every non-private operational read (another
// agent's row and thread, its non-member job and task, the unrelated list
// rows).
// ============================================================================

/// A distinctive value the fixture writes into the owner's private bootstrap
/// files. It must appear in the OWNER's read and in no non-owner payload.
const CAD2207_PRIVATE_NAME: &str = "Violet Quill (private)";
const CAD2207_PRIVATE_TONE: &str = "wry and unhurried";
const CAD2207_PRIVATE_BODY: &str =
    "private mailbox body: only the member's assistant may read this";

fn seed_cad2207_private_profile(fx: &Fx, sub: &str) {
    let root = fx.shared.assistant_principal_cwd(ORG, sub);
    super::bootstrap_files::write_profile(
        &root,
        &super::bootstrap_files::Profile {
            name: Some(CAD2207_PRIVATE_NAME.into()),
            tone: Some(CAD2207_PRIVATE_TONE.into()),
            preferences: Some("terse answers only".into()),
            user_name: Some("Mx Private".into()),
            look: None,
        },
    )
    .unwrap_or_else(|e| panic!("seed the private profile for {sub}: {e}"));
}

#[test]
fn operator_cannot_read_or_list_a_member_private_assistant() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let ben = fx.member_runtime("sub-ben");
    seed_cad2207_private_profile(&fx, "sub-alice");

    // ---- positive controls: the verified member's own signed route still
    // reads the private profile and the bound conversation ----
    let own_state = fx.op(
        "member_assist_state",
        json!({"member_envelope": fx.member_envelope("sub-alice")}),
    );
    assert_eq!(
        own_state["assistant"]["alias"].as_str(),
        Some(alice.alias.as_str()),
        "the owner's signed envelope did not resolve its own assistant"
    );
    assert_eq!(
        own_state["assistant"]["identity"]["name"].as_str(),
        Some(CAD2207_PRIVATE_NAME),
        "the owner could not read its own private profile name: {own_state}"
    );
    assert_eq!(
        own_state["assistant"]["identity"]["tone"].as_str(),
        Some(CAD2207_PRIVATE_TONE),
        "the owner could not read its own private profile tone"
    );
    assert_eq!(
        own_state["conversation_id"].as_str(),
        Some(alice.thread_id.as_str()),
        "the owner's profile read did not name its bound conversation"
    );
    let own_history = fx.op(
        "member_assist_history",
        json!({"member_envelope": fx.member_envelope("sub-alice")}),
    );
    assert_eq!(
        own_history["conversation"]["id"].as_str(),
        Some(alice.thread_id.as_str()),
        "the owner could not read its own private conversation"
    );
    // The owner's own assistant session still reads its own surface.
    for (method, params) in [
        ("agent_show", json!({"alias": alice.alias})),
        ("agent_events", json!({"alias": alice.alias})),
        ("thread_read", json!({"alias": alice.alias})),
        ("message_list", json!({"to": alice.alias})),
    ] {
        assert!(
            fx.as_assistant(&alice.alias, method, params).is_ok(),
            "the owner's own assistant could not {method} its own surface"
        );
    }

    // ---- positive controls: the operator keeps every NON-private
    // operational read (the privacy guard must not become a blanket
    // operator lockout, and job/task policy is untouched by this slice) ----
    assert!(
        fx.call(
            Asserted::Operator,
            "agent_show",
            json!({"alias": "ordinary-worker"})
        )
        .is_ok(),
        "operator lost a non-private agent_show"
    );
    assert!(
        fx.call(
            Asserted::Operator,
            "thread_read",
            json!({"alias": "ordinary-worker"})
        )
        .is_ok(),
        "operator lost a non-private thread_read"
    );
    assert!(
        fx.call(
            Asserted::Operator,
            "job_show",
            json!({"job": alice.foreign_job})
        )
        .is_ok(),
        "operator lost the independent non-member job read"
    );
    assert!(
        fx.call(
            Asserted::Operator,
            "task_show",
            json!({"task": alice.foreign_task})
        )
        .is_ok(),
        "operator lost the independent non-member task read"
    );
    // Alias-level liveness discloses state and reason only — no profile, no
    // conversation — so it stays an operator control path (CAD-2207). This is
    // the scoped exception, not a blanket operator lockout. The registration
    // state is `starting` until an actor claims it; pin it idle so the
    // bounded read returns at once instead of waiting for a turn.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET state='idle' WHERE alias=?",
                rusqlite::params![alice.alias],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        fx.call(
            Asserted::Operator,
            "agent_wait",
            json!({"alias": alice.alias, "until": "idle", "timeout": 1}),
        )
        .is_ok(),
        "operator lost alias-level liveness for the private assistant"
    );

    // A stored message addressed to the private assistant: its body is the
    // member's mail. The owning assistant still reads its own copy; the
    // operator, an unverified caller and a foreign assistant get no body.
    let private_msg = "cad2207-private-mail";
    let (dup, _state) = fx
        .shared
        .store
        .enqueue(
            &alice.alias,
            CAD2207_PRIVATE_BODY,
            None,
            private_msg,
            "test",
        )
        .unwrap_or_else(|e| panic!("seed the private mailbox message: {e}"));
    assert!(!dup, "the private mailbox message id collided");
    let own_mail = fx
        .as_assistant(
            &alice.alias,
            "message_read",
            json!({"message": private_msg}),
        )
        .unwrap_or_else(|e| panic!("owning assistant could not read its own mail: {e}"));
    assert!(
        own_mail["text"]
            .as_str()
            .is_some_and(|text| text.contains(CAD2207_PRIVATE_BODY)),
        "owning assistant's own mailbox body is missing: {own_mail}"
    );
    // The owning assistant read the SAME row body above; this refusal is
    // therefore about the caller, not about the message being unreadable.
    let operator_mail = fx.call(
        Asserted::Operator,
        "message_read",
        json!({"message": private_msg}),
    );
    assert!(
        operator_mail.is_err(),
        "a proven operator read the private assistant's stored mail body: {operator_mail:?}"
    );
    for (who, label) in [
        (Asserted::Unproven, "an unverified external caller"),
        (
            Asserted::Agent(ben.alias.clone()),
            "another member's assistant",
        ),
    ] {
        let read = fx.call(who, "message_read", json!({"message": private_msg}));
        assert!(
            read.is_err(),
            "{label} read the private assistant's stored mail body: {read:?}"
        );
    }

    let before = fx.tree();
    // Every alias-targeted private read: the operator acting only as
    // operator, an unproven external harness, and another member's bound
    // assistant. `agent_show` is also probed with the profile-instructions
    // flag so the profile projection is covered, not only the row.
    let probes: [(&str, Value); 5] = [
        ("agent_show", json!({"alias": alice.alias})),
        (
            "agent_show",
            json!({"alias": alice.alias, "include_profile_instructions": true}),
        ),
        ("agent_events", json!({"alias": alice.alias})),
        ("thread_read", json!({"alias": alice.alias})),
        ("message_list", json!({"to": alice.alias})),
    ];
    for (method, params) in &probes {
        // The same verb + alias succeeds for the owner's own assistant and
        // the same verb on a non-private alias succeeds for the operator, so
        // the refusal below is about THIS target, not a blanket error.
        let operator = fx.call(Asserted::Operator, method, params.clone());
        assert!(
            operator.is_err(),
            "a proven operator read member-private {method} {params}: {operator:?}"
        );
        let external = fx.call(Asserted::Unproven, method, params.clone());
        assert!(
            external.is_err(),
            "an unverified external caller read member-private {method} {params}: {external:?}"
        );
        let foreign = fx.as_assistant(&ben.alias, method, params.clone());
        assert!(
            foreign.is_err(),
            "another member's assistant read member-private {method} {params}: {foreign:?}"
        );
    }

    // `agent_requests` discloses a private assistant's pending effect
    // parameters, and `agent_send`/`agent_ask` would queue it a turn or steer
    // it. The same operator verbs on an ordinary agent still work — the
    // positive control that keeps this from being a blanket operator refusal.
    // `agent_requests` is deliberately NOT here: the landed guard keeps the
    // operator's actionable workflow and withholds only the pending params
    // (a redaction, not a refusal), so an `is_err` expectation would be
    // wrong. See the report for why it is left unexercised by this fixture.
    for (method, private, ordinary) in [
        (
            "agent_send",
            json!({"alias": alice.alias, "text": "private steer"}),
            json!({"alias": "ordinary-worker", "text": "ordinary steer"}),
        ),
        (
            "agent_ask",
            json!({"alias": alice.alias, "text": "private ask", "wait": 0}),
            json!({"alias": "ordinary-worker", "text": "ordinary ask", "wait": 0}),
        ),
    ] {
        assert!(
            fx.call(Asserted::Operator, method, ordinary.clone())
                .is_ok(),
            "operator lost {method} on a non-private target"
        );
        let operator = fx.call(Asserted::Operator, method, private.clone());
        assert!(
            operator.is_err(),
            "a proven operator reached member-private {method} {private}: {operator:?}"
        );
        let external = fx.call(Asserted::Unproven, method, private.clone());
        assert!(
            external.is_err(),
            "an unverified external caller reached member-private {method} {private}: {external:?}"
        );
        let foreign = fx.as_assistant(&ben.alias, method, private.clone());
        assert!(
            foreign.is_err(),
            "another member's assistant reached member-private {method} {private}: {foreign:?}"
        );
    }

    // Lists FILTER: the private row is absent while unrelated rows and the
    // operator's own operational view remain.
    for who in [Asserted::Operator, Asserted::Unproven] {
        let listed = fx
            .call(who.clone(), "agent_list", json!({}))
            .unwrap_or_else(|e| panic!("{who:?} agent_list refused: {e}"));
        let aliases: Vec<&str> = listed["agents"]
            .as_array()
            .expect("agent_list carries no agents array")
            .iter()
            .filter_map(|a| a["alias"].as_str())
            .collect();
        assert!(
            !aliases.contains(&alice.alias.as_str()),
            "{who:?} agent_list exposed member alice's private assistant row"
        );
        assert!(
            !aliases.contains(&ben.alias.as_str()),
            "{who:?} agent_list exposed member ben's private assistant row"
        );
        assert!(
            aliases.contains(&"ordinary-worker"),
            "{who:?} agent_list lost the unrelated non-private row"
        );
        let serialized = listed.to_string();
        assert!(
            !serialized.contains(CAD2207_PRIVATE_NAME)
                && !serialized.contains(CAD2207_PRIVATE_TONE)
                && !serialized.contains(alice.thread_id.as_str()),
            "{who:?} agent_list leaked private profile/conversation data"
        );
    }
    assert_eq!(
        fx.tree(),
        before,
        "a refused private read or list changed the tracker"
    );

    // Still positive after the refusals: the owner's own route is intact.
    let after_state = fx.op(
        "member_assist_state",
        json!({"member_envelope": fx.member_envelope("sub-alice")}),
    );
    assert_eq!(
        after_state["assistant"]["identity"]["name"].as_str(),
        Some(CAD2207_PRIVATE_NAME),
        "the owner's private profile broke after refused foreign reads"
    );
}

/// CAD-2207: a caller-named alias, profile label or claimed member is never
/// proof of identity. Only the SIGNED member envelope selects the member, and
/// an unverified external harness resolves no identity at all.
#[test]
fn profile_or_alias_label_never_manufactures_member_owner_authority() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let ben = fx.member_runtime("sub-ben");
    seed_cad2207_private_profile(&fx, "sub-alice");

    let alice_env = fx.member_envelope("sub-alice");
    // Control: the OWNER's signed envelope is the one proof that reads the
    // owner's private profile (and it is what the board relays).
    let owner = fx.op(
        "member_assist_state",
        json!({"member_envelope": alice_env.clone()}),
    );
    assert_eq!(
        owner["assistant"]["alias"].as_str(),
        Some(alice.alias.as_str())
    );
    assert_eq!(
        owner["assistant"]["identity"]["name"].as_str(),
        Some(CAD2207_PRIVATE_NAME)
    );

    // A DIFFERENT member's signed envelope resolves that member's own
    // assistant and never the first member's private value, even on the same
    // board relay.
    let other = fx.op(
        "member_assist_state",
        json!({"member_envelope": fx.member_envelope("sub-ben")}),
    );
    assert_eq!(
        other["assistant"]["alias"].as_str(),
        Some(ben.alias.as_str())
    );
    assert_ne!(
        other["assistant"]["identity"]["name"].as_str(),
        Some(CAD2207_PRIVATE_NAME),
        "another member's route returned alice's private profile"
    );
    assert!(!other.to_string().contains(CAD2207_PRIVATE_NAME));

    let before = fx.tree();
    // A caller-selected identity field is refused on the member route: the
    // label cannot redirect or widen the read.
    for selector in [
        json!({"sub": "sub-ben"}),
        json!({"profile": "assist-agent-sub-ben"}),
        json!({"alias": ben.alias}),
        json!({"member_as": "sub-ben"}),
        json!({"owner": "sub-ben"}),
    ] {
        let mut params = selector.clone();
        params["member_envelope"] = json!(alice_env);
        let r = fx.op_err("member_assist_state", params);
        assert!(
            r.is_err(),
            "a caller-selected identity field was accepted beside the signed \
             member envelope: {r:?}"
        );
    }
    // No envelope, only claims: still refused — no profile resolves.
    let claims_only = fx.op_err(
        "member_assist_state",
        json!({
            "sub": "sub-alice",
            "alias": alice.alias,
            "profile": "assist-agent-sub-alice",
            "member_as": "sub-alice",
            "owner": "sub-alice",
        }),
    );
    assert!(
        claims_only.is_err(),
        "claimed identity fields resolved a member profile without a signed \
         envelope: {claims_only:?}"
    );
    // The same claimed selectors do not unlock the generic private read.
    for extra in [
        json!({"sub": "sub-alice"}),
        json!({"profile": "assist-agent-sub-alice"}),
        json!({"member_as": "sub-alice"}),
        json!({"owner": "sub-alice"}),
        json!({"member_envelope": alice_env}),
    ] {
        let mut params = json!({"alias": alice.alias});
        for (key, value) in extra.as_object().expect("selector is not an object") {
            params[key.as_str()] = value.clone();
        }
        let r = fx.call(Asserted::Operator, "agent_show", params.clone());
        assert!(
            r.is_err(),
            "a claimed selector unlocked the private alias read: {params} -> {r:?}"
        );
    }
    // An unverified external harness never invents an identity.
    assert!(
        fx.call(Asserted::Unproven, "agent_identity", json!({}))
            .is_err(),
        "an unverified caller was attributed an identity"
    );
    assert!(
        fx.call(
            Asserted::Unproven,
            "agent_show",
            json!({"alias": alice.alias, "as": "assist-agent-sub-alice"}),
        )
        .is_err(),
        "an unverified caller read the private alias by claiming one"
    );
    assert_eq!(fx.tree(), before, "a refused selector changed the tracker");

    // Still positive: the owner's envelope keeps working after the refusals.
    let after_state = fx.op(
        "member_assist_state",
        json!({"member_envelope": fx.member_envelope("sub-alice")}),
    );
    assert_eq!(
        after_state["assistant"]["identity"]["name"].as_str(),
        Some(CAD2207_PRIVATE_NAME),
        "the owner's profile read broke after refused selectors"
    );
}

/// CAD-2207: the remote CLI's passive inbox inspector is a workspace OWNER,
/// and owner rights are not proof of the member whose personal assistant owns
/// the mailbox (`cli_rpc::rpc_cli_message_inbox`). The bound principal's own
/// verified `cli_envelope` still peeks its own mailbox, and an ordinary
/// (non-member) mailbox keeps the owner's inspection path; a DIFFERENT
/// workspace owner is refused for the member's assistant mailbox only.
///
/// Minting matches the real contract: the `agenticos-wiki-actor/1` header,
/// the closed claim set, and the fixture platform's JWKS (`kid`) — the daemon
/// re-verifies the Ed25519 signature, iss/aud/org/time pins and the scope
/// exactly as the container does.
fn cad2207_cli_envelope(fx: &Fx, sub: &str, role: &str) -> String {
    let now = crate::issue::time::now_epoch();
    let header = json!({"alg":"EdDSA","typ":"agenticos-wiki-actor/1","kid":KID});
    let claims = json!({
        "iss": fx.issuer,
        "aud": format!("http://{}", fx.host),
        "sub": sub,
        "actor": format!("users/{sub}"),
        "principal_kind": "user",
        "role_at_issue": role,
        "organization_id": ORG,
        "scope": ["cli.read", "cli.write"],
        "credential_id": format!("cred-{sub}"),
        "iat": now,
        "exp": now + 120,
        "jti": format!("cli-jti-{sub}-{now}"),
    });
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    );
    format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(fx.key.sign(signed.as_bytes()).as_ref())
    )
}

#[test]
fn cli_owner_cannot_inspect_a_member_private_assistant_mailbox() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    // A member's personal assistant registered as an `inbox` mailbox: the
    // passive inspector's real precondition. Without the privacy guard the
    // same owner envelope WOULD read it, so the refusal below can only come
    // from the member boundary — never from an unrelated placement error.
    let inbox_dir = fx.dir.path().join("inbox-cwd");
    std::fs::create_dir_all(&inbox_dir).unwrap();
    let register_inbox = |alias: &str| {
        fx.shared
            .store
            .register_agent(&crate::store::NewAgent {
                alias,
                provider: "inbox",
                endpoint_kind: "inbox",
                role: "worker",
                cwd: inbox_dir.to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap_or_else(|e| panic!("register inbox mailbox '{alias}': {e}"));
    };
    // The binding must exist BEFORE the alias is registered: `assistant_bind`
    // refuses an alias already registered as an ordinary agent (never adopt).
    fx.shared
        .store
        .assistant_bind(
            ORG,
            "sub-cli",
            "assist_sub-cli",
            "assist-agent-sub-cli",
            Some("rev0"),
        )
        .unwrap_or_else(|e| panic!("bind the member to its assistant mailbox: {e}"));
    register_inbox("assist-agent-sub-cli");
    fx.shared
        .store
        .enqueue(
            "assist-agent-sub-cli",
            CAD2207_PRIVATE_BODY,
            None,
            "mail-private",
            "test",
        )
        .unwrap_or_else(|e| panic!("seed the private mailbox: {e}"));
    // An ordinary mailbox (no member binding): it keeps the workspace owner's
    // existing inspection policy.
    register_inbox("inbox-ordinary");
    fx.shared
        .store
        .enqueue(
            "inbox-ordinary",
            "ordinary body",
            None,
            "mail-ordinary",
            "test",
        )
        .unwrap();

    let peek = |sub: &str, alias: &str| -> Result<Value> {
        fx.op_err(
            "cli_message_inbox",
            json!({"cli_envelope": cad2207_cli_envelope(&fx, sub, "owner"), "alias": alias}),
        )
    };

    // Positive control: the BOUND member's own verified envelope reads its
    // own mailbox.
    let own = peek("sub-cli", "assist-agent-sub-cli")
        .unwrap_or_else(|e| panic!("the bound member's own cli inbox read was refused: {e}"));
    assert!(
        own["messages"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["id"].as_str() == Some("mail-private"))),
        "the bound member's own cli inbox read lost its queued entry: {own}"
    );
    // Positive control: a workspace owner still inspects an ORDINARY mailbox.
    let ordinary = peek("sub-admin", "inbox-ordinary")
        .unwrap_or_else(|e| panic!("an owner's ordinary inbox inspection was refused: {e}"));
    assert!(
        ordinary["messages"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["id"].as_str() == Some("mail-ordinary"))),
        "an owner's ordinary inbox inspection lost its entry: {ordinary}"
    );

    // Native `agent_inbox` follows the same rule for the member's assistant
    // mailbox; the operator's ordinary peek still works.
    assert!(
        fx.call(
            Asserted::Operator,
            "agent_inbox",
            json!({"alias": "assist-agent-sub-cli", "peek": true}),
        )
        .is_err(),
        "a proven operator peeked a member's private assistant mailbox"
    );
    let ordinary_peek = fx
        .call(
            Asserted::Operator,
            "agent_inbox",
            json!({"alias": "inbox-ordinary", "peek": true}),
        )
        .expect("operator lost the ordinary inbox peek");
    assert!(
        ordinary_peek["messages"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["id"].as_str() == Some("mail-ordinary"))),
        "operator's ordinary inbox peek lost its entry: {ordinary_peek}"
    );

    // A DIFFERENT workspace owner is not the member: the SAME valid owner
    // envelope is refused for the member's assistant mailbox only.
    assert!(
        peek("sub-admin", "assist-agent-sub-cli").is_err(),
        "another workspace owner inspected a member's private assistant mailbox"
    );
    // Still positive: the bound member's read is intact after the refusal.
    assert!(
        peek("sub-cli", "assist-agent-sub-cli").is_ok(),
        "the bound member's cli inbox read broke after the refused foreign inspection"
    );
}

/// Mint a live verified public session for one principal — the CAD-1412
/// human-room guard's first input. The fixture writes the row through the
/// production opener with the fixture realm; the live roster the guard
/// fetches is set by [`cad2207_room_roster`]. `role` "owner" maps the board
/// user to the operator role (the ask authority); any other role is a plain
/// member.
fn cad2207_public_session(fx: &Fx, sub: &str, name: &str, role: &str) -> String {
    let now = crate::issue::time::now_epoch();
    let jti = format!("cad2207-{sub}-{now}");
    fx.shared
        .operator_auth()
        .open_public_bound_with_intent(
            crate::operator_auth::BoardUser {
                sub: sub.to_string(),
                email: format!("{sub}@example.com"),
                name: name.to_string(),
                role: if role == "owner" {
                    "operator".to_string()
                } else {
                    "member".to_string()
                },
                handle: sub.to_string(),
            },
            (jti.as_str(), now + 60),
            "cad2207-fixture",
            crate::operator_auth::PublicRealm {
                issuer: fx.issuer.clone(),
                company: ORG.to_string(),
                host: fx.host.clone(),
            },
            now,
            |_| Ok(()),
        )
        .unwrap()
        .expect("the fixture's verified public session was refused")
        .token
}

/// The live roster the fixture platform answers for the human-room guard.
fn cad2207_room_roster(fx: &Fx, members: &[(&str, &str, &str)]) {
    fx.authority.lock().unwrap().roster = members
        .iter()
        .map(|(sub, name, role)| json!({"subject_id": sub, "name": name, "role": role}))
        .collect();
}

/// CAD-2207: a shared room is not an authorization to expose a member's
/// personal assistant. `human_room_ask` refuses ANY member-assistant target
/// (its replies would be visible to every participant), while the same
/// verified owner still asks an ordinary agent in an ordinary mixed room.
///
/// The room is seeded through the STORE (the RPC add/create guards are a
/// different check), so this is the ask guard under test. The verified human
/// session is a real public-session row plus the fixture platform's live
/// roster — the daemon's own guard chain, not a mirrored decision.
#[test]
fn human_room_ask_refuses_a_member_private_assistant() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let assistant_uid = fx
        .shared
        .store
        .agent(&alice.alias)
        .expect("the assistant agent row is missing")
        .uid;
    let ordinary_uid = fx
        .shared
        .store
        .agent("ordinary-worker")
        .expect("the ordinary worker is missing")
        .uid;

    cad2207_room_roster(&fx, &[("sub-room-owner", "Room Owner", "owner")]);
    let token = cad2207_public_session(&fx, "sub-room-owner", "Room Owner", "owner");

    let actor = crate::store::human_threads::VerifiedHuman::with_name(
        &fx.issuer,
        ORG,
        "sub-room-owner",
        "Room Owner",
    )
    .unwrap();
    let make_room = |title: &str, key: &str, agent: &str| {
        fx.shared
            .store
            .human_group_create(
                &actor,
                "mixed_group",
                title,
                key,
                &[crate::store::human_threads::RoomMember::Agent(
                    agent.to_string(),
                )],
            )
            .unwrap_or_else(|e| panic!("seed room '{title}': {e}"))
    };
    let private_room = make_room("private room", "cad2207-room-private", &alice.alias);
    let ordinary_room = make_room("ordinary room", "cad2207-room-ordinary", "ordinary-worker");

    let ask = |room: &str, uid: &str, client_id: &str| -> Result<Value> {
        fx.op_err(
            "human_room_ask",
            json!({
                "token": token,
                "room": room,
                "agent_uid": uid,
                "client_id": client_id,
                "body": "a room question",
            }),
        )
    };

    // Positive control: the same verified owner still asks an ordinary agent.
    assert!(
        ask(&ordinary_room, &ordinary_uid, "cad2207-ask-ordinary").is_ok(),
        "a verified owner lost the ordinary mixed-room ask"
    );

    let before = fx.tree();
    let queued_before = fx.shared.store.messages(&alice.alias).unwrap().len();
    // Negative: the member's personal assistant is refused in a shared room.
    assert!(
        ask(&private_room, &assistant_uid, "cad2207-ask-private").is_err(),
        "a verified owner addressed a member's private assistant in a shared room"
    );
    assert_eq!(
        fx.shared.store.messages(&alice.alias).unwrap().len(),
        queued_before,
        "a refused room ask queued the private assistant"
    );
    assert_eq!(fx.tree(), before, "a refused room ask changed the tracker");
}

/// CAD-2207: a member assistant's pending requests and their input are the
/// member's private context. A generic operator connection cannot list them
/// and cannot answer ANY decision for them — accept, decline (blind or
/// answered) and the free-text request included: every path refuses BEFORE a
/// handle is claimed, with the store's generic lifecycle text, and discloses
/// no alias, handle or input. The pending request and the underlying tracker
/// stay untouched. Ordinary agents keep the operator's normal listing and
/// accept/decline paths.
#[test]
fn private_request_input_is_withheld_and_blind_approval_is_refused() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    const PRIVATE_INPUT: &str = "private request input: member-only command text";
    const SAFE_HANDLE: &str = "cad2207-private-request";
    const FREETEXT_HANDLE: &str = "cad2207-private-freetext";
    const ORDINARY_HANDLE: &str = "cad2207-ordinary-request";
    const ORDINARY_DECLINE_HANDLE: &str = "cad2207-ordinary-decline";

    let seed = |handle: &str, alias: &str, method: &str| {
        fx.shared.pending.lock().unwrap().insert(
            handle.to_string(),
            super::PendingRequest {
                alias: alias.to_string(),
                id: Value::Null,
                method: method.to_string(),
                params: json!({
                    "kind": "approval",
                    "tool": "Bash",
                    "input_summary": PRIVATE_INPUT,
                    "input": {"command": PRIVATE_INPUT},
                }),
            },
        );
    };
    seed(SAFE_HANDLE, &alice.alias, "cadence/approval");
    seed(FREETEXT_HANDLE, &alice.alias, "item/tool/requestUserInput");
    seed(ORDINARY_HANDLE, "ordinary-worker", "cadence/approval");
    seed(
        ORDINARY_DECLINE_HANDLE,
        "ordinary-worker",
        "cadence/approval",
    );

    let pending_has = |handle: &str| fx.shared.pending.lock().unwrap().contains_key(handle);
    let pending_shape = || {
        let mut rows: Vec<(String, String)> = fx
            .shared
            .pending
            .lock()
            .unwrap()
            .iter()
            .map(|(handle, request)| (handle.clone(), request.alias.clone()))
            .collect();
        rows.sort();
        rows
    };
    let answered_count = || fx.shared.answered.lock().unwrap().len();
    let generic_refusal = crate::store::RESTRICTED_LIFECYCLE;
    let assert_generic = |error: &crate::error::Error, what: &str| {
        let text = error.to_string();
        assert_eq!(
            text, generic_refusal,
            "{what} did not answer with the generic lifecycle refusal: {text}"
        );
        for secret in [
            alice.alias.as_str(),
            "sub-alice",
            SAFE_HANDLE,
            FREETEXT_HANDLE,
            PRIVATE_INPUT,
        ] {
            assert!(
                !text.contains(secret),
                "{what} leaked '{secret}' in its refusal: {text}"
            );
        }
    };

    let before_pending = pending_shape();
    let before_answered = answered_count();
    let before_tree = fx.tree();
    let before_agent = fx.shared.store.agent(&alice.alias).unwrap().to_json();
    let before_binding = fx
        .shared
        .store
        .assistant_binding(ORG, "sub-alice")
        .unwrap()
        .expect("the fixture binding vanished")
        .to_json();

    // ---- the generic operator cannot even list the private rows ----
    let error = fx
        .call(
            Asserted::Operator,
            "agent_requests",
            json!({"alias": alice.alias}),
        )
        .expect_err("the generic operator listed a member assistant's pending requests");
    assert_generic(&error, "agent_requests");

    // ---- every decision — blind or answered, accept or decline, provider or
    // free-text — is refused before the handle is claimed ----
    for params in [
        json!({"alias": alice.alias, "request": SAFE_HANDLE, "decision": "accept"}),
        json!({"alias": alice.alias, "request": SAFE_HANDLE, "decision": "decline"}),
        json!({"alias": alice.alias, "request": SAFE_HANDLE, "decision": "accept", "answers": {"q": "a"}}),
        json!({"alias": alice.alias, "request": SAFE_HANDLE, "decision": "decline", "answers": {"q": "a"}}),
        json!({"alias": alice.alias, "request": FREETEXT_HANDLE, "decision": "decline"}),
        json!({"alias": alice.alias, "request": FREETEXT_HANDLE, "answers": {"q": "a"}}),
    ] {
        let error = fx
            .call(Asserted::Operator, "agent_respond", params.clone())
            .expect_err("an operator decision for a member assistant was accepted");
        assert_generic(&error, "agent_respond");
    }

    assert_eq!(
        pending_shape(),
        before_pending,
        "a refused listing/decision changed the pending requests"
    );
    assert!(
        pending_has(SAFE_HANDLE) && pending_has(FREETEXT_HANDLE),
        "a refused decision consumed a private request"
    );
    assert_eq!(
        answered_count(),
        before_answered,
        "a refused decision parked an answer"
    );
    assert_eq!(
        fx.tree(),
        before_tree,
        "a refused decision changed the tracker"
    );
    assert_eq!(
        fx.shared.store.agent(&alice.alias).unwrap().to_json(),
        before_agent,
        "a refused decision changed the assistant's runtime row"
    );
    assert_eq!(
        fx.shared
            .store
            .assistant_binding(ORG, "sub-alice")
            .unwrap()
            .expect("the fixture binding vanished")
            .to_json(),
        before_binding,
        "a refused decision changed the member binding"
    );

    // ---- the same operator listing still carries an ordinary agent's real
    // params: the refusal is target-keyed, not a blanket denial ----
    let ordinary_listed = fx
        .call(
            Asserted::Operator,
            "agent_requests",
            json!({"alias": "ordinary-worker"}),
        )
        .expect("operator agent_requests for an ordinary agent refused");
    assert!(
        ordinary_listed.to_string().contains(PRIVATE_INPUT),
        "the operator's ordinary-agent request listing lost its params: {ordinary_listed}"
    );
    assert!(
        pending_has(ORDINARY_HANDLE) && pending_has(ORDINARY_DECLINE_HANDLE),
        "the control listing consumed the ordinary requests"
    );

    // ---- ordinary agents keep the normal accept and decline paths ----
    let ordinary_accept = fx
        .call(
            Asserted::Operator,
            "agent_respond",
            json!({"alias": "ordinary-worker", "request": ORDINARY_HANDLE, "decision": "accept"}),
        )
        .expect("an ordinary agent's request lost the operator accept path");
    assert_eq!(ordinary_accept["state"], "answered");
    assert!(
        !pending_has(ORDINARY_HANDLE),
        "the ordinary accept did not consume its request"
    );
    let ordinary_decline = fx
        .call(
            Asserted::Operator,
            "agent_respond",
            json!({"alias": "ordinary-worker", "request": ORDINARY_DECLINE_HANDLE, "decision": "decline"}),
        )
        .expect("an ordinary agent's request lost the operator decline path");
    assert_eq!(ordinary_decline["state"], "answered");
    assert!(
        !pending_has(ORDINARY_DECLINE_HANDLE),
        "the ordinary decline did not consume its request"
    );
}

/// CAD-1380 acceptance 2 (durable notification intent). A real pending
/// `assist_intent` written by a real `member_job_request` must survive the
/// daemon stream's real 200-row retention (`prune_stream("daemon", 200)`)
/// after more than 200 ordinary daemon events. The committed dedupe key of
/// the retained job must not be recreated by a repeat of the same request.
/// (Feature-OFF queue retention is not covered here; see the coverage note.)
#[test]
fn pending_assist_intent_survives_daemon_stream_retention() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    fx.provision("sub-ivy");
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-ivy"), "title": "retain me", "request_key": "ret-k1", "body": "b"}),
    );
    let job = req["job"].as_str().unwrap().to_string();
    let pending_before = fx
        .shared
        .store
        .pending_assist_job_notifications(500)
        .unwrap();
    let pending_for_job = |rows: &[(i64, Value)]| {
        rows.iter()
            .filter(|(_, p)| p["job"].as_str() == Some(job.as_str()))
            .count()
    };
    assert!(
        pending_for_job(&pending_before) >= 1,
        "the real request left no pending intent for job {job}"
    );

    // 210 ordinary daemon events, then the real watcher retention pass.
    for i in 0..210 {
        fx.shared
            .store
            .event_public("daemon", "wal_checkpointed", json!({"n": i}))
            .unwrap();
    }
    fx.shared.store.prune_stream("daemon", 200).unwrap();

    let pending_after = fx
        .shared
        .store
        .pending_assist_job_notifications(500)
        .unwrap();
    assert!(
        pending_for_job(&pending_after) >= 1,
        "daemon-stream retention deleted the pending assist intent for job {job}"
    );
    // Dedupe: re-emitting the EXACT canonical transition (same request key)
    // must not recreate a pending intent for the retained job.
    let _ = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-ivy"), "title": "retain me", "request_key": "ret-k1", "body": "b"}),
    );
    let again = fx
        .shared
        .store
        .pending_assist_job_notifications(500)
        .unwrap();
    assert_eq!(
        pending_for_job(&again),
        pending_for_job(&pending_after),
        "a repeated transition re-created an intent for job {job}"
    );
}

/// CAD-1380 acceptance 2c, privacy half. A real drain of a pending job intent
/// must return Ok and must not expose the private request body or the service
/// custody token to any notice it delivers to the member's assistant.
#[test]
fn drain_delivers_no_private_body_or_custody_to_notices() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    fx.provision("sub-fay");
    let _ = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-fay"), "title": "drain privacy", "request_key": "drain-k1", "body": "PRIVATE_DRAIN_BODY"}),
    );
    assert!(
        !fx.shared
            .store
            .pending_assist_job_notifications(500)
            .unwrap()
            .is_empty(),
        "no pending intent to drain"
    );
    let queued = fx
        .shared
        .route_assist_job_notifications()
        .expect("drain returned an error");
    assert!(
        queued >= 1,
        "drain delivered no notice; privacy outcome unobserved"
    );
    let notices = fx.shared.store.messages("assist-agent-sub-fay").unwrap();
    assert!(
        notices.iter().any(|m| !m.body.is_empty()),
        "drain queued a notice but no body is observable on the member assistant"
    );
    for m in notices {
        assert!(
            !m.body.contains("PRIVATE_DRAIN_BODY"),
            "private request body reached a notice"
        );
        assert!(
            !m.body.contains(&format!("hcs_{}", "d".repeat(43))),
            "service custody token leaked into a notice"
        );
    }
}

/// CAD-1380 acceptance 2b (terminal dedupe retention). The terminal dedupe
/// row of a LIVE canonical job must survive the real 200-row daemon-stream
/// prune after more than 200 ordinary events, and re-emitting the exact same
/// canonical request must not recreate a second terminal row.
#[test]
fn terminal_dedupe_for_live_canonical_job_survives_retention() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    fx.provision("sub-tia");
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-tia"), "title": "terminal live", "request_key": "tlk-1", "body": "b"}),
    );
    let job = req["job"].as_str().unwrap().to_string();
    let seq = fx
        .shared
        .store
        .pending_assist_job_notifications(500)
        .unwrap()
        .into_iter()
        .find(|(_, p)| p["job"].as_str() == Some(job.as_str()))
        .expect("live job has no pending intent")
        .0;
    fx.shared
        .store
        .finish_assist_job_notification(seq, "delivered")
        .unwrap();

    let db = fx.dir.path().join("cadence.sqlite3");
    let terminal = |j: &str| -> i64 {
        rusqlite::Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM events WHERE alias='daemon' AND kind='assist_intent' \
                 AND json_extract(payload,'$.job')=?1 AND json_extract(payload,'$.status')!='pending'",
                [j],
                |r| r.get(0),
            )
            .unwrap()
    };
    let before = terminal(&job);
    assert!(before >= 1, "no terminal dedupe row before retention");

    for i in 0..210 {
        fx.shared
            .store
            .event_public("daemon", "wal_checkpointed", json!({"n": i}))
            .unwrap();
    }
    fx.shared.store.prune_stream("daemon", 200).unwrap();
    assert!(
        terminal(&job) >= before,
        "daemon-stream retention deleted the terminal dedupe row of live job {job}"
    );

    let _ = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-tia"), "title": "terminal live", "request_key": "tlk-1", "body": "b"}),
    );
    assert_eq!(
        terminal(&job),
        before,
        "a repeated transition re-created a terminal row for live job {job}"
    );
}

/// CAD-1380 acceptance 3 (confined principal grants). Distinct verified
/// subjects must never share one confined read/write cwd. Two subjects that
/// share their first 48 characters (the `sub_safe` truncation) must each get
/// a DISTINCT assistant cwd when lazily registered through the real
/// `member_assist_state` path. Alias-legal subjects only: the agent alias
/// grammar (`[a-z0-9-]`, <=64) gates registration before the cwd step.
#[test]
fn distinct_member_subjects_never_share_one_confined_cwd() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let shared = "p".repeat(48);
    let (x, y) = (shared.clone(), format!("{shared}-t"));
    for sub in [&x, &y] {
        fx.provision(sub);
        let _ = fx.op(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope(sub)}),
        );
    }
    let cwd_of = |sub: &str| {
        fx.shared
            .store
            .agent(&format!("assist-agent-{sub}"))
            .unwrap()
            .cwd
    };
    assert_ne!(
        cwd_of(&x),
        cwd_of(&y),
        "distinct subjects sharing a 48-char prefix share one confined cwd"
    );
}

/// CAD-1380 acceptance 4 (own-turn report resolution). A bound assistant's
/// own held turn, reported with NO message id (the caller-bound form) or a
/// unique id PREFIX, must reach the existing `report_target` resolver rather
/// than being refused by the member policy's exact lookup. A forged
/// reference to a foreign member's message (exact id) stays refused.
#[test]
fn assistant_own_held_turn_report_reaches_existing_resolver() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let rt = fx.member_runtime("sub-dana");
    let own = rt.alias.clone();
    let other = fx.member_runtime("sub-eli");

    // Hold the assistant's own turn through the real store turn marker.
    fx.shared
        .store
        .enqueue(&own, "own work", None, "own-held-1", "test")
        .unwrap();
    let own_msg = fx
        .shared
        .store
        .messages(&own)
        .unwrap()
        .into_iter()
        .find(|m| m.body == "own work")
        .expect("own message not stored")
        .id;
    fx.shared
        .store
        .mark_running(&own_msg, "own-held-turn")
        .unwrap();

    // Forged foreign reference: the other member's exact message id.
    let forged = err_text(
        fx.as_assistant(
            &own,
            "message_report",
            json!({"message": other.foreign_msg, "result": "x"}),
        ),
        "forged foreign message_report",
    );
    assert!(
        !forged.is_empty(),
        "a forged foreign message_report was not refused"
    );

    // Own held turn, id-less: the resolver must run past the member policy.
    // Token/generation/state refusals downstream are acceptable here; a
    // member-policy refusal naming the message as "not its own" is not.
    let idless = fx.as_assistant(&own, "message_report", json!({"result": "done"}));
    if let Err(e) = &idless {
        assert!(
            !e.to_string().contains("is not its own"),
            "id-less own-turn report blocked by the member policy: {e}"
        );
    }

    // Own held turn, unique id prefix: the same policy must not refuse it.
    let prefix = &own_msg[..own_msg.len().min(8)];
    let by_prefix = fx.as_assistant(
        &own,
        "message_report",
        json!({"message": prefix, "token": "own-held-turn", "result": "done"}),
    );
    if let Err(e) = &by_prefix {
        assert!(
            !e.to_string().contains("is not its own"),
            "unique-prefix own-turn report blocked by the member policy: {e}"
        );
    }
}

/// CAD-1380 acceptance 5 (service-transport original credential). Under
/// enrolled service transport, a member-initiated call whose signed envelope
/// names revoked credential C1 must be refused even though the binding has
/// been reconciled to a live C2. The current-C2 call stays positive, and the
/// revalidation body must carry the incoming verified credential, not a
/// binding borrow.
#[test]
fn service_transport_keeps_incoming_credential_provenance() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    fx.provision("sub-gus");

    // C1 is the credential the envelope is signed under (`cred-sub-gus`).
    let c1_envelope = fx.envelope(
        "sub-gus",
        json!({}),
        json!({"credential_id": "cred-sub-gus"}),
    );

    // Positive control: C1 live, service transport, member read succeeds.
    assert!(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": c1_envelope.clone()}),
        )
        .is_ok(),
        "live C1 member refused under service transport"
    );

    // Revoke C1 at the platform and re-pin the binding to a live C2 (a renewal
    // reconcile). The C1-signed envelope must now be refused.
    fx.set_member("sub-gus", |m| {
        m.recorded_credential = "cred-sub-gus-renewed".to_string();
        m.credential_live = true;
    });
    // C1 stays revoked in the per-credential map (the old credential is dead).
    fx.set_member("sub-gus", |m| {
        m.live_credentials
            .insert("cred-sub-gus".to_string(), (false, Vec::new()));
    });
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": c1_envelope}),
        ),
        "revoked-C1 envelope under service transport",
    );
    assert!(
        e.contains("revalidation")
            || e.contains("authority")
            || e.contains("refused")
            || e.contains("member"),
        "revoked-C1 envelope refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before, "a revoked-C1 call moved the tracker");

    // Current-C2 control: a fresh envelope under the live C2 stays positive.
    let c2_envelope = fx.envelope(
        "sub-gus",
        json!({}),
        json!({"credential_id": "cred-sub-gus-renewed"}),
    );
    let c2 = fx.op_err(
        "member_assist_state",
        json!({"member_envelope": c2_envelope}),
    );
    assert!(
        c2.is_ok(),
        "live C2 member refused under service transport: {c2:?}"
    );
}

fn with_deferred_service(fx: &Fx) {
    fx.enroll_service(ServiceAuthority {
        token: format!("hcs_{}", "d".repeat(43)),
        org: ORG.to_string(),
        audience: format!("http://{}", fx.host),
        caps: vec!["assist.revalidate".into()],
        live: true,
    });
}

/// The stored `assist_requests` provenance row for a job — the deferred
/// commit's re-prove target.
fn assist_provenance(fx: &Fx, job: &str) -> crate::store::AssistRequest {
    fx.shared
        .store
        .assist_request_for_job(job)
        .unwrap_or_else(|e| panic!("provenance lookup failed: {e}"))
        .unwrap_or_else(|| panic!("no assist_requests row for {job}"))
}

/// A member requests a job, then the operator/PM path dispatches the task
/// to an ordinary worker — exercising `rpc_task_dispatch`'s deferred-commit
/// re-prove end to end under a live service credential.
#[test]
fn deferred_dispatch_reproves_live_authority_and_refuses_self() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let rt = fx.member_runtime("sub-dora");
    // Join the ordinary worker into each member-PM's group as the
    // dispatch target — `check_group_member` requires `params.upstream`
    // name the job's PM, so a legitimate dispatch joins the group first
    // and the deferred-authority guard (not group membership) is what
    // decides each case.
    let join_group = |pm: &str| {
        fx.shared
            .store
            .fixture_write(|tx| {
                tx.execute(
                    "UPDATE agents SET params=? WHERE alias='ordinary-worker'",
                    rusqlite::params![json!({"upstream": pm}).to_string()],
                )?;
                Ok(())
            })
            .unwrap();
    };
    join_group(&rt.alias);

    // The member requests the job through the real member path (writes the
    // `assist_requests` provenance + creates the job + draft task).
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-dora"), "title": "dora job", "request_key": "d-k1", "body": "b"}),
    );
    let job = req["job"].as_str().unwrap().to_string();
    let task = format!("{job}-t1");
    let prov = assist_provenance(&fx, &job);
    assert_eq!(prov.org, ORG);
    assert_eq!(prov.sub, "sub-dora");
    assert_eq!(prov.credential_id.as_deref(), Some("cred-sub-dora"));

    // POSITIVE: member live + service grant + ordinary worker target —
    // the deferred `job_dispatch` revalidation answers allow and the
    // dispatch lands. This is the case a wrong operation enum (e.g.
    // `assist.job.dispatch` absent from the strict schema) could never
    // satisfy: the platform would 400 the pull and fail closed.
    let dispatched = fx.op(
        "task_dispatch",
        json!({"task": task, "to": rt.worker, "by": "operator"}),
    );
    assert!(
        dispatched["task"]["id"].as_str() == Some(task.as_str()),
        "approved assist-bound dispatch refused under live authority: {dispatched}"
    );
    let kickoff_id = dispatched["message"].as_str().unwrap().to_string();
    assert!(!kickoff_id.is_empty());
    // The kickoff went to the WORKER, never the assistant.
    let kickoff = fx.shared.store.message(&kickoff_id).unwrap().unwrap();
    assert_eq!(
        kickoff.alias, rt.worker,
        "the dispatch kickoff landed on the assistant"
    );

    // ---- removed member after enqueue: the queued effect fails closed ----
    fx.remove_member("sub-dora");
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "task_dispatch",
            json!({"task": task, "to": rt.worker, "by": "operator"}),
        ),
        "task_dispatch after member removal",
    );
    assert!(
        e.contains("revalidation")
            || e.contains("authority")
            || e.contains("refused")
            || e.contains("allow")
            || e.contains("member"),
        "removed-member dispatch refused for another reason: {e}"
    );
    assert_eq!(
        fx.tree(),
        before,
        "a removed member's queued dispatch committed an effect"
    );

    // ---- issuing credential revoked / grant narrowed after enqueue ----
    let rt2 = fx.member_runtime("sub-ed");
    join_group(&rt2.alias);
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-ed"), "title": "ed job", "request_key": "e-k1", "body": "b"}),
    );
    let job2 = req["job"].as_str().unwrap().to_string();
    let task2 = format!("{job2}-t1");
    // Revoke the member's recorded issuing credential at the platform.
    fx.set_member("sub-ed", |m| m.credential_live = false);
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "task_dispatch",
            json!({"task": task2, "to": rt2.worker, "by": "operator"}),
        ),
        "credential-revoked dispatch",
    );
    assert!(
        e.contains("revalidation") || e.contains("authority") || e.contains("refused"),
        "credential-revoked dispatch refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);
    // Narrow the grant — `assist.jobs` withdrawn → the deferred op refuses.
    fx.set_member("sub-ed", |m| {
        m.credential_live = true;
        m.credential_caps = vec!["assist.read".into()];
    });
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "task_dispatch",
            json!({"task": task2, "to": rt2.worker, "by": "operator"}),
        ),
        "grant-narrowed dispatch",
    );
    assert!(
        e.contains("revalidation") || e.contains("authority") || e.contains("refused"),
        "grant-narrowed dispatch refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);

    // ---- provenance unreadable/absent → fail closed, never skip the re-prove ----
    let rt3 = fx.member_runtime("sub-fox");
    join_group(&rt3.alias);
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-fox"), "title": "fox job", "request_key": "f-k1", "body": "b"}),
    );
    let job3 = req["job"].as_str().unwrap().to_string();
    let task3 = format!("{job3}-t1");
    // Deleting the provenance row does NOT unbind the job — its id is the
    // `assist-<digest>` family, so a missing/corrupt provenance is a
    // fail-closed refusal, never a silent drop to an ordinary dispatch
    // that would commit a member effect with no live re-prove.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute("DELETE FROM assist_requests WHERE job=?", [job3.as_str()])?;
            Ok(())
        })
        .unwrap();
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "task_dispatch",
            json!({"task": task3, "to": rt3.worker, "by": "operator"}),
        ),
        "missing-provenance dispatch",
    );
    assert!(
        e.contains("assist")
            || e.contains("provenance")
            || e.contains("revalidation")
            || e.contains("binding")
            || e.contains("refused"),
        "missing-provenance dispatch refused for another reason: {e}"
    );
    assert_eq!(
        fx.tree(),
        before,
        "a dispatch committed with no member provenance to re-prove"
    );

    // ---- self-dispatch refuses: the `to` override AND the stored assignee ----
    let rt4 = fx.member_runtime("sub-gail");
    join_group(&rt4.alias);
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-gail"), "title": "gail job", "request_key": "g-k1", "body": "b"}),
    );
    let job4 = req["job"].as_str().unwrap().to_string();
    let task4 = format!("{job4}-t1");
    let before = fx.tree();
    // The `to` override naming the assistant's OWN alias must refuse —
    // self-approval. The check compares the binding's `alias` (the
    // registered agent), never the `assist_<sub>` handle.
    let e = err_text(
        fx.op_err(
            "task_dispatch",
            json!({"task": task4, "to": rt4.alias, "by": "operator"}),
        ),
        "self-dispatch via to=alias",
    );
    assert!(
        e.contains("self") || e.contains("assist") || e.contains("refused"),
        "self-dispatch to the assistant alias refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);

    // A task whose STORED `assignee` is the assistant alias — written
    // directly so `to` is absent — must refuse the same way. The guard's
    // self-approval arm checks the FINAL resolved assignee (`to` OR the
    // task's stored `assignee`), so a stored self-assignee is refused even
    // with no override param.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE tasks SET assignee=? WHERE id=?",
                rusqlite::params![rt4.alias, task4],
            )?;
            Ok(())
        })
        .unwrap();
    let before = fx.tree();
    let e = err_text(
        fx.op_err("task_dispatch", json!({"task": task4, "by": "operator"})),
        "self-dispatch via stored assignee",
    );
    assert!(
        e.contains("self") || e.contains("assist") || e.contains("refused"),
        "self-dispatch via stored assignee refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);
}

/// A configured `hosted-board` custody record whose bytes cannot be loaded
/// must refuse the deferred dispatch — never silently degrade to the
/// member-envelope bearer path at consume time.
#[test]
fn deferred_dispatch_fails_closed_on_unloadable_custody() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-hal");
    // Group join so the dispatch reaches the deferred-authority guard.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET params=? WHERE alias='ordinary-worker'",
                rusqlite::params![json!({"upstream": rt.alias}).to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-hal"), "title": "hal job", "request_key": "h-k1", "body": "b"}),
    );
    let job = req["job"].as_str().unwrap().to_string();
    let task = format!("{job}-t1");
    // Land the `platform_credentials` row WITHOUT custody bytes — the
    // fail-closed arm: a half-configured service binding refuses rather
    // than degrading.
    let host = fx.host.clone();
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT OR REPLACE INTO platform_credentials
                 (platform,account,scopes,fingerprint,custody,exchange,enrolled_at,by,connection_id,credential_revision)
                 VALUES('hosted-board',?1,'[\"assist.revalidate\"]','fp','file','token',1.0,'operator','conn',1)",
                rusqlite::params![host],
            )?;
            Ok(())
        })
        .unwrap();
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "task_dispatch",
            json!({"task": task, "to": rt.worker, "by": "operator"}),
        ),
        "unloadable-custody dispatch",
    );
    assert!(
        e.contains("custody") || e.contains("credential") || e.contains("refused"),
        "unloadable-custody dispatch refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);
}

/// The queued effect's RECORDED provenance is original — a job_request
/// minted under `cred-sub` keeps that `credential_id` in its
/// `assist_requests` row, and the deferred dispatch re-proves THAT
/// recorded credential, never a credential minted later. A queued
/// request whose issuing credential was revoked/narrowed after enqueue
/// CANNOT silently adopt a replacement credential: the platform answers
/// `allow:false` because the recorded credential is dead, even while a
/// fresh mint under a different (live) credential would pass its own
/// provenance check. This is SEC-002's effect-time rule — the deferred
/// commit re-proves the REQUEST's recorded authority, not whatever the
//  member can mint now.
#[test]
fn deferred_dispatch_reproves_original_provenance_not_replacement() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let rt = fx.member_runtime("sub-pat");
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET params=? WHERE alias='ordinary-worker'",
                rusqlite::params![json!({"upstream": rt.alias}).to_string()],
            )?;
            Ok(())
        })
        .unwrap();

    // The member requests under the ORIGINAL recorded credential
    // (`cred-sub-pat`). The provenance row must record it verbatim —
    // that record, not a later mint, is what the deferred re-prove names.
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-pat"), "title": "pat job", "request_key": "p-k1", "body": "b"}),
    );
    let job = req["job"].as_str().unwrap().to_string();
    let task = format!("{job}-t1");
    let prov = assist_provenance(&fx, &job);
    assert_eq!(
        prov.credential_id.as_deref(),
        Some("cred-sub-pat"),
        "the request's recorded provenance is not the issuing credential: {prov:?}"
    );

    // Between enqueue and commit the issuing credential is revoked AND
    // the member mints a replacement credential — a live bridge with a
    // different id. The platform's binding still records the ORIGINAL
    // issuing credential as provenance; the revalidate must compute
    // `allow` from THAT recorded credential's (now-dead) state. The
    // deferred dispatch therefore refuses even though a brand-new member
    // envelope would verify — the queued effect cannot silently adopt
    // the replacement credential's authority.
    fx.set_member("sub-pat", |m| m.credential_live = false);
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "task_dispatch",
            json!({"task": task, "to": rt.worker, "by": "operator"}),
        ),
        "queued dispatch after original-credential revocation",
    );
    assert!(
        e.contains("revalidation")
            || e.contains("authority")
            || e.contains("refused")
            || e.contains("allow")
            || e.contains("member"),
        "old-provenance dispatch refused for another reason: {e}"
    );
    assert_eq!(
        fx.tree(),
        before,
        "a queued effect committed under a dead issuing credential"
    );

    // Restore the ORIGINAL credential's liveness — only then does the
    // recorded provenance re-prove and the queued dispatch commit. The
    // replacement credential never entered the re-prove; the dispatch is
    // bound to the request's own recorded authority end to end.
    fx.set_member("sub-pat", |m| m.credential_live = true);
    let dispatched = fx.op(
        "task_dispatch",
        json!({"task": task, "to": rt.worker, "by": "operator"}),
    );
    assert!(
        dispatched["task"]["id"].as_str() == Some(task.as_str()),
        "restored-provenance dispatch refused: {dispatched}"
    );
}

/// A GLOBAL assistant-level provenance re-pin must NEVER reauthorize an
/// OLD queued dispatch whose own recorded `assist_requests.credential_id`
/// is dead — the deferred `job_dispatch` re-prove names the OP's own
/// recorded credential (`resource.credential_id`), not the binding's
/// latest. The ONLY path that admits it is the explicit job-specific
/// `member_job_reconcile`, which re-pins the job's recorded provenance to
/// the member's CURRENT live credential — bound to job + owner + current
/// actor, never a silent global adoption.
#[test]
fn old_queued_dispatch_needs_job_reconcile_not_global_renewal() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let rt = fx.member_runtime("sub-quinn");
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET params=? WHERE alias='ordinary-worker'",
                rusqlite::params![json!({"upstream": rt.alias}).to_string()],
            )?;
            Ok(())
        })
        .unwrap();

    // The member requests under the ORIGINAL recorded credential.
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-quinn"), "title": "q job", "request_key": "q-k1", "body": "b"}),
    );
    let job = req["job"].as_str().unwrap().to_string();
    let task = format!("{job}-t1");
    let prov = assist_provenance(&fx, &job);
    assert_eq!(prov.credential_id.as_deref(), Some("cred-sub-quinn"));

    // The original issuing credential dies (revoked/expired) AND the
    // member renews the assistant under a NEW live credential — the
    // binding's recorded provenance is re-pinned to `cred-sub-quinn-new`
    // (the global reconcile the platform's `/v2/assistant` path runs).
    // The queued dispatch's OWN recorded `assist_requests.credential_id`
    // is still `cred-sub-quinn` (dead) — a global renewal must NOT
    // reauthorize it.
    fx.set_member("sub-quinn", |m| {
        // The binding's recorded provenance is re-pinned to the NEW live
        // credential (`credential_live`/caps describe the RECORDED cred,
        // now the new one — live). The ORIGINAL op credential stays dead
        // in `live_credentials`: the queued dispatch's own recorded
        // `assist_requests.credential_id` (`cred-sub-quinn`) is what the
        // `job_dispatch` re-prove names — and it is dead.
        m.credential_live = true;
        m.recorded_credential = "cred-sub-quinn-new".to_string();
        m.live_credentials.insert(
            "cred-sub-quinn-new".to_string(),
            (
                true,
                vec![
                    "assist.read".into(),
                    "assist.write".into(),
                    "assist.jobs".into(),
                ],
            ),
        );
        m.live_credentials.insert(
            "cred-sub-quinn".to_string(),
            (false, vec!["assist.jobs".into()]),
        );
    });
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "task_dispatch",
            json!({"task": task, "to": rt.worker, "by": "operator"}),
        ),
        "queued dispatch after global renewal",
    );
    assert!(
        e.contains("revalidation")
            || e.contains("authority")
            || e.contains("refused")
            || e.contains("allow")
            || e.contains("credential")
            || e.contains("member"),
        "global-renewal queued dispatch refused for another reason: {e}"
    );
    assert_eq!(
        fx.tree(),
        before,
        "a global renewal silently adopted an old queued dispatch"
    );

    // The ONLY admission path: the OWNING member's explicit job-specific
    // `member_job_reconcile` — re-pins this job's recorded provenance to
    // the member's CURRENT live credential (the envelope's `credential_id`),
    // pinned to the job's CURRENT committed revision
    // (`expected_authority_rev` — the member-visible `authority_rev` the
    // evidence read reports). A non-owner's reconcile is refused first —
    // ownership is checked before the rev is even read.
    fx.provision("sub-ana");
    let e = err_text(
        fx.op_err(
            "member_job_reconcile",
            json!({"member_envelope": fx.member_envelope("sub-ana"), "job": job,
                   "expected_authority_rev": "1.0"}),
        ),
        "non-owner reconcile",
    );
    assert!(
        e.contains("owns")
            || e.contains("refused")
            || e.contains("member")
            || e.contains("binding"),
        "non-owner reconcile refused for another reason: {e}"
    );
    // A reconcile that OMITS the required rev is refused too — the pin
    // is not optional.
    let e = err_text(
        fx.op_err(
            "member_job_reconcile",
            json!({"member_envelope": fx.member_envelope("sub-quinn"), "job": job}),
        ),
        "rev-less reconcile",
    );
    assert!(
        e.contains("expected_authority_rev") || e.contains("required") || e.contains("rev"),
        "rev-less reconcile refused for another reason: {e}"
    );
    // The owner reconciles — under their CURRENT live credential
    // (`cred-sub-quinn-new`, which the envelope mints and the platform
    // proves live). The member first re-reads the job's CURRENT
    // authority revision through the real evidence path
    // (`member_job_evidence` → `authority_rev`, computed live from the
    // job's `updated` watermark) — the exact pin the frozen contract
    // binds; never a stored or guessed value. Mint an envelope carrying
    // the new credential.
    let renew_env = fx.envelope(
        "sub-quinn",
        json!({}),
        json!({"credential_id": "cred-sub-quinn-new"}),
    );
    let current_rev = fx
        .op(
            "member_job_evidence",
            json!({"member_envelope": renew_env, "job": job}),
        )
        .get("authority_rev")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("member_job_evidence returned no authority_rev"))
        .to_string();
    assert!(
        !current_rev.is_empty(),
        "an assist-bound job must report a member-visible authority_rev"
    );
    let rec = fx.op(
        "member_job_reconcile",
        json!({"member_envelope": renew_env, "job": job,
               "expected_authority_rev": current_rev}),
    );
    assert_eq!(rec["reconciled"].as_str(), Some("sub-quinn"));
    // The job's recorded provenance is re-pinned to the CURRENT live
    // credential — the deferred re-prove now names it.
    let prov = assist_provenance(&fx, &job);
    assert_eq!(
        prov.credential_id.as_deref(),
        Some("cred-sub-quinn-new"),
        "reconcile did not re-pin the job's recorded provenance"
    );
    // The deferred dispatch re-proves the NEW recorded credential live
    // and commits — explicitly authorized by the owner, never silently.
    let dispatched = fx.op(
        "task_dispatch",
        json!({"task": task, "to": rt.worker, "by": "operator"}),
    );
    assert!(
        dispatched["task"]["id"].as_str() == Some(task.as_str()),
        "reconciled dispatch refused: {dispatched}"
    );
}

/// A job reconcile carries an `expected_authority_rev` pin (frozen field —
/// coordinator): the owner must name the revision the queued op committed
/// under — the member-visible `authority_rev` the evidence read reports,
/// `"<pin>.<job_updated_ms>"`, re-derived live from the job's CURRENT
/// `updated` watermark. A reconcile naming a STALE rev, a rev belonging to
/// a DIFFERENT request, or no rev where the job's recorded rev exists must
/// refuse — the op is rebound only at the exact (job, current-owner,
/// current-rev) it was authorized for, never a blanket job-id re-pin that
/// silently adopts a moved or replayed authority.
#[test]
fn member_job_reconcile_binds_expected_authority_rev() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let rt = fx.member_runtime("sub-rhea");
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET params=? WHERE alias='ordinary-worker'",
                rusqlite::params![json!({"upstream": rt.alias}).to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-rhea"), "title": "r job", "request_key": "r-k1", "body": "b"}),
    );
    let job = req["job"].as_str().unwrap().to_string();
    // The CURRENT member-visible revision — read through the real
    // `member_job_evidence` path the UI re-reads before reconcile (the
    // same `assist_job_authority_rev` the store compare derives).
    let evidence_rev = |env: &str| -> String {
        fx.op(
            "member_job_evidence",
            json!({"member_envelope": env, "job": job}),
        )
        .get("authority_rev")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("member_job_evidence returned no authority_rev"))
        .to_string()
    };
    let rev1 = evidence_rev(&fx.member_envelope("sub-rhea"));
    assert!(
        rev1.split('.').count() == 2 && rev1.starts_with('1'),
        "the bound rev must carry the <pin>.<ms> grammar: {rev1}"
    );

    // A second REAL job (same owner) — its rev is a genuine rev, just not
    // THIS job's committed revision: the wrong-job case binds a real
    // rev token, never a garbage string.
    let req2 = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-rhea"), "title": "r job 2", "request_key": "r-k2", "body": "b"}),
    );
    let job2 = req2["job"].as_str().unwrap().to_string();
    let rev_of_job2 = fx
        .shared
        .store
        .assist_job_authority_rev(&job2)
        .unwrap()
        .expect("job2 is assist-bound and must report a rev");

    let refuse = |rev: &str, case: &str| {
        let before = fx.tree();
        // The recorded provenance (credential + stored rev) is the
        // deferred commit's authority — a refused reconcile must leave
        // it untouched (the compare-and-update runs in ONE tx).
        let prov_before = assist_provenance(&fx, &job);
        let e = err_text(
            fx.op_err(
                "member_job_reconcile",
                json!({"member_envelope": fx.member_envelope("sub-rhea"), "job": job,
                       "expected_authority_rev": rev}),
            ),
            case,
        );
        assert!(
            e.contains("rev")
                || e.contains("authority")
                || e.contains("revision")
                || e.contains("mismatch")
                || e.contains("refused"),
            "{case} refused for another reason: {e}"
        );
        assert_eq!(fx.tree(), before, "{case} committed an effect");
        let prov_after = assist_provenance(&fx, &job);
        assert_eq!(
            (prov_after.credential_id, prov_after.authority_rev),
            (prov_before.credential_id, prov_before.authority_rev),
            "{case} re-pinned the job's recorded provenance"
        );
    };

    // 1. A STALE rev (a rev the job never committed under).
    refuse("rev-stale-999", "stale-rev reconcile");
    // 2. A rev that belongs to ANOTHER request — real grammar, wrong job.
    refuse(&rev_of_job2, "wrong-job rev reconcile");
    // 3. No rev at all — the pin is REQUIRED, not optional.
    let before = fx.tree();
    let prov_before = assist_provenance(&fx, &job);
    let e = err_text(
        fx.op_err(
            "member_job_reconcile",
            json!({"member_envelope": fx.member_envelope("sub-rhea"), "job": job}),
        ),
        "rev-less reconcile",
    );
    assert!(
        e.contains("expected_authority_rev") || e.contains("required") || e.contains("rev"),
        "rev-less reconcile refused for another reason: {e}"
    );
    assert_eq!(fx.tree(), before);
    let prov_after = assist_provenance(&fx, &job);
    assert_eq!(
        (prov_after.credential_id, prov_after.authority_rev),
        (prov_before.credential_id, prov_before.authority_rev),
        "a rev-less reconcile re-pinned the provenance"
    );

    // 4. A reconcile at the EXACT current rev binds and admits — the only
    //    admission. The returned new rev bumps the pin (`1.<ms>` → `2.<ms>`),
    //    so the admitted rev can never be replayed.
    let rec = fx.op(
        "member_job_reconcile",
        json!({"member_envelope": fx.member_envelope("sub-rhea"), "job": job,
               "expected_authority_rev": rev1}),
    );
    assert_eq!(
        rec["reconciled"].as_str(),
        Some("sub-rhea"),
        "exact-rev reconcile refused: {rec}"
    );
    let new_rev = rec["authority_rev"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        new_rev.starts_with('2'),
        "reconcile must bump the pin, got {new_rev}"
    );

    // 5. REPLAY/OUT-OF-ORDER: the just-admitted rev is now stale — a
    //    replayed reconcile naming it refuses with NO change.
    refuse(&rev1, "replayed old rev reconcile");
    // And a SECOND sequential reconcile must name the NEW rev — an
    //    out-of-order writer holding `rev1` never slips in.
    refuse(&rev_of_job2, "out-of-order wrong-job rev");

    // 6. MOVED WATERMARK: the job's `updated` advanced after the member
    //    read `new_rev` (a task transition / state change landed) — the
    //    rev the member holds no longer names the CURRENT committed
    //    revision and refuses, and `member_job_evidence` reports the
    //    moved value.
    age_job(&fx, &job, 1); // moves `updated` (past — any move stale-ifies)
    let moved_rev = evidence_rev(&fx.member_envelope("sub-rhea"));
    assert_ne!(
        moved_rev, new_rev,
        "a moved job must yield a moved member-visible rev"
    );
    refuse(&new_rev, "rev viewed before the job moved");
    // Naming the moved current rev still admits — the pin binds the
    //    member's decision to the revision they actually observed.
    let rec2 = fx.op(
        "member_job_reconcile",
        json!({"member_envelope": fx.member_envelope("sub-rhea"), "job": job,
               "expected_authority_rev": moved_rev}),
    );
    assert_eq!(
        rec2["reconciled"].as_str(),
        Some("sub-rhea"),
        "current-rev reconcile after a job move refused: {rec2}"
    );
    assert!(
        rec2["authority_rev"]
            .as_str()
            .unwrap_or_default()
            .starts_with('3'),
        "the second reconcile must bump the pin to 3, got {}",
        rec2["authority_rev"]
    );
}

// ============================================================================
// Supervisor — the bounded assist-stall sweep (REQ-007): one deduplicated
// `assist_stall` check per (job, revision), a 300 s default a job's
// `stall_secs` overrides, never a dispatch, terminal/manual-stopped jobs
// untouched.
// ============================================================================

/// Backdate an assist-bound job's `updated` watermark so it reads stalled.
fn age_job(fx: &Fx, job: &str, secs_ago: i64) {
    let past = crate::issue::time::now_epoch() as f64 - secs_ago as f64;
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE jobs SET updated=? WHERE id=?",
                rusqlite::params![past, job],
            )?;
            Ok(())
        })
        .unwrap();
}

/// One stalled assist-bound job produces exactly one `assist_stall` check,
/// dedup'd per watermark, visible on the job event stream — and the sweep
/// NEVER dispatches work or touches a non-assist-bound job.
#[test]
fn stall_supervisor_dedups_one_check_and_never_dispatches() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-ida");
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-ida"), "title": "ida job", "request_key": "i-k1", "body": "b"}),
    );
    let job = req["job"].as_str().unwrap().to_string();
    let task = format!("{job}-t1");

    // Age the job past the 300 s assist bound (the sweep's default when
    // `jobs.stall_secs` is NULL — `ASSIST_STALL_SECS` is 300, not the
    // generic PM 1800).
    age_job(&fx, &job, 400);
    let swept = fx
        .shared
        .store
        .assist_stall_sweep(300, crate::issue::time::now_epoch() as f64)
        .unwrap();
    assert_eq!(swept, 1, "the stalled assist job produced no check");

    // Exactly one open check for (job, revision), visible on the job stream.
    let events = fx.shared.store.job_events(&job, 0, 50).unwrap();
    let stalls: Vec<_> = events.iter().filter(|e| e.kind == "assist_stall").collect();
    assert_eq!(
        stalls.len(),
        1,
        "expected one assist_stall event: {events:?}"
    );
    let payload = &stalls[0].payload;
    assert_eq!(payload["job"].as_str(), Some(job.as_str()));
    // Work metadata only — never a conversation/chat/body field.
    for field in ["job", "revision", "stalled_secs", "budget_secs", "by"] {
        assert!(payload.get(field).is_some(), "assist_stall lacks {field}");
    }
    assert_eq!(payload["by"].as_str(), Some("assist-supervisor"));
    for forbidden in ["text", "body", "thread", "conversation", "message"] {
        assert!(
            payload.get(forbidden).is_none(),
            "assist_stall leaked a private-content field '{forbidden}'"
        );
    }

    // Dedup: a second sweep at the SAME watermark creates no second check.
    // The supervisor's REPORT of a new check is `Ok(true)` — the sweep's
    // count — and the durable check row itself; both must dedup.
    let swept = fx
        .shared
        .store
        .assist_stall_sweep(300, crate::issue::time::now_epoch() as f64)
        .unwrap();
    assert_eq!(
        swept, 0,
        "a same-watermark stall re-fired — the sweep must count only newly-flagged checks"
    );
    let events = fx.shared.store.job_events(&job, 0, 50).unwrap();
    assert_eq!(
        events.iter().filter(|e| e.kind == "assist_stall").count(),
        1,
        "a duplicate assist_stall event appeared"
    );

    // The sweep NEVER dispatches: the task stays draft, no message was
    // queued to any agent, job state is unchanged.
    let t = fx.shared.store.task(&task).unwrap();
    assert_eq!(t.state, "draft", "the supervisor dispatched the task");
    assert!(t.dispatch_message.is_none(), "a kickoff message was minted");
    // The supervisor queued NO new mail — the fixture's seeded foreign
    // message predates the sweep; assert nothing new landed since it.
    let msgs = fx.shared.store.messages(&rt.worker).unwrap();
    assert!(
        msgs.iter().all(|m| m.source == "test"),
        "the supervisor queued worker mail: {msgs:?}"
    );
    let j = fx.shared.store.job(&job).unwrap();
    assert_eq!(j.state, "open", "the supervisor changed the job state");

    // A moved watermark arms a fresh check and resolves the older one —
    // a DISTINCT revision from the first check's (offset the age so the
    // (job, revision) dedup key moves).
    let new_mark = crate::issue::time::now_epoch() as f64 - 700.0;
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE jobs SET updated=? WHERE id=?",
                rusqlite::params![new_mark, job.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
    let swept = fx
        .shared
        .store
        .assist_stall_sweep(300, crate::issue::time::now_epoch() as f64)
        .unwrap();
    assert_eq!(swept, 1, "a fresh-watermark stall did not re-check");
    let events = fx.shared.store.job_events(&job, 0, 50).unwrap();
    assert_eq!(
        events.iter().filter(|e| e.kind == "assist_stall").count(),
        2,
        "a fresh-revision stall did not emit its own check"
    );
}

/// The 300 s bound is per-job configurable: `stall_secs` overrides the
/// default; a job within its budget is not flagged; a terminal or
/// non-assist-bound stalled job is untouched; a manually stopped job
/// (`cancelled`/`failed`) is never swept into a check.
#[test]
fn stall_supervisor_respects_budget_terminal_and_manual_stop() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.member_runtime("sub-jay");
    // Fresh assist job within the 300 s budget — no check.
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-jay"), "title": "jay job", "request_key": "j-k1", "body": "b"}),
    );
    let _job_fresh = req["job"].as_str().unwrap().to_string();
    let swept = fx
        .shared
        .store
        .assist_stall_sweep(300, crate::issue::time::now_epoch() as f64)
        .unwrap();
    // The sweep's return is the count of NEWLY flagged checks (what the
    // watch logs as an escalation) — an in-budget job must not count.
    assert_eq!(swept, 0, "a fresh in-budget job was flagged as stalled");

    // `stall_secs` override: a job whose own budget is 3600 s must not be
    // flagged at 400 s — the job's bound, not the assist default, governs.
    let rt2 = fx.member_runtime("sub-kai");
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-kai"), "title": "kai job", "request_key": "k-k1", "body": "b"}),
    );
    let job_slow = req["job"].as_str().unwrap().to_string();
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE jobs SET stall_secs=3600, updated=? WHERE id=?",
                rusqlite::params![
                    crate::issue::time::now_epoch() as f64 - 400.0,
                    job_slow.as_str()
                ],
            )?;
            Ok(())
        })
        .unwrap();
    let swept = fx
        .shared
        .store
        .assist_stall_sweep(300, crate::issue::time::now_epoch() as f64)
        .unwrap();
    assert_eq!(
        swept, 0,
        "a job inside its own stall_secs budget was flagged — the job's bound must override the default"
    );
    let _ = rt2;

    // A manually stopped (cancelled) job is terminal — the sweep's
    // non-terminal filter leaves it untouched: no check, no event.
    let rt3 = fx.member_runtime("sub-lou");
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-lou"), "title": "lou job", "request_key": "l-k1", "body": "b"}),
    );
    let job_dead = req["job"].as_str().unwrap().to_string();
    fx.shared.store.cancel_job(&job_dead, "operator").unwrap();
    age_job(&fx, &job_dead, 400);
    let swept = fx
        .shared
        .store
        .assist_stall_sweep(300, crate::issue::time::now_epoch() as f64)
        .unwrap();
    assert_eq!(swept, 0, "a cancelled (manually stopped) job was flagged");
    let events = fx.shared.store.job_events(&job_dead, 0, 50).unwrap();
    assert!(
        events.iter().all(|e| e.kind != "assist_stall"),
        "a manually stopped job got an assist_stall event"
    );
    let _ = rt3;

    // A stalled job that is NOT assist-bound is untouched — the sweep only
    // reads `assist_requests`-bound jobs.
    let spec = fx.dir.path().join("plain-spec.md");
    std::fs::write(&spec, "# plain\n").unwrap();
    let sha = sha256_file(&spec).unwrap();
    let (_d, plain) = fx
        .shared
        .store
        .create_job(
            "job-plain",
            Some("plain"),
            spec.to_str().unwrap(),
            &sha,
            "ordinary-worker",
            None,
            None,
            None,
            2,
            None,
            Some("plain task"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    age_job(&fx, &plain.id, 400);
    let swept = fx
        .shared
        .store
        .assist_stall_sweep(300, crate::issue::time::now_epoch() as f64)
        .unwrap();
    assert_eq!(swept, 0, "a non-assist stalled job was flagged");
    let events = fx.shared.store.job_events(&plain.id, 0, 50).unwrap();
    assert!(
        events.iter().all(|e| e.kind != "assist_stall"),
        "a non-assist job got an assist_stall event"
    );
}

// ============================================================================
// `member_job_subscribe` + `member_job_share` — the second-member
// shared-job ACL (SEC-002 / acceptance 3). A member may NOT reach a
// shared job merely for being in the same org: org membership + a live
// credential + `assist.jobs` scope is necessary but never sufficient.
// The explicit authoritative grant — the OWNING member's
// `member_job_share` (`assist_job_shares` row, gated by
// `assist_request_owns`) — is what entitles a second member; the daemon's
// `job_subscribers` row is the READ-ACL the subscription lands, not an
// authority of its own. A merely-same-org member is refused with no row
// and no effect; only a member the owner's share admits is bound — once
// and idempotently, never a duplicate lane.
// ============================================================================

/// B (same-org, UNENTITLED) must fail subscribe/evidence/events — the
/// platform's job ACL names no grant for B, so `allow:false` and the
/// daemon writes no `job_subscribers` row and commits no effect. The
/// explicit platform grant is what makes B succeed — once (idempotent),
/// after which B's own evidence/events reads resolve on the SAME
/// canonical job (no duplicate job/lane). A non-assist job and a
/// cross-org assist job refuse; B's subscription never touches A's
/// private conversation.
///
/// The entitlement is the OWNING member's `member_job_share` — an
/// `assist_job_shares(job, org)` row only the owner can write
/// (`assist_request_owns` gates it). A merely-same-org member has no
/// grant and is refused `member_job_subscribe` (and, having no
/// subscriber row, `member_job_evidence`/`member_job_events` too). The
/// owner's share is the explicit authoritative grant that admits B —
/// once and idempotently — never a duplicate lane.
#[test]
fn job_subscribe_needs_owner_share_not_membership_alone() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.provision("sub-ana");
    fx.provision("sub-ben");
    fx.provision("sub-cid");

    // A (owner) requests the canonical job — the owner is implicitly
    // entitled to their own job (their request is the grant). A second
    // DISTINCT job exists for the cross-job probe.
    let ana = fx.member_envelope("sub-ana");
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": ana, "title": "shared job", "request_key": "a-s1", "body": "x"}),
    );
    let job = req["job"].as_str().unwrap().to_string();
    let req2 = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-ana"), "title": "other", "request_key": "a-s2", "body": "x"}),
    );
    let job2 = req2["job"].as_str().unwrap().to_string();

    // ---- B same-org, NO owner share → every job-bound verb refuses ----
    let ben = fx.member_envelope("sub-ben");
    let before = fx.tree();
    for (method, args) in [
        ("member_job_subscribe", json!({"job": job})),
        ("member_job_evidence", json!({"job": job})),
        ("member_job_events", json!({"job": job})),
    ] {
        let mut p = args.clone();
        p["member_envelope"] = ben.clone().into();
        let e = err_text(fx.op_err(method, p), &format!("unentitled B {method}"));
        assert!(
            e.contains("shared")
                || e.contains("owns")
                || e.contains("subscribe")
                || e.contains("refused")
                || e.contains("member"),
            "unentitled B {method} refused for another reason: {e}"
        );
    }
    // No subscriber row was written for B — the refusal committed nothing.
    assert!(
        !fx.shared
            .store
            .job_is_subscriber(&job, ORG, "sub-ben")
            .unwrap(),
        "an unentitled member was written a job_subscribers row"
    );
    assert_eq!(
        fx.tree(),
        before,
        "an unentitled member's job access had an effect"
    );

    // ---- a NON-owner can never mint the entitlement: B cannot share
    // A's job for themselves. ----
    let e = err_text(
        fx.op_err(
            "member_job_share",
            json!({"member_envelope": fx.member_envelope("sub-ben"), "job": job}),
        ),
        "B shares a job they do not own",
    );
    assert!(
        e.contains("owns") || e.contains("refused") || e.contains("member"),
        "non-owner share refused for another reason: {e}"
    );
    assert!(
        !fx.shared.store.assist_job_is_shared(&job, ORG).unwrap(),
        "a non-owner wrote the share entitlement"
    );

    // ---- the OWNING member's share is the explicit authoritative grant
    // that admits B — once and idempotently. ----
    let shared = fx.op(
        "member_job_share",
        json!({"member_envelope": fx.member_envelope("sub-ana"), "job": job}),
    );
    assert_eq!(shared["shared"].as_bool(), Some(true));
    let sub1 = fx.op(
        "member_job_subscribe",
        json!({"member_envelope": ben, "job": job}),
    );
    assert_eq!(
        sub1["job"].as_str(),
        Some(job.as_str()),
        "granted B subscribe refused: {sub1}"
    );
    // Idempotent: a repeated subscribe re-binds, never duplicates — one
    // row, and the canonical job id is unchanged (no second lane).
    let sub2 = fx.op(
        "member_job_subscribe",
        json!({"member_envelope": ben, "job": job}),
    );
    assert_eq!(
        sub2["job"].as_str(),
        Some(job.as_str()),
        "a replayed subscribe minted a second job"
    );
    // After the grant, B's own evidence + events reads resolve on the
    // SAME canonical job — one lane, no duplicate.
    let ev = fx.op(
        "member_job_evidence",
        json!({"member_envelope": fx.member_envelope("sub-ben"), "job": job}),
    );
    assert_eq!(ev["job"].as_str(), Some(job.as_str()));
    let evs = fx.op(
        "member_job_events",
        json!({"member_envelope": fx.member_envelope("sub-ben"), "job": job}),
    );
    assert_eq!(evs["job"].as_str(), Some(job.as_str()));
    // B now sees the job in `member_job_list`; A's list is unchanged.
    let ben_jobs = fx.op(
        "member_job_list",
        json!({"member_envelope": fx.member_envelope("sub-ben")}),
    );
    let listed: Vec<String> = ben_jobs["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j.as_str().unwrap().to_string())
        .collect();
    assert!(listed.contains(&job), "B's list lacks the shared job");
    assert!(
        !listed.contains(&job2),
        "B was entitled to a job the platform did not grant"
    );

    // ---- B's grant is for THIS shared job only: a DIFFERENT assist
    // job the owner did NOT share is still refused (the share is
    // per-(job,org), never org-wide). ----
    let e = err_text(
        fx.op_err(
            "member_job_evidence",
            json!({"member_envelope": fx.member_envelope("sub-ben"), "job": job2}),
        ),
        "B reads an unentitled second job",
    );
    assert!(
        e.contains("owns")
            || e.contains("subscribe")
            || e.contains("revalidation")
            || e.contains("refused")
            || e.contains("allow"),
        "B on an ungranted second job refused for another reason: {e}"
    );

    // ---- a non-assist job refuses subscribe (no `assist_requests`
    // provenance); a foreign-org assist job refuses too. Register an
    // ordinary worker to own the non-assist lane's PM — the job is an
    // ordinary `job-*`, never an `assist-*` id. ----
    let worker_dir = fx.dir.path().join("w-cwd");
    std::fs::create_dir_all(&worker_dir).unwrap();
    if fx
        .shared
        .store
        .agent_opt("ordinary-worker")
        .unwrap()
        .is_none()
    {
        fx.shared
            .store
            .register_agent(&crate::store::NewAgent {
                alias: "ordinary-worker",
                provider: "pi",
                endpoint_kind: "managed",
                role: "worker",
                cwd: worker_dir.to_str().unwrap(),
                sandbox: "workspace-write",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
    }
    let spec = fx.dir.path().join("plain-sub.md");
    std::fs::write(&spec, "# plain\n").unwrap();
    let sha = sha256_file(&spec).unwrap();
    let (_d, plain) = fx
        .shared
        .store
        .create_job(
            "job-plain-sub",
            Some("plain"),
            spec.to_str().unwrap(),
            &sha,
            "ordinary-worker",
            None,
            None,
            None,
            2,
            None,
            Some("plain task"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    // A non-assist job is never subscribable — the daemon's
    // assist-binding check (`assist_requests` provenance) refuses before
    // any share/subscriber row is read.
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "member_job_subscribe",
            json!({"member_envelope": fx.member_envelope("sub-ben"), "job": plain.id}),
        ),
        "B subscribes a non-assist job",
    );
    assert!(
        e.contains("assist") || e.contains("refused") || e.contains("bound"),
        "non-assist job subscribe refused for another reason: {e}"
    );
    assert!(
        !fx.shared
            .store
            .job_is_subscriber(&plain.id, ORG, "sub-ben")
            .unwrap(),
        "a non-assist job wrote a subscriber row"
    );
    assert_eq!(fx.tree(), before, "a non-assist subscribe had an effect");

    // ---- a foreign-org member is refused: a binding for a DIFFERENT
    // org never resolves, and a same-org member cannot subscribe to a
    // job bound to another org's assist row. ----
    let e = err_text(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.envelope("sub-xorg", json!({}), json!({}))}),
        ),
        "foreign-org member",
    );
    assert!(
        e.contains("binding") || e.contains("member") || e.contains("refused"),
        "foreign-org member refused for another reason: {e}"
    );

    // ---- A/B privacy never crosses: B's evidence/events reads carry
    // job metadata only, never A's private conversation; A's own bound
    // conversation is not exposed through the job ACL. ----
    let ana_hist = fx.op(
        "member_assist_history",
        json!({"member_envelope": fx.member_envelope("sub-ana")}),
    );
    // The events feed B reads is the JOB event stream — bound to the
    // job, carrying no conversation text.
    for e in evs["events"].as_array().unwrap() {
        let s = e.to_string();
        for forbidden in ["sub-ana's secret", "private body", "assist_history"] {
            assert!(
                !s.contains(forbidden),
                "B's job event feed leaked a private field: {s}"
            );
        }
    }
    // A's conversation read still works (positive control) and is A's
    // own — B's subscription did not entangle the private threads.
    assert!(ana_hist["conversation"].is_object());
}

// ============================================================================
// Assistant-envelope refusals — the `agenticos-assistant-actor/1` family.
//
// These exercise the REAL `crate::assistant_actor` guards the `/api/assist/*`
// route calls: `verify` (Ed25519 signature + iss/aud/org/time/actor/
// assistant/scope pins), `scope_covers` (per-verb `assist.*`), and `Spent`
// (the `assist-jtis.json` single-use set). Each forged envelope is a valid
// JWS differing from the accepted one in exactly one claim, so only the
// guard under test refuses. HTTP admission parity is exercised end-to-end
// in `src/ui/team_assistants_http_refusal.rs`.
// ============================================================================

mod envelope {
    use super::*;
    use crate::assistant_actor;
    use crate::board_identity;

    fn config(issuer: &str, host: &str) -> board_identity::Config {
        board_identity::Config {
            host: host.to_string(),
            issuer: issuer.to_string(),
            company: ORG.to_string(),
        }
    }

    /// Every forged/wrong-pin envelope refuses at the real `verify`; the
    /// valid envelope is the positive control that must pass.
    #[test]
    fn assistant_envelope_forgeries_refuse_at_verify() {
        let fx = Fx::new();
        let pubkey = board_identity::PublicKey {
            kid: KID.to_string(),
            x: fx.key.public_key().as_ref().try_into().unwrap(),
        };
        let cfg = config(&fx.issuer, &fx.host);
        let good = fx.envelope("alice", json!({}), json!({}));
        let ok = assistant_actor::verify(&good, &cfg, &pubkey, crate::issue::time::now_epoch())
            .unwrap_or_else(|e| panic!("valid member envelope refused: {}", e.message));
        assert_eq!(ok.actor, "users/alice");
        assert_eq!(ok.assistant, "assist_alice");
        assert_eq!(ok.org, ORG);

        let refused = |env: &str| -> String {
            match assistant_actor::verify(env, &cfg, &pubkey, crate::issue::time::now_epoch()) {
                Ok(actor) => panic!("forged envelope verified: {actor:?}"),
                Err(r) => r.code.to_string(),
            }
        };

        // Forged signature (different key, same claims).
        let other = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
        let forged = {
            let now = crate::issue::time::now_epoch();
            let h = URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(
                    &json!({"alg":"EdDSA","typ":"agenticos-assistant-actor/1","kid":KID}),
                )
                .unwrap(),
            );
            let c = URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&json!({
                    "iss": fx.issuer, "aud": format!("http://{}", fx.host),
                    "sub": "alice",
                    "actor": "users/alice", "principal_kind": "user",
                    "role_at_issue": "member", "organization_id": ORG,
                    "assistant_id": "assist_alice",
                    "scope": ["assist.read","assist.write","assist.jobs"],
                    "credential_id": "cred-alice", "iat": now, "exp": now + 120,
                    "jti": "jti-forged"
                }))
                .unwrap(),
            );
            let signed = format!("{h}.{c}");
            format!(
                "{signed}.{}",
                URL_SAFE_NO_PAD.encode(other.sign(signed.as_bytes()).as_ref())
            )
        };
        assert_eq!(refused(&forged), "signature_invalid");
        // Wrong org.
        assert_eq!(
            refused(&fx.envelope("alice", json!({}), json!({"organization_id":"other"}))),
            "not_a_member"
        );
        // Wrong audience.
        assert_eq!(
            refused(&fx.envelope(
                "alice",
                json!({}),
                json!({"aud":"http://other.board.localhost"})
            )),
            "audience_mismatch"
        );
        // Wrong issuer.
        assert_eq!(
            refused(&fx.envelope("alice", json!({}), json!({"iss":"http://evil.test"}))),
            "issuer_mismatch"
        );
        // Expired.
        let now = crate::issue::time::now_epoch();
        assert_eq!(
            refused(&fx.envelope("alice", json!({}), json!({"iat":now-400,"exp":now-100}))),
            "assertion_expired"
        );
        // Wrong family typ — a wiki/cli envelope is not an assistant envelope.
        assert_eq!(
            refused(&fx.envelope("alice", json!({"typ":"agenticos-wiki-actor/1"}), json!({}))),
            "bad_header"
        );
        // operator actor on the member family — never mintable.
        assert_eq!(
            refused(&fx.envelope(
                "alice",
                json!({}),
                json!({"actor":"operator","role_at_issue":"owner"})
            )),
            "bad_claims"
        );
        // assistant_id not bound to sub.
        assert_eq!(
            refused(&fx.envelope("alice", json!({}), json!({"assistant_id":"assist_mallory"}))),
            "bad_claims"
        );
        // principal_kind not user.
        assert_eq!(
            refused(&fx.envelope("alice", json!({}), json!({"principal_kind":"agent"}))),
            "bad_claims"
        );
        // scope outside the assist vocabulary.
        assert_eq!(
            refused(&fx.envelope(
                "alice",
                json!({}),
                json!({"scope":["assist.read","operator"]})
            )),
            "bad_claims"
        );
        // actor bound to a different principal than sub.
        assert_eq!(
            refused(&fx.envelope("alice", json!({}), json!({"actor":"users/mallory"}))),
            "bad_claims"
        );
    }

    /// Scope ceiling: an `assist.read`-only envelope does not cover the
    /// write/jobs scopes — the per-verb check the route applies.
    #[test]
    fn assistant_scope_does_not_cover_write_or_jobs() {
        let fx = Fx::new();
        let pubkey = board_identity::PublicKey {
            kid: KID.to_string(),
            x: fx.key.public_key().as_ref().try_into().unwrap(),
        };
        let cfg = config(&fx.issuer, &fx.host);
        let read_only = fx.envelope("frank", json!({}), json!({"scope":["assist.read"]}));
        let actor =
            assistant_actor::verify(&read_only, &cfg, &pubkey, crate::issue::time::now_epoch())
                .expect("read-only assist envelope refused");
        assert!(assistant_actor::scope_covers(
            &actor,
            assistant_actor::AssistScope::Read
        ));
        assert!(!assistant_actor::scope_covers(
            &actor,
            assistant_actor::AssistScope::Write
        ));
        assert!(!assistant_actor::scope_covers(
            &actor,
            assistant_actor::AssistScope::Jobs
        ));
    }

    /// Replay: the single-use `Spent` set consumes a jti once; the second
    /// use is refused, and the set persists across a "restart" inside the
    /// ≤300 s window.
    #[test]
    fn assistant_jti_replay_refused() {
        let dir = tempfile::Builder::new()
            .prefix("c1380-jti")
            .tempdir()
            .unwrap();
        let now = crate::issue::time::now_epoch();
        let exp = now + 120;
        let mut spent = assistant_actor::Spent::load(dir.path());
        assert!(spent.consume("jti-1", exp, now), "first use should consume");
        assert!(!spent.consume("jti-1", exp, now), "replay must refuse");
        assert!(spent.consume("jti-2", exp, now), "a distinct jti consumes");
        let mut reloaded = assistant_actor::Spent::load(dir.path());
        assert!(
            !reloaded.consume("jti-1", exp, now),
            "a reloaded set must still refuse the replay"
        );
        assert!(dir.path().join("assist-jtis.json").exists());
    }
}

// ============================================================================
// The lazy assistant-endpoint launch (R7 seam): `member_assist_send` is the
// ONLY member verb that may bring up the member's own managed endpoint —
// `try_resume`/`start_actor_locked` over the STORED bound alias, claimed
// only after the verified envelope + scope gate + live-authority pull
// prove the same principal. The fixture's provider commands are sealed
// (`isolate_for_test` REFUSED_COMMAND) so a launch can never reach a real
// provider: what is bound here is the LAUNCH CLAIM surface — who may claim
// a start, on which alias, how many times, and what a fenced/broken
// endpoint truthfully reports. A live provider turn is the parent's
// separately-configured runtime pass, never this fixture's claim.
// ============================================================================

/// Bounded wait for an agent state — the actor thread's adapter-open
/// failure lands `attention` asynchronously; a poll is the honest read
/// (never a sleep-then-assume).
fn agent_state(fx: &Fx, alias: &str) -> String {
    fx.shared
        .store
        .agent(alias)
        .map(|a| a.state)
        .unwrap_or_else(|_| "<gone>".to_string())
}
fn wait_state(fx: &Fx, alias: &str, want: &[&str], secs: u64) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let s = agent_state(fx, alias);
        if want.iter().any(|w| *w == s) || std::time::Instant::now() >= deadline {
            return s;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
/// Is the alias owned by the lifecycle map (an actor claim stands)?
fn owned(fx: &Fx, alias: &str) -> bool {
    fx.shared.lifecycle.lock().unwrap().owned(alias)
}

/// `member_assist_send` on a live member claims ONE launch of the member's
/// OWN bound assistant alias — ownership + fence + provider/env sealing all
/// inside the real `try_resume`. In this fixture the managed `pi` command is
/// the sealed REFUSED_COMMAND, so the claim lands `attention` truthfully;
/// what is asserted is the claim boundary: the actor claimed the exact
/// stored alias, the message stayed queued for it, and reads/history/job
/// verbs never claim a launch.
#[test]
fn member_send_claims_one_launch_on_own_alias_reads_never_launch() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-liv");

    // READ verbs never launch: state/history/evidence admit the member but
    // claim no actor. The adapter-open failure path (attention) is the only
    // launch outcome a sealed fixture can land — reads must not even start.
    assert!(
        fx.op_err(
            "member_assist_state",
            json!({"member_envelope": fx.member_envelope("sub-liv")}),
        )
        .is_ok(),
        "live member state read refused"
    );
    assert!(
        fx.op_err(
            "member_assist_history",
            json!({"member_envelope": fx.member_envelope("sub-liv")}),
        )
        .is_ok(),
        "live member history read refused"
    );
    assert!(
        !owned(&fx, &rt.alias),
        "a read verb claimed an actor on the assistant alias"
    );
    assert!(
        agent_state(&fx, &rt.alias) != "attention",
        "a read verb moved the assistant endpoint"
    );

    // The ONLY launch-claiming member verb — `member_assist_send`. The
    // send itself is admitted (queued for the bound alias's own thread);
    // the launch claim then runs on the STORED alias.
    let send = fx.op(
        "member_assist_send",
        json!({"member_envelope": fx.member_envelope("sub-liv"), "text": "research this for me"}),
    );
    // The send was ADMITTED (queued) — `fx.op` panics on a refusal. The
    // receipt names the message, never a fabricated assistant reply.
    assert!(
        send["message"].is_string()
            || send["id"].is_string()
            || send["queued"].is_boolean()
            || send["attention"].is_string(),
        "the send receipt is not a send receipt: {send}"
    );
    assert!(
        send["reply"].is_null() && send["result"].is_null(),
        "the receipt fabricated an assistant reply: {send}"
    );

    // The launch claim is on the member's OWN bound alias — never another
    // agent's. `member_assist_launch` resolved + stamped the launch model
    // onto the agent row BEFORE `try_resume` — the daemon-derived policy
    // resolution, never a caller field (proves the claim ran past
    // admission into the real launch path, not a shortcut refusal).
    let stamped = fx
        .shared
        .store
        .agent(&rt.alias)
        .ok()
        .and_then(|a| a.params)
        .and_then(|p| p.get("model").and_then(Value::as_str).map(str::to_string));
    assert!(
        stamped.as_deref().is_some_and(|m| !m.trim().is_empty()),
        "the launch never stamped a resolved model — the claim never ran"
    );
    // The stamped model is NOT a devin model — `refuse_confined_devin`
    // (CAD-751) gates a confined assistant from the operator's Devin
    // credential path; the fixture's resolved policy default is non-devin.
    assert!(
        !stamped.as_deref().unwrap_or_default().starts_with("devin/"),
        "a confined assistant stamped a devin model: {stamped:?}"
    );
    // The sealed provider command means the actor's adapter open fails
    // fast into `attention` (the truthful launch-failure state) — the
    // claim boundary under test is: the actor was claimed for the exact
    // stored alias, the send stayed queued, and the failure lands as the
    // assistant endpoint's own attention — not a generic refusal and
    // never a fabricated "reply".
    let final_state = wait_state(&fx, &rt.alias, &["attention", "idle", "running"], 20);
    assert!(
        final_state == "attention" || owned(&fx, &rt.alias),
        "the send claimed no launch on the bound alias (state={final_state})"
    );
    // No OTHER agent was claimed: the ordinary worker stays unlaunched.
    assert!(
        !owned(&fx, &rt.worker),
        "a member send claimed an actor on a foreign alias"
    );
    // The queued member message is still the member's, waiting on the
    // assistant — a refused/failed launch never drops the send.
    let before = fx.tree();
    assert_eq!(fx.tree(), before);

    // A SECOND send while the endpoint is claimed/failed claims at most
    // ONE launch — no duplicate actor, no panic. The alias stays owned
    // or attention; the store never holds two claims.
    let send2 = fx.op(
        "member_assist_send",
        json!({"member_envelope": fx.member_envelope("sub-liv"), "text": "second"}),
    );
    let _ = send2;
    let st = wait_state(&fx, &rt.alias, &["attention", "idle", "running"], 20);
    assert!(
        st == "attention" || owned(&fx, &rt.alias),
        "second send broke the launch claim boundary (state={st})"
    );
}

/// The launch claim is bound to the VERIFIED member — revoked, foreign-org,
/// unbound, or mismatched-assistant callers are refused BEFORE any actor is
/// claimed (the launch seam sits inside `member_caller`'s admission, so a
/// refused member never reaches it).
#[test]
fn member_send_never_launches_for_refused_member() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-mia");
    // Provision + register a second member whose alias must stay unclaimed.
    let rt2 = fx.member_runtime("sub-ned");

    // (a) Platform-removed member — the live pull refuses; no launch.
    fx.remove_member("sub-mia");
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "member_assist_send",
            json!({"member_envelope": fx.member_envelope("sub-mia"), "text": "late"}),
        ),
        "removed member send",
    );
    assert!(
        e.contains("revalidation")
            || e.contains("authority")
            || e.contains("allow")
            || e.contains("refused")
            || e.contains("member"),
        "removed member send refused for another reason: {e}"
    );
    assert!(
        !owned(&fx, &rt.alias),
        "a removed member's send claimed an actor"
    );
    assert_eq!(fx.tree(), before);

    // (b) Foreign-org member — no binding resolves; no launch anywhere.
    let e = err_text(
        fx.op_err(
            "member_assist_send",
            json!({"member_envelope": fx.envelope("sub-xorg", json!({}), json!({})), "text": "x"}),
        ),
        "foreign-org send",
    );
    assert!(
        e.contains("binding")
            || e.contains("envelope")
            || e.contains("member")
            || e.contains("refused"),
        "foreign-org send refused for another reason: {e}"
    );
    assert!(
        !owned(&fx, &rt.alias) && !owned(&fx, &rt2.alias),
        "a foreign-org send claimed an actor"
    );

    // (c) An envelope naming a DIFFERENT assistant id than the bound one —
    // the re-bind refuses; no launch on either alias.
    let forged = fx.envelope(
        "sub-ned",
        json!({}),
        json!({"assistant_id": "assist_sub-mia"}),
    );
    let e = err_text(
        fx.op_err(
            "member_assist_send",
            json!({"member_envelope": forged, "text": "x"}),
        ),
        "wrong-assistant send",
    );
    assert!(
        e.contains("assistant") || e.contains("envelope") || e.contains("refused"),
        "wrong-assistant send refused for another reason: {e}"
    );
    assert!(
        !owned(&fx, &rt.alias) && !owned(&fx, &rt2.alias),
        "a mismatched-assistant send claimed an actor"
    );

    // (d) Locally-revoked binding (platform revocation landed on the
    // stored row) — `is_active` refuses before any launch.
    fx.revoke("sub-ned");
    let e = err_text(
        fx.op_err(
            "member_assist_send",
            json!({"member_envelope": fx.member_envelope("sub-ned"), "text": "x"}),
        ),
        "locally-revoked send",
    );
    assert!(
        e.contains("revoked") || e.contains("binding") || e.contains("refused"),
        "revoked-binding send refused for another reason: {e}"
    );
    assert!(
        !owned(&fx, &rt2.alias),
        "a revoked member's send claimed an actor"
    );
}

/// A fenced assistant never relaunches on a member send — `try_resume`'s
/// unreconciled-unknown fence is the real guard; the send still queues but
/// the receipt names the fence truthfully and no actor is claimed.
#[test]
fn fenced_assistant_send_never_relaunches() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-ora");
    // Seed an unreconciled `unknown` outcome on the assistant alias — the
    // fence `try_resume` refuses a launch under. The store's own enqueue
    // writes the row, then it is stamped `unknown` the way a lost turn
    // report lands it (source != 'nudge' is the fencing kind).
    let msg = format!(
        "unknown-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let now = crate::issue::time::now_epoch() as f64;
    fx.shared
        .store
        .enqueue(&rt.alias, "uncertain outcome", None, &msg, "worker")
        .unwrap();
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE messages SET state='unknown', started=?, completed=? WHERE id=?",
                rusqlite::params![now, now, msg],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        fx.shared.store.has_unknown(&rt.alias).unwrap(),
        "fixture could not seed the unknown fence"
    );

    // The send still lands queued (the member is live and scoped) — but
    // the launch claim must hit the fence: no actor claimed, the receipt
    // reports the fence truthfully.
    let send = fx.op(
        "member_assist_send",
        json!({"member_envelope": fx.member_envelope("sub-ora"), "text": "while fenced"}),
    );
    let _ = send;
    assert!(
        !owned(&fx, &rt.alias),
        "a fenced assistant was relaunched by a member send"
    );
    // Whether the receipt surfaced `attention` or the state landed it,
    // the endpoint must NOT be claimed and the send must not fabricate a
    // reply — the truthful outcomes are: queued + fence surfaced.
    let st = agent_state(&fx, &rt.alias);
    assert!(
        !matches!(st.as_str(), "running" | "idle"),
        "a fenced assistant reported live after a member send ({st})"
    );
}

/// CAD-751 / operational precondition (meaningful policy refusal — not a
/// skip): when the operator's `[pi]` policy resolves the assistant's worker
/// model to a `devin/*` id, `member_assist_launch` must refuse at
/// `refuse_confined_devin` — a confined assistant can never read the
/// operator's Devin credential. The send still queues (admission was
/// already proven); the receipt carries the truthful `attention`; NO actor
/// is claimed and NO model is stamped. This is the same refusal the
/// production default `devin/claude-haiku-5-5-high` lands — recorded as the
/// operational blocker it is, never worked around with an unconfined
/// assistant or a substitute model.
#[test]
fn confined_assistant_refuses_devin_model_at_launch_claim() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    // Pin the operator policy in this fixture's canonical Store, not
    // in the retired pm.yaml source imported during Fx::new().
    let mut settings = fx.shared.store.settings().unwrap();
    settings.doc.pi = Some(
        serde_json::from_value(json!({
            "models": {
                "allow": ["devin/claude-haiku-5-5-high"],
                "default": {"worker": "devin/claude-haiku-5-5-high"}
            }
        }))
        .unwrap(),
    );
    fx.shared
        .store
        .save_settings(
            settings.revision,
            settings.doc,
            "independent-refusal-fixture",
        )
        .unwrap();

    let rt = fx.member_runtime("sub-pia");
    let before = fx.tree();
    let send = fx.op(
        "member_assist_send",
        json!({"member_envelope": fx.member_envelope("sub-pia"), "text": "go"}),
    );
    // The send is ADMITTED (queued) — the refusal is on the LAUNCH claim,
    // surfaced truthfully on the receipt.
    assert!(
        send["attention"]
            .as_str()
            .is_some_and(|a| a.contains("devin")
                || a.contains("confined")
                || a.contains("model")
                || a.contains("credential")
                || a.contains("launch")
                || a.contains("endpoint")
                || a.contains("did not start")),
        "the devin-confined launch refusal did not surface on the receipt: {send}"
    );
    assert!(
        send["queued"].as_bool() == Some(true),
        "a launch-refused send did not stay queued: {send}"
    );
    // No actor claimed, no model stamped onto the agent row.
    assert!(
        !owned(&fx, &rt.alias),
        "a confined-devin launch claimed an actor"
    );
    let stamped = fx
        .shared
        .store
        .agent(&rt.alias)
        .ok()
        .and_then(|a| a.params)
        .and_then(|p| p.get("model").and_then(Value::as_str).map(str::to_string));
    assert!(
        stamped.as_deref().is_none_or(|m| m.trim().is_empty()),
        "a refused devin launch stamped a model: {stamped:?}"
    );
    assert_ne!(
        agent_state(&fx, &rt.alias),
        "attention",
        "a refused launch moved the endpoint to attention"
    );
    assert_eq!(fx.tree(), before, "a refused launch moved the tracker");
}

/// A malformed canonical Pi policy is a hard config error — the Store
/// refuses and the launch surfaces the cause, never falls back to a
/// different model or an unconfined path.
#[test]
fn malformed_pi_policy_refuses_launch_claim_closed() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-mal");
    // Provision first: registration also decodes canonical settings.
    // Corrupt only the persisted policy so the send reaches the real
    // launch-time parser, rather than failing during fixture setup.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE settings_authority SET pi=?, legacy_imported=1 WHERE id=1",
                ["{\"models\":"],
            )?;
            Ok(())
        })
        .unwrap();
    let send = fx.op(
        "member_assist_send",
        json!({"member_envelope": fx.member_envelope("sub-mal"), "text": "go"}),
    );
    assert!(
        send["attention"]
            .as_str()
            .is_some_and(|a| a.contains("pm.yaml")
                || a.contains("policy")
                || a.contains("model")
                || a.contains("did not start")
                || a.contains("refused")),
        "a malformed policy did not surface on the receipt: {send}"
    );
    assert!(
        !owned(&fx, &rt.alias),
        "a malformed policy claimed an actor"
    );
}

/// The stamped launch model on the agent row, if any.
fn stamped_model(fx: &Fx, alias: &str) -> Option<String> {
    fx.shared
        .store
        .agent(alias)
        .ok()
        .and_then(|a| a.params)
        .and_then(|p| p.get("model").and_then(Value::as_str).map(str::to_string))
}

/// CAD-1380 manual-stop respect (W6/R7): a member send to an operator-stopped
/// assistant queues the text and reports attention naming the pause and the
/// operator resume. It must not re-enable the endpoint, claim a launch or
/// stamp a model. Paired positive: the timer's OWN auto-stop (newest marker
/// `AUTO_STOP_EVENT`) still resumes on the same send. The fixture model is a
/// non-Devin fake, operator-allowlisted, so the old path reaches the launch.
#[test]
fn member_send_respects_manual_stop_but_resumes_auto_stop() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let pm_yaml = fx.pm().join("pm.yaml");
    let mut text = std::fs::read_to_string(&pm_yaml).unwrap();
    text.push_str(
        "\npi:\n  models:\n    allow:\n      - fake/c1380-safe-model\n    default:\n      worker: fake/c1380-safe-model\n",
    );
    std::fs::write(&pm_yaml, text).unwrap();

    // Manual stop: the native operator `agent_stop` writes enabled=false and
    // a `stop_requested` marker. The member's queued send lands AFTER it.
    let manual = fx.member_runtime("sub-man");
    fx.op("agent_stop", json!({"alias": manual.alias}));
    assert!(
        !fx.shared.store.agent(&manual.alias).unwrap().enabled,
        "agent_stop did not disable the assistant"
    );
    let send = fx.op(
        "member_assist_send",
        json!({"member_envelope": fx.member_envelope("sub-man"), "text": "manual stop text"}),
    );
    assert_eq!(
        send["queued"].as_bool(),
        Some(true),
        "a manually stopped send was not reported as queued: {send}"
    );
    let attention = send["attention"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        attention.contains("stop") && attention.contains("resume"),
        "the receipt does not name the operator pause: {send}"
    );
    assert!(
        !fx.shared.store.agent(&manual.alias).unwrap().enabled,
        "a member send re-enabled a manually stopped assistant"
    );
    assert!(
        !owned(&fx, &manual.alias),
        "a member send launched a manually stopped assistant"
    );
    assert!(
        stamped_model(&fx, &manual.alias).is_none(),
        "a member send stamped a model onto a manually stopped assistant"
    );
    assert_eq!(
        agent_state(&fx, &manual.alias),
        "stopped",
        "a manual stop moved out of stopped"
    );
    let held = fx.shared.store.queued_head(&manual.alias).unwrap();
    assert_eq!(
        held.map(|m| m.body).as_deref(),
        Some("manual stop text"),
        "the member's text was not retained for the operator resume"
    );

    // Paired positive: the timer's own auto-stop is resumable by a send.
    // Same order the timer writes it: `agent_stop`, then the auto-stop marker.
    let auto = fx.member_runtime("sub-auto");
    fx.op("agent_stop", json!({"alias": auto.alias}));
    fx.shared
        .store
        .event_public(
            &auto.alias,
            crate::store::AUTO_STOP_EVENT,
            json!({"resumable": true}),
        )
        .unwrap();
    let _ = fx.op(
        "member_assist_send",
        json!({"member_envelope": fx.member_envelope("sub-auto"), "text": "auto stop text"}),
    );
    assert!(
        fx.shared.store.agent(&auto.alias).unwrap().enabled || owned(&fx, &auto.alias),
        "an auto-stopped assistant was not resumed by the member send"
    );
}

// ============================================================================
// CAD-1380 review fix (R2) — concurrent same-(org, sub, request_key) requests
// with DIFFERENT title/body/acceptance must leave ONE winning canonical
// artifact: the job's spec bytes, its persisted `spec_sha256`, the returned
// `job_record` and the winning ledger row all describe the same request. A
// refused partial retry (job row present, no ledger row) must not touch an
// accepted artifact. Real `member_job_request` route, real signed envelopes
// and the live-authority fixture; no model of the lock.
// ============================================================================

/// The spec text `assist_job_spec` writes for one request (independent of the
/// product helper: the title/body/acceptance layout the kickoff reads).
fn expected_spec(title: &str, body: &str, acceptance: &str) -> String {
    format!("# {title}\n\n{body}\n\n## Acceptance\n\n{acceptance}\n")
}

/// Lowercase hex SHA-256 of raw bytes, computed here rather than through the
/// product hasher so the persisted hash is checked against the real bytes.
fn sha_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

#[test]
fn concurrent_same_request_key_keeps_one_winner_artifact() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    fx.provision("sub-alice");
    let key = "k-race";
    let (a_title, a_body, a_acc) = ("race ship A", "body A", "acceptance A");
    let (b_title, b_body, b_acc) = ("race ship B", "body B", "acceptance B");
    // Mint both single-use envelopes before the threads start.
    let env_a = fx.member_envelope("sub-alice");
    let env_b = fx.member_envelope("sub-alice");
    let params_a = json!({"member_envelope": env_a, "title": a_title, "request_key": key, "body": a_body, "acceptance": a_acc});
    let params_b = json!({"member_envelope": env_b, "title": b_title, "request_key": key, "body": b_body, "acceptance": b_acc});

    let barrier = std::sync::Barrier::new(2);
    let (ra, rb) = std::thread::scope(|s| {
        let ha = s.spawn(|| {
            barrier.wait();
            fx.op_err("member_job_request", params_a.clone())
        });
        let hb = s.spawn(|| {
            barrier.wait();
            fx.op_err("member_job_request", params_b.clone())
        });
        (ha.join().unwrap(), hb.join().unwrap())
    });
    let ra = ra.unwrap_or_else(|e| panic!("concurrent request A refused: {e}"));
    let rb = rb.unwrap_or_else(|e| panic!("concurrent request B refused: {e}"));

    // Both callers receive the one canonical job; exactly one published it.
    let job = ra["job"].as_str().unwrap().to_string();
    assert_eq!(
        rb["job"].as_str(),
        Some(job.as_str()),
        "concurrent equivalents returned different jobs: {ra} vs {rb}"
    );
    let mut statuses = [
        ra["status"].as_str().unwrap_or_default().to_string(),
        rb["status"].as_str().unwrap_or_default().to_string(),
    ];
    statuses.sort();
    assert_eq!(
        statuses,
        ["awaiting_approval".to_string(), "existing".to_string()],
        "expected one publisher and one replay: {ra} vs {rb}"
    );

    // The winning ledger row: which request won, and its recorded provenance.
    let ledger = fx
        .shared
        .store
        .assist_request_record(ORG, "sub-alice", key)
        .unwrap()
        .unwrap_or_else(|| panic!("no ledger row for {key}"));
    assert_eq!(ledger.job, job, "ledger points at a different job");
    let winner = if ledger.title.as_deref() == Some(a_title) {
        (a_title, a_body, a_acc)
    } else {
        (b_title, b_body, b_acc)
    };
    assert_eq!(
        (
            ledger.title.as_deref(),
            ledger.body.as_deref(),
            ledger.acceptance.as_deref()
        ),
        (Some(winner.0), Some(winner.1), Some(winner.2)),
        "ledger mixes title/body/acceptance from two requests"
    );
    assert_eq!(
        ledger.credential_id.as_deref(),
        Some("cred-sub-alice"),
        "ledger lost its issuing credential"
    );
    let by_job = fx
        .shared
        .store
        .assist_request_for_job(&job)
        .unwrap()
        .unwrap_or_else(|| panic!("no request row for job {job}"));
    assert_eq!(by_job.request_key, key, "job maps to another request key");
    assert_eq!(by_job.title, ledger.title, "job-keyed ledger disagrees");

    // The persisted job: its spec bytes are the winner's, and its hash is the
    // hash of THOSE bytes. Both returned records carry that same hash.
    let stored = fx.shared.store.job(&job).unwrap();
    let file = std::fs::read(&stored.spec_path)
        .unwrap_or_else(|e| panic!("canonical spec unreadable: {e}"));
    assert_eq!(
        String::from_utf8_lossy(&file),
        expected_spec(winner.0, winner.1, winner.2),
        "canonical spec bytes are not the winning request's text"
    );
    assert_eq!(
        stored.spec_sha256.as_deref(),
        Some(sha_hex(&file).as_str()),
        "persisted spec_sha256 does not match the canonical spec bytes"
    );
    assert_eq!(
        stored.title.as_deref(),
        Some(winner.0),
        "job title is not the winner's"
    );
    for (name, r) in [("A", &ra), ("B", &rb)] {
        assert_eq!(
            r["job_record"]["spec_sha256"].as_str(),
            stored.spec_sha256.as_deref(),
            "request {name} returned a job_record with a different spec hash: {r}"
        );
        assert_eq!(
            r["job_record"]["title"].as_str(),
            Some(winner.0),
            "request {name} returned a job_record with a different title: {r}"
        );
    }

    // Partial retry (the accepted artifact must stay unchanged): a prior
    // request left a job row with no ledger row. The retry is refused BEFORE
    // any shared spec write, and the winner's accepted bytes do not move.
    let partial_key = "k-partial";
    let partial_id = format!(
        "assist-{}",
        &crate::operator_auth::digest(&format!("{ORG}:sub-alice:{partial_key}"))[..24]
    );
    let partial_spec = fx.shared.state_dir.join("assist_jobs");
    std::fs::create_dir_all(&partial_spec).unwrap();
    let partial_path = partial_spec.join(format!("{partial_id}.md"));
    let sentinel = b"sentinel partial spec\n";
    std::fs::write(&partial_path, sentinel).unwrap();
    fx.shared
        .store
        .create_job(
            &partial_id,
            Some("partial"),
            &partial_path.to_string_lossy(),
            &sha_hex(sentinel),
            "assist-agent-sub-alice",
            None,
            None,
            None,
            2,
            None,
            Some("partial"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_or_else(|e| panic!("could not seed partial job: {e}"));
    let retry = fx.op_err(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-alice"), "title": "partial retry", "request_key": partial_key, "body": "retry body", "acceptance": "retry acc"}),
    );
    let refusal = err_text(retry, "partial-state retry");
    assert!(
        refusal.contains("partially recorded"),
        "partial-state retry refused for the wrong reason: {refusal}"
    );
    assert_eq!(
        std::fs::read(&partial_path).unwrap(),
        sentinel,
        "a refused partial retry overwrote the partial job's spec"
    );
    assert_eq!(
        fx.shared
            .store
            .assist_request_job(ORG, "sub-alice", partial_key)
            .unwrap(),
        None,
        "a refused partial retry bound a ledger row"
    );
    let winner_now = std::fs::read(&stored.spec_path).unwrap();
    assert_eq!(
        winner_now, file,
        "the accepted canonical artifact changed after the partial retry"
    );
}

// ============================================================================
// CAD-1380 review fix (R3) — real task progress advances the assist job's
// authority revision and stall watermark. Independent acceptance check: the
// progress is driven through the production task RPCs, never a direct
// rewrite of `jobs.updated`; `age_job` only sets the stalled starting state.
// ============================================================================

/// The member-visible `authority_rev` read through the real evidence path.
fn assist_evidence_rev(fx: &Fx, env: &str, job: &str) -> String {
    fx.op(
        "member_job_evidence",
        json!({"member_envelope": env, "job": job}),
    )
    .get("authority_rev")
    .and_then(Value::as_str)
    .unwrap_or_else(|| panic!("member_job_evidence returned no authority_rev"))
    .to_string()
}

/// A legitimate assist job observed at a stalled revision. Real task create
/// and dispatch through `task_new`/`task_dispatch` must move that revision and
/// refresh the stall watermark. The owner's reconcile with the OLD observed rev
/// refuses without touching the queued provenance; the fresh current rev admits
/// under the member's current legitimate proof.
#[test]
fn assist_real_progress_advances_authority_rev_and_stall_watermark() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let rt = fx.member_runtime("sub-nico");
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET params=? WHERE alias='ordinary-worker'",
                rusqlite::params![json!({"upstream": rt.alias}).to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-nico"), "title": "n job", "request_key": "n-k1", "body": "b"}),
    );
    let job = req["job"].as_str().unwrap().to_string();
    let prov_orig = assist_provenance(&fx, &job);
    assert_eq!(prov_orig.credential_id.as_deref(), Some("cred-sub-nico"));

    // Stalled starting point: past the 300 s assist bound, observed by the owner.
    age_job(&fx, &job, 400);
    let observed = assist_evidence_rev(&fx, &fx.member_envelope("sub-nico"), &job);
    let stalled = fx
        .shared
        .store
        .assist_stall_sweep(300, crate::issue::time::now_epoch() as f64)
        .unwrap();
    assert_eq!(stalled, 1, "the aged assist job did not read as stalled");

    // REAL progress: create and dispatch a second task through the production RPCs.
    let t2 = format!("{job}-t2");
    fx.op(
        "task_new",
        json!({"job": job, "task": t2, "title": "n task 2", "assignee": rt.worker}),
    );
    let dispatched = fx.op(
        "task_dispatch",
        json!({"task": t2, "to": rt.worker, "by": "operator"}),
    );
    assert_eq!(
        dispatched["task"]["id"].as_str(),
        Some(t2.as_str()),
        "real task dispatch refused: {dispatched}"
    );

    let moved = assist_evidence_rev(&fx, &fx.member_envelope("sub-nico"), &job);
    assert_ne!(
        moved, observed,
        "real task create/dispatch did not advance the job's authority revision"
    );
    assert_eq!(
        fx.shared
            .store
            .assist_stall_sweep(300, crate::issue::time::now_epoch() as f64)
            .unwrap(),
        0,
        "recent real task progress left the job stalled"
    );

    // The OLD observed revision refuses: no effect, queued provenance unchanged.
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "member_job_reconcile",
            json!({"member_envelope": fx.member_envelope("sub-nico"), "job": job,
                   "expected_authority_rev": observed}),
        ),
        "stale observed-rev reconcile",
    );
    assert!(
        e.contains("rev") || e.contains("authority") || e.contains("mismatch"),
        "stale observed-rev reconcile refused for another reason: {e}"
    );
    assert_eq!(
        fx.tree(),
        before,
        "a stale observed rev committed an effect"
    );
    let prov_after = assist_provenance(&fx, &job);
    assert_eq!(
        prov_after.credential_id, prov_orig.credential_id,
        "a stale observed rev re-pinned the queued original credential"
    );

    // The member's current legitimate proof (a renewed live credential) plus the
    // FRESH current revision admits the owner's reconcile.
    fx.set_member("sub-nico", |m| {
        m.credential_live = true;
        m.recorded_credential = "cred-sub-nico-new".to_string();
        m.live_credentials.insert(
            "cred-sub-nico-new".to_string(),
            (
                true,
                vec![
                    "assist.read".into(),
                    "assist.write".into(),
                    "assist.jobs".into(),
                ],
            ),
        );
    });
    let renew = fx.envelope(
        "sub-nico",
        json!({}),
        json!({"credential_id": "cred-sub-nico-new"}),
    );
    let fresh = assist_evidence_rev(&fx, &renew, &job);
    let rec = fx.op(
        "member_job_reconcile",
        json!({"member_envelope": renew, "job": job, "expected_authority_rev": fresh}),
    );
    assert_eq!(
        rec["reconciled"].as_str(),
        Some("sub-nico"),
        "fresh-rev owner reconcile refused: {rec}"
    );
    assert_eq!(
        assist_provenance(&fx, &job).credential_id.as_deref(),
        Some("cred-sub-nico-new"),
        "fresh-rev reconcile did not re-pin to the owner's current credential"
    );
}

// ============================================================================
// CAD-1380 review fix (R5) — member_job_evidence grades a task only on an
// AVAILABLE submitted artifact. Independent acceptance check, built on real
// task submission (dispatch kickoff finished by the worker), real verdict
// RPC and a temp LOCAL git repo. Positive/mixed/stale/missing states are
// each asserted per task and for the whole job.
// ============================================================================

/// Run git in `dir` (local, fixed argv) and return trimmed stdout.
fn r5_git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "r5")
        .env("GIT_AUTHOR_EMAIL", "r5@example.invalid")
        .env("GIT_COMMITTER_NAME", "r5")
        .env("GIT_COMMITTER_EMAIL", "r5@example.invalid")
        .output()
        .expect("git unavailable");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Commit `body` as the repo's file and return the new HEAD sha.
fn r5_commit(dir: &std::path::Path, body: &str) -> String {
    std::fs::write(dir.join("artifact.txt"), body).unwrap();
    r5_git(dir, &["add", "artifact.txt"]);
    r5_git(dir, &["commit", "-q", "-m", body]);
    r5_git(dir, &["rev-parse", "HEAD"])
}

/// Real worker submission: dispatch the task, then the worker's kickoff
/// completion reports `sha` through the production finish path.
fn r5_submit(fx: &Fx, rt: &MemberRuntime, task: &str, sha: &str) {
    fx.op(
        "task_dispatch",
        json!({"task": task, "to": rt.worker, "by": "operator"}),
    );
    let kickoff = fx
        .shared
        .store
        .task(task)
        .unwrap()
        .dispatch_message
        .expect("dispatch recorded no kickoff");
    let msg = fx
        .shared
        .store
        .message(&kickoff)
        .unwrap()
        .expect("kickoff missing");
    fx.shared
        .store
        .finish(
            &msg,
            "completed",
            &json!({"status": "completed", "sha": sha}),
            None,
        )
        .unwrap();
}

/// The member's evidence read for `job`.
fn r5_evidence(fx: &Fx, env: &str, job: &str) -> Value {
    fx.op(
        "member_job_evidence",
        json!({"member_envelope": env, "job": job}),
    )
}

/// The per-task evidence entry for `task` in an evidence read.
fn r5_task<'a>(ev: &'a Value, task: &str) -> &'a Value {
    ev["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task"].as_str() == Some(task))
        .unwrap_or_else(|| panic!("no evidence entry for {task}: {ev}"))
}

#[test]
fn member_job_evidence_requires_available_submitted_artifact() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let rt = fx.member_runtime("sub-nico");
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET params=? WHERE alias='ordinary-worker'",
                rusqlite::params![json!({"upstream": rt.alias}).to_string()],
            )?;
            Ok(())
        })
        .unwrap();
    let env = fx.member_envelope("sub-nico");
    let job = fx.op(
        "member_job_request",
        json!({"member_envelope": env, "title": "r5 job", "request_key": "r5-k1", "body": "b"}),
    )["job"]
        .as_str()
        .unwrap()
        .to_string();

    // A real local repo: the worker's submitted commit is an object here.
    let repo = fx.dir.path().join("r5-repo");
    std::fs::create_dir_all(&repo).unwrap();
    r5_git(&repo, &["init", "-q"]);
    let sha1 = r5_commit(&repo, "first artifact\n");
    let repo_s = repo.to_string_lossy().to_string();

    // ---- POSITIVE: one task, present artifact, independent pass. ----
    // Adopt the task `member_job_request` already created. Its server-recorded
    // worktree and assignee are fixture coordinator metadata only: no head,
    // state, verdict or availability is set here.
    let t1 = format!("{job}-t1");
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE tasks SET worktree=?, assignee=? WHERE id=? AND job_id=?",
                rusqlite::params![repo_s, rt.worker, t1, job],
            )?;
            Ok(())
        })
        .unwrap();
    r5_submit(&fx, &rt, &t1, &sha1);
    // The independent reviewer is the operator connection, never the worker.
    fx.op(
        "task_verdict",
        json!({"task": t1, "sha": sha1, "verdict": "pass"}),
    );
    let ev = r5_evidence(&fx, &env, &job);
    let t = r5_task(&ev, &t1);
    let positive_available = t["artifact_available"].as_bool();
    assert_eq!(
        t["evidence_state"].as_str(),
        Some("independently_verified"),
        "positive task: {ev}"
    );
    assert_eq!(
        ev["evidence_state"].as_str(),
        Some("independently_verified"),
        "positive job: {ev}"
    );

    // ---- MIXED: a second task is created but never submitted. The passed
    // first task must not label the whole job independently_verified. ----
    let t2 = format!("{job}-t2");
    fx.op(
        "task_new",
        json!({"job": job, "task": t2, "title": "r5 t2", "assignee": rt.worker, "worktree": repo_s}),
    );
    let ev = r5_evidence(&fx, &env, &job);
    assert_eq!(
        r5_task(&ev, &t1)["evidence_state"].as_str(),
        Some("independently_verified"),
        "mixed first task: {ev}"
    );
    assert_eq!(
        r5_task(&ev, &t2)["evidence_state"].as_str(),
        Some("reported"),
        "unsubmitted task: {ev}"
    );
    assert_ne!(
        ev["evidence_state"].as_str(),
        Some("independently_verified"),
        "mixed job labelled independently_verified: {ev}"
    );
    assert_eq!(
        positive_available,
        Some(true),
        "present artifact read as missing: {ev}"
    );

    // ---- STALE: t1 is reopened and a NEW head is submitted. The old pass
    // names sha1, so it must not rescue sha2. ----
    fx.shared.store.reopen_task(&t1, "operator").unwrap();
    let sha2 = r5_commit(&repo, "second artifact\n");
    r5_submit(&fx, &rt, &t1, &sha2);
    let ev = r5_evidence(&fx, &env, &job);
    let t = r5_task(&ev, &t1);
    assert_eq!(
        t["head_sha"].as_str(),
        Some(sha2.as_str()),
        "new head not recorded: {ev}"
    );
    assert_ne!(
        t["evidence_state"].as_str(),
        Some("independently_verified"),
        "stale pass rescued the new head: {ev}"
    );
    assert_ne!(
        t["evidence_state"].as_str(),
        Some("inspected"),
        "stale verdict inspected the new head: {ev}"
    );

    // ---- MISSING: the recorded worktree disappears. The submitted commit is
    // no longer available, so the task is unavailable (verdict kept as audit
    // data only), never inspected/independently_verified. ----
    std::fs::remove_dir_all(&repo).unwrap();
    let ev = r5_evidence(&fx, &env, &job);
    let t = r5_task(&ev, &t1);
    assert_eq!(
        t["artifact_available"].as_bool(),
        Some(false),
        "missing artifact read as present: {ev}"
    );
    assert_eq!(
        t["evidence_state"].as_str(),
        Some("unavailable"),
        "missing artifact task: {ev}"
    );
    assert_eq!(
        ev["evidence_state"].as_str(),
        Some("unavailable"),
        "missing artifact job: {ev}"
    );
    assert_ne!(
        ev["evidence_state"].as_str(),
        Some("independently_verified"),
        "missing artifact job verified: {ev}"
    );
}

/// R1 narrow bad case: a bound assistant's own `thread_read` must stop at the
/// platform gate once the member's issuing credential is revoked at the
/// platform, while the LOCAL binding stays active. Real `as_assistant`
/// (daemon-derived agent identity) and the real platform fixture; no role in
/// params.
#[test]
fn assist_own_thread_read_refuses_platform_revoked_member() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let sub = "sub-c1380-plat";
    let rt = fx.member_runtime(sub);

    // Positive: live platform authority admits the assistant's own thread read.
    let live = fx.as_assistant(&rt.alias, "thread_read", json!({"alias": rt.alias}));
    assert!(
        live.is_ok(),
        "live member own thread_read refused: {live:?}"
    );

    // Platform-side revocation of the member's issuing credential. The local
    // binding is deliberately left active so only the live pull can refuse.
    fx.set_member(sub, |m| m.credential_live = false);
    assert!(
        fx.shared
            .store
            .assistant_for_alias(&rt.alias)
            .unwrap()
            .is_some_and(|b| b.is_active()),
        "the local binding must remain active for this platform-only case"
    );
    let refused = fx.as_assistant(&rt.alias, "thread_read", json!({"alias": rt.alias}));
    assert!(
        refused.is_err(),
        "platform-revoked member read its own thread through the assistant identity: {refused:?}"
    );
}

// ============================================================================
// CAD-1380 review fix (R1) — a QUEUED member turn is refused by its ORIGINAL
// credential at the claim and admission gates, not by the latest pin. Real
// `member_assist_send`, the real `take_app_aware` / `admit_app_submission`
// helpers and the live-authority fixture; no adapter reached on a refusal.
// ============================================================================

/// A member's send queued while live is refused once its ORIGINAL issuing
/// credential is dead, even though the local binding is still active. The
/// stamp keeps the original `cred-<sub>` id; the refusal retains the message
/// body as failed and records a `member_turn_refused` event. The positive
/// control is the same queued turn taken and admitted while the credential
/// is live.
#[test]
fn queued_member_turn_refused_by_original_credential_at_claim() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let sub = "sub-c1380-queued";
    let rt = fx.member_runtime(sub);

    // Live positive: send, claim and admit while the original credential is live.
    let sent = fx.op(
        "member_assist_send",
        json!({"member_envelope": fx.member_envelope(sub), "text": "live turn"}),
    );
    let live_id = sent["message"]
        .as_str()
        .expect("live send has no message id");
    let live_msg = fx
        .shared
        .store
        .message(live_id)
        .unwrap()
        .expect("live message missing");
    let stamp = fx
        .shared
        .store
        .message_member_authority(live_id)
        .unwrap()
        .expect("live member message carries no member authority stamp");
    assert_eq!(
        stamp.credential_id,
        format!("cred-{sub}"),
        "the queued stamp is not the ORIGINAL issuing credential"
    );
    let (taken, _admission) = fx.shared.take_app_aware(&rt.alias).unwrap();
    let crate::store::Take::Message(taken) = taken else {
        panic!("live queued member turn was not taken");
    };
    assert_eq!(
        taken.id, live_msg.id,
        "took a different message than the live send"
    );
    fx.shared
        .admit_app_submission(&taken)
        .expect("live member turn refused at admission");
    // Post-claim guard: revoke after the claim, then the real admission gate
    // must refuse with a PreWrite error before any adapter is reached.
    fx.set_member(sub, |m| m.credential_live = false);
    assert!(
        matches!(
            fx.shared.admit_app_submission(&taken),
            Err(crate::Error::PreWrite(_))
        ),
        "a revoked post-claim turn was not refused at the PreWrite admission gate"
    );
    fx.set_member(sub, |m| m.credential_live = true);
    // Controlled fixture closure (not provider execution): the first row is
    // finished through the real store so it is no longer the queue head.
    fx.shared
        .store
        .finish(
            &taken,
            "failed",
            &json!({"status": "failed", "text": "", "error": "fixture closure after admission guard"}),
            Some("fixture closure after admission guard"),
        )
        .expect("could not close the first live fixture row");

    // Second send queued while live, then the ORIGINAL credential dies. The
    // local binding stays active, so only the original-credential re-prove
    // can refuse it at claim.
    let queued = fx.op(
        "member_assist_send",
        json!({"member_envelope": fx.member_envelope(sub), "text": "queued turn"}),
    );
    let queued_id = queued["message"]
        .as_str()
        .expect("queued send has no message id")
        .to_string();
    fx.set_member(sub, |m| m.credential_live = false);
    assert!(
        fx.shared
            .store
            .assistant_for_alias(&rt.alias)
            .unwrap()
            .is_some_and(|b| b.is_active()),
        "the local binding must remain active for this original-credential case"
    );

    let (claimed, _admission) = fx.shared.take_app_aware(&rt.alias).unwrap();
    assert!(
        !matches!(claimed, crate::store::Take::Message(_)),
        "a queued turn whose original credential is dead was taken as runnable"
    );
    let retained = fx
        .shared
        .store
        .message(&queued_id)
        .unwrap()
        .expect("refused queued message was dropped, not retained");
    assert!(
        retained.state == "failed" || retained.state == "held",
        "refused queued message is neither failed nor held: {}",
        retained.state
    );
    assert!(
        retained.body.contains("queued turn"),
        "refused queued message lost its body"
    );
    let events = fx.shared.store.events(&rt.alias, 0, 500).unwrap();
    assert!(
        events.iter().any(|e| e.kind == "member_turn_refused"
            && e.payload["message"].as_str() == Some(queued_id.as_str())),
        "no truthful member_turn_refused event names the refused message"
    );
}

// ============================================================================
// CAD-1380 independent acceptance (r4) — a platform-revoked member must not
// receive a NEW private job notification before a live jobs-scope proof.
// Real `member_job_request` route, signed envelopes and the live-authority
// fixture; the existing Store route `job_notice` is driven directly.
// ============================================================================

#[test]
fn assist_job_notice_never_queues_private_delivery_before_live_proof() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let sub = "sub-c1380-r4-notice";
    let rt = fx.member_runtime(sub);

    // Real job request through the signed member route.
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope(sub), "title": "r4 notice job", "request_key": "r4-notice-k1", "body": "r4 body", "acceptance": "r4 acceptance"}),
    );
    let job = req["job"]
        .as_str()
        .expect("member_job_request returned no job id")
        .to_string();
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .into_iter()
        .next()
        .expect("job has no first task");

    // Record the assistant's full message set and queued head before any notice.
    let ids_before: Vec<String> = fx
        .shared
        .store
        .messages(&rt.alias)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    let head_before = fx
        .shared
        .store
        .queued_head(&rt.alias)
        .unwrap()
        .map(|m| m.id);
    assert!(
        head_before.is_none(),
        "fixture assistant already has a queued head: {head_before:?}"
    );

    // Platform revocation of the issuing credential; the local binding stays active.
    fx.set_member(sub, |m| m.credential_live = false);
    assert!(
        fx.shared
            .store
            .assistant_for_alias(&rt.alias)
            .unwrap()
            .is_some_and(|b| b.is_active()),
        "the local binding must remain active for this platform-only case"
    );

    // The existing Store route fires a private notice for the job's task.
    fx.shared
        .store
        .job_notice(
            &task.id,
            "blocked",
            "independent-r4-no-direct",
            "PRIVATE_NOTE_MUST_NOT_FAN_OUT",
        )
        .expect("real job notice route failed");

    // No NEW queued message may land on the revoked member's private assistant.
    let head_after = fx
        .shared
        .store
        .queued_head(&rt.alias)
        .unwrap()
        .map(|m| m.id);
    assert!(
        head_after.is_none(),
        "a revoked member's private assistant received a new queued notice: {head_after:?}"
    );
    let ids_after: Vec<String> = fx
        .shared
        .store
        .messages(&rt.alias)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(
        ids_after, ids_before,
        "a revoked member's private assistant gained a new message row"
    );
}

// ============================================================================
// CAD-1380 independent acceptance (r4 drain) - the real Shared drain fans out
// metadata-only assist notices to live, entitled subscribers only. Real
// member_job_request / member_job_share / member_job_subscribe RPCs, real
// Store.job_notice, the real route_assist_job_notifications drain and the real
// admit_app_submission gate. No adapter or provider is reached.
// ============================================================================

/// Full message-id set for an alias (every state), in store order.
fn r4d_ids(fx: &Fx, alias: &str) -> Vec<String> {
    fx.shared
        .store
        .messages(alias)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect()
}

#[test]
fn assist_drain_notifies_only_live_entitled_subscribers() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let owner = fx.member_runtime("sub-ana");
    let sub = fx.member_runtime("sub-ben");
    let control = fx.member_runtime("sub-cid");

    // Owner requests the job; the request body carries a private marker.
    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-ana"), "title": "r4d drain job", "request_key": "r4d-k1", "body": "PRIVATE_REQUEST_BODY_R4D", "acceptance": "r4d acceptance"}),
    );
    let job = req["job"].as_str().unwrap().to_string();
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .into_iter()
        .next()
        .expect("job has no first task");

    // The source also emits the inert `awaiting_approval` notice for a new
    // request. Drain it now, while the owner is live, and check it is metadata
    // only. Only this real drain runs: no provider, delete or finish of rows.
    let initial = fx
        .shared
        .route_assist_job_notifications()
        .expect("real drain failed (initial awaiting_approval)");
    assert!(
        initial >= 1,
        "initial awaiting_approval notice was not queued"
    );
    for m in fx.shared.store.messages(&owner.alias).unwrap() {
        if m.source == "assist_job_event" {
            for private in [
                "PRIVATE_REQUEST_BODY_R4D",
                "r4d drain job",
                "r4d acceptance",
            ] {
                assert!(
                    !m.body.contains(private),
                    "initial owner notice leaked {private}: {}",
                    m.body
                );
            }
        }
    }

    // Owner shares; owner and subscriber subscribe through the real RPCs.
    fx.op(
        "member_job_share",
        json!({"member_envelope": fx.member_envelope("sub-ana"), "job": job}),
    );
    for s in ["sub-ana", "sub-ben"] {
        fx.op(
            "member_job_subscribe",
            json!({"member_envelope": fx.member_envelope(s), "job": job}),
        );
    }

    let owner_base = r4d_ids(&fx, &owner.alias);
    let sub_base = r4d_ids(&fx, &sub.alias);
    let control_base = r4d_ids(&fx, &control.alias);

    // ---- 1. Both live: one real notice, one drain. Owner and subscriber each
    // gain metadata-only job events; the non-subscriber gains none. ----
    fx.shared
        .store
        .job_notice(
            &task.id,
            "blocked",
            "r4d-a",
            "PRIVATE_NOTE_R4D_MUST_NOT_FAN_OUT",
        )
        .expect("real job notice route failed (step 1)");
    let queued = fx
        .shared
        .route_assist_job_notifications()
        .expect("real drain failed (step 1)");
    assert_eq!(queued, 2, "drain did not queue owner + subscriber notices");
    let owner_1 = r4d_ids(&fx, &owner.alias);
    let sub_1 = r4d_ids(&fx, &sub.alias);
    assert_eq!(
        owner_1.len(),
        owner_base.len() + 1,
        "owner did not gain one notice"
    );
    assert_eq!(
        sub_1.len(),
        sub_base.len() + 1,
        "subscriber did not gain one notice"
    );
    assert_eq!(
        r4d_ids(&fx, &control.alias),
        control_base,
        "non-subscriber received a job notice"
    );
    let sub_notice_id = sub_1
        .iter()
        .find(|id| !sub_base.contains(id))
        .unwrap()
        .clone();
    for (alias, base) in [(&owner.alias, &owner_base), (&sub.alias, &sub_base)] {
        for m in fx.shared.store.messages(alias).unwrap() {
            if m.source != "assist_job_event" || base.contains(&m.id) {
                continue;
            }
            assert!(
                m.body
                    .starts_with(&format!("Cadence job {job}: task {} r", task.id)),
                "notice body is not the synthetic metadata form: {}",
                m.body
            );
            assert!(
                m.body.contains("is now blocked"),
                "notice body lacks the synthetic state: {}",
                m.body
            );
            for private in [
                "PRIVATE_REQUEST_BODY_R4D",
                "PRIVATE_NOTE_R4D",
                "r4d drain job",
                "r4d acceptance",
            ] {
                assert!(
                    !m.body.contains(private),
                    "notice body leaked {private}: {}",
                    m.body
                );
            }
        }
    }

    // ---- 2. Drain again with no new intents: nothing new is queued. ----
    let again = fx
        .shared
        .route_assist_job_notifications()
        .expect("second drain failed");
    assert_eq!(again, 0, "an already-drained intent was re-queued");
    assert_eq!(
        r4d_ids(&fx, &owner.alias),
        owner_1,
        "replayed drain changed the owner"
    );
    assert_eq!(
        r4d_ids(&fx, &sub.alias),
        sub_1,
        "replayed drain changed the subscriber"
    );

    // ---- 3. A distinct transition key is not suppressed by the first one. ----
    fx.shared
        .store
        .job_notice(
            &task.id,
            "blocked",
            "r4d-b",
            "PRIVATE_NOTE_R4D_MUST_NOT_FAN_OUT",
        )
        .expect("real job notice route failed (step 3)");
    let distinct = fx
        .shared
        .route_assist_job_notifications()
        .expect("drain failed (step 3)");
    assert_eq!(distinct, 2, "a distinct transition key was suppressed");
    let owner_3 = r4d_ids(&fx, &owner.alias);
    let sub_3 = r4d_ids(&fx, &sub.alias);
    assert_eq!(
        owner_3.len(),
        owner_1.len() + 1,
        "owner missed the distinct notice"
    );
    assert_eq!(
        sub_3.len(),
        sub_1.len() + 1,
        "subscriber missed the distinct notice"
    );

    // ---- 4. Revoke only the subscriber's platform credential; the local binding
    // stays active. The next notice reaches the owner only. ----
    fx.set_member("sub-ben", |m| m.credential_live = false);
    assert!(
        fx.shared
            .store
            .assistant_for_alias(&sub.alias)
            .unwrap()
            .is_some_and(|b| b.is_active()),
        "the subscriber's local binding must stay active"
    );
    fx.shared
        .store
        .job_notice(
            &task.id,
            "blocked",
            "r4d-c",
            "PRIVATE_NOTE_R4D_MUST_NOT_FAN_OUT",
        )
        .expect("real job notice route failed (step 4)");
    let revoked = fx
        .shared
        .route_assist_job_notifications()
        .expect("drain failed (step 4)");
    assert_eq!(
        revoked, 1,
        "revoked subscriber drain count is not owner-only"
    );
    let owner_4 = r4d_ids(&fx, &owner.alias);
    assert_eq!(
        owner_4.len(),
        owner_3.len() + 1,
        "owner missed the post-revoke notice"
    );
    assert_eq!(
        r4d_ids(&fx, &sub.alias),
        sub_3,
        "revoked subscriber received a new message after revocation"
    );
    assert_eq!(
        r4d_ids(&fx, &control.alias),
        control_base,
        "non-subscriber received a job notice after revocation"
    );

    // ---- 5. The subscriber's step-1 notice was never taken. Admission at the
    // effect-time job-scope gate must refuse it with PreWrite, keeping the
    // original row and body. ----
    let queued_sub = fx
        .shared
        .store
        .message(&sub_notice_id)
        .unwrap()
        .expect("step-1 subscriber notice missing");
    let body_before = queued_sub.body.clone();
    let state_before = queued_sub.state.clone();
    assert!(
        matches!(
            fx.shared.admit_app_submission(&queued_sub),
            Err(crate::Error::PreWrite(_))
        ),
        "a revoked subscriber's queued notice was admitted at the effect gate"
    );
    let after = fx
        .shared
        .store
        .message(&sub_notice_id)
        .unwrap()
        .expect("refused subscriber notice was dropped");
    assert_eq!(after.body, body_before, "refused notice body changed");
    assert_eq!(after.state, state_before, "refused notice state changed");
}

/// CAD-1380 R4 (drain routing + stall trigger). CAD-1420 retired the
/// feature-enable gate, so the drain routes without any flag: the initial
/// intent is delivered as metadata only, and a real stall sweep queues a
/// metadata-only owner notice through the same drain without dispatching the
/// task. Authority, body and custody privacy are still asserted on each notice.
#[test]
fn assist_drain_routes_without_flag_and_stall_notice_is_metadata_only() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let owner = fx.member_runtime("sub-ana");

    let req = fx.op(
        "member_job_request",
        json!({"member_envelope": fx.member_envelope("sub-ana"), "title": "r4s stall job", "request_key": "r4s-k1", "body": "PRIVATE_REQUEST_BODY_R4S", "acceptance": "r4s acceptance"}),
    );
    let job = req["job"].as_str().unwrap().to_string();
    let task = fx
        .shared
        .store
        .tasks_for_job(&job)
        .unwrap()
        .into_iter()
        .next()
        .expect("job has no first task");

    // ---- 1. Pending intents are durable: captured before the drain. ----
    let pending_before = fx
        .shared
        .store
        .pending_assist_job_notifications(50)
        .unwrap();
    assert!(
        !pending_before.is_empty(),
        "the member_job_request left no pending initial intent"
    );
    let owner_before_gate = r4d_ids(&fx, &owner.alias);
    let job_before = fx.shared.store.job(&job).unwrap();
    let task_before = fx.shared.store.task(&task.id).unwrap();

    // ---- 2. The drain delivers the initial metadata notice to the live owner
    // and nothing private. ----
    let initial = fx
        .shared
        .route_assist_job_notifications()
        .expect("exact-true drain failed");
    assert!(initial >= 1, "exact-true drain queued no initial notice");
    let owner_initial = r4d_ids(&fx, &owner.alias);
    assert!(
        owner_initial.len() > owner_before_gate.len(),
        "the owner gained no initial metadata notice"
    );
    assert_eq!(
        fx.shared.store.job(&job).unwrap().state,
        job_before.state,
        "the initial drain changed the job state"
    );
    assert_eq!(
        fx.shared.store.task(&task.id).unwrap().state,
        task_before.state,
        "the initial drain changed the task state"
    );
    for m in fx.shared.store.messages(&owner.alias).unwrap() {
        if m.source == "assist_job_event" {
            for private in ["PRIVATE_REQUEST_BODY_R4S", "r4s acceptance"] {
                assert!(
                    !m.body.contains(private),
                    "initial notice leaked {private}: {}",
                    m.body
                );
            }
        }
    }

    // ---- 3. Age the job past the assist bound and sweep. The sweep itself
    // queues no private mail and dispatches nothing. ----
    age_job(&fx, &job, 400);
    let owner_pre_sweep = r4d_ids(&fx, &owner.alias);
    let swept = fx
        .shared
        .store
        .assist_stall_sweep(300, crate::issue::time::now_epoch() as f64)
        .expect("real stall sweep failed");
    assert_eq!(swept, 1, "the stalled assist job produced no check");
    assert_eq!(
        r4d_ids(&fx, &owner.alias),
        owner_pre_sweep,
        "the sweep alone queued a private message"
    );
    let stalled_drain = fx
        .shared
        .route_assist_job_notifications()
        .expect("drain after stall failed");
    assert!(stalled_drain >= 1, "the stall intent was not drained");
    let owner_stalled = r4d_ids(&fx, &owner.alias);
    let stall_ids: Vec<&String> = owner_stalled
        .iter()
        .filter(|id| !owner_pre_sweep.contains(id))
        .collect();
    assert_eq!(
        stall_ids.len(),
        1,
        "expected exactly one new owner stall notice"
    );
    let stall_msg = fx
        .shared
        .store
        .message(stall_ids[0])
        .unwrap()
        .expect("stall notice missing");
    assert_eq!(stall_msg.source, "assist_job_event");
    assert!(
        stall_msg.body.contains("is now stalled"),
        "stall notice lacks the synthetic state: {}",
        stall_msg.body
    );
    for private in [
        "PRIVATE_REQUEST_BODY_R4S",
        "r4s stall job",
        "r4s acceptance",
    ] {
        assert!(
            !stall_msg.body.contains(private),
            "stall notice leaked {private}: {}",
            stall_msg.body
        );
    }
    let task_after = fx.shared.store.task(&task.id).unwrap();
    assert_eq!(
        task_after.state, "draft",
        "the stall sweep dispatched the task"
    );
    assert!(
        task_after.dispatch_message.is_none(),
        "the stall sweep minted a kickoff"
    );

    // ---- 4. Same-watermark sweep returns 0; a repeated drain adds no IDs. ----
    let again_swept = fx
        .shared
        .store
        .assist_stall_sweep(300, crate::issue::time::now_epoch() as f64)
        .expect("repeat stall sweep failed");
    assert_eq!(again_swept, 0, "a same-watermark stall was re-flagged");
    let replay = fx
        .shared
        .route_assist_job_notifications()
        .expect("repeat drain failed");
    assert_eq!(replay, 0, "an already-drained stall intent was re-queued");
    assert_eq!(
        r4d_ids(&fx, &owner.alias),
        owner_stalled,
        "a repeated sweep or drain added message IDs"
    );
}

/// The ordinary worker's upstream metadata, as the R5 fixture sets it, so the
/// operator dispatch to it is in the job's group.
fn r4a_upstream(fx: &Fx, rt: &MemberRuntime) {
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET params=? WHERE alias='ordinary-worker'",
                rusqlite::params![json!({"upstream": rt.alias}).to_string()],
            )?;
            Ok(())
        })
        .unwrap();
}

/// CAD-1380 R4 (author REVISE) - a real operator `revise` verdict on a
/// submitted task notifies the assigned author with metadata only. A
/// replacement author whose registered birth is after the real kickoff is
/// refused at the drain and gets no notice. No provider runs; the kickoff and
/// the verdict are real RPCs and the worker's completion is the controlled
/// `r5_submit` finish.
#[test]
fn assist_revise_notifies_only_kickoff_proven_author_metadata_only() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let rt = fx.member_runtime("sub-rev");
    let env = fx.member_envelope("sub-rev");
    let job = fx.op(
        "member_job_request",
        json!({"member_envelope": env, "title": "r4a revise job", "request_key": "r4a-k1", "body": "PRIVATE_REQUEST_BODY_R4A", "acceptance": "r4a acceptance"}),
    )["job"]
        .as_str()
        .unwrap()
        .to_string();

    let repo = fx.dir.path().join("r4a-repo");
    r4a_upstream(&fx, &rt);
    std::fs::create_dir_all(&repo).unwrap();
    r5_git(&repo, &["init", "-q"]);
    let sha1 = r5_commit(&repo, "r4a artifact\n");
    let repo_s = repo.to_string_lossy().to_string();

    let t1 = format!("{job}-t1");
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE tasks SET worktree=?, assignee=? WHERE id=? AND job_id=?",
                rusqlite::params![repo_s, rt.worker, t1, job],
            )?;
            Ok(())
        })
        .unwrap();

    // Drain the initial awaiting_approval intents, then baseline the worker.
    fx.shared
        .route_assist_job_notifications()
        .expect("initial drain failed");
    // Real submission (the kickoff), baseline AFTER it, then an independent
    // operator REVISE with a private note.
    r5_submit(&fx, &rt, &t1, &sha1);
    let worker_base = r4d_ids(&fx, &rt.worker);
    fx.op(
        "task_verdict",
        json!({"task": t1, "sha": sha1, "verdict": "revise", "message": "PRIVATE_QA_NOTE_R4A"}),
    );
    let task = fx.shared.store.task(&t1).unwrap();
    assert_eq!(task.state, "revising", "REVISE did not reopen the task");
    // The REVISE intent carries the stored revision; the verdict keeps it.
    let rev = task.revision;

    // ---- 1. Real drain: exactly one new author notice, metadata only. ----
    // The owner's own subscriber notice may also queue; only the worker counts.
    fx.shared
        .route_assist_job_notifications()
        .expect("REVISE drain failed");
    let worker_1 = r4d_ids(&fx, &rt.worker);
    let new_ids: Vec<&String> = worker_1
        .iter()
        .filter(|id| !worker_base.contains(id))
        .collect();
    assert_eq!(new_ids.len(), 1, "expected exactly one new author message");
    let notice = fx
        .shared
        .store
        .message(new_ids[0])
        .unwrap()
        .expect("author notice missing");
    assert_eq!(
        notice.source, "job_event",
        "author notice source is not job_event"
    );
    assert!(
        notice
            .body
            .starts_with(&format!("Cadence job {job}: task {t1} r{rev}")),
        "author notice lacks canonical job/task/revision: {}",
        notice.body
    );
    assert!(
        notice.body.contains("is now revising"),
        "author notice lacks the state: {}",
        notice.body
    );
    for private in [
        "PRIVATE_REQUEST_BODY_R4A",
        "PRIVATE_QA_NOTE_R4A",
        "r4a acceptance",
    ] {
        assert!(
            !notice.body.contains(private),
            "author notice leaked {private}"
        );
    }
    assert_eq!(
        fx.shared.store.task(&t1).unwrap().state,
        "revising",
        "the notice changed the task state"
    );

    // ---- 2. Replay adds no new worker IDs and no kickoff is minted. ----
    let replay = fx
        .shared
        .route_assist_job_notifications()
        .expect("replay drain failed");
    assert_eq!(replay, 0, "an already-drained REVISE intent was re-queued");
    assert_eq!(
        r4d_ids(&fx, &rt.worker),
        worker_1,
        "a replayed REVISE drain added worker IDs"
    );
}

/// Bad case: a replacement author whose registered birth is AFTER the real
/// kickoff. The fixture birth is mutated after the REVISE intent exists; the
/// drain must refuse it with no author notice, and the message stays unqueued.
#[test]
fn assist_revise_refuses_author_born_after_kickoff() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let rt = fx.member_runtime("sub-rev2");
    let env = fx.member_envelope("sub-rev2");
    let job = fx.op(
        "member_job_request",
        json!({"member_envelope": env, "title": "r4a birth job", "request_key": "r4a-k2", "body": "PRIVATE_REQUEST_BODY_R4A2", "acceptance": "r4a2 acceptance"}),
    )["job"]
        .as_str()
        .unwrap()
        .to_string();

    let repo = fx.dir.path().join("r4a-repo2");
    r4a_upstream(&fx, &rt);
    std::fs::create_dir_all(&repo).unwrap();
    r5_git(&repo, &["init", "-q"]);
    let sha1 = r5_commit(&repo, "r4a2 artifact\n");
    let repo_s = repo.to_string_lossy().to_string();

    let t1 = format!("{job}-t1");
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE tasks SET worktree=?, assignee=? WHERE id=? AND job_id=?",
                rusqlite::params![repo_s, rt.worker, t1, job],
            )?;
            Ok(())
        })
        .unwrap();
    fx.shared
        .route_assist_job_notifications()
        .expect("initial drain failed");
    r5_submit(&fx, &rt, &t1, &sha1);
    fx.op(
        "task_verdict",
        json!({"task": t1, "sha": sha1, "verdict": "revise", "message": "PRIVATE_QA_NOTE_R4A2"}),
    );
    assert_eq!(fx.shared.store.task(&t1).unwrap().state, "revising");

    // Negative fixture input: move the assignee's registered birth AFTER the
    // real kickoff. This is not a permission rescue; the drain must refuse.
    let kickoff = fx
        .shared
        .store
        .task_kickoff_created(&t1, &rt.worker)
        .unwrap()
        .expect("no real kickoff for the worker");
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET created=? WHERE alias=?",
                rusqlite::params![kickoff + 3600.0, rt.worker],
            )?;
            Ok(())
        })
        .unwrap();

    let pending_author = |fx: &Fx| {
        fx.shared
            .store
            .pending_assist_job_notifications(50)
            .unwrap()
            .into_iter()
            .filter(|(_, p)| p["target"] == "author")
            .count()
    };
    assert_eq!(
        pending_author(&fx),
        1,
        "the REVISE author intent was not pending before the refused drain"
    );
    fx.shared
        .route_assist_job_notifications()
        .expect("birth-refusal drain returned an error");
    assert_eq!(
        pending_author(&fx),
        0,
        "the refused author intent stayed pending (not finished)"
    );
    // The birth mutation hides pre-birth terminal rows from `messages()`, so
    // compare the worker's full queued set: no new job_event row may exist.
    let new_author_notice = fx
        .shared
        .store
        .messages(&rt.worker)
        .unwrap()
        .into_iter()
        .filter(|m| m.source == "job_event")
        .count();
    assert_eq!(
        new_author_notice, 0,
        "an author born after kickoff gained a job_event notice"
    );

    assert_eq!(
        fx.shared.store.task(&t1).unwrap().state,
        "revising",
        "the refused drain changed the task"
    );
}

/// CAD-1380 R4 (Master Home) acceptance: one owned `serve_with` daemon (stateB)
/// on its own `/tmp/c1380-*` root, a real confined `master_start`, a real
/// signed member request and the real R4 drain. `Fx` supplies only the
/// loopback platform, signing key and member authority (its own stateA).
/// Native commands for this case go through stateB's socket. The Pi runtime is
/// the synthetic gateway `tests/fixtures/pi-gateway.py` (controlled evidence,
/// not provider acceptance). stateB is stopped and joined by RAII.
mod master_home_acceptance {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use serde_json::{json, Value};
    use tempfile::{Builder, TempDir};

    use super::{AssistantFlagOn, Fx, MemberAuthority, ServiceAuthority, ORG};
    use crate::adapter::ProviderEnv;
    use crate::{client, daemon, master};

    /// The owned daemon (stateB): stop flag, join handle and the state dir.
    struct OwnedDaemon {
        _root: TempDir,
        state: PathBuf,
        stop: Arc<AtomicBool>,
        serve: Option<JoinHandle<crate::Result<()>>>,
    }

    /// The lane's own `target/debug/cadence`, a sibling of the test binary
    /// (`target/debug/deps/cadence_agent-*`). Never the installed binary.
    fn lane_cadence_bin() -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let bin = exe.parent().unwrap().parent().unwrap().join("cadence");
        assert!(
            bin.is_file(),
            "lane cadence binary missing: {}",
            bin.display()
        );
        bin
    }

    impl OwnedDaemon {
        /// Start stateB. The board config and the enrolled service credential
        /// are written BEFORE `serve_with`, from the same `Fx` platform.
        fn start(fx: &Fx, token: &str, sub: &str, fake_gateway: &Path) -> Self {
            // The process temp root (respects TMPDIR), the same root
            // `test_seam::confine` checks. A hardcoded `/tmp` is refused on
            // runners whose temp root differs.
            let root = Builder::new().prefix("c1380-").tempdir().unwrap();
            let [state, pm, home] = ["s", "pm", "h"].map(|d| root.path().join(d));
            // Real PM tracker, the same supported init `Fx::new` uses.
            crate::issue::Pm::init(&pm).unwrap();
            // Synthetic model allowlist, ONLY in this temp pm.yaml.
            let yaml = pm.join("pm.yaml");
            let mut text = std::fs::read_to_string(&yaml).unwrap();
            text.push_str(
                "\npi:\n  models:\n    allow: [\"fake/model-1\"]\n    default: {master: \"fake/model-1\", worker: \"fake/model-1\"}\n",
            );
            std::fs::write(&yaml, text).unwrap();
            // The same trust root `Fx` uses: its loopback platform host/issuer.
            std::fs::create_dir_all(&state).unwrap();
            crate::board_identity::write_config(
                &state,
                &crate::board_identity::Config {
                    host: fx.host.clone(),
                    issuer: fx.issuer.clone(),
                    company: ORG.to_string(),
                },
            )
            .unwrap();
            // The enrolled `assist.revalidate` service credential, written to
            // stateB's custody and platform_credentials row (not Fx's store).
            Self::enroll_service(&state, &fx.host, token, sub);

            let env = ProviderEnv::refusing_providers();
            env.set(
                "CADENCE_PI_COMMAND",
                format!("python3 {}", fake_gateway.display()),
            );
            env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
            env.set("HOME", home.to_str().unwrap());
            env.set(
                "CADENCE_CONFINE_COMMAND",
                lane_cadence_bin().to_string_lossy(),
            );
            let stop = Arc::new(AtomicBool::new(false));
            let opts = daemon::ServeOptions {
                provider_env: env,
                stop: Some(stop.clone()),
                test_seam: true,
                ..Default::default()
            };
            let dir = state.clone();
            let serve = Some(thread::spawn(move || daemon::serve_with(&dir, opts)));
            let mut this = Self {
                _root: root,
                state,
                stop,
                serve,
            };
            // Same 60 s deadline. A serve thread that ends first is a startup
            // failure: take its real `serve_with` Result and report it.
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                if client::rpc_timeout(&this.state, "health", json!({}), Duration::from_secs(1))
                    .is_ok()
                {
                    break;
                }
                if this.serve.as_ref().is_some_and(|h| h.is_finished()) {
                    let result = this
                        .serve
                        .take()
                        .expect("serve handle present")
                        .join()
                        .expect("serve_with thread panicked");
                    panic!("owned daemon startup failed: serve_with returned {result:?}");
                }
                assert!(Instant::now() < deadline, "daemon health: timed out");
                thread::sleep(Duration::from_millis(50));
            }
            this
        }

        /// TRUSTED SYNTHETIC BOOTSTRAP FIXTURE - not a recorded operator
        /// enrollment and not proof of supported `platform_enroll`, custody
        /// isolation or activation. It writes the same two artifacts
        /// `Fx::enroll_service` writes (generated dummy token in temp custody
        /// plus a matching `platform_credentials` row) into the fresh, owned
        /// stateB before `serve_with` starts, then closes the handle so the
        /// daemon is the sole owner. It only seeds CONSUMER guards.
        fn enroll_service(state: &Path, host: &str, token: &str, sub: &str) {
            let key = crate::platform::custody::Key {
                platform: "hosted-board",
                account: host,
            };
            crate::platform::custody::Custody::open(state)
                .unwrap()
                .put(&key, token.as_bytes())
                .unwrap();
            let fingerprint = crate::secret::fingerprint(token.as_bytes());
            // Create the schema on the exact DB file the daemon opens, then
            // drop every handle before `serve_with` makes the daemon the owner.
            let store = crate::store::Store::open(&state.join("cadence.sqlite3")).unwrap();
            store
                .assistant_bind(
                    ORG,
                    sub,
                    &format!("assist_{sub}"),
                    &format!("assist-agent-{sub}"),
                    Some("rev0"),
                )
                .unwrap();
            drop(store);
            let conn = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO platform_credentials
                 (platform,account,scopes,fingerprint,custody,exchange,enrolled_at,by,connection_id,credential_revision)
                 VALUES('hosted-board',?1,'[\"assist.revalidate\"]',?2,'file','token',1.0,'trusted-fixture',
                        'conn-hosted-board-fixture',1)",
                rusqlite::params![host, fingerprint],
            )
            .unwrap();
            drop(conn);
        }

        fn rpc(&self, method: &str, params: Value) -> Value {
            client::rpc(&self.state, method, params).unwrap_or_else(|e| panic!("{method}: {e}"))
        }

        /// The native refusal of a call made AS the registered Master, via the
        /// compiled test-seam assertion (`Asserted::Agent`), or `None` if it
        /// succeeded. JSON `by` is never the caller identity.
        fn master_err(&self, method: &str, params: Value) -> Option<String> {
            crate::test_seam::scoped(
                crate::test_seam::Asserted::Agent(master::ALIAS.to_string()),
                || client::rpc(&self.state, method, params),
            )
            .err()
            .map(|e| e.to_string())
        }

        /// The same request from the operator connection (control contrast).
        fn rpc_err_operator(&self, method: &str, params: Value) -> Option<String> {
            crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
                client::rpc(&self.state, method, params)
            })
            .err()
            .map(|e| e.to_string())
        }

        fn until(&self, what: &str, mut done: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !done() {
                assert!(Instant::now() < deadline, "{what}: timed out");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    impl Drop for OwnedDaemon {
        fn drop(&mut self) {
            // Stop only the daemon this test started: flag, then join.
            self.stop.store(true, Ordering::SeqCst);
            if let Some(h) = self.serve.take() {
                let _ = h.join();
            }
        }
    }

    #[test]
    fn master_home_report_is_metadata_only_and_master_cannot_verdict() {
        let _flag = AssistantFlagOn::arm();
        let fx = Fx::new();
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi-gateway.py");
        let token = format!("hcs_{}", "e".repeat(43));
        let sub = "sub-home";
        // Fixture platform member authority: the live membership and recorded
        // credential `Fx::provision` records (its binding half is on stateB).
        fx.authority
            .lock()
            .unwrap()
            .members
            .insert(sub.to_string(), MemberAuthority::live(sub));
        // Fixture platform authority for stateB's service bearer: the same
        // registration `Fx::enroll_service` makes (org, audience, live cap).
        fx.authority
            .lock()
            .unwrap()
            .services
            .push(ServiceAuthority {
                token: token.clone(),
                org: ORG.to_string(),
                audience: format!("http://{}", fx.host),
                caps: vec!["assist.revalidate".into()],
                live: true,
            });
        let daemon = OwnedDaemon::start(&fx, &token, sub, &fake);

        // Real confined master_start on stateB: no unconfined, no TEST_NO_LANDLOCK.
        let started = daemon.rpc("master_start", json!({"provider": "pi"}));
        assert_eq!(
            started["confined"],
            json!(true),
            "master not confined: {started}"
        );
        assert_eq!(
            started["alias"],
            json!(master::ALIAS),
            "master did not register: {started}"
        );

        // Real signed member request on stateB, with a private marker.
        let env = fx.member_envelope(sub);
        let req = daemon.rpc(
            "member_job_request",
            json!({"member_envelope": env, "title": "home job", "request_key": "home-k1", "body": "PRIVATE_REQUEST_BODY_HOME", "acceptance": "home acceptance"}),
        );
        let job = req["job"].as_str().unwrap().to_string();
        let task = format!("{job}-t1");
        let master_body = |d: &OwnedDaemon| -> Vec<Value> {
            d.rpc("thread_read", json!({"alias": master::ALIAS, "limit": 500}))["entries"]
                .as_array()
                .unwrap()
                .clone()
        };
        let report_count = |entries: &[Value]| {
            entries
                .iter()
                .filter(|e| {
                    e["text"].as_str().is_some_and(|b| {
                        b.starts_with(&format!("Cadence job {job}: task {task} r"))
                    })
                })
                .count()
        };

        // The owned daemon's own watch loop runs the real drain. Wait for the
        // deterministic awaiting_approval report to appear on the Home thread.
        daemon.until("master Home report", || {
            report_count(&master_body(&daemon)) >= 1
        });
        let entries = master_body(&daemon);
        let reports: Vec<&Value> = entries
            .iter()
            .filter(|e| {
                e["text"]
                    .as_str()
                    .is_some_and(|b| b.starts_with(&format!("Cadence job {job}: task {task} r")))
            })
            .collect();
        assert_eq!(
            reports.len(),
            1,
            "expected exactly one Home report: {entries:?}"
        );
        let body = reports[0]["text"].as_str().unwrap();
        assert!(
            body.contains("is now awaiting_approval"),
            "report lacks the state: {body}"
        );
        for private in ["PRIVATE_REQUEST_BODY_HOME", "home acceptance", "home job"] {
            assert!(
                !body.contains(private),
                "Home report leaked {private}: {body}"
            );
        }

        // Replay: a second watch-loop pass and a repeated read add no report.
        let base = report_count(&master_body(&daemon));
        // Discard an in-flight drain, then witness a fresh completed pass
        // of this owned daemon's real watch loop.
        super::super::test_events::subscribe(&daemon.state)
            .wait_for(
                super::super::test_events::Event::AssistNoticesDrained,
                2,
                Duration::from_secs(10),
            )
            .expect("the owned daemon did not drain notices again");
        assert_eq!(
            report_count(&master_body(&daemon)),
            base,
            "a replay duplicated the report"
        );
        assert_eq!(base, 1, "the report count changed after replay");

        // Identity contrast: the same valid dispatch from the OPERATOR connection
        // is not a master-policy refusal, so the `master_refused` denial below is
        // caused by the registered Master identity, not the payload.
        let as_operator = daemon.rpc_err_operator(
            "task_dispatch",
            json!({"task": task, "to": master::ALIAS, "by": "operator"}),
        );
        assert!(
            as_operator
                .as_deref()
                .is_none_or(|e| !e.contains("the master may not call")),
            "the operator's call was refused by the Master policy: {as_operator:?}"
        );

        // The Master's own task_dispatch and task_verdict, with a valid request
        // shape, are refused by the MASTER POLICY (`master_refused`), not by a
        // draft-task or zero-SHA error.
        let task_before = daemon.rpc("task_show", json!({"task": task}));
        let policy = |method: &str, err: Option<String>| {
            let err = err.unwrap_or_else(|| panic!("the Master was allowed to call {method}"));
            // The two real master-policy refusals (`master_refused`): the Home
            // allowlist text, or the app-conversation gate text. Anything else
            // is not the policy and fails.
            let home = format!("the master may not call {method}");
            let app = format!("not {method}; setup and admin work is the home thread's");
            assert!(
                err.contains(&home) || err.contains(&app),
                "{method} was not refused by the master policy: {err}"
            );
            assert!(
                !err.contains("draft"),
                "{method} refused for task state, not policy: {err}"
            );
        };
        let dispatch = daemon.master_err(
            "task_dispatch",
            json!({"task": task, "to": master::ALIAS, "by": "operator"}),
        );
        policy("task_dispatch", dispatch);
        let verdict = daemon.master_err(
            "task_verdict",
            json!({"task": task, "sha": "0".repeat(40), "verdict": "pass"}),
        );
        policy("task_verdict", verdict);
        assert_eq!(
            daemon.rpc("task_show", json!({"task": task})),
            task_before,
            "a refused Master call changed the task"
        );
        assert_eq!(
            report_count(&master_body(&daemon)),
            1,
            "a refused call wrote a Home report"
        );
        let _ = fake;
    }
}

/// CAD-1380 independent refusal acceptance (review-sol cad7dcd, rules 1-4).
/// Authored by the acceptance author, NOT the implementer. Each bad case is
/// paired with a positive control in the same test. A red result here is a
/// real source defect for the implementer to fix, never a reason to weaken.
mod cad1380_rereview_refusal {
    use super::*;

    /// Rule 1: an EXISTING registered runtime whose cwd is another principal's
    /// root must be refused BEFORE the adapter is granted it, on the real
    /// resume path. The positive control proves the canonical cwd is not a
    /// blanket deny.
    #[test]
    fn existing_foreign_cwd_refused_before_resume_grant() {
        let _flag = AssistantFlagOn::arm();
        let fx = Fx::new();
        // Two independent members. The POSITIVE (canonical root) and the BAD
        // (foreign root) runtimes are separate aliases, so the bad alias has
        // no actor before its stored cwd is changed.
        let good = fx.member_runtime("sub-cwd-good");
        let bad = fx.member_runtime("sub-cwd-bad");
        let foreign = fx.dir.path().join("foreign-principal-root");
        std::fs::create_dir_all(&foreign).unwrap();
        // Positive control: the canonical root passes the cwd guard on the REAL
        // resume path. Provider/launch outcomes may vary, so the only property
        // asserted is that the refusal never names the principal cwd.
        match fx.shared.try_resume_assistant(&good.alias) {
            Ok(()) => assert!(
                fx.shared.lifecycle.lock().unwrap().owned(&good.alias),
                "canonical resume returned Ok without a live actor"
            ),
            Err(e) => assert!(
                !e.to_string().contains("principal's dedicated cwd"),
                "canonical cwd was blanket-refused: {e}"
            ),
        }
        // Tear the positive actor down through the public agent_stop path and
        // assert it left the lifecycle table, so nothing from it runs during
        // the bad case.
        if fx.shared.lifecycle.lock().unwrap().owned(&good.alias) {
            fx.op("agent_stop", json!({"alias": good.alias}));
            assert!(
                !fx.shared.lifecycle.lock().unwrap().owned(&good.alias),
                "the canonical actor survived agent_stop"
            );
        }
        // Bad case: the BAD alias is still unowned; repoint its stored cwd.
        assert!(
            !fx.shared.lifecycle.lock().unwrap().owned(&bad.alias),
            "bad alias already owned before the cwd change"
        );
        // Bad case: the BAD alias is still unowned; repoint its stored cwd.
        assert!(
            !fx.shared.lifecycle.lock().unwrap().owned(&bad.alias),
            "bad alias already owned before the cwd change"
        );
        fx.shared
            .store
            .fixture_write(|tx| {
                tx.execute(
                    "UPDATE agents SET cwd=? WHERE alias=?",
                    rusqlite::params![foreign.to_str().unwrap(), bad.alias],
                )?;
                Ok(())
            })
            .unwrap();
        let err = fx
            .shared
            .try_resume_assistant(&bad.alias)
            .expect_err("resume accepted a foreign principal's cwd");
        assert!(
            err.to_string().contains("principal") && err.to_string().contains("cwd"),
            "foreign cwd refused for another reason: {err}"
        );
        assert!(
            !fx.shared.lifecycle.lock().unwrap().owned(&bad.alias),
            "a refused resume still started an actor"
        );
    }

    /// Rule 2: an ordinary worker reading the global daemon stream must not
    /// receive a member-private `assist_intent` row. The operator tail is the
    /// positive control: the same row IS visible to the operator, so this is
    /// not a blanket deny of the stream.
    #[test]
    fn ordinary_worker_daemon_stream_hides_member_intent() {
        let _flag = AssistantFlagOn::arm();
        let fx = Fx::new();
        with_deferred_service(&fx);
        let rt = fx.member_runtime("sub-tail");
        let req = fx.op(
            "member_job_request",
            json!({"member_envelope": fx.member_envelope("sub-tail"), "title": "tail", "request_key": "t-k1", "body": "b"}),
        );
        let job = req["job"].as_str().unwrap().to_string();
        // Positive control: the operator tail sees the private intent.
        let op_tail = fx
            .op("agent_events", json!({"alias": "daemon", "tail": true}))
            .to_string();
        assert!(
            op_tail.contains("assist_intent") && op_tail.contains(&job),
            "operator daemon tail lost its private intent for {job}: {op_tail}"
        );
        // Positive control on the SAME ordinary read: a public stream row is
        // visible, so the read genuinely succeeded (not a blanket denial).
        fx.shared
            .store
            .event_public("daemon", "wal_checkpointed", json!({"n": 1}))
            .unwrap();
        let worker_tail = fx
            .call(
                Asserted::Agent(rt.worker.clone()),
                "agent_events",
                json!({"alias": "daemon", "tail": true}),
            )
            .unwrap_or_else(|e| panic!("ordinary daemon tail was denied outright: {e}"));
        let worker_text = worker_tail.to_string();
        assert!(
            worker_text.contains("wal_checkpointed"),
            "ordinary daemon tail hid the public stream row: {worker_text}"
        );
        // Bad case: the member's private job/intent must be absent from it.
        assert!(
            !worker_text.contains(&job) && !worker_text.contains("assist_intent"),
            "ordinary worker read member-private assist_intent for job {job}: {worker_text}"
        );
    }

    /// Rule 2 (wait): an ordinary/unproven caller must not wait on a member
    /// assistant alias. The non-member wait is the positive control.
    #[test]
    fn ordinary_worker_cannot_wait_on_member_assistant() {
        let _flag = AssistantFlagOn::arm();
        let fx = Fx::new();
        let rt = fx.member_runtime("sub-wait");
        // Make the ordinary worker's state a real, idle fixture value so a
        // non-member `until:idle` wait has a genuine successful answer.
        fx.shared
            .store
            .set_agent_state(&rt.worker, "idle", None)
            .unwrap();
        // Positive control: the non-member wait returns the actual idle answer.
        let ok = fx
            .call(
                Asserted::Agent(rt.worker.clone()),
                "agent_wait",
                json!({"alias": rt.worker, "until": "idle", "timeout": 0}),
            )
            .unwrap_or_else(|e| panic!("non-member idle wait did not answer: {e}"));
        assert_eq!(ok["reason"], json!("idle"), "non-member wait: {ok}");
        assert_eq!(ok["alias"], json!(rt.worker), "non-member wait: {ok}");
        // Bad case: the same ordinary worker waits on the member assistant. It
        // must be refused by the member PRIVACY/ownership gate, with that
        // specific reason, not a generic timeout, malformed or endpoint error.
        let waited = fx.call(
            Asserted::Agent(rt.worker.clone()),
            "agent_wait",
            json!({"alias": rt.alias, "until": "idle", "timeout": 0}),
        );
        let text = waited
            .as_ref()
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| format!("answered: {:?}", waited));
        assert!(
            waited.is_err()
                && (text.contains("assist")
                    || text.contains("principal")
                    || text.contains("private")
                    || text.contains("own")),
            "member assistant idle wait not refused by the privacy gate: {text}"
        );
        assert!(
            !text.contains("timed out"),
            "member wait refused only by timeout, not the privacy gate: {text}"
        );
    }

    /// Rule 3: a binding revoked while the HTTP dispatch pull is in flight must
    /// refuse the kickoff commit. The pull is held at a deterministic barrier
    /// (no sleep). The revoke commits through the real fenced store. The pull
    /// is then released still answering ALLOW (remote C1 remains valid), so
    /// only the locally committed revocation can refuse the commit.
    #[test]
    fn revoke_during_inflight_dispatch_pull_refuses_kickoff() {
        let _flag = AssistantFlagOn::arm();
        let fx = Fx::new();
        with_deferred_service(&fx);
        let rt = fx.member_runtime("sub-race");
        let join_group = |pm: &str| {
            fx.shared
                .store
                .fixture_write(|tx| {
                    tx.execute(
                        "UPDATE agents SET params=? WHERE alias='ordinary-worker'",
                        rusqlite::params![json!({"upstream": pm}).to_string()],
                    )?;
                    Ok(())
                })
                .unwrap();
        };
        join_group(&rt.alias);
        let req = fx.op(
            "member_job_request",
            json!({"member_envelope": fx.member_envelope("sub-race"), "title": "race", "request_key": "r-k1", "body": "b"}),
        );
        let job = req["job"].as_str().unwrap().to_string();
        let task = format!("{job}-t1");
        let task_before = fx.shared.store.task(&task).unwrap();
        let tree_before = fx.tree();

        // Hold the NEXT revalidate pull (the dispatch's live authority pull).
        fx.authority.lock().unwrap().hold_next = true;
        let dispatch_shared = fx.shared.clone();
        let dispatch_task = task.clone();
        let worker = rt.worker.clone();
        let handle = std::thread::spawn(move || {
            scoped(Asserted::Operator, || {
                dispatch_shared.dispatch(
                    "task_dispatch",
                    &json!({"task": dispatch_task, "to": worker, "by": "operator"}),
                    std::process::id(),
                )
            })
        });
        // Wait until the pull is parked at the barrier. A dispatch that never
        // reaches the live pull is a test failure, not a hang.
        {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut guard = fx.authority.lock().unwrap();
            while !guard.held {
                assert!(
                    Instant::now() < deadline,
                    "dispatch never reached the live revalidate pull (no barrier hit)"
                );
                let (g, _) = fx
                    .barrier
                    .wait_timeout(guard, Duration::from_millis(100))
                    .unwrap_or_else(|e| e.into_inner());
                guard = g;
            }
        }
        // Commit the local revocation through the real fenced store while the
        // remote answer is still in flight.
        assert!(
            fx.shared.store.assistant_revoke(ORG, "sub-race").unwrap(),
            "revoke did not commit"
        );
        // Release the pull; the remote still answers ALLOW.
        fx.release_barrier();
        let result = handle.join().expect("dispatch thread panicked");

        assert!(
            result.is_err(),
            "kickoff committed after a local revocation landed mid-pull: {result:?}"
        );
        assert_eq!(
            fx.shared.store.task(&task).unwrap().revision,
            task_before.revision,
            "a refused kickoff advanced the task revision"
        );
        assert_eq!(
            fx.shared.store.task(&task).unwrap().state,
            task_before.state,
            "a refused kickoff changed the task state"
        );
        assert_eq!(
            fx.tree(),
            tree_before,
            "a refused kickoff left a dispatch effect"
        );
    }

    /// Rule 3 control: the same barrier-held dispatch with NO revoke must
    /// commit the kickoff under live authority. Proves the race refusal above
    /// is a stale-binding refusal, not a blanket dispatch denial.
    #[test]
    fn unrevoked_inflight_dispatch_pull_commits_kickoff() {
        let _flag = AssistantFlagOn::arm();
        let fx = Fx::new();
        with_deferred_service(&fx);
        let rt = fx.member_runtime("sub-ctrl");
        fx.shared
            .store
            .fixture_write(|tx| {
                tx.execute(
                    "UPDATE agents SET params=? WHERE alias='ordinary-worker'",
                    rusqlite::params![json!({"upstream": rt.alias}).to_string()],
                )?;
                Ok(())
            })
            .unwrap();
        let req = fx.op(
            "member_job_request",
            json!({"member_envelope": fx.member_envelope("sub-ctrl"), "title": "ctrl", "request_key": "c-k1", "body": "b"}),
        );
        let job = req["job"].as_str().unwrap().to_string();
        let task = format!("{job}-t1");
        fx.authority.lock().unwrap().hold_next = true;
        let shared = fx.shared.clone();
        let (t, w) = (task.clone(), rt.worker.clone());
        let handle = std::thread::spawn(move || {
            scoped(Asserted::Operator, || {
                shared.dispatch(
                    "task_dispatch",
                    &json!({"task": t, "to": w, "by": "operator"}),
                    std::process::id(),
                )
            })
        });
        {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut guard = fx.authority.lock().unwrap();
            while !guard.held {
                assert!(
                    Instant::now() < deadline,
                    "control never reached the live pull"
                );
                let (g, _) = fx
                    .barrier
                    .wait_timeout(guard, Duration::from_millis(100))
                    .unwrap_or_else(|e| e.into_inner());
                guard = g;
            }
        }
        fx.release_barrier();
        let result = handle.join().expect("control dispatch thread panicked");
        assert!(
            result.is_ok(),
            "unrevoked live dispatch was refused (control broken): {result:?}"
        );
    }

    /// Rule 4 (evidence): the assistant's OWN running turn, reported by id-less
    /// and prefix `ack` through the real handler, succeeds; a wrong token, a
    /// stale generation and a foreign message are each refused for their
    /// specific reason. `ack` mutates only the report field (no outcome) and is
    /// the only valid form for a `pi` managed endpoint (`result` completes the
    /// turn). A registry-minted token is used, never a hand-written one.
    #[test]
    fn own_running_turn_ack_by_idless_and_prefix_is_exact() {
        let _flag = AssistantFlagOn::arm();
        let fx = Fx::new();
        with_deferred_service(&fx);
        let rt = fx.member_runtime("sub-ack");
        let other = fx.member_runtime("sub-ack2");
        let gen = "0123456789abcdef0123456789abcdef";
        let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(gen);
        // Install the agent's current generation and a running turn through
        // the store (the same columns the real endpoint-open path writes).
        fx.shared
            .store
            .fixture_write(|tx| {
                tx.execute(
                    "UPDATE agents SET generation=? WHERE alias=?",
                    rusqlite::params![gen, rt.alias],
                )?;
                Ok(())
            })
            .unwrap();
        fx.shared
            .store
            .enqueue(&rt.alias, "own turn", None, "own-ack-1", "test")
            .unwrap();
        let own_msg = fx
            .shared
            .store
            .messages(&rt.alias)
            .unwrap()
            .into_iter()
            .find(|m| m.body == "own turn")
            .expect("own message not stored")
            .id;
        fx.shared.store.mark_running(&own_msg, &token).unwrap();

        // Bad 1: wrong token for the same running message.
        let wrong = fx.as_assistant(
            &rt.alias,
            "message_report",
            json!({"message": own_msg, "token": "pi-0123456789abcdef0123456789abcdef-ffffffffffffffffffffffffffffffff", "kind": "ack"}),
        );
        let wrong_text = wrong
            .as_ref()
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            wrong.is_err() && wrong_text.contains("Token does not match"),
            "wrong token not refused by the submission-token gate: {wrong:?}"
        );
        // Bad 2: a foreign member's message id, even with its real token.
        let foreign = fx.as_assistant(
            &rt.alias,
            "message_report",
            json!({"message": other.foreign_msg, "kind": "ack"}),
        );
        let foreign_text = foreign
            .as_ref()
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            foreign.is_err() && foreign_text.contains("not its own"),
            "foreign message not refused by ownership: {foreign:?}"
        );
        // Positive: prefix ack of its own running turn, through the real resolver.
        let prefix = &own_msg[..own_msg.len().min(8)];
        let by_prefix = fx
            .as_assistant(
                &rt.alias,
                "message_report",
                json!({"message": prefix, "token": token, "kind": "ack"}),
            )
            .unwrap_or_else(|e| panic!("own prefix ack refused: {e}"));
        assert!(
            by_prefix.is_object(),
            "prefix ack not an answer: {by_prefix}"
        );
        // The ack changes only the report field; the outcome stays running.
        assert_eq!(
            fx.shared.store.message(&own_msg).unwrap().unwrap().state,
            "running",
            "an ack changed the message outcome"
        );
        // Stale generation: the same token under a different stored generation.
        fx.shared
            .store
            .fixture_write(|tx| {
                tx.execute(
                    "UPDATE agents SET generation=? WHERE alias=?",
                    rusqlite::params!["fedcba9876543210fedcba9876543210", rt.alias],
                )?;
                Ok(())
            })
            .unwrap();
        let stale = fx.as_assistant(
            &rt.alias,
            "message_report",
            json!({"message": own_msg, "token": token, "kind": "ack"}),
        );
        let stale_text = stale
            .as_ref()
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            stale.is_err() && stale_text.contains("stale endpoint generation"),
            "stale generation not refused: {stale:?}"
        );
    }

    /// Rule 4: a forged reference to ANOTHER member's message must be refused
    /// by the member policy with an ownership reason, not accepted or passed
    /// through to the generic resolver. Only the refusal shape is pinned; a
    /// valid own-report positive needs a live generation this fixture cannot
    /// construct without a source hook (reported as a scope gap).
    #[test]
    fn forged_foreign_message_report_refused_by_ownership() {
        let _flag = AssistantFlagOn::arm();
        let fx = Fx::new();
        with_deferred_service(&fx);
        let rt = fx.member_runtime("sub-rep");
        let other = fx.member_runtime("sub-rep2");
        let forged = fx.as_assistant(
            &rt.alias,
            "message_report",
            json!({"message": other.foreign_msg, "kind": "ack"}),
        );
        let text = forged
            .as_ref()
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            forged.is_err() && text.contains("not its own"),
            "forged foreign message_report not refused by ownership: {forged:?}"
        );
    }
}

// ============================================================================
// CAD-1420 bootstrap ACK caller guard (independent acceptance, one bad case
// plus one owner control). Authored by the acceptance maintainer from the
// decision note, not by the source writer. Uses the REAL fixtures: a live
// member's stamped `member_assist_send`, the real `take_app_aware`, a
// registry-minted current turn token and `Asserted::Agent` callers.
// ============================================================================

/// Synthetic quiescent guard fixture (NOT a first-send or provider proof). The
/// victim's stamped running turn is written by the production stamp writer
/// `enqueue_sent` with `Sender::VerifiedMember` and the fixture's recorded
/// original credential (`cred-<sub>`). No send RPC, no actor launch, no signed
/// first-send admission. One registry-minted current token stays `running` for
/// the whole sequence; `turn_token_current` is asserted before any ACK.
///
/// Bad case (first, against a pending rev 0 snapshot): a foreign member carrying
/// the victim's exact running message and current token is refused as "not its
/// own"; bootstrap DB state/revision, all five private files under the principal
/// root and the PM tree are byte-identical before and after.
///
/// Owner lifecycle on the same turn (controlled revisions, not fixed numbers):
/// defer at rev 0 -> deferred; resume at the returned rev -> pending with the
/// original BOOTSTRAP body in context; complete with the agreed profile -> complete,
/// profile values written, BOOTSTRAP removed. A user-owned note outside the
/// managed `preferences` section survives completion. After completion the
/// existing member_assist_state reuses the assistant without starting an actor,
/// BOOTSTRAP is not recreated, and `member_profile_context` carries the agreed
/// values with no pending guide.
#[test]
fn bootstrap_ack_by_foreign_caller_cannot_apply_with_valid_victim_turn() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let victim_sub = "sub-boot-victim";
    let victim = fx.member_runtime(victim_sub);
    let thief = fx.member_runtime("sub-boot-thief");
    let victim_binding = fx
        .shared
        .store
        .assistant_binding(ORG, victim_sub)
        .unwrap()
        .expect("victim binding missing");
    let cwd = fx.shared.assistant_principal_cwd(ORG, victim_sub);
    const FILES: [&str; 5] = [
        "BOOTSTRAP.md",
        "IDENTITY.md",
        "SOUL.md",
        "USER.md",
        "AGENTS.md",
    ];
    let files = |cwd: &Path| -> BTreeMap<String, Option<Vec<u8>>> {
        FILES
            .into_iter()
            .map(|n| (n.to_string(), std::fs::read(cwd.join(n)).ok()))
            .collect()
    };
    let db = |fx: &Fx| {
        let b = fx
            .shared
            .store
            .assistant_binding(ORG, victim_sub)
            .unwrap()
            .unwrap();
        (b.bootstrap_state, b.bootstrap_revision)
    };

    // Synthetic stamped running turn (see the doc): production stamp writer,
    // original fixture credential, generation and state set by fixture_write.
    let gen = "0123456789abcdef0123456789abcdef";
    let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(gen);
    let msg_id = "boot-victim-msg-1";
    fx.shared
        .store
        .enqueue_sent(
            &victim.alias,
            "boot turn",
            None,
            msg_id,
            &format!("member-{victim_sub}"),
            None,
            &crate::store::Sender::VerifiedMember(crate::store::MemberAuthority {
                org: ORG.to_string(),
                sub: victim_sub.to_string(),
                assistant_id: victim_binding.assistant_id.clone(),
                credential_id: format!("cred-{victim_sub}"),
            }),
        )
        .unwrap();
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET generation=?, state='idle', params=? WHERE alias=?",
                rusqlite::params![
                    gen,
                    json!({"report_timeout_secs": 3600}).to_string(),
                    victim.alias
                ],
            )?;
            Ok(())
        })
        .unwrap();
    fx.shared.store.mark_running(msg_id, &token).unwrap();
    let agent = fx.shared.store.agent(&victim.alias).unwrap();
    assert!(
        crate::adapter::registry::turn_token_current(
            &agent.provider,
            &agent.endpoint_kind,
            agent.generation.as_deref(),
            &token,
        ),
        "fixture token is not current for the stored provider/endpoint/generation"
    );
    let running = fx.shared.store.message(msg_id).unwrap().unwrap();
    assert_eq!(running.state, "running", "victim turn is not running");
    assert_eq!(running.turn_id.as_deref(), Some(token.as_str()));

    // Pending rev 0 snapshot: BOOTSTRAP present, user-owned note outside the
    // managed preferences section added before the snapshot.
    let before_files = files(&cwd);
    assert!(
        before_files["BOOTSTRAP.md"].is_some(),
        "fixture seeded no BOOTSTRAP.md"
    );
    let user_path = cwd.join("USER.md");
    let user_note = "\nUser note: keep this line.\n";
    let user_text = std::fs::read_to_string(&user_path).unwrap();
    std::fs::write(&user_path, format!("{user_text}{user_note}")).unwrap();
    let before_files = files(&cwd);
    let before_db = db(&fx);
    assert_eq!(before_db.0, "pending", "fixture binding is not pending");
    let before_tree = fx.tree();

    // Bad case: foreign connection, victim's exact running message and token.
    let stolen = fx.as_assistant(
        &thief.alias,
        "message_report",
        json!({"message": msg_id, "token": token, "kind": "ack",
               "bootstrap": {"action": "complete", "expected_revision": before_db.1,
                             "profile": {"name": "Stolen", "tone": "stolen", "preferences": "stolen"}}}),
    );
    let stolen_text = stolen
        .as_ref()
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        stolen.is_err() && stolen_text.contains("not its own"),
        "foreign caller's bootstrap ACK refused for another reason: {stolen:?}"
    );
    assert_eq!(
        db(&fx),
        before_db,
        "refused foreign ACK changed bootstrap DB fields"
    );
    assert_eq!(
        files(&cwd),
        before_files,
        "refused foreign ACK changed private files"
    );
    assert_eq!(
        fx.tree(),
        before_tree,
        "refused foreign ACK changed the PM tree"
    );

    // Wrong-derived caller: a proven operator connection carrying the victim's
    // valid running message and current token is not the assistant's own
    // connection, so the changed CAD-1420 caller branch refuses it with its
    // exact reason. Nothing mutates.
    let operator_ack = fx.op_err(
        "message_report",
        json!({"message": msg_id, "token": token, "kind": "ack",
               "bootstrap": {"action": "complete", "expected_revision": before_db.1,
                             "profile": {"name": "Operator", "tone": "operator", "preferences": "operator"}}}),
    );
    let operator_text = operator_ack
        .as_ref()
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(
        operator_ack.is_err()
            && operator_text
                .contains("bootstrap ack refused: it needs the assistant's own connection"),
        "operator-derived bootstrap ACK refused for another reason: {operator_ack:?}"
    );
    assert_eq!(
        db(&fx),
        before_db,
        "refused operator-derived ACK changed bootstrap DB fields"
    );
    assert_eq!(
        files(&cwd),
        before_files,
        "refused operator-derived ACK changed private files"
    );
    assert_eq!(
        fx.tree(),
        before_tree,
        "refused operator-derived ACK changed the PM tree"
    );

    let ack = |fx: &Fx, bootstrap: Value| {
        fx.as_assistant(
            &victim.alias,
            "message_report",
            json!({"message": msg_id, "token": token, "kind": "ack", "bootstrap": bootstrap}),
        )
    };
    let rev0 = db(&fx).1;

    // Owner: defer at the stored revision.
    let deferred = ack(&fx, json!({"action": "defer", "expected_revision": rev0}))
        .unwrap_or_else(|e| panic!("owner defer refused: {e}"));
    assert!(deferred.is_object(), "defer not an answer: {deferred}");
    let (state, rev1) = db(&fx);
    assert_eq!(state, "deferred", "defer did not record deferred");
    assert!(rev1 > rev0, "defer did not advance the revision");
    let ctx = fx
        .shared
        .member_profile_context(&running)
        .unwrap()
        .unwrap_or_default();
    assert!(
        ctx.contains("state: deferred"),
        "deferred context lacks state: {ctx}"
    );
    assert!(
        ctx.contains(&format!("revision: {rev1}")),
        "deferred context lacks revision: {ctx}"
    );
    assert!(
        !ctx.contains("## Bootstrap (pending)"),
        "deferred context carried the pending guide"
    );

    // Owner: resume at the returned revision.
    let resumed = ack(&fx, json!({"action": "resume", "expected_revision": rev1}))
        .unwrap_or_else(|e| panic!("owner resume refused: {e}"));
    assert!(resumed.is_object(), "resume not an answer: {resumed}");
    let (state, rev2) = db(&fx);
    assert_eq!(state, "pending", "resume did not record pending");
    assert!(rev2 > rev1, "resume did not advance the revision");
    let ctx = fx
        .shared
        .member_profile_context(&running)
        .unwrap()
        .unwrap_or_default();
    assert!(
        ctx.contains("## Bootstrap (pending)"),
        "resumed context lacks the pending guide"
    );
    assert!(
        ctx.contains(&format!("revision: {rev2}")),
        "resumed context lacks revision: {ctx}"
    );

    // Owner: complete with the agreed profile at the returned revision.
    let completed = ack(
        &fx,
        json!({"action": "complete", "expected_revision": rev2,
               "profile": {"name": "Agreed name", "tone": "concise", "preferences": "short answers"}}),
    )
    .unwrap_or_else(|e| panic!("owner complete refused (control broken): {e}"));
    assert!(completed.is_object(), "complete not an answer: {completed}");
    let (state, rev3) = db(&fx);
    assert_eq!(
        state, "complete",
        "owner complete did not record completion"
    );
    assert!(rev3 > rev2, "complete did not advance the revision");

    let after = files(&cwd);
    let text = |name: &str| {
        after
            .get(name)
            .and_then(|b| b.as_ref())
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default()
    };
    assert!(
        text("IDENTITY.md").contains("Agreed name"),
        "IDENTITY.md lacks the agreed name"
    );
    assert!(
        text("SOUL.md").contains("concise"),
        "SOUL.md lacks the agreed tone"
    );
    assert!(
        text("USER.md").contains("short answers"),
        "USER.md lacks the agreed preferences"
    );
    assert!(
        text("USER.md").contains("User note: keep this line."),
        "user-owned USER.md note was lost"
    );
    assert!(
        after["BOOTSTRAP.md"].is_none(),
        "BOOTSTRAP.md survived completion"
    );

    // Post-complete: a fresh signed member_assist_state reuses the bound
    // assistant without an actor start, does not recreate BOOTSTRAP, and keeps
    // the profile text.
    let state_read = fx.op(
        "member_assist_state",
        json!({"member_envelope": fx.member_envelope(victim_sub)}),
    );
    assert_eq!(
        state_read["assistant"]["alias"].as_str(),
        Some(victim.alias.as_str()),
        "post-complete state read did not reuse the assistant alias"
    );
    assert!(
        files(&cwd)["BOOTSTRAP.md"].is_none(),
        "BOOTSTRAP was reseeded after completion"
    );
    let after_read = files(&cwd);
    assert_eq!(
        after_read["IDENTITY.md"], after["IDENTITY.md"],
        "IDENTITY changed on a read"
    );
    assert_eq!(
        after_read["SOUL.md"], after["SOUL.md"],
        "SOUL changed on a read"
    );
    assert_eq!(
        after_read["USER.md"], after["USER.md"],
        "USER changed on a read"
    );

    let ctx = fx
        .shared
        .member_profile_context(&running)
        .unwrap()
        .unwrap_or_default();
    assert!(
        ctx.contains("Agreed name") && ctx.contains("concise") && ctx.contains("short answers"),
        "complete context lacks the agreed values: {ctx}"
    );
    assert!(
        ctx.contains("state: complete"),
        "complete context lacks state: {ctx}"
    );
    assert!(
        ctx.contains(&format!("revision: {rev3}")),
        "complete context lacks revision: {ctx}"
    );
    assert!(
        !ctx.contains("## Bootstrap (pending)"),
        "complete context carried the pending guide"
    );
}

// ============================================================================
// CAD-1420 independent refusal acceptance — `platform_assistant_provision`,
// the dedicated platform-authority projection, authored by a worker that is
// NOT the implementer. The guard contract under test (from the ticket, not
// the implementation): a genuinely absent `assistants` row may be projected
// ONLY when (a) the caller is the proven operator/board relay, (b) the
// signed `member_envelope` cryptographically re-verifies, (c) the daemon's
// `hosted-board` `assist.revalidate` service credential is enrolled through
// the NORMAL `platform_enroll` operator path for THIS board's configured
// host, and (d) the live upstream pull re-allows the member's ORIGINAL
// issuing credential for the exact operation + scope. A revoked/stale
// original CID denies at the LIVE pull — before any binding row, agent,
// thread or private-cwd file exists. The fixture platform is synthetic; it
// proves the daemon honors the real guard, never a live product R1.
// ============================================================================

/// The native HTTP `assist_*` surface on a real owned daemon + real board.
/// `state` is the daemon's own dir; `Fx` supplies ONLY the loopback
/// platform (JWKS + revalidate), the signing key and member authority —
/// the same two-server layout the route runs in production.
struct ProvisionHttpRig {
    _root: tempfile::TempDir,
    state: PathBuf,
    pm: PathBuf,
    board_port: u16,
    /// The board's public host — the platform's `localhost:<port>`,
    /// carried so `call` names it in the Host header.
    public_host: String,
    stop: Arc<AtomicBool>,
    _threads: Vec<std::thread::JoinHandle<()>>,
}

impl ProvisionHttpRig {
    /// Boot a REAL daemon (`serve_with`, test-seam armed) on its own temp
    /// state + PM, then a REAL `crate::ui::serve` board whose public host
    /// names the fixture platform. `fx`'s platform is the authority for
    /// both the route's envelope verify and the daemon's live pull.
    fn start(fx: &Fx) -> Self {
        let root = tempfile::Builder::new()
            .prefix("c1420-http")
            .tempdir()
            .unwrap();
        let state = root.path().join("s");
        let pm = root.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        // The same trust root `Fx` uses: the loopback platform's host/issuer.
        crate::board_identity::write_config(
            &state,
            &crate::board_identity::Config {
                host: fx.host.clone(),
                issuer: fx.issuer.clone(),
                company: ORG.to_string(),
            },
        )
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();

        // Real daemon on a real socket; the PM dir is the fixture's own.
        {
            let dir = state.clone();
            let opts = ServeOptions {
                test_seam: true,
                stop: Some(stop.clone()),
                ..ServeOptions::default()
            };
            opts.provider_env
                .set("CADENCE_PM_DIR", pm.to_str().unwrap());
            // `.expect` inside the thread keeps a daemon startup failure
            // observable as a joinable panic rather than a dropped Result.
            threads.push(std::thread::spawn(move || {
                crate::daemon::serve_with(&dir, opts).expect("owned test daemon failed");
            }));
            let deadline = std::time::Instant::now() + Duration::from_secs(30);
            while !state.join("cadence.sock").exists()
                || crate::test_seam::Seam::token_at(&state).is_none()
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "owned daemon never started"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        // Real board with a public identity so `/api/assist/*` diverts —
        // the same surface the platform relay forwards to.
        let platform_port: u16 = fx
            .host
            .rsplit(':')
            .next()
            .unwrap_or_default()
            .parse()
            .unwrap_or(0);
        let mut board_port = 0u16;
        for port in 3110..=3199u16 {
            if port == platform_port {
                continue;
            }
            let (startup, ready) = std::sync::mpsc::channel();
            let board = crate::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(stop.clone()),
                startup: Some(startup),
                test_seam: true,
                allow_hosts: vec![fx.host.clone()],
                public: Some(crate::ui::PublicBoard {
                    host: fx.host.clone(),
                    issuer: fx.issuer.clone(),
                    company: ORG.into(),
                    authorize_url: format!("{}/authorize", fx.issuer),
                }),
                ..crate::ui::ServeOpts::default()
            };
            let (s, p) = (state.clone(), pm.clone());
            let thread = std::thread::spawn(move || drop(crate::ui::serve(&s, &p, &board)));
            match ready.recv_timeout(Duration::from_secs(30)).unwrap() {
                Ok(()) => {
                    board_port = port;
                    threads.push(thread);
                    break;
                }
                Err(_) => {
                    thread.join().unwrap();
                }
            }
        }
        assert!(board_port > 0, "board could not bind in 3110..=3199");
        Self {
            _root: root,
            state,
            pm,
            board_port,
            public_host: fx.host.clone(),
            stop,
            _threads: threads,
        }
    }

    /// `POST /api/assist/<verb>` on the board's public host → (status, body).
    fn call(&self, verb: &str, envelope: &str, args: &Value) -> (u16, Value) {
        let body = serde_json::to_vec(args).unwrap();
        let mut socket = std::net::TcpStream::connect(("127.0.0.1", self.board_port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        use std::io::Write as _;
        write!(
            socket,
            "POST /api/assist/{verb} HTTP/1.1\r\nHost: {}\r\n\
             Authorization: Bearer wikienv_{envelope}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            self.public_host,
            body.len()
        )
        .unwrap();
        socket.write_all(&body).unwrap();
        let mut response = Vec::new();
        std::io::Read::read_to_end(&mut socket, &mut response).unwrap();
        let split = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("no http header/body split");
        let header = std::str::from_utf8(&response[..split]).unwrap();
        let status = header
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let value = serde_json::from_slice(&response[split + 4..]).unwrap_or_else(
            |_| json!({"_nonjson": String::from_utf8_lossy(&response[split+4..]).into_owned()}),
        );
        (status, value)
    }

    /// The daemon's own `client::rpc` on the real socket — the same path
    /// the board's relay uses. `who` is asserted through the test seam.
    fn rpc_as(&self, who: Asserted, method: &str, params: Value) -> Result<Value> {
        crate::test_seam::scoped(who, || crate::client::rpc(&self.state, method, params))
    }

    /// A `Shared` opened alongside the live daemon — the second handle the
    /// HTTP refusal module already uses for reads against the owned state.
    fn store(&self) -> Arc<Shared> {
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", self.pm.to_str().unwrap());
        crate::daemon::Shared::new(&self.state, &opts).unwrap()
    }

    /// Whole-PM-tree byte snapshot — the "no effect" probe for refused calls.
    fn tree(&self) -> BTreeMap<String, Vec<u8>> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                if rel == ".git" {
                    continue;
                }
                if p.is_dir() {
                    out.insert(format!("{rel}/"), vec![]);
                    walk(base, &p, out);
                } else {
                    out.insert(rel, std::fs::read(&p).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.pm, &self.pm, &mut out);
        out
    }
}

impl Drop for ProvisionHttpRig {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Assert that NOTHING was projected for `sub`: no `assistants` row, no
/// registered agent under the server-derived alias, no bound thread and no
/// private-cwd directory under `state_dir/assistants/<digest>`.
fn assert_nothing_projected(shared: &Shared, org: &str, sub: &str, case: &str) {
    assert!(
        shared.store.assistant_binding(org, sub).unwrap().is_none(),
        "{case}: a refused projection left an assistants row"
    );
    let alias = shared.assistant_principal_alias(org, sub);
    assert!(
        shared.store.agent_opt(&alias).unwrap().is_none(),
        "{case}: a refused projection registered agent '{alias}'"
    );
    assert!(
        shared.store.thread(&alias).unwrap().is_none(),
        "{case}: a refused projection bound a thread for '{alias}'"
    );
    assert!(
        !shared.assistant_principal_cwd(org, sub).exists(),
        "{case}: a refused projection created the member's private cwd"
    );
}

/// CAD-1420 acceptance: a genuinely absent binding may be projected ONLY
/// through the live-authority path. A fresh member whose ORIGINAL issuing
/// credential is dead at the platform is denied at the LIVE pull on BOTH
/// the direct `platform_assistant_provision` RPC and the real
/// `/api/assist/<verb>` HTTP surface — with NO `assistants` row, NO
/// registered agent, NO bound thread and NO private-cwd files. The
/// enrolled `hosted-board` service credential goes through the NORMAL
/// `platform_enroll` operator RPC — never a raw SQL/custody seed — so a
/// missing or wrongly-scoped service cannot masquerade as a live denial.
/// Every refused case pairs with an authorized control in the same
/// fixture so a blanket-fail cannot pass. The fixture platform is
/// synthetic — it exercises the real guard, never a live product R1.
#[test]
fn platform_provision_live_denial_creates_no_binding_or_files() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();

    // ---- Enrollment guards: the new normal `hosted-board` enroll path
    // must not be unguarded. A wrong host and a broadened scope each
    // refuse BEFORE any custody record lands — the account is pinned to
    // this board's configured host and the declared scope set is exactly
    // `assist.revalidate`. `accept_same_uid_risk` is the test-only
    // fixture operator's consent for same-uid custody (ADR 0006 P4), not
    // a live human approval.
    let service_token = format!("hcs_{}", "f".repeat(43));
    for (label, params) in [
        (
            "wrong host",
            json!({"platform": "hosted-board", "account": "other-board.example",
                   "scopes": ["assist.revalidate"], "token": service_token,
                   "accept_same_uid_risk": true}),
        ),
        (
            "broadened scope",
            json!({"platform": "hosted-board", "account": fx.host.clone(),
                   "scopes": ["assist.revalidate", "results.submit"], "token": service_token,
                   "accept_same_uid_risk": true}),
        ),
    ] {
        let before = fx.tree();
        let e = err_text(
            fx.op_err("platform_enroll", params),
            &format!("hosted-board enroll {label}"),
        );
        assert!(
            !e.is_empty(),
            "hosted-board enroll {label} produced an empty refusal"
        );
        assert_eq!(fx.tree(), before, "a refused {label} enroll had an effect");
    }
    // A refused enroll leaves no loadable record.
    assert!(
        fx.shared
            .store
            .platform_credential("hosted-board", &fx.host)
            .unwrap()
            .is_none(),
        "a refused hosted-board enroll left a credential record"
    );

    // ---- NORMAL enrollment: the real `platform_enroll` operator RPC for
    // THIS board's configured host with exactly `assist.revalidate`. ----
    let enrolled = fx.op_err(
        "platform_enroll",
        json!({"platform": "hosted-board", "account": fx.host.clone(),
               "scopes": ["assist.revalidate"], "token": service_token,
               "accept_same_uid_risk": true}),
    );
    assert!(
        enrolled.is_ok(),
        "the normal hosted-board enroll refused: {enrolled:?}"
    );
    // The fixture platform recognizes the enrolled service bearer.
    fx.authority
        .lock()
        .unwrap()
        .services
        .push(ServiceAuthority {
            token: service_token.clone(),
            org: ORG.to_string(),
            audience: format!("http://{}", fx.host),
            caps: vec!["assist.revalidate".into()],
            live: true,
        });

    // ---- POSITIVE CONTROL (direct RPC): a live member with a live
    // ORIGINAL credential projects exactly one server-derived binding. ----
    let sub = "sub-new";
    fx.authority
        .lock()
        .unwrap()
        .members
        .insert(sub.to_string(), MemberAuthority::live(sub));
    let env = fx.envelope(sub, json!({}), json!({}));
    let ok = fx
        .op_err(
            "platform_assistant_provision",
            json!({"member_envelope": env, "operation": "assist_state"}),
        )
        .unwrap_or_else(|e| panic!("live member's projection refused: {e}"));
    assert_eq!(
        ok["provisioned"].as_bool(),
        Some(true),
        "live member was not projected: {ok}"
    );
    let binding = fx
        .shared
        .store
        .assistant_binding(ORG, sub)
        .unwrap()
        .expect("live member produced no binding");
    assert_eq!(binding.org, ORG);
    assert_eq!(binding.sub, sub);
    assert_eq!(binding.assistant_id, format!("assist_{sub}"));
    assert!(
        binding.alias.starts_with("assist-") && binding.alias.len() == 55,
        "binding alias is not the server-derived digest form: {}",
        binding.alias
    );
    assert!(
        fx.shared.store.agent_opt(&binding.alias).unwrap().is_none(),
        "the projection registered an agent"
    );
    assert!(
        !fx.shared.assistant_principal_cwd(ORG, sub).exists(),
        "the projection created the member's private cwd"
    );

    // ---- LIVE DENIAL (direct RPC): the ORIGINAL issuing credential is
    // dead upstream. The member is still listed and the envelope still
    // verifies, so ONLY the live pull's `credential_id` re-prove can
    // refuse — this is never a missing-enrollment or envelope failure.
    fx.authority
        .lock()
        .unwrap()
        .members
        .insert("sub-dead".to_string(), MemberAuthority::live("sub-dead"));
    fx.set_member("sub-dead", |m| m.credential_live = false);
    let dead_env = fx.envelope("sub-dead", json!({}), json!({}));
    let before = fx.tree();
    let e = err_text(
        fx.op_err(
            "platform_assistant_provision",
            json!({"member_envelope": dead_env, "operation": "assist_state"}),
        ),
        "platform_assistant_provision with a dead original credential",
    );
    // The exact live-guard refusal — not a custody, scope, binding or
    // envelope failure masquerading as revocation.
    assert!(
        e.contains("the platform revalidation did not allow"),
        "dead-credential projection refused outside the live guard: {e}"
    );
    assert_nothing_projected(&fx.shared, ORG, "sub-dead", "direct RPC denial");
    assert_eq!(
        fx.tree(),
        before,
        "a refused projection changed the tracker"
    );

    // ---- LIVE DENIAL (HTTP): the real `/api/assist/*` surface on an
    // owned daemon + board. The rig's daemon enrolls the same service
    // credential through the NORMAL `platform_enroll` over its real
    // socket, asserted through the test seam as the operator.
    let http_fx = Fx::new();
    let rig = ProvisionHttpRig::start(&http_fx);
    let rig_token = format!("hcs_{}", "g".repeat(43));
    let enrolled = rig.rpc_as(
        Asserted::Operator,
        "platform_enroll",
        json!({"platform": "hosted-board", "account": http_fx.host.clone(),
               "scopes": ["assist.revalidate"], "token": rig_token,
               "accept_same_uid_risk": true}),
    );
    assert!(
        enrolled.is_ok(),
        "the rig daemon's normal hosted-board enroll refused: {enrolled:?}"
    );
    http_fx
        .authority
        .lock()
        .unwrap()
        .services
        .push(ServiceAuthority {
            token: rig_token,
            org: ORG.to_string(),
            audience: format!("http://{}", http_fx.host),
            caps: vec!["assist.revalidate".into()],
            live: true,
        });

    // Closed-arg guard with an otherwise-authorized LIVE member: an
    // unknown POST field refuses BEFORE any projection — a malformed
    // call never reaches the RPC. A fresh envelope each call: the HTTP
    // route consumes the jti before arg validation, so reusing one would
    // only re-test replay, not the guard under test.
    http_fx.authority.lock().unwrap().members.insert(
        "sub-httpok".to_string(),
        MemberAuthority::live("sub-httpok"),
    );
    let http_before = rig.tree();
    let (status, body) = rig.call(
        "assist_state",
        &http_fx.envelope("sub-httpok", json!({}), json!({})),
        &json!({"bogus": true}),
    );
    assert_eq!(
        status, 400,
        "an unknown-arg assist_state was not refused at HTTP: {body}"
    );
    assert_nothing_projected(&rig.store(), ORG, "sub-httpok", "unknown-arg refusal");
    assert_eq!(rig.tree(), http_before, "an unknown-arg call had an effect");

    // Positive control: a FRESH envelope for the same live member admits —
    // proving the route, the projection and the member verb all pass.
    let (status, body) = rig.call(
        "assist_state",
        &http_fx.envelope("sub-httpok", json!({}), json!({})),
        &json!({}),
    );
    assert_eq!(
        status, 200,
        "a live member's assist_state was refused at HTTP: {body}"
    );
    let binding = rig
        .store()
        .store
        .assistant_binding(ORG, "sub-httpok")
        .unwrap()
        .expect("a live member's HTTP call created no binding");
    assert_eq!(binding.assistant_id, "assist_sub-httpok");

    // The live denial through the SAME HTTP surface: a fresh envelope for
    // a member whose original credential is dead upstream must be refused
    // at the live pull — before any binding, agent, thread or file.
    http_fx.authority.lock().unwrap().members.insert(
        "sub-httpdead".to_string(),
        MemberAuthority::live("sub-httpdead"),
    );
    http_fx.set_member("sub-httpdead", |m| m.credential_live = false);
    let http_before = rig.tree();
    let (status, body) = rig.call(
        "assist_state",
        &http_fx.envelope("sub-httpdead", json!({}), json!({})),
        &json!({}),
    );
    assert_eq!(
        status, 403,
        "a dead-credential assist_state was not refused 403 at HTTP: {body}"
    );
    // The HTTP refusal names the live-guard denial, not a bare 403.
    let message = body["error"]["message"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .unwrap_or_default();
    assert!(
        message.contains("the platform revalidation did not allow"),
        "the HTTP refusal did not name the live-guard denial: {body}"
    );
    assert_nothing_projected(&rig.store(), ORG, "sub-httpdead", "HTTP denial");
    assert_eq!(rig.tree(), http_before, "a refused HTTP call had an effect");

    // A valid-shape `assist_send` for a SECOND dead-credential member —
    // the same live denial on the write operation's surface.
    http_fx.authority.lock().unwrap().members.insert(
        "sub-httpdead2".to_string(),
        MemberAuthority::live("sub-httpdead2"),
    );
    http_fx.set_member("sub-httpdead2", |m| m.credential_live = false);
    let http_before = rig.tree();
    let (status, body) = rig.call(
        "assist_send",
        &http_fx.envelope("sub-httpdead2", json!({}), json!({})),
        &json!({"text": "still refused"}),
    );
    assert_eq!(
        status, 403,
        "a dead-credential assist_send was not refused 403 at HTTP: {body}"
    );
    let message = body["error"]["message"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .unwrap_or_default();
    assert!(
        message.contains("the platform revalidation did not allow"),
        "the HTTP send refusal did not name the live-guard denial: {body}"
    );
    assert_nothing_projected(&rig.store(), ORG, "sub-httpdead2", "HTTP send denial");
    assert_eq!(rig.tree(), http_before, "a refused HTTP send had an effect");

    // ---- Direct-RPC controls on the rig daemon: a non-operator caller
    // and a read-scope-only envelope are refused before the live pull. ----
    let rig_shared = rig.store();
    let nonop = rig.rpc_as(
        Asserted::Agent("writer".into()),
        "platform_assistant_provision",
        json!({"member_envelope": http_fx.envelope("sub-httpok", json!({}), json!({})),
               "operation": "assist_state"}),
    );
    assert!(
        nonop.is_err(),
        "an agent caller reached platform_assistant_provision: {nonop:?}"
    );
    let readonly = http_fx.envelope("sub-httpok", json!({}), json!({"scope": ["assist.read"]}));
    let e = err_text(
        rig.rpc_as(
            Asserted::Operator,
            "platform_assistant_provision",
            json!({"member_envelope": readonly, "operation": "assist_send"}),
        ),
        "read-scope projection of a write operation",
    );
    assert!(
        e.contains("scope") || e.contains("refused") || e.contains("assist."),
        "a read-scope projection of assist_send refused for another reason: {e}"
    );
    let _ = rig_shared;
}

// CAD-1568 second-signer guard acceptance: the child-agent principal boundary
// for remote CLI messaging, authored independently of the implementer and the
// ticket author's check above.
// CAD-1817 Phase 1 identity/profile refusal and operator-write acceptance.

/// CAD-1726 acceptance (reviewer-written, not the implementer's): "Hosted:
/// ... a member sees no card in `MemberChat` and is refused by the read
/// route." A real artifact card (written by the store's own put path) sits
/// between two ordinary replies in the member's bound conversation. The
/// operator's view of that thread holds the card (control); the member's
/// `member_assist_history` returns both replies and never the card, on the
/// forward branch and on both paged branches (`tail`, `before`). The
/// member's assistant identity and a member envelope are refused by
/// `artifact_read`.
///
/// Guards: `operator_only_entries_hidden` in `rpc_member_assist_history`
/// (daemon/members_rpc.rs); `artifact_operator` + `reject_artifact_fields`
/// (daemon/artifacts_rpc.rs).
#[test]
fn cad1726_member_history_never_carries_an_artifact_card() {
    use crate::store::{ArtifactStore, LocalArtifacts, NewEntry, PutArtifact};
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-ana");
    let reply = |text: &str| {
        fx.shared
            .store
            .thread_append_to(
                &rt.thread_id,
                NewEntry {
                    role: "agent",
                    kind: "assistant_text",
                    text,
                    payload: None,
                    message_id: None,
                },
            )
            .unwrap()
            .expect("the member's bound thread exists")
    };
    reply("visible reply one");
    let card = LocalArtifacts {
        store: &fx.shared.store,
        state_dir: &fx.shared.state_dir,
    }
    .put(
        PutArtifact {
            id: None,
            kind: "md",
            title: "Operator-only canvas",
            thread_id: &rt.thread_id,
            scope: "home",
            context_id: "",
            created_by: "master",
            message_id: "m-cad1726-member",
        },
        b"# operator only",
    )
    .unwrap();
    let last = reply("visible reply two");

    // Control: the operator's view of the same thread holds the card.
    let all = fx
        .shared
        .store
        .thread_entries_of(&rt.thread_id, 0, 100)
        .unwrap();
    assert!(
        all.iter().any(|e| e.kind == "artifact"
            && e.payload
                .as_ref()
                .is_some_and(|p| p["id"] == json!(card.id))),
        "the card was not written to the member's thread"
    );

    let env = fx.member_envelope("sub-ana");
    for (branch, params) in [
        ("forward", json!({"member_envelope": env, "after": 0})),
        ("tail", json!({"member_envelope": env, "tail": true})),
        (
            "before",
            json!({"member_envelope": env, "before": last + 1}),
        ),
    ] {
        let page = fx.op("member_assist_history", params);
        assert_eq!(
            page["conversation"]["id"],
            json!(rt.thread_id),
            "{branch}: {page}"
        );
        let entries = page["entries"].as_array().unwrap();
        let texts: Vec<&str> = entries.iter().filter_map(|e| e["text"].as_str()).collect();
        assert!(
            texts.contains(&"visible reply one") && texts.contains(&"visible reply two"),
            "{branch}: the member lost an ordinary entry: {page}"
        );
        assert!(
            entries.iter().all(|e| e["kind"] != json!("artifact")),
            "{branch}: a member received an artifact card: {page}"
        );
        let raw = page.to_string();
        assert!(
            !raw.contains(&card.id) && !raw.contains("Operator-only canvas"),
            "{branch}: the card leaked into the member page: {raw}"
        );
    }

    // The member is refused by the read route: as its assistant agent,
    // and with its envelope on the operator channel.
    let err = err_text(
        fx.as_assistant(
            &rt.alias,
            "artifact_read",
            json!({"id": card.id, "version": 1, "conversation": rt.thread_id}),
        ),
        "assistant artifact_read",
    );
    // The member boundary refuses first (the assist agent's surface is
    // its own conversation); the artifact handler would refuse an agent too.
    assert!(
        err.contains("may not call 'artifact_read'") && err.contains("member boundary"),
        "assistant artifact_read: {err}"
    );
    let err = err_text(
        fx.op_err(
            "artifact_read",
            json!({"member_envelope": env, "id": card.id, "version": 1,
                   "conversation": rt.thread_id}),
        ),
        "member envelope artifact_read",
    );
    assert!(
        err.contains("'member_envelope' is not accepted"),
        "member envelope artifact_read: {err}"
    );
}

// ============================================================================
// CAD-2207 independent negative acceptance (second author): dispatch routing,
// durable room authorship after an agent-row removal, the room membership and
// verified-human agent-list guards, and the private effect read/decision path.
//
// Every check drives the real daemon dispatch/store path under the real
// `test-seam` caller identities. Each negative is paired with a positive
// control that proves the same verb/envelope/path still works for an ordinary
// agent or an authorized member, so a blanket error cannot pass. No check
// asserts refusal wording: only side effects (queued rows, stored posts,
// durable effect state) decide.
// ============================================================================

/// Run `git` in `dir`; a fixture failure is not a guard failure.
fn cad2207_git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .env("GIT_AUTHOR_NAME", "cad2207")
        .env("GIT_AUTHOR_EMAIL", "cad2207@example.invalid")
        .env("GIT_COMMITTER_NAME", "cad2207")
        .env("GIT_COMMITTER_EMAIL", "cad2207@example.invalid")
        .args(args)
        .output()
        .expect("git unavailable");
    assert!(
        out.status.success(),
        "fixture git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A real eligible dispatch fixture: a git repo with a registered live lane
/// (`<repo>/.cadence/wt/<lane>`), a project declaring it, an issue whose open
/// `worktree` ref names that lane, and a `doing` claim held by the ordinary
/// worker. `dispatch_send` resolves every one of those itself; nothing here
/// mirrors its guards.
struct Cad2207Dispatch {
    issue: String,
    lane: PathBuf,
}

fn cad2207_dispatch_fixture(fx: &Fx) -> Cad2207Dispatch {
    let repo = fx.dir.path().join("cad2207-repo");
    std::fs::create_dir_all(&repo).unwrap();
    cad2207_git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    std::fs::write(repo.join("README.md"), "cad2207 fixture\n").unwrap();
    cad2207_git(&repo, &["add", "."]);
    cad2207_git(&repo, &["commit", "--quiet", "-m", "fixture"]);
    let repo = repo.canonicalize().unwrap();
    let lane = crate::worktree::layout::worktrees_dir(&repo).join("cad2207-lane");
    std::fs::create_dir_all(lane.parent().unwrap()).unwrap();
    cad2207_git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "cadence/cad2207-lane",
            lane.to_str().unwrap(),
        ],
    );
    let pm = crate::issue::Pm::at(&fx.pm()).expect("fixture PM");
    let project = "cad2207";
    let prefix = "C7";
    let issue = "C7-1";
    crate::issue::write::project_add(
        &pm,
        project,
        prefix,
        &[repo.display().to_string()],
        &[],
        &[],
        None,
    )
    .unwrap_or_else(|e| panic!("fixture project: {e}"));
    crate::issue::write::new_issue(
        &pm,
        &repo,
        Some(project),
        "dispatch acceptance fixture",
        None,
        None,
        &[],
        None,
        None,
        &[],
        Some(issue),
        Some("fixture body\n\n## Acceptance\n\n- [ ] fixture work\n"),
        "cad2207-acceptance",
    )
    .unwrap_or_else(|e| panic!("fixture issue: {e}"));
    let (_project, dir) = crate::issue::write::issue_dir(&pm, issue).unwrap();
    let (mut front, body) = crate::issue::write::load_front(&dir).unwrap();
    front.status = "doing".to_string();
    front.owner = Some("ordinary-worker".to_string());
    front.refs = vec![crate::issue::model::Ref {
        kind: "worktree".to_string(),
        url: None,
        path: Some(lane.display().to_string()),
        label: None,
        closed: None,
        worktree: None,
        cargo_target: None,
        agent: None,
    }];
    front.claim = Some(crate::issue::model::Claim {
        by: "ordinary-worker".to_string(),
        at: crate::issue::time::iso(crate::issue::time::now_epoch()),
        session: None,
        last_seen: None,
        note: None,
        stale: None,
    });
    crate::issue::write::save_front(&dir, &front, &body).unwrap();
    Cad2207Dispatch {
        issue: issue.to_string(),
        lane,
    }
}

/// A verified human principal derived exactly as `human_current_actor` does
/// (same issuer, org and subject), for seeding a room through the store.
fn cad2207_human_actor(
    fx: &Fx,
    sub: &str,
    name: &str,
) -> crate::store::human_threads::VerifiedHuman {
    crate::store::human_threads::VerifiedHuman::with_name(&fx.issuer, ORG, sub, name)
        .unwrap_or_else(|e| panic!("verified human {sub}: {e}"))
}

fn cad2207_room_topic(room: &str) -> String {
    crate::store::topic_id(crate::store::human_threads::ROOM_TOPIC_TYPE, room)
}

fn cad2207_post_bodies(view: &Value) -> Vec<&str> {
    view["posts"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|post| post["body"].as_str())
                .collect()
        })
        .unwrap_or_default()
}

/// Stage a real `platform_effects` row through the store's own staging write
/// (`authorization_kind` defaults to `agent_grant`, which the read model
/// selects). Returns its brokered request handle.
fn cad2207_stage_effect(fx: &Fx, alias: &str, key: &str, text: &str) -> String {
    let request = format!("req-{key}");
    let row = crate::store::EffectRow {
        effect_id: format!("eff-{key}"),
        request: request.clone(),
        agent: alias.to_string(),
        platform: "cad2207-fixture".to_string(),
        account: "cad2207-account".to_string(),
        tool: "widgets.publish".to_string(),
        label: None,
        input: json!({"body": text}),
        input_summary: text.to_string(),
        preview: text.to_string(),
        source_name: None,
        source_hash: None,
        scopes: vec!["widgets:write".to_string()],
        task: None,
        state: "waiting".to_string(),
        close_reason: None,
        decision: None,
        outcome: None,
        needs_you: false,
        staged_at: 0.0,
        updated_at: 0.0,
    };
    fx.shared
        .store
        .effect_stage(&row)
        .unwrap_or_else(|e| panic!("stage effect {key}: {e}"));
    request
}

/// CAD-2207: `dispatch_send` is a queue-then-report path. An operator that
/// names a member's personal assistant must not queue the kickoff and must not
/// route its reply into a mailbox the member does not own. The SAME real
/// issue/project/lane/claim fixture accepts an ordinary agent, so the refusal
/// below is about the private target — never an ineligible fixture (missing
/// lane, unheld claim, unresolved project).
#[test]
fn cad2207_dispatch_send_cannot_queue_for_a_member_assistant_or_route_its_reply() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let reply_dir = fx.dir.path().join("dispatch-reply-cwd");
    std::fs::create_dir_all(&reply_dir).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "ordinary-reply",
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: reply_dir.to_str().unwrap(),
            sandbox: "workspace-write",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let dispatch = cad2207_dispatch_fixture(&fx);

    // Positive control: the same issue, claim holder, lane and reply mailbox
    // accept a real kickoff for an ordinary agent. This is the eligibility
    // proof for every field the negative call reuses.
    let ordinary_mid = "cad2207-dispatch-ordinary";
    let ordinary = fx
        .call(
            Asserted::Operator,
            "dispatch_send",
            json!({
                "alias": "ordinary-worker",
                "issue": dispatch.issue,
                "text": "ordinary kickoff",
                "reply_to": "ordinary-reply",
                "message": ordinary_mid,
            }),
        )
        .unwrap_or_else(|e| panic!("the ordinary dispatch fixture is not eligible: {e}"));
    assert_eq!(ordinary["duplicate"], json!(false), "{ordinary}");
    let queued = fx
        .shared
        .store
        .message(ordinary_mid)
        .unwrap()
        .expect("the ordinary kickoff was accepted without a stored message");
    assert_eq!(queued.alias, "ordinary-worker");
    assert_eq!(queued.reply_to.as_deref(), Some("ordinary-reply"));
    assert_eq!(queued.issue.as_deref(), Some(dispatch.issue.as_str()));
    assert_eq!(
        queued.worktree.as_deref(),
        Some(dispatch.lane.to_str().unwrap()),
        "the accepted kickoff did not bind the issue's real lane"
    );

    // Negative: the identical call, only the target changes to the member's
    // personal assistant. It must not queue the kickoff, must not store a
    // reply route to the caller-named mailbox, and must not touch the
    // assistant's queue.
    let private_mid = "cad2207-dispatch-private";
    let private_text = "private kickoff that must never queue";
    let private_before = fx.shared.store.messages(&alice.alias).unwrap().len();
    let worker_before = fx.shared.store.messages("ordinary-worker").unwrap().len();
    let reply_before = fx.shared.store.messages("ordinary-reply").unwrap().len();
    let refused = fx.call(
        Asserted::Operator,
        "dispatch_send",
        json!({
            "alias": alice.alias,
            "issue": dispatch.issue,
            "text": private_text,
            "reply_to": "ordinary-reply",
            "message": private_mid,
        }),
    );
    assert!(
        refused.is_err(),
        "an operator queued a dispatch kickoff to a member's private assistant: {refused:?}"
    );
    assert!(
        fx.shared.store.message(private_mid).unwrap().is_none(),
        "the refused private dispatch still stored its message"
    );
    assert_eq!(
        fx.shared.store.messages(&alice.alias).unwrap().len(),
        private_before,
        "the refused private dispatch queued the member's assistant"
    );
    assert_eq!(
        fx.shared.store.messages("ordinary-worker").unwrap().len(),
        worker_before,
        "the refused private dispatch changed the target worker's queue"
    );
    assert_eq!(
        fx.shared.store.messages("ordinary-reply").unwrap().len(),
        reply_before,
        "the refused private dispatch routed a copy into the caller-named mailbox"
    );
    assert!(
        !fx.shared
            .store
            .messages("ordinary-reply")
            .unwrap()
            .iter()
            .any(|m| m.body.contains(private_text) || m.id == private_mid),
        "the private kickoff text reached the unowned reply mailbox"
    );
    assert!(
        !fx.shared
            .store
            .messages(&alice.alias)
            .unwrap()
            .iter()
            .any(|m| m.body.contains(private_text)),
        "the private assistant received the kickoff text"
    );
}

/// CAD-2207: room posts are durable authorship. Once an assistant's `agents`
/// row is removed, its recorded sender alias still resolves to the
/// `assistants` binding, so its existing posts stay hidden from another member
/// and another agent while the owning member and every unrelated ordinary post
/// remain visible. The fixture writes the posts through the real store post
/// path before the removal (an old row, not a simulated one).
#[test]
fn cad2207_private_assistant_room_posts_stay_hidden_after_agent_row_removal() {
    const PRIVATE: &str = "cad2207 private room reply: only the owning member sees this";
    const ORDINARY_AGENT: &str = "cad2207 ordinary agent room reply";
    const ORDINARY_HUMAN: &str = "cad2207 other member room reply";
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let assistant_uid = fx.shared.store.agent(&alice.alias).unwrap().uid;
    let ordinary_uid = fx.shared.store.agent("ordinary-worker").unwrap().uid;
    cad2207_room_roster(
        &fx,
        &[
            ("sub-alice", "Alice", "member"),
            ("sub-ben", "Ben", "member"),
        ],
    );
    let alice_actor = cad2207_human_actor(&fx, "sub-alice", "Alice");
    let ben_actor = cad2207_human_actor(&fx, "sub-ben", "Ben");
    fx.shared
        .store
        .human_directory_upsert(
            ORG,
            &[
                (alice_actor.clone(), "Alice".to_string()),
                (ben_actor.clone(), "Ben".to_string()),
            ],
        )
        .unwrap();

    // Seed an older mixed room containing the private assistant (the add
    // guard now refuses new admissions; existing rows must still be filtered).
    let room = fx
        .shared
        .store
        .human_group_create(
            &ben_actor,
            "mixed_group",
            "cad2207 room",
            "cad2207-room-posts",
            &[
                crate::store::human_threads::RoomMember::Human(alice_actor.id.clone()),
                crate::store::human_threads::RoomMember::Agent(alice.alias.clone()),
                crate::store::human_threads::RoomMember::Agent("ordinary-worker".to_string()),
            ],
        )
        .unwrap_or_else(|e| panic!("seed the mixed room: {e}"));
    let topic = cad2207_room_topic(&room);
    fx.shared
        .store
        .human_agent_post(ORG, &assistant_uid, &topic, "cad2207-room-private", PRIVATE)
        .unwrap();
    fx.shared
        .store
        .human_agent_post(
            ORG,
            &ordinary_uid,
            &topic,
            "cad2207-room-agent",
            ORDINARY_AGENT,
        )
        .unwrap();
    fx.shared
        .store
        .human_room_post(&ben_actor, &room, "cad2207-room-human", ORDINARY_HUMAN)
        .unwrap();

    let owner_token = cad2207_public_session(&fx, "sub-alice", "Alice", "member");
    let ben_token = cad2207_public_session(&fx, "sub-ben", "Ben", "member");

    // Before the removal the binding is still resolvable through the row.
    let ben_before = fx
        .op_err("human_room_read", json!({"token": ben_token, "room": room}))
        .unwrap_or_else(|e| panic!("another member lost the room read: {e}"));
    let before_bodies = cad2207_post_bodies(&ben_before);
    assert!(
        before_bodies.iter().all(|body| !body.contains(PRIVATE)),
        "another member read a private assistant post: {ben_before}"
    );
    assert!(
        before_bodies
            .iter()
            .any(|body| body.contains(ORDINARY_AGENT))
            && before_bodies
                .iter()
                .any(|body| body.contains(ORDINARY_HUMAN)),
        "another member lost an unrelated room post: {ben_before}"
    );

    // The agent row disappears. The binding and the post row survive; a
    // removed row must not turn the private sender ordinary.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "DELETE FROM agents WHERE alias=?",
                rusqlite::params![alice.alias],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        fx.shared.store.agent_opt(&alice.alias).unwrap().is_none(),
        "the fixture's agent-row removal did not land"
    );
    assert!(
        fx.shared
            .store
            .assistant_for_alias(&alice.alias)
            .unwrap()
            .is_some(),
        "the fixture's durable assistant binding disappeared with the row"
    );

    let owner = fx
        .op_err(
            "human_room_read",
            json!({"token": owner_token, "room": room}),
        )
        .unwrap_or_else(|e| panic!("the owning member lost the room read: {e}"));
    let ben = fx
        .op_err("human_room_read", json!({"token": ben_token, "room": room}))
        .unwrap_or_else(|e| panic!("another member lost the room read: {e}"));
    let other_agent = fx
        .call(
            Asserted::Agent("ordinary-worker".to_string()),
            "topic_read",
            json!({"topic": topic, "limit": 50}),
        )
        .unwrap_or_else(|e| panic!("another agent lost the room read: {e}"));

    let owner_bodies = cad2207_post_bodies(&owner);
    assert!(
        owner_bodies.iter().any(|body| body.contains(PRIVATE)),
        "the owning member lost its own assistant's post: {owner}"
    );
    assert!(
        owner_bodies
            .iter()
            .any(|body| body.contains(ORDINARY_AGENT))
            && owner_bodies
                .iter()
                .any(|body| body.contains(ORDINARY_HUMAN)),
        "the owning member lost an unrelated room post: {owner}"
    );
    let ben_bodies = cad2207_post_bodies(&ben);
    assert!(
        ben_bodies.iter().all(|body| !body.contains(PRIVATE)),
        "another member read a private assistant post after the row removal: {ben}"
    );
    assert!(
        ben_bodies.iter().any(|body| body.contains(ORDINARY_AGENT))
            && ben_bodies.iter().any(|body| body.contains(ORDINARY_HUMAN)),
        "another member lost an unrelated room post after the row removal: {ben}"
    );
    let agent_bodies = cad2207_post_bodies(&other_agent);
    assert!(
        agent_bodies.iter().all(|body| !body.contains(PRIVATE)),
        "another agent read a private assistant post after the row removal: {other_agent}"
    );
    assert!(
        agent_bodies
            .iter()
            .any(|body| body.contains(ORDINARY_AGENT))
            && agent_bodies
                .iter()
                .any(|body| body.contains(ORDINARY_HUMAN)),
        "another agent lost an unrelated room post after the row removal: {other_agent}"
    );
}

/// CAD-2207: the reachable room-membership guards and the verified-human agent
/// list. A private assistant cannot be added to or created inside a shared
/// room, nor appear in another member's agent list, while ordinary agents keep
/// those paths. A transcript post by another authorized member is NOT an
/// outage: it still lands, does not queue/steer the private assistant, and the
/// assistant's own post stays hidden.
#[test]
fn cad2207_room_membership_and_human_agent_list_refuse_a_private_assistant() {
    const PRIVATE: &str = "cad2207 private membership post";
    const ORDINARY_POST: &str = "cad2207 ordinary member post";
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let assistant_uid = fx.shared.store.agent(&alice.alias).unwrap().uid;
    let ordinary_uid = fx.shared.store.agent("ordinary-worker").unwrap().uid;
    let worker2_dir = fx.dir.path().join("worker2-cwd");
    std::fs::create_dir_all(&worker2_dir).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "ordinary-worker-2",
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: worker2_dir.to_str().unwrap(),
            sandbox: "workspace-write",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let worker2_uid = fx.shared.store.agent("ordinary-worker-2").unwrap().uid;

    cad2207_room_roster(
        &fx,
        &[
            ("sub-alice", "Alice", "member"),
            ("sub-ben", "Ben", "member"),
        ],
    );
    let alice_actor = cad2207_human_actor(&fx, "sub-alice", "Alice");
    let ben_actor = cad2207_human_actor(&fx, "sub-ben", "Ben");
    fx.shared
        .store
        .human_directory_upsert(
            ORG,
            &[
                (alice_actor.clone(), "Alice".to_string()),
                (ben_actor.clone(), "Ben".to_string()),
            ],
        )
        .unwrap();
    let alice_token = cad2207_public_session(&fx, "sub-alice", "Alice", "member");
    let ben_token = cad2207_public_session(&fx, "sub-ben", "Ben", "member");

    // Verified human agent list: the owner sees its own assistant and the
    // ordinary fleet; another verified member sees the ordinary fleet only.
    let ben_list = fx.op("human_agents_list", json!({"token": ben_token}));
    let ben_aliases: Vec<&str> = ben_list["agents"]
        .as_array()
        .expect("human_agents_list carries no agents array")
        .iter()
        .filter_map(|row| row["alias"].as_str())
        .collect();
    assert!(
        !ben_aliases.contains(&alice.alias.as_str()),
        "another verified member saw a private assistant in human_agents_list: {ben_list}"
    );
    assert!(
        ben_aliases.contains(&"ordinary-worker") && ben_aliases.contains(&"ordinary-worker-2"),
        "another verified member lost an ordinary agent row: {ben_list}"
    );
    let owner_list = fx.op("human_agents_list", json!({"token": alice_token}));
    let owner_aliases: Vec<&str> = owner_list["agents"]
        .as_array()
        .expect("human_agents_list carries no agents array")
        .iter()
        .filter_map(|row| row["alias"].as_str())
        .collect();
    assert!(
        owner_aliases.contains(&alice.alias.as_str()),
        "the owning member lost its own assistant from human_agents_list: {owner_list}"
    );
    assert!(
        owner_aliases.contains(&"ordinary-worker"),
        "the owning member lost an ordinary agent row: {owner_list}"
    );

    // Create: a private assistant is refused; an ordinary agent succeeds.
    let create_private = fx.op_err(
        "human_group_create",
        json!({
            "token": ben_token,
            "kind": "mixed_group",
            "title": "cad2207 private create",
            "client_id": "cad2207-create-private",
            "agents": [assistant_uid],
        }),
    );
    assert!(
        create_private.is_err(),
        "a verified member created a shared room with a private assistant: {create_private:?}"
    );
    let created = fx
        .op_err(
            "human_group_create",
            json!({
                "token": ben_token,
                "kind": "mixed_group",
                "title": "cad2207 ordinary create",
                "client_id": "cad2207-create-ordinary",
                "agents": [ordinary_uid],
            }),
        )
        .unwrap_or_else(|e| panic!("a verified member lost the ordinary room create: {e}"));
    let ordinary_room = created["room"]
        .as_str()
        .expect("the ordinary room create returned no room")
        .to_string();

    // Add: a private assistant is refused; a second ordinary agent succeeds.
    let add_private = fx.op_err(
        "human_room_add",
        json!({
            "token": ben_token,
            "room": ordinary_room,
            "agent_uid": assistant_uid,
        }),
    );
    assert!(
        add_private.is_err(),
        "a verified member added a private assistant to a shared room: {add_private:?}"
    );
    fx.op_err(
        "human_room_add",
        json!({
            "token": ben_token,
            "room": ordinary_room,
            "agent_uid": worker2_uid,
        }),
    )
    .unwrap_or_else(|e| panic!("a verified member lost the ordinary room add: {e}"));

    // Post: an older shared room still holds the assistant's durable
    // membership (seeded through the store). Another authorized member may
    // still post a transcript row; that must not queue or steer the private
    // assistant, and its post stays hidden from that member.
    let shared_room = fx
        .shared
        .store
        .human_group_create(
            &ben_actor,
            "mixed_group",
            "cad2207 shared room",
            "cad2207-room-mixed",
            &[
                crate::store::human_threads::RoomMember::Human(alice_actor.id.clone()),
                crate::store::human_threads::RoomMember::Agent(alice.alias.clone()),
            ],
        )
        .unwrap_or_else(|e| panic!("seed the shared room: {e}"));
    let topic = cad2207_room_topic(&shared_room);
    fx.shared
        .store
        .human_agent_post(
            ORG,
            &assistant_uid,
            &topic,
            "cad2207-membership-private",
            PRIVATE,
        )
        .unwrap();
    let assistant_messages = fx.shared.store.messages(&alice.alias).unwrap().len();
    let posted = fx
        .op_err(
            "human_room_post",
            json!({
                "token": ben_token,
                "room": shared_room,
                "client_id": "cad2207-ben-post",
                "body": ORDINARY_POST,
            }),
        )
        .unwrap_or_else(|e| panic!("another member lost the shared-room post: {e}"));
    assert!(
        posted["post"]["body"]
            .as_str()
            .is_some_and(|body| body.contains(ORDINARY_POST)),
        "another member's shared-room post did not land: {posted}"
    );
    assert_eq!(
        fx.shared.store.messages(&alice.alias).unwrap().len(),
        assistant_messages,
        "a transcript post queued or steered the private assistant"
    );

    let ben_view = fx
        .op_err(
            "human_room_read",
            json!({"token": ben_token, "room": shared_room}),
        )
        .unwrap_or_else(|e| panic!("another member lost the shared-room read: {e}"));
    let ben_bodies = cad2207_post_bodies(&ben_view);
    assert!(
        ben_bodies.iter().any(|body| body.contains(ORDINARY_POST)),
        "another member lost its own shared-room post: {ben_view}"
    );
    assert!(
        ben_bodies.iter().all(|body| !body.contains(PRIVATE)),
        "another member read the private assistant's shared-room post: {ben_view}"
    );
    let owner_view = fx
        .op_err(
            "human_room_read",
            json!({"token": alice_token, "room": shared_room}),
        )
        .unwrap_or_else(|e| panic!("the owning member lost the shared-room read: {e}"));
    let owner_bodies = cad2207_post_bodies(&owner_view);
    assert!(
        owner_bodies.iter().any(|body| body.contains(PRIVATE)),
        "the owning member lost its assistant's shared-room post: {owner_view}"
    );
    assert!(
        owner_bodies.iter().any(|body| body.contains(ORDINARY_POST)),
        "the owning member lost another member's post: {owner_view}"
    );
}

/// CAD-2207: a staged effect's input is private to the member's assistant.
/// `platform_effects` must not leak it; `agent_respond` must not accept,
/// answer or steer it (only a bare negative decline is preserved); close must
/// not echo the caller's reason or the input. Ordinary effects keep every one
/// of those paths, including the visible record.
#[test]
fn cad2207_private_effect_input_and_decisions_stay_private() {
    const PRIVATE: &str = "cad2207 private effect input: member-only command text";
    const ORDINARY: &str = "cad2207 ordinary effect input";
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");

    let private_accept = cad2207_stage_effect(&fx, &alice.alias, "cad2207-private-accept", PRIVATE);
    let private_decline =
        cad2207_stage_effect(&fx, &alice.alias, "cad2207-private-decline", PRIVATE);
    let private_close = cad2207_stage_effect(&fx, &alice.alias, "cad2207-private-close", PRIVATE);
    let ordinary_accept =
        cad2207_stage_effect(&fx, "ordinary-worker", "cad2207-ordinary-accept", ORDINARY);
    let ordinary_decline =
        cad2207_stage_effect(&fx, "ordinary-worker", "cad2207-ordinary-decline", ORDINARY);
    let ordinary_close =
        cad2207_stage_effect(&fx, "ordinary-worker", "cad2207-ordinary-close", ORDINARY);

    // Read model: the private input never appears; the ordinary input does.
    let all = fx.op("platform_effects", json!({}));
    assert!(
        !all.to_string().contains(PRIVATE),
        "the operator's effect read model leaked a member-private input: {all}"
    );
    assert!(
        all.to_string().contains(ORDINARY),
        "the operator's effect read model lost an ordinary effect input: {all}"
    );
    let scoped = fx.op("platform_effects", json!({"agent": alice.alias}));
    assert!(
        !scoped.to_string().contains(PRIVATE),
        "the operator's filtered effect read model leaked a member-private input: {scoped}"
    );
    for handle in [&private_accept, &private_decline, &private_close] {
        if let Some(row) = all["effects"].as_array().and_then(|rows| {
            rows.iter()
                .find(|row| row["request"].as_str() == Some(handle))
        }) {
            assert!(
                !row.to_string().contains(PRIVATE),
                "the operator saw a member-private effect row with its input: {row}"
            );
        }
    }

    // Approval, answers and a caller-authored reason are all blocked before
    // the row is claimed; the row stays waiting with no decision.
    let accepted = fx.call(
        Asserted::Operator,
        "agent_respond",
        json!({"alias": alice.alias, "request": private_accept, "decision": "accept"}),
    );
    assert!(
        accepted.is_err(),
        "an operator accepted a member-private effect: {accepted:?}"
    );
    for params in [
        json!({"alias": alice.alias, "request": private_accept, "decision": "decline",
               "answers": {"question": "private answer"}}),
        json!({"alias": alice.alias, "request": private_accept, "decision": "decline",
               "reason": "private steer"}),
    ] {
        let refused = fx.call(Asserted::Operator, "agent_respond", params.clone());
        assert!(
            refused.is_err(),
            "an operator steered a member-private effect with {params}: {refused:?}"
        );
    }
    let waiting = fx
        .shared
        .store
        .effect_by_request(&private_accept)
        .unwrap()
        .expect("the private accept row disappeared");
    assert_eq!(waiting.state, "waiting", "a refused press changed the row");
    assert!(
        waiting.decision.is_none(),
        "a refused press recorded a decision"
    );
    assert_eq!(
        fx.shared
            .store
            .effect_by_request(&private_decline)
            .unwrap()
            .expect("the private decline row disappeared")
            .state,
        "waiting"
    );

    // Preserved half: a bare decline executes nothing and is allowed, but its
    // response still carries no private input.
    let declined = fx
        .call(
            Asserted::Operator,
            "agent_respond",
            json!({"alias": alice.alias, "request": private_decline, "decision": "decline"}),
        )
        .unwrap_or_else(|e| panic!("the safe bare decline was refused: {e}"));
    assert_eq!(declined["state"], json!("answered"), "{declined}");
    assert_eq!(declined["effect"]["state"], json!("declined"), "{declined}");
    assert!(
        !declined.to_string().contains(PRIVATE),
        "the safe decline echoed the member-private input: {declined}"
    );
    let declined_row = fx
        .shared
        .store
        .effect_by_request(&private_decline)
        .unwrap()
        .expect("the bare decline did not decide its row");
    assert_eq!(declined_row.state, "declined");
    assert!(
        declined_row.decision.is_some(),
        "the bare decline recorded no decision"
    );

    // Close: server-owned reason only, and the record is input-free.
    let caller_reason = "cad2207 caller-authored close reason";
    let closed = fx
        .call(
            Asserted::Operator,
            "platform_effect_close",
            json!({"request": private_close, "reason": caller_reason}),
        )
        .unwrap_or_else(|e| panic!("the private effect could not be closed safely: {e}"));
    assert_eq!(closed["record"]["state"], json!("closed"), "{closed}");
    assert!(
        closed["record"].get("close_reason").is_none(),
        "the private close receipt serialized a close_reason: {closed}"
    );
    assert!(
        closed["record"].get("outcome").is_none(),
        "the private close receipt serialized an outcome: {closed}"
    );
    assert!(
        !closed.to_string().contains(PRIVATE),
        "the private close leaked the member-private input: {closed}"
    );
    assert!(
        !closed.to_string().contains(caller_reason),
        "the private close echoed the caller-authored reason: {closed}"
    );
    let closed_row = fx
        .shared
        .store
        .effect_by_request(&private_close)
        .unwrap()
        .expect("the private close did not land");
    assert_eq!(closed_row.state, "closed");
    assert_ne!(
        closed_row.close_reason.as_deref(),
        Some(caller_reason),
        "the private close persisted the caller-authored reason"
    );

    // The outcome receipt after a decision/close is the member's context too:
    // no other agent's mailbox may carry the private input.
    for agent in fx.shared.store.agents().unwrap() {
        if agent.alias == alice.alias {
            continue;
        }
        assert!(
            !fx.shared
                .store
                .messages(&agent.alias)
                .unwrap()
                .iter()
                .any(|m| m.body.contains(PRIVATE)),
            "a private effect receipt reached another agent's mailbox: {}",
            agent.alias
        );
    }

    // Ordinary-effect positive controls: the same paths still work and still
    // return the ordinary record's real input.
    let ordinary_accepted = fx
        .call(
            Asserted::Operator,
            "agent_respond",
            json!({"alias": "ordinary-worker", "request": ordinary_accept, "decision": "accept"}),
        )
        .unwrap_or_else(|e| panic!("an ordinary effect lost the operator accept path: {e}"));
    assert!(
        ordinary_accepted.to_string().contains(ORDINARY),
        "the ordinary accept lost its visible input: {ordinary_accepted}"
    );
    let ordinary_declined = fx
        .call(
            Asserted::Operator,
            "agent_respond",
            json!({"alias": "ordinary-worker", "request": ordinary_decline, "decision": "decline"}),
        )
        .unwrap_or_else(|e| panic!("an ordinary effect lost the operator decline path: {e}"));
    assert!(
        ordinary_declined.to_string().contains(ORDINARY),
        "the ordinary decline lost its visible input: {ordinary_declined}"
    );
    let ordinary_closed = fx
        .call(
            Asserted::Operator,
            "platform_effect_close",
            json!({"request": ordinary_close}),
        )
        .unwrap_or_else(|e| panic!("an ordinary effect lost the operator close path: {e}"));
    assert!(
        ordinary_closed.to_string().contains(ORDINARY),
        "the ordinary close lost its visible input: {ordinary_closed}"
    );
}

/// CAD-2207: a bound assistant may carry a nonowner `upstream` label. An
/// allowed bare decline of a private effect must not deliver the private
/// input_summary/preview to that mailbox; an ordinary agent's upstream keeps
/// the normal outcome routing.
#[test]
fn cad2207_private_effect_decline_does_not_route_to_a_nonowner_upstream() {
    const PRIVATE: &str = "cad2207 private upstream secret";
    const ORDINARY: &str = "cad2207 ordinary upstream outcome";
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let reply_dir = fx.dir.path().join("effect-reply-cwd");
    std::fs::create_dir_all(&reply_dir).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "ordinary-reply",
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: reply_dir.to_str().unwrap(),
            sandbox: "workspace-write",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    // A nonowner upstream label on both the private assistant and an ordinary
    // agent — the routing decision must be target-keyed, not blanket.
    for alias in [alice.alias.as_str(), "ordinary-worker"] {
        fx.shared
            .store
            .fixture_write(|tx| {
                tx.execute(
                    "UPDATE agents SET params=json_set(COALESCE(params,'{}'),'$.upstream',?) \
                     WHERE alias=?",
                    rusqlite::params!["ordinary-reply", alias],
                )?;
                Ok(())
            })
            .unwrap();
    }

    let private_request =
        cad2207_stage_effect(&fx, &alice.alias, "cad2207-private-upstream", PRIVATE);
    let declined = fx
        .call(
            Asserted::Operator,
            "agent_respond",
            json!({"alias": alice.alias, "request": private_request, "decision": "decline"}),
        )
        .unwrap_or_else(|e| panic!("the safe private decline was refused: {e}"));
    assert_eq!(declined["effect"]["state"], json!("declined"), "{declined}");
    assert!(
        !declined.to_string().contains(PRIVATE),
        "the private decline receipt leaked the secret input: {declined}"
    );
    let upstream = fx.shared.store.messages("ordinary-reply").unwrap();
    assert!(
        upstream.iter().all(|m| !m.body.contains(PRIVATE)),
        "a private effect decline routed its secret body/result to a nonowner upstream"
    );

    // Positive: the same upstream routing works for an ordinary effect.
    let ordinary_request = cad2207_stage_effect(
        &fx,
        "ordinary-worker",
        "cad2207-ordinary-upstream",
        ORDINARY,
    );
    let before = fx.shared.store.messages("ordinary-reply").unwrap().len();
    fx.call(
        Asserted::Operator,
        "agent_respond",
        json!({"alias": "ordinary-worker", "request": ordinary_request, "decision": "decline"}),
    )
    .unwrap_or_else(|e| panic!("the ordinary decline was refused: {e}"));
    let after = fx.shared.store.messages("ordinary-reply").unwrap();
    assert!(
        after.len() > before,
        "the ordinary effect outcome did not reach its configured upstream"
    );
    assert!(
        after.iter().any(|m| m.body.contains(ORDINARY)),
        "the ordinary upstream outcome lost its visible input"
    );
}

fn cad2207_room_unread(view: &Value, room: &str) -> (i64, bool) {
    let group = view["groups"]
        .as_array()
        .and_then(|rows| rows.iter().find(|g| g["room"].as_str() == Some(room)))
        .unwrap_or_else(|| panic!("room {room} missing from the inbox: {view}"));
    (
        group["unread"]
            .as_i64()
            .expect("notification group carries no unread count"),
        group["mention"].as_bool().unwrap_or(false),
    )
}

/// CAD-2207: filtering hidden assistant posts must not strand a client.
/// Seed one ordinary post, then more than eight pages of newer hidden
/// assistant posts. A non-owner's empty first page still returns `has_more`
/// plus a usable cursor, and following that cursor reaches the ordinary post;
/// the owner sees its own assistant's new posts in the same first page; the
/// agent `topic_read` page keeps the same continuation; and the non-owner's
/// room/inbox unread counts exclude hidden posts while the owner's include
/// them.
#[test]
fn cad2207_room_paging_and_unread_exclude_hidden_posts_without_stranding() {
    const ORDINARY: &str = "cad2207 paging ordinary post";
    const PRIVATE: &str = "cad2207 paging hidden assistant post";
    const PAGE: i64 = 2;
    const HIDDEN: usize = (PAGE as usize) * 8 + 1;
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let assistant_uid = fx.shared.store.agent(&alice.alias).unwrap().uid;
    let ordinary_uid = fx.shared.store.agent("ordinary-worker").unwrap().uid;
    cad2207_room_roster(
        &fx,
        &[
            ("sub-alice", "Alice", "member"),
            ("sub-ben", "Ben", "member"),
        ],
    );
    let alice_actor = cad2207_human_actor(&fx, "sub-alice", "Alice");
    let ben_actor = cad2207_human_actor(&fx, "sub-ben", "Ben");
    fx.shared
        .store
        .human_directory_upsert(
            ORG,
            &[
                (alice_actor.clone(), "Alice".to_string()),
                (ben_actor.clone(), "Ben".to_string()),
            ],
        )
        .unwrap();
    let room = fx
        .shared
        .store
        .human_group_create(
            &ben_actor,
            "mixed_group",
            "cad2207 paging",
            "cad2207-room-paging",
            &[
                crate::store::human_threads::RoomMember::Human(alice_actor.id.clone()),
                crate::store::human_threads::RoomMember::Agent(alice.alias.clone()),
                crate::store::human_threads::RoomMember::Agent("ordinary-worker".to_string()),
            ],
        )
        .unwrap_or_else(|e| panic!("seed the paging room: {e}"));
    let topic = cad2207_room_topic(&room);
    // The ordinary post is OLDER than every hidden assistant post.
    fx.shared
        .store
        .human_agent_post(
            ORG,
            &ordinary_uid,
            &topic,
            "cad2207-paging-ordinary",
            ORDINARY,
        )
        .unwrap();
    for index in 0..HIDDEN {
        fx.shared
            .store
            .human_agent_post(
                ORG,
                &assistant_uid,
                &topic,
                &format!("cad2207-paging-hidden-{index}"),
                &format!("{PRIVATE} #{index}"),
            )
            .unwrap();
    }
    let alice_token = cad2207_public_session(&fx, "sub-alice", "Alice", "member");
    let ben_token = cad2207_public_session(&fx, "sub-ben", "Ben", "member");

    // Non-owner first page: the newest PAGE posts are hidden, so the visible
    // page is empty — yet it must still say there is more and where to go.
    let first = fx
        .op_err(
            "human_room_read",
            json!({"token": ben_token, "room": room, "limit": PAGE}),
        )
        .unwrap_or_else(|e| panic!("nonowner first page: {e}"));
    assert!(
        cad2207_post_bodies(&first).is_empty(),
        "the nonowner's hidden-only first page was not empty: {first}"
    );
    assert_eq!(
        first["has_more"],
        json!(true),
        "an empty filtered page lost has_more: {first}"
    );
    let mut before = first["cursor"]
        .as_i64()
        .expect("an empty filtered page returned no cursor");
    assert!(
        before > 0,
        "the empty filtered page cursor was not usable: {first}"
    );
    let mut reached = false;
    for _ in 0..(HIDDEN + 4) {
        let page = fx
            .op_err(
                "human_room_read",
                json!({"token": ben_token, "room": room, "limit": PAGE, "before": before}),
            )
            .unwrap_or_else(|e| panic!("nonowner continuation page: {e}"));
        let bodies = cad2207_post_bodies(&page);
        assert!(
            bodies.iter().all(|body| !body.contains(PRIVATE)),
            "a continuation page exposed a hidden assistant post: {page}"
        );
        if bodies.iter().any(|body| body.contains(ORDINARY)) {
            reached = true;
            break;
        }
        if page["has_more"] != json!(true) {
            break;
        }
        let next = page["cursor"]
            .as_i64()
            .expect("a continuation page returned no cursor");
        assert!(
            next < before,
            "the continuation cursor did not move older: {page}"
        );
        before = next;
    }
    assert!(
        reached,
        "the nonowner's continuation never reached the ordinary post"
    );

    // The owner sees its own assistant's new posts in the same first page.
    let owner_first = fx
        .op_err(
            "human_room_read",
            json!({"token": alice_token, "room": room, "limit": PAGE}),
        )
        .unwrap_or_else(|e| panic!("owner first page: {e}"));
    let owner_bodies = cad2207_post_bodies(&owner_first);
    assert!(
        owner_bodies.iter().any(|body| body.contains(PRIVATE)),
        "the owner lost its own assistant's new posts: {owner_first}"
    );
    assert_eq!(
        owner_first["has_more"],
        json!(true),
        "the owner's first page lost has_more: {owner_first}"
    );

    // Agent `topic_read`: the same continuation contract for another agent.
    let agent_first = fx
        .call(
            Asserted::Agent("ordinary-worker".to_string()),
            "topic_read",
            json!({"topic": topic, "limit": PAGE}),
        )
        .unwrap_or_else(|e| panic!("agent first page: {e}"));
    assert!(
        cad2207_post_bodies(&agent_first).is_empty(),
        "the other agent's hidden-only first page was not empty: {agent_first}"
    );
    assert_eq!(
        agent_first["more_before"],
        json!(true),
        "an empty filtered agent page lost more_before: {agent_first}"
    );
    let mut agent_before = agent_first["cursor"]
        .as_i64()
        .expect("an empty filtered agent page returned no cursor");
    assert!(agent_before > 0);
    let mut agent_reached = false;
    for _ in 0..(HIDDEN + 4) {
        let page = fx
            .call(
                Asserted::Agent("ordinary-worker".to_string()),
                "topic_read",
                json!({"topic": topic, "limit": PAGE, "before": agent_before}),
            )
            .unwrap_or_else(|e| panic!("agent continuation page: {e}"));
        let bodies = cad2207_post_bodies(&page);
        assert!(
            bodies.iter().all(|body| !body.contains(PRIVATE)),
            "an agent continuation page exposed a hidden assistant post: {page}"
        );
        if bodies.iter().any(|body| body.contains(ORDINARY)) {
            agent_reached = true;
            break;
        }
        if page["more_before"] != json!(true) {
            break;
        }
        let next = page["cursor"]
            .as_i64()
            .expect("an agent continuation page returned no cursor");
        assert!(
            next < agent_before,
            "the agent continuation cursor did not move older: {page}"
        );
        agent_before = next;
    }
    assert!(
        agent_reached,
        "the other agent's continuation never reached the ordinary post"
    );

    // Unread/notifications: the non-owner counts only the ordinary post; the
    // owner also counts its own assistant's posts, which are hidden to others.
    let ben_notifications = fx.op("human_notifications", json!({"token": ben_token}));
    let (ben_unread, ben_mention) = cad2207_room_unread(&ben_notifications, &room);
    assert_eq!(
        ben_unread, 1,
        "the nonowner's unread count included hidden assistant posts: {ben_notifications}"
    );
    assert!(
        !ben_mention,
        "the nonowner received a hidden mention: {ben_notifications}"
    );
    assert_eq!(
        ben_notifications["unread_total"],
        json!(1),
        "the nonowner's unread total included hidden posts: {ben_notifications}"
    );
    let ben_rooms = fx.op("human_rooms_list", json!({"token": ben_token}));
    let ben_room = ben_rooms["rooms"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["id"].as_str() == Some(room.as_str()))
        })
        .expect("the nonowner's room list lost the room");
    assert_eq!(
        ben_room["unread"],
        json!(1),
        "the nonowner's room list counted hidden assistant posts: {ben_rooms}"
    );
    let owner_notifications = fx.op("human_notifications", json!({"token": alice_token}));
    let (owner_unread, owner_mention) = cad2207_room_unread(&owner_notifications, &room);
    assert_eq!(
        owner_unread,
        (HIDDEN as i64) + 1,
        "the owner's unread count lost its own assistant posts: {owner_notifications}"
    );
    assert!(!owner_mention);
    assert_eq!(
        owner_notifications["unread_total"],
        json!((HIDDEN as i64) + 1),
        "the owner's unread total lost its own assistant posts: {owner_notifications}"
    );
    let owner_rooms = fx.op("human_rooms_list", json!({"token": alice_token}));
    let owner_room = owner_rooms["rooms"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["id"].as_str() == Some(room.as_str()))
        })
        .expect("the owner's room list lost the room");
    assert_eq!(
        owner_room["unread"],
        json!((HIDDEN as i64) + 1),
        "the owner's room list lost its own assistant posts: {owner_rooms}"
    );
}

/// CAD-2207: `thread_send` and the operator-chat `conversation_*` verbs carry
/// no member-envelope proof. A private assistant alias is refused before any
/// message/thread/app-conversation write, while the same verbs keep working
/// for an ordinary agent with a known installation.
#[test]
fn cad2207_operator_chat_verbs_refuse_private_app_and_thread_surfaces() {
    const INSTALL: &str = "cad2207-install";
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    // A known installation: `app_contexts` is the store's own proof.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO app_contexts
                 (id,install_id,request_id,revision,state,config_json,config_digest,created,updated)
                 VALUES('cad2207-context',?1,'cad2207-request',1,'active',?2,'digest',0,0)",
                rusqlite::params![
                    INSTALL,
                    json!({"schema":1,"label":"cad2207 fixture","input_defaults":{}}).to_string()
                ],
            )?;
            Ok(())
        })
        .unwrap();

    // Private conversation listing/creation is refused before the store's
    // ensure/create; the private alias keeps no app thread rows.
    assert!(
        fx.shared
            .store
            .conversation_list(&alice.alias, INSTALL)
            .unwrap()
            .is_empty(),
        "the fixture already had an app conversation for the private alias"
    );
    let list_private = fx.op_err(
        "conversation_list",
        json!({"alias": alice.alias, "install_id": INSTALL}),
    );
    assert!(
        list_private.is_err(),
        "an operator listed a member assistant's app conversations: {list_private:?}"
    );
    assert!(
        fx.shared
            .store
            .conversation_list(&alice.alias, INSTALL)
            .unwrap()
            .is_empty(),
        "a refused private conversation list created a thread row"
    );
    let create_private = fx.op_err(
        "conversation_create",
        json!({"alias": alice.alias, "install_id": INSTALL}),
    );
    assert!(
        create_private.is_err(),
        "an operator created a member assistant's app conversation: {create_private:?}"
    );
    assert!(
        fx.shared
            .store
            .conversation_list(&alice.alias, INSTALL)
            .unwrap()
            .is_empty(),
        "a refused private conversation create wrote a thread row"
    );

    // Positive controls: the same operator verbs work for an ordinary agent.
    let listed = fx
        .op_err(
            "conversation_list",
            json!({"alias": "ordinary-worker", "install_id": INSTALL}),
        )
        .unwrap_or_else(|e| panic!("the ordinary conversation list was refused: {e}"));
    assert!(
        listed["general"].as_str().is_some(),
        "the ordinary conversation list carried no General row: {listed}"
    );
    let created = fx
        .op_err(
            "conversation_create",
            json!({"alias": "ordinary-worker", "install_id": INSTALL}),
        )
        .unwrap_or_else(|e| panic!("the ordinary conversation create was refused: {e}"));
    assert_eq!(created["created"], json!(true), "{created}");
    assert!(
        created["conversation"]["id"].as_str().is_some(),
        "the ordinary conversation create returned no id: {created}"
    );

    // `thread_send` is the operator-chat write half: its private target is
    // refused before a message, thread entry or source stamp exists.
    let before_entries = fx
        .shared
        .store
        .thread_entries_of(&alice.thread_id, 0, 100)
        .unwrap()
        .len();
    let send_private = fx.call(
        Asserted::Operator,
        "thread_send",
        json!({"alias": alice.alias, "text": "private thread text",
               "message": "cad2207-thread-private"}),
    );
    assert!(
        send_private.is_err(),
        "an operator sent into a member assistant's thread: {send_private:?}"
    );
    assert!(
        fx.shared
            .store
            .message("cad2207-thread-private")
            .unwrap()
            .is_none(),
        "a refused private thread send stored its message"
    );
    assert_eq!(
        fx.shared
            .store
            .thread_entries_of(&alice.thread_id, 0, 100)
            .unwrap()
            .len(),
        before_entries,
        "a refused private thread send wrote a thread entry"
    );
    // Positive: the same send for an ordinary agent lands.
    let sent = fx
        .call(
            Asserted::Operator,
            "thread_send",
            json!({"alias": "ordinary-worker", "text": "ordinary thread text",
                   "message": "cad2207-thread-ordinary"}),
        )
        .unwrap_or_else(|e| panic!("the ordinary thread send was refused: {e}"));
    assert!(
        sent["thread"]["id"].as_str().is_some(),
        "the ordinary thread send returned no thread: {sent}"
    );
    let ordinary_message = fx.shared.store.message("cad2207-thread-ordinary").unwrap();
    assert!(
        ordinary_message.is_some_and(|m| m.alias == "ordinary-worker"),
        "the ordinary thread send did not land in the ordinary mailbox"
    );
}

/// CAD-2207: a queued turn for a bound assistant that an operator (or another
/// untrusted conversational source) enqueued must not re-prove at execution
/// time just because the operator is trusted. The row stays queued and
/// unchanged; the same seed on an ordinary alias is unaffected, and a real
/// stamped `member_assist_send` turn still re-proves.
#[test]
fn cad2207_unstamped_operator_turn_cannot_reprove_into_a_bound_assistant() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let reply_dir = fx.dir.path().join("reprove-reply-cwd");
    std::fs::create_dir_all(&reply_dir).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "ordinary-reply",
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: reply_dir.to_str().unwrap(),
            sandbox: "workspace-write",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();

    // The bypass shape: a bound assistant's queued turn with source
    // `operator` and no member authority stamp.
    let (duplicate, state) = fx
        .shared
        .store
        .enqueue(
            &alice.alias,
            "operator-queued private turn",
            None,
            "cad2207-operator-turn",
            "operator",
        )
        .unwrap();
    assert!(!duplicate);
    let seeded = fx
        .shared
        .store
        .message("cad2207-operator-turn")
        .unwrap()
        .expect("the seeded operator turn was not stored");
    assert_eq!(seeded.state, state);
    assert!(
        fx.shared
            .store
            .message_member_authority(&seeded.id)
            .unwrap()
            .is_none(),
        "the fixture's operator turn carried a member stamp"
    );

    let refused = fx.shared.member_message_reprove(&seeded);
    assert!(
        refused.is_err(),
        "an unstamped operator turn re-proved for a member's assistant: {refused:?}"
    );
    let after = fx
        .shared
        .store
        .message("cad2207-operator-turn")
        .unwrap()
        .expect("the refused turn was deleted");
    assert_eq!(
        after.state, seeded.state,
        "a refused reprove changed the queued turn"
    );
    assert!(
        after.result.is_none(),
        "a refused reprove wrote a result onto the turn"
    );
    assert!(
        fx.shared
            .store
            .messages(&alice.alias)
            .unwrap()
            .iter()
            .any(|m| m.id == "cad2207-operator-turn"),
        "the refused turn left the assistant's queue"
    );

    // Historical `agent_send` (source `user`) and dispatch (source `dispatch`,
    // with a reply routed into another mailbox) are the same bypass under a
    // different label: an unstamped turn has no member authority, whatever it
    // claims its source is.
    for (id, source, reply_to) in [
        ("cad2207-user-turn", "user", None),
        ("cad2207-dispatch-turn", "dispatch", Some("ordinary-worker")),
    ] {
        fx.shared
            .store
            .enqueue(
                &alice.alias,
                "historical unstamped turn",
                reply_to,
                id,
                source,
            )
            .unwrap();
        let row = fx
            .shared
            .store
            .message(id)
            .unwrap()
            .unwrap_or_else(|| panic!("historical {source} turn was not stored"));
        assert_eq!(row.reply_to.as_deref(), reply_to);
        assert!(
            fx.shared
                .store
                .message_member_authority(&row.id)
                .unwrap()
                .is_none(),
            "the historical {source} turn carried a member stamp"
        );
        let refused = fx.shared.member_message_reprove(&row);
        assert!(
            refused.is_err(),
            "a historical unstamped {source} turn re-proved for a member's assistant: \
             {refused:?}"
        );
        let after = fx
            .shared
            .store
            .message(id)
            .unwrap()
            .unwrap_or_else(|| panic!("the refused {source} turn was deleted"));
        assert_eq!(
            after.state, row.state,
            "a refused {source} reprove changed the queued turn"
        );
        assert!(
            after.result.is_none(),
            "a refused {source} reprove wrote a result"
        );
    }

    // Positive: the same historical sources on a non-bound ordinary alias are
    // ordinary work and re-prove.
    for (id, source, reply_to) in [
        ("cad2207-ordinary-turn", "operator", None),
        ("cad2207-ordinary-user", "user", None),
        (
            "cad2207-ordinary-dispatch",
            "dispatch",
            Some("ordinary-reply"),
        ),
    ] {
        fx.shared
            .store
            .enqueue(
                "ordinary-worker",
                "ordinary historical turn",
                reply_to,
                id,
                source,
            )
            .unwrap();
        let ordinary = fx
            .shared
            .store
            .message(id)
            .unwrap()
            .unwrap_or_else(|| panic!("ordinary {source} turn was not stored"));
        fx.shared
            .member_message_reprove(&ordinary)
            .unwrap_or_else(|e| panic!("an ordinary agent's {source} turn was refused: {e}"));
    }

    // Positive: a real member send is stamped and still re-proves.
    fx.op(
        "member_assist_send",
        json!({
            "member_envelope": fx.member_envelope("sub-alice"),
            "text": "real member turn",
            "message": "cad2207-member-turn",
        }),
    );
    let member_turn = fx
        .shared
        .store
        .message("cad2207-member-turn")
        .unwrap()
        .expect("the member send did not queue its turn");
    assert!(
        fx.shared
            .store
            .message_member_authority(&member_turn.id)
            .unwrap()
            .is_some(),
        "the real member send carried no member authority stamp"
    );
    fx.shared
        .member_message_reprove(&member_turn)
        .unwrap_or_else(|e| panic!("the stamped member turn did not re-prove: {e}"));
}

/// CAD-2207: operator reconcile/cancel must not touch a member assistant's
/// private message. The landed contract refuses both before any write or
/// route, so the row keeps its state/body/result/error and no unowned mailbox
/// receives the outcome. Ordinary messages keep the normal reconcile/cancel
/// path, and the owning member's own signed/assistant read still works.
#[test]
fn cad2207_private_message_reconcile_and_cancel_refuse_without_mutation() {
    const PRIVATE_BODY: &str = "cad2207 private queued body";
    const PRIVATE_ERROR: &str = "cad2207 private provider error";
    const PRIVATE_RESULT: &str = "cad2207 private result";
    const ORDINARY_BODY: &str = "cad2207 ordinary queued body";
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");

    let seed = |id: &str, state: &str, body: &str, reply_to: Option<&str>| {
        fx.shared
            .store
            .enqueue(&alice.alias, body, reply_to, id, "user")
            .unwrap();
        fx.shared
            .store
            .fixture_write(|tx| {
                tx.execute(
                    "UPDATE messages SET state=?, error=?, result=? WHERE id=?",
                    rusqlite::params![
                        state,
                        PRIVATE_ERROR,
                        json!({"private": PRIVATE_RESULT}).to_string(),
                        id
                    ],
                )?;
                Ok(())
            })
            .unwrap();
    };
    let snapshot = |id: &str| {
        let message = fx
            .shared
            .store
            .message(id)
            .unwrap()
            .expect("message vanished");
        (
            message.state,
            message.error,
            message.result.map(|value| value.to_string()),
        )
    };

    // Private reconcile: an operator status/note/sha is refused before the
    // store write and before any reply_to routing.
    seed(
        "cad2207-private-reconcile",
        "unknown",
        PRIVATE_BODY,
        Some("ordinary-worker"),
    );
    let reconcile_before = snapshot("cad2207-private-reconcile");
    let reconcile = fx.op_err(
        "message_reconcile",
        json!({
            "message": "cad2207-private-reconcile",
            "status": "completed",
            "note": "cad2207 operator note",
            "sha": "0123456789abcdef0123456789abcdef01234567",
        }),
    );
    assert!(
        reconcile.is_err(),
        "an operator reconciled a member assistant's private unknown turn: {reconcile:?}"
    );
    assert_eq!(
        snapshot("cad2207-private-reconcile"),
        reconcile_before,
        "a refused private reconcile mutated the message"
    );
    // Private cancel: an operator reason is refused before the store write and
    // before any reply_to notice.
    seed(
        "cad2207-private-cancel",
        "queued",
        PRIVATE_BODY,
        Some("ordinary-worker"),
    );
    let cancel_before = snapshot("cad2207-private-cancel");
    let cancel = fx.op_err(
        "message_cancel",
        json!({
            "message": "cad2207-private-cancel",
            "by": "operator",
            "reason": "cad2207 operator cancel",
        }),
    );
    assert!(
        cancel.is_err(),
        "an operator cancelled a member assistant's private queued turn: {cancel:?}"
    );
    assert_eq!(
        snapshot("cad2207-private-cancel"),
        cancel_before,
        "a refused private cancel mutated the message"
    );
    assert!(
        !fx.shared
            .store
            .messages("ordinary-worker")
            .unwrap()
            .iter()
            .any(|m| m.body.contains(PRIVATE_BODY)
                || m.body.contains(PRIVATE_ERROR)
                || m.body.contains(PRIVATE_RESULT)),
        "a refused private reconcile/cancel routed private content to an unowned mailbox"
    );

    // No caller may mutate the private rows through these verbs.
    for who in [
        Asserted::Agent("ordinary-worker".to_string()),
        Asserted::Unproven,
    ] {
        let reconciled = fx.call(
            who.clone(),
            "message_reconcile",
            json!({"message": "cad2207-private-reconcile", "status": "completed"}),
        );
        assert!(
            reconciled.is_err(),
            "{who:?} reconciled a member assistant's private turn: {reconciled:?}"
        );
        assert_eq!(
            snapshot("cad2207-private-reconcile"),
            reconcile_before,
            "{who:?} mutated the private unknown turn"
        );
        let cancelled = fx.call(
            who.clone(),
            "message_cancel",
            json!({"message": "cad2207-private-cancel", "by": "ordinary-worker"}),
        );
        assert!(
            cancelled.is_err(),
            "{who:?} cancelled a member assistant's private turn: {cancelled:?}"
        );
        assert_eq!(
            snapshot("cad2207-private-cancel"),
            cancel_before,
            "{who:?} mutated the private queued turn"
        );
    }

    // Ordinary messages keep the normal reconcile/cancel path and receipt.
    fx.shared
        .store
        .enqueue(
            "ordinary-worker",
            ORDINARY_BODY,
            None,
            "cad2207-ordinary-reconcile",
            "user",
        )
        .unwrap();
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE messages SET state='unknown' WHERE id='cad2207-ordinary-reconcile'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let ordinary_reconcile = fx
        .op_err(
            "message_reconcile",
            json!({"message": "cad2207-ordinary-reconcile", "status": "completed",
                   "note": "ordinary reconcile"}),
        )
        .unwrap_or_else(|e| panic!("the ordinary message reconcile was refused: {e}"));
    assert!(
        ordinary_reconcile.to_string().contains(ORDINARY_BODY),
        "the ordinary reconcile receipt lost its message body: {ordinary_reconcile}"
    );
    assert_eq!(
        fx.shared
            .store
            .message("cad2207-ordinary-reconcile")
            .unwrap()
            .unwrap()
            .state,
        "completed"
    );

    fx.shared
        .store
        .enqueue(
            "ordinary-worker",
            ORDINARY_BODY,
            None,
            "cad2207-ordinary-cancel",
            "user",
        )
        .unwrap();
    let ordinary_cancel = fx
        .op_err(
            "message_cancel",
            json!({"message": "cad2207-ordinary-cancel", "by": "operator",
                   "reason": "ordinary cancel"}),
        )
        .unwrap_or_else(|e| panic!("the ordinary message cancel was refused: {e}"));
    assert!(
        ordinary_cancel.to_string().contains(ORDINARY_BODY),
        "the ordinary cancel receipt lost its message body: {ordinary_cancel}"
    );
    assert_eq!(
        fx.shared
            .store
            .message("cad2207-ordinary-cancel")
            .unwrap()
            .unwrap()
            .state,
        "cancelled"
    );

    // The owning member's own paths remain: signed history still reads and its
    // assistant still reads its own queued body.
    fx.op_err(
        "member_assist_history",
        json!({"member_envelope": fx.member_envelope("sub-alice")}),
    )
    .unwrap_or_else(|e| panic!("the member lost its signed history after refusals: {e}"));
    let own = fx
        .as_assistant(
            &alice.alias,
            "message_read",
            json!({"message": "cad2207-private-cancel"}),
        )
        .unwrap_or_else(|e| panic!("the assistant lost its own message read: {e}"));
    assert!(
        own["text"]
            .as_str()
            .is_some_and(|text| text.contains(PRIVATE_BODY)),
        "the assistant's own read lost its queued body: {own}"
    );
}

/// CAD-2207: a task kickoff is trusted only when its job is the member's own
/// assist-bound request. `tasks.dispatch_message`, `assignee` and a matching
/// reply_to are not owner permission: an attacker PM's job that merely names
/// the victim assistant must fail the take path with no mailbox route.
#[test]
fn cad2207_task_kickoff_provenance_requires_member_owned_job() {
    const SPEC_TEXT: &str = "cad2207 attacker task spec secret";
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let attacker_dir = fx.dir.path().join("attacker-cwd");
    std::fs::create_dir_all(&attacker_dir).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "attacker-pm",
            provider: "pi",
            endpoint_kind: "managed",
            role: "pm",
            cwd: attacker_dir.to_str().unwrap(),
            sandbox: "workspace-write",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let spec = fx.dir.path().join("attacker-spec.md");
    std::fs::write(&spec, SPEC_TEXT).unwrap();
    let spec_sha = super::sha256_file(&spec).unwrap();
    let snapshot = |id: &str| {
        let message = fx
            .shared
            .store
            .message(id)
            .unwrap()
            .expect("message vanished");
        (
            message.state,
            message.result.map(|value| value.to_string()),
            message.error,
            message.body,
            message.reply_to,
            message.task_id,
        )
    };
    let set_task = |task: &str, assignee: &str, dispatch: &str| {
        fx.shared
            .store
            .fixture_write(|tx| {
                tx.execute(
                    "UPDATE tasks SET assignee=?, state='dispatched', dispatch_message=? WHERE id=?",
                    rusqlite::params![assignee, dispatch, task],
                )?;
                Ok(())
            })
            .unwrap();
    };

    // Attacker PM's ordinary job: no assist_requests row. The task names the
    // victim assistant exactly and the kickoff id is the exact
    // dispatch_message, yet the take must refuse.
    let (_, attacker_job) = fx
        .shared
        .store
        .create_job(
            "cad2207-attacker-job",
            Some("attacker job"),
            spec.to_str().unwrap(),
            &spec_sha,
            "attacker-pm",
            None,
            None,
            None,
            2,
            None,
            Some("attacker task"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let attacker_task = format!("{}-t1", attacker_job.id);
    let attacker_kickoff = "cad2207-attacker-kickoff";
    fx.shared
        .store
        .enqueue_task(
            &alice.alias,
            SPEC_TEXT,
            Some("attacker-pm"),
            attacker_kickoff,
            "job_dispatch",
            Some(&attacker_task),
        )
        .unwrap();
    set_task(&attacker_task, &alice.alias, attacker_kickoff);
    assert_eq!(
        fx.shared
            .store
            .task(&attacker_task)
            .unwrap()
            .dispatch_message
            .as_deref(),
        Some(attacker_kickoff),
        "the fixture did not record the exact task dispatch message"
    );
    let message = fx
        .shared
        .store
        .message(attacker_kickoff)
        .unwrap()
        .expect("attacker kickoff missing");
    let before = snapshot(attacker_kickoff);
    let refused = fx.shared.member_message_reprove(&message);
    assert!(
        refused.is_err(),
        "an attacker job's kickoff naming the victim assistant took the turn: {refused:?}"
    );
    assert_eq!(
        snapshot(attacker_kickoff),
        before,
        "a refused attacker kickoff mutated its queued turn"
    );
    assert!(
        !fx.shared
            .store
            .messages("attacker-pm")
            .unwrap()
            .iter()
            .any(|m| m.body.contains(SPEC_TEXT)),
        "an attacker task kickoff routed its spec to the attacker mailbox"
    );

    // Member-owned job, but the kickoff routes the reply to an arbitrary
    // mailbox: still refused.
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope("sub-alice"),
            "title": "cad2207 member job",
            "request_key": "cad2207-member-job",
            "body": "member spec",
        }),
    );
    let member_job = request["job"].as_str().expect("member job id").to_string();
    let member_task = format!("{member_job}-t1");
    let bad_reply_kickoff = "cad2207-member-bad-reply";
    fx.shared
        .store
        .enqueue_task(
            &alice.alias,
            "member kickoff body",
            Some("attacker-pm"),
            bad_reply_kickoff,
            "job_dispatch",
            Some(&member_task),
        )
        .unwrap();
    set_task(&member_task, &alice.alias, bad_reply_kickoff);
    let message = fx
        .shared
        .store
        .message(bad_reply_kickoff)
        .unwrap()
        .expect("member bad-reply kickoff missing");
    let before = snapshot(bad_reply_kickoff);
    let refused = fx.shared.member_message_reprove(&message);
    assert!(
        refused.is_err(),
        "a member-owned kickoff routed to an arbitrary mailbox took the turn: {refused:?}"
    );
    assert_eq!(
        snapshot(bad_reply_kickoff),
        before,
        "a refused bad-reply kickoff mutated its queued turn"
    );
    assert!(
        !fx.shared
            .store
            .messages("attacker-pm")
            .unwrap()
            .iter()
            .any(|m| m.body.contains(SPEC_TEXT)),
        "a bad-reply member kickoff routed private content to the attacker mailbox"
    );

    // Even a member-owned task kickoff is refused under the final typed
    // contract: no durable member-approved assignment/sharing receipt exists
    // yet, so every unstamped task kickoff to a bound assistant fails and the
    // row is left untouched. This is the reported temporary blocker.
    let good_kickoff = "cad2207-member-good";
    fx.shared
        .store
        .enqueue_task(
            &alice.alias,
            "member good kickoff",
            None,
            good_kickoff,
            "job_dispatch",
            Some(&member_task),
        )
        .unwrap();
    set_task(&member_task, &alice.alias, good_kickoff);
    let message = fx
        .shared
        .store
        .message(good_kickoff)
        .unwrap()
        .expect("member good kickoff missing");
    let before = snapshot(good_kickoff);
    let refused = fx.shared.member_message_reprove(&message);
    assert!(
        refused.is_err(),
        "an unstamped member-owned task kickoff took the turn before a durable \
         member-approved receipt exists: {refused:?}"
    );
    assert_eq!(
        snapshot(good_kickoff),
        before,
        "a refused member-owned task kickoff mutated its queued turn"
    );

    // Ordinary job/task kickoff on a non-bound alias is unaffected.
    let (_, ordinary_job) = fx
        .shared
        .store
        .create_job(
            "cad2207-ordinary-job",
            Some("ordinary job"),
            spec.to_str().unwrap(),
            &spec_sha,
            "ordinary-worker",
            None,
            None,
            None,
            2,
            None,
            Some("ordinary task"),
            None,
            None,
            None,
            Some("ordinary-worker"),
            None,
        )
        .unwrap();
    let ordinary_task = format!("{}-t1", ordinary_job.id);
    let ordinary_kickoff = "cad2207-ordinary-kickoff";
    fx.shared
        .store
        .enqueue_task(
            "ordinary-worker",
            "ordinary kickoff body",
            None,
            ordinary_kickoff,
            "job_dispatch",
            Some(&ordinary_task),
        )
        .unwrap();
    set_task(&ordinary_task, "ordinary-worker", ordinary_kickoff);
    let message = fx
        .shared
        .store
        .message(ordinary_kickoff)
        .unwrap()
        .expect("ordinary kickoff missing");
    assert!(fx
        .shared
        .store
        .assistant_for_alias("ordinary-worker")
        .unwrap()
        .is_none());
    fx.shared
        .member_message_reprove(&message)
        .unwrap_or_else(|e| panic!("an ordinary task kickoff was refused: {e}"));
}

/// CAD-2207: `agent_unfence` is a bulk reconcile that bypasses the private
/// `message_reconcile` guard. It must refuse a member-bound alias before any
/// status write or reply routing; ordinary aliases keep the operator recovery.
#[test]
fn cad2207_agent_unfence_refuses_private_unknown_recovery() {
    const PRIVATE_BODY: &str = "cad2207 private unknown body";
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let attacker_dir = fx.dir.path().join("unfence-attacker-cwd");
    std::fs::create_dir_all(&attacker_dir).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "attacker-pm",
            provider: "pi",
            endpoint_kind: "managed",
            role: "pm",
            cwd: attacker_dir.to_str().unwrap(),
            sandbox: "workspace-write",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let snapshot = |id: &str| {
        let message = fx
            .shared
            .store
            .message(id)
            .unwrap()
            .expect("message vanished");
        (
            message.state,
            message.result.map(|value| value.to_string()),
            message.error,
            message.body,
            message.reply_to,
        )
    };

    let private_id = "cad2207-unfence-private";
    fx.shared
        .store
        .enqueue(
            &alice.alias,
            PRIVATE_BODY,
            Some("attacker-pm"),
            private_id,
            "user",
        )
        .unwrap();
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE messages SET state='unknown', error=? WHERE id=?",
                rusqlite::params!["cad2207 private unknown error", private_id],
            )?;
            Ok(())
        })
        .unwrap();
    let before = snapshot(private_id);
    let refused = fx.op_err(
        "agent_unfence",
        json!({"alias": alice.alias, "status": "failed", "note": "operator note"}),
    );
    assert!(
        refused.is_err(),
        "agent_unfence reconciled a member assistant's unknown turn: {refused:?}"
    );
    assert_eq!(
        snapshot(private_id),
        before,
        "a refused private agent_unfence mutated the unknown message"
    );
    assert!(
        !fx.shared
            .store
            .messages("attacker-pm")
            .unwrap()
            .iter()
            .any(|m| m.body.contains(PRIVATE_BODY)),
        "a refused private agent_unfence routed private content to a nonowner"
    );

    // Ordinary aliases keep the operator recovery.
    let ordinary_id = "cad2207-unfence-ordinary";
    fx.shared
        .store
        .enqueue(
            "ordinary-worker",
            "ordinary unknown body",
            None,
            ordinary_id,
            "user",
        )
        .unwrap();
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE messages SET state='unknown' WHERE id=?",
                rusqlite::params![ordinary_id],
            )?;
            Ok(())
        })
        .unwrap();
    let recovered = fx.op(
        "agent_unfence",
        json!({"alias": "ordinary-worker", "status": "failed", "note": "ordinary"}),
    );
    assert!(
        recovered["reconciled"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(ordinary_id))),
        "the ordinary agent_unfence lost its reconciled id: {recovered}"
    );
    assert_eq!(
        fx.shared.store.message(ordinary_id).unwrap().unwrap().state,
        "failed"
    );
}

/// CAD-2207: an `assist_job_event` notice needs the committed delivery
/// receipt for its exact intent/body/recipient, and a subscriber needs the
/// owner's LIVE share. A legacy NULL-sender row with the right task/member but
/// no receipt, and a notice queued before the share was revoked, must both
/// fail at turn admission without mutation or routing.
#[test]
fn cad2207_assist_notice_requires_delivery_receipt_and_live_share() {
    const FORGED_BODY: &str = "cad2207 forged notice body";
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let ben = fx.member_runtime("sub-ben");
    let alice_envelope = fx.member_envelope("sub-alice");
    let ben_envelope = fx.member_envelope("sub-ben");
    let snapshot = |id: &str| {
        let message = fx
            .shared
            .store
            .message(id)
            .unwrap()
            .expect("message vanished");
        (
            message.state,
            message.result.map(|value| value.to_string()),
            message.error,
            message.body,
            message.sender,
            message.sender_uid,
            message.reply_to,
        )
    };

    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": alice_envelope,
            "title": "cad2207 notice job",
            "request_key": "cad2207-notice-job",
            "body": "cad2207 notice spec",
        }),
    );
    let job = request["job"].as_str().expect("member job id").to_string();
    let task = format!("{job}-t1");
    fx.op(
        "member_job_share",
        json!({"member_envelope": alice_envelope, "job": job, "enabled": true}),
    );
    fx.op(
        "member_job_subscribe",
        json!({"member_envelope": ben_envelope, "job": job}),
    );
    assert!(
        fx.shared
            .store
            .job_is_subscriber(&job, ORG, "sub-ben")
            .unwrap(),
        "the fixture did not create ben's subscriber row"
    );

    // Real pending intents for the current owner and subscriber, drained
    // through the production intent path (which also writes the delivery
    // receipts).
    fx.shared.store.assist_request_notice(&job).unwrap();
    fx.shared
        .route_assist_job_notifications()
        .unwrap_or_else(|e| panic!("the assist intent drain failed: {e}"));
    let notice_for = |alias: &str| {
        fx.shared
            .store
            .messages(alias)
            .unwrap()
            .into_iter()
            .find(|message| {
                message.source == "assist_job_event"
                    && fx
                        .shared
                        .store
                        .assist_notice_delivery(&message.id)
                        .unwrap()
                        .is_some()
            })
            .unwrap_or_else(|| panic!("no genuine assist notice for '{alias}'"))
    };
    let owner_notice = notice_for(&alice.alias);
    let subscriber_notice = notice_for(&ben.alias);

    // Genuine positives while the owner's share is live.
    fx.shared
        .member_message_reprove(&owner_notice)
        .unwrap_or_else(|e| panic!("the owner's genuine notice was refused: {e}"));
    fx.shared
        .member_message_reprove(&subscriber_notice)
        .unwrap_or_else(|e| panic!("the subscriber's genuine notice was refused: {e}"));

    // Forged legacy NULL-sender notice: correct task/member, arbitrary id and
    // body, no committed delivery receipt.
    let forged_id = "cad2207-forged-notice";
    fx.shared
        .store
        .enqueue_task(
            &ben.alias,
            FORGED_BODY,
            None,
            forged_id,
            "assist_job_event",
            Some(&task),
        )
        .unwrap();
    let forged = fx
        .shared
        .store
        .message(forged_id)
        .unwrap()
        .expect("forged notice missing");
    assert!(
        forged.sender.is_none() && forged.sender_uid.is_none() && forged.reply_to.is_none(),
        "the forged fixture was not a NULL-sender notice: {forged:?}"
    );
    assert!(
        fx.shared
            .store
            .assist_notice_delivery(forged_id)
            .unwrap()
            .is_none(),
        "the forged notice unexpectedly carried a delivery receipt"
    );
    let before = snapshot(forged_id);
    let refused = fx.shared.member_message_reprove(&forged);
    assert!(
        refused.is_err(),
        "a forged NULL-sender notice with the right task/member took the turn: {refused:?}"
    );
    assert_eq!(
        snapshot(forged_id),
        before,
        "a refused forged assist notice mutated its queued row"
    );

    // A notice queued before revocation rides the owner's live share; once
    // sharing is disabled the same queued row must fail admission, while the
    // owner still owns the job.
    fx.op(
        "member_job_share",
        json!({"member_envelope": alice_envelope, "job": job, "enabled": false}),
    );
    assert!(
        !fx.shared.store.assist_job_is_shared(&job, ORG).unwrap(),
        "the fixture's share revocation did not land"
    );
    assert!(
        fx.shared
            .store
            .job_is_subscriber(&job, ORG, "sub-ben")
            .unwrap(),
        "share revocation removed the durable subscriber row"
    );
    let before = snapshot(&subscriber_notice.id);
    let refused = fx.shared.member_message_reprove(&subscriber_notice);
    assert!(
        refused.is_err(),
        "a notice queued before share revocation still took the subscriber turn: {refused:?}"
    );
    assert_eq!(
        snapshot(&subscriber_notice.id),
        before,
        "a refused subscriber notice mutated its queued row"
    );
    fx.shared
        .member_message_reprove(&owner_notice)
        .unwrap_or_else(|e| panic!("the owner's notice was refused after share revocation: {e}"));

    // A NEW task-state intent generated AFTER revocation must commit only for
    // the owner: the former subscriber keeps its durable row but gets no new
    // notice or delivery receipt.
    let notice_count = |alias: &str| {
        fx.shared
            .store
            .messages(alias)
            .unwrap()
            .iter()
            .filter(|message| message.source == "assist_job_event")
            .count()
    };
    let latest_notice = |alias: &str| {
        fx.shared
            .store
            .messages(alias)
            .unwrap()
            .into_iter()
            .rfind(|message| {
                message.source == "assist_job_event"
                    && fx
                        .shared
                        .store
                        .assist_notice_delivery(&message.id)
                        .unwrap()
                        .is_some()
            })
            .unwrap_or_else(|| panic!("no delivered notice for '{alias}'"))
    };
    let ben_before = notice_count(&ben.alias);
    let owner_before = notice_count(&alice.alias);
    fx.shared
        .store
        .job_notice(&task, "blocked", "cad2207-after-revoke", "after revoke")
        .unwrap();
    fx.shared
        .route_assist_job_notifications()
        .unwrap_or_else(|e| panic!("the post-revocation intent drain failed: {e}"));
    assert_eq!(
        notice_count(&ben.alias),
        ben_before,
        "a new task-state intent after revocation reached the former subscriber"
    );
    assert_eq!(
        notice_count(&alice.alias),
        owner_before + 1,
        "the owner lost a new task-state notice after revocation"
    );
    fx.shared
        .member_message_reprove(&latest_notice(&alice.alias))
        .unwrap_or_else(|e| panic!("the owner's new post-revocation notice was refused: {e}"));
}

/// CAD-2207: a private effect outcome notice is owner-bound. Only the bound
/// assistant receives it; the committed receipt binds message, effect,
/// assistant tuple and body; a real live notice reproves; and forged/
/// unreceipted, changed-body or foreign-assistant notices are refused.
#[test]
fn cad2207_effect_notice_receipt_is_owner_bound_and_enforced() {
    const PRIVATE: &str = "cad2207 effect notice private body";
    const ORDINARY: &str = "cad2207 effect notice ordinary body";
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let alice = fx.member_runtime("sub-alice");
    let ben = fx.member_runtime("sub-ben");
    let reply_dir = fx.dir.path().join("effect-notice-reply-cwd");
    std::fs::create_dir_all(&reply_dir).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "ordinary-reply",
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: reply_dir.to_str().unwrap(),
            sandbox: "workspace-write",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    for alias in [alice.alias.as_str(), "ordinary-worker"] {
        fx.shared
            .store
            .fixture_write(|tx| {
                tx.execute(
                    "UPDATE agents SET params=json_set(COALESCE(params,'{}'),'$.upstream',?) \
                     WHERE alias=?",
                    rusqlite::params!["ordinary-reply", alias],
                )?;
                Ok(())
            })
            .unwrap();
    }

    let effect_id_for = |request: &str| {
        fx.shared
            .store
            .effect_by_request(request)
            .unwrap()
            .unwrap_or_else(|| panic!("effect row missing for {request}"))
            .effect_id
    };
    let notice_id = |effect_id: &str| crate::proto::daemon_message_id("effect", effect_id);
    let decline = |alias: &str, request: &str| {
        fx.call(
            Asserted::Operator,
            "agent_respond",
            json!({"alias": alias, "request": request, "decision": "decline"}),
        )
        .unwrap_or_else(|e| panic!("safe decline for {request} was refused: {e}"));
    };
    let snapshot = |message: &crate::store::Message| {
        (
            message.state.clone(),
            message.result.clone(),
            message.body.clone(),
            message.alias.clone(),
        )
    };

    let real_request = cad2207_stage_effect(&fx, &alice.alias, "cad2207-effect-notice", PRIVATE);
    let changed_request = cad2207_stage_effect(&fx, &alice.alias, "cad2207-effect-body", PRIVATE);
    let foreign_request =
        cad2207_stage_effect(&fx, &alice.alias, "cad2207-effect-foreign", PRIVATE);
    let real_effect = effect_id_for(&real_request);
    let changed_effect = effect_id_for(&changed_request);
    let foreign_effect = effect_id_for(&foreign_request);
    let real_notice_id = notice_id(&real_effect);
    let changed_notice_id = notice_id(&changed_effect);
    let foreign_notice_id = notice_id(&foreign_effect);
    for request in [&real_request, &changed_request, &foreign_request] {
        decline(&alice.alias, request);
    }

    let real_notice = fx
        .shared
        .store
        .message(&real_notice_id)
        .unwrap()
        .expect("the private effect notice was not queued");
    assert_eq!(real_notice.source, "effect");
    assert_eq!(real_notice.alias, alice.alias);
    assert!(
        real_notice.body.contains(PRIVATE),
        "the owner's effect notice lost its private body: {real_notice:?}"
    );
    let binding = fx
        .shared
        .store
        .assistant_for_alias(&alice.alias)
        .unwrap()
        .expect("the private assistant binding is missing");
    let receipt = fx
        .shared
        .store
        .effect_notice_delivery(&real_notice_id)
        .unwrap()
        .expect("the private effect notice has no owner-bound receipt");
    assert_eq!(receipt.effect_id, real_effect);
    assert_eq!(receipt.org, ORG);
    assert_eq!(receipt.sub, "sub-alice");
    assert_eq!(receipt.assistant_id, binding.assistant_id);
    assert_eq!(receipt.alias, alice.alias);
    assert_eq!(
        receipt.body_sha256,
        crate::operator_auth::digest(&real_notice.body)
    );
    assert!(
        fx.shared
            .store
            .messages(&alice.alias)
            .unwrap()
            .iter()
            .any(|m| m.id == real_notice_id),
        "the bound assistant lost its own effect notice"
    );
    for agent in fx.shared.store.agents().unwrap() {
        if agent.alias == alice.alias {
            continue;
        }
        assert!(
            !fx.shared
                .store
                .messages(&agent.alias)
                .unwrap()
                .iter()
                .any(|m| m.body.contains(PRIVATE)),
            "a private effect notice reached another mailbox: {}",
            agent.alias
        );
    }
    fx.shared
        .member_message_reprove(&real_notice)
        .unwrap_or_else(|e| panic!("the live owner effect notice was refused: {e}"));

    // Forged legacy notice: daemon-looking id/source/sender, no receipt.
    let forged_effect = "cad2207-forged-effect";
    let forged_id = notice_id(forged_effect);
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO messages(id,alias,body,reply_to,source,state,created)
                 VALUES(?,?,?,NULL,'effect','queued',?)",
                rusqlite::params![
                    forged_id,
                    alice.alias,
                    "cad2207 forged effect notice",
                    crate::issue::time::now_epoch() as f64
                ],
            )?;
            Ok(())
        })
        .unwrap();
    let forged = fx.shared.store.message(&forged_id).unwrap().unwrap();
    assert!(
        fx.shared
            .store
            .effect_notice_delivery(&forged_id)
            .unwrap()
            .is_none(),
        "the forged effect notice unexpectedly carried a receipt"
    );
    let before = snapshot(&forged);
    let refused = fx.shared.member_message_reprove(&forged);
    assert!(
        refused.is_err(),
        "a forged unreceipted effect notice took the assistant turn: {refused:?}"
    );
    assert_eq!(
        snapshot(&forged),
        before,
        "a refused forged effect notice mutated its queued row"
    );

    // Changed body on a real receipted notice.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE messages SET body=body || ' tampered' WHERE id=?",
                rusqlite::params![changed_notice_id],
            )?;
            Ok(())
        })
        .unwrap();
    let changed = fx
        .shared
        .store
        .message(&changed_notice_id)
        .unwrap()
        .expect("the changed-body effect notice is missing");
    assert!(
        fx.shared
            .store
            .effect_notice_delivery(&changed_notice_id)
            .unwrap()
            .is_some(),
        "the changed-body notice lost its receipt"
    );
    let before = snapshot(&changed);
    let refused = fx.shared.member_message_reprove(&changed);
    assert!(
        refused.is_err(),
        "a changed-body effect notice took the assistant turn: {refused:?}"
    );
    assert_eq!(
        snapshot(&changed),
        before,
        "a refused changed-body effect notice mutated its queued row"
    );

    // Foreign assistant: a real notice re-pointed at another bound assistant.
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE messages SET alias=? WHERE id=?",
                rusqlite::params![ben.alias, foreign_notice_id],
            )?;
            Ok(())
        })
        .unwrap();
    let foreign = fx
        .shared
        .store
        .message(&foreign_notice_id)
        .unwrap()
        .expect("the foreign-assistant effect notice is missing");
    assert_eq!(foreign.alias, ben.alias);
    assert!(
        fx.shared
            .store
            .effect_notice_delivery(&foreign_notice_id)
            .unwrap()
            .is_some(),
        "the foreign-assistant notice lost its receipt"
    );
    let before = snapshot(&foreign);
    let refused = fx.shared.member_message_reprove(&foreign);
    assert!(
        refused.is_err(),
        "a foreign-assistant effect notice took the turn: {refused:?}"
    );
    assert_eq!(
        snapshot(&foreign),
        before,
        "a refused foreign-assistant effect notice mutated its queued row"
    );

    // Ordinary effect routing remains positive through the same upstream.
    let ordinary_request = cad2207_stage_effect(
        &fx,
        "ordinary-worker",
        "cad2207-effect-notice-ordinary",
        ORDINARY,
    );
    let before = fx.shared.store.messages("ordinary-reply").unwrap().len();
    decline("ordinary-worker", &ordinary_request);
    let after = fx.shared.store.messages("ordinary-reply").unwrap();
    assert!(
        after.len() > before,
        "the ordinary effect notice did not reach its upstream"
    );
    assert!(
        after.iter().any(|m| m.body.contains(ORDINARY)),
        "the ordinary upstream notice lost its visible body"
    );
    assert!(
        after.iter().all(|m| !m.body.contains(PRIVATE)),
        "a private effect notice reached the ordinary upstream mailbox"
    );

    // Live member authority is required even with a matching receipt.
    fx.remove_member("sub-alice");
    let refused = fx.shared.member_message_reprove(&real_notice);
    assert!(
        refused.is_err(),
        "a revoked member's effect notice still took the turn: {refused:?}"
    );
    let _ = ben;
}

/// CAD-2207: a retained effect row must never inherit a later member
/// binding through alias reuse. The old ordinary effect has no stage-time
/// owner, so after the agent is removed and the alias rebound its outcome is
/// suppressed (no receipt, no mailbox); a genuinely new effect staged after
/// the bind records its owner and reaches that owner exactly once.
#[test]
fn cad2207_effect_alias_reuse_cannot_inherit_the_old_owner() {
    const OLD_SECRET: &str = "cad2207 old ordinary effect secret";
    const NEW_SECRET: &str = "cad2207 new member effect secret";
    let alias = "reused-effect";
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let old_dir = fx.dir.path().join("reused-old-cwd");
    std::fs::create_dir_all(&old_dir).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias,
            provider: "inbox",
            endpoint_kind: "inbox",
            role: "worker",
            cwd: old_dir.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();

    let old_request = cad2207_stage_effect(&fx, alias, "cad2207-old-effect", OLD_SECRET);
    let old_effect = fx
        .shared
        .store
        .effect_by_request(&old_request)
        .unwrap()
        .unwrap()
        .effect_id;
    assert!(
        fx.shared
            .store
            .effect_member_owner(&old_effect)
            .unwrap()
            .is_none(),
        "an effect staged under a non-member alias recorded an owner"
    );
    fx.shared
        .store
        .remove_agent(alias, true, &json!({"by": "operator"}))
        .unwrap();
    assert!(
        fx.shared.store.agent_opt(alias).unwrap().is_none(),
        "the fixture did not remove the old ordinary agent row"
    );

    fx.authority
        .lock()
        .unwrap()
        .members
        .insert("sub-carol".to_string(), MemberAuthority::live("sub-carol"));
    fx.shared
        .store
        .assistant_bind(ORG, "sub-carol", "assist_sub-carol", alias, Some("rev0"))
        .unwrap();
    let carol_envelope = fx.member_envelope("sub-carol");
    let carol_state = fx.op(
        "member_assist_state",
        json!({"member_envelope": carol_envelope}),
    );
    assert_eq!(carol_state["assistant"]["alias"].as_str(), Some(alias));
    let carol_binding = fx
        .shared
        .store
        .assistant_for_alias(alias)
        .unwrap()
        .expect("the rebound binding is missing");
    let carol_uid = fx.shared.store.agent(alias).unwrap().uid;

    // Old effect outcome: RPC may succeed, but no receipt/mailbox copy may
    // reach the rebound member.
    let old_notice_id = crate::proto::daemon_message_id("effect", &old_effect);
    fx.call(
        Asserted::Operator,
        "agent_respond",
        json!({"alias": alias, "request": old_request, "decision": "decline"}),
    )
    .unwrap_or_else(|e| panic!("the old effect's safe decline was refused: {e}"));
    assert!(
        fx.shared
            .store
            .effect_member_owner(&old_effect)
            .unwrap()
            .is_none(),
        "the old effect acquired a stage-time owner retroactively"
    );
    assert!(
        fx.shared
            .store
            .effect_notice_delivery(&old_notice_id)
            .unwrap()
            .is_none(),
        "the old effect minted a delivery receipt for the new member"
    );
    assert!(
        !fx.shared
            .store
            .messages(alias)
            .unwrap()
            .iter()
            .any(|m| m.id == old_notice_id || m.body.contains(OLD_SECRET)),
        "the old ordinary effect leaked into the rebound member's mailbox"
    );
    for agent in fx.shared.store.agents().unwrap() {
        assert!(
            !fx.shared
                .store
                .messages(&agent.alias)
                .unwrap()
                .iter()
                .any(|m| m.body.contains(OLD_SECRET)),
            "the old ordinary effect leaked into mailbox '{}'",
            agent.alias
        );
    }

    // New effect after the bind: stage-time owner + one receipted notice.
    let new_request = cad2207_stage_effect(&fx, alias, "cad2207-carol-effect", NEW_SECRET);
    let new_effect = fx
        .shared
        .store
        .effect_by_request(&new_request)
        .unwrap()
        .unwrap()
        .effect_id;
    let owner = fx
        .shared
        .store
        .effect_member_owner(&new_effect)
        .unwrap()
        .expect("the new member effect has no stage-time owner");
    assert_eq!(owner.effect_id, new_effect);
    assert_eq!(owner.org, ORG);
    assert_eq!(owner.sub, "sub-carol");
    assert_eq!(owner.assistant_id, carol_binding.assistant_id);
    assert_eq!(owner.alias, alias);
    assert_eq!(owner.agent_uid, carol_uid);

    fx.call(
        Asserted::Operator,
        "agent_respond",
        json!({"alias": alias, "request": new_request, "decision": "decline"}),
    )
    .unwrap_or_else(|e| panic!("the new member effect's safe decline was refused: {e}"));
    let new_notice_id = crate::proto::daemon_message_id("effect", &new_effect);
    let notice = fx
        .shared
        .store
        .message(&new_notice_id)
        .unwrap()
        .expect("the new member effect produced no notice");
    assert_eq!(notice.alias, alias);
    assert!(notice.body.contains(NEW_SECRET));
    assert!(!notice.body.contains(OLD_SECRET));
    let receipt = fx
        .shared
        .store
        .effect_notice_delivery(&new_notice_id)
        .unwrap()
        .expect("the new member effect notice has no receipt");
    assert_eq!(receipt.effect_id, new_effect);
    assert_eq!(receipt.org, ORG);
    assert_eq!(receipt.sub, "sub-carol");
    assert_eq!(receipt.assistant_id, carol_binding.assistant_id);
    assert_eq!(receipt.alias, alias);
    assert_eq!(
        receipt.body_sha256,
        crate::operator_auth::digest(&notice.body)
    );
    assert_eq!(
        fx.shared
            .store
            .messages(alias)
            .unwrap()
            .iter()
            .filter(|m| m.id == new_notice_id)
            .count(),
        1
    );
    fx.shared
        .member_message_reprove(&notice)
        .unwrap_or_else(|e| panic!("the new member effect notice did not reprove: {e}"));
    let history = fx.op(
        "member_assist_history",
        json!({"member_envelope": fx.member_envelope("sub-carol")}),
    );
    assert!(
        !history.to_string().contains(OLD_SECRET),
        "the old ordinary effect reached the new member's history: {history}"
    );
}

/// CAD-2207: a retained `task_failed` event from an ordinary agent must not
/// become readable after its alias is rebound to a member assistant. The own
/// assistant's forward and tail event pages must omit the old reason and show
/// the event created after the binding; an ordinary alias keeps its history.
#[test]
fn cad2207_retained_event_alias_reuse_cannot_read_the_old_owner_stream() {
    const OLD_REASON: &str = "cad2207 old ordinary task failure reason";
    const NEW_REASON: &str = "cad2207 new member task event reason";
    const ORDINARY_REASON: &str = "cad2207 ordinary event reason";
    let alias = "reused-event";
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let old_dir = fx.dir.path().join("reused-event-old-cwd");
    std::fs::create_dir_all(&old_dir).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias,
            provider: "inbox",
            endpoint_kind: "inbox",
            role: "worker",
            cwd: old_dir.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    fx.shared
        .store
        .event_public_scoped(
            alias,
            "task_failed",
            json!({"reason": OLD_REASON}),
            Some("job-old"),
            Some("task-old"),
        )
        .unwrap();
    fx.shared
        .store
        .remove_agent(alias, true, &json!({"by": "operator"}))
        .unwrap();
    fx.authority
        .lock()
        .unwrap()
        .members
        .insert("sub-carol".to_string(), MemberAuthority::live("sub-carol"));
    fx.shared
        .store
        .assistant_bind(ORG, "sub-carol", "assist_sub-carol", alias, Some("rev0"))
        .unwrap();
    let carol = fx.op(
        "member_assist_state",
        json!({"member_envelope": fx.member_envelope("sub-carol")}),
    );
    assert_eq!(carol["assistant"]["alias"].as_str(), Some(alias));
    fx.shared
        .store
        .event_public_scoped(
            alias,
            "task_failed",
            json!({"reason": NEW_REASON}),
            Some("job-new"),
            Some("task-new"),
        )
        .unwrap();

    let forward = fx
        .call(
            Asserted::Agent(alias.to_string()),
            "agent_events",
            json!({"alias": alias, "after": 0}),
        )
        .unwrap_or_else(|e| panic!("the rebound assistant's event page failed: {e}"));
    assert!(
        !forward.to_string().contains(OLD_REASON),
        "the old ordinary task reason appeared in the forward event page: {forward}"
    );
    assert!(
        forward.to_string().contains(NEW_REASON),
        "the new event after binding is missing from the forward page: {forward}"
    );
    let tail = fx
        .call(
            Asserted::Agent(alias.to_string()),
            "agent_events",
            json!({"alias": alias, "tail": true}),
        )
        .unwrap_or_else(|e| panic!("the rebound assistant's tail event page failed: {e}"));
    assert!(
        !tail.to_string().contains(OLD_REASON),
        "the old ordinary task reason appeared in the tail event page: {tail}"
    );
    assert!(
        tail.to_string().contains(NEW_REASON),
        "the new event after binding is missing from the tail page: {tail}"
    );

    // Ordinary alias behavior is unchanged.
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "ordinary-event",
            provider: "inbox",
            endpoint_kind: "inbox",
            role: "worker",
            cwd: old_dir.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    fx.shared
        .store
        .event_public_scoped(
            "ordinary-event",
            "task_failed",
            json!({"reason": ORDINARY_REASON}),
            Some("job-ordinary"),
            Some("task-ordinary"),
        )
        .unwrap();
    let ordinary = fx
        .call(
            Asserted::Agent("ordinary-event".to_string()),
            "agent_events",
            json!({"alias": "ordinary-event", "after": 0}),
        )
        .unwrap_or_else(|e| panic!("the ordinary agent's event page failed: {e}"));
    assert!(
        ordinary.to_string().contains(ORDINARY_REASON),
        "ordinary event history was hidden: {ordinary}"
    );
}

/// CAD-2207: retained terminal mail under a reused alias must not become
/// visible to a newly bound assistant through a known old recipient UID or a
/// NULL-UID legacy row with an equal/later forged timestamp. Own agent_show,
/// message_list and message_read must all exclude it; new current-UID mail
/// stays readable and ordinary aliases keep the legacy behavior.
#[test]
fn cad2207_retained_mail_alias_reuse_cannot_read_the_old_owner_mail() {
    const NEW_BODY: &str = "cad2207 new member current mail";
    const ORDINARY_BODY: &str = "cad2207 ordinary legacy mail";
    let alias = "reused-mail";
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let old_dir = fx.dir.path().join("reused-mail-old-cwd");
    std::fs::create_dir_all(&old_dir).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias,
            provider: "inbox",
            endpoint_kind: "inbox",
            role: "worker",
            cwd: old_dir.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let old_uid = fx.shared.store.agent(alias).unwrap().uid;
    let old_ids = [
        "cad2207-old-known-equal",
        "cad2207-old-known-later",
        "cad2207-old-null-equal",
        "cad2207-old-null-later",
    ];
    for (index, id) in old_ids.iter().enumerate() {
        let recipient = if index < 2 {
            Some(old_uid.as_str())
        } else {
            None
        };
        fx.shared
            .store
            .fixture_write(|tx| {
                tx.execute(
                    "INSERT INTO messages(id,alias,body,reply_to,source,state,created,recipient_uid) \
                     VALUES(?,?,?,NULL,'user','completed',0.0,?)",
                    rusqlite::params![id, alias, format!("old mail {id}"), recipient],
                )?;
                Ok(())
            })
            .unwrap();
    }
    fx.shared
        .store
        .remove_agent(alias, true, &json!({"by": "operator"}))
        .unwrap();

    // Active pre-cutoff rows inserted after removal but before the bind; the
    // binding cutoff includes them and active_messages must still hide them.
    let old_active_ids = ["cad2207-old-active-known", "cad2207-old-active-null"];
    for (index, id) in old_active_ids.iter().enumerate() {
        let recipient = if index == 0 {
            Some(old_uid.as_str())
        } else {
            None
        };
        fx.shared
            .store
            .fixture_write(|tx| {
                tx.execute(
                    "INSERT INTO messages(id,alias,body,reply_to,source,state,created,recipient_uid) \
                     VALUES(?,?,?,NULL,'user','queued',0.0,?)",
                    rusqlite::params![id, alias, format!("old active mail {id}"), recipient],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fx.authority
        .lock()
        .unwrap()
        .members
        .insert("sub-carol".to_string(), MemberAuthority::live("sub-carol"));
    fx.shared
        .store
        .assistant_bind(ORG, "sub-carol", "assist_sub-carol", alias, Some("rev0"))
        .unwrap();
    fx.op(
        "member_assist_state",
        json!({"member_envelope": fx.member_envelope("sub-carol")}),
    );
    // First proved read stamps the event fence's agent uid; later mail reads
    // must not depend on an unstamped fence.
    fx.call(
        Asserted::Agent(alias.to_string()),
        "agent_events",
        json!({"alias": alias, "after": 0}),
    )
    .unwrap_or_else(|e| panic!("the rebound assistant's event fence read failed: {e}"));
    let new_agent = fx.shared.store.agent(alias).unwrap();
    for (index, id) in old_ids.iter().enumerate() {
        let created = if index % 2 == 0 {
            new_agent.created
        } else {
            new_agent.created + 1.0
        };
        fx.shared
            .store
            .fixture_write(|tx| {
                tx.execute(
                    "UPDATE messages SET created=? WHERE id=?",
                    rusqlite::params![created, id],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fx.shared
        .store
        .enqueue(alias, NEW_BODY, None, "cad2207-new-current-mail", "user")
        .unwrap();
    // Post-cutoff but addressed to the OLD known UID: the sequence fallback
    // is for NULL-UID legacy rows only, so this must stay hidden.
    let stale_uid_id = "cad2207-post-cutoff-old-uid";
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO messages(id,alias,body,reply_to,source,state,created,recipient_uid) \
                 VALUES(?,?,?,NULL,'user','queued',?,?)",
                rusqlite::params![
                    stale_uid_id,
                    alias,
                    "cad2207 post-cutoff old uid mail",
                    crate::issue::time::now_epoch() as f64,
                    old_uid
                ],
            )?;
            Ok(())
        })
        .unwrap();
    // A NULL-UID row after the cutoff is the legacy/daemon path and must stay
    // visible, including as a cursor.
    let null_after_id = "cad2207-post-cutoff-null-uid";
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO messages(id,alias,body,reply_to,source,state,created,recipient_uid) \
                 VALUES(?,?,?,NULL,'user','queued',?,NULL)",
                rusqlite::params![
                    null_after_id,
                    alias,
                    "cad2207 post-cutoff null uid mail",
                    crate::issue::time::now_epoch() as f64
                ],
            )?;
            Ok(())
        })
        .unwrap();
    let stale_row = fx.shared.store.message(stale_uid_id).unwrap().unwrap();
    let fence = fx
        .shared
        .store
        .assistant_event_fence(ORG, "sub-carol")
        .unwrap()
        .expect("the rebound binding has no event fence");
    assert!(
        stale_row.seq > fence.message_cutoff_seq,
        "the post-cutoff fixture did not land above the mail cutoff"
    );
    assert!(
        !fx.shared
            .store
            .agent_reads_message(&stale_row, alias)
            .unwrap(),
        "store.agent_reads_message admitted a post-cutoff old-UID row"
    );
    assert!(
        !fx.shared
            .store
            .messages(alias)
            .unwrap()
            .iter()
            .any(|m| m.id == stale_uid_id),
        "store.messages exposed the post-cutoff old-UID row"
    );
    let hidden_ids: Vec<&str> = old_ids
        .iter()
        .chain(old_active_ids.iter())
        .chain(std::iter::once(&stale_uid_id))
        .copied()
        .collect();
    let listed = fx
        .call(
            Asserted::Agent(alias.to_string()),
            "message_list",
            json!({"to": alias}),
        )
        .unwrap_or_else(|e| panic!("the rebound assistant's message list failed: {e}"));
    let ids: Vec<&str> = listed["messages"]
        .as_array()
        .expect("message_list carries no messages")
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect();
    assert!(
        !hidden_ids.iter().any(|old| ids.contains(old)),
        "retained old mail appeared in the rebound assistant's message_list: {listed}"
    );
    assert!(
        !ids.contains(&stale_uid_id),
        "post-cutoff mail addressed to the old UID appeared in message_list: {listed}"
    );
    assert!(
        ids.contains(&"cad2207-new-current-mail"),
        "the current-UID mail was missing from message_list: {listed}"
    );
    assert!(
        ids.contains(&null_after_id),
        "the post-cutoff NULL-UID mail was hidden from message_list: {listed}"
    );
    assert_eq!(
        listed.get("omitted").and_then(Value::as_u64).unwrap_or(0),
        0,
        "message_list counted hidden old mail: {listed}"
    );
    for id in &hidden_ids {
        let read = fx.call(
            Asserted::Agent(alias.to_string()),
            "message_read",
            json!({"message": *id}),
        );
        assert!(
            read.is_err(),
            "the rebound assistant read retained old mail '{id}': {read:?}"
        );
    }
    let current = fx
        .call(
            Asserted::Agent(alias.to_string()),
            "message_read",
            json!({"message": "cad2207-new-current-mail"}),
        )
        .unwrap_or_else(|e| panic!("the current-UID mail was refused: {e}"));
    assert!(current["text"]
        .as_str()
        .is_some_and(|t| t.contains(NEW_BODY)));
    let shown = fx
        .call(
            Asserted::Agent(alias.to_string()),
            "agent_show",
            json!({"alias": alias}),
        )
        .unwrap_or_else(|e| panic!("the rebound assistant's agent_show failed: {e}"));
    for id in &hidden_ids {
        assert!(
            !shown.to_string().contains(*id),
            "agent_show exposed retained old message '{id}': {shown}"
        );
    }

    // Bounded agent_show paths use separate windows and must apply the same
    // fence: limit, since, and active_only.
    let bounded = fx
        .call(
            Asserted::Agent(alias.to_string()),
            "agent_show",
            json!({"alias": alias, "limit": 50}),
        )
        .unwrap_or_else(|e| panic!("the bounded agent_show limit path failed: {e}"));
    assert!(
        bounded.to_string().contains("cad2207-new-current-mail"),
        "the bounded limit path lost current-UID mail: {bounded}"
    );
    for id in &hidden_ids {
        assert!(
            !bounded.to_string().contains(*id),
            "agent_show limit exposed retained old message '{id}': {bounded}"
        );
    }
    assert_eq!(
        bounded.get("omitted").and_then(Value::as_u64).unwrap_or(0),
        0,
        "agent_show limit counted hidden old mail: {bounded}"
    );
    let bounded_since = fx
        .call(
            Asserted::Agent(alias.to_string()),
            "agent_show",
            json!({"alias": alias, "since": 0}),
        )
        .unwrap_or_else(|e| panic!("the bounded agent_show since path failed: {e}"));
    assert!(
        bounded_since
            .to_string()
            .contains("cad2207-new-current-mail"),
        "the bounded since path lost current-UID mail: {bounded_since}"
    );
    for id in &hidden_ids {
        assert!(
            !bounded_since.to_string().contains(*id),
            "agent_show since exposed retained old message '{id}': {bounded_since}"
        );
    }
    assert_eq!(
        bounded_since
            .get("omitted")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        0,
        "agent_show since counted hidden old mail: {bounded_since}"
    );
    let active = fx
        .call(
            Asserted::Agent(alias.to_string()),
            "agent_show",
            json!({"alias": alias, "active_only": true}),
        )
        .unwrap_or_else(|e| panic!("the active_only agent_show path failed: {e}"));
    assert!(
        active.to_string().contains("cad2207-new-current-mail"),
        "the active_only path lost current-UID mail: {active}"
    );
    assert!(
        active.to_string().contains(null_after_id),
        "the active_only path lost post-cutoff NULL-UID mail: {active}"
    );
    for id in &hidden_ids {
        assert!(
            !active.to_string().contains(*id),
            "agent_show active_only exposed retained old message '{id}': {active}"
        );
    }

    // A hidden old id used as `since` must be rejected exactly like an absent
    // id on both bounded paths, so it cannot act as an existence oracle.
    let since_call = |method: &str, since: &str| {
        let params = if method == "agent_show" {
            json!({"alias": alias, "since": since})
        } else {
            json!({"to": alias, "since": since})
        };
        fx.call(Asserted::Agent(alias.to_string()), method, params)
    };
    let absent_since = "cad2207-absent-since-id";
    for method in ["agent_show", "message_list"] {
        let hidden_since = since_call(method, hidden_ids[0]);
        let absent = since_call(method, absent_since);
        assert!(
            hidden_since.is_err(),
            "{method} accepted a hidden old id as since: {hidden_since:?}"
        );
        assert!(
            absent.is_err(),
            "{method} accepted an absent id as since: {absent:?}"
        );
        assert_eq!(
            hidden_since.unwrap_err().code(),
            absent.unwrap_err().code(),
            "{method} treated a hidden old id differently from an absent id"
        );
        assert!(
            since_call(method, "cad2207-new-current-mail").is_ok(),
            "{method} rejected the current-UID id as since"
        );
        assert!(
            since_call(method, null_after_id).is_ok(),
            "{method} rejected the post-cutoff NULL-UID id as since"
        );
    }

    // Ordinary aliases keep the NULL-UID legacy behavior.
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "ordinary-mail",
            provider: "inbox",
            endpoint_kind: "inbox",
            role: "worker",
            cwd: old_dir.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let ordinary_agent = fx.shared.store.agent("ordinary-mail").unwrap();
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO messages(id,alias,body,reply_to,source,state,created,recipient_uid) \
                 VALUES('cad2207-ordinary-legacy-main','ordinary-mail',?1,NULL,'user', \
                        'completed',?2,NULL)",
                // A terminal legacy row is listed only when its `created` is at
                // or after the current registration; `now_epoch()` truncates to
                // whole seconds and can land just before the fractional
                // `agent.created`, which would make this fixture look hidden.
                rusqlite::params![ORDINARY_BODY, ordinary_agent.created + 1.0],
            )?;
            Ok(())
        })
        .unwrap();
    let ordinary_list = fx
        .call(
            Asserted::Agent("ordinary-mail".to_string()),
            "message_list",
            json!({"to": "ordinary-mail"}),
        )
        .unwrap_or_else(|e| panic!("the ordinary message list failed: {e}"));
    assert!(ordinary_list
        .to_string()
        .contains("cad2207-ordinary-legacy-main"));
    let ordinary_read = fx
        .call(
            Asserted::Agent("ordinary-mail".to_string()),
            "message_read",
            json!({"message": "cad2207-ordinary-legacy-main"}),
        )
        .unwrap_or_else(|e| panic!("the ordinary legacy mail was refused: {e}"));
    assert!(ordinary_read["text"]
        .as_str()
        .is_some_and(|t| t.contains(ORDINARY_BODY)));
}

/// CAD-2207: the public update/health projections and `agent_list`'s
/// `profiles` list may say generic work is in flight, but they must never
/// name a bound member assistant, its private message, or its runtime slot.
/// A real assistant is bound and given a running turn; an ordinary worker
/// stays visible as the positive control. The update projections must still
/// report busy so an upgrade cannot read private work as idle.
#[test]
fn cad2207_public_update_health_and_profile_lists_withhold_private_runtime() {
    const PRIVATE_SUB: &str = "sub-priv-public";
    const PRIVATE_ALIAS: &str = "assist-agent-sub-priv-public";
    const PRIVATE_BODY: &str = "cad2207 private in-flight body";
    const PRIVATE_MSG: &str = "cad2207-private-inflight-msg";
    const SHARED_PROFILE: &str = "cad2207-shared-profile";
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    with_deferred_service(&fx);
    let rt = fx.member_runtime(PRIVATE_SUB);
    assert_eq!(
        rt.alias, PRIVATE_ALIAS,
        "the fixture bound an unexpected private alias"
    );
    assert!(
        fx.shared
            .store
            .assistant_for_alias(PRIVATE_ALIAS)
            .unwrap()
            .is_some(),
        "the private assistant was not really bound"
    );
    let private_agent = fx.shared.store.agent(PRIVATE_ALIAS).unwrap();
    assert_eq!(
        (
            private_agent.provider.as_str(),
            private_agent.endpoint_kind.as_str()
        ),
        ("pi", "managed"),
        "the private assistant has no actor, so it cannot exercise update waiting"
    );

    // Give the public profile a runtime binding to the private assistant. The
    // profile itself is a normal shared definition; only its runtime alias is
    // private and must disappear from the public projection.
    let created = fx.op(
        "agent_profile_create",
        json!({"alias": SHARED_PROFILE, "catalogue": "developer"}),
    );
    assert_eq!(
        created["id"].as_str(),
        Some(SHARED_PROFILE),
        "the shared profile was not created: {created}"
    );
    let mut private_params = private_agent.params.clone().unwrap_or_else(|| json!({}));
    private_params["profile"] = json!(SHARED_PROFILE);
    let private_params = private_params.to_string();
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "UPDATE agents SET params=? WHERE alias=?",
                rusqlite::params![private_params, PRIVATE_ALIAS],
            )?;
            tx.execute(
                "INSERT INTO messages(id,alias,body,reply_to,source,state,created,recipient_uid) \
                 VALUES(?1,?2,?3,NULL,'user','running',?4,?5)",
                rusqlite::params![
                    PRIVATE_MSG,
                    PRIVATE_ALIAS,
                    PRIVATE_BODY,
                    crate::store::now_public(),
                    private_agent.uid
                ],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(
        fx.shared
            .store
            .inflight_messages()
            .unwrap()
            .iter()
            .any(|(alias, id, _, _)| alias.as_str() == PRIVATE_ALIAS && id == PRIVATE_MSG),
        "the private running turn was not stored for the update gate"
    );

    fn busy(value: &Value) -> bool {
        match value {
            Value::Number(number) => number.as_u64().is_some_and(|n| n > 0),
            Value::Bool(flag) => *flag,
            Value::Array(rows) => !rows.is_empty(),
            Value::Object(fields) => fields.values().any(busy),
            Value::String(text) => !text.is_empty(),
            Value::Null => false,
        }
    }
    let busy_signal = |response: &Value| {
        [
            "unattributed_work_pending",
            "private_waiting",
            "update_waiting",
            "waiting",
            "waiting_count",
            "blockers",
            "blocked",
            "busy",
        ]
        .iter()
        .any(|key| response.get(*key).is_some_and(busy))
    };
    fn assert_opaque_waiting_row(response: &Value, key: &str, label: &str) {
        let rows = response[key]
            .as_array()
            .unwrap_or_else(|| panic!("{label} {key} is not an array: {response}"));
        assert!(
            !rows.is_empty(),
            "{label} {key} hid the private-only in-flight work: {response}"
        );
        assert!(
            rows.iter().any(|row| {
                row.get("alias").and_then(Value::as_str).is_some()
                    && row.get("message").and_then(Value::as_str).is_some()
                    && row.get("state").and_then(Value::as_str).is_some()
                    && row.get("age_secs").and_then(Value::as_f64).is_some()
            }),
            "{label} {key} has no client-visible opaque waiter row: {response}"
        );
    }

    for (who, label) in [
        (Asserted::Operator, "operator"),
        (Asserted::Unproven, "nonowner"),
    ] {
        let health = fx
            .call(who.clone(), "health", json!({}))
            .unwrap_or_else(|e| panic!("{label} health refused: {e}"));
        assert!(
            busy_signal(&health),
            "{label} health read private work as idle: {health}"
        );
        assert_opaque_waiting_row(&health, "update_waiting", label);
        for secret in [PRIVATE_ALIAS, PRIVATE_MSG, PRIVATE_BODY] {
            assert!(
                !health.to_string().contains(secret),
                "{label} health exposed private '{secret}': {health}"
            );
        }

        let status = fx
            .call(who.clone(), "update_status", json!({}))
            .unwrap_or_else(|e| panic!("{label} update_status refused: {e}"));
        assert!(
            busy_signal(&status),
            "{label} update_status read private work as idle: {status}"
        );
        assert_opaque_waiting_row(&status, "waiting", label);
        for secret in [PRIVATE_ALIAS, PRIVATE_MSG, PRIVATE_BODY] {
            assert!(
                !status.to_string().contains(secret),
                "{label} update_status exposed private '{secret}': {status}"
            );
        }

        let listed = fx
            .call(who.clone(), "agent_list", json!({}))
            .unwrap_or_else(|e| panic!("{label} agent_list refused: {e}"));
        let aliases: Vec<&str> = listed["agents"]
            .as_array()
            .expect("agent_list carries no agents array")
            .iter()
            .filter_map(|agent| agent["alias"].as_str())
            .collect();
        assert!(
            aliases.contains(&"ordinary-worker"),
            "{label} agent_list lost the ordinary worker: {listed}"
        );
        assert!(
            !aliases.contains(&PRIVATE_ALIAS),
            "{label} agent_list exposed the private assistant row: {listed}"
        );
        for secret in [PRIVATE_ALIAS, PRIVATE_MSG, PRIVATE_BODY] {
            assert!(
                !listed.to_string().contains(secret),
                "{label} agent_list exposed private '{secret}': {listed}"
            );
        }
        let profiles = listed["profiles"]
            .as_array()
            .expect("agent_list carries no profiles array");
        let shared = profiles
            .iter()
            .find(|profile| profile["id"].as_str() == Some(SHARED_PROFILE))
            .unwrap_or_else(|| panic!("{label} agent_list lost the shared profile: {listed}"));
        assert!(
            !shared["runtimes"]
                .as_array()
                .expect("the shared profile carries no runtimes array")
                .iter()
                .any(|runtime| runtime.as_str() == Some(PRIVATE_ALIAS)),
            "{label} agent_list.profiles exposed the private runtime: {shared}"
        );
    }
}

/// CAD-2207: the public update wait list must be ONE generic aggregate row
/// for any in-flight actor. Private-only and ordinary-only work are therefore
/// indistinguishable in the meaningful status fields, while neither exposes
/// its alias, message id, body, a private label or a class-specific flag. The
/// ordinary agent remains visible in `agent_list`.
#[test]
fn cad2207_public_waiting_shape_is_identical_for_private_and_ordinary_work() {
    const ORDINARY: &str = "cad2207-wait-ordinary";
    const ORDINARY_MSG: &str = "cad2207-wait-ordinary-msg";
    const ORDINARY_BODY: &str = "cad2207 ordinary in-flight body";
    const PRIVATE_SUB: &str = "sub-wait-shape";
    const PRIVATE_MSG: &str = "cad2207-wait-private-msg";
    const PRIVATE_BODY: &str = "cad2207 private in-flight body";
    let _flag = AssistantFlagOn::arm();

    #[derive(Debug, PartialEq)]
    struct WaitingShape {
        rows: usize,
        keys: BTreeSet<String>,
        state: Option<String>,
        age_secs_is_number: bool,
        count: Option<u64>,
    }
    fn waiting_shape(response: &Value, key: &str, label: &str) -> WaitingShape {
        let rows = response[key]
            .as_array()
            .unwrap_or_else(|| panic!("{label} {key} is not an array: {response}"));
        assert_eq!(
            rows.len(),
            1,
            "{label} {key} must be one aggregate row, not per-actor rows: {response}"
        );
        let row = rows[0]
            .as_object()
            .unwrap_or_else(|| panic!("{label} {key} row is not an object: {response}"));
        for field in ["alias", "message", "state"] {
            assert!(
                row.get(field).is_some_and(Value::is_string),
                "{label} {key} row has no string {field}: {response}"
            );
        }
        assert!(
            row.get("age_secs").and_then(Value::as_u64).is_some(),
            "{label} {key} row has no nonnegative numeric age_secs: {response}"
        );
        let keys: BTreeSet<String> = row.keys().cloned().collect();
        assert!(
            !keys.iter().any(|field| {
                let field = field.to_ascii_lowercase();
                field.contains("private")
                    || field.contains("ordinary")
                    || field.contains("restricted")
                    || field.contains("unattributed")
                    || field.contains("class")
            }),
            "{label} {key} carries a class-specific field: {response}"
        );
        for text in row.values().filter_map(Value::as_str) {
            let text = text.to_ascii_lowercase();
            assert!(
                !text.contains("private")
                    && !text.contains("ordinary")
                    && !text.contains("restricted"),
                "{label} {key} carries a class label: {response}"
            );
        }
        WaitingShape {
            rows: rows.len(),
            keys,
            state: row.get("state").and_then(Value::as_str).map(str::to_string),
            // Age is a live clock observation, not fixture-stable; only its
            // nonnegative numeric type is part of the privacy shape.
            age_secs_is_number: true,
            count: response.get("waiting_count").and_then(Value::as_u64),
        }
    }
    fn assert_no_class_flag(response: &Value, label: &str) {
        for key in [
            "private_waiting",
            "ordinary_waiting",
            "unattributed_work_pending",
            "restricted_work_pending",
        ] {
            assert!(
                response.get(key).is_none(),
                "{label} exposes class-specific flag '{key}': {response}"
            );
        }
    }
    fn insert_running(fx: &Fx, alias: &str, id: &str, body: &str) {
        let uid = fx.shared.store.agent(alias).unwrap().uid;
        fx.shared
            .store
            .fixture_write(|tx| {
                tx.execute(
                    "INSERT INTO messages(id,alias,body,reply_to,source,state,created,recipient_uid) \
                     VALUES(?1,?2,?3,NULL,'user','running',?4,?5)",
                    rusqlite::params![id, alias, body, crate::store::now_public(), uid],
                )?;
                Ok(())
            })
            .unwrap();
    }

    // Ordinary-only fixture: one ordinary actor with a running turn.
    let ordinary = Fx::new();
    with_deferred_service(&ordinary);
    let ordinary_cwd = ordinary.dir.path().join("ordinary-wait-cwd");
    std::fs::create_dir_all(&ordinary_cwd).unwrap();
    ordinary
        .shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: ORDINARY,
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: ordinary_cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    insert_running(&ordinary, ORDINARY, ORDINARY_MSG, ORDINARY_BODY);

    // Private-only in-flight work: the member assistant is bound and has the
    // running turn; the ordinary worker only has its queued foreign seed.
    let private = Fx::new();
    with_deferred_service(&private);
    let rt = private.member_runtime(PRIVATE_SUB);
    let private_alias = rt.alias.clone();
    insert_running(&private, &private_alias, PRIVATE_MSG, PRIVATE_BODY);

    for (who, label) in [
        (Asserted::Operator, "operator"),
        (Asserted::Unproven, "nonowner"),
    ] {
        let ordinary_health = ordinary
            .call(who.clone(), "health", json!({}))
            .unwrap_or_else(|e| panic!("{label} ordinary health refused: {e}"));
        let private_health = private
            .call(who.clone(), "health", json!({}))
            .unwrap_or_else(|e| panic!("{label} private health refused: {e}"));
        for response in [&ordinary_health, &private_health] {
            assert_no_class_flag(response, label);
            for secret in [
                ORDINARY,
                ORDINARY_MSG,
                ORDINARY_BODY,
                private_alias.as_str(),
                PRIVATE_MSG,
                PRIVATE_BODY,
            ] {
                assert!(
                    !response.to_string().contains(secret),
                    "{label} health exposed '{secret}': {response}"
                );
            }
        }
        assert_eq!(
            waiting_shape(&ordinary_health, "update_waiting", label),
            waiting_shape(&private_health, "update_waiting", label),
            "{label} health waiting shapes differ between ordinary and private work"
        );

        let ordinary_status = ordinary
            .call(who.clone(), "update_status", json!({}))
            .unwrap_or_else(|e| panic!("{label} ordinary update_status refused: {e}"));
        let private_status = private
            .call(who.clone(), "update_status", json!({}))
            .unwrap_or_else(|e| panic!("{label} private update_status refused: {e}"));
        for response in [&ordinary_status, &private_status] {
            assert_no_class_flag(response, label);
            for secret in [
                ORDINARY,
                ORDINARY_MSG,
                ORDINARY_BODY,
                private_alias.as_str(),
                PRIVATE_MSG,
                PRIVATE_BODY,
            ] {
                assert!(
                    !response.to_string().contains(secret),
                    "{label} update_status exposed '{secret}': {response}"
                );
            }
        }
        assert_eq!(
            waiting_shape(&ordinary_status, "waiting", label),
            waiting_shape(&private_status, "waiting", label),
            "{label} update_status waiting shapes differ between ordinary and private work"
        );

        let listed = ordinary
            .call(who, "agent_list", json!({}))
            .unwrap_or_else(|e| panic!("{label} ordinary agent_list refused: {e}"));
        assert!(
            listed["agents"].as_array().is_some_and(|rows| rows
                .iter()
                .any(|row| row["alias"].as_str() == Some(ORDINARY))),
            "{label} ordinary agent_list lost its visible ordinary agent: {listed}"
        );
    }
}

// CAD-1767 G2: the member-assist-history half (check author, failing first).

// CAD-2207: independent acceptance for LOCAL share-revocation ordering against
// an in-flight notice turn (see the module header for its narrow scope, the
// deterministic barrier, the expected red and the run command).
mod cad2207_continuity_revocation_acceptance;
mod cad2207_continuity_withdrawal_race;
mod cad2207_history_revocation_acceptance;
mod cad2207_history_socket_withdrawal_acceptance;
mod cad2207_pre_admission_acceptance;
mod cad2207_queue_head_race_acceptance;
mod cad2207_share_revocation_ordering;
// CAD-2207: independent acceptance for the operator's cross-agent work
// conversation view (`work_conversation_list` / `work_conversation_read`),
// including durable provenance after an `agents` row is gone (see the module
// header for its scope, the expected red and the cases it does not cover).
mod cad2207_activity_privacy;
mod cad2207_work_conversation_privacy;
// CAD-2207: independent acceptance for the dispatcher/store lifecycle guards
// on a member's personal assistant — generic operator verbs and the direct
// store remove/gc/archive/restore paths refuse it, with ordinary controls and
// the binding's in-flight fence ordering (see the module header for its
// narrow scope and the cases it does not claim).
mod cad2207_lifecycle_guard;
// CAD-2207: independent acceptance for the legacy topic verbs on a MIXED
// fleet topic — generic operator membership mutation of a bound assistant,
// its own publish, and non-owner reads/aggregate counts must refuse or
// withhold while already-shared history and ordinary peers stay usable (see
// the module header for its narrow scope and the fixture seam report).
mod cad2207_topic_ask_race_acceptance;
mod cad2207_topic_privacy;
// CAD-2207: independent acceptance for the operator SETTINGS and MODEL-DEFAULTS
// projections on a mixed fleet — observed-model suggestions, live-use counts,
// profile used-by lists and the settings write's reference refusal must withhold
// a bound private assistant's alias/subject/model while ordinary rows stay
// visible, with the refused write proven not to mutate (see the module header
// for its scope, the profile-pointer limitation and the run command).
mod cad2207_settings_models_privacy;
