//! The daemon's operator-auth verbs (CAD-313, ADR 0004 phase 1). The
//! daemon is the one authority: the CLI mints login links here, and the
//! board exchanges and checks sessions here.
//!
//! - `operator_link_mint {secret, origin}` — `cadence ui login`. The
//!   caller must derive no agent identity, pass positive operator proof
//!   ([`Shared::proven_operator`]) AND present the operator secret
//!   ([`crate::operator_auth::read_secret`], strict modes). Answers a
//!   single-use nonce; the event `operator_link_minted` names the pid
//!   and origin, never the nonce.
//! - `operator_session_open {nonce, origin, user_agent?}` — the board's
//!   `POST /api/session`. The nonce is the credential. A refusal records
//!   `operator_link_rejected` with its reason. The verb is open to any
//!   socket caller, so it runs the board's agent check itself: a
//!   connection that derives an agent (a pane or enrolled managed
//!   endpoint on its ancestry — an agent that skipped the board, or a
//!   board an agent started) spends the nonce and gets no session.
//! - `operator_session_check {token, key, origin}` — the board, on every
//!   private read and write, `/api/meta`, and live streams: the cookie's
//!   token AND the tab's `X-Cadence-Session` key, both required.
//! - `operator_session_logout {token}` — the board's
//!   `POST /api/session/logout`: possession of the token ends it.
//! - `operator_session_stolen {token, agent}` — the board saw a session
//!   presented by a process tied to an agent: the session ends and
//!   `operator_session_from_agent` is recorded.
//! - `operator_sessions {secret, revoke?, revoke_all?}` and
//!   `operator_secret_rotate {secret}` — `cadence ui sessions` and
//!   `ui login --rotate`, under the same gate as the mint.
//!
//! No verb ever answers a stored credential, and none reads who the
//! caller is from a request field.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::{
    optional_str, public_membership, reject_operator_fields, required_str, Shared, DAEMON_ALIAS,
};
use crate::error::{Error, Result};
use crate::operator_auth::{self as auth, Origin};

fn authenticated_principal_hash(issuer: &str, workspace: &str, subject: &str) -> String {
    auth::digest(&format!(
        "cadence:authenticated-principal:v1\0{issuer}\0{workspace}\0{subject}"
    ))
}

fn origin_param(params: &Value) -> Result<Origin> {
    let raw = required_str(params, "origin")?;
    // `public` (CAD-526) is deliberately excluded: a public session is
    // born only of a verified platform assertion at board_session_open —
    // no login link may ever mint one.
    match raw {
        "loopback" => Ok(Origin::Loopback),
        "tailnet" => Ok(Origin::Tailnet),
        _ => Err(Error::invalid(
            "invalid_request",
            format!("origin must be 'loopback' or 'tailnet', not '{raw}'"),
        )),
    }
}

/// CAD-1374: the fixed fields of one audited board-session mutation. The
/// actor and the verified human come from the caller (a signed session, the
/// daemon's own peer identity, or the operator secret), never from the rows.
struct Audit {
    action: &'static str,
    actor_kind: &'static str,
    human: Value,
    company: Option<String>,
    resource: String,
    reason: &'static str,
    agent: Option<String>,
    request: String,
}

/// One prepared frame of a batch mutation: its audit record and the safe rows it names.
type Frames = Vec<(Audit, Vec<auth::AuditTarget>)>;

const UPLOAD_ACCOUNT_SETUP_COMPENSATION_REASON: &str = "upload_account_setup_compensation";

/// The verified human a row names: a signed-session snapshot, labelled as such.
fn human_json(t: &auth::AuditTarget) -> Value {
    json!({
        "sub": t.sub,
        "company": t.company,
        "session": t.session,
        "role": t.role,
        "role_basis": t.role.as_ref().map(|_| "signed_session_snapshot"),
    })
}

fn target_json(t: &auth::AuditTarget) -> Value {
    json!({
        "session": t.session,
        "origin": t.origin.as_str(),
        "sub": t.sub,
        "role": t.role,
        "role_basis": t.role.as_ref().map(|_| "signed_session_snapshot"),
        "company": t.company,
    })
}

/// Batch rows grouped by their verified company (`None` = local rows, never
/// backfilled). An empty batch is one empty local group, so a mutation that
/// voids only unexchanged links still gets its prepared/terminal pair.
fn by_company(targets: &[auth::AuditTarget]) -> Vec<(Option<String>, Vec<auth::AuditTarget>)> {
    let mut groups: BTreeMap<Option<String>, Vec<auth::AuditTarget>> = BTreeMap::new();
    for t in targets {
        groups.entry(t.company.clone()).or_default().push(t.clone());
    }
    if groups.is_empty() {
        groups.insert(None, Vec::new());
    }
    groups.into_iter().collect()
}

fn resource_of(targets: &[auth::AuditTarget]) -> String {
    match targets {
        [one] => format!("session:{}", one.session),
        _ => "board:sessions".to_string(),
    }
}

fn open_audit(company: &str, request: &str, row: &auth::AuditTarget) -> Audit {
    Audit {
        action: "board_session_opened",
        actor_kind: "human",
        human: human_json(row),
        company: Some(company.to_string()),
        resource: format!("session:{}", row.session),
        reason: "platform_sign_in",
        agent: None,
        request: request.to_string(),
    }
}

fn logout_audit(
    agent: Option<&str>,
    company: Option<String>,
    request: &str,
    targets: &[auth::AuditTarget],
) -> Audit {
    // A registered agent peer is never human because a cookie came with it.
    let (actor_kind, human) = match (agent, targets.first()) {
        (Some(_), _) => ("agent", Value::Null),
        (None, Some(t)) if t.sub.is_some() => ("human", human_json(t)),
        _ => ("unknown", Value::Null),
    };
    Audit {
        action: "board_session_logged_out",
        actor_kind,
        human,
        company,
        resource: resource_of(targets),
        reason: "session_logout",
        agent: agent.map(str::to_string),
        request: request.to_string(),
    }
}

fn stolen_audit(
    agent: Option<&str>,
    company: Option<String>,
    request: &str,
    targets: &[auth::AuditTarget],
) -> Audit {
    Audit {
        action: "board_session_revoked",
        actor_kind: if agent.is_some() { "agent" } else { "unknown" },
        human: Value::Null,
        company,
        resource: resource_of(targets),
        reason: "presented_by_agent",
        agent: agent.map(str::to_string),
        request: request.to_string(),
    }
}

/// A server-side compensation for a public session whose upload-account
/// setup failed after the prepared open was durably applied.
fn upload_account_setup_compensation_audit(
    company: Option<String>,
    request: &str,
    targets: &[auth::AuditTarget],
) -> Audit {
    Audit {
        action: "board_session_revoked",
        actor_kind: "system",
        human: Value::Null,
        company,
        resource: resource_of(targets),
        reason: UPLOAD_ACCOUNT_SETUP_COMPENSATION_REASON,
        agent: None,
        request: request.to_string(),
    }
}

/// An operator-secret action on the local CLI path: never a human.
fn operator_audit(
    reason: &'static str,
    company: Option<String>,
    request: &str,
    targets: &[auth::AuditTarget],
) -> Audit {
    Audit {
        action: "board_session_revoked",
        actor_kind: "operator",
        human: Value::Null,
        company,
        resource: resource_of(targets),
        reason,
        agent: None,
        request: request.to_string(),
    }
}

impl Shared {
    pub(super) fn operator_now(&self) -> i64 {
        (self.operator_clock)()
    }

    pub(super) fn operator_auth(&self) -> std::sync::MutexGuard<'_, auth::Auth> {
        self.operator_auth.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The gate for the operator-auth verbs a CLI runs: no agent identity
    /// on the connection, positive operator proof, then possession of the
    /// secret — compared in constant time against the file, read under
    /// strict modes at every call (a rotated or loosened file takes effect
    /// at once).
    fn operator_with_secret(&self, verb: &str, params: &Value, peer_pid: u32) -> Result<()> {
        reject_operator_fields(verb, params)?;
        if let Some(who) = self.slot_identity(peer_pid)? {
            return Err(Error::rejected(format!(
                "{verb} is an operator action — this connection is agent '{}'; \
                 run it from the operator's own shell, outside every pane and managed endpoint",
                who.lane()
            )));
        }
        self.proven_operator(verb, peer_pid)?;
        let presented = params
            .get("secret")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::invalid(
                    "operator_secret",
                    format!("{verb} needs the operator secret — `cadence ui login` reads it"),
                )
            })?;
        let on_disk = auth::read_secret(&self.state_dir)?;
        if !auth::same_credential(presented, &on_disk) {
            return Err(Error::invalid(
                "operator_secret",
                format!("{verb} refused: the presented operator secret does not match"),
            ));
        }
        Ok(())
    }

    pub(super) fn rpc_operator_link_mint(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_with_secret("ui login", params, peer_pid)?;
        let origin = origin_param(params)?;
        let now = self.operator_now();
        // This shared local account denotes the authority established by
        // the native secret-gated login, not a guessed human or process ID.
        let principal = auth::digest("cadence:local-administrative-authority:v1");
        let account_id = self.store.chat_upload_account(&principal, "local-admin")?;
        let nonce = self
            .operator_auth()
            .mint_for_account(origin, now, account_id)?;
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "operator_link_minted",
            json!({"pid": peer_pid, "origin": origin.as_str()}),
        );
        Ok(json!({
            "nonce": nonce,
            "origin": origin.as_str(),
            "expires_in": auth::LINK_TTL_SECS,
        }))
    }

    pub(super) fn rpc_operator_session_open(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let nonce = required_str(params, "nonce")?;
        let origin = origin_param(params)?;
        let user_agent = optional_str(params, "user_agent").unwrap_or_default();
        let agent = self
            .slot_identity(peer_pid)?
            .map(|who| who.lane().to_string());
        let now = self.operator_now();
        let opened = self.operator_auth().open(nonce, origin, user_agent, now);
        match opened {
            Ok(created) => {
                let opened = created?;
                if let Some(agent) = agent {
                    self.operator_auth().revoke_token(&opened.token)?;
                    let _ = self.store.event_public(
                        DAEMON_ALIAS,
                        "operator_session_from_agent",
                        json!({"agent": agent, "revoked": true, "alert": true}),
                    );
                    return Err(Error::invalid(
                        "session_from_agent",
                        format!(
                            "session open refused: this connection is agent '{agent}' — \
                             the link is spent"
                        ),
                    ));
                }
                let _ = self.store.event_public(
                    DAEMON_ALIAS,
                    "operator_session_opened",
                    json!({"session": opened.session.id, "origin": origin.as_str()}),
                );
                Ok(json!({"token": opened.token, "key": opened.key, "session": opened.session}))
            }
            Err(why) => {
                // An already-used link means someone else may have
                // opened it first: loud, for `agent events` and review.
                let _ = self.store.event_public(
                    DAEMON_ALIAS,
                    "operator_link_rejected",
                    json!({"reason": why.as_str(), "origin": origin.as_str(),
                           "alert": why == auth::LinkRefusal::AlreadyUsed}),
                );
                Err(Error::invalid(
                    "login_link",
                    format!("{} ({})", why.explain(), why.as_str()),
                ))
            }
        }
    }

    pub(super) fn rpc_operator_session_check(&self, params: &Value) -> Result<Value> {
        let token = required_str(params, "token")?;
        let key = optional_str(params, "key").unwrap_or_default();
        let origin = origin_param(params)?;
        let now = self.operator_now();
        let session = self.operator_auth().check(token, key, origin, now)?;
        Ok(json!({"valid": session.is_some(), "session": session}))
    }

    /// `operator_session_open_device {token, origin, user_agent?}` —
    /// the board's device-grant sign-in exchange (CAD-777). `token` is
    /// the issuer-approved `agc_` grant, verified LIVE against the
    /// daemon-owned config (`operator/device-login.json`, written only
    /// by `operator_device_login_set` under the operator-secret gate,
    /// CAD-841) before anything is minted: the subject and workspace
    /// come out of that verification, never out of request fields, so
    /// a socket caller cannot forge them. The verified subject must
    /// then be on the config's allowlist — the operator named the few
    /// principals who may sign in remotely; any other verified
    /// workspace member is refused, loudly.
    ///
    /// A connection that derives an agent is refused before any issuer
    /// contact — a browser session is never minted for a pane.
    pub(super) fn rpc_operator_session_open_device(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        if let Some(who) = self.slot_identity(peer_pid)? {
            return Err(Error::rejected(format!(
                "operator_session_open_device is the device sign-in exchange — this connection \
                 is agent '{}'; a browser session is never minted for a pane",
                who.lane()
            )));
        }
        let token = required_str(params, "token")?;
        let origin = origin_param(params)?;
        let user_agent = optional_str(params, "user_agent").unwrap_or_default();
        // The daemon-owned config is the only issuer/org authority
        // (CAD-841): an absent file (or one failing strict modes or
        // schema — a legacy board-written pin included) fails closed
        // with no session, before any issuer contact.
        let authority = crate::device_login::read_config(&self.state_dir)?;
        let config = crate::device_login::DeviceConfig::new(&authority.issuer, &authority.org)?;
        let verified = crate::device_login::verify_session(
            &crate::device_login::UreqTransport::new(),
            &config,
            token,
        )
        // A failed live verification is not a caller refusal — the
        // HTTP layer maps this code to 502. The inner messages are
        // fixed strings carrying no issuer content
        // (`refusals_carry_no_issuer_content`).
        .map_err(|e| Error::invalid("device_verification_failed", e.to_string()))?;
        // CAD-851: the verify above can block for the whole transport
        // timeout, and authority can retire inside that window — the
        // operator's `device-login clear`/`set` lands between verify and
        // mint. The re-check runs under the session mutex immediately
        // before the mint it protects, and the config writers hold the
        // same mutex for their file mutation (CAD-841 r1): the write is
        // ordered either before this re-read — and the mint refuses —
        // or after `open_device` finished under the then-live
        // authority. An unchanged config means the allowlist check
        // below still applies the operator's current list.
        let mut auth = self.operator_auth();
        let authority_now = match crate::device_login::read_config(&self.state_dir) {
            Ok(a) => a,
            Err(e) => {
                let _ = self.store.event_public(
                    DAEMON_ALIAS,
                    "operator_device_session_refused",
                    json!({"reason": "authority_lost", "origin": origin.as_str()}),
                );
                return Err(e);
            }
        };
        if authority_now != authority {
            let _ = self.store.event_public(
                DAEMON_ALIAS,
                "operator_device_session_refused",
                json!({"reason": "authority_changed", "origin": origin.as_str()}),
            );
            return Err(Error::invalid(
                "device_authority_changed",
                "device sign-in refused: the daemon's device-login \
                 configuration changed while the grant was being verified \
                 — sign in again",
            ));
        }
        // The allowlist is the operator's gate (review of #541): a
        // verified workspace member who is not on it gets no session.
        // The refusal echoes the subject id — ids aren't credentials,
        // and naming it is how the operator learns what to allowlist.
        if !authority_now.subjects.contains(&verified.subject_id) {
            let _ = self.store.event_public(
                DAEMON_ALIAS,
                "operator_device_session_refused",
                json!({"subject": verified.subject_id, "org": verified.org,
                       "origin": origin.as_str()}),
            );
            return Err(Error::invalid(
                "device_subject_not_allowed",
                format!(
                    "device sign-in refused: subject '{}' is not on this board's \
                     device-login allowlist — allowlist it with \
                     `cadence ui device-login set --subject {}`",
                    verified.subject_id, verified.subject_id
                ),
            ));
        }
        let user = auth::BoardUser {
            sub: verified.subject_id.clone(),
            email: String::new(),
            name: String::new(),
            role: "operator".to_string(),
            handle: verified.subject_id.clone(),
        };
        let now = self.operator_now();
        let principal = authenticated_principal_hash(
            &authority_now.issuer,
            &verified.org,
            &verified.subject_id,
        );
        let account_id = self.store.chat_upload_account(&principal, "device")?;
        let opened = auth.open_device_account(user, origin, user_agent, now, Some(account_id))?;
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "operator_device_session_opened",
            json!({"session": opened.session.id, "origin": origin.as_str(), "org": verified.org}),
        );
        Ok(json!({"token": opened.token, "key": opened.key, "session": opened.session}))
    }

    /// `device_login_config {}` — the board-facing read of the
    /// daemon-owned device-login config (CAD-841). Read-only: any
    /// socket caller may learn `{configured, issuer, org}` — the
    /// issuer URL reaches browsers anyway via the approval link — but
    /// the subject allowlist never leaves the daemon; minting consults
    /// it server-side only.
    pub(super) fn rpc_device_login_config(&self) -> Result<Value> {
        match crate::device_login::read_config(&self.state_dir) {
            Ok(config) => Ok(json!({
                "configured": true,
                "issuer": config.issuer,
                "org": config.org,
            })),
            Err(_) => Ok(json!({"configured": false})),
        }
    }

    /// `operator_device_login_set {secret, issuer, org, subjects}` —
    /// the only writer of the mint-authority config (CAD-841). Same
    /// caller rule as `ui login`: positive operator proof + the
    /// operator secret, so an agent caller, a detached pane child or a
    /// secret-less caller is refused before the file changes. The
    /// triple is validated exactly as the read path demands, so a bad
    /// or forged field can never persist. Retires the device sessions
    /// the new authority no longer covers — every session when the
    /// issuer or workspace changed, the removed subjects otherwise —
    /// before the config is written.
    pub(super) fn rpc_operator_device_login_set(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_with_secret("ui device-login set", params, peer_pid)?;
        let issuer = required_str(params, "issuer")?;
        let org = required_str(params, "org")?;
        let subjects: Vec<String> = params
            .get("subjects")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::invalid(
                    "bad_request",
                    "operator_device_login_set needs a non-empty `subjects` array of \
                     issuer subject ids",
                )
            })?
            .iter()
            .map(|v| {
                v.as_str().map(str::to_string).ok_or_else(|| {
                    Error::invalid("bad_request", "`subjects` entries must be strings")
                })
            })
            .collect::<Result<_>>()?;
        // `DeviceConfig::new` normalizes the issuer origin; `check`
        // (via `write_config`) validates the whole triple — the stored
        // file carries the normalized values it was audited as.
        let normalized = crate::device_login::DeviceConfig::new(issuer, org)?;
        let config = crate::device_login::DeviceLoginConfig {
            issuer: normalized.issuer().to_string(),
            org: normalized.org().to_string(),
            subjects,
        };
        // The write runs under the session mutex — the mint path's
        // post-verify re-check holds the same guard, so a set/clear
        // linearizes against it: either the write lands before that
        // re-read (the mint refuses) or after the mint completed under
        // the then-live authority. Without the shared guard a clear
        // could return between the re-read and `open_device` and a
        // session would mint under already-retired authority (r1).
        // Validate the complete proposed allowlist before any retirement:
        // a rejected set must not end sessions under the unchanged config.
        config.check()?;
        // Retire sessions the new authority no longer covers. A changed
        // issuer or workspace retires every device session; a same-realm
        // set keeps only the subjects still on the allowlist.
        let request = uuid::Uuid::new_v4().to_string();
        let mut frames = Frames::new();
        let mut auth = self.operator_auth();
        let keep = match crate::device_login::read_config(&self.state_dir) {
            Ok(old) if old.issuer == config.issuer && old.org == config.org => {
                Some(config.subjects.clone())
            }
            _ => None,
        };
        let result = auth.revoke_device_sessions_with_intent(keep.as_deref(), |targets| {
            frames = self.prepare_groups(targets, |company, rows| {
                operator_audit("operator_device_login_retired", company, &request, rows)
            })?;
            Ok(())
        });
        let revoked = self.settle_frames(frames, result)?;
        crate::device_login::write_config(&self.state_dir, &config)?;
        // `agent_events` is an open read — the allowlist never enters the
        // event log, only that a set happened, how many names and how many
        // sessions the retirement ended.
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "operator_device_login_set",
            json!({"issuer": config.issuer, "org": config.org,
                   "subject_count": config.subjects.len(), "revoked": revoked}),
        );
        Ok(
            json!({"configured": true, "issuer": config.issuer, "org": config.org,
                  "revoked": revoked}),
        )
    }

    /// `operator_device_login_clear {secret}` — removes the
    /// mint-authority config (CAD-841). Every device session is retired
    /// first; local link and public sessions are untouched. Afterwards
    /// the mint path and every board route fail closed until the
    /// operator sets it again. Clearing on an unconfigured daemon is a
    /// no-op success.
    pub(super) fn rpc_operator_device_login_clear(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_with_secret("ui device-login clear", params, peer_pid)?;
        // Same session mutex as `set` — the retirement linearizes against
        // the mint path's post-verify re-check (r1).
        let request = uuid::Uuid::new_v4().to_string();
        let mut frames = Frames::new();
        let mut auth = self.operator_auth();
        let result = auth.revoke_device_sessions_with_intent(None, |targets| {
            frames = self.prepare_groups(targets, |company, rows| {
                operator_audit("operator_device_login_retired", company, &request, rows)
            })?;
            Ok(())
        });
        let revoked = self.settle_frames(frames, result)?;
        crate::device_login::clear_config(&self.state_dir)?;
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "operator_device_login_cleared",
            json!({"revoked": revoked}),
        );
        Ok(json!({"configured": false, "revoked": revoked}))
    }

    /// `operator_device_login_show {secret}` — the full triple,
    /// subjects included, for `cadence ui device-login show`
    /// (CAD-841). Secret-gated because the allowlist names the
    /// operators who may sign in; the open `device_login_config` read
    /// omits it.
    pub(super) fn rpc_operator_device_login_show(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_with_secret("ui device-login show", params, peer_pid)?;
        match crate::device_login::read_config(&self.state_dir) {
            Ok(config) => Ok(json!({
                "configured": true,
                "issuer": config.issuer,
                "org": config.org,
                "subjects": config.subjects,
            })),
            Err(_) => Ok(json!({"configured": false})),
        }
    }

    /// `board_session_open {assertion, user_agent?}` — the board's
    /// `POST /__platform/session` (CAD-526, contract §4/§9). The
    /// assertion is the credential: structure, Ed25519 signature
    /// against the platform JWKS, `iss`/`aud`/`exp`/`iat`, this
    /// instance's `company`, the role map, and the authoritative
    /// single-use `jti` all pass here before a session exists. The
    /// trust root is the daemon-owned `operator/board-identity.json`;
    /// nothing the request carries chooses it.
    ///
    /// A connection that derives an agent is refused before the `jti`
    /// is consumed — an agent never mints a browser session, and a
    /// refused call must not burn the real sign-in's id.
    pub(super) fn rpc_board_session_open(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        if let Some(who) = self.slot_identity(peer_pid)? {
            let lane = who.lane().to_string();
            self.record_security_denial("agent_connection", None, Some(&lane))?;
            return Err(Error::rejected(format!(
                "board_session_open is the platform sign-in exchange — this connection \
                 is agent '{}'; a browser session is never minted for a pane",
                who.lane()
            )));
        }
        let assertion = required_str(params, "assertion")?;
        let user_agent = optional_str(params, "user_agent").unwrap_or_default();
        let prior_session = optional_str(params, "prior_session");
        let config = match crate::board_identity::read_config(&self.state_dir) {
            Ok(config) => config,
            Err(e) => {
                self.record_security_denial("capability_unavailable", None, None)?;
                return Err(e);
            }
        };
        let now = self.operator_now();
        let parsed = crate::board_identity::parse(assertion)
            .map_err(|r| self.board_rejected(r.code, &r.message, Some(config.company.as_str())))?;
        // `iss` names the JWKS origin (contract §6) — only after it
        // matches the configured issuer, so a foreign assertion never
        // points the fetch at its own keys. `aud` is the cheap early
        // refuse: a sibling board's assertion goes no further.
        if parsed.issuer() != config.issuer {
            return Err(self.board_rejected(
                "issuer_mismatch",
                "the assertion was not issued by this board's platform",
                Some(config.company.as_str()),
            ));
        }
        if parsed.audience() != config.host {
            return Err(self.board_rejected(
                "audience_mismatch",
                "the assertion was minted for another board host",
                Some(config.company.as_str()),
            ));
        }
        let key = {
            let mut cache = self.board_jwks.lock().unwrap_or_else(|e| e.into_inner());
            cache
                .key(&config.issuer, parsed.kid(), now)
                // A fetched-but-unpublished `kid` is the caller's bad
                // assertion (`assertion_invalid`); an unreachable or
                // malformed JWKS is ours — `capability_unavailable`.
                .map_err(|e| match e.code() {
                    Some("assertion_invalid") => self.board_rejected(
                        "assertion_invalid",
                        &e.to_string(),
                        Some(config.company.as_str()),
                    ),
                    _ => self.board_rejected(
                        "capability_unavailable",
                        &e.to_string(),
                        Some(config.company.as_str()),
                    ),
                })?
        };
        let identity = crate::board_identity::verify(&parsed, &config, &key, now)
            .map_err(|r| self.board_rejected(r.code, &r.message, Some(config.company.as_str())))?;
        let realm = crate::operator_auth::PublicRealm {
            issuer: config.issuer.clone(),
            company: config.company.clone(),
            host: config.host.clone(),
        };
        let principal =
            authenticated_principal_hash(&config.issuer, &config.company, &identity.user().sub);
        // CAD-1374: a native daemon's board realm must match its captured
        // realm before any session opens; a standalone one saves the realm
        // it verified under. Captured mode is never re-read here.
        self.require_realm_matches_mode(&realm)?;
        // CAD-1412: the bound open also persists the verified `HumanScope`
        // (issuer + company), derived from this typed realm inside
        // `open_public_bound_with_intent` — never a request field.
        // Resolve a prior cookie only through the same live, scoped public
        // session proof used by human-thread routes. Claims are never trusted.
        let prior_consent_session = if let Some(token) = prior_session {
            self.operator_auth()
                .verified_human_scope(token, now)?
                .filter(|(scope, user)| {
                    scope.issuer != realm.issuer
                        || scope.organization_id != realm.company
                        || user.sub != identity.user().sub
                })
                .map(|_| crate::operator_auth::digest(token))
        } else {
            None
        };
        let request = uuid::Uuid::new_v4().to_string();
        let company = config.company.clone();
        let mut audit = None;
        let mut opened_row = None;
        let deadline = crate::board_identity::accepted_until(parsed.exp());
        let result = {
            // The verifier's clock predates the JWKS fetch. Sample again
            // while the Auth lock is held so the store cannot be reached
            // after the deadline with a stale time (a concurrent check may
            // already have pruned the jti).
            let mut auth = self.operator_auth();
            let opened_at = self.operator_now();
            auth.open_public_bound_with_intent(
                identity.user(),
                (parsed.jti(), deadline),
                user_agent,
                realm,
                opened_at,
                |row| {
                    let a = open_audit(&company, &request, row);
                    self.prepare_mutation(&a, std::slice::from_ref(row))?;
                    audit = Some(a);
                    opened_row = Some(row.clone());
                    if let Some(session) = prior_consent_session.as_deref() {
                        self.store.push_consents_revoke_session(session)?;
                    }
                    Ok(())
                },
            )
        };
        let opened = match result {
            Ok(opened) => opened,
            Err(e) => {
                // Prepared accepted and the Auth write failed: uncertain.
                if let (Some(a), Some(row)) = (audit, opened_row) {
                    if self
                        .finish_uncertain(&a, std::slice::from_ref(&row))
                        .is_err()
                    {
                        return Err(self.terminal_unavailable());
                    }
                }
                return Err(e);
            }
        };
        match opened {
            None => {
                let _ = self.store.event_public(
                    DAEMON_ALIAS,
                    "board_assertion_rejected",
                    json!({"reason": "replayed"}),
                );
                Err(self.security_denied(
                    "assertion_replayed",
                    "the assertion was already exchanged",
                    Some(company.as_str()),
                ))
            }
            Some(mut opened) => {
                let account_binding = self
                    .store
                    .chat_upload_account(&principal, "authenticated")
                    .and_then(|account_id| {
                        self.operator_auth()
                            .bind_account(&opened.token, &account_id)
                    });
                opened.session = match account_binding {
                    Ok(session) => session,
                    Err(error) => {
                        // The public open already changed durable Auth state. Keep
                        // its prepared frame uncertain, then compensate through
                        // the same audited intent/settlement path as other revokes.
                        let open_uncertain = match audit.as_ref().zip(opened_row.as_ref()) {
                            Some((a, row)) => self.finish_uncertain(a, std::slice::from_ref(row)),
                            None => Err(self.terminal_unavailable()),
                        };
                        let mut frames = Frames::new();
                        let cleanup = self.operator_auth().revoke_token_with_intent(
                            &opened.token,
                            |targets| {
                                frames = self.prepare_groups(targets, |company, rows| {
                                    upload_account_setup_compensation_audit(company, &request, rows)
                                })?;
                                Ok(())
                            },
                        );
                        let cleanup = self.settle_frames(frames, cleanup.map(usize::from));
                        if open_uncertain.is_err() || !matches!(cleanup, Ok(1)) {
                            return Err(self.terminal_unavailable());
                        }
                        return Err(error);
                    }
                };
                let _ = self.store.event_public(
                    DAEMON_ALIAS,
                    "board_session_opened",
                    json!({
                        "session": opened.session.id,
                        "origin": Origin::Public.as_str(),
                        "sub": opened.session.user.as_ref().map(|u| u.sub.as_str()),
                        "role": opened.session.user.as_ref().map(|u| u.role.as_str()),
                    }),
                );
                if let Some(a) = audit {
                    let row = auth::AuditTarget {
                        session: opened.session.id.clone(),
                        origin: Origin::Public,
                        sub: opened.session.user.as_ref().map(|u| u.sub.clone()),
                        role: opened.session.user.as_ref().map(|u| u.role.clone()),
                        company: Some(company.clone()),
                    };
                    self.finish_terminal(&a, std::slice::from_ref(&row))
                        .map_err(|_| self.terminal_unavailable())?;
                }
                Ok(json!({
                    "ok": true,
                    "token": opened.token,
                    "session": opened.session,
                }))
            }
        }
    }

    /// `human_actor_check {token}` (CAD-1412) — the board's relay asks
    /// which verified human a public session names, for human-threads
    /// identity. It answers the derived principal id and display name only.
    /// It grants no operator, approval or room authority, and it runs
    /// only on the operator's own connection (agent callers refused by
    /// `operator_connection`). Missing, expired, legacy-unscoped or
    /// non-public sessions fail closed.
    pub(super) fn rpc_human_actor_check(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let token = required_str(params, "token")?;
        let (_, _, actor) = self.verified_human_session(token, params, peer_pid)?;
        Ok(json!({"human": {"id": actor.id, "name": actor.name}}))
    }

    /// Native, narrow: the [`crate::store::human_threads::VerifiedHuman`] a
    /// live public session names (CAD-1412). The operator's own connection
    /// is required (`operator_connection` refuses agent and identity-field
    /// callers), so a registered agent can never resolve a human. Only a
    /// session stored with a verified `issuer`/`organization_id` scope
    /// yields an actor; the principal is the verified issuer, org and exact
    /// `sub`, never the handle or email.
    pub(crate) fn verified_human_session(
        &self,
        token: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<(
        auth::HumanScope,
        String,
        crate::store::human_threads::VerifiedHuman,
    )> {
        self.operator_connection("human actor check", params, peer_pid)?;
        let now = self.operator_now();
        let found = self.operator_auth().verified_human_scope(token, now)?;
        let (scope, user) = found.ok_or_else(|| {
            Error::invalid(
                "human_session_required",
                "no live verified human session: sign in again",
            )
        })?;
        let name = if user.name.is_empty() {
            user.sub.clone()
        } else {
            user.name.clone()
        };
        let actor = crate::store::human_threads::VerifiedHuman::with_name(
            &scope.issuer,
            &scope.organization_id,
            &user.sub,
            &name,
        )?;
        Ok((scope, user.sub, actor))
    }

    /// `board_session_check {token}` — the board, on every public-host
    /// request: the `__Host-aos-board-session` cookie alone is the
    /// credential (there is no page key on this surface). Only
    /// `public` rows can match.
    pub(super) fn rpc_board_session_check(&self, params: &Value) -> Result<Value> {
        let token = required_str(params, "token")?;
        let session = match self.current_public_session(token)? {
            public_membership::Authority::Granted { view, .. } => Some(*view),
            public_membership::Authority::Denied => None,
        };
        Ok(json!({"valid": session.is_some(), "session": session}))
    }

    /// `board_session_member {handle}` — the daemon's proof that a
    /// `member_as` claim names a live public member session
    /// (CAD-1129). The bearer-caller board relays this so `member_as`
    /// never rides an unchecked string; the answer is yes/no only.
    ///
    /// The method carries no credential of its own, so it answers only
    /// the operator's own connection: an agent or detached caller could
    /// otherwise enumerate who is signed in to the board.
    pub(super) fn rpc_board_session_member(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("board member session check", params, peer_pid)?;
        let handle = required_str(params, "handle")?;
        let member = self.handle_has_current_member(handle)?;
        Ok(json!({"member": member}))
    }

    /// CAD-1374: one canonical board-session mutation record. `prepared` is
    /// appended BEFORE the `Auth` write; if it fails the mutation is refused
    /// (`audit_unavailable`). `completed` is appended after it. Stores only the
    /// static action, the noncredential session reference, and the verified
    /// subject and role. Never the token, key, jti, user agent or assertion.
    fn audit_payload(&self, a: &Audit, targets: &[auth::AuditTarget], outcome: &str) -> Value {
        json!({
            "schema_version": 1,
            "at": crate::issue::time::now_epoch(),
            "company": a.company,
            "actor_kind": a.actor_kind,
            "human": a.human,
            "agent": a.agent,
            "action": a.action,
            "resource": a.resource,
            "outcome": outcome,
            "reason": a.reason,
            "request_id": a.request,
            "correlation_id": a.request,
            "causation_id": null,
            "revision_or_digest": null,
            "revision_availability": "session_state_has_no_content_revision",
            "targets": targets.iter().map(target_json).collect::<Vec<_>>(),
        })
    }

    /// Append `prepared` BEFORE an `Auth` write. Failure refuses the mutation
    /// (`audit_unavailable`); nothing changed, so no terminal claim is made.
    fn prepare_mutation(&self, a: &Audit, targets: &[auth::AuditTarget]) -> Result<()> {
        let payload = self.audit_payload(a, targets, "prepared");
        self.store
            .event_public(DAEMON_ALIAS, "board_session_mutation_prepared", payload)
            .map_err(|_| {
                Error::invalid(
                    "audit_unavailable",
                    "the board session change was refused: its audit record could not be written",
                )
            })
    }

    /// Append the terminal record after a successful `Auth` write. Failure is
    /// an uncertain-outcome error: the mutation may have happened, and the
    /// prepared record is retained, never retried.
    fn finish_terminal(&self, a: &Audit, targets: &[auth::AuditTarget]) -> Result<()> {
        let payload = self.audit_payload(a, targets, "completed");
        self.store
            .event_public(DAEMON_ALIAS, "board_session_mutation_finished", payload)
    }

    /// The `Auth` write failed after `prepared` was accepted: the outcome is
    /// unknown. Record uncertainty and never claim a clean no-op.
    fn finish_uncertain(&self, a: &Audit, targets: &[auth::AuditTarget]) -> Result<()> {
        let payload = self.audit_payload(a, targets, "uncertain");
        self.store
            .event_public(DAEMON_ALIAS, "board_session_mutation_finished", payload)
    }

    /// Prepare one frame per company group, all BEFORE any `Auth` write. A
    /// refused prepare returns at once, so the caller's intent refuses and no
    /// mutation happens; earlier prepared frames stay prepared, never terminal.
    fn prepare_groups(
        &self,
        targets: &[auth::AuditTarget],
        make: impl Fn(Option<String>, &[auth::AuditTarget]) -> Audit,
    ) -> Result<Frames> {
        let mut frames = Vec::new();
        for (company, rows) in by_company(targets) {
            let a = make(company, &rows);
            self.prepare_mutation(&a, &rows)?;
            frames.push((a, rows));
        }
        Ok(frames)
    }

    /// Settle the frames of a mutation whose `Auth` call ran its intent first.
    /// An `Auth` error marks every prepared group uncertain; success gives each
    /// group its terminal record. A failed terminal append is the static
    /// uncertain refusal, and nothing is retried.
    fn settle_frames(&self, frames: Frames, result: Result<usize>) -> Result<usize> {
        match result {
            Err(e) => {
                let mut recorded = true;
                for (a, rows) in &frames {
                    if self.finish_uncertain(a, rows).is_err() {
                        recorded = false;
                    }
                }
                if recorded {
                    Err(e)
                } else {
                    Err(self.terminal_unavailable())
                }
            }
            Ok(n) => {
                let mut all = true;
                for (a, rows) in &frames {
                    all &= self.finish_terminal(a, rows).is_ok();
                }
                if all {
                    Ok(n)
                } else {
                    Err(self.terminal_unavailable())
                }
            }
        }
    }

    fn terminal_unavailable(&self) -> Error {
        Error::invalid(
            "state_outcome_unavailable",
            "the session change may have happened, but its durable outcome could not be confirmed; prepared record retained, not retried",
        )
    }

    /// Append the canonical pre-auth refusal. Stores only the static reason,
    /// the trusted configured company (or null) and the daemon-derived agent
    /// lane. A failed append is an error, so the caller never mints.
    fn record_security_denial(
        &self,
        reason: &'static str,
        company: Option<&str>,
        agent: Option<&str>,
    ) -> Result<()> {
        let request = uuid::Uuid::new_v4().to_string();
        let payload = json!({
            "schema_version": 1,
            "at": crate::issue::time::now_epoch(),
            "company": company,
            "actor_kind": if agent.is_some() { "agent" } else { "unknown" },
            "human": null,
            "agent": agent,
            "action": "board_session_security_denied",
            "resource": "board:session_exchange",
            "outcome": "refused",
            "reason": reason,
            "request_id": request,
            "correlation_id": request,
            "causation_id": null,
            "revision_or_digest": null,
            "revision_availability": "session_state_has_no_content_revision",
            "targets": [],
        });
        self.store
            .event_public(DAEMON_ALIAS, "board_session_security_denied", payload)
            .map_err(|_| {
                Error::invalid(
                    "audit_unavailable",
                    "the board sign-in was refused: its audit record could not be written",
                )
            })
    }

    /// The refusal a pre-auth check returns: its canonical record, or the
    /// sanitized `audit_unavailable` refusal when that record cannot be written.
    fn security_denied(&self, code: &'static str, message: &str, company: Option<&str>) -> Error {
        match self.record_security_denial(code, company, None) {
            Ok(()) => Error::invalid(code, message),
            Err(refusal) => refusal,
        }
    }

    /// A refused assertion is loud: the legacy `board_assertion_rejected`
    /// notice, then the canonical denial (the original code and message are
    /// kept when that record is written).
    fn board_rejected(&self, code: &'static str, message: &str, company: Option<&str>) -> Error {
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "board_assertion_rejected",
            json!({"reason": code}),
        );
        self.security_denied(code, message, company)
    }

    /// `operator_session_logout {token}` — the cookie holder ends its own
    /// session, whatever its authority. The company comes only from the
    /// matched row's verified realm, never from the request body.
    pub(super) fn rpc_operator_session_logout(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let token = required_str(params, "token")?;
        let agent = self
            .slot_identity(peer_pid)?
            .map(|who| who.lane().to_string());
        let request = uuid::Uuid::new_v4().to_string();
        // CAD-1423: logout revokes this session's push consents first, keyed by
        // the same digest the session row carries, so no consent outlives it.
        self.store
            .push_consents_revoke_session(&crate::operator_auth::digest(token))?;
        let mut frames = Frames::new();
        let result = self
            .operator_auth()
            .revoke_token_with_intent(token, |targets| {
                frames = self.prepare_groups(targets, |company, rows| {
                    logout_audit(agent.as_deref(), company, &request, rows)
                })?;
                Ok(())
            });
        let revoked = self.settle_frames(frames, result.map(usize::from))? > 0;
        Ok(json!({"revoked": revoked}))
    }

    pub(super) fn rpc_operator_session_stolen(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let token = required_str(params, "token")?;
        // The actor is the daemon's own view of the peer; the body's `agent`
        // label is only the legacy alarm text.
        let agent = optional_str(params, "agent").unwrap_or("unknown");
        let peer = self
            .slot_identity(peer_pid)?
            .map(|who| who.lane().to_string());
        let request = uuid::Uuid::new_v4().to_string();
        let mut frames = Frames::new();
        let result = self
            .operator_auth()
            .revoke_token_with_intent(token, |targets| {
                frames = self.prepare_groups(targets, |company, rows| {
                    stolen_audit(peer.as_deref(), company, &request, rows)
                })?;
                Ok(())
            });
        let revoked = self.settle_frames(frames, result.map(usize::from))? > 0;
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "operator_session_from_agent",
            json!({"agent": agent, "revoked": revoked, "alert": true}),
        );
        Ok(json!({"revoked": revoked}))
    }

    pub(super) fn rpc_operator_sessions(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_with_secret("ui sessions", params, peer_pid)?;
        let request = uuid::Uuid::new_v4().to_string();
        let mut revoked = 0;
        if params.get("revoke_all").and_then(Value::as_bool) == Some(true) {
            let mut frames = Frames::new();
            // `revoke_all` also voids links; the count is the sessions it ended.
            let result = self.operator_auth().revoke_all_with_intent(|targets| {
                frames = self.prepare_groups(targets, |company, rows| {
                    operator_audit("operator_revoke_all", company, &request, rows)
                })?;
                Ok(())
            });
            revoked += self.settle_frames(frames, result)?;
        }
        if let Some(id) = optional_str(params, "revoke") {
            let mut frames = Frames::new();
            let result = self.operator_auth().revoke_id_with_intent(id, |targets| {
                frames = self.prepare_groups(targets, |company, rows| {
                    operator_audit("operator_revoke", company, &request, rows)
                })?;
                Ok(())
            });
            revoked += self.settle_frames(frames, result)?;
        }
        let sessions = self.operator_auth().list(self.operator_now())?;
        if revoked > 0 {
            let _ = self.store.event_public(
                DAEMON_ALIAS,
                "operator_sessions_revoked",
                json!({"count": revoked, "pid": peer_pid}),
            );
        }
        Ok(json!({"revoked": revoked, "sessions": sessions}))
    }

    pub(super) fn rpc_operator_secret_rotate(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_with_secret("ui login --rotate", params, peer_pid)?;
        let request = uuid::Uuid::new_v4().to_string();
        // Under the Auth guard and before the first secret write, every company
        // group the rotation ends is prepared (a local or empty group too, since
        // rotation always has an effect). A refused prepare stops the rotation.
        let mut store = self.operator_auth();
        let frames = self.prepare_groups(&store.audit_targets(), |company, rows| {
            operator_audit("operator_secret_rotate", company, &request, rows)
        })?;
        let rotated = auth::rotate_secret(&self.state_dir).and_then(|()| store.revoke_all());
        drop(store);
        let revoked = self.settle_frames(frames, rotated)?;
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "operator_secret_rotated",
            json!({"revoked": revoked, "pid": peer_pid}),
        );
        Ok(json!({"rotated": true, "revoked": revoked}))
    }
}
