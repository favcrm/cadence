//! CAD-1380/AOS-216: the daemon/board side of the member team-assistant
//! authority — verifying the **assistant actor envelope** AgenticOS's
//! `/__platform/assistant/authorize` mints and
//! `/__platform/assistant/call` forwards as the bearer of
//! `POST /api/assist/<verb>` inside the container.
//!
//! A distinct JWS family from the cli/wiki envelopes: header
//! `{alg:"EdDSA", typ:"agenticos-assistant-actor/1", kid}` over a closed
//! claim set `iss, aud, sub, actor, principal_kind, role_at_issue,
//! organization_id, assistant_id, scope, credential_id, iat, exp, jti`.
//! No field is caller-supplied. The family carries MEMBER-bound authority
//! only: `actor` is always `users/<sub>` equal to `sub`, `principal_kind`
//! is always `user`, and `assistant_id` is always `assist_<sub>` — the
//! schema's pins, re-checked in [`verify`]. This family can never mint
//! `operator`, a `service/` or an `agents/` actor, and a cli/wiki envelope
//! is refused here by the `typ` pin.
//!
//! [`verify`] is `checkWikiActorEnvelope` mirrored to the assistant
//! claims: structure, the signature against the issuer's published
//! Ed25519 keys (the same `loadBoardKeys` keyring), then the pins:
//! `iss` = configured issuer, `aud` = THIS board's public origin,
//! `organization_id` = this instance's company, `exp`/`iat` bounded,
//! `exp - iat ≤ 300`, scope ⊆ {`assist.read`,`assist.write`,`assist.jobs`}.
//! Every refusal fails closed; a refused call changes nothing.
//!
//! The bearer spelling is `wikienv_<envelope>` — the shared prefix
//! ([`crate::cli_actor::BEARER_PREFIX`]); the route strips it before
//! verify. The single-use `jti` set is separate from the cli family's
//! (`assist-jtis.json`): each family's replay memory is independent, so
//! one envelope can never double-consume across families.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;

use std::path::Path;

use crate::board_identity::{self, PublicKey};
use crate::error::Error;

/// The bearer prefix the mint emits (`wikiActorBearer`, shared).
pub const BEARER_PREFIX: &str = "wikienv_";

/// The envelope family's required JWS `typ` — refuses a cli/wiki envelope.
pub const HEADER_TYP: &str = "agenticos-assistant-actor/1";

/// `iat` may lead the clock by this much; `exp` gets no trailing skew.
pub const SKEW_SECS: i64 = 30;
/// The envelope's longest life (`HOSTED_CADENCE_MAX_TTL_SECONDS`).
const MAX_LIFE_SECS: i64 = 300;
/// Bounded bearer header value.
const BEARER_CAP: usize = 8192;

/// The member-assistant verbs the board's `/api/assist/` route family
/// serves — the AOS-216 contract's v1 list. A verb absent here is refused
/// before any body is read; the path segment, never a request field,
/// names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistVerb {
    // assist.read
    AssistState,
    AssistHistory,
    // assist.write
    AssistSend,
    // assist.jobs
    JobRequest,
    JobList,
    JobEvidence,
    JobEvents,
    JobSubscribe,
    JobShare,
    JobReconcile,
}

/// The `assist.*` scope a verb needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistScope {
    Read,
    Write,
    Jobs,
}

impl AssistVerb {
    /// The path segment → the verb, or `None` (refused) for anything not
    /// allowlisted.
    pub fn of(segment: &str) -> Option<AssistVerb> {
        Some(match segment {
            "assist_state" => AssistVerb::AssistState,
            "assist_history" => AssistVerb::AssistHistory,
            "assist_send" => AssistVerb::AssistSend,
            "job_request" => AssistVerb::JobRequest,
            "job_list" => AssistVerb::JobList,
            "job_evidence" => AssistVerb::JobEvidence,
            "job_events" => AssistVerb::JobEvents,
            "job_subscribe" => AssistVerb::JobSubscribe,
            "job_share" => AssistVerb::JobShare,
            "job_reconcile" => AssistVerb::JobReconcile,
            _ => return None,
        })
    }

    pub fn scope(self) -> AssistScope {
        match self {
            AssistVerb::AssistState | AssistVerb::AssistHistory => AssistScope::Read,
            AssistVerb::AssistSend => AssistScope::Write,
            AssistVerb::JobRequest
            | AssistVerb::JobList
            | AssistVerb::JobEvidence
            | AssistVerb::JobEvents
            | AssistVerb::JobSubscribe
            | AssistVerb::JobShare
            | AssistVerb::JobReconcile => AssistScope::Jobs,
        }
    }
}

/// Why an envelope or call was refused.
#[derive(Clone, Debug)]
pub struct Rejection {
    pub code: &'static str,
    pub message: String,
}

impl Rejection {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self::new("assertion_invalid", message)
    }
}

/// The JWS header — `{alg, typ, kid}`; the family is told apart by `typ`
/// AND by the closed claim set, both pinned.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    typ: String,
    kid: String,
}

/// `hostedCadenceAssistantActorClaimsSchema` — the closed claim set.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    iss: String,
    aud: String,
    sub: String,
    /// `users/<sub>` only — the member's own handle.
    actor: String,
    /// Always `user` — assistants are personal, minted only for users.
    principal_kind: String,
    /// `owner` | `member` at mint.
    role_at_issue: String,
    organization_id: String,
    /// `assist_<sub>` — the bound assistant, server-derived.
    assistant_id: String,
    scope: Vec<String>,
    credential_id: String,
    iat: i64,
    exp: i64,
    jti: String,
}

/// The verified member/assistant identity the daemon derives from the
/// route derives every binding from, never a request field.
#[derive(Clone, Debug)]
pub struct AssistActor {
    /// `claims.sub` — the member's server-issued user id.
    pub sub: String,
    /// `users/<sub>` — the member actor spelling.
    pub actor: String,
    /// `assist_<sub>` — the bound assistant handle.
    pub assistant: String,
    /// The organization the membership binds.
    pub org: String,
    /// The granted assist scopes.
    pub scopes: Vec<String>,
    /// Single-use token id.
    pub jti: String,
    pub exp: i64,
    /// The issuing credential the mint bound — the member's authority
    /// provenance a deferred commit re-proves (job_request records it).
    pub credential_id: String,
    /// Attribution handle — `users/<sub>` tail.
    pub handle: String,
}

/// Extract the compact envelope from `Authorization`: exactly
/// `Bearer wikienv_<h>.<p>.<s>` on this route family.
pub fn envelope_from_bearer(header: Option<&str>) -> Result<String, Rejection> {
    let refuse = || {
        Rejection::new(
            "unauthorized",
            "an assistant actor envelope bearer is required",
        )
    };
    let value = header.ok_or_else(refuse)?.trim();
    if value.is_empty() || value.len() > BEARER_CAP || !value.is_ascii() {
        return Err(refuse());
    }
    let rest = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .ok_or_else(refuse)?;
    let env = rest
        .strip_prefix(BEARER_PREFIX)
        .filter(|e| !e.is_empty())
        .ok_or_else(refuse)?;
    if !env
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
    {
        return Err(refuse());
    }
    Ok(env.to_string())
}

/// Structure only — three non-empty base64url parts, the exact header
/// family, the closed claim set. No signature or pin check runs here.
fn parse(envelope: &str) -> Result<(Header, Claims, String, Vec<u8>), Rejection> {
    let mut parts = envelope.split('.');
    let (h, p, s) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s), None) if !h.is_empty() && !p.is_empty() && !s.is_empty() => {
            (h, p, s)
        }
        _ => return Err(Rejection::invalid("envelope is not a compact JWS")),
    };
    let decode = |part: &str| {
        URL_SAFE_NO_PAD
            .decode(part)
            .map_err(|_| Rejection::invalid("envelope is not base64url"))
    };
    let header: Header = serde_json::from_slice(&decode(h)?)
        .map_err(|_| Rejection::new("bad_header", "envelope header is not the contract shape"))?;
    if header.alg != "EdDSA" || header.typ != HEADER_TYP || header.kid.is_empty() {
        return Err(Rejection::new(
            "bad_header",
            "envelope header is not the assistant-actor shape",
        ));
    }
    let claims: Claims = serde_json::from_slice(&decode(p)?)
        .map_err(|_| Rejection::new("bad_claims", "envelope claims are not the assistant shape"))?;
    let signature = decode(s)?;
    if signature.len() != 64 {
        return Err(Rejection::invalid("envelope signature is not Ed25519"));
    }
    Ok((header, claims, format!("{h}.{p}"), signature))
}

/// Extract the signed header's `kid` for the JWKS lookup — used by the
/// route and by the daemon's member re-verify to pick the verifying key
/// before full [`verify`] runs. `None` on any structure failure (the
/// caller treats it as an unverifiable envelope).
pub fn peek_kid(envelope: &str) -> Option<String> {
    let head = envelope.split('.').next().unwrap_or_default();
    let bytes = URL_SAFE_NO_PAD.decode(head).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let kid = value["kid"].as_str()?.to_string();
    if kid.is_empty() {
        None
    } else {
        Some(kid)
    }
}

/// The scheme a public `aud` origin carries for `host`.
fn public_scheme(host: &str) -> &'static str {
    let name = host.split(':').next().unwrap_or_default();
    if name == "localhost" || name.ends_with(".localhost") {
        "http"
    } else {
        "https"
    }
}

/// The attribution handle for a verified actor: its subject in the shared
/// identifier grammar ([`crate::ident::sanitized`]).
fn handle_of(actor: &str, sub: &str) -> String {
    let tail = actor.split_once('/').map(|(_, t)| t).unwrap_or(sub);
    crate::ident::sanitized(tail)
}

/// Verify an assistant envelope end to end: structure, `kid` → key, the
/// Ed25519 signature, then every contract pin against `expected` — the
/// pins come from the daemon's own `board-identity.json`, never the
/// envelope or the request.
pub fn verify(
    envelope: &str,
    expected: &board_identity::Config,
    key: &PublicKey,
    now: i64,
) -> Result<AssistActor, Rejection> {
    let (header, claims, signed, signature) = parse(envelope)?;
    if header.kid != key.kid {
        return Err(Rejection::invalid(
            "the envelope's key id does not name the verifying key",
        ));
    }
    {
        use ring::signature::{UnparsedPublicKey, ED25519};
        UnparsedPublicKey::new(&ED25519, &key.x)
            .verify(signed.as_bytes(), &signature)
            .map_err(|_| {
                Rejection::new(
                    "signature_invalid",
                    "the envelope's signature does not verify",
                )
            })?;
    }
    if claims.iss != expected.issuer {
        return Err(Rejection::new(
            "issuer_mismatch",
            "the envelope was not issued by this board's platform",
        ));
    }
    let want_aud = format!("{}://{}", public_scheme(&expected.host), expected.host);
    if claims.aud != want_aud {
        return Err(Rejection::new(
            "audience_mismatch",
            "the envelope was minted for another board host",
        ));
    }
    if claims.organization_id != expected.company {
        return Err(Rejection::new(
            "not_a_member",
            "the envelope names a workspace this instance does not serve",
        ));
    }
    if claims.iat > now + SKEW_SECS
        || claims.exp <= now
        || claims.exp <= claims.iat
        || claims.exp - claims.iat > MAX_LIFE_SECS
    {
        return Err(Rejection::new(
            "assertion_expired",
            "the envelope is expired or outside its time bounds",
        ));
    }
    // Member-bound only: this family never carries operator/service/agent.
    if claims.principal_kind != "user" {
        return Err(Rejection::new(
            "bad_claims",
            "the assistant envelope requires a user principal",
        ));
    }
    if claims.actor != format!("users/{}", claims.sub) {
        return Err(Rejection::new(
            "bad_claims",
            "the assistant envelope actor is not bound to its member",
        ));
    }
    if claims.assistant_id != format!("assist_{}", claims.sub) {
        return Err(Rejection::new(
            "bad_claims",
            "the assistant envelope is not bound to its member's assistant",
        ));
    }
    if claims.role_at_issue != "owner" && claims.role_at_issue != "member" {
        return Err(Rejection::new(
            "bad_claims",
            "the assistant envelope role is not a member role",
        ));
    }
    if claims
        .scope
        .iter()
        .any(|s| !matches!(s.as_str(), "assist.read" | "assist.write" | "assist.jobs"))
    {
        return Err(Rejection::new(
            "bad_claims",
            "the envelope carries a scope outside the assist vocabulary",
        ));
    }
    Ok(AssistActor {
        handle: handle_of(&claims.actor, &claims.sub),
        sub: claims.sub,
        actor: claims.actor,
        assistant: claims.assistant_id,
        org: claims.organization_id,
        scopes: claims.scope,
        jti: claims.jti,
        exp: claims.exp,
        credential_id: claims.credential_id,
    })
}

/// Does the verified actor's granted scope set cover `need`?
pub fn scope_covers(actor: &AssistActor, need: AssistScope) -> bool {
    let want = match need {
        AssistScope::Read => "assist.read",
        AssistScope::Write => "assist.write",
        AssistScope::Jobs => "assist.jobs",
    };
    actor.scopes.iter().any(|s| s == want)
}

// ---------- single-use `jti` (assist family, separate from cli) ----------

/// The assist family's own seen-set: `assist-jtis.json`. Kept distinct
/// from the cli family's `cli-jtis.json` so a jti spent on one family is
/// still spent — and a forge cannot replay across families either (each
/// family's `typ` pin already refuses the other's envelope, so an assist
/// jti can only ever be minted into an assist envelope).
const SPENT_FILE: &str = "assist-jtis.json";

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct SpentFile {
    #[serde(default)]
    jtis: std::collections::HashMap<String, i64>,
}

/// Per-`state_dir` seen-set for the assist family.
pub struct Spent {
    path: std::path::PathBuf,
    jtis: std::collections::HashMap<String, i64>,
    corrupt: bool,
}

impl Spent {
    pub fn load(state_dir: &Path) -> Self {
        let path = state_dir.join(SPENT_FILE);
        match std::fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self {
                path,
                jtis: Default::default(),
                corrupt: false,
            },
            Err(_) => Self {
                path,
                jtis: Default::default(),
                corrupt: true,
            },
            Ok(b) => match serde_json::from_slice::<SpentFile>(&b) {
                Ok(f) => Self {
                    path,
                    jtis: f.jtis,
                    corrupt: false,
                },
                Err(_) => Self {
                    path,
                    jtis: Default::default(),
                    corrupt: true,
                },
            },
        }
    }

    /// First use records `sha256(jti)` until `exp`; a second in-window use
    /// returns `false` (the `assertion_replayed` refusal). Corrupt set or
    /// failed persist refuses closed.
    pub fn consume(&mut self, jti: &str, exp: i64, now: i64) -> bool {
        if self.corrupt {
            return false;
        }
        self.jtis.retain(|_, e| now <= *e);
        let key = crate::operator_auth::digest(jti);
        if self.jtis.contains_key(&key) {
            return false;
        }
        self.jtis.insert(key, exp);
        self.persist().is_ok()
    }

    fn persist(&self) -> crate::error::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = self.path.with_extension("tmp");
        match std::fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::internal(format!("{}: {e}", tmp.display()))),
        }
        let body = serde_json::to_vec_pretty(&SpentFile {
            jtis: self.jtis.clone(),
        })?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)?;
        file.write_all(&body)?;
        file.sync_all()?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}
