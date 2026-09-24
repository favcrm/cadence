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
//! - `operator_session_check {token, origin}` — the board, on every
//!   operator decision and `/api/meta`.
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

use serde_json::{json, Value};

use super::{optional_str, reject_operator_fields, required_str, Shared, DAEMON_ALIAS};
use crate::error::{Error, Result};
use crate::operator_auth::{self as auth, Origin};

fn origin_param(params: &Value) -> Result<Origin> {
    let raw = required_str(params, "origin")?;
    Origin::parse(raw).ok_or_else(|| {
        Error::invalid(
            "invalid_request",
            format!("origin must be 'loopback' or 'tailnet', not '{raw}'"),
        )
    })
}

impl Shared {
    fn operator_now(&self) -> i64 {
        (self.operator_clock)()
    }

    fn operator_auth(&self) -> std::sync::MutexGuard<'_, auth::Auth> {
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
        let nonce = self.operator_auth().mint(origin, now)?;
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
                Ok(json!({"token": opened.token, "session": opened.session}))
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
        let origin = origin_param(params)?;
        let now = self.operator_now();
        let session = self.operator_auth().check(token, origin, now)?;
        Ok(json!({"valid": session.is_some(), "session": session}))
    }

    pub(super) fn rpc_operator_session_logout(&self, params: &Value) -> Result<Value> {
        let token = required_str(params, "token")?;
        let revoked = self.operator_auth().revoke_token(token)?;
        Ok(json!({"revoked": revoked}))
    }

    pub(super) fn rpc_operator_session_stolen(&self, params: &Value) -> Result<Value> {
        let token = required_str(params, "token")?;
        let agent = optional_str(params, "agent").unwrap_or("unknown");
        let revoked = self.operator_auth().revoke_token(token)?;
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "operator_session_from_agent",
            json!({"agent": agent, "revoked": revoked, "alert": true}),
        );
        Ok(json!({"revoked": revoked}))
    }

    pub(super) fn rpc_operator_sessions(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_with_secret("ui sessions", params, peer_pid)?;
        let mut revoked = 0;
        let mut store = self.operator_auth();
        if params.get("revoke_all").and_then(Value::as_bool) == Some(true) {
            revoked += store.revoke_all()?;
        }
        if let Some(id) = optional_str(params, "revoke") {
            revoked += store.revoke_id(id)?;
        }
        let sessions = store.list(self.operator_now())?;
        drop(store);
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
        let mut store = self.operator_auth();
        auth::rotate_secret(&self.state_dir)?;
        let revoked = store.revoke_all()?;
        drop(store);
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "operator_secret_rotated",
            json!({"revoked": revoked, "pid": peer_pid}),
        );
        Ok(json!({"rotated": true, "revoked": revoked}))
    }
}
