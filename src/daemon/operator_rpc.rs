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
//!   write and `/api/meta`: the cookie's token AND the page's
//!   `X-Cadence-Session` key, both required.
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
            handle: verified.subject_id,
        };
        let now = self.operator_now();
        let opened = auth.open_device(user, origin, user_agent, now)?;
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
    /// or forged field can never persist.
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
        let _auth = self.operator_auth();
        crate::device_login::write_config(&self.state_dir, &config)?;
        // `agent_events` is an open read — the allowlist never enters
        // the event log, only that a set happened and how many names.
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "operator_device_login_set",
            json!({"issuer": config.issuer, "org": config.org,
                   "subject_count": config.subjects.len()}),
        );
        Ok(json!({"configured": true, "issuer": config.issuer, "org": config.org}))
    }

    /// `operator_device_login_clear {secret}` — removes the
    /// mint-authority config (CAD-841). Afterwards the mint path and
    /// every board route fail closed until the operator sets it again.
    /// Clearing on an unconfigured daemon is a no-op success.
    pub(super) fn rpc_operator_device_login_clear(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_with_secret("ui device-login clear", params, peer_pid)?;
        // Same session mutex as `set` — the retirement linearizes
        // against the mint path's post-verify re-check (r1).
        let _auth = self.operator_auth();
        crate::device_login::clear_config(&self.state_dir)?;
        let _ = self
            .store
            .event_public(DAEMON_ALIAS, "operator_device_login_cleared", json!({}));
        Ok(json!({"configured": false}))
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
            return Err(Error::rejected(format!(
                "board_session_open is the platform sign-in exchange — this connection \
                 is agent '{}'; a browser session is never minted for a pane",
                who.lane()
            )));
        }
        let assertion = required_str(params, "assertion")?;
        let user_agent = optional_str(params, "user_agent").unwrap_or_default();
        let config = crate::board_identity::read_config(&self.state_dir)?;
        let now = self.operator_now();
        let parsed = crate::board_identity::parse(assertion)
            .map_err(|r| self.board_rejected(r.code, &r.message))?;
        // `iss` names the JWKS origin (contract §6) — only after it
        // matches the configured issuer, so a foreign assertion never
        // points the fetch at its own keys. `aud` is the cheap early
        // refuse: a sibling board's assertion goes no further.
        if parsed.issuer() != config.issuer {
            return Err(self.board_rejected(
                "issuer_mismatch",
                "the assertion was not issued by this board's platform",
            ));
        }
        if parsed.audience() != config.host {
            return Err(self.board_rejected(
                "audience_mismatch",
                "the assertion was minted for another board host",
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
                    Some("assertion_invalid") => {
                        self.board_rejected("assertion_invalid", &e.to_string())
                    }
                    _ => self.board_rejected("capability_unavailable", &e.to_string()),
                })?
        };
        let identity = crate::board_identity::verify(&parsed, &config, &key, now)
            .map_err(|r| self.board_rejected(r.code, &r.message))?;
        let opened = self.operator_auth().open_public(
            identity.user(),
            parsed.jti(),
            parsed.exp(),
            user_agent,
            now,
        )?;
        match opened {
            None => {
                let _ = self.store.event_public(
                    DAEMON_ALIAS,
                    "board_assertion_rejected",
                    json!({"reason": "replayed"}),
                );
                Err(Error::invalid(
                    "assertion_replayed",
                    "the assertion was already exchanged",
                ))
            }
            Some(opened) => {
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
                Ok(json!({
                    "ok": true,
                    "token": opened.token,
                    "session": opened.session,
                }))
            }
        }
    }

    /// `board_session_check {token}` — the board, on every public-host
    /// request: the `__Host-aos-board-session` cookie alone is the
    /// credential (there is no page key on this surface). Only
    /// `public` rows can match.
    pub(super) fn rpc_board_session_check(&self, params: &Value) -> Result<Value> {
        let token = required_str(params, "token")?;
        let now = self.operator_now();
        let session = self.operator_auth().check_public(token, now)?;
        Ok(json!({"valid": session.is_some(), "session": session}))
    }

    /// `board_session_member {handle}` — the daemon's proof that a
    /// `member_as` claim names a live public member session
    /// (CAD-1129). The bearer-caller board relays this so `member_as`
    /// never rides an unchecked string; the answer is yes/no only.
    pub(super) fn rpc_board_session_member(&self, params: &Value) -> Result<Value> {
        let handle = required_str(params, "handle")?;
        let now = self.operator_now();
        let member = self.operator_auth().check_member(handle, now)?;
        Ok(json!({"member": member}))
    }

    /// A refused assertion is loud — `board_assertion_rejected` records
    /// the refusal's code (never the assertion or key material).
    fn board_rejected(&self, code: &'static str, message: &str) -> Error {
        let _ = self.store.event_public(
            DAEMON_ALIAS,
            "board_assertion_rejected",
            json!({"reason": code}),
        );
        Error::invalid(code, message)
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
