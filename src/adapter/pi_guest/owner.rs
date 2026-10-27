//! Private root-owner launch permits. Neither `Authorized` wire JSON nor a
//! VerifiedGrant can construct this object. Origin is the constructor's physical
//! root custody, separately signed runtime lifetime and authentic external
//! one-use Pi operation. This is NOT an owned-helper proof: kernel admission
//! against the actual retained child remains the constructor dispatcher's job.
use crate::error::{Error, Result};
use crate::installer_bundle::constructor::{self, RuntimeProof};
use crate::protected_pi_profile::authority::{
    self, Authorized, ImageProfile, Role, Selection, SignedOperation,
};
pub(crate) use authority::OperationScope;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
static PROFILE_MINTED: AtomicBool = AtomicBool::new(false);

// Independently authored ticket acceptance; implementers own registration only.
#[cfg(test)]
mod acceptance;
mod authentication;
mod provision;
pub(crate) use provision::GenerationView;

const PROFILE: &str = "/opt/cadence/pi-profile.json";
const POLICY: &str = "/opt/cadence/pi-policy.json";
fn refuse() -> Error {
    Error::rejected("protected Pi owner scope/current/provenance is UNKNOWN or refused")
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    version: u32,
    routes: Vec<Route>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Route {
    alias: String,
    role: Role,
    models: Vec<String>,
}
#[derive(Clone, PartialEq, Eq)]
struct Epoch {
    global: String,
    company: String,
    epoch: u64,
    lineage: String,
    database_epoch: u64,
}
fn current(proof: &RuntimeProof, until: Instant) -> Result<Epoch> {
    proof.recheck(until)?;
    // Runtime authority has its OWN signed lifetime. An enrollment receipt,
    // completed construction or a fresh-looking local epoch cannot renew it.
    let current = constructor::runtime_current(until)?;
    proof.recheck(until)?;
    Ok(Epoch {
        global: current.global,
        company: current.company,
        epoch: current.epoch,
        lineage: current.lineage.reference,
        database_epoch: current.lineage.database_epoch,
    })
}
fn pin(value: &serde_json::Value, name: &str) -> Result<[u8; 32]> {
    let text = value
        .get(name)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(refuse)?;
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(refuse());
    }
    let mut out = [0; 32];
    for (i, to) in out.iter_mut().enumerate() {
        *to = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).map_err(|_| refuse())?;
    }
    if out == [0; 32] {
        return Err(refuse());
    }
    Ok(out)
}
fn read_elected<T: for<'a> Deserialize<'a> + Serialize>(
    path: &str,
    digest: [u8; 32],
    max: u64,
) -> Result<T> {
    let file = File::from(super::topology::open_at2(
        Path::new(path),
        super::execfd::OpenKind::ExecFile,
    )?);
    let before = file.metadata()?;
    let mut fs: libc::statvfs = unsafe { std::mem::zeroed() };
    if !before.is_file()
        || before.uid() != 0
        || before.gid() != 0
        || before.mode() & 0o7777 != 0o644
        || before.nlink() != 1
        || before.size() == 0
        || before.size() > max
        || unsafe { libc::fstatvfs(file.as_raw_fd(), &mut fs) } != 0
        || fs.f_flag & libc::ST_RDONLY == 0
    {
        return Err(refuse());
    }
    let mut bytes = Vec::new();
    (&file).take(max + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 != before.size()
        || <[u8; 32]>::from(Sha256::digest(&bytes)) != digest
        || (
            before.dev(),
            before.ino(),
            before.size(),
            before.ctime(),
            before.ctime_nsec(),
            before.mtime(),
            before.mtime_nsec(),
        ) != (
            after.dev(),
            after.ino(),
            after.size(),
            after.ctime(),
            after.ctime_nsec(),
            after.mtime(),
            after.mtime_nsec(),
        )
    {
        return Err(refuse());
    }
    let typed: T = serde_json::from_slice(&bytes).map_err(|_| refuse())?;
    let canonical = serde_json::to_vec(&serde_json::to_value(&typed).map_err(|_| refuse())?)
        .map_err(|_| refuse())?;
    if canonical != bytes {
        return Err(refuse());
    }
    Ok(typed)
}
#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Issued,
    Consumed,
    Current,
    Unknown,
}
#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum OperationPhase {
    Issued,
    Consumed,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationReply {
    version: u32,
    reference: String,
    scope: OperationScope,
    outcome: Outcome,
    phase: OperationPhase,
    authorization: String,
}
impl OperationReply {
    fn read(value: serde_json::Value) -> Result<Self> {
        serde_json::from_value(value).map_err(|_| refuse())
    }
    fn require(
        &self,
        scope: &OperationScope,
        reference: Option<&str>,
        outcome: Outcome,
        phase: OperationPhase,
    ) -> Result<()> {
        if self.version != 1
            || &self.scope != scope
            || self.outcome != outcome
            || self.phase != phase
            || self.reference.len() != 32
            || !self
                .reference
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !self.reference.bytes().any(|b| b != b'0')
            || reference.is_some_and(|r| r != self.reference)
        {
            return Err(refuse());
        }
        Ok(())
    }
}

/// Root-only factory input with independently authenticated image/policy and
/// separately authenticated runtime proof. No caller supplies pins, a key, paths,
/// permit booleans, claimed process identity or owner snapshots to this factory.
pub(crate) struct OwnerProfile {
    proof: RuntimeProof,
    image: ImageProfile,
    policy: Policy,
    profile_sha256: [u8; 32],
    policy_sha256: [u8; 32],
    generations: RefCell<std::collections::BTreeSet<(String, String)>>,
}
impl OwnerProfile {
    pub(crate) fn from_constructor(until: Instant) -> Result<Self> {
        if unsafe { libc::getuid() } != 0 || unsafe { libc::geteuid() } != 0 {
            return Err(refuse());
        }
        let until = until.min(Instant::now() + Duration::from_secs(authority::DEADLINE_SECS));
        let proof = constructor::runtime_proof(until)?;
        proof.recheck(until)?;
        let binding: serde_json::Value =
            serde_json::from_str(constructor::binding_json()?).map_err(|_| refuse())?;
        let pins = binding
            .get("challenge")
            .and_then(|v| v.get("pins"))
            .ok_or_else(refuse)?;
        let profile_sha256 = pin(pins, "piGraph")?;
        let policy_sha256 = pin(pins, "policy")?;
        let image: ImageProfile =
            read_elected(PROFILE, profile_sha256, authority::MAX_FRAME as u64)?;
        image.validate()?;
        if image.helper_sha256 != pin(pins, "helper")? || image.node_sha256 != pin(pins, "node")? {
            return Err(refuse());
        }
        let policy: Policy = read_elected(POLICY, policy_sha256, 65536)?;
        if policy.version != 1 || policy.routes.is_empty() || policy.routes.len() > 128 {
            return Err(refuse());
        }
        let mut aliases = std::collections::BTreeSet::new();
        for route in &policy.routes {
            if route.alias.is_empty()
                || route.alias.len() > 192
                || route.alias.contains(['\0', '\n', '\r'])
                || !aliases.insert(&route.alias)
                || route.models.is_empty()
                || route.models.len() > 64
            {
                return Err(refuse());
            }
            for model in &route.models {
                if !model.contains('/') {
                    return Err(refuse());
                }
                crate::protected_pi_profile::Routing::for_agent(route.role == Role::Master, model)
                    .map_err(Error::rejected)?;
            }
        }
        proof.recheck(until)?;
        // An epoch has exactly one issuer. Dropping/recreating a profile must
        // not erase issued generations or clear unresolved launch obligations.
        if PROFILE_MINTED
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(refuse());
        }
        Ok(Self {
            proof,
            image,
            policy,
            profile_sha256,
            policy_sha256,
            generations: RefCell::new(std::collections::BTreeSet::new()),
        })
    }
    pub(crate) fn issue(&self, selection: Selection, alias: &str) -> Result<LaunchPermit> {
        selection.validate()?;
        let route = self
            .policy
            .routes
            .iter()
            .find(|r| r.alias == alias)
            .ok_or_else(refuse)?;
        if route.role != selection.role
            || !route.models.contains(&selection.model)
            || super::Segments::new(alias, &selection.generation)?.alias_hex()
                != selection.alias_sha256
        {
            return Err(refuse());
        }
        let guest_gid = super::topology::resolve_primary_gid(super::acct::GUEST)?;
        let shared_gid = super::topology::resolve_gid_pub(super::acct::SHARED_GROUP)?;
        if guest_gid == 0 || shared_gid == 0 {
            return Err(refuse());
        }
        // The issuer can outlive a launch's IO budget, but NOT the independently
        // authenticated RuntimeProof. Each operation gets a fresh bounded IO
        // window; this does not renew or extend external owner authority.
        let until = Instant::now() + Duration::from_secs(authority::DEADLINE_SECS);
        let epoch = current(&self.proof, until)?;
        let scope = OperationScope {
            version: 1,
            binding_json: constructor::binding_json()?.to_owned(),
            global: epoch.global.clone(),
            company: epoch.company.clone(),
            epoch: epoch.epoch,
            lineage: epoch.lineage.clone(),
            database_epoch: epoch.database_epoch,
            alias: alias.to_owned(),
            selection: selection.clone(),
            helper_sha256: self.image.helper_sha256,
            node_sha256: self.image.node_sha256,
            profile_sha256: self.profile_sha256,
            policy_sha256: self.policy_sha256,
        };
        // The factory enforces ONE OwnerProfile per constructor process.
        // Retired generations are never evicted: reopening an already issued
        // alias/generation is a replay, not fresh. Exhaustion refuses closed.
        let mut generations = self.generations.try_borrow_mut().map_err(|_| refuse())?;
        if generations.len() >= 4096
            || !generations.insert((selection.alias_sha256.clone(), selection.generation.clone()))
        {
            return Err(refuse());
        }
        drop(generations);
        // Never retry this generation after a partial request or lost ACK.
        // Only the actual external owner's durable one-use operation can issue
        // the reference; local UUIDs/phase bits/current readbacks are not grants.
        // ONLY actual independently qualified Pi-purpose PUBLIC trust. Neither
        // a caller key nor receipt/grant/runtime trust can authorize this scope.
        let keys = constructor::pi_public_keys(until)?;
        let expiry = constructor::pi_expires_at_ms(until)?;
        let response = constructor::pi_acquire(&scope, until)?;
        let reply = OperationReply::read(response)?;
        reply.require(&scope, None, Outcome::Issued, OperationPhase::Issued)?;
        let authorization = authentication::authenticate_operation(
            &scope,
            &reply.reference,
            &reply.authorization,
            keys,
            authentication::now_ms()?,
            expiry,
        )?;
        let until = until.min(authorization.deadline());
        if current(&self.proof, until)? != epoch {
            return Err(refuse());
        }
        authorization.require(&reply.authorization, authentication::now_ms()?)?;
        let launch = Authorized {
            version: 1,
            selection,
            alias: alias.to_owned(),
            operation: reply.reference,
            supervisor: authority::SUPERVISOR_UID,
            guest: authority::GUEST_UID,
            guest_gid,
            shared_gid,
            image: self.image.clone(),
        };
        launch.validate(&launch.selection)?;
        Ok(LaunchPermit {
            proof: self.proof.clone(),
            launch,
            epoch,
            scope,
            until,
            authorization,
            phase: Cell::new(Phase::Prepared),
            unknown: Cell::new(false),
            view: RefCell::new(None),
        })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Prepared,
    Provisioned,
    Armed,
    Burned,
}
/// Non-Clone, private-origin, exact-scope, one-use permit. Serial root dispatch
/// owns this object; JSON output is a description, not a reconstruction API.
pub(crate) struct LaunchPermit {
    proof: RuntimeProof,
    launch: Authorized,
    epoch: Epoch,
    scope: OperationScope,
    until: Instant,
    authorization: authentication::AuthenticatedOperation,
    phase: Cell<Phase>,
    unknown: Cell<bool>,
    view: RefCell<Option<GenerationView>>,
}
/// Same production binding guard used by arm/consume. Expected values are the
/// retained owner's permit, not echoed caller fields. Exposed within crate for
/// the independent ticket-derived acceptance author, no provenance test port.
pub(crate) fn require_binding(
    expected: &Authorized,
    requested: &Selection,
    operation: Option<&str>,
) -> Result<()> {
    requested.validate()?;
    if &expected.selection != requested || operation.is_some_and(|op| op != expected.operation) {
        return Err(refuse());
    }
    Ok(())
}
impl LaunchPermit {
    pub(crate) fn describe(&self) -> &Authorized {
        &self.launch
    }
    /// Forward only the retained ORIGINAL signature/scope. A description never
    /// constructs this permit; helper elects its trust independently from media.
    pub(crate) fn signed_operation(&self) -> Result<Box<SignedOperation>> {
        if self.unknown.get() || Instant::now() >= self.until {
            return Err(refuse());
        }
        self.authorization.recheck(authentication::now_ms()?)?;
        Ok(Box::new(SignedOperation {
            authorization: self.authorization.original().to_owned(),
            scope: self.scope.clone(),
        }))
    }
    fn request(&self, consume: bool) -> Result<()> {
        if self.unknown.get() {
            return Err(refuse());
        }
        let result = (|| {
            self.authorization.recheck(authentication::now_ms()?)?;
            if Instant::now() >= self.until || current(&self.proof, self.until)? != self.epoch {
                return Err(refuse());
            }
            let response = if consume {
                constructor::pi_consume(&self.launch.operation, &self.scope, self.until)?
            } else {
                constructor::pi_current(&self.launch.operation, &self.scope, self.until)?
            };
            let reply = OperationReply::read(response)?;
            reply.require(
                &self.scope,
                Some(&self.launch.operation),
                if consume {
                    Outcome::Consumed
                } else {
                    Outcome::Current
                },
                if self.phase.get() == Phase::Burned {
                    OperationPhase::Consumed
                } else {
                    OperationPhase::Issued
                },
            )?;
            // Exact ORIGINAL independently signed authorization in every
            // issued/current/consumed reply. A replacement/renewal is refused.
            self.authorization
                .require(&reply.authorization, authentication::now_ms()?)?;
            if current(&self.proof, self.until)? != self.epoch {
                return Err(refuse());
            }
            self.authorization.recheck(authentication::now_ms()?)?;
            Ok(())
        })();
        if result.is_err() {
            self.unknown.set(true);
        }
        result
    }
    pub(crate) fn recheck(&self) -> Result<()> {
        self.request(false)
    }
    /// Root dispatcher admits its OWN supervisor before issuing/provisioning.
    /// Partial filesystem/current failures remain UNKNOWN, never repaired or
    /// deleted here. The guest receives a description only after this succeeds.
    pub(crate) fn provision(&self) -> Result<()> {
        if self.phase.get() != Phase::Prepared || self.unknown.get() {
            return Err(refuse());
        }
        let result = (|| {
            let view = provision::create(self)?;
            let mut slot = self.view.try_borrow_mut().map_err(|_| refuse())?;
            if slot.is_some() {
                return Err(refuse());
            }
            *slot = Some(view);
            self.phase.set(Phase::Provisioned);
            Ok(())
        })();
        if result.is_err() {
            self.unknown.set(true);
        }
        result
    }
    /// Move ONLY the originally provisioned view after successful durable
    /// consume/current. Called by serve on its successful Node-release path;
    /// later retirement uses runtime lifetime, not this expired launch H.P.S.
    pub(crate) fn into_generation_view(self) -> Result<GenerationView> {
        if self.phase.get() != Phase::Burned || self.unknown.get() {
            return Err(refuse());
        }
        self.recheck()?;
        self.view.into_inner().ok_or_else(refuse)
    }
    pub(crate) fn require_provisioned(&self) -> Result<()> {
        if self.phase.get() != Phase::Provisioned || self.unknown.get() {
            return Err(refuse());
        }
        self.recheck()
    }
    /// Root dispatcher must independently match the kernel caller against the
    /// retained own-created helper handle BEFORE invoking this scope transition.
    pub(crate) fn arm(&self, selection: &Selection) -> Result<()> {
        require_binding(&self.launch, selection, None)?;
        if self.phase.get() != Phase::Provisioned {
            return Err(refuse());
        }
        self.recheck()?;
        self.phase.set(Phase::Armed);
        Ok(())
    }
    /// Burn before sampling/sending. Any stale/closed/ABA owner or lost ACK is
    /// UNKNOWN and cannot be replayed into another generation or connection.
    pub(crate) fn consume(&self, selection: &Selection, operation: &str) -> Result<()> {
        require_binding(&self.launch, selection, Some(operation))?;
        if self.phase.replace(Phase::Burned) != Phase::Armed {
            return Err(refuse());
        }
        // Burned phase is only local replay protection, NEVER authority.
        // Actual durable external consume/current must acknowledge this scope.
        self.request(true)
    }
}
