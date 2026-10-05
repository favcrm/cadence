//! Private root-owner launch permits. Neither `Authorized` wire JSON nor a
//! VerifiedGrant can construct this object. Origin is the real constructor's
//! retained kernel custody plus its authenticated current-owner relay.
use crate::error::{Error, Result};
use crate::installer_bundle::constructor::{self, Proof};
use crate::protected_pi_profile::authority::{self, Authorized, ImageProfile, Role, Selection};
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
fn current(proof: &Proof, until: Instant) -> Result<Epoch> {
    proof.recheck(until)?;
    let response = constructor::owner_request(
        constructor::Kind::ConsumedCurrent,
        constructor::binding_json()?.as_bytes(),
        until,
    )?;
    // owner_request validates exact parent operation, binding, lineage, phase,
    // sequence and closure through the retained provider channel, never a file.
    let current = response.current.ok_or_else(refuse)?;
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
/// Root-only factory input with independently authenticated image/policy and
/// actual retained constructor proof. No caller supplies pins, a key, paths,
/// permit booleans, claimed process identity or owner snapshots to this factory.
pub(crate) struct OwnerProfile {
    proof: Proof,
    image: ImageProfile,
    policy: Policy,
    until: Instant,
    generations: RefCell<std::collections::BTreeSet<(String, String)>>,
}
impl OwnerProfile {
    pub(crate) fn from_constructor(until: Instant) -> Result<Self> {
        if unsafe { libc::getuid() } != 0 || unsafe { libc::geteuid() } != 0 {
            return Err(refuse());
        }
        let until = until.min(Instant::now() + Duration::from_secs(authority::DEADLINE_SECS));
        let proof = constructor::proof()?;
        proof.recheck(until)?;
        let binding: serde_json::Value =
            serde_json::from_str(constructor::binding_json()?).map_err(|_| refuse())?;
        let pins = binding
            .get("challenge")
            .and_then(|v| v.get("pins"))
            .ok_or_else(refuse)?;
        let image: ImageProfile =
            read_elected(PROFILE, pin(pins, "piGraph")?, authority::MAX_FRAME as u64)?;
        image.validate()?;
        if image.helper_sha256 != pin(pins, "helper")? || image.node_sha256 != pin(pins, "node")? {
            return Err(refuse());
        }
        let policy: Policy = read_elected(POLICY, pin(pins, "policy")?, 65536)?;
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
            until,
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
        if route.role != selection.role || !route.models.contains(&selection.model) {
            return Err(refuse());
        }
        let epoch = current(&self.proof, self.until)?;
        let launch = Authorized {
            version: 1,
            selection,
            alias: alias.to_owned(),
            operation: uuid::Uuid::new_v4().simple().to_string(),
            supervisor: authority::SUPERVISOR_UID,
            guest: authority::GUEST_UID,
            guest_gid: super::topology::resolve_primary_gid(super::acct::GUEST)?,
            shared_gid: super::topology::resolve_gid_pub(super::acct::SHARED_GROUP)?,
            image: self.image.clone(),
        };
        launch.validate(&launch.selection)?;
        // The factory enforces ONE OwnerProfile per constructor process.
        // Retired generations are never evicted: reopening an already issued
        // alias/generation is a replay, not fresh. Exhaustion refuses closed.
        let mut generations = self.generations.try_borrow_mut().map_err(|_| refuse())?;
        if generations.len() >= 4096
            || !generations.insert((
                launch.selection.alias_sha256.clone(),
                launch.selection.generation.clone(),
            ))
        {
            return Err(refuse());
        }
        Ok(LaunchPermit {
            proof: self.proof.clone(),
            launch,
            epoch,
            until: self.until,
            phase: Cell::new(Phase::Prepared),
        })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Prepared,
    Armed,
    Burned,
}
/// Non-Clone, private-origin, exact-scope, one-use permit. Serial root dispatch
/// owns this object; JSON output is a description, not a reconstruction API.
pub(crate) struct LaunchPermit {
    proof: Proof,
    launch: Authorized,
    epoch: Epoch,
    until: Instant,
    phase: Cell<Phase>,
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
    pub(crate) fn recheck(&self) -> Result<()> {
        if Instant::now() >= self.until || current(&self.proof, self.until)? != self.epoch {
            return Err(refuse());
        }
        Ok(())
    }
    /// Root dispatcher must independently match the kernel caller against the
    /// retained own-created helper handle BEFORE invoking this scope transition.
    pub(crate) fn arm(&self, selection: &Selection) -> Result<()> {
        require_binding(&self.launch, selection, None)?;
        if self.phase.get() != Phase::Prepared {
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
        self.recheck()
    }
}
