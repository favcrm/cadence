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
use crate::board_identity::{dedicated_deployment_ok, dedicated_issuer_ok, Role};
use crate::error::{Error, Result};
use crate::operator_auth::{PublicRealm, PublicSnapshot, SessionView};

const ENV_COMPANY: &str = "AGENTICOS_COMPANY";
const ENV_INSTANCE: &str = "AGENTICOS_INSTANCE";
const ENV_BOARD_HOST: &str = "AGENTICOS_BOARD_HOST";
const ENV_BOARD_ISSUER: &str = "AGENTICOS_BOARD_ISSUER";
const ENV_BOARD_COMPANY: &str = "AGENTICOS_BOARD_COMPANY";
const ENV_DEDICATED: &str = "AGENTICOS_DEDICATED_BOARD_DEPLOYMENT";
const ENV_VERIFIER_FILE: &str = "AGENTICOS_DEDICATED_BOARD_VERIFIER_FILE";

/// The platform-placed membership context, captured once at startup.
#[derive(Clone, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum MembershipMode {
    /// No platform placement: the operator's local sessions only. A public
    /// row still needs a saved realm that matches the current board config.
    Standalone,
    /// Platform-placed. Authority is re-proven fresh against the platform.
    Native(NativeContext),
    Dedicated(DedicatedContext),
    /// Native placement was signalled but its context is partial or invalid.
    /// Every public authority decision is refused.
    RequiredUnavailable,
}

/// The immutable native binding: the paired core identity and the board realm.
#[derive(Clone, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub struct NativeContext {
    pub company: String,
    pub instance: String,
    pub realm: PublicRealm,
}

#[derive(Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct DedicatedContext {
    pub deployment: String,
    pub realm: PublicRealm,
    verifier: String,
}

impl std::fmt::Debug for DedicatedContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DedicatedContext")
            .field("deployment", &self.deployment)
            .field("realm", &self.realm)
            .field("verifier", &"[redacted]")
            .finish()
    }
}

fn read_verifier(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    if !path.is_absolute() {
        return None;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = opts.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || !matches!(meta.len(), 64 | 65) {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o077 != 0 || meta.nlink() != 1 {
            return None;
        }
    }
    let mut verifier = String::with_capacity(65);
    file.take(66).read_to_string(&mut verifier).ok()?;
    if verifier.ends_with('\n') {
        verifier.pop();
    }
    (verifier.len() == 64
        && verifier
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(verifier)
}

fn unavailable() -> Error {
    Error::invalid(
        "capability_unavailable",
        "dedicated owner authority is unavailable",
    )
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerEnvelope {
    ok: bool,
    data: OwnerProof,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerProof {
    version: String,
    deployment_id: String,
    workspace_id: String,
    audience: String,
    subject_id: String,
    assertion_jti: String,
    role: String,
}

fn owner_body_matches(bytes: &[u8], ctx: &DedicatedContext, jti: &str, sub: &str) -> bool {
    let Ok(body) = serde_json::from_slice::<OwnerEnvelope>(bytes) else {
        return false;
    };
    body.ok
        && body.data.version == "dedicated-board-owner.v1"
        && body.data.deployment_id == ctx.deployment
        && body.data.workspace_id == ctx.realm.company
        && body.data.audience == ctx.realm.host
        && body.data.subject_id == sub
        && body.data.assertion_jti == jti
        && body.data.role == "owner"
}

impl DedicatedContext {
    // A 403 alone is an authoritative withdrawal. All other failures are
    // unavailable, including a malformed or mismatched success body.
    fn owner(&self, jti: &str, sub: &str) -> Result<bool> {
        if uuid::Uuid::parse_str(jti).is_err() {
            return Err(unavailable());
        }
        let url = format!(
            "{}/v2/board/dedicated/owner?deployment={}&assertion={}",
            self.realm.issuer, self.deployment, jti
        );
        let agent = crate::issuer_transport::agent(
            ureq::Agent::config_builder()
                .timeout_global(Some(std::time::Duration::from_secs(5)))
                .max_redirects(0)
                .http_status_as_error(false),
        );
        let mut response = agent
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.verifier))
            .call()
            .map_err(|_| unavailable())?;
        match response.status().as_u16() {
            403 => Ok(false),
            200 => {
                let bytes = response
                    .body_mut()
                    .with_config()
                    .limit(8192)
                    .read_to_vec()
                    .map_err(|_| unavailable())?;
                if owner_body_matches(&bytes, self, jti, sub) {
                    Ok(true)
                } else {
                    Err(unavailable())
                }
            }
            _ => Err(unavailable()),
        }
    }
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
    #[doc(hidden)]
    pub fn capture(hosted: &crate::lease::Hosted, read: &dyn Fn(&str) -> Option<String>) -> Self {
        let raw_company = read(ENV_COMPANY);
        let raw_instance = read(ENV_INSTANCE);
        let raw_dedicated = read(ENV_DEDICATED);
        let verifier_file = read(ENV_VERIFIER_FILE);
        if verifier_file.is_some() && raw_dedicated.is_none() {
            return Self::RequiredUnavailable;
        }
        let native_lease = hosted.lease.as_deref().is_some_and(is_native_lease);
        if let Some(deployment) = raw_dedicated {
            if raw_company.is_some()
                || raw_instance.is_some()
                || native_lease
                || !dedicated_deployment_ok(&deployment)
            {
                return Self::RequiredUnavailable;
            }
            let (Some(host), Some(issuer), Some(company), Some(path)) = (
                nonempty(read(ENV_BOARD_HOST)),
                nonempty(read(ENV_BOARD_ISSUER)),
                nonempty(read(ENV_BOARD_COMPANY)),
                nonempty(verifier_file),
            ) else {
                return Self::RequiredUnavailable;
            };
            if !dedicated_issuer_ok(&issuer)
                || !(1..=128).contains(&company.len())
                || !company
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                || host.is_empty()
                || host.bytes().any(|b| {
                    b.is_ascii_uppercase()
                        || b.is_ascii_control()
                        || b.is_ascii_whitespace()
                        || matches!(b, b'/' | b'?' | b'#' | b'@')
                })
                || host.parse::<ureq::http::uri::Authority>().is_err()
            {
                return Self::RequiredUnavailable;
            }
            let Some(verifier) = read_verifier(std::path::Path::new(&path)) else {
                return Self::RequiredUnavailable;
            };
            return Self::Dedicated(DedicatedContext {
                deployment,
                realm: PublicRealm {
                    issuer,
                    company,
                    host,
                },
                verifier,
            });
        }
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
            dedicated,
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
        if matches!(&self.membership_mode, MembershipMode::Standalone) && dedicated.is_none() {
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
        }
        let fresh = match &self.membership_mode {
            MembershipMode::Native(ctx) if dedicated.is_none() && saved == ctx.realm => {
                #[cfg(any(test, feature = "test-seam"))]
                let result = match &self.membership_lookup_for_test {
                    Some(lookup) => lookup(&hash, &user.sub, &ctx.company, &ctx.instance),
                    None => crate::board_membership::lookup_current_membership_by_digest(
                        &hash,
                        &user.sub,
                        &ctx.company,
                        &ctx.instance,
                        &self.board_jwks,
                    ),
                };
                #[cfg(not(any(test, feature = "test-seam")))]
                let result = crate::board_membership::lookup_current_membership_by_digest(
                    &hash,
                    &user.sub,
                    &ctx.company,
                    &ctx.instance,
                    &self.board_jwks,
                );
                result
            }
            MembershipMode::Dedicated(ctx) if saved == ctx.realm && user.is_operator() => {
                let Some(binding) = dedicated
                    .as_ref()
                    .filter(|b| b.deployment == ctx.deployment)
                else {
                    return Ok(Authority::Denied);
                };
                ctx.owner(&binding.assertion_jti, &user.sub)
                    .map(|valid| valid.then_some(Role::Operator))
            }
            _ => return Ok(Authority::Denied),
        };
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
        if after.hash != hash
            || after.revoked
            || after.realm.as_ref() != Some(&saved)
            || after.dedicated != dedicated
        {
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

    /// Local residue of a fresh platform owner decision completed outside PM,
    /// custody and release guards. Re-reads the exact cookie row and realm;
    /// there is no network call. The saved role may withhold, but cannot grant,
    /// the operator authority established by `fresh_operator`.
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
        let binding_matches = match &self.membership_mode {
            MembershipMode::Native(ctx) => ctx.realm == *fresh_realm && after.dedicated.is_none(),
            MembershipMode::Dedicated(ctx) => {
                ctx.realm == *fresh_realm
                    && after
                        .dedicated
                        .as_ref()
                        .is_some_and(|binding| binding.deployment == ctx.deployment)
            }
            _ => false,
        };
        if !binding_matches {
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
            MembershipMode::Dedicated(ctx) if ctx.realm == *realm => Ok(()),
            _ => Err(Error::invalid(
                "capability_unavailable",
                "the board realm does not match this platform-placed daemon",
            )),
        }
    }

    pub(super) fn dedicated_mint_binding(
        &self,
        realm: &PublicRealm,
        user: &crate::operator_auth::BoardUser,
        jti: &str,
    ) -> Result<Option<crate::operator_auth::DedicatedBinding>> {
        self.require_realm_matches_mode(realm)?;
        let MembershipMode::Dedicated(ctx) = &self.membership_mode else {
            return Ok(None);
        };
        if !user.is_operator() || !ctx.owner(jti, &user.sub)? {
            return Err(Error::invalid(
                "assertion_invalid",
                "current owner authority was not proven",
            ));
        }
        if self.current_realm()? != *realm {
            return Err(unavailable());
        }
        Ok(Some(crate::operator_auth::DedicatedBinding {
            deployment: ctx.deployment.clone(),
            assertion_jti: jti.to_string(),
        }))
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

    #[test]
    fn dedicated_mode_and_closed_owner_proof() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("verifier");
        let secret = "a".repeat(64);
        std::fs::write(&path, &secret).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut env = std::collections::HashMap::from([
            (ENV_DEDICATED, "fixture-board".to_string()),
            (ENV_BOARD_HOST, "dedicated.example.test".to_string()),
            (ENV_BOARD_ISSUER, "https://issuer.example.test".to_string()),
            (ENV_BOARD_COMPANY, "fixture-workspace".to_string()),
            (ENV_VERIFIER_FILE, path.to_str().unwrap().to_string()),
        ]);
        let capture = |env: &std::collections::HashMap<&str, String>| {
            MembershipMode::capture(&crate::lease::Hosted::default(), &|name| {
                env.get(name).cloned()
            })
        };
        let MembershipMode::Dedicated(ctx) = capture(&env) else {
            panic!("dedicated config refused")
        };
        assert!(!format!("{ctx:?}").contains(&secret));
        let jti = "bdb2c121-85fa-443e-8849-92a0f9434187";
        let mut body = json!({"ok": true, "data": {
            "version": "dedicated-board-owner.v1", "deployment_id": "fixture-board",
            "workspace_id": "fixture-workspace", "audience": "dedicated.example.test",
            "subject_id": "fixture-owner", "assertion_jti": jti, "role": "owner"
        }});
        assert!(owner_body_matches(
            &serde_json::to_vec(&body).unwrap(),
            &ctx,
            jti,
            "fixture-owner"
        ));
        body["data"]["role"] = json!("member");
        assert!(!owner_body_matches(
            &serde_json::to_vec(&body).unwrap(),
            &ctx,
            jti,
            "fixture-owner"
        ));
        body["data"]["role"] = json!("owner");
        body["data"]["extra"] = json!(true);
        assert!(!owner_body_matches(
            &serde_json::to_vec(&body).unwrap(),
            &ctx,
            jti,
            "fixture-owner"
        ));
        env.insert(ENV_INSTANCE, "native".into());
        assert!(matches!(capture(&env), MembershipMode::RequiredUnavailable));
        env.remove(ENV_INSTANCE);
        env.insert(ENV_DEDICATED, String::new());
        assert!(matches!(capture(&env), MembershipMode::RequiredUnavailable));
        env.remove(ENV_DEDICATED);
        assert!(matches!(capture(&env), MembershipMode::RequiredUnavailable));
        env.remove(ENV_VERIFIER_FILE);
        assert!(matches!(capture(&env), MembershipMode::Standalone));
    }

    #[test]
    fn dedicated_pinned_owner_recheck_requires_the_exact_bound_row() {
        use crate::operator_auth::DedicatedBinding;

        let dir = tempfile::tempdir().unwrap();
        let realm = PublicRealm {
            issuer: "https://api.example.test".into(),
            company: "fixture-workspace".into(),
            host: "board.vps.test".into(),
        };
        crate::board_identity::write_config(
            dir.path(),
            &crate::board_identity::Config {
                host: realm.host.clone(),
                issuer: realm.issuer.clone(),
                company: realm.company.clone(),
            },
        )
        .unwrap();
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", dir.path().join("no-pm").to_str().unwrap());
        let mut shared = Shared::new(dir.path(), &opts).unwrap();
        Arc::get_mut(&mut shared).unwrap().membership_mode =
            MembershipMode::Dedicated(DedicatedContext {
                deployment: "fixture-board".into(),
                realm: realm.clone(),
                verifier: "a".repeat(64),
            });

        let sub = "fixture-owner";
        let now = shared.operator_now();
        let check = |deployment: Option<&str>, jti: &str| {
            let opened = shared
                .operator_auth()
                .open_public_bound_with_intent(
                    BoardUser {
                        sub: sub.into(),
                        email: "owner@example.test".into(),
                        name: "Fixture Owner".into(),
                        role: "operator".into(),
                        handle: "owner".into(),
                    },
                    (jti, now + 60),
                    "test",
                    (
                        realm.clone(),
                        deployment.map(|deployment| DedicatedBinding {
                            deployment: deployment.into(),
                            assertion_jti: jti.into(),
                        }),
                    ),
                    now,
                    |_| Ok(()),
                )
                .unwrap()
                .unwrap();
            let hash = crate::operator_auth::digest(&opened.token);
            shared
                .public_session_still(&opened.token, &hash, sub, &realm, true)
                .unwrap()
        };
        assert!(check(
            Some("fixture-board"),
            "bdb2c121-85fa-443e-8849-92a0f9434187"
        ));
        assert!(!check(
            Some("other-board"),
            "9b6dde13-57cc-4dca-b645-1b291fc46581"
        ));
        assert!(!check(None, "8ddee254-7085-4822-8315-a2589df6abf9"));
    }

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

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn cad2487_entry_refuses_demoted_public_owner() {
        use std::sync::atomic::{AtomicBool, Ordering};

        const OWNER_ACCOUNT: &str = "acu-11111111111111111111111111111111";
        const OTHER_ACCOUNT: &str = "acu-22222222222222222222222222222222";

        let dir = tempfile::tempdir().unwrap();
        let demoted = Arc::new(AtomicBool::new(false));
        let membership_demoted = Arc::clone(&demoted);
        let (shared, token) =
            native_shared(dir.path(), "entry-owner", "operator", move |_, _, _, _| {
                let role = if membership_demoted.load(Ordering::Relaxed) {
                    Role::Member
                } else {
                    Role::Operator
                };
                Ok(Some(role))
            });
        shared
            .operator_auth()
            .bind_account(&token, OWNER_ACCOUNT)
            .unwrap();
        let hash = crate::operator_auth::digest(&token);

        assert!(
            shared
                .entry_public_owner_is_current(&hash, Some(OWNER_ACCOUNT))
                .unwrap(),
            "a current owner can continue for the account bound to the source session"
        );
        assert!(
            !shared
                .entry_public_owner_is_current(&hash, Some(OTHER_ACCOUNT))
                .unwrap(),
            "a current owner cannot continue for a different account"
        );

        let proof = json!({"token": &token, "key": "", "origin": "public"});
        super::super::credential_entry_rpc::management_session_binding(&shared, &proof)
            .expect("the current public owner can bind a management session");

        demoted.store(true, Ordering::Relaxed);
        assert!(
            !shared
                .entry_public_owner_is_current(&hash, Some(OWNER_ACCOUNT))
                .unwrap(),
            "the source session no longer authorizes entry after live demotion"
        );
        let error = super::super::credential_entry_rpc::management_session_binding(&shared, &proof)
            .expect_err("the same public proof must fail after live demotion");
        assert_eq!(error.code(), Some("entry_session_required"));
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn cad2487_standalone_public_login_does_not_authorize_custody() {
        let dir = tempfile::tempdir().unwrap();
        crate::board_identity::write_config(
            dir.path(),
            &crate::board_identity::Config {
                host: HOST.into(),
                issuer: ISSUER.into(),
                company: COMPANY.into(),
            },
        )
        .unwrap();
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", dir.path().join("no-pm").to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let now = shared.operator_now();
        let opened = shared
            .operator_auth()
            .open_public_bound(
                BoardUser {
                    sub: "standalone-owner".into(),
                    email: "owner@example.test".into(),
                    name: "Fixture owner".into(),
                    role: "operator".into(),
                    handle: "standalone-owner".into(),
                },
                "standalone-owner-jti",
                now + 60,
                "cad2487-fixture",
                realm(),
                now,
            )
            .unwrap()
            .unwrap();
        assert!(matches!(
            shared.current_public_session(&opened.token).unwrap(),
            Authority::Granted {
                role: Role::Operator,
                ..
            }
        ));
        let proof = json!({"token": opened.token, "key": "", "origin": "public"});
        let error = super::super::credential_entry_rpc::management_session_binding(&shared, &proof)
            .expect_err("a signed standalone session cannot prove current owner membership");
        assert_eq!(error.code(), Some("capability_unavailable"));
    }
}
