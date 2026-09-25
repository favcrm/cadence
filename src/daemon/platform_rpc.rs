//! CAD-366 / ADR 0006 §5.1, §5.3, §5.5: platform custody and grants —
//! the daemon verbs an operator enrolls, revokes and rotates platform
//! credentials through, and the reads an agent checks its own grants
//! with.
//!
//! Every mutating verb gates on [`Shared::operator_connection`]: the
//! caller's authority comes from the connection alone — no `by`, no
//! claimed alias, and an agent or unproven caller is refused before any
//! custody or record is touched. `platform_grants`/`platform_check`
//! bind an agent caller to its own alias.
//!
//! No verb ever returns credential bytes. Enrollment takes the token
//! over the socket once (operator→daemon) into custody; results,
//! events and errors carry `{platform, account, scopes, fingerprint,
//! enrolled_at, by}` only.

use serde_json::{json, Value};

use super::{optional_str, required_str, AgentCaller, Shared};
use crate::error::{Error, Result};
use crate::platform::{self, custody, Enrollment};
use crate::proto::identifier;
use crate::store::{scope_list, scope_name, CredentialRecord, Grant};

/// How the daemon records the actor on custody/grant writes — a
/// constant: every mutating verb gates on `operator_connection`
/// first, so the only `by` these rows can carry is the operator.
const OPERATOR: &str = "operator";

impl Shared {
    /// ADR 0006's P4 residual, surfaced as a gate: how this daemon's
    /// custody is isolated from the agents it manages. `Some(mode)`
    /// names an isolation that keeps bytes out of every managed
    /// agent's reach; `None` means custody is protected only by uid
    /// and by the master's Landlock confinement — every same-uid
    /// unconfined pty agent can read it. No mode exists today;
    /// `platform enroll` refuses `custody_unprotected` unless the
    /// operator passes `accept_same_uid_risk`.
    fn custody_isolation(&self) -> Option<&'static str> {
        None
    }

    /// `platform_enroll {platform, account, scopes, shape?, token?,
    /// class?, accept_same_uid_risk?}` — ADR 0006 §5.3. `shape` is
    /// `token` (the operator-enrolled scoped token, crossing the
    /// socket once) or `consent` (the adapter's device-code/OTP
    /// exchange — the seam CAD-501 registers into). The credential
    /// never leaves this handler: custody stores it, the record keeps
    /// a fingerprint.
    ///
    /// While [`Self::custody_isolation`] reports no mode, a first
    /// enrollment refuses `custody_unprotected` before any platform
    /// traffic — custody is same-uid readable by managed agents
    /// (ADR 0006 P4). `accept_same_uid_risk: true` is the operator's
    /// explicit override, recorded on the `platform_connected` event
    /// as `custody_risk_accepted: "same-uid"`.
    pub(super) fn rpc_platform_enroll(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("platform enroll", params, peer_pid)?;
        self.enroll_inner(params, false)
    }

    /// `platform_rotate {platform, account, ...}` — §5.3: re-enroll
    /// under the same handle; grants and the project default do not
    /// change. Rotation inherits the enroll-time custody acceptance —
    /// the exposure it refreshes was consented to already.
    pub(super) fn rpc_platform_rotate(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("platform rotate", params, peer_pid)?;
        self.enroll_inner(params, true)
    }

    fn enroll_inner(&self, params: &Value, rotate: bool) -> Result<Value> {
        let platform = identifier(required_str(params, "platform")?, "Platform")?;
        let account = identifier(required_str(params, "account")?, "Account")?;
        let declared = match params.get("scopes") {
            Some(_) => scope_list(params, "scopes")?,
            // Rotation without a `scopes` keeps the record's set —
            // redeclaring is optional, losing them never is.
            None if rotate => self
                .store
                .platform_credential(&platform, &account)?
                .map(|r| r.scopes)
                .ok_or_else(|| {
                    Error::rejected(format!(
                        "no credential is enrolled for {platform}/{account} — \
                         `platform enroll` it first"
                    ))
                })?,
            None => return Err(Error::rejected("Missing or non-array 'scopes'")),
        };
        let accept_risk = params
            .get("accept_same_uid_risk")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // Serialize the custody write against the record write it
        // pairs with: concurrent enrolls of one account, or a revoke
        // racing a put, must never leave a record whose fingerprint
        // disagrees with the bytes under it. Held across check →
        // exchange → put → record; a consent adapter's network wait
        // stretches it, which is correct — the record must not turn
        // over mid-exchange.
        let _custody_guard = self
            .platform_custody_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Existence is checked inside the lock before custody is
        // touched — an early `put` would leave bytes disagreeing with
        // the record's fingerprint. The store re-checks inside its
        // transaction, so these pre-checks only order the refusal
        // ahead of the write.
        let existing = self.store.platform_credential(&platform, &account)?;
        if !rotate && existing.is_some() {
            return Err(Error::rejected(format!(
                "platform '{platform}' account '{account}' is already enrolled — \
                 `platform rotate` replaces its credential"
            )));
        }
        if rotate && existing.is_none() {
            return Err(Error::rejected(format!(
                "no credential is enrolled for {platform}/{account} — \
                 `platform enroll` it first"
            )));
        }
        // A first enrollment gates on custody isolation: today none —
        // the file store and the keychain alike are readable by every
        // same-uid unconfined agent (ADR 0006 P4). Refuse before any
        // exchange runs; `accept_same_uid_risk` is the operator's
        // recorded override. Rotate inherits the enroll-time choice.
        let risk = if !rotate && self.custody_isolation().is_none() {
            if !accept_risk {
                return Err(Error::invalid(
                    "custody_unprotected",
                    "platform enroll refused: custody on this daemon is not isolated \
                     from managed agents — the 0600 store/keychain is same-uid \
                     readable (ADR 0006 P4 residual; only the master is confined). \
                     Enroll anyway with `--accept-same-uid-risk`; the acceptance \
                     is recorded on the audit event",
                ));
            }
            Some("same-uid")
        } else {
            None
        };
        let enrollment: Enrollment = match optional_str(params, "shape").unwrap_or("token") {
            "token" => platform::enroll_token(params, &declared)?,
            "consent" => platform::enroll_consent(&platform, &account, &declared, params)?,
            other => {
                return Err(Error::rejected(format!(
                    "unknown exchange shape '{other}' — 'token' or 'consent'"
                )))
            }
        };
        let fingerprint = crate::secret::fingerprint(&enrollment.bytes);
        let key = custody::Key {
            platform: &platform,
            account: &account,
        };
        // A rotate captures the old bytes first: if the record write
        // fails, custody is put back exactly as it was — never left
        // holding bytes the record disowns. A load failure refuses the
        // rotate before anything is overwritten.
        let prior = match &existing {
            Some(record) => Some(self.platform_custody.load(&record.custody, &key)?),
            None => None,
        };
        let custody_tag = self.platform_custody.put(&key, &enrollment.bytes)?;
        let record = CredentialRecord {
            platform,
            account,
            scopes: enrollment.scopes,
            fingerprint,
            custody: custody_tag.to_string(),
            exchange: enrollment.exchange.to_string(),
            enrolled_at: crate::issue::time::now_epoch() as f64,
            by: OPERATOR.to_string(),
        };
        if let Err(err) = self.store.platform_enroll(&record, rotate, risk) {
            // The record refused — custody must hold exactly what the
            // record (still) describes: the old bytes for a rotate,
            // nothing for a fresh enroll.
            let key = custody::Key {
                platform: &record.platform,
                account: &record.account,
            };
            match &prior {
                Some(old) => {
                    let _ = self.platform_custody.put(&key, old);
                }
                None => {
                    let _ = self.platform_custody.remove(custody_tag, &key);
                }
            }
            return Err(err);
        }
        // The result and its event carry handles only — prove it: a
        // byte leak here is a withheld response, never a quiet one.
        let result = record.to_json();
        platform::refuse_leak(
            "platform enroll result",
            &result.to_string(),
            &enrollment.bytes,
        )?;
        Ok(json!({"state": "enrolled", "account": result}))
    }

    /// `platform_revoke {platform, account, reason?}` — the operator
    /// disconnects an account: custody drops the bytes, the record
    /// and its grants go, pending effects bound to the credential
    /// close unanswered with the named reason (§5.3 — the hook
    /// CAD-506 completes), and the audit events land in one
    /// transaction.
    pub(super) fn rpc_platform_revoke(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("platform revoke", params, peer_pid)?;
        let platform = identifier(required_str(params, "platform")?, "Platform")?;
        let account = identifier(required_str(params, "account")?, "Account")?;
        let reason = optional_str(params, "reason");
        // Same lock the enroll path serializes on: a revoke racing a
        // rotate must not interleave remove with put.
        let _custody_guard = self
            .platform_custody_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let record = self
            .store
            .platform_credential(&platform, &account)?
            .ok_or_else(|| {
                Error::rejected(format!(
                    "no credential is enrolled for {platform}/{account}"
                ))
            })?;
        let key = custody::Key {
            platform: &platform,
            account: &account,
        };
        // `reason` is operator text that lands on audit events and the
        // agent-visible `request_closed` — screen it against the bytes
        // it must never carry before the drain publishes it. A custody
        // that no longer reads has nothing left to leak.
        let bytes = self.platform_custody.load(&record.custody, &key).ok();
        if let (Some(reason), Some(bytes)) = (reason, bytes.as_deref()) {
            platform::refuse_leak("platform revoke reason", reason, bytes)?;
        }
        // Bytes first: a revoke that cannot drop custody refuses
        // before the record goes — the record without custody still
        // fails closed at load.
        self.platform_custody.remove(&record.custody, &key)?;
        let closed = self.close_credential_effects(
            &platform,
            &account,
            reason.unwrap_or("credential revoked"),
        );
        let revoked = self
            .store
            .platform_revoke(&platform, &account, OPERATOR, reason, &closed);
        let Some((record, grants)) = (match revoked {
            Err(err) => {
                // The record stayed — custody must describe it again:
                // put the bytes back (best effort; the record fails
                // closed at load if the restore cannot land either).
                if let Some(bytes) = bytes {
                    let _ = self.platform_custody.put(&key, &bytes);
                }
                return Err(err);
            }
            Ok(v) => v,
        }) else {
            return Err(Error::rejected(format!(
                "no credential is enrolled for {platform}/{account}"
            )));
        };
        self.wake();
        Ok(json!({
            "state": "revoked",
            "platform": record.platform,
            "account": record.account,
            "fingerprint": record.fingerprint,
            "grants_revoked": grants.len(),
            "effects_closed": closed,
        }))
    }

    /// `platform_grant {agent, platform, account, scopes}` — the
    /// operator's record that `agent` may call the account at those
    /// scopes (§5.3). `agent` names the TARGET; it is not an identity
    /// field — the caller is still proven by the connection.
    pub(super) fn rpc_platform_grant(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("platform grant", params, peer_pid)?;
        let agent = identifier(required_str(params, "agent")?, "Agent")?;
        self.store.agent(&agent).map_err(|_| {
            Error::rejected(format!("grant target '{agent}' is not a registered agent"))
        })?;
        let platform = identifier(required_str(params, "platform")?, "Platform")?;
        let account = identifier(required_str(params, "account")?, "Account")?;
        let scopes = scope_list(params, "scopes")?;
        let grant = self
            .store
            .platform_grant_add(&agent, &platform, &account, &scopes, OPERATOR)?;
        Ok(json!({"state": "granted", "grant": grant.to_json()}))
    }

    /// `platform_ungrant {agent, platform, account, scopes?}` —
    /// revoke scopes off a grant (`scopes` omitted: the whole grant).
    pub(super) fn rpc_platform_ungrant(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("platform ungrant", params, peer_pid)?;
        let agent = identifier(required_str(params, "agent")?, "Agent")?;
        let platform = identifier(required_str(params, "platform")?, "Platform")?;
        let account = identifier(required_str(params, "account")?, "Account")?;
        let scopes = match params.get("scopes") {
            None | Some(Value::Null) => None,
            Some(_) => Some(scope_list(params, "scopes")?),
        };
        let (existed, surviving) = self.store.platform_grant_revoke(
            &agent,
            &platform,
            &account,
            scopes.as_deref(),
            OPERATOR,
        )?;
        if !existed {
            return Err(Error::rejected(format!(
                "'{agent}' holds no grant on {platform}/{account}"
            )));
        }
        // CAD-506: a staged send's frozen scopes may no longer be
        // covered — drain the waiting rows this grant loss stranded
        // rather than leave a press to close them at Execute.
        let stranded: Vec<String> = self
            .store
            .platform_effects(Some(&agent))?
            .into_iter()
            .filter(|r| {
                r.platform == platform
                    && r.account == account
                    && r.state == "waiting"
                    && r.scopes
                        .iter()
                        .any(|s| surviving.as_ref().is_none_or(|g| !g.covers(s)))
            })
            .map(|r| (r.request, r.effect_id))
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|(request, effect_id)| {
                self.store
                    .effect_close(crate::store::EffectKey::Id(effect_id), "grant_revoked")
                    .ok()
                    .flatten()
                    .map(|row| {
                        let _ = self.store.event_public(
                            &row.agent,
                            "request_closed",
                            json!({"request": request, "kind": "effect",
                                   "reason": "grant_revoked"}),
                        );
                        row.request
                    })
            })
            .collect();
        if !stranded.is_empty() {
            self.wake();
        }
        Ok(
            json!({"state": "revoked", "grant": surviving.map(|g| g.to_json()),
                  "effects_closed": stranded}),
        )
    }

    /// `platform_grants {agent?}` — §5.3's "what am I allowed": an
    /// agent caller reads its own grants only (a param naming another
    /// agent refuses); the operator reads all, or one agent's.
    pub(super) fn rpc_platform_grants(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let caller = self.agent_caller(peer_pid, "platform grants")?;
        let filter = match &caller {
            AgentCaller::Operator => match optional_str(params, "agent") {
                Some(a) => Some(identifier(a, "Agent")?),
                None => None,
            },
            AgentCaller::Agent(alias) => {
                if let Some(named) = optional_str(params, "agent") {
                    if named != alias.as_str() {
                        return Err(Error::rejected(format!(
                            "platform grants refused: '{alias}' reads its own grants — \
                             '{named}' is another agent's"
                        )));
                    }
                }
                Some(alias.clone())
            }
        };
        let grants = self
            .store
            .platform_grants(filter.as_deref())?
            .iter()
            .map(Grant::to_json)
            .collect::<Vec<_>>();
        Ok(json!({"grants": grants}))
    }

    /// `platform_check {platform, scope, account?|project?}` —
    /// pre-flight the §5.3 grant check over the socket. An agent
    /// checks against its own alias; the operator passes `agent`.
    /// `account` defaults to the caller's (or named) project's
    /// default account for the platform.
    pub(super) fn rpc_platform_check(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let caller = self.agent_caller(peer_pid, "platform check")?;
        let platform = identifier(required_str(params, "platform")?, "Platform")?;
        let scope = scope_name(required_str(params, "scope")?)?;
        let agent = match &caller {
            AgentCaller::Agent(alias) => {
                if let Some(named) = optional_str(params, "agent") {
                    if named != alias.as_str() {
                        return Err(Error::rejected(format!(
                            "platform check refused: '{alias}' checks its own grants — \
                             '{named}' is another agent's"
                        )));
                    }
                }
                alias.clone()
            }
            AgentCaller::Operator => identifier(
                required_str(params, "agent").map_err(|_| {
                    Error::rejected(
                        "operator checks name 'agent' — the grant holder they are checking for",
                    )
                })?,
                "Agent",
            )?,
        };
        let account = match optional_str(params, "account") {
            Some(a) => identifier(a, "Account")?,
            None => {
                // The project default resolves the account: the
                // param's, else the caller agent's own project.
                let project = match optional_str(params, "project") {
                    Some(p) => identifier(p, "Project")?,
                    None => {
                        let agent_row = self.store.agent(&agent)?;
                        crate::issue::project::key_for_cwd(
                            &self.pm_dir()?,
                            std::path::Path::new(&agent_row.cwd),
                        )
                        .ok_or_else(|| {
                            Error::rejected(format!(
                                "'{agent}'s cwd resolves to no project — pass 'account' \
                                 or 'project'"
                            ))
                        })?
                    }
                };
                self.store
                    .platform_default(&project, &platform)?
                    .ok_or_else(|| {
                        Error::rejected(format!(
                            "project '{project}' names no default account for '{platform}'"
                        ))
                    })?
                    .account
            }
        };
        // §5.3: a refusal names the missing scope and happens before
        // any platform traffic — the check touches no custody.
        let grant = platform::require_grant(&self.store, &agent, &platform, &account, scope)?;
        Ok(json!({"granted": true, "grant": grant.to_json()}))
    }

    /// `platform_accounts` — the enrolled handles (§5.3's record:
    /// platform, account, scopes, fingerprint, enrolled_at, by), open
    /// to every socket reader.
    pub(super) fn rpc_platform_accounts(&self, _params: &Value) -> Result<Value> {
        let accounts = self
            .store
            .platform_credentials()?
            .iter()
            .map(CredentialRecord::to_json)
            .collect::<Vec<_>>();
        Ok(json!({"accounts": accounts}))
    }

    /// `platform_default_set {project, platform, account}` — the
    /// project-level default account (§5.1) a call resolves through
    /// when it names none.
    pub(super) fn rpc_platform_default_set(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("platform default set", params, peer_pid)?;
        let project = identifier(required_str(params, "project")?, "Project")?;
        // The key must name a real project — a default for a typo is
        // dead config the check verb would never find.
        crate::issue::project::resolve(&self.pm_dir()?, Some(&project), &self.state_dir)?;
        let platform = identifier(required_str(params, "platform")?, "Platform")?;
        let account = identifier(required_str(params, "account")?, "Account")?;
        let default = self
            .store
            .platform_default_set(&project, &platform, &account, OPERATOR)?;
        Ok(json!({
            "state": "set",
            "default": {"project": default.project, "platform": default.platform,
                        "account": default.account, "by": default.by},
        }))
    }

    /// `platform_defaults` — every project default, open read.
    pub(super) fn rpc_platform_defaults(&self, _params: &Value) -> Result<Value> {
        let defaults = self
            .store
            .platform_defaults()?
            .iter()
            .map(|d| {
                json!({"project": d.project, "platform": d.platform,
                       "account": d.account, "set_at": d.set_at, "by": d.by})
            })
            .collect::<Vec<_>>();
        Ok(json!({"defaults": defaults}))
    }

    /// §5.3's revocation hook: every pending effect bound to
    /// `platform`/`account` closes unanswered — CAD-506's durable
    /// `platform_effects` rows (`waiting`/`reconcile`, close reason
    /// `credential_revoked`), and any in-memory `kind:"effect"`
    /// brokered request opened before the durable lane existed. The
    /// agent-lane `request_closed` ends any waiter on the handle.
    fn close_credential_effects(&self, platform: &str, account: &str, reason: &str) -> Vec<String> {
        /// The credential a pending request is bound to, wherever the
        /// kind records it — `params.platform`/`params.account`, or
        /// `params.input.platform`/`params.input.account` for a
        /// brokered effect.
        fn bound_to(req: &Value, platform: &str, account: &str) -> bool {
            let at = |base: &Value| {
                base["platform"].as_str() == Some(platform)
                    && base["account"].as_str() == Some(account)
            };
            at(req) || at(&req["input"])
        }
        let mut closed = Vec::new();
        let drained: Vec<(String, String)> = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            let bound: Vec<String> = pending
                .iter()
                .filter(|(_, req)| {
                    req.params["kind"] == "effect" && bound_to(&req.params, platform, account)
                })
                .map(|(handle, _)| handle.clone())
                .collect();
            let mut out = Vec::new();
            for handle in bound {
                if let Some(req) = pending.remove(&handle) {
                    out.push((handle, req.alias));
                }
            }
            out
        };
        for (handle, alias) in drained {
            self.relax_waiting(&alias);
            let _ = self.store.event_public(
                &alias,
                "request_closed",
                json!({"request": handle, "reason": reason,
                       "by": "credential revoked"}),
            );
            closed.push(handle);
        }
        // The durable rows — the row's close_reason is the contract's
        // code; the operator's free-text reason rides the lane event
        // and the revoke's own audit event.
        let rows = self
            .store
            .platform_effects(None)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| {
                r.platform == platform
                    && r.account == account
                    && matches!(r.state.as_str(), "waiting" | "reconcile")
            })
            .map(|r| (r.request.clone(), r.effect_id.clone(), r.agent.clone()))
            .collect::<Vec<_>>();
        for (handle, effect_id, alias) in rows {
            if let Ok(Some(_)) = self
                .store
                .effect_close(crate::store::EffectKey::Id(effect_id), "credential_revoked")
            {
                let _ = self.store.event_public(
                    &alias,
                    "request_closed",
                    json!({"request": handle, "kind": "effect",
                           "reason": "credential_revoked", "detail": reason,
                           "by": "credential revoked"}),
                );
                closed.push(handle);
            }
        }
        if !closed.is_empty() {
            self.wake();
        }
        closed
    }

    /// `platform_outbox {effect_id?}` (CAD-546) — the `local`
    /// platform's publish ledger: every released `publish` lands an
    /// item under the outbox root. The operator's alone: the posts are
    /// already approved content, but the ledger's previews and paths
    /// are the operator's board's concern — the same
    /// `operator_connection` proof the custody verbs take, and the
    /// gate the board's `/api/outbox` relays through.
    pub(super) fn rpc_platform_outbox(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("platform outbox", params, peer_pid)?;
        let Some(outbox) = &self.outbox_dir else {
            return Err(Error::rejected(
                "this daemon runs no `local` platform — no outbox is configured",
            ));
        };
        let effect_id = optional_str(params, "effect_id").map(str::to_string);
        platform::local::list_items(outbox, effect_id.as_deref())
    }
}
