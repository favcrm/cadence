//! CAD-1126: hosted tenant SMTP through the `smtp.internal` pass-through.
//!
//! A hosted container is offline (`enableInternet = false`), so the
//! direct SMTP client in `smtp.rs` cannot reach a mail server. The
//! AgenticOS Worker answers HTTP at `smtp.internal` and opens the SMTP
//! connection (465 implicit TLS or 587 STARTTLS) on the daemon's behalf.
//! The Worker stores nothing; the daemon keeps custody of the tenant's
//! credential and hands it over only inside the request body of one
//! call, at send time.
//!
//! Wire shape (contract v2): `POST /v1/send` with
//! `{server:{host,port,tls_mode,username,password}, envelope:{from,to},
//! message_b64}` and `POST /v1/verify` with `{server}`. Answers are
//! `{ok:true,data}` or `{ok:false,error:{code,step,message}}`.
//!
//! What this module owns: the request bodies, and the classification of
//! every answer into the delivery ledger's [`SmtpOutcome`] vocabulary,
//! failing closed. The message bytes are the exact RFC 5322 message
//! `smtp::prepare_message` builds for the direct path. Nothing here logs
//! or returns the password; every text that leaves is screened.

use std::time::Duration;

use serde_json::{json, Value};

use super::smtp::{base64_encode, screened, SmtpOutcome};
use crate::error::{Error, Result};

/// Where the pass-through lives inside a hosted container.
pub const DEFAULT_BASE: &str = "http://smtp.internal";
/// Optional override of the base, honoured only on a daemon that holds a
/// real hosted lease (the isolated test rig points it at a fake).
pub const BASE_ENV: &str = "CADENCE_SMTP_INTERNAL_URL";

/// Worker budget is 60 s per send; leave the daemon a little more.
const SEND_TIMEOUT: Duration = Duration::from_secs(75);
const BODY_CAP: u64 = 64 * 1024;
const TEXT_CHARS: usize = 300;
/// The Worker's hard cap on a decoded message.
const MESSAGE_CAP: usize = 10 * 1024 * 1024;

/// The pass-through client. Holds no credential.
#[derive(Clone)]
pub struct SmtpInternal {
    base: String,
    http: ureq::Agent,
}

/// The server block of both calls, borrowed from custody for one call.
pub struct Server<'a> {
    pub host: &'a str,
    pub port: u16,
    pub tls_mode: &'a str,
    pub username: &'a str,
    pub secret: &'a [u8],
}

impl Server<'_> {
    fn to_json(&self) -> Result<Value> {
        let password = std::str::from_utf8(self.secret)
            .map_err(|_| Error::rejected("SMTP secret exceeds its supported shape or bounds"))?;
        Ok(json!({
            "host": self.host,
            "port": self.port,
            "tls_mode": self.tls_mode,
            "username": self.username,
            "password": password,
        }))
    }
}

impl SmtpInternal {
    pub fn new(base: &str) -> Result<Self> {
        let base = base.trim().trim_end_matches('/').to_string();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(Error::rejected("smtp.internal base URL must be http(s)"));
        }
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(SEND_TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        Ok(Self {
            base,
            http: ureq::Agent::new_with_config(config),
        })
    }

    /// The configured base: [`BASE_ENV`] when set, else [`DEFAULT_BASE`].
    /// Honoured only on the lease-gated path (a daemon holding a real
    /// hosted lease); the CAD-1158 image-admitted path always uses the
    /// fixed [`DEFAULT_BASE`] so caller env can never redirect it.
    pub fn from_env() -> Result<Self> {
        let base = std::env::var(BASE_ENV)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        Self::new(base.as_deref().unwrap_or(DEFAULT_BASE))
    }

    /// CAD-1158: build the bridge-owned relay from the dedicated
    /// image-owned SMTP proof. The base is always the fixed
    /// [`DEFAULT_BASE`]; no environment, app or RPC value redirects the
    /// transport that carries the custodied credential.
    pub(crate) fn from_admission(
        _: &crate::platform::deployments::HostedSmtpAdmission,
    ) -> Result<Self> {
        Self::new(DEFAULT_BASE)
    }

    /// The relay origin under test or in production (`http://smtp.internal`
    /// for image-admitted composition). Secret-free; safe to assert on.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Connect, TLS, EHLO, AUTH, QUIT — no send. `Ok(())` is a verified
    /// login; `Err` carries a plain-language, secret-screened reason.
    pub fn verify(&self, server: &Server<'_>) -> Result<()> {
        let body = json!({ "server": server.to_json()? });
        match self.call("/v1/verify", &body) {
            Reply::Data(data) if data.get("verified").and_then(Value::as_bool) == Some(true) => {
                Ok(())
            }
            Reply::Data(_) => Err(Error::invalid(
                "smtp_failed",
                "The mail server could not be verified.",
            )),
            Reply::Failed {
                code,
                step,
                message,
            } => Err(Error::invalid(error_code(&code, &step), message)),
            Reply::Unknown(message) => Err(Error::invalid("smtp_unknown", message)),
        }
    }

    /// Submit one prepared RFC 5322 message. `Err` is only for local
    /// grammar failures; every wire result is an `Ok` classification.
    pub fn send_outcome(
        &self,
        server: &Server<'_>,
        from: &str,
        to: &str,
        message: &[u8],
    ) -> Result<SmtpOutcome> {
        if message.len() > MESSAGE_CAP {
            return Err(Error::rejected("SMTP body exceeds its supported bounds"));
        }
        let body = json!({
            "server": server.to_json()?,
            "envelope": { "from": from, "to": [to] },
            "message_b64": base64_encode(message),
        });
        Ok(match self.call("/v1/send", &body) {
            Reply::Data(data) => classify_sent(&data, to, server.secret),
            Reply::Failed {
                code,
                step,
                message,
            } => classify_failure(&code, &step, message),
            Reply::Unknown(message) => SmtpOutcome::Uncertain { message },
        })
    }

    fn call(&self, path: &str, body: &Value) -> Reply {
        let url = format!("{}{path}", self.base);
        let response = self.http.post(&url).send_json(body);
        let mut response = match response {
            Ok(response) => response,
            Err(error) => return transport_failure(&error),
        };
        let status = response.status().as_u16();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(BODY_CAP)
            .read_to_vec();
        let Ok(bytes) = bytes else {
            return Reply::Unknown("The mail relay's answer could not be read.".into());
        };
        let Ok(document) = serde_json::from_slice::<Value>(&bytes) else {
            return Reply::Unknown(format!(
                "The mail relay answered with something unexpected (HTTP {status})."
            ));
        };
        if document.get("ok").and_then(Value::as_bool) == Some(true) {
            if let Some(data) = document.get("data").filter(|d| d.is_object()) {
                return Reply::Data(data.clone());
            }
        }
        if document.get("ok").and_then(Value::as_bool) == Some(false) {
            let error = document.get("error");
            let field = |name: &str| {
                error
                    .and_then(|e| e.get(name))
                    .and_then(Value::as_str)
                    .filter(|v| {
                        !v.is_empty()
                            && v.len() <= 32
                            && v.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                    })
                    .unwrap_or("")
                    .to_string()
            };
            let (code, step) = (field("code"), field("step"));
            // The relay's own message is advisory text; the words the
            // operator reads come from the typed code and step alone.
            return Reply::Failed {
                message: plain_message(&code, &step).to_string(),
                code,
                step,
            };
        }
        Reply::Unknown(format!(
            "The mail relay answered with something unexpected (HTTP {status})."
        ))
    }
}

enum Reply {
    Data(Value),
    Failed {
        code: String,
        step: String,
        message: String,
    },
    /// The answer was unreadable or malformed after the request left.
    Unknown(String),
}

fn transport_failure(error: &ureq::Error) -> Reply {
    use ureq::Timeout;
    match error {
        ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::Timeout(Timeout::Resolve | Timeout::Connect) => Reply::Failed {
            code: "unreachable".into(),
            step: "connect".into(),
            message: "The mail relay is not reachable right now. Try again in a moment.".into(),
        },
        _ => Reply::Unknown("The mail relay's outcome is unknown.".into()),
    }
}

/// The stable wire code the board maps to its own safe wording. Anything
/// the relay sends outside the contract's list collapses to one code.
fn error_code(code: &str, step: &str) -> &'static str {
    match code {
        "auth" => "smtp_auth",
        "tls" => "smtp_tls",
        "connect" => "smtp_connect",
        "dns" => "smtp_dns",
        "private_host" => "smtp_private_host",
        "port_not_allowed" => "smtp_port",
        "timeout" => "smtp_timeout",
        // Only the per-company cap at validate is a rate limit; the same
        // code at ehlo/mail/rcpt is the mail server refusing.
        "rejected" if step == "validate" => "smtp_rate",
        "rejected" => "smtp_refused",
        "not_provisioned" => "smtp_not_provisioned",
        "invalid" => "smtp_invalid",
        "unreachable" => "smtp_unreachable",
        _ => "smtp_failed",
    }
}

/// Plain words for a `{code, step}` pair. The text never carries the
/// server's banner, the address list or any secret.
pub fn plain_message(code: &str, step: &str) -> &'static str {
    match (code, step) {
        ("auth", _) => "Couldn't sign in: check the app password (and the username).",
        ("tls", _) => "Couldn't make a secure connection to the mail server. Check the security setting and port.",
        ("connect", _) => "Couldn't reach the mail server. Check the server name and port.",
        ("dns", _) => "That mail server name doesn't exist. Check the spelling.",
        ("private_host", _) => "That mail server address isn't allowed. Use your provider's public server name.",
        ("port_not_allowed", _) => "Only port 465 (SSL/TLS) and port 587 (STARTTLS) are allowed.",
        ("timeout", _) => "The mail server took too long to answer. Try again.",
        ("recipient", _) => "The mail server refused this recipient address.",
        ("data", _) => "The mail server refused the message.",
        ("rejected", "validate") => {
            "Too many emails were sent in a short time. Wait a little and try again."
        }
        ("rejected", _) => {
            "Your mail server refused the message: check the From address is allowed on this account."
        }
        ("not_provisioned", _) => "Email sending isn't set up for this workspace yet.",
        ("invalid", _) => "The email settings are not valid. Check each field.",
        (_, "auth") => "Couldn't sign in: check the app password (and the username).",
        (_, "tls") => "Couldn't make a secure connection to the mail server.",
        (_, "connect") => "Couldn't reach the mail server.",
        _ => "The mail server could not be used.",
    }
}

/// The board's fixed wording for one of this module's wire codes
/// (`smtp_auth`, ...). `None` for any other code. The board never relays
/// the daemon's message, only this table.
pub fn wire_message(wire_code: &str) -> Option<&'static str> {
    let code = match wire_code {
        "smtp_auth" => "auth",
        "smtp_tls" => "tls",
        "smtp_connect" => "connect",
        "smtp_dns" => "dns",
        "smtp_private_host" => "private_host",
        "smtp_port" => "port_not_allowed",
        "smtp_timeout" => "timeout",
        "smtp_rate" => return Some(plain_message("rejected", "validate")),
        "smtp_refused" => return Some(plain_message("rejected", "mail")),
        "smtp_not_provisioned" => "not_provisioned",
        "smtp_invalid" => "invalid",
        "smtp_unreachable" => "unreachable",
        "smtp_failed" | "smtp_unknown" => "",
        _ => return None,
    };
    Some(plain_message(code, ""))
}

fn bounded(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(TEXT_CHARS)
        .collect()
}

/// A `{ok:true}` send answer. Only an answer that names the recipient
/// as accepted, with none rejected, is acceptance; anything else after
/// the message left is never retried.
fn classify_sent(data: &Value, to: &str, secret: &[u8]) -> SmtpOutcome {
    let list = |name: &str| -> Option<Vec<String>> {
        data.get(name)?
            .as_array()?
            .iter()
            .map(|v| v.as_str().map(|s| s.trim().to_ascii_lowercase()))
            .collect()
    };
    let (Some(accepted), Some(rejected)) = (list("accepted"), list("rejected")) else {
        return SmtpOutcome::Uncertain {
            message: "The mail relay's answer was not understood.".into(),
        };
    };
    let wanted = to.trim().to_ascii_lowercase();
    if rejected.is_empty() && accepted == [wanted] {
        let reply = data
            .get("server_reply")
            .and_then(Value::as_str)
            .map(bounded)
            .unwrap_or_default();
        return SmtpOutcome::Accepted {
            code: 250,
            message: screened(&reply, secret),
        };
    }
    if !rejected.is_empty() {
        return SmtpOutcome::Rejected {
            code: 550,
            message: plain_message("recipient", "rcpt").to_string(),
        };
    }
    SmtpOutcome::Uncertain {
        message: "The mail relay's answer was not understood.".into(),
    }
}

/// A `{ok:false}` send answer. Failures before DATA left provably
/// sent nothing and are safe to retry; a failure inside DATA, or a
/// timeout there, is unresolved and never retried.
fn classify_failure(code: &str, step: &str, message: String) -> SmtpOutcome {
    match (code, step) {
        // Whatever the code, a failure at step `data` came after the
        // body may have been written: acceptance is unknowable.
        (_, "data") => SmtpOutcome::Uncertain { message },
        ("recipient", _) => SmtpOutcome::Rejected { code: 550, message },
        ("data", _) => SmtpOutcome::Rejected { code: 554, message },
        // The per-company cap is the only retry-later `rejected`.
        ("rejected", "validate") => SmtpOutcome::Deferred { code: 429, message },
        // At ehlo/mail/rcpt the mail server refused: not a rate limit.
        ("rejected", _) => SmtpOutcome::Rejected { code: 550, message },
        (
            "invalid" | "port_not_allowed" | "private_host" | "dns" | "connect" | "tls" | "auth"
            | "timeout" | "not_provisioned" | "unreachable",
            _,
        ) => SmtpOutcome::NotSubmitted { message },
        _ => SmtpOutcome::Uncertain { message },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_words_never_echo_the_relay_text() {
        assert!(plain_message("auth", "auth").starts_with("Couldn't sign in"));
        assert!(plain_message("zzz", "auth").starts_with("Couldn't sign in"));
        assert_eq!(plain_message("", ""), "The mail server could not be used.");
    }

    #[test]
    fn any_failure_at_step_data_is_never_retried() {
        for code in [
            "connect",
            "rejected",
            "tls",
            "auth",
            "invalid",
            "recipient",
            "data",
            "timeout",
            "",
            "zzz",
        ] {
            assert!(
                matches!(
                    classify_failure(code, "data", String::new()),
                    SmtpOutcome::Uncertain { .. }
                ),
                "code {code} at step data must be Uncertain"
            );
        }
    }

    #[test]
    fn rejected_is_a_rate_limit_only_at_validate() {
        assert!(matches!(
            classify_failure("rejected", "validate", String::new()),
            SmtpOutcome::Deferred { .. }
        ));
        for step in ["ehlo", "mail", "rcpt", ""] {
            assert!(
                matches!(
                    classify_failure("rejected", step, String::new()),
                    SmtpOutcome::Rejected { code: 550, .. }
                ),
                "rejected at {step} is a server refusal"
            );
        }
        assert_eq!(error_code("rejected", "validate"), "smtp_rate");
        assert_eq!(error_code("rejected", "mail"), "smtp_refused");
        assert!(wire_message("smtp_refused")
            .unwrap()
            .contains("refused the message"));
        assert!(wire_message("smtp_rate").unwrap().contains("Too many"));
    }

    #[test]
    fn failures_inside_data_are_never_retried() {
        assert!(matches!(
            classify_failure("timeout", "data", String::new()),
            SmtpOutcome::Uncertain { .. }
        ));
        assert!(matches!(
            classify_failure("auth", "auth", String::new()),
            SmtpOutcome::NotSubmitted { .. }
        ));
        assert!(matches!(
            classify_failure("rejected", "validate", String::new()),
            SmtpOutcome::Deferred { .. }
        ));
    }

    #[test]
    fn acceptance_needs_the_recipient_named_and_none_rejected() {
        let ok = json!({"accepted":["A@x.com"],"rejected":[],"server_reply":"250 OK"});
        assert!(matches!(
            classify_sent(&ok, "a@x.com", b"s"),
            SmtpOutcome::Accepted { .. }
        ));
        let bad = json!({"accepted":[],"rejected":["a@x.com"]});
        assert!(matches!(
            classify_sent(&bad, "a@x.com", b"s"),
            SmtpOutcome::Rejected { .. }
        ));
        assert!(matches!(
            classify_sent(&json!({}), "a@x.com", b"s"),
            SmtpOutcome::Uncertain { .. }
        ));
    }
}
