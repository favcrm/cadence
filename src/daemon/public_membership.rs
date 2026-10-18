//! CAD-1374: the central public-authority gate. Every public-session
//! authority decision (a board request, a human receipt, an owner or member
//! claim) passes here, so a withdrawn or demoted platform membership cannot
//! be read from a cached `Auth` row.
//!
//! Mode is captured ONCE at startup. Native means the platform placed this
//! daemon (a core `AGENTICOS_COMPANY`/`AGENTICOS_INSTANCE` marker is present,
//! or `hosted.lease` is the accepted `http://lease.internal` endpoint). A
//! partial or invalid native configuration is `RequiredUnavailable`: it
//! never falls back to cached local authority.

use serde_json::json;

use super::*;
use crate::board_identity::Role;
use crate::error::{Error, Result};
use crate::operator_auth::{PublicRealm, PublicSnapshot, SessionView};

const ENV_COMPANY: &str = "AGENTICOS_COMPANY";
const ENV_INSTANCE: &str = "AGENTICOS_INSTANCE";
const ENV_BOARD_HOST: &str = "AGENTICOS_BOARD_HOST";
const ENV_BOARD_ISSUER: &str = "AGENTICOS_BOARD_ISSUER";
const ENV_BOARD_COMPANY: &str = "AGENTICOS_BOARD_COMPANY";

/// The platform-placed membership context, captured once at startup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MembershipMode {
    /// No platform placement: the operator's local sessions only. A public
    /// row still needs a saved realm that matches the current board config.
    Standalone,
    /// Platform-placed. Authority is re-proven fresh against the platform.
    Native(NativeContext),
    /// Native placement was signalled but its context is partial or invalid.
    /// Every public authority decision is refused.
    RequiredUnavailable,
}

/// The immutable native binding: the paired core identity and the board realm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeContext {
    pub company: String,
    pub instance: String,
    pub realm: PublicRealm,
}

/// The outcome of a central authority check. `Granted` carries the view
/// with its role set to the EFFECTIVE role, so every caller reads one answer.
pub(crate) enum Authority {
    Granted {
        sub: String,
        role: Role,
        hash: String,
        realm: PublicRealm,
        display: String,
        view: Box<SessionView>,
    },
    /// Not authoritative: unknown, revoked, expired, userless, realm-mismatched,
    /// or the platform answered no membership (and the row was tombstoned).
    Denied,
}

/// The accepted native lease endpoints. Any other `hosted.lease` value is
/// not a native signal here; `lease::acquire` owns its own validation.
fn is_native_lease(raw: &str) -> bool {
    matches!(
        raw,
        "http://lease.internal" | "http://lease.internal/" | "http://lease.internal/renew"
    )
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty())
}

fn company_ok(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn instance_ok(s: &str) -> bool {
    (1..=12).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
}

/// The same view with its user's role set to the effective role. Only the
/// returned value carries it; nothing is persisted.
fn with_effective_role(mut view: SessionView, role: Role) -> SessionView {
    if let Some(user) = view.user.as_mut() {
        user.role = role.as_str().to_string();
    }
    view
}

impl MembershipMode {
    /// Capture the mode from the same `Hosted` value the lease was acquired
    /// with, plus the daemon's env reader. Read once; never refreshed.
    /// Any PRESENT core marker signals native, even an empty one.
    pub(crate) fn capture(
        hosted: &crate::lease::Hosted,
        read: &dyn Fn(&str) -> Option<String>,
    ) -> Self {
        let raw_company = read(ENV_COMPANY);
        let raw_instance = read(ENV_INSTANCE);
        let native_lease = hosted.lease.as_deref().is_some_and(is_native_lease);
        if raw_company.is_none() && raw_instance.is_none() && !native_lease {
            return Self::Standalone;
        }
        let (Some(company), Some(instance)) = (nonempty(raw_company), nonempty(raw_instance))
        else {
            return Self::RequiredUnavailable;
        };
        if !company_ok(&company) || !instance_ok(&instance) {
            return Self::RequiredUnavailable;
        }
        let (Some(host), Some(issuer), Some(board_company)) = (
            nonempty(read(ENV_BOARD_HOST)),
            nonempty(read(ENV_BOARD_ISSUER)),
            nonempty(read(ENV_BOARD_COMPANY)),
        ) else {
            return Self::RequiredUnavailable;
        };
        if board_company != company {
            return Self::RequiredUnavailable;
        }
        Self::Native(NativeContext {
            company: company.clone(),
            instance,
            realm: PublicRealm {
                issuer,
                company,
                host,
            },
        })
    }
}

impl Shared {
    /// The one authority decision for a public session named by its raw
    /// cookie. Touches the idle clock exactly as `check_public` always has,
    /// then snapshots the row. No `Auth` lock is held across the lookup.
    pub(super) fn current_public_session(&self, token: &str) -> Result<Authority> {
        let now = self.operator_now();
        let snapshot = {
            let mut auth = self.operator_auth();
            if auth.check_public(token, now)?.is_none() {
                return Ok(Authority::Denied);
            }
            auth.public_snapshot(token, now)
        };
        match snapshot {
            Some(snapshot) => self.current_public_hash_from(snapshot),
            None => Ok(Authority::Denied),
        }
    }

    /// The same decision for a known FULL credential hash (a live cap). It
    /// does not touch the idle clock, as the keyed liveness check never did.
    pub(super) fn current_session_for_hash(&self, hash: &str) -> Result<Authority> {
        let snapshot = self
            .operator_auth()
            .public_snapshot_by_hash(hash, self.operator_now());
        match snapshot {
            Some(snapshot) => self.current_public_hash_from(snapshot),
            None => Ok(Authority::Denied),
        }
    }

    /// The saved realm must equal the CURRENT board config, so a changed
    /// `ui.json` or config cannot rebind an old cookie. Native also requires
    /// it to equal the captured startup realm.
    fn current_realm(&self) -> Result<PublicRealm> {
        let config = crate::board_identity::read_config(&self.state_dir)?;
        Ok(PublicRealm {
            issuer: config.issuer,
            company: config.company,
            host: config.host,
        })
    }

    fn current_public_hash_from(&self, snap: PublicSnapshot) -> Result<Authority> {
        let PublicSnapshot {
            hash,
            view,
            realm,
            revoked,
        } = snap;
        if revoked || view.expires_at <= self.operator_now() {
            return Ok(Authority::Denied);
        }
        let Some(user) = view.user.clone() else {
            return Ok(Authority::Denied);
        };
        // A row without a saved realm (legacy, or written before the realm
        // existed) names no company, so it cannot grant anything.
        let Some(saved) = realm else {
            return Ok(Authority::Denied);
        };
        if saved != self.current_realm()? {
            return Ok(Authority::Denied);
        }
        let native = match &self.membership_mode {
            MembershipMode::RequiredUnavailable => return Ok(Authority::Denied),
            MembershipMode::Standalone => None,
            MembershipMode::Native(ctx) => {
                if saved != ctx.realm {
                    return Ok(Authority::Denied);
                }
                Some(ctx.clone())
            }
        };
        let Some(ctx) = native else {
            // Standalone: no private check. The saved identity stands, with
            // its stored role, which is what it always was.
            let role = if user.is_operator() {
                Role::Operator
            } else {
                Role::Member
            };
            return Ok(Authority::Granted {
                sub: user.sub.clone(),
                role,
                hash,
                realm: saved,
                display: user.actor(),
                view: Box::new(with_effective_role(view, role)),
            });
        };
        // CAD-XXXX test seam: when a `#[cfg(test)]` driver installed a
        // synthetic lookup on `Shared`, resolve the fresh platform role
        // through it so the real mapping below sees the injected outcome
        // verbatim. Production builds carry no field and always call the
        // private `api.internal` transport.
        #[cfg(test)]
        let fresh = match &self.membership_lookup_for_test {
            Some(lookup) => lookup(&hash, &user.sub, &ctx.company, &ctx.instance),
            None => crate::board_membership::lookup_current_membership_by_digest(
                &hash,
                &user.sub,
                &ctx.company,
                &ctx.instance,
                &self.board_jwks,
            ),
        };
        #[cfg(not(test))]
        let fresh = crate::board_membership::lookup_current_membership_by_digest(
            &hash,
            &user.sub,
            &ctx.company,
            &ctx.instance,
            &self.board_jwks,
        );
        // A transient outage is unavailable, not evidence that the session
        // was revoked. Propagate it without granting cached authority.
        let fresh = fresh?;
        let Some(fresh_role) = fresh else {
            self.revoke_public_authority(&hash, &view, &user.sub, &saved)?;
            return Ok(Authority::Denied);
        };
        // Re-lock after the network: the same row must still be live,
        // unrevoked, bound to the same realm and subject, and the config must
        // still be the saved one. A logout or sign-in during the lookup denies.
        let now = self.operator_now();
        let Some(after) = self.operator_auth().public_snapshot_by_hash(&hash, now) else {
            return Ok(Authority::Denied);
        };
        if after.hash != hash || after.revoked || after.realm.as_ref() != Some(&saved) {
            return Ok(Authority::Denied);
        }
        let Some(after_user) = after.view.user.clone() else {
            return Ok(Authority::Denied);
        };
        if after_user.sub != user.sub || self.current_realm()? != saved {
            return Ok(Authority::Denied);
        }
        // Clamp against the LATEST stored baseline and the fresh platform role:
        // owner only when both say owner. A stored member is never raised, and
        // a demoted owner is a member.
        let role = if fresh_role.is_operator() && after_user.is_operator() {
            Role::Operator
        } else {
            Role::Member
        };
        Ok(Authority::Granted {
            sub: after_user.sub.clone(),
            role,
            hash,
            realm: saved,
            display: after_user.actor(),
            view: Box::new(with_effective_role(after.view, role)),
        })
    }

    /// Local residue of a native public decision whose central membership
    /// lookup already finished outside PM, custody and release guards.
    /// Re-reads the cookie row and current realm only: no membership POST
    /// and no JWKS fetch. `fresh_operator` is that central decision; the
    /// stored role can withhold operator authority and cannot grant it.
    pub(super) fn public_session_still(
        &self,
        token: &str,
        fresh_hash: &str,
        fresh_sub: &str,
        fresh_realm: &PublicRealm,
        fresh_operator: bool,
    ) -> Result<bool> {
        if !fresh_operator {
            return Ok(false);
        }
        let MembershipMode::Native(native) = &self.membership_mode else {
            return Ok(false);
        };
        if native.realm != *fresh_realm {
            return Ok(false);
        }
        let now = self.operator_now();
        let snapshot = {
            let mut auth = self.operator_auth();
            if auth.check_public(token, now)?.is_none() {
                return Ok(false);
            }
            auth.public_snapshot(token, now)
        };
        let Some(after) = snapshot else {
            return Ok(false);
        };
        if after.hash != fresh_hash || after.revoked || after.realm.as_ref() != Some(fresh_realm) {
            return Ok(false);
        }
        let Some(user) = after.view.user else {
            return Ok(false);
        };
        if user.sub != fresh_sub || after.view.expires_at <= now || !user.is_operator() {
            return Ok(false);
        }
        Ok(self.current_realm()? == *fresh_realm)
    }

    /// CAD-1374: the fresh freshness fence for an approved run's human. The
    /// sealed binding (company, sub, session reference) must still name a live
    /// public row whose fresh central decision is the owner of that company.
    /// Revoked, demoted, outage, changed realm or a partial binding refuses;
    /// the stored role is never returned as authority. The auth lock is not
    /// held across the platform lookup.
    pub(super) fn fresh_run_human(&self, sealed: &Value) -> Result<()> {
        let revoked = || {
            Error::gate_coded(
                "human_authority_revoked",
                "the approving owner's board session is no longer current; new privileged dispatch is refused",
            )
        };
        let (Some(sub), Some(company), Some(session)) = (
            sealed["sub"].as_str(),
            sealed["company"].as_str(),
            sealed["session"].as_str(),
        ) else {
            return Err(revoked());
        };
        let now = self.operator_now();
        let snapshot = self
            .operator_auth()
            .public_snapshot_by_session_id(session, now);
        let Some(snapshot) = snapshot else {
            return Err(revoked());
        };
        match self.current_public_hash_from(snapshot)? {
            Authority::Granted {
                sub: fresh_sub,
                role: Role::Operator,
                realm,
                view,
                ..
            } if fresh_sub == sub && realm.company == company && view.id == session => Ok(()),
            // A fresh decision that names a member or another identity is a
            // proved absence of the owner binding.
            Authority::Granted { .. } => Err(revoked()),
            // Denied is an unproved refusal: authority is not proven, and it
            // is not asserted as revoked. Lookup outages propagate above.
            Authority::Denied => Err(Error::gate_coded(
                "human_authority_unavailable",
                "the approving owner's current board authority cannot be proven; new privileged dispatch is refused",
            )),
        }
    }

    /// Write the durable revocation intent BEFORE the tombstone. A failed
    /// audit write returns `Err`, so the row stays authoritative and the
    /// caller refuses. Only static reasons and mapped identifiers are stored.
    fn revoke_public_authority(
        &self,
        hash: &str,
        view: &SessionView,
        sub: &str,
        realm: &PublicRealm,
    ) -> Result<()> {
        let request = uuid::Uuid::new_v4().to_string();
        self.store.event_public(
            DAEMON_ALIAS,
            "public_authority_revoked",
            json!({
                "schema_version": 1,
                "at": crate::issue::time::now_epoch(),
                "company": realm.company,
                "human": null,
                "actor": "SYSTEM",
                "actor_kind": "system",
                "action": "public_authority_revoked",
                "resource": format!("session:{}", view.id),
                "outcome": "prepared",
                "reason": "membership_withdrawn",
                "request_id": request,
                "correlation_id": request,
                "causation_id": null,
                "target_user": sub,
            }),
        )?;
        // CAD-1423: a withdrawn membership also ends that session's push consents.
        self.store.push_consents_revoke_session(hash)?;
        self.operator_auth()
            .revoke_public(hash, self.operator_now())?;
        Ok(())
    }

    /// For a full-hash cap: `None` when the hash is not a public row (the
    /// caller keeps its keyed check). `Some(true)` when the row is currently
    /// an owner by a fresh central decision, or a current member and the
    /// caller passed `allow_member` (CAD-1537: only for a read-only screen
    /// cap or context). Every other public outcome, including a member
    /// without `allow_member`, a removed or revoked row, a standalone
    /// refusal, and an outage, is `Some(false)`, with no local fallback.
    pub(super) fn public_row_hash(&self, hash: &str, allow_member: bool) -> Option<bool> {
        self.operator_auth()
            .public_snapshot_by_hash(hash, self.operator_now())?;
        Some(match self.current_session_for_hash(hash) {
            Ok(Authority::Granted {
                role: Role::Operator,
                ..
            }) => true,
            Ok(Authority::Granted {
                role: Role::Member, ..
            }) => allow_member,
            _ => false,
        })
    }

    /// Refuse a board realm that does not match the captured native realm
    /// before a session opens. Standalone has no captured realm to match.
    pub(super) fn require_realm_matches_mode(&self, realm: &PublicRealm) -> Result<()> {
        match &self.membership_mode {
            MembershipMode::Standalone => Ok(()),
            MembershipMode::Native(ctx) if ctx.realm == *realm => Ok(()),
            _ => Err(Error::invalid(
                "capability_unavailable",
                "the board realm does not match this platform-placed daemon",
            )),
        }
    }

    /// Does a live public member named `handle` hold a current membership?
    /// Each candidate is re-proven fresh; a cached row alone never answers
    /// yes. Any refusal or outage answers no.
    pub(super) fn handle_has_current_member(&self, handle: &str) -> Result<bool> {
        let now = self.operator_now();
        let candidates = self
            .operator_auth()
            .public_snapshots_for_handle(handle, now);
        for snap in candidates {
            if let Ok(Authority::Granted {
                role: Role::Member, ..
            }) = self.current_public_hash_from(snap)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::{ServeOptions, Shared};
    use crate::operator_auth::BoardUser;
    use std::sync::Arc;

    const COMPANY: &str = "co_outage";
    const INSTANCE: &str = "inst0001";
    const ISSUER: &str = "http://127.0.0.1:3999";
    const HOST: &str = "outage.board.localhost:3111";

    fn realm() -> PublicRealm {
        PublicRealm {
            issuer: ISSUER.to_string(),
            company: COMPANY.to_string(),
            host: HOST.to_string(),
        }
    }

    /// A `Shared` captured in `Native` mode (native markers present, no
    /// lease), serving `realm()`, with one live bound public session row
    /// for `sub`/`role`. The closure is installed as the test-only
    /// membership lookup, so `current_public_session` resolves the fresh
    /// platform answer through it instead of `api.internal`.
    fn native_shared(
        dir: &std::path::Path,
        sub: &str,
        role: &str,
        lookup: impl Fn(&str, &str, &str, &str) -> Result<Option<Role>> + Send + Sync + 'static,
    ) -> (Arc<Shared>, String) {
        crate::board_identity::write_config(
            dir,
            &crate::board_identity::Config {
                host: HOST.to_string(),
                issuer: ISSUER.to_string(),
                company: COMPANY.to_string(),
            },
        )
        .unwrap();
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", dir.join("no-pm").to_str().unwrap());
        for (name, value) in [
            (ENV_COMPANY, COMPANY),
            (ENV_INSTANCE, INSTANCE),
            (ENV_BOARD_HOST, HOST),
            (ENV_BOARD_ISSUER, ISSUER),
            (ENV_BOARD_COMPANY, COMPANY),
        ] {
            opts.provider_env.set(name, value);
        }
        let mut shared = Shared::new(dir, &opts).unwrap_or_else(|e| panic!("Shared::new: {e}"));
        Arc::get_mut(&mut shared)
            .expect("fresh Shared has one Arc")
            .membership_lookup_for_test = Some(Box::new(lookup));
        let opened = shared
            .operator_auth()
            .open_public_bound(
                BoardUser {
                    sub: sub.to_string(),
                    email: format!("{sub}@example.test"),
                    name: sub.to_string(),
                    role: role.to_string(),
                    handle: sub.to_string(),
                },
                "outage-jti",
                shared.operator_now() + 60,
                "outage-test",
                realm(),
                shared.operator_now(),
            )
            .unwrap()
            .expect("fresh jti opens a public session");
        (shared, opened.token)
    }

    /// CAD-XXXX regression: a native-mode membership lookup outage must NOT
    /// collapse into `Authority::Denied` — the indistinguishable answer an
    /// authoritative "no membership" also gives. On `board_session_check`
    /// `Denied` becomes `{valid:false}` (the board bounces to sign-in, 401),
    /// while a lookup `Err` must surface as an error (the board's
    /// `daemon_unavailable` 503). `Ok(None)` — an authenticated absence —
    /// stays `Denied`; only the outage is distinct.
    ///
    /// Driven through the real `current_public_session` →
    /// `current_public_hash_from` mapping over a real stored session, with
    /// only the network lookup result injected (test-only seam). RED on the
    /// deployed build: it maps `Err(_) => Ok(Authority::Denied)`.
    #[test]
    fn lookup_outage_is_unavailable_not_authoritative_denied() {
        let dir = tempfile::tempdir().unwrap();
        let (shared, token) = native_shared(dir.path(), "outage-user", "member", |_, _, _, _| {
            Err(Error::invalid(
                "capability_unavailable",
                "board membership could not be confirmed",
            ))
        });
        let answered = shared.current_public_session(&token);
        let error = match answered {
            Err(error) => error,
            Ok(_) => panic!(
                "a membership lookup outage must surface as an unavailable error \
                 (board answers 503), not collapse into Denied → invalid session"
            ),
        };
        assert_eq!(
            error.code(),
            Some("capability_unavailable"),
            "the outage error must be a capability_unavailable refusal, not \
             a session rejection: {error}"
        );
    }

    /// Control for the outage case: an AUTHENTICATED absence — the platform
    /// answered, `role: null` — is the real `Denied`, an invalid session
    /// that must not be mistaken for an outage. The tombstone path writes
    /// `public_authority_revoked` and revokes the row; both stay exercised.
    #[test]
    fn authenticated_absence_is_still_denied_not_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let (shared, token) =
            native_shared(dir.path(), "gone-user", "member", |_, _, _, _| Ok(None));
        let answered = shared.current_public_session(&token).unwrap();
        assert!(
            matches!(answered, Authority::Denied),
            "an authenticated absence must stay Denied (invalid session, 401), \
             never the outage error"
        );
    }
}
