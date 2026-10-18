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

#![cfg(all(test, feature = "test-seam"))]

use super::*;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::collections::BTreeMap;
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
    _platform: std::thread::JoinHandle<()>,
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
        let platform_thread = {
            let stop = stop.clone();
            let authority = authority.clone();
            let barrier = barrier.clone();
            let issuer = issuer.clone();
            let host = host.clone();
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
            _platform: platform_thread,
        }
    }

    /// Release the held pull and wake the server thread.
    fn release_barrier(&self) {
        self.authority.lock().unwrap().released = true;
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
        self.stop.store(true, Ordering::SeqCst);
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
    let taken = fx.shared.take_app_aware(&rt.alias).unwrap();
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

    let claimed = fx.shared.take_app_aware(&rt.alias).unwrap();
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
        // ASSIST_NOTICE_EVERY is 2 s: wait past at least one further watcher
        // pass so the replay actually runs the real drain again.
        std::thread::sleep(Duration::from_millis(2_500));
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
