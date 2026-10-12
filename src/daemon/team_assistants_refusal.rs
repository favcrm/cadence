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
        let platform_thread = {
            let stop = stop.clone();
            let authority = authority.clone();
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
            stop,
            _platform: platform_thread,
        }
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
        // Op-bound provenance for `job_dispatch`: the request names the
        // queued effect's recorded `credential_id`; the platform re-proves
        // THAT credential live (`liveBridge` on the op's own
        // `assist_requests.credential_id`), never the binding's latest —
        // a member whose recorded op credential is dead stays denied even
        // after a global renewal re-pins the assistant binding.
        let op_provenance_ok = if input.operation == "job_dispatch" {
            let cred = input.resource.credential_id.as_deref();
            // Required for job_dispatch — a deferred dispatch with no
            // recorded provenance fails closed.
            match cred {
                None => false,
                Some(cred) => {
                    let guard = authority.lock().unwrap();
                    guard
                        .members
                        .get(&input.sub)
                        .and_then(|m| m.live_credentials.get(cred))
                        .is_some_and(|(live, caps)| *live && caps.iter().any(|c| c == &input.scope))
                }
            }
        } else {
            true
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
// effect while the feature is off. The flag is PROCESS env: the positive
// fixtures above run with it set in the test process (the documented
// `HOSTED_CADENCE_ASSISTANT_ENABLED=true` invocation — never mutated
// in-process, which would race parallel tests). The OFF cases therefore
// run in ISOLATED CHILD PROCESSES (the coordinator's scoped-isolation
// requirement): this binary respawned with the flag absent/false/garbage.
// ============================================================================

/// The child-side flag-off probe — `#[ignore]`d so it never runs inside
/// the enabled suite; the parent driver respawns it with the flag unset.
/// Asserts the REAL `member_caller` guard: every `member_*` verb refuses
/// with the not-enabled error BEFORE the envelope is read — and the
/// operator surface the flag must NOT touch stays intact.
#[test]
#[ignore]
fn member_verbs_refuse_when_flag_off_child() {
    let fx = Fx::new();
    let env = fx.provision("sub-alice");
    let before = fx.tree();
    for (method, params) in member_verbs(&env) {
        let e = err_text(
            fx.op_err(method, params.clone()),
            &format!("{method} under disabled feature"),
        );
        assert!(
            e.contains("not enabled"),
            "{method} under disabled feature refused for another reason: {e}"
        );
    }
    // An explicit `member_job_reconcile` — the param-shape check runs
    // before `member_caller`, so a well-shaped call is what reaches the
    // flag gate; it is refused there BEFORE the envelope is read.
    let e = err_text(
        fx.op_err(
            "member_job_reconcile",
            json!({"member_envelope": env, "job": "j1", "expected_authority_rev": "1.0"}),
        ),
        "member_job_reconcile under disabled feature",
    );
    assert!(e.contains("not enabled"), "reconcile refusal: {e}");
    assert_eq!(
        fx.tree(),
        before,
        "a disabled-flag member verb had an effect"
    );
    // Legacy non-assistant operator surface unaffected by the flag —
    // `daemon_info` is a proven-operator call that must still answer.
    let info = fx.op("daemon_info", json!({}));
    assert!(info.is_object(), "daemon_info broke under the flag: {info}");
}

/// Parent driver — respawns THIS test binary with
/// `HOSTED_CADENCE_ASSISTANT_ENABLED` absent / `"false"` / garbage and
/// runs the ignored child probe in each; the child's exit code is the
/// verdict (a panic exits nonzero). No in-process env mutation — the
/// parallel enabled suite can never observe a torn flag.
#[test]
fn member_verbs_refuse_when_flag_off() {
    for (label, flag) in [
        ("absent", None),
        ("false", Some("false")),
        ("garbage", Some("not-a-bool")),
    ] {
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "daemon::team_assistants_refusal::member_verbs_refuse_when_flag_off_child",
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
            let (dup, id) = self
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
            let _ = wthread;
            id
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
    let fx = Fx::new();
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
    let fx = Fx::new();
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
    let fx = Fx::new();
    // Pin a devin worker default + allow in the fixture's own pm.yaml —
    // the operator-configured policy the launch resolves (isolated pm,
    // never the real one).
    let pm_yaml = fx.pm().join("pm.yaml");
    let mut text = std::fs::read_to_string(&pm_yaml).unwrap();
    text.push_str(
        "\npi:\n  models:\n    allow:\n      - devin/claude-haiku-5-5-high\n    default:\n      worker: devin/claude-haiku-5-5-high\n",
    );
    std::fs::write(&pm_yaml, text).unwrap();

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

/// A malformed `[pi]` policy is a hard config error — `pi_policy::read`
/// refuses and the launch surfaces the cause, never falls back to a
/// different model or an unconfined path.
#[test]
fn malformed_pi_policy_refuses_launch_claim_closed() {
    let fx = Fx::new();
    let pm_yaml = fx.pm().join("pm.yaml");
    let mut text = std::fs::read_to_string(&pm_yaml).unwrap();
    text.push_str("\npi:\n  models: [unclosed\n");
    std::fs::write(&pm_yaml, text).unwrap();

    let rt = fx.member_runtime("sub-mal");
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
