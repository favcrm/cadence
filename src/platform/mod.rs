//! CAD-366 / ADR 0006 §5.1, §5.3, §5.5: the connected-platform trust
//! boundary. Custody holds enrolled platform credentials; grants name
//! what an agent may call; the check the proxy (CAD-506) runs before
//! any platform traffic lives here.
//!
//! Nothing in this module ever returns credential bytes to a caller.
//! `enroll` takes them, hands them to [`custody`], and records a
//! fingerprint; every result, event and refusal carries handles only.

pub mod adapter;
pub mod agenticos;
pub mod agenticos_external;
pub mod connections;
pub mod custody;
pub mod deployments;
pub mod hosted_email;
pub mod local;
pub mod smtp;
pub mod smtp_internal;

pub use adapter::{AppArtifactError, PlatformAdapter};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Exact operator-visible price of one frozen provider capability call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppCapabilityQuote {
    pub schema: u32,
    pub currency: String,
    pub unit_price_micros: u64,
    pub units: u32,
    pub total_price_micros: u64,
    pub price_revision: String,
}

impl AppCapabilityQuote {
    pub fn valid(&self) -> bool {
        self.schema == 1
            && self.currency == "USD"
            && (1..=100).contains(&self.units)
            && self.unit_price_micros > 0
            && self.total_price_micros == self.unit_price_micros.saturating_mul(self.units as u64)
            && self.total_price_micros <= 1_000_000_000
            && !self.price_revision.is_empty()
            && self.price_revision.len() <= 128
    }
}

#[cfg(test)]
mod app_capability_quote_tests {
    use super::AppCapabilityQuote;

    #[test]
    fn cad632_quote_refuses_forged_total_currency_or_revision() {
        let valid = AppCapabilityQuote {
            schema: 1,
            currency: "USD".into(),
            unit_price_micros: 1_880,
            units: 1,
            total_price_micros: 1_880,
            price_revision: "price-v1".into(),
        };
        assert!(valid.valid());
        let mut changed = valid.clone();
        changed.total_price_micros += 1;
        assert!(!changed.valid());
        changed = valid.clone();
        changed.currency = "EUR".into();
        assert!(!changed.valid());
        changed = valid;
        changed.price_revision.clear();
        assert!(!changed.valid());
    }
}

/// Bounded provider output. The broker stores the JSON receipt and optional
/// downloaded bytes before exposing either to an app caller.
pub struct AppCapabilityOutput {
    pub result: Value,
    pub asset: Option<AppCapabilityAsset>,
}

pub struct AppCapabilityAsset {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

use crate::error::{Error, Result};
use crate::store::{Grant, Store};

pub use custody::{Custody, Key};

/// CAD-1065: one fixed monotonic budget for the whole
/// `connection_test` operation. Every bounded step — custody read,
/// DNS slot, connect, TLS, AUTH, QUIT — draws from the same deadline;
/// `remaining` returns `None` once it has elapsed, so no step ever
/// computes a zero or negative wait.
pub struct OpDeadline {
    end: std::time::Instant,
}

impl OpDeadline {
    /// A fresh budget of `seconds` from now.
    pub fn in_seconds(seconds: u64) -> Self {
        Self {
            end: std::time::Instant::now() + std::time::Duration::from_secs(seconds),
        }
    }

    /// Time left, or `None` when the budget is spent.
    pub fn remaining(&self) -> Option<std::time::Duration> {
        self.end.checked_duration_since(std::time::Instant::now())
    }

    /// True when nothing is left — the operation classifies `timeout`.
    pub fn expired(&self) -> bool {
        self.remaining().is_none()
    }
}

/// The built-in account the `local` platform always exposes — no
/// enrollment, no custody, no `--accept-same-uid-risk` (CAD-577). The
/// local outbox is always available, so a fresh app install is runnable
/// from the board with no CLI setup.
pub const BUILTIN_LOCAL_ACCOUNT: &str = "local";

/// Provider-owned account metadata; composition reviews this exact allowlist.
/// This is credential selection, not capability matching or grant derivation.
pub struct BuiltinAccount {
    pub platform: &'static str,
    pub account: &'static str,
}

const BUILTIN_ACCOUNTS: &[BuiltinAccount] = &[local::BUILTIN_ACCOUNT, agenticos::BUILTIN_ACCOUNT];

/// Is `(platform, account)` a built-in account that needs no
/// enrollment? `local/local` (CAD-577) and hosted `agenticos/hosted`
/// (CAD-501): the gate treats them as connected, with no credential
/// bytes to load. Every other account still enrolls.
pub fn is_builtin(platform: &str, account: &str) -> bool {
    BUILTIN_ACCOUNTS
        .iter()
        .any(|known| known.platform == platform && known.account == account)
}

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
    /// The exact bytes the leak screens hold outputs against.
    /// Opaque exchanges screen the whole credential; structured
    /// custody (CAD-785 SMTP) screens only the password — the
    /// transport fields and sender are legitimately projected into
    /// operator metadata, so screening the whole document would trip
    /// on shared JSON framing instead of real leaks.
    pub screen: Option<Vec<u8>>,
}

impl Enrollment {
    /// Bytes the `refuse_leak` screens must hold outputs against.
    pub fn screen_bytes(&self) -> &[u8] {
        self.screen.as_deref().unwrap_or(&self.bytes)
    }
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
            "'token' length must be 1-8192 bytes after trimming whitespace",
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
        screen: None,
    })
}

/// CAD-785, exchange shape `smtp` — the operator-typed SMTP sender:
/// host, port, TLS mode, username, secret and verified sender
/// identity. The transport fields and sender ride custody bytes
/// with the secret (never App SQLite, never an opaque token); the
/// record keeps the fingerprint and the `smtp` exchange name. The
/// declared scopes must be exactly the reviewed `email:send` scope.
pub fn enroll_smtp(params: &Value, declared: &[String]) -> Result<Enrollment> {
    // `declared` arrives validated by the custody path — inherited
    // from the live record on rotate — so only the reviewed scope
    // set enrolls, on either verb.
    if declared != [smtp::SCOPE_EMAIL_SEND] {
        return Err(Error::rejected(
            "SMTP enrollment carries exactly the reviewed email:send scope",
        ));
    }
    let enrollment = smtp::parse_enrollment(params)?;
    let bytes = smtp::custody_bytes(&enrollment)?;
    Ok(Enrollment {
        screen: Some(enrollment.secret.clone()),
        bytes,
        scopes: declared.to_vec(),
        exchange: "smtp",
    })
}

/// §5.3, exchange shape 2 — the consent exchange: the platform's
/// device-code/OTP flow, run by the operator, mints a scoped
/// credential for the daemon. The adapter that knows a platform's
/// flow registers in [`CONSENT_ADAPTERS`]. AgenticOS does not: its
/// hosted door binds the company with no credential, and a
/// self-hosted grant is an enrolled scoped token (`shape: "token"`),
/// not a board consent exchange (CAD-501).
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
             AgenticOS's hosted door needs none; enroll a scoped token with \
             `shape: \"token\"`"
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
        screen: None,
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
            Err(Error::rejected(
                if enrolled || is_builtin(platform, account) {
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
                },
            ))
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
    // The built-in account holds no bytes — there is nothing to load
    // and nothing to leak (CAD-577). The gate still runs its grant
    // check before this is ever reached.
    if is_builtin(platform, account) {
        return Ok(Vec::new());
    }
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
/// must never contain `secret` or a fragment of eight or more characters.
/// Callers check before the value leaves
/// the daemon; a hit is a bug — the refusal withholds the value and
/// names only where the leak would have surfaced.
pub fn refuse_leak(what: &str, text: &str, secret: &[u8]) -> Result<()> {
    let secret = String::from_utf8_lossy(secret);
    let characters: Vec<_> = secret.chars().collect();
    let carries_secret = !secret.is_empty() && text.contains(secret.as_ref());
    let carries_fragment = characters
        .windows(8)
        .any(|fragment| text.contains(&fragment.iter().collect::<String>()));
    if carries_secret || carries_fragment {
        return Err(Error::internal(format!(
            "{what} would carry the enrolled credential — withheld"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn leak_screen_refuses_prefix_suffix_and_interior_fragments() {
        let credential = b"abcdefghIJKLMNOPqrstuvwx";
        for fragment in ["abcdefgh", "IJKLMNOP", "qrstuvwx"] {
            let error = refuse_leak("revoke reason", &format!("lost {fragment}!"), credential)
                .unwrap_err()
                .to_string();
            assert!(error.contains("withheld"));
            assert!(!error.contains(fragment));
        }
        assert!(refuse_leak("revoke reason", "abcdefg", credential).is_ok());
        assert!(refuse_leak("revoke reason", "unrelated", credential).is_ok());
        assert!(refuse_leak("result", "short", b"short").is_err());
        assert!(refuse_leak("result", "anything", b"").is_ok());
        // Eight means characters, not UTF-8 bytes: four non-ASCII
        // characters are not an eight-character credential fragment.
        let unicode = "αβγδεζηθικλμ";
        assert!(refuse_leak("reason", "αβγδ", unicode.as_bytes()).is_ok());
        assert!(refuse_leak("reason", "αβγδεζηθ", unicode.as_bytes()).is_err());
    }

    #[test]
    fn token_length_refusal_names_length_without_echoing_input() {
        for token in [String::new(), "x".repeat(8193)] {
            let error = enroll_token(&json!({"token": token}), &[])
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains("length"), "{error}");
            assert!(error.contains("8192"), "{error}");
            assert!(!error.contains(&"x".repeat(32)));
        }
        assert!(enroll_token(&json!({"token": "x".repeat(8192)}), &[]).is_ok());
    }
}
