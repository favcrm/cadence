//! CAD-1019 slice 3: the daemon side of the remote CLI — verifying the
//! **cli actor envelope** AgenticOS's `/__platform/cli/authorize` mints
//! (AOS-128) and that `/__platform/cli/call` forwards as the bearer of
//! `POST /api/cli/<verb>` inside the container.
//!
//! The wire contract is `hostedCadenceCliActorClaimsSchema`
//! (`packages/contracts/src/hosted-cadence-auth.ts` on AgenticOS
//! staging): a compact Ed25519 JWS — the same `mintWikiActorEnvelope`
//! shape as AOS-122's wiki envelopes, so the header is
//! `{alg:"EdDSA", typ:"agenticos-wiki-actor/1", kid}` — over a closed
//! claim set: `iss`, `aud` (the board's public *origin*, scheme
//! included), `sub`, `actor`, `principal_kind`, `role_at_issue`,
//! `organization_id`, `scope` (`cli.read`/`cli.write`),
//! `credential_id`, `iat`, `exp`, `jti`. No field is caller-supplied;
//! `typ` distinguishes nothing — the claim set is the family.
//!
//! [`verify`] is `checkWikiActorEnvelope` mirrored to the cli claims:
//! structure first, the signature against the issuer's published
//! Ed25519 keys (the SAME `loadBoardKeys` keyring —
//! [`crate::board_identity`] fetches
//! `{iss}/.well-known/agenticos-board-jwks.json`), then the pins the
//! contract binds: `iss` = the configured issuer, `aud` = THIS board's
//! exact public origin, `organization_id` = this instance's company,
//! `exp`/`iat` inside bounded skew, `exp - iat ≤ 300`, the
//! owner-only-operator rule, and scope ⊆ {`cli.read`, `cli.write`}.
//! Every refusal fails closed with a contract-shaped `error.code`; a
//! refused call changes nothing.
//!
//! The bearer spelling is `wikienv_<envelope>` — the mint's shared
//! prefix ([`BEARER_PREFIX`]); the route strips it before verify.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;

use std::path::Path;

use crate::board_identity::{self, PublicKey};
use crate::error::Error;

/// The bearer prefix the mint emits (`wikiActorBearer`, AOS-122) —
/// `Bearer wikienv_<header>.<claims>.<sig>`; the cli family shares it.
pub const BEARER_PREFIX: &str = "wikienv_";

/// `iat` may lead the clock by this much — and `exp` gets NO trailing
/// skew: `checkWikiActorEnvelope` refuses `exp <= now` outright, and
/// the container check is no looser than the worker's.
pub const SKEW_SECS: i64 = 30;
/// `HOSTED_CADENCE_MAX_TTL_SECONDS` — the envelope's longest life.
const MAX_LIFE_SECS: i64 = 300;
/// The whole bearer header value is bounded — an envelope is a few
/// hundred bytes; a megabyte of base64 is not one.
const BEARER_CAP: usize = 8192;

/// The remote verbs the board's `/api/cli/` route family serves — the
/// AOS-128 `HOSTED_CADENCE_CLI_VERBS` allowlist ∩ the contract's v1
/// table (they agree today). A verb absent here is refused before any
/// body is read — the path segment, never a request field, names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    // cli.read
    Status,
    AgentList,
    AgentShow,
    IssueLs,
    IssueShow,
    IssueHistory,
    MessageRead,
    MessageInbox,
    TeamList,
    // cli.write
    IssueNew,
    IssueComment,
    IssueSet,
    MessageSend,
}

/// The `cli.*` scope a verb needs — `cli.write` never implies
/// `cli.read` (the contract's `hostedCadenceCliVerbAllowed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Read,
    Write,
}

impl Verb {
    /// The path segment → the verb, or `None` (refused) for anything
    /// not allowlisted — including a `..` or a second `/`, which can
    /// never reach here (the segment grammar rejects them first).
    pub fn of(segment: &str) -> Option<Verb> {
        Some(match segment {
            "status" => Verb::Status,
            "agent_list" => Verb::AgentList,
            "agent_show" => Verb::AgentShow,
            "issue_ls" => Verb::IssueLs,
            "issue_show" => Verb::IssueShow,
            "issue_history" => Verb::IssueHistory,
            "message_read" => Verb::MessageRead,
            "message_inbox" => Verb::MessageInbox,
            "team_list" => Verb::TeamList,
            "issue_new" => Verb::IssueNew,
            "issue_comment" => Verb::IssueComment,
            "issue_set" => Verb::IssueSet,
            "message_send" => Verb::MessageSend,
            _ => return None,
        })
    }

    pub fn scope(self) -> Scope {
        match self {
            Verb::Status
            | Verb::AgentList
            | Verb::AgentShow
            | Verb::IssueLs
            | Verb::IssueShow
            | Verb::IssueHistory
            | Verb::MessageRead
            | Verb::MessageInbox
            | Verb::TeamList => Scope::Read,
            Verb::IssueNew | Verb::IssueComment | Verb::IssueSet | Verb::MessageSend => {
                Scope::Write
            }
        }
    }
}

/// Why an envelope or call was refused — `code` is the contract's
/// vocabulary; the worker's `checkCliCall` maps the same names.
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

/// The JWS header — exactly `{alg, typ, kid}`; the cli family shares
/// the wiki mint's `agenticos-wiki-actor/1` type string (AOS-128 reuses
/// `mintWikiActorEnvelope`), so the envelope family is told apart by
/// the claim set, never by `typ`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    typ: String,
    kid: String,
}

/// `hostedCadenceCliActorClaimsSchema` — the closed claim set. Every
/// field required, nothing else permitted: no `wiki_prefixes`, no
/// `client_id`/`azp`, no field that could rename the actor or the org.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    iss: String,
    /// The public board origin — `https://<slug>.cadencecloud.app`
    /// (the contract's `hostedCadenceAudienceSchema`), not a bare host.
    aud: String,
    sub: String,
    /// `operator` | `users/<h>` | `service/<h>` | `agents/<h>`.
    actor: String,
    principal_kind: String,
    /// The issuer-side membership role at mint: `owner` | `member`.
    role_at_issue: String,
    organization_id: String,
    scope: Vec<String>,
    /// The exact issuing credential — required in the claim set (a
    /// missing or extra field fails the closed shape); the container
    /// binds nothing further to it, so it is parsed, not read.
    #[allow(dead_code)]
    credential_id: String,
    iat: i64,
    exp: i64,
    jti: String,
}

/// The envelope's verified actor — the caller identity the route
/// derives every attribution from, never a request field.
#[derive(Clone, Debug)]
pub struct CliActor {
    /// `claims.sub` — the principal's server-issued id.
    pub sub: String,
    /// `claims.actor` verbatim — `operator`, `users/<sub>`,
    /// `service/<sub>` or `agents/<id>` (the schema bounds each).
    pub actor: String,
    /// The granted cli scopes, each already narrowed to the
    /// `{cli.read, cli.write}` vocabulary.
    pub scopes: Vec<String>,
    /// The single-use token id — the route may remember it until `exp`.
    pub jti: String,
    pub exp: i64,
    /// The `[A-Za-z0-9_-]` handle derived for attribution fields
    /// (comment authors, send provenance) — never "operator": an
    /// owner-user's envelope derives the `users/<sub>` spelling of its
    /// own subject, so a remote send/comment is never written as the
    /// board's operator handle.
    pub handle: String,
}

/// Extract the compact envelope from `Authorization`: exactly
/// `Bearer wikienv_<h>.<p>.<s>` — nothing else satisfies this route,
/// and a bearer meant for another route never parses here.
pub fn envelope_from_bearer(header: Option<&str>) -> Result<String, Rejection> {
    let refuse = || Rejection::new("unauthorized", "a cli actor envelope bearer is required");
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

/// Structure only — three non-empty base64url parts, the exact header,
/// the closed claim set. No signature or pin check runs here.
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
    if header.alg != "EdDSA" || header.typ != "agenticos-wiki-actor/1" || header.kid.is_empty() {
        return Err(Rejection::new(
            "bad_header",
            "envelope header is not the contract shape",
        ));
    }
    let claims: Claims = serde_json::from_slice(&decode(p)?)
        .map_err(|_| Rejection::new("bad_claims", "envelope claims are not the contract shape"))?;
    let signature = decode(s)?;
    if signature.len() != 64 {
        return Err(Rejection::invalid("envelope signature is not Ed25519"));
    }
    Ok((header, claims, format!("{h}.{p}"), signature))
}

/// The scheme a public `aud` origin carries for `host` — the board's
/// own rule (`operator::public_scheme`): `http` on `*.localhost` dev
/// names, `https` everywhere else.
fn public_scheme(host: &str) -> &'static str {
    let name = host.split(':').next().unwrap_or_default();
    if name == "localhost" || name.ends_with(".localhost") {
        "http"
    } else {
        "https"
    }
}

/// The `[A-Za-z0-9_-]` ≤64 attribution handle for a verified actor —
/// `users/<sub>`/`service/<sub>`/`agents/<id>` map to their namespaced
/// tail so the daemon-visible author can never read as `operator` or
/// collide with a pane alias it does not own.
fn handle_of(actor: &str, sub: &str) -> String {
    let tail = actor.split_once('/').map(|(_, t)| t).unwrap_or(sub);
    tail.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

/// Verify an envelope end to end (the container half of AOS-128's
/// `checkCliCall`): structure, `kid` → the verifying key, the Ed25519
/// signature, then every contract pin against `expected` — the pins
/// come from the daemon's own `board-identity.json` ([`Config`]),
/// never from the envelope or the request.
///
/// `expected_audience` is this board's exact public origin — the
/// caller computes `public_scheme(host)` once; `organization_id` is
/// the configured company.
pub fn verify(
    envelope: &str,
    expected: &board_identity::Config,
    key: &PublicKey,
    now: i64,
) -> Result<CliActor, Rejection> {
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
    // `aud` is the public ORIGIN — `https://<host>` on cadencecloud,
    // `http://<host>` on a `*.localhost` dev board (the board's
    // `public_scheme` rule).
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
    // Time bounds exactly as the worker's check (`iat > now + 30`,
    // `exp <= now`) plus the claim schema's `exp > iat` and ≤300 s
    // life — the envelope never outlives the authority that minted it.
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
    // The claim schema's invariants, re-checked against our parse:
    // every scope is the cli vocabulary; `operator` actor only ever
    // rides a verified owner USER; `owner` role never maps a service.
    if claims
        .scope
        .iter()
        .any(|s| s != "cli.read" && s != "cli.write")
    {
        return Err(Rejection::new(
            "bad_claims",
            "the envelope carries a scope outside the cli vocabulary",
        ));
    }
    if claims.actor == "operator"
        && !(claims.principal_kind == "user" && claims.role_at_issue == "owner")
    {
        return Err(Rejection::new(
            "bad_claims",
            "the operator actor requires a verified owner user",
        ));
    }
    if claims.role_at_issue == "owner"
        && claims.principal_kind != "user"
        && claims.principal_kind != "agent"
    {
        return Err(Rejection::new(
            "bad_claims",
            "a service can never carry the owner role",
        ));
    }
    let actor_ok = claims.actor == "operator"
        || claims
            .actor
            .strip_prefix("users/")
            .or_else(|| claims.actor.strip_prefix("service/"))
            .or_else(|| claims.actor.strip_prefix("agents/"))
            .is_some_and(|h| {
                !h.is_empty()
                    && h.len() <= 200
                    && h.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            });
    if !actor_ok {
        return Err(Rejection::new(
            "bad_claims",
            "the envelope actor is not a contract spelling",
        ));
    }
    Ok(CliActor {
        handle: handle_of(&claims.actor, &claims.sub),
        sub: claims.sub,
        actor: claims.actor,
        scopes: claims.scope,
        jti: claims.jti,
        exp: claims.exp,
    })
}

/// Does the verified actor's granted scope set cover `need`? The
/// envelope's `scope` is the server's intersected ceiling — this route
/// never widens it.
pub fn scope_covers(actor: &CliActor, need: Scope) -> bool {
    let want = match need {
        Scope::Read => "cli.read",
        Scope::Write => "cli.write",
    };
    actor.scopes.iter().any(|s| s == want)
}

// ---------- single-use `jti` ----------

/// The contract calls the envelope single-use — and a captured bearer
/// can be POSTed to this route directly, skipping the worker's
/// live-bearer rebind, so the container is itself the single-use
/// authority (spec review, PR #744). [`Spent`] is the persisted
/// seen-set: `sha256(jti)` → the envelope's own `exp`, so a restart
/// inside the ≤300 s window still refuses the replay, and an entry
/// prunes once its `exp` passes — a replay after `exp` fails the time
/// check first anyway.
const SPENT_FILE: &str = "cli-jtis.json";

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct SpentFile {
    /// `sha256(jti)` → `exp`. Only the digest is kept — the raw id is
    /// never persisted, exactly as `operator_auth`'s sign-in jtis.
    #[serde(default)]
    jtis: std::collections::HashMap<String, i64>,
}

/// Per-`state_dir` seen-set. `load` reads it; [`Spent::consume`] is
/// check-and-record in one step — the caller holds whatever lock keeps
/// two calls from racing on the same file.
pub struct Spent {
    path: std::path::PathBuf,
    jtis: std::collections::HashMap<String, i64>,
    /// The persisted file existed but would not parse — the safe
    /// failure is to refuse every consume until the set is writable
    /// again, never to let a replay slip through a gap in memory.
    corrupt: bool,
}

impl Spent {
    /// Load the seen-set of `state_dir`. A missing file is an empty
    /// set; a present-but-unreadable or unparseable one marks `corrupt`
    /// so [`Spent::consume`] fails closed — a corrupted single-use
    /// memory must never silently permit the replay it cannot see.
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

    /// First use records `sha256(jti)` until `exp` and returns `true`;
    /// any second in-window use returns `false` — the caller's
    /// `assertion_replayed` refusal — and writes nothing. A corrupt
    /// seen-set or a failed persist also returns `false`: no in-window
    /// envelope may run while the single-use memory is unreliable.
    /// Expired entries prune on every consume.
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
        // Persist BEFORE the caller dispatches — a crash after the write
        // must still know the id is spent, and a failed persist refuses
        // the call rather than silently keeping the id replayable.
        self.persist().is_ok()
    }

    /// Atomic write: tmp + rename, 0600 — a partially-persisted set is
    /// never read back as authoritative.
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
