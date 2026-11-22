//! First-run handover facts, saved hosted logins, and bounded billing-offer reads.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::Error;

/// One bounded read of the owner-only billing offer, never a retry.
const OFFER_TIMEOUT: Duration = Duration::from_secs(8);
/// Cap on the offer body, as every other bounded JSON read here.
const OFFER_BODY_MAX: usize = 1_048_576;

/// Where the person works after this run: this machine only, or a hosted
/// workspace this machine holds a login for. Reported as a fact, never
/// inferred from a marker file.
pub const SCOPE_LOCAL: &str = "local";
pub const SCOPE_HOSTED: &str = "hosted";

/// The live instruction the handover carries. It is a live line, not a
/// replacement for the briefing: `cadence self`, `cadence agent list` and
/// the briefing's own report route stay the durable/live truth.
pub const LIVE_INSTRUCTION: &str = "\
Ask for the work you want done — an issue, a repo, a question — not another setup.
`cadence status` reports work, not billing.
Do not rerun setup, open a second login or checkout, or type a card, OTP or key here: card details stay on Stripe.
In a cadence pane report with the route your briefing names (`cadence message result <id> --token <turn_id> --text '...'`; a turn-result reporter's final text is the report).";

/// The sign-in fact: signed out, this run's verified grant, a login saved
/// earlier, a failed approval, or a store that could not be inspected.
/// `state` is one of `signed_out`, `verified`, `saved`, `failed`, `unknown`.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SignInFact {
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl SignInFact {
    pub fn signed_out(detail: impl Into<String>) -> Self {
        Self {
            state: "signed_out",
            slug: None,
            endpoint: None,
            detail: detail.into(),
            code: None,
        }
    }

    /// The issuer's own slug and endpoint from this run's browser-approved
    /// grant — the only sign-in this build calls verified.
    pub fn verified(slug: &str, endpoint: &str) -> Self {
        Self {
            state: "verified",
            slug: Some(slug.to_string()),
            endpoint: Some(endpoint.to_string()),
            detail: format!("signed in to {slug} ({endpoint}), verified by this run's grant"),
            code: None,
        }
    }

    /// A login this machine already holds. The credential files are named
    /// only; their contents, endpoint and expiry are never re-read here,
    /// so this is `saved` and never `verified`. A credential whose
    /// workspace cannot be named is still a saved login, never signed out.
    pub fn saved(saved: &SavedLogins) -> Self {
        let slugs = saved.slugs.as_slice();
        let (slug, detail) = match (slugs, saved.unnamed) {
            ([], false) => (None, "no hosted login on this machine".to_string()),
            ([one], false) => (
                Some(one.clone()),
                format!(
                    "a saved login for {one} is on this machine — not verified by this run, \
                     and it may be expired"
                ),
            ),
            ([], true) => (
                None,
                "a saved hosted login is on this machine (from `cadence auth login`), but \
                 this run cannot name its workspace (it is recorded inside the credential \
                 file, which is not read here) — not verified by this run, and it may be \
                 expired"
                    .to_string(),
            ),
            (many, _) => (
                None,
                format!(
                    "saved hosted logins for {}{} — none verified by this run and any may be \
                     expired; `cadence org list` names them",
                    many.join(", "),
                    if saved.unnamed {
                        " and one whose workspace this run cannot name"
                    } else {
                        ""
                    }
                ),
            ),
        };
        Self {
            state: if saved.is_empty() {
                "signed_out"
            } else {
                "saved"
            },
            slug,
            endpoint: None,
            detail,
            code: None,
        }
    }

    pub fn failed(error: &Error) -> Self {
        Self {
            state: "failed",
            slug: None,
            endpoint: None,
            detail: format!("the browser approval did not complete: {error}"),
            code: error.code().map(str::to_owned),
        }
    }

    pub fn unknown(detail: impl Into<String>) -> Self {
        Self {
            state: "unknown",
            slug: None,
            endpoint: None,
            detail: detail.into(),
            code: None,
        }
    }

    pub fn is_signed_out(&self) -> bool {
        self.state == "signed_out"
    }
}

/// One independently reported component of the plan fact: the server's own
/// trial, paid-subscription or Checkout state. Present only when this run
/// read the offer; absent — never a default or a guess — when it did not.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct PlanState {
    pub state: &'static str,
    pub detail: String,
}

impl PlanState {
    pub(super) fn new(state: &'static str, detail: impl Into<String>) -> Self {
        Self {
            state,
            detail: detail.into(),
        }
    }
}

/// The plan fact: what the server's offer says about trial, paid
/// subscription and Checkout. `state` is one of `none`, `skipped`,
/// `trial_active`, `trial_eligible`, `trial_used`, `trial_expired`,
/// `trial_failed`, `trial_unknown`, `checkout_pending`, `checkout_open`,
/// `checkout_completed`, `checkout_settled`, `checkout_unknown`, `past_due`,
/// `subscription_unknown`, `unknown` (not read this run) or `unavailable`
/// (the read was refused). Nothing here is computed by this client.
///
/// `trial`, `subscription` and `checkout` are the same read reported as three
/// independent server facts, so an active trial is never hidden by an open
/// Checkout and an active subscription never hides a trial. They are absent
/// when the offer was not read, and `detail` is the combined human line. A
/// component's own `state` is its server-derived name (`trial_*`,
/// `subscription_active`/`subscription_past_due`/`subscription_inactive`/
/// `subscription_none`/`subscription_unknown`, or `checkout_*`).
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct PlanFact {
    pub state: &'static str,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkout_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trial: Option<Box<PlanState>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription: Option<Box<PlanState>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkout: Option<Box<PlanState>>,
}

impl PlanFact {
    fn new(state: &'static str, detail: impl Into<String>) -> Self {
        Self {
            state,
            detail: detail.into(),
            checkout_url: None,
            code: None,
            trial: None,
            subscription: None,
            checkout: None,
        }
    }

    /// No hosted workspace on this machine: nothing is billed.
    pub fn none() -> Self {
        Self::new(
            "none",
            "No hosted workspace — nothing is billed on this machine.",
        )
    }

    /// Not read by this run. Never a claim that there is no plan.
    pub fn unknown(detail: impl Into<String>) -> Self {
        Self::new("unknown", detail)
    }

    /// The server refused or could not answer the read.
    pub fn unavailable(code: &str, detail: impl Into<String>) -> Self {
        Self {
            state: "unavailable",
            detail: detail.into(),
            checkout_url: None,
            code: Some(code.to_string()),
            trial: None,
            subscription: None,
            checkout: None,
        }
    }
}

/// The four facts and the one next command the first run hands over.
#[derive(Serialize, Debug, Clone)]
pub struct Handover {
    pub scope: &'static str,
    pub sign_in: SignInFact,
    pub plan: PlanFact,
    /// The board this run started or found, on the resolved port.
    pub board_url: String,
    /// Whether that board answered this run's `ui` check.
    pub board_running: bool,
    /// The hosted workspace's board, when this run verified one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosted_board_url: Option<String>,
    /// Exactly one command to run next.
    pub next_command: String,
    pub instruction: String,
}

/// A workspace slug as the issuer mints it (`remote_enrollment::slug`):
/// lowercase alphanumerics and `-`, bounded. A credential file name that
/// is not one is ignored rather than echoed into a printed command.
fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 63
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// What the credential store holds, by file name only — never contents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedLogins {
    /// Workspace slugs named by `cli-<slug>.json` credential files.
    pub slugs: Vec<String>,
    /// A credential exists whose workspace this inspection cannot name:
    /// `cadence auth login` stores `credential.json`, whose workspace is
    /// inside the file. It is a saved login all the same.
    pub unnamed: bool,
}

impl SavedLogins {
    pub fn is_empty(&self) -> bool {
        self.slugs.is_empty() && !self.unnamed
    }
}

/// The hosted logins this machine holds: the credential file names only,
/// never their contents. `None` means the store could not be inspected —
/// never "signed out".
pub fn saved_hosted_logins() -> Option<SavedLogins> {
    saved_logins_in(&crate::remote_auth::auth_dir(None).ok()?)
}

pub(super) fn saved_logins_in(dir: &Path) -> Option<SavedLogins> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(SavedLogins::default()),
        Err(_) => return None,
    };
    let mut saved = SavedLogins::default();
    for entry in entries {
        // A listing that fails part-way cannot say the store is empty.
        let entry = entry.ok()?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == "credential.json" {
            saved.unnamed = true;
        } else if let Some(slug) = name
            .strip_prefix("cli-")
            .and_then(|rest| rest.strip_suffix(".json"))
        {
            if valid_slug(slug) {
                saved.slugs.push(slug.to_string());
            }
        }
    }
    saved.slugs.sort();
    Some(saved)
}

/// What the billing read produced: the server's offer, or the exact reason
/// there is none. `code` is the server's own stable code where it answered,
/// so "this grant was never consented for billing" is never confused with
/// "the credential is dead".
#[derive(Debug, Clone)]
pub enum OfferRead {
    Offer(serde_json::Value),
    Unavailable { code: String, detail: String },
}

impl OfferRead {
    fn unavailable(code: &str, detail: impl Into<String>) -> Self {
        Self::Unavailable {
            code: code.to_string(),
            detail: detail.into(),
        }
    }
}

/// `GET <endpoint>/__platform/hosted-cadence/<org>/billing/offer` with the
/// saved human login, on the board origin that is the credential's exact
/// audience. Read only: no Checkout is started, cancelled or replayed, no
/// redirect is followed and nothing is written. The offer is the server's
/// own; every refusal is reported with its code.
pub fn billing_offer(target: &crate::remote_cli::RemoteTarget) -> OfferRead {
    // Owner-class money authority is never spent by an agent's process.
    if std::env::var_os("CADENCE_ALIAS").is_some()
        || std::env::var_os("CADENCE_GRANT_TOKEN").is_some()
    {
        return OfferRead::unavailable(
            "human_session_required",
            "a managed agent cannot spend the saved human login for a billing read",
        );
    }
    let endpoint = target.endpoint.trim_end_matches('/');
    if !endpoint.starts_with("https://") || target.org_id.is_empty() {
        return OfferRead::unavailable(
            "invalid_target",
            "the saved workspace has no https board origin to read billing from",
        );
    }
    let deadline = Instant::now() + OFFER_TIMEOUT;
    let dir = match crate::remote_auth::auth_dir(None) {
        Ok(dir) => dir,
        Err(e) => {
            return OfferRead::unavailable(
                "credential",
                format!("the hosted credential directory is unavailable: {e}"),
            )
        }
    };
    let material = match crate::remote_cli::cli_material_until(&dir, target, deadline) {
        Ok(material) => material,
        Err(e) => return OfferRead::unavailable("credential", e.to_string()),
    };
    if material.subject_id.is_none() || material.issuer.is_none() {
        return OfferRead::unavailable(
            "credential",
            "the saved login carries no member and issuer pin",
        );
    }
    let agent = crate::issuer_transport::agent(
        ureq::Agent::config_builder()
            .timeout_global(Some(deadline.saturating_duration_since(Instant::now())))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None),
    );
    let url = format!(
        "{endpoint}/__platform/hosted-cadence/{}/billing/offer",
        target.org_id
    );
    let authorization = format!("Bearer {}", material.access_token);
    let response = match agent
        .get(url.as_str())
        .header("Authorization", authorization.as_str())
        .call()
    {
        Ok(response) => response,
        Err(e) => {
            return OfferRead::unavailable(
                "transport",
                format!("the billing offer could not be reached: {e}"),
            )
        }
    };
    let status = response.status().as_u16();
    if (300..400).contains(&status) {
        return OfferRead::unavailable(
            "redirect",
            "the billing offer answered with a redirect; none was followed",
        );
    }
    let mut bytes = Vec::new();
    if response
        .into_body()
        .as_reader()
        .take((OFFER_BODY_MAX + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return OfferRead::unavailable("transport", "the billing offer body could not be read");
    }
    if bytes.len() > OFFER_BODY_MAX {
        return OfferRead::unavailable("transport", "the billing offer response exceeded 1 MiB");
    }
    let body: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(body) => body,
        Err(_) => {
            return OfferRead::unavailable("invalid", "the billing offer response was not JSON")
        }
    };
    if status == 200 {
        return if body["ok"] == true {
            OfferRead::Offer(body["data"].clone())
        } else {
            OfferRead::unavailable(
                "invalid",
                "the billing offer response was not an ok envelope",
            )
        };
    }
    let code = body["code"].as_str().unwrap_or("unknown").to_string();
    let detail = match code.as_str() {
        "capability_not_consented" => "this grant does not carry billing.read — the optional scope was declined at consent, the approver's role could not grant it, or the grant predates it. That is not a reason to sign in again; the plan and any Checkout are shown in the workspace dashboard",
        "owner_required" => "only the workspace owner may read billing",
        "human_session_required" => {
            "billing requires the owner's human login, not an agent credential"
        }
        "unauthorized" => {
            "the saved credential was refused for this workspace (dead, revoked, or bound to another audience)"
        }
        "capability_unavailable" => "the server cannot read billing right now",
        _ => "the billing read was refused",
    };
    OfferRead::unavailable(&code, format!("{detail} ({code})"))
}
