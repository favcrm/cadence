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

use super::custody_rpc::CustodyOperation;
use super::{optional_str, required_str, AgentCaller, Shared};
use crate::error::{Error, Result};
use crate::platform::{self, custody, Enrollment};
use crate::proto::identifier;
use crate::store::{scope_list, scope_name, CredentialRecord, Grant};

/// How the daemon records the actor on custody/grant writes — a
/// constant: every mutating verb gates on `operator_connection`
/// first, so the only `by` these rows can carry is the operator.
const OPERATOR: &str = "operator";

/// CAD-1420: the one platform whose `account` is a board HOST, not an
/// operator-chosen handle — the daemon's enrolled `hosted-board`
/// revalidation custody (`assist.revalidate`). The generic
/// `proto::identifier` grammar (`[a-z0-9-]`, ≤64) cannot spell a DNS
/// name with dots or a `localhost:port`, so `hosted-board` keeps its
/// own narrow account parser in `platform::connections`, shared with
/// store re-validation; every other platform keeps the generic grammar
/// unchanged.
const HOSTED_BOARD_PLATFORM: &str = "hosted-board";

/// The account parse for enroll/rotate/revoke: `hosted-board` admits
/// its board-host spelling via the shared
/// `platform::connections::provider_account_identifier`; every other
/// platform — and every other verb — takes the existing parser
/// unchanged.
fn enroll_account_identifier(platform: &str, value: &str) -> Result<String> {
    if platform == HOSTED_BOARD_PLATFORM {
        platform::connections::provider_account_identifier(platform, value)
    } else {
        identifier(value, "Account")
    }
}

/// CAD-1420: enroll/rotate of `hosted-board` binds the account to THIS
/// daemon's configured board host (`board-identity.json`), the same
/// trim the live pull applies. A hosted-board credential is
/// revalidation custody for the configured board only — an operator
/// can never enroll a foreign host's service token under it. Refuses
/// before custody or any record when the board identity is unconfigured
/// or the supplied host differs.
fn hosted_board_account_matches_config(
    state_dir: &std::path::Path,
    account: &str,
    verb: &str,
) -> Result<()> {
    let configured = crate::board_identity::read_config(state_dir)
        .map_err(|_| {
            Error::rejected(format!(
                "{verb} refused: no board identity is configured — there is no \
                 hosted-board host to enroll"
            ))
        })?
        .host;
    let configured = configured.trim_end_matches('/');
    if account != configured {
        return Err(Error::rejected(format!(
            "{verb} refused: hosted-board account '{account}' is not this board's \
             configured host '{configured}' — service custody enrolls only for \
             the board this daemon serves"
        )));
    }
    Ok(())
}

type CredentialMetadataProjection<'a> = dyn Fn(&CredentialRecord) -> Result<Value> + 'a;

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
        self.enroll_inner(params, false, None, None, None)
    }

    /// `platform_rotate {platform, account, ...}` — §5.3: re-enroll
    /// under the same handle; grants and the project default do not
    /// change. Rotation inherits the enroll-time custody acceptance —
    /// the exposure it refreshes was consented to already.
    pub(super) fn rpc_platform_rotate(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("platform rotate", params, peer_pid)?;
        self.enroll_inner(params, true, None, None, None)
    }

    pub(super) fn enroll_inner(
        &self,
        params: &Value,
        rotate: bool,
        expected_id: Option<&str>,
        allowed_scopes: Option<&[String]>,
        metadata_projection: Option<&CredentialMetadataProjection<'_>>,
    ) -> Result<Value> {
        let operation = self.custody_operation()?;
        self.enroll_inner_expected(
            &operation,
            params,
            rotate,
            expected_id,
            None,
            allowed_scopes,
            metadata_projection,
        )
    }

    /// `enroll_inner` with the caller's expected revision bound into
    /// the CAS (CAD-1128): for encrypted rotation/revoke the caller's
    /// `expected_revision`/`expected_connection_id` MUST reach the
    /// core transaction, never a silently reloaded later value.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn enroll_inner_expected(
        &self,
        operation: &CustodyOperation<'_>,
        params: &Value,
        rotate: bool,
        expected_id: Option<&str>,
        expected_revision: Option<u64>,
        allowed_scopes: Option<&[String]>,
        metadata_projection: Option<&CredentialMetadataProjection<'_>>,
    ) -> Result<Value> {
        let platform = if allowed_scopes.is_some() {
            platform::connections::provider_identifier(
                required_str(params, "platform")?,
                "Platform",
            )?
        } else {
            identifier(required_str(params, "platform")?, "Platform")?
        };
        let account = if allowed_scopes.is_some() {
            platform::connections::provider_account_identifier(
                &platform,
                required_str(params, "account")?,
            )?
        } else {
            enroll_account_identifier(&platform, required_str(params, "account")?)?
        };
        // CAD-1420: a `hosted-board` account IS this board's public
        // host — enroll/rotate refuses any other spelling, before
        // custody or a record. The declared scopes for that custody
        // are exactly `assist.revalidate`; no arbitrary service
        // capability enrolls under it. Revoke stays unbound to the
        // CURRENT host so a stale host's credential can still drop.
        if platform == HOSTED_BOARD_PLATFORM {
            hosted_board_account_matches_config(
                &self.state_dir,
                &account,
                if rotate {
                    "platform rotate"
                } else {
                    "platform enroll"
                },
            )?;
        }
        let accept_risk = params
            .get("accept_same_uid_risk")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // The caller retains custody across live identity, exchange and publication.
        // Existence is checked inside the lock before custody is
        // touched — an early `put` would leave bytes disagreeing with
        // the record's fingerprint. The store re-checks inside its
        // transaction, so these pre-checks only order the refusal
        // ahead of the write.
        let existing = self.store.platform_credential(&platform, &account)?;
        let encrypted_existing = existing.as_ref().is_some_and(|record| {
            record
                .custody
                .starts_with(crate::platform::encrypted_sqlite::TAG_PREFIX)
        });
        if rotate && encrypted_existing {
            if allowed_scopes.is_none()
                || optional_str(params, "shape") != Some(platform::smtp::ENROLLMENT_SHAPE)
                || existing.as_ref().map(|record| record.exchange.as_str())
                    != Some(platform::smtp::ENROLLMENT_SHAPE)
            {
                return Err(Error::invalid(
                    "unsupported_custody_use",
                    "encrypted rotation requires typed SMTP connection management",
                ));
            }
            if expected_id
                != existing
                    .as_ref()
                    .map(|record| record.connection_id.as_str())
                || expected_revision != existing.as_ref().map(|record| record.credential_revision)
            {
                return Err(Error::invalid(
                    "stale_revision",
                    "encrypted rotation requires the exact current connection and revision",
                ));
            }
        }
        if expected_id
            .is_some_and(|id| existing.as_ref().map(|r| r.connection_id.as_str()) != Some(id))
        {
            return Err(Error::rejected("connection incarnation is stale"));
        }
        if expected_revision
            .is_some_and(|rev| existing.as_ref().map(|r| r.credential_revision) != Some(rev))
        {
            return Err(Error::invalid(
                "stale_revision",
                "connection revision is stale — reload and re-approve",
            ));
        }
        let declared = match params.get("scopes") {
            Some(_) => scope_list(params, "scopes")?,
            None if rotate => existing
                .as_ref()
                .map(|r| r.scopes.clone())
                .ok_or_else(|| Error::rejected(format!(
                    "no credential is enrolled for {platform}/{account} — `platform enroll` it first"
                )))?,
            None => return Err(Error::rejected("Missing or non-array scopes")),
        };

        if allowed_scopes
            .is_some_and(|allowed| declared.iter().any(|scope| !allowed.contains(scope)))
        {
            return Err(Error::rejected(
                "connection scopes are not declared by reviewed capabilities",
            ));
        }
        // `hosted-board` custody is the daemon's `assist.revalidate`
        // service proof and nothing else: the declared scope set is
        // exactly that one scope, on enroll and on rotate alike.
        if platform == HOSTED_BOARD_PLATFORM && declared != ["assist.revalidate"] {
            return Err(Error::rejected(
                "hosted-board enrollment carries exactly the assist.revalidate scope",
            ));
        }
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
        // CAD-1128 custody selection. `encrypted` is true only when
        // the vault is configured AND this write belongs to it: a new
        // SMTP-shape enrollment through the connection path, or a
        // rotation of a record already vault-backed. Mixed records
        // resolve their own durable backend tag — a legacy record
        // stays legacy, never silently migrates. Under a configured
        // daemon, a NEW credential-bearing non-SMTP enrollment refuses
        // before any exchange or write; credential-less builtins and
        // legacy rotations are untouched.
        let encrypted = match self.platform_custody.encrypted() {
            Some(_) if rotate => encrypted_existing,
            Some(_) => {
                optional_str(params, "shape") == Some(platform::smtp::ENROLLMENT_SHAPE)
                    && allowed_scopes.is_some()
            }
            None => false,
        };
        if self.platform_custody.encrypted().is_some() && !rotate && !encrypted {
            // A fresh enrollment under the configured vault. Builtin
            // credential-less accounts never reach enroll_inner, so a
            // non-SMTP write here is a credential-bearing enrollment
            // the configured backend refuses — before any exchange.
            return Err(Error::invalid(
                "unsupported_custody_use",
                "this daemon's custody is encrypted — only SMTP-shape enrollments are supported",
            ));
        }
        if encrypted {
            self.refuse_hosted_encrypted()?;
        }
        let enrollment: Enrollment = match optional_str(params, "shape").unwrap_or("token") {
            "token" => platform::enroll_token(params, &declared)?,
            "consent" => platform::enroll_consent(&platform, &account, &declared, params)?,
            // CAD-785: the typed SMTP sender enrolls only through
            // the provider-neutral connection path — the legacy
            // platform verbs have no typed transport grammar and
            // must not mint sender custody.
            "smtp" if allowed_scopes.is_some() => {
                platform::enroll_smtp(params, &declared)?
            }
            other => {
                return Err(Error::rejected(format!(
                    "unknown exchange shape '{other}' — 'token', 'consent', or 'smtp' via connection management"
                )))
            }
        };
        if encrypted {
            let (envelope, projection) = platform::smtp::custody_decode(&enrollment.bytes)?;
            platform::refuse_leak(
                "SMTP enrollment projection",
                &projection.to_json().to_string(),
                envelope.secret(),
            )?;
        }
        platform::refuse_leak(
            "platform enrollment metadata",
            &json!({"platform":platform,"account":account,"scopes":enrollment.scopes}).to_string(),
            enrollment.screen_bytes(),
        )?;
        let fingerprint = crate::secret::fingerprint(&enrollment.bytes);
        let mut record = CredentialRecord {
            connection_id: existing
                .as_ref()
                .map(|r| r.connection_id.clone())
                .unwrap_or_else(|| format!("conn-{}", uuid::Uuid::new_v4().simple())),
            credential_revision: existing
                .as_ref()
                .map(|r| {
                    r.credential_revision
                        .checked_add(1)
                        .filter(|revision| *revision <= i64::MAX as u64)
                        .ok_or_else(|| Error::rejected("connection revision is exhausted"))
                })
                .transpose()?
                .unwrap_or(1),
            platform,
            account,
            scopes: enrollment.scopes.clone(),
            fingerprint,
            custody: self.platform_custody.tag().to_string(),
            exchange: enrollment.exchange.to_string(),
            enrolled_at: crate::issue::time::now_epoch() as f64,
            by: OPERATOR.to_string(),
        };
        platform::refuse_leak(
            "platform credential metadata",
            &record.to_json().to_string(),
            enrollment.screen_bytes(),
        )?;
        platform::refuse_leak("platform credential audit",&json!({"record":record.to_json(),"old":existing.as_ref().map(CredentialRecord::to_json),"rotated":rotate,"custody_risk_accepted":risk,"response":{"state":"enrolled","account":record.to_json()},"event_kinds":["platform_connected","credential_revoked","platform_disconnected"],"stream":"audit:platforms"}).to_string(),enrollment.screen_bytes())?;
        if let Some(project) = metadata_projection {
            platform::refuse_leak(
                "connection metadata",
                &project(&record)?.to_string(),
                enrollment.screen_bytes(),
            )?;
        }
        if encrypted {
            // CAD-1128 vault-first: commit the immutable ciphertext row
            // under the exact proposed core identity/revision BEFORE
            // core publishes the reference. A refused core write leaves
            // a non-authoritative orphan — never overwritten, never a
            // credential. `key_id` binds this daemon's configured key.
            let vault = self
                .platform_custody
                .encrypted()
                .ok_or_else(|| Error::internal("encrypted custody vanished"))?;
            let key_id = vault
                .key_id()
                .ok_or_else(|| Error::invalid("locked", "custody is locked"))?;
            let expected = crate::platform::encrypted_sqlite::ExpectedIdentity {
                workspace_id: self.store.connection_workspace_id()?,
                connection_id: record.connection_id.clone(),
                provider: record.platform.clone(),
                account: record.account.clone(),
                credential_revision: record.credential_revision,
                exchange: record.exchange.clone(),
                key_id,
            };
            record.custody = vault.write_row(&expected, &enrollment.bytes)?;
            if let Err(err) = self.store.platform_enroll(&record, rotate, risk) {
                // Contract I4: no ciphertext compensation — the orphan
                // stays, non-authoritative until a fresh attempt.
                return Err(self.encrypted_core_error(err));
            }
        } else {
            let key = custody::Key {
                platform: &record.platform,
                account: &record.account,
            };
            // A rotate captures the old bytes first: if the record write
            // fails, custody is put back exactly as it was — never left
            // holding bytes the record disowns. A load failure refuses the
            // rotate before anything is overwritten.
            let prior = match &existing {
                Some(record) => Some(self.platform_custody.load(&record.custody, &key)?),
                None => None,
            };
            self.platform_custody.put_legacy(&key, &enrollment.bytes)?;
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
                        let _ = self.platform_custody.put_legacy(&key, old);
                    }
                    None => {
                        let _ = self.platform_custody.remove(&record.custody, &key);
                    }
                }
                return Err(err);
            }
        }
        // The result and its event carry handles only — prove it: a
        // byte leak here is a withheld response, never a quiet one.
        let result = record.to_json();
        platform::refuse_leak(
            "platform enroll result",
            &result.to_string(),
            enrollment.screen_bytes(),
        )?;
        let connection = if allowed_scopes.is_some() {
            Some(self.connection_record_admitted(&record, operation)?)
        } else {
            None
        };
        Ok(json!({"state": "enrolled", "account": result, "connection": connection}))
    }

    /// `platform_revoke {platform, account, reason?}` — the operator
    /// disconnects an account: custody drops the bytes, the record
    /// and its grants go, pending effects bound to the credential
    /// close unanswered with the named reason (§5.3 — the hook
    /// CAD-506 completes), and the audit events land in one
    /// transaction.
    pub(super) fn rpc_platform_revoke(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("platform revoke", params, peer_pid)?;
        self.revoke_inner(params, None)
    }

    pub(super) fn revoke_inner(&self, params: &Value, expected_id: Option<&str>) -> Result<Value> {
        let caller_id = params
            .get("expected_connection_id")
            .map(|value| {
                value.as_str().ok_or_else(|| {
                    Error::invalid("stale_revision", "expected_connection_id must be a string")
                })
            })
            .transpose()?;
        let expected_revision = params
            .get("expected_revision")
            .map(|value| {
                value
                    .as_u64()
                    .filter(|revision| (1..=i64::MAX as u64).contains(revision))
                    .ok_or_else(|| {
                        Error::invalid(
                            "stale_revision",
                            "expected_revision must be a valid revision",
                        )
                    })
            })
            .transpose()?;
        if expected_id
            .zip(caller_id)
            .is_some_and(|(expected, caller)| expected != caller)
        {
            return Err(Error::rejected("connection incarnation is stale"));
        }
        self.revoke_inner_expected(params, caller_id.or(expected_id), expected_revision)
    }

    /// `revoke_inner` carrying the caller's expected revision CAS
    /// (CAD-1128): an encrypted revoke binds both exact incarnation
    /// and revision, refuses caller-authored reason text outright
    /// (`reason_unscreenable`), and closes only waiting/decided
    /// generic rows inside the same guarded core transaction —
    /// executing/reconcile rows stay uncertain, never drained early.
    pub(super) fn revoke_inner_expected(
        &self,
        params: &Value,
        expected_id: Option<&str>,
        expected_revision: Option<u64>,
    ) -> Result<Value> {
        let platform = if expected_id.is_some() {
            platform::connections::provider_identifier(
                required_str(params, "platform")?,
                "Platform",
            )?
        } else {
            identifier(required_str(params, "platform")?, "Platform")?
        };
        let account = if expected_id.is_some() {
            platform::connections::provider_account_identifier(
                &platform,
                required_str(params, "account")?,
            )?
        } else {
            // CAD-1420: the `hosted-board` account is the board's host
            // spelling. Revoke validates the grammar only — it must NOT
            // pin the CURRENT configured host, or a credential enrolled
            // under a previous host could never be revoked.
            enroll_account_identifier(&platform, required_str(params, "account")?)?
        };
        let reason = optional_str(params, "reason");
        // Same operation section as enrollment and supported send.
        let _custody_guard = self.custody_operation()?;
        let record = self
            .store
            .platform_credential(&platform, &account)?
            .ok_or_else(|| {
                Error::rejected(format!(
                    "no credential is enrolled for {platform}/{account}"
                ))
            })?;
        if expected_id.is_some_and(|id| record.connection_id != id) {
            return Err(Error::rejected("connection incarnation is stale"));
        }
        if expected_revision.is_some_and(|rev| record.credential_revision != rev) {
            return Err(Error::invalid(
                "stale_revision",
                "connection revision is stale — reload and re-approve",
            ));
        }
        let encrypted = record
            .custody
            .starts_with(crate::platform::encrypted_sqlite::TAG_PREFIX);
        if encrypted && (expected_id.is_none() || expected_revision.is_none()) {
            return Err(Error::invalid(
                "stale_revision",
                "an encrypted revoke requires exact expected_connection_id and expected_revision",
            ));
        }
        // CAD-1128 R3: an encrypted record's caller-supplied reason is
        // unscreenable — refuse it outright rather than echo/persist it.
        if encrypted && reason.is_some() {
            return Err(Error::invalid(
                "reason_unscreenable",
                "an encrypted record carries no caller-authored revoke reason",
            ));
        }
        let key = custody::Key {
            platform: &platform,
            account: &account,
        };
        // `reason` is operator text that lands on audit events and the
        // agent-visible `request_closed` — screen it against the bytes
        // it must never carry before the drain publishes it. A custody
        // that no longer reads has nothing left to leak. Encrypted
        // records are never caller-reasoned (above) and carry no
        // decrypted bytes here, so no screen runs for them.
        let bytes = if encrypted {
            None
        } else {
            self.platform_custody.load(&record.custody, &key).ok()
        };
        if reason.is_some() && !encrypted && bytes.is_none() {
            return Err(Error::invalid(
                "reason_unscreenable",
                "credential unavailable; omit reason",
            ));
        }
        if let (Some(reason), Some(bytes)) = (reason, bytes.as_deref()) {
            platform::refuse_leak("platform revoke reason", reason, bytes)?;
        }
        // Bytes first: a revoke that cannot drop custody refuses
        // before the record goes — the record without custody still
        // fails closed at load. The encrypted arm is an acknowledged
        // no-op: ciphertext is retained, never compensated.
        self.platform_custody.remove(&record.custody, &key)?;
        // Contract: the encrypted revoke retires authority AND closes
        // provably-unexecuted waiting/decided rows in ONE guarded core
        // transaction (`platform_revoke_encrypted`) — `close_credential_effects`
        // is deliberately not used, it drains reconcile rows and memory
        // before the commit lands. A legacy revoke keeps its existing
        // helper + `platform_revoke` order.
        let revoked = if encrypted {
            self.store.platform_revoke_encrypted(
                &platform,
                &account,
                OPERATOR,
                expected_id.unwrap_or(&record.connection_id),
                expected_revision.unwrap_or(record.credential_revision),
            )
        } else {
            let closed = self.close_credential_effects(
                &platform,
                &account,
                reason.unwrap_or("credential revoked"),
            );
            self.store
                .platform_revoke(&platform, &account, OPERATOR, reason, &closed)
                .map(|opt| opt.map(|(r, g)| (r, g, closed)))
        };
        let Some((record, grants, closed)) = (match revoked {
            Err(err) => {
                if encrypted {
                    return Err(self.encrypted_core_error(err));
                }
                // The record stayed — custody must describe it again:
                // put the bytes back (best effort; the record fails
                // closed at load if the restore cannot land either).
                if let Some(bytes) = bytes {
                    let _ = self.platform_custody.put_legacy(&key, &bytes);
                }
                return Err(err);
            }
            Ok(v) => v,
        }) else {
            return Err(Error::rejected(format!(
                "no credential is enrolled for {platform}/{account}"
            )));
        };
        // Only a confirmed encrypted core commit authorizes the pending drain.
        // Never hold C or a DB borrow while touching pending/notifications.
        drop(_custody_guard);
        if encrypted {
            self.finish_encrypted_revoke(&closed);
        }
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

    fn finish_encrypted_revoke(&self, closed: &[String]) {
        for id in closed {
            // Closed IDs came from the guarded transaction's exactly-one-row
            // updates. Re-read only immutable routing; never close a reconcile,
            // executing or newly queued row merely because its account matches.
            let Ok(Some(row)) = self.store.effect_by_id(id) else {
                continue;
            };
            if row.state != "closed" || row.close_reason.as_deref() != Some("credential_revoked") {
                continue;
            }
            let removed = {
                let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
                if pending.get(&row.request).is_some_and(|request| {
                    request.alias == row.agent && request.params["kind"] == "effect"
                }) {
                    pending.remove(&row.request).is_some()
                } else {
                    false
                }
            };
            if removed {
                self.relax_waiting(&row.agent);
            }
            let _ = self.store.event_public(
                &row.agent,
                "request_closed",
                json!({"request": row.request, "kind": "effect", "reason": "credential_revoked"}),
            );
        }
    }

    fn encrypted_core_error(&self, error: Error) -> Error {
        if error.code() == Some("outcome_unknown") || error.kind() == "internal" {
            if let Some(vault) = self.platform_custody.encrypted() {
                vault.fence();
            }
            return Error::invalid(
                "outcome_unknown",
                "encrypted authority publication requires reconciliation",
            );
        }
        match error.code() {
            Some("busy") => Error::busy_coded("busy", "core authority writer busy; no publication"),
            Some("stale_revision") => {
                Error::invalid("stale_revision", "connection revision is stale")
            }
            _ => Error::invalid("storage_unavailable", "core authority write refused"),
        }
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
                        self.turn_project(&agent_row)
                            .map(|turn| turn.key)
                            .ok_or_else(|| {
                                Error::rejected(format!(
                                    "'{agent}' has no project: no running task and its cwd \
                                     is in no project — pass 'account' or 'project'"
                                ))
                            })?
                    }
                };
                match self.store.platform_default(&project, &platform)? {
                    Some(d) => d.account,
                    // The built-in account is always available (CAD-577).
                    None if platform::is_builtin(&platform, platform::BUILTIN_LOCAL_ACCOUNT) => {
                        platform::BUILTIN_LOCAL_ACCOUNT.to_string()
                    }
                    None => {
                        return Err(Error::rejected(format!(
                            "project '{project}' names no default account for '{platform}'"
                        )))
                    }
                }
            }
        };
        // §5.3: a refusal names the missing scope and happens before
        // any platform traffic — the check touches no custody.
        let grant = platform::require_grant(&self.store, &agent, &platform, &account, scope)?;
        Ok(json!({"granted": true, "grant": grant.to_json()}))
    }

    /// `platform_accounts` — the enrolled handles (§5.3's record:
    /// platform, account, scopes, fingerprint, enrolled_at, by), open
    /// to every socket reader. The built-in `local/local` account is
    /// always listed (CAD-577) — no enrollment, `custody: "built-in"`,
    /// so `platform accounts` shows it beside the real ones.
    pub(super) fn rpc_platform_accounts(&self, _params: &Value) -> Result<Value> {
        let mut accounts = vec![json!({
            "platform": crate::platform::local::PLATFORM,
            "account": crate::platform::BUILTIN_LOCAL_ACCOUNT,
            "scopes": ["publish"],
            "fingerprint": Value::Null,
            "custody": "built-in",
            "exchange": "built-in",
            "enrolled_at": Value::Null,
            "by": "built-in",
        })];
        accounts.extend(
            self.store
                .platform_credentials()?
                .iter()
                .map(CredentialRecord::to_json),
        );
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
