//! CAD-1412: the one native guard every human-room RPC runs first.
//!
//! Order, all server side: (1) the session token must name a live, scoped
//! public session (`verified_human_session`, which also runs
//! `operator_connection`); (2) the stored scope must equal this board's
//! configured issuer and company; (3) the current platform roster is read
//! from the configured canonical public host with the session cookie only;
//! (4) the roster's issuer and organization must match the scope, and the
//! verified subject must be a current member; (5) the roster is written to
//! the registry as the only directory source.
//!
//! Nothing here takes identity from a request: the token is the only input,
//! and the host, issuer and company come from server config.

use std::time::Duration;

use serde::Deserialize;

use super::Shared;
use crate::error::{Error, Result};
use crate::operator_auth::HumanScope;
use crate::store::human_threads::VerifiedHuman;

/// The whole roster read: no retry, no redirect.
const ROSTER_TIMEOUT: Duration = Duration::from_secs(5);
/// The roster response cap.
const ROSTER_CAP: u64 = 64 * 1024;

/// The exact DTO the platform roster returns. Unknown fields are refused.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Roster {
    pub issuer: String,
    pub organization_id: String,
    pub members: Vec<RosterMember>,
}

/// One current member. `role` is presentation; only membership is checked.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RosterMember {
    pub subject_id: String,
    pub name: String,
    pub role: String,
}

/// The verified, current caller: the store principal plus the live roster.
pub(crate) struct CurrentActor {
    pub actor: VerifiedHuman,
    /// Exact subject from the verified session, for excluding self from discovery.
    pub subject: String,
    pub scope: HumanScope,
    pub roster: Roster,
    /// The caller's own role in the live roster (`owner` or `member`).
    pub self_role: String,
}

/// The roster read's URL, built from configured server values only.
pub(crate) fn roster_url(host: &str) -> String {
    format!(
        "{}://{}/__platform/threads/roster",
        crate::ui::public_scheme(host),
        host
    )
}

/// Parse and check the roster body against the verified scope and subject.
/// Pure: no network, so the tenancy and membership rules are testable.
pub(crate) fn check_roster(body: &[u8], scope: &HumanScope, sub: &str) -> Result<Roster> {
    let roster: Roster = serde_json::from_slice(body)
        .map_err(|_| unavailable("the roster response was malformed"))?;
    if roster.issuer != scope.issuer || roster.organization_id != scope.organization_id {
        return Err(Error::invalid(
            "tenancy_mismatch",
            "the current roster names a different organization than this sign-in",
        ));
    }
    for m in &roster.members {
        if !matches!(m.role.as_str(), "owner" | "member") {
            return Err(unavailable("the roster response had an unknown role"));
        }
    }
    if !roster.members.iter().any(|m| m.subject_id == sub) {
        return Err(Error::invalid(
            "not_a_member",
            "you are not a current member of this organization",
        ));
    }
    Ok(roster)
}

fn unavailable(why: &str) -> Error {
    Error::invalid(
        "roster_unavailable",
        format!("the current team roster is unavailable: {why}"),
    )
}

/// Read the roster from the canonical public host with the session cookie.
/// Blocking, bounded, no redirects, no logging; a failure is
/// `roster_unavailable` and changes no session state.
fn fetch_roster(host: &str, cookie: &str) -> Result<Vec<u8>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(ROSTER_TIMEOUT))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let cookie_header = format!("{}={cookie}", crate::ui::PUBLIC_COOKIE);
    let mut resp = agent
        .get(&roster_url(host))
        .header("Cookie", &cookie_header)
        .call()
        .map_err(|_| unavailable("the platform did not answer"))?;
    if resp.status() != 200 {
        return Err(unavailable("the platform refused the roster read"));
    }
    resp.body_mut()
        .with_config()
        .limit(ROSTER_CAP)
        .read_to_vec()
        .map_err(|_| unavailable("the roster response was not readable"))
}

impl Shared {
    /// The single fresh-current-actor guard. Verifies the session first and
    /// the stored scope against config, then reads the live roster, then
    /// registers the roster in the directory. Callers get the principal and
    /// the roster without a second fetch.
    pub(crate) fn human_current_actor(
        &self,
        token: &str,
        params: &serde_json::Value,
        peer_pid: u32,
    ) -> Result<CurrentActor> {
        let (scope, sub, actor) = self.verified_human_session(token, params, peer_pid)?;
        let config = crate::board_identity::read_config(&self.state_dir)?;
        if scope.issuer != config.issuer || scope.organization_id != config.company {
            return Err(Error::invalid(
                "tenancy_mismatch",
                "this sign-in belongs to a different board configuration",
            ));
        }
        let body = fetch_roster(&config.host, token)?;
        let roster = check_roster(&body, &scope, &sub)?;
        let directory: Vec<(VerifiedHuman, String)> = roster
            .members
            .iter()
            .map(|m| {
                VerifiedHuman::with_name(
                    &scope.issuer,
                    &scope.organization_id,
                    &m.subject_id,
                    &m.name,
                )
                .map(|h| (h, m.name.clone()))
            })
            .collect::<Result<_>>()?;
        self.store
            .human_directory_upsert(&scope.organization_id, &directory)?;
        let self_role = roster
            .members
            .iter()
            .find(|m| m.subject_id == sub)
            .map(|m| m.role.clone())
            .unwrap_or_default();
        Ok(CurrentActor {
            actor,
            subject: sub,
            scope,
            roster,
            self_role,
        })
    }
}
