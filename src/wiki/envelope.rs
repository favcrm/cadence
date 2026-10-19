//! CAD-1401: the container's verifier for the hosted **wiki actor
//! envelope** (`hostedCadenceWikiActorClaimsSchema`, AOS-122). It is a
//! separate claim set from the CLI envelope in `cli_actor`: the wiki
//! envelope carries `wiki_prefixes` and a per-scope `prefix_grants`
//! ceiling, and the CLI envelope carries neither. The CLI verifier is
//! reused for the substrate (JWS shape, Ed25519, pins, expiry), never
//! its schema.
//!
//! Verification happens in this process, against this board's own
//! identity pins. A verified envelope yields a [`WikiGrant`]: the
//! attributable caller plus the prefix ceiling every result must fit.
//! The ceiling is per scope (`prefix_grants`), so a `wiki.write` action
//! is checked against the write entry only, never a shared read root.

use serde::Deserialize;

use crate::board_identity::{self, PublicKey};
use crate::cli_actor::{self, Rejection, SKEW_SECS};

/// `HOSTED_CADENCE_MAX_TTL_SECONDS` — the longest envelope life.
const MAX_LIFE_SECS: i64 = 300;
/// Each scope's prefix list is bounded by the contract (`max(16)`).
const MAX_PREFIXES: usize = 16;

/// The wiki scopes the contract defines. `wiki.delete` exists in the
/// vocabulary but no wiki action this route serves needs it, so a
/// grant carrying it is refused as outside this route's allowlist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WikiScope {
    Read,
    Write,
}

impl WikiScope {
    fn parse(value: &str) -> Option<WikiScope> {
        match value {
            "wiki.read" => Some(WikiScope::Read),
            "wiki.write" => Some(WikiScope::Write),
            _ => None,
        }
    }
}

/// The contract's `wiki.read` ⊂ enumerate/read, `wiki.write` ⊂ write.
/// The action → scope map lives here, once, for the translated routes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WikiAction {
    Enumerate,
    Read,
    Write,
}

impl WikiAction {
    pub fn scope(self) -> WikiScope {
        match self {
            WikiAction::Enumerate | WikiAction::Read => WikiScope::Read,
            WikiAction::Write => WikiScope::Write,
        }
    }
}

/// `hostedCadenceWikiActorClaimsSchema`, closed. Every field is
/// required and nothing else is permitted.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    iss: String,
    aud: String,
    sub: String,
    actor: String,
    principal_kind: String,
    role_at_issue: String,
    organization_id: String,
    scope: Vec<String>,
    wiki_prefixes: Vec<String>,
    prefix_grants: Vec<Grant>,
    credential_id: String,
    iat: i64,
    exp: i64,
    jti: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    scope: String,
    prefixes: Vec<String>,
}

/// A verified wiki envelope: the caller identity (from the signed
/// claims only) and the per-scope prefix ceiling.
#[derive(Clone, Debug)]
pub struct WikiGrant {
    pub sub: String,
    /// Verified board/company pins; transfer jobs retain this authority
    /// namespace across fresh envelopes and daemon restarts.
    pub issuer: String,
    pub organization_id: String,
    pub audience: String,
    /// `operator` only for a verified owner user; otherwise the
    /// namespaced `users/`, `service/` or `agents/` spelling.
    pub actor: String,
    pub jti: String,
    pub exp: i64,
    scopes: Vec<WikiScope>,
    /// `prefix_grants` keyed by scope, each list already validated.
    grants: Vec<(WikiScope, Vec<String>)>,
}

impl WikiGrant {
    /// The prefix ceiling for `scope`, or `None` when the envelope
    /// never granted that scope.
    fn prefixes_for(&self, scope: WikiScope) -> Option<&[String]> {
        if !self.scopes.contains(&scope) {
            return None;
        }
        self.grants
            .iter()
            .find(|(s, _)| *s == scope)
            .map(|(_, p)| p.as_slice())
    }

    /// Does this envelope authorize `action`? The action's scope must be
    /// granted, and the caller must hold a prefix for it.
    pub fn allows(&self, action: WikiAction) -> bool {
        self.prefixes_for(action.scope())
            .is_some_and(|p| !p.is_empty())
    }

    /// Is a normalized file path a strict descendant of any prefix granted
    /// for `action`? Unlike directory reads, a file cannot be the prefix itself.
    pub fn contains_file(&self, action: WikiAction, path: &str) -> bool {
        self.prefixes_for(action.scope()).is_some_and(|prefixes| {
            prefixes
                .iter()
                .any(|prefix| path.starts_with(prefix.as_str()))
        })
    }

    /// Is `path` (a normalized, root-relative wiki path with no trailing
    /// slash) inside the prefix ceiling for `action`'s scope? Every read
    /// result is checked here, listings and search hits included.
    pub fn contains(&self, action: WikiAction, path: &str) -> bool {
        let Some(prefixes) = self.prefixes_for(action.scope()) else {
            return false;
        };
        prefixes.iter().any(|prefix| {
            // A prefix ends in '/'. A path equal to the prefix's own
            // directory name (without the slash) is the directory itself.
            let dir = prefix.trim_end_matches('/');
            path == dir || path.starts_with(prefix.as_str())
        })
    }
}

/// Verify a wiki envelope end to end: structure, `kid`, Ed25519
/// signature, then every pin against this board's own configuration.
/// The pins never come from the envelope or the request.
pub fn verify(
    envelope: &str,
    expected: &board_identity::Config,
    key: &PublicKey,
    now: i64,
) -> Result<WikiGrant, Rejection> {
    let (claims, signed, signature, kid) = parse(envelope)?;
    if kid != key.kid {
        return Err(Rejection {
            code: "assertion_invalid",
            message: "the envelope's key id does not name the verifying key".into(),
        });
    }
    {
        use ring::signature::{UnparsedPublicKey, ED25519};
        UnparsedPublicKey::new(&ED25519, &key.x)
            .verify(signed.as_bytes(), &signature)
            .map_err(|_| Rejection {
                code: "signature_invalid",
                message: "the envelope's signature does not verify".into(),
            })?;
    }
    if claims.iss != expected.issuer {
        return Err(refuse(
            "issuer_mismatch",
            "the envelope was not issued by this board's platform",
        ));
    }
    let want_aud = format!("{}://{}", public_scheme(&expected.host), expected.host);
    if claims.aud != want_aud {
        return Err(refuse(
            "audience_mismatch",
            "the envelope was minted for another board host",
        ));
    }
    if claims.organization_id != expected.company {
        return Err(refuse(
            "not_a_member",
            "the envelope names a workspace this instance does not serve",
        ));
    }
    if claims.iat > now + SKEW_SECS
        || claims.exp <= now
        || claims.exp <= claims.iat
        || claims.exp - claims.iat > MAX_LIFE_SECS
    {
        return Err(refuse(
            "assertion_expired",
            "the envelope is expired or outside its time bounds",
        ));
    }
    if claims.jti.is_empty() || claims.credential_id.is_empty() {
        return Err(refuse(
            "bad_claims",
            "the envelope lacks its jti or credential",
        ));
    }
    // Actor spelling and the operator rule, as the contract's refine.
    let actor_ok = claims.actor == "operator"
        || ["users/", "service/", "agents/"].iter().any(|ns| {
            claims.actor.strip_prefix(ns).is_some_and(|h| {
                !h.is_empty()
                    && h.len() <= 200
                    && h.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            })
        });
    if !actor_ok {
        return Err(refuse(
            "bad_claims",
            "the envelope actor is not a contract spelling",
        ));
    }
    if claims.actor == "operator"
        && !(claims.principal_kind == "user" && claims.role_at_issue == "owner")
    {
        return Err(refuse(
            "bad_claims",
            "the operator actor requires a verified owner user",
        ));
    }
    if claims.role_at_issue == "owner"
        && claims.principal_kind != "user"
        && claims.principal_kind != "agent"
    {
        return Err(refuse(
            "bad_claims",
            "a service can never carry the owner role",
        ));
    }
    if !matches!(claims.principal_kind.as_str(), "user" | "service" | "agent") {
        return Err(refuse(
            "bad_claims",
            "the envelope principal kind is unknown",
        ));
    }

    // Scopes: each one wiki, each once, and granted exactly once by a
    // prefix grant. A `wiki.delete` or a cli scope is outside this route.
    let mut scopes = Vec::with_capacity(claims.scope.len());
    for raw in &claims.scope {
        let scope = WikiScope::parse(raw)
            .ok_or_else(|| refuse("bad_claims", "the envelope carries a non-wiki scope"))?;
        if scopes.contains(&scope) {
            return Err(refuse("bad_claims", "the envelope repeats a wiki scope"));
        }
        scopes.push(scope);
    }
    if scopes.is_empty() || claims.prefix_grants.len() != scopes.len() {
        return Err(refuse(
            "bad_claims",
            "prefix_grants must cover exactly the granted scopes",
        ));
    }
    let mut grants = Vec::with_capacity(claims.prefix_grants.len());
    for grant in &claims.prefix_grants {
        let scope = WikiScope::parse(&grant.scope)
            .filter(|s| scopes.contains(s))
            .ok_or_else(|| refuse("bad_claims", "a prefix grant names an ungranted scope"))?;
        if grants.iter().any(|(s, _)| *s == scope) {
            return Err(refuse("bad_claims", "a prefix grant repeats its scope"));
        }
        if grant.prefixes.is_empty() || grant.prefixes.len() > MAX_PREFIXES {
            return Err(refuse(
                "bad_claims",
                "a prefix grant has no bounded prefix set",
            ));
        }
        for prefix in &grant.prefixes {
            if !valid_prefix(prefix) {
                return Err(refuse(
                    "bad_claims",
                    "a prefix grant names a malformed prefix",
                ));
            }
        }
        grants.push((scope, grant.prefixes.clone()));
    }
    // wiki_prefixes must equal the union of the granted prefixes.
    let union: std::collections::BTreeSet<&str> = grants
        .iter()
        .flat_map(|(_, p)| p.iter().map(String::as_str))
        .collect();
    let listed: std::collections::BTreeSet<&str> =
        claims.wiki_prefixes.iter().map(String::as_str).collect();
    if union != listed || claims.wiki_prefixes.len() != listed.len() {
        return Err(refuse(
            "bad_claims",
            "wiki_prefixes is not the union of the prefix grants",
        ));
    }

    Ok(WikiGrant {
        sub: claims.sub,
        issuer: claims.iss,
        organization_id: claims.organization_id,
        audience: claims.aud,
        actor: claims.actor,
        jti: claims.jti,
        exp: claims.exp,
        scopes,
        grants,
    })
}

fn refuse(code: &'static str, message: &str) -> Rejection {
    Rejection {
        code,
        message: message.into(),
    }
}

/// A prefix is a normalized root-relative directory ending in `/`:
/// no leading slash, no backslash, no NUL, no `..`, no `:` segment, no
/// control character. The same grammar `hostedCadenceWikiPrefixSchema`
/// accepts.
fn valid_prefix(prefix: &str) -> bool {
    prefix.len() <= 260
        && prefix.ends_with('/')
        && !prefix.starts_with('/')
        && !prefix.contains('\\')
        && prefix.split('/').all(|seg| {
            seg != ".." && !seg.contains(':') && !seg.bytes().any(|b| b < 0x20 || b == 0x7f)
        })
        && !prefix.trim_end_matches('/').is_empty()
}

/// The public-origin scheme rule the CLI verifier also uses.
fn public_scheme(host: &str) -> &'static str {
    let name = host.split(':').next().unwrap_or_default();
    if name == "localhost" || name.ends_with(".localhost") {
        "http"
    } else {
        "https"
    }
}

/// Structure only: three base64url parts, the exact header, the closed
/// claim set. Returns the claims, the signed bytes, the signature and
/// the header `kid`.
fn parse(envelope: &str) -> Result<(Claims, String, Vec<u8>, String), Rejection> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let mut parts = envelope.split('.');
    let (h, p, s) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s), None) if !h.is_empty() && !p.is_empty() && !s.is_empty() => {
            (h, p, s)
        }
        _ => return Err(refuse("assertion_invalid", "envelope is not a compact JWS")),
    };
    let decode = |part: &str| {
        URL_SAFE_NO_PAD
            .decode(part)
            .map_err(|_| refuse("assertion_invalid", "envelope is not base64url"))
    };
    let header: serde_json::Value = serde_json::from_slice(&decode(h)?)
        .map_err(|_| refuse("bad_header", "envelope header is not the contract shape"))?;
    let exact = header.as_object().is_some_and(|o| o.len() == 3)
        && header["alg"] == "EdDSA"
        && header["typ"] == "agenticos-wiki-actor/1";
    let kid = header["kid"].as_str().unwrap_or_default().to_string();
    if !exact || kid.is_empty() {
        return Err(refuse(
            "bad_header",
            "envelope header is not the contract shape",
        ));
    }
    let claims: Claims = serde_json::from_slice(&decode(p)?)
        .map_err(|_| refuse("bad_claims", "envelope claims are not the contract shape"))?;
    let signature = decode(s)?;
    if signature.len() != 64 {
        return Err(refuse(
            "assertion_invalid",
            "envelope signature is not Ed25519",
        ));
    }
    Ok((claims, format!("{h}.{p}"), signature, kid))
}

/// The bearer spelling `wikienv_<h>.<p>.<s>` — the same shared prefix
/// the CLI route strips. Re-exported so the route has one parser.
pub fn envelope_from_bearer(header: Option<&str>) -> Result<String, Rejection> {
    cli_actor::envelope_from_bearer(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(scopes: &[WikiScope], prefixes: &[(&str, &[&str])]) -> WikiGrant {
        WikiGrant {
            sub: "s".into(),
            issuer: "https://issuer.example.test".into(),
            organization_id: "fixture-company".into(),
            audience: "https://board.example.test".into(),
            actor: "users/u".into(),
            jti: "j".into(),
            exp: 0,
            scopes: scopes.to_vec(),
            grants: prefixes
                .iter()
                .map(|(scope, ps)| {
                    let s = WikiScope::parse(scope).unwrap();
                    (s, ps.iter().map(|p| p.to_string()).collect())
                })
                .collect(),
        }
    }

    #[test]
    fn a_write_grant_never_reaches_a_read_only_root() {
        // The member reads the shared root but writes only its own.
        let g = grant(
            &[WikiScope::Read, WikiScope::Write],
            &[("wiki.read", &["global/"]), ("wiki.write", &["users/u/"])],
        );
        assert!(g.allows(WikiAction::Read));
        assert!(g.contains(WikiAction::Read, "global/a.md"));
        assert!(!g.contains(WikiAction::Write, "global/a.md"));
        assert!(g.contains(WikiAction::Write, "users/u/a.md"));
    }

    #[test]
    fn a_scope_never_granted_is_refused() {
        let g = grant(&[WikiScope::Read], &[("wiki.read", &["global/"])]);
        assert!(!g.allows(WikiAction::Write));
        assert!(!g.contains(WikiAction::Write, "global/a.md"));
    }

    #[test]
    fn a_sibling_that_shares_a_name_prefix_is_outside() {
        let g = grant(&[WikiScope::Read], &[("wiki.read", &["global/"])]);
        assert!(!g.contains(WikiAction::Read, "global-evil/x.md"));
        assert!(g.contains(WikiAction::Read, "global"));
    }

    #[test]
    fn file_authorization_requires_a_descendant_of_any_action_prefix() {
        let g = grant(
            &[WikiScope::Read, WikiScope::Write],
            &[
                ("wiki.read", &["global/"]),
                ("wiki.write", &["global/scoped/", "global/scoped/narrow/"]),
            ],
        );
        assert!(g.contains(WikiAction::Read, "global"));
        assert!(!g.contains_file(WikiAction::Write, "global/scoped"));
        assert!(!g.contains_file(WikiAction::Write, "global/scoped.extracted.md"));
        assert!(!g.contains_file(WikiAction::Write, "global/scoped-evil/a.md"));
        assert!(!g.contains_file(WikiAction::Write, "global/outside.md"));
        assert!(g.contains_file(WikiAction::Write, "global/scoped/a.md"));
        assert!(g.contains_file(WikiAction::Write, "global/scoped/narrow"));
        assert!(g.contains_file(WikiAction::Write, "global/scoped/narrow/a.md"));
        let read_only = grant(&[WikiScope::Read], &[("wiki.read", &["global/"])]);
        assert!(!read_only.contains_file(WikiAction::Write, "global/a.md"));
    }

    #[test]
    fn prefixes_refuse_traversal_and_absolute_forms() {
        assert!(valid_prefix("global/"));
        assert!(!valid_prefix("/global/"));
        assert!(!valid_prefix("global/../x/"));
        assert!(!valid_prefix("global"));
        assert!(!valid_prefix("a:b/"));
        assert!(!valid_prefix("a\\b/"));
    }
}
