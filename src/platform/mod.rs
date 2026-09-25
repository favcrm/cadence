//! CAD-366 / ADR 0006 §5.1, §5.3, §5.5: the connected-platform trust
//! boundary. Custody holds enrolled platform credentials; grants name
//! what an agent may call; the check the proxy (CAD-506) runs before
//! any platform traffic lives here.
//!
//! Nothing in this module ever returns credential bytes to a caller.
//! `enroll` takes them, hands them to [`custody`], and records a
//! fingerprint; every result, event and refusal carries handles only.

pub mod adapter;
pub mod custody;

pub use adapter::PlatformAdapter;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::store::{Grant, Store};

pub use custody::{Custody, Key};

/// What `platform enroll` produces for one exchange — the credential
/// bytes plus the scope set the enrollment records. Callers (the RPC
/// layer) never see `bytes` after this.
pub struct Enrollment {
    pub bytes: Vec<u8>,
    /// The scopes the record carries — the operator's declared set,
    /// or the platform-granted set a consent exchange returns.
    pub scopes: Vec<String>,
    /// `token` or `consent` — the exchange shape that produced it.
    pub exchange: &'static str,
}

// ---------- enrollment ----------

/// §5.3, exchange shape 1 — the operator-minted scoped token: bytes
/// off the request, classified first (a personal approval token is not
/// enrollable). Surrounding whitespace is a paste error, not part of
/// the credential: it is trimmed before screening and custody, so a
/// padded `ghp_…` is still refused.
pub fn enroll_token(params: &Value, declared: &[String]) -> Result<Enrollment> {
    let raw = params
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected("Missing or non-string 'token'"))?;
    let token = raw.trim();
    if token.is_empty() || token.len() > 8192 {
        return Err(Error::rejected(
            "'token' must be 1-8192 non-whitespace chars",
        ));
    }
    if token.chars().any(char::is_whitespace) {
        return Err(Error::rejected(
            "'token' contains whitespace — a credential never does; \
             check the paste",
        ));
    }
    refuse_personal(params.get("class").and_then(Value::as_str), token)?;
    Ok(Enrollment {
        bytes: token.as_bytes().to_vec(),
        scopes: declared.to_vec(),
        exchange: "token",
    })
}

/// §5.3, exchange shape 2 — the consent exchange: the platform's
/// device-code/OTP flow, run by the operator, mints a scoped
/// credential for the daemon. The adapter that knows a platform's
/// flow registers in [`CONSENT_ADAPTERS`]; no adapter is registered
/// yet (CAD-501 lands AgenticOS's), so a consent enroll names the
/// missing adapter and refuses.
pub type ConsentExchange = fn(&ConsentRequest) -> Result<ConsentOutcome>;

pub struct ConsentRequest<'a> {
    pub platform: &'a str,
    pub account: &'a str,
    /// Scopes the operator asked for on the consent screen.
    pub scopes: &'a [String],
    /// Adapter-specific fields (`params` minus the handled ones).
    pub params: &'a Value,
}

/// What an adapter's consent exchange mints.
pub struct ConsentOutcome {
    /// The platform-issued credential — to custody, never a caller.
    pub credential: Vec<u8>,
    /// The scopes the platform actually granted, when the adapter can
    /// read them back; the declared set is recorded otherwise.
    pub granted_scopes: Option<Vec<String>>,
}

/// `(platform, exchange)` pairs — the hook CAD-501 fills. A platform
/// not listed has no consent shape: enrollment refuses.
static CONSENT_ADAPTERS: &[(&str, ConsentExchange)] = &[];

pub fn consent_adapter(platform: &str) -> Option<ConsentExchange> {
    CONSENT_ADAPTERS
        .iter()
        .find(|(p, _)| *p == platform)
        .map(|(_, f)| *f)
}

/// Run `platform`'s consent exchange, or refuse naming why. `token`
/// params never participate — consent mints its own credential.
pub fn enroll_consent(
    platform: &str,
    account: &str,
    declared: &[String],
    params: &Value,
) -> Result<Enrollment> {
    if params.get("token").is_some() {
        return Err(Error::rejected(
            "'token' does not belong to a consent exchange — the platform \
             issues its credential to the daemon directly (ADR 0006 §5.3)",
        ));
    }
    let exchange = consent_adapter(platform).ok_or_else(|| {
        Error::rejected(format!(
            "no consent-exchange adapter is registered for platform '{platform}' — \
             CAD-501 lands AgenticOS's; enroll a scoped token with `shape: \"token\"`"
        ))
    })?;
    let outcome = exchange(&ConsentRequest {
        platform,
        account,
        scopes: declared,
        params,
    })?;
    if outcome.credential.is_empty() {
        return Err(Error::internal(
            "consent adapter returned an empty credential — refusing to enroll",
        ));
    }
    let scopes = outcome.granted_scopes.unwrap_or_else(|| declared.to_vec());
    if scopes.is_empty() {
        return Err(Error::rejected(
            "a consent exchange must produce a scoped credential — none were granted",
        ));
    }
    Ok(Enrollment {
        bytes: outcome.credential,
        scopes,
        exchange: "consent",
    })
}

/// §5.3: a personal approval/publish credential is not enrollable.
/// `class` is the operator's declaration — anything but `scoped`
/// refuses by name; `token` is also screened against the personal
/// credential prefixes platforms mint (a fine-grained token is
/// scoped; a user-class one is not). Per-platform verification lands
/// with the adapters (CAD-367, CAD-501).
fn refuse_personal(class: Option<&str>, token: &str) -> Result<()> {
    match class.unwrap_or("scoped") {
        "scoped" => {}
        other => {
            return Err(Error::rejected(format!(
                "a '{other}' credential is a personal approval/publish token — \
                 not enrollable (ADR 0006 §5.3); custody holds platform-scoped \
                 credentials only"
            )))
        }
    }
    if let Some(kind) = personal_token_kind(token) {
        return Err(Error::rejected(format!(
            "the token's shape is a {kind} — a personal credential, not a \
             platform-scoped one; enroll a narrowly scoped platform token \
             (ADR 0006 §5.3)"
        )));
    }
    Ok(())
}

/// The personal-credential class `token`'s prefix names, if any. A
/// best-effort screen until platform adapters verify enrolled tokens
/// — every user-bound shape a platform mints is refused: GitHub's
/// `ghu_`/`gho_`/`ghp_`/`ghr_`/`github_pat_` family (user-to-server,
/// OAuth, classic and fine-grained personal tokens, refresh tokens)
/// and GitLab's `glpat-`. App-bound credentials are not personal —
/// `ghs_` (a GitHub App installation token) enrolls under its
/// declared scopes; per-platform verification lands with the adapters
/// (CAD-367, CAD-501).
fn personal_token_kind(token: &str) -> Option<&'static str> {
    for (prefix, kind) in [
        ("ghu_", "GitHub user-to-server token"),
        ("gho_", "GitHub OAuth user token"),
        ("ghp_", "GitHub personal access token"),
        ("ghr_", "GitHub refresh token"),
        ("github_pat_", "GitHub fine-grained personal access token"),
        ("glpat-", "GitLab personal access token"),
    ] {
        if token.starts_with(prefix) {
            return Some(kind);
        }
    }
    None
}

// ---------- the grant check (CAD-506's gate calls this) ----------

/// The check §5.3 requires before any platform traffic: `agent` may
/// call `platform`/`account` at `scope` only while a grant covers it.
/// `Ok(grant)` admits; the refusal names the missing scope — never a
/// credential — and is raised before custody is even consulted.
pub fn require_grant(
    store: &Store,
    agent: &str,
    platform: &str,
    account: &str,
    scope: &str,
) -> Result<Grant> {
    crate::store::scope_name(scope)?;
    match store.platform_grant(agent, platform, account)? {
        Some(grant) if grant.covers(scope) => Ok(grant),
        Some(grant) => Err(Error::rejected(format!(
            "scope '{scope}' is not in '{agent}'s grant on {platform}/{account} \
             (granted: {}) — the operator widens it with `cadence platform grant`",
            grant.scopes.join(", ")
        ))),
        None => {
            let enrolled = store.platform_credential(platform, account)?.is_some();
            Err(Error::rejected(if enrolled {
                format!(
                    "'{agent}' holds no grant on {platform}/{account} — scope \
                     '{scope}' is not granted; the operator grants with \
                     `cadence platform grant`"
                )
            } else {
                format!(
                    "no credential is enrolled for {platform}/{account} — the \
                     operator enrolls with `cadence platform enroll`, then grants"
                )
            }))
        }
    }
}

/// The credential bytes for `(platform, account)` — the only path
/// that ever reads custody. The effect gate calls it inside a grant
/// check (never from an RPC handler), and it verifies the bytes still
/// match the recorded fingerprint so a custody/record tear (a crash
/// between the custody write and the record write at rotate) refuses
/// instead of proxying stale bytes.
pub fn load_credential(
    store: &Store,
    custody: &Custody,
    platform: &str,
    account: &str,
) -> Result<Vec<u8>> {
    let record = store
        .platform_credential(platform, account)?
        .ok_or_else(|| {
            Error::rejected(format!(
                "no credential is enrolled for {platform}/{account}"
            ))
        })?;
    let bytes = custody.load(&record.custody, &Key { platform, account })?;
    if crate::secret::fingerprint(&bytes) != record.fingerprint {
        return Err(Error::internal(format!(
            "custody bytes for {platform}/{account} no longer match the recorded \
             fingerprint — re-enroll or rotate"
        )));
    }
    Ok(bytes)
}

/// Belt-and-braces: a serialized result, event payload or error text
/// must never contain `secret`. Callers check before the value leaves
/// the daemon; a hit is a bug — the refusal withholds the value and
/// names only where the leak would have surfaced.
pub fn refuse_leak(what: &str, text: &str, secret: &[u8]) -> Result<()> {
    let secret = String::from_utf8_lossy(secret);
    if !secret.is_empty() && text.contains(secret.as_ref()) {
        return Err(Error::internal(format!(
            "{what} would carry the enrolled credential — withheld"
        )));
    }
    Ok(())
}
