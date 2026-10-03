//! CAD-1063: the hosted CRM email transport.
//!
//! A hosted tenant container has no internet egress, so the SMTP client
//! cannot reach any mail server. Hosted CRM mail instead goes through the
//! platform's tenant email path, `POST /v1/runtime/email/send` on
//! `api.internal` (AgenticOS AOS-53): the company and instance come from
//! the platform's provisioning context, the caller must hold the company
//! lease, and the platform binds the `idempotency-key` header and a
//! SHA-256 digest of `{to, subject, text, html}` in its approval ledger.
//! It executes through the tenant's own email provider only when that
//! key is `granted` (standing grant) or `approved` (owner decision), at
//! most once per key.
//!
//! What this module owns:
//! - the request: exactly the contract's strict body, no extra fields;
//! - the key: `crm-` plus sha256 over the durable delivery key and the
//!   content digest. A deferred retry re-renders with a fresh
//!   unsubscribe token, i.e. different bytes, so the key must follow
//!   the bytes or the platform refuses the replay as `key_conflict`;
//! - the classification of every answer into the delivery ledger's
//!   [`SmtpOutcome`] vocabulary, failing closed: only
//!   `status:"sent"` with `executed:true` and a provider message id is
//!   `Accepted`. Anything ambiguous after the request left is
//!   `Uncertain` and is never retried;
//! - `pending` (key neither granted nor approved) as the typed
//!   [`SmtpOutcome::PendingApproval`], never a refusal.
//!
//! The platform fixes `From:` to the tenant's agent address and accepts
//! no custom headers (no `List-Unsubscribe`, no `Reply-To`). See
//! `claudedocs/crm-hosted-email-transport-20261003.md` for the contract
//! gaps. Receipts record platform acceptance only, never delivery.

use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::smtp::SmtpOutcome;
use crate::error::{Error, Result};

/// Path under the hosted API origin.
pub const SEND_PATH: &str = "/v1/runtime/email/send";
/// Env the provisioning layer sets with the tenant's sending address
/// (what the platform will put in `From:`). Absent means hosted CRM
/// email is not offered: the platform exposes no way to learn it.
pub const FROM_ADDRESS_ENV: &str = "CADENCE_HOSTED_EMAIL_FROM";
/// Optional display name for that address.
pub const FROM_NAME_ENV: &str = "CADENCE_HOSTED_EMAIL_FROM_NAME";
/// The reserved connection id of the hosted sender row is derived from
/// the built-in `agenticos`/`hosted` connection; the link pins this
/// authorization revision (there is no credential to rotate).
pub const AUTH_REVISION: i64 = 1;
/// `tls_mode` marker in the sender projection: no socket, no TLS.
pub const TRANSPORT_MARK: &str = "platform";
/// Operator-visible reason a delivery waits on the owner.
pub const WAITING_APPROVAL_REASON: &str = "waiting for owner approval in AgenticOS";

const TIMEOUT: Duration = Duration::from_secs(30);
const BODY_CAP: usize = 1024 * 1024;
const TEXT_CAP: usize = 1_000_000;
const TEXT_CHARS: usize = 512;

/// The hosted sender: where the platform email door is and which
/// address the platform will send from.
#[derive(Clone)]
pub struct HostedEmail {
    base: String,
    from_address: String,
    from_name: String,
    http: ureq::Agent,
}

impl HostedEmail {
    pub fn new(base: &str, from_address: &str, from_name: &str) -> Result<Self> {
        super::smtp::validate_sender(from_address)
            .map_err(|_| Error::rejected("hosted email sender address is invalid"))?;
        let base = base.trim().trim_end_matches('/').to_string();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(Error::rejected("hosted email base URL must be http(s)"));
        }
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        Ok(Self {
            base,
            from_address: from_address.to_string(),
            from_name: from_name.to_string(),
            http: ureq::Agent::new_with_config(config),
        })
    }

    /// A shorter bound for the door call. An execution that exceeds it
    /// is `Uncertain` — the platform may still have sent the message.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        self.http = ureq::Agent::new_with_config(config);
        self
    }

    /// Hosted configuration from the container environment; `None`
    /// when the provisioning layer supplied no sending address.
    pub fn from_env(base: &str) -> Result<Option<Self>> {
        let Some(address) = std::env::var(FROM_ADDRESS_ENV)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        else {
            return Ok(None);
        };
        let name = std::env::var(FROM_NAME_ENV)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "AgenticOS".to_string());
        Self::new(base, &address, &name).map(Some)
    }

    /// The sender projection the link digest and the board show. The
    /// transport fields name the platform door, never a socket.
    pub fn projection(&self) -> super::smtp::SmtpProjection {
        let host = self
            .base
            .split("://")
            .nth(1)
            .unwrap_or_default()
            .split('/')
            .next()
            .unwrap_or_default()
            .to_string();
        super::smtp::SmtpProjection {
            host,
            port: 0,
            tls_mode: TRANSPORT_MARK.to_string(),
            username: String::new(),
            sender: self.from_address.clone(),
            sender_name: self.from_name.clone(),
        }
    }

    /// Submit one rendered message. `Err` is only for local grammar
    /// failures (the platform's own bounds, checked before any byte
    /// leaves); every wire result is an `Ok` classification.
    pub fn send_outcome(&self, message: &HostedMessage, key: &str) -> Result<SmtpOutcome> {
        let body = request_body(message)?;
        let url = format!("{}{SEND_PATH}", self.base);
        let response = self
            .http
            .post(&url)
            .header("idempotency-key", key)
            .send_json(&body);
        let mut response = match response {
            Ok(response) => response,
            Err(error) => return Ok(transport_failure(&error)),
        };
        let status = response.status().as_u16();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(BODY_CAP as u64)
            .read_to_vec();
        let Ok(bytes) = bytes else {
            return Ok(uncertain("AgenticOS answer could not be read"));
        };
        let document: Value = match serde_json::from_slice(&bytes) {
            Ok(document) => document,
            // A 4xx without a JSON envelope never reached the ledger.
            Err(_) if (400..500).contains(&status) => {
                return Ok(SmtpOutcome::Rejected {
                    code: status,
                    message: format!("AgenticOS refused the request (HTTP {status})"),
                })
            }
            Err(_) => return Ok(uncertain("AgenticOS answer was not understood")),
        };
        Ok(classify(status, &document))
    }
}

/// One rendered message: exactly the contract's content fields.
pub struct HostedMessage {
    pub to: String,
    pub subject: String,
    pub text: String,
    pub html: String,
}

fn request_body(message: &HostedMessage) -> Result<Value> {
    super::smtp::validate_sender(&message.to)
        .map_err(|_| Error::rejected("hosted email recipient is invalid"))?;
    if message.subject.len() > 998 || message.subject.contains(['\r', '\n']) {
        return Err(Error::rejected(
            "hosted email subject must be one line of at most 998 bytes",
        ));
    }
    if message.text.len() > TEXT_CAP || message.html.len() > TEXT_CAP {
        return Err(Error::rejected("hosted email body exceeds its bounds"));
    }
    Ok(json!({
        "to": [message.to],
        "subject": message.subject,
        "text": message.text,
        "html": message.html,
    }))
}

/// The platform's `emailSendContentDigest`: sha256 hex over the JSON
/// of `{to (trimmed, lowercased, sorted), subject, text, html}`. The
/// platform computes it server-side; Cadence computes the same bytes so
/// receipts and idempotency keys follow the exact content.
pub fn content_digest(message: &HostedMessage) -> String {
    let mut to = vec![message.to.trim().to_lowercase()];
    to.sort();
    // Field order is the platform's (`to, subject, text, html`); a
    // `json!` map would sort the keys and break the digest.
    let canonical = format!(
        "{{\"to\":{},\"subject\":{},\"text\":{},\"html\":{}}}",
        json!(to),
        json!(message.subject),
        json!(message.text),
        json!(message.html),
    );
    Sha256::digest(canonical.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The platform key (`^[A-Za-z0-9_-]{8,128}$`) for one submission: the
/// durable per-delivery key (or a test-send seed) bound to the exact
/// content digest.
pub fn idempotency_key(seed: &str, digest: &str) -> String {
    let seed = seed.strip_prefix("sha256:").unwrap_or(seed);
    let hash = Sha256::digest(format!("cadence-crm-hosted-email-v1\n{seed}\n{digest}").as_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("crm-{hex}")
}

fn bounded(text: &str) -> String {
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(TEXT_CHARS)
        .collect();
    clean
}

/// Door error codes are identifiers; anything else is withheld.
fn code_of(document: &Value, pointer: &str) -> Option<String> {
    document
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|c| {
            !c.is_empty()
                && c.len() <= 64
                && c.chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        })
        .map(str::to_string)
}

fn uncertain(message: &str) -> SmtpOutcome {
    SmtpOutcome::Uncertain {
        message: bounded(message),
    }
}

fn transport_failure(error: &ureq::Error) -> SmtpOutcome {
    use ureq::Timeout;
    match error {
        // The request provably never left this host.
        ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::Timeout(Timeout::Resolve | Timeout::Connect) => SmtpOutcome::NotSubmitted {
            message: "AgenticOS email door is unreachable".to_string(),
        },
        // Anything after the connect may have been executed.
        _ => uncertain("AgenticOS email request outcome is unknown"),
    }
}

/// Map one door answer. Only a self-consistent `sent` is acceptance.
fn classify(http: u16, document: &Value) -> SmtpOutcome {
    let data = document.get("data").filter(|d| d.is_object());
    let Some(data) = data else {
        return classify_error(http, document);
    };
    let status = data.get("status").and_then(Value::as_str).unwrap_or("");
    let executed = data.get("executed").and_then(Value::as_bool);
    let provider_id = data.get("providerMessageId").and_then(Value::as_str);
    let error_code = code_of(data, "/errorCode");
    let decision = data.get("decision").and_then(Value::as_str).unwrap_or("");
    let key_ok = data
        .get("key")
        .and_then(Value::as_str)
        .is_some_and(valid_key);
    if !key_ok {
        return uncertain("AgenticOS answer carried no valid key");
    }
    let authorized = matches!(decision, "approved" | "granted");
    match status {
        "sent"
            if executed == Some(true)
                && authorized
                && provider_id.is_some_and(|p| !p.is_empty()) =>
        {
            SmtpOutcome::Accepted {
                code: http,
                message: format!("accepted by AgenticOS ({decision})"),
            }
        }
        "pending" if decision == "pending" && executed == Some(false) => {
            SmtpOutcome::PendingApproval {
                message: WAITING_APPROVAL_REASON.to_string(),
            }
        }
        // The provider definitively refused: permanent.
        "failed" if authorized => SmtpOutcome::Rejected {
            code: http,
            message: format!(
                "AgenticOS provider refused ({})",
                error_code.as_deref().unwrap_or("email_provider_failed")
            ),
        },
        // The platform proved the provider was never called.
        "not_executed" if authorized => SmtpOutcome::NotSubmitted {
            message: format!(
                "AgenticOS did not send ({})",
                error_code.as_deref().unwrap_or("not_executed")
            ),
        },
        // uncertain, in_flight, or any status or shape this client does
        // not recognise: never resend.
        _ => uncertain(&format!(
            "AgenticOS delivery is unresolved ({})",
            if status.is_empty() {
                "unknown"
            } else {
                "see AgenticOS"
            }
        )),
    }
}

/// An `{ok:false,error}` envelope with no typed delivery data: the
/// request failed before the provider could be called.
fn classify_error(http: u16, document: &Value) -> SmtpOutcome {
    let code = code_of(document, "/error/code");
    let label = code.as_deref().unwrap_or("error");
    match (http, code.as_deref()) {
        // Ledger refusals are final for this content.
        (_, Some("declined" | "digest_mismatch" | "key_conflict" | "invalid_request")) => {
            SmtpOutcome::Rejected {
                code: http,
                message: format!("AgenticOS refused the send ({label})"),
            }
        }
        (403, _) => SmtpOutcome::Rejected {
            code: http,
            message: format!("AgenticOS refused the send ({label})"),
        },
        // Lease lost or capability not configured: the provider was
        // never reached, so a later attempt is safe.
        (409, Some("lease_lost")) | (503, Some("capability_unavailable")) => {
            SmtpOutcome::Deferred {
                code: http,
                message: format!("AgenticOS cannot send right now ({label})"),
            }
        }
        (400..=499, _) => SmtpOutcome::Rejected {
            code: http,
            message: format!("AgenticOS refused the send ({label})"),
        },
        _ => uncertain("AgenticOS answered with an unexpected error"),
    }
}

fn valid_key(key: &str) -> bool {
    (8..=128).contains(&key.len())
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> HostedMessage {
        HostedMessage {
            to: "B@example.com".into(),
            subject: "Hé \"q\" \u{2028} \u{1}".into(),
            text: "line1\nline2 \\ \t".into(),
            html: "<p>é \u{1F600} </p>".into(),
        }
    }

    // Vector produced by the platform's own canonicalisation
    // (JSON.stringify + SHA-256, node) for the same fields.
    #[test]
    fn cad1063_content_digest_matches_the_platform_vector() {
        assert_eq!(
            content_digest(&message()),
            "9e863ca5173aae79831be05db97d34db0512386e5436c4f103528f56371750be"
        );
    }

    #[test]
    fn cad1063_key_alphabet_and_binding() {
        let a = idempotency_key("sha256:abc", "d1");
        assert!(valid_key(&a));
        assert_eq!(a, idempotency_key("abc", "d1"));
        assert_ne!(a, idempotency_key("abc", "d2"));
        assert_ne!(a, idempotency_key("abd", "d1"));
    }

    fn data(status: &str, decision: &str, executed: bool, provider: Option<&str>) -> Value {
        json!({"ok": executed, "data": {
            "key": "crm-abcdefgh", "decision": decision, "status": status,
            "executed": executed, "providerMessageId": provider,
            "errorCode": if status == "sent" || status == "pending" { Value::Null } else { json!("some_code") },
            "repeated": false,
        }})
    }

    fn name(outcome: &SmtpOutcome) -> &'static str {
        match outcome {
            SmtpOutcome::Accepted { .. } => "accepted",
            SmtpOutcome::Deferred { .. } => "deferred",
            SmtpOutcome::Rejected { .. } => "rejected",
            SmtpOutcome::NotSubmitted { .. } => "not_submitted",
            SmtpOutcome::Uncertain { .. } => "uncertain",
            SmtpOutcome::PendingApproval { .. } => "pending",
        }
    }

    #[test]
    fn cad1063_classification_table_fails_closed() {
        let cases: Vec<(u16, Value, &str)> = vec![
            (200, data("sent", "granted", true, Some("p1")), "accepted"),
            (200, data("sent", "approved", true, Some("p1")), "accepted"),
            // acceptance needs executed, an authorising decision and a provider id
            (200, data("sent", "approved", true, None), "uncertain"),
            (
                200,
                data("sent", "approved", false, Some("p1")),
                "uncertain",
            ),
            (200, data("sent", "pending", true, Some("p1")), "uncertain"),
            (202, data("pending", "pending", false, None), "pending"),
            (202, data("pending", "approved", false, None), "uncertain"),
            (502, data("failed", "approved", false, None), "rejected"),
            (502, data("failed", "pending", false, None), "uncertain"),
            (
                409,
                data("not_executed", "approved", false, None),
                "not_submitted",
            ),
            (409, data("uncertain", "approved", false, None), "uncertain"),
            (202, data("in_flight", "approved", false, None), "uncertain"),
            (
                200,
                data("surprise", "approved", true, Some("p1")),
                "uncertain",
            ),
            (
                200,
                json!({"ok": true, "data": {"status": "sent"}}),
                "uncertain",
            ),
            (
                409,
                json!({"ok": false, "error": {"code": "declined"}}),
                "rejected",
            ),
            (
                409,
                json!({"ok": false, "error": {"code": "key_conflict"}}),
                "rejected",
            ),
            (
                409,
                json!({"ok": false, "error": {"code": "digest_mismatch"}}),
                "rejected",
            ),
            (
                400,
                json!({"ok": false, "error": {"code": "invalid_request"}}),
                "rejected",
            ),
            (
                403,
                json!({"ok": false, "error": {"code": "not_provisioned"}}),
                "rejected",
            ),
            (
                409,
                json!({"ok": false, "error": {"code": "lease_lost"}}),
                "deferred",
            ),
            (
                503,
                json!({"ok": false, "error": {"code": "capability_unavailable"}}),
                "deferred",
            ),
            (
                500,
                json!({"ok": false, "error": {"code": "boom"}}),
                "uncertain",
            ),
            (502, json!({"ok": false}), "uncertain"),
        ];
        for (http, document, want) in cases {
            assert_eq!(name(&classify(http, &document)), want, "{http} {document}");
        }
    }
}
