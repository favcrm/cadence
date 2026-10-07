//! CAD-1180 origin-ordered tracker durability. Capture and ONE reservation
//! run under the ORIGINAL writer PmLock, after commit. This deliberately
//! holds the lock across bounded (at most five seconds) private transport:
//! reserving after release could reverse publication order. Upload/complete
//! run after release, using only that accepted token, with one whole-operation
//! monotonic twenty-second budget. No replay, re-reserve, or chunk retry.
//!
//! Required mode is independent of Store presence. Legacy test stores retain
//! the old persist seam, but cannot satisfy the required protocol gate or
//! manufacture a required reservation. Actual confirmation includes the full
//! content-bound host receipt. All post-commit failures retain applied=true.

#[cfg(test)]
pub mod observation;
pub mod tracker;
pub use tracker::{
    configure_from_env, verify_boot_command, Descriptor, HostReceipt, Mode, Reservation,
};

use crate::error::{Error, Result};
use crate::issue::{git, Pm, PmLock};
use serde_json::{json, Value};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

const FREEZE_CAP: u64 = 256 * 1024 * 1024;

/// Native serving scope. The full host authority binding also includes its
/// separate, NON-null authority epoch and boot ID (see tracker::AuthorityBinding).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub company: String,
    pub instance: String,
    pub generation: u64,
}

/// Fields retained for the independently authored legacy QA harness. Required
/// mode is an explicit separate setting, not inferred from this handle.
#[derive(Clone)]
pub struct Hosted {
    pub binding: Binding,
    pub store: Arc<dyn Store>,
    pub persist_budget: Duration,
}
impl Hosted {
    pub fn validate(&self) -> Result<()> {
        if self.binding.company.trim().is_empty()
            || self.binding.instance.trim().is_empty()
            || self.binding.generation == 0
            || self.persist_budget.is_zero()
        {
            return Err(Error::rejected(
                "hosted durability binding/budget unavailable",
            ));
        }
        Ok(())
    }
    pub fn validate_for(&self, expected_company: &str) -> Result<()> {
        self.validate()?;
        if self.binding.company != expected_company {
            return Err(Error::rejected("hosted durability company mismatch"));
        }
        Ok(())
    }
    pub fn validate_mode(&self, mode: Mode) -> Result<()> {
        self.validate()?;
        if mode == Mode::Required {
            if !self.store.ordered_protocol() {
                return Err(Error::rejected("tracker-v1 protocol unavailable"));
            }
            self.store.validate_required(&self.binding)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Receipt {
    pub commit: String,
    /// Immutable originating commit's Lease-Epoch trailer; None when unleased.
    /// This is never equated with the host's authority_epoch domain.
    pub lease_epoch: Option<u64>,
    pub artifact_sha256: String,
    pub artifact_bytes: u64,
    pub binding: Binding,
}
impl Receipt {
    pub fn json(&self) -> Value {
        json!({"commit":self.commit, "lease_epoch":self.lease_epoch,
            "origin_lease_epoch":self.lease_epoch, "artifact_sha256":self.artifact_sha256,
            "artifact_bytes":self.artifact_bytes, "company":self.binding.company,
            "instance":self.binding.instance, "generation":self.binding.generation})
    }
}

/// No Clone: do not duplicate a potentially 256MiB artifact. The export uses
/// an owned disk spool, then a single bounded vector for legacy Store API
/// compatibility; uploads spool only one 1MiB chunk, not another whole bundle.
#[derive(Debug)]
pub struct Captured {
    pub receipt: Receipt,
    pub artifact: Option<Vec<u8>>,
    pub reservation: Option<Reservation>,
    pub failure: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Durability {
    Confirmed,
    Unconfirmed,
}
impl Durability {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Unconfirmed => "unconfirmed",
        }
    }
}

/// Monotonic shared budget, not a cancellation primitive. The actual HTTP
/// backend uses bounded curl plus an owned-child watchdog; a legacy injected
/// Store remains cooperative and cannot satisfy Required mode.
#[derive(Clone, Debug)]
pub struct Deadline {
    at: Instant,
}
impl Deadline {
    pub fn after(budget: Duration) -> Self {
        Self {
            at: Instant::now() + budget,
        }
    }
    pub fn remaining(&self) -> Duration {
        self.at.saturating_duration_since(Instant::now())
    }
    pub fn expired(&self) -> bool {
        self.remaining().is_zero()
    }
}

/// Legacy effect seam only. This incomplete proof cannot confirm a Required
/// tracker-v1 write, which must return the complete HostReceipt instead.
#[derive(Clone, Debug)]
pub struct EffectReceipt {
    pub effect_id: String,
    pub commit: String,
    pub binding: Binding,
    pub lease_epoch: Option<u64>,
    pub artifact_sha256: String,
}
#[derive(Clone, Debug)]
pub enum StoreOutcome {
    Confirmed { effect: EffectReceipt },
    Unconfirmed { reason: String },
}

pub trait Store: Send + Sync {
    /// Legacy persistence, after guard release. Errors are known-applied
    /// unconfirmed, never a mutation replay. Called exactly once in this seam.
    fn persist(
        &self,
        receipt: &Receipt,
        artifact: &[u8],
        deadline: &Deadline,
    ) -> Result<StoreOutcome>;
    /// Optional extension preserves old QA stubs; defaults never fabricate an
    /// accepted token or admit an unsupported Required backend.
    fn ordered_protocol(&self) -> bool {
        false
    }
    fn validate_required(&self, _binding: &Binding) -> Result<()> {
        Err(Error::rejected("tracker-v1 ordered capability unavailable"))
    }
    fn reserve(
        &self,
        _receipt: &Receipt,
        _descriptor: &Descriptor,
        _manifest: &str,
        _deadline: &Deadline,
    ) -> Result<Reservation> {
        Err(Error::rejected("tracker-v1 reservation unavailable"))
    }
    fn complete_reserved(
        &self,
        _receipt: &Receipt,
        _artifact: &[u8],
        _reservation: &Reservation,
        _deadline: &Deadline,
    ) -> Result<HostReceipt> {
        Err(Error::rejected("tracker-v1 completion unavailable"))
    }
}
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
fn commit_epoch(pm: &Pm, _lock: &PmLock, commit: &str) -> Result<Option<u64>> {
    // An unleased caller cannot invent origin authority with a title/body
    // containing 'Lease-Epoch:'. For leased writers use the final immutable
    // trailer stamped by commit, never the live (possibly renewed) epoch.
    if !pm.lease_attached() {
        return Ok(None);
    }
    let body = git(
        &pm.dir,
        &[
            "log",
            "-1",
            "--format=%(trailers:key=Lease-Epoch,valueonly)",
            commit,
        ],
    )?;
    let epoch = body
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .and_then(|value| value.trim().parse().ok())
        .ok_or_else(|| Error::rejected("origin lease trailer unavailable"))?;
    Ok(Some(epoch))
}
fn freeze(pm: &Pm, _lock: &PmLock) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(&pm.dir)
        .args(["bundle", "create", "-", "HEAD"]);
    tracker::run_bounded(
        command,
        &Deadline::after(Duration::from_secs(20)),
        FREEZE_CAP,
    )
}
/// Read the bundle's bounded textual header directly, avoiding a second child
/// and an unbounded stdin/output pipe. The host restore independently verifies
/// the full bundle and actual fetched commit before opening tracker readiness.
fn bundle_head(bytes: &[u8]) -> Option<&str> {
    let header = bytes.get(..bytes.len().min(16384))?;
    let end = header.windows(2).position(|w| w == b"\n\n")?;
    let header = std::str::from_utf8(&header[..end]).ok()?;
    if !header.starts_with("# v2 git bundle\n") && !header.starts_with("# v3 git bundle\n") {
        return None;
    }
    header.lines().find_map(|line| {
        let (sha, name) = line.split_once(' ')?;
        (name == "HEAD").then_some(sha)
    })
}

/// Under the STILL-held original guard: freeze, descriptor/hash, and reserve
/// exactly once. Capture failure cannot undo the already committed mutation.
pub fn capture(pm: &Pm, lock: &PmLock, hosted: &Hosted) -> Captured {
    let commit = git(&pm.dir, &["rev-parse", "HEAD"]).unwrap_or_default();
    let epoch = if commit.is_empty() {
        Err(Error::rejected("origin commit unavailable"))
    } else {
        commit_epoch(pm, lock, &commit)
    };
    let epoch_failure = epoch.as_ref().err().map(ToString::to_string);
    let lease_epoch = epoch.unwrap_or(None);
    let artifact = if commit.is_empty() || epoch_failure.is_some() {
        None
    } else {
        freeze(pm, lock)
            .ok()
            .filter(|b| bundle_head(b) == Some(commit.as_str()))
    };
    let (artifact_sha256, artifact_bytes) = artifact
        .as_ref()
        .map(|a| (sha256_hex(a), a.len() as u64))
        .unwrap_or_default();
    let receipt = Receipt {
        commit,
        lease_epoch,
        artifact_sha256,
        artifact_bytes,
        binding: hosted.binding.clone(),
    };
    let mut failure = epoch_failure.or_else(|| {
        artifact
            .is_none()
            .then(|| "origin freeze unavailable".to_string())
    });
    #[cfg(test)]
    if let Some(bytes) = &artifact {
        observation::captured(&receipt, bytes);
    }
    let reservation = if hosted.store.ordered_protocol() {
        artifact.as_ref().and_then(|bytes| {
            let attempt: Result<Reservation> = (|| {
                // Reject known host-incompatible frozen trees BEFORE reserve.
                tracker::restore_profile(
                    &pm.dir,
                    &receipt.commit,
                    &Deadline::after(Duration::from_secs(5)),
                )?;
                let descriptor = Descriptor::from_artifact(&receipt, bytes)?;
                let manifest = sha256_hex(&descriptor.canonical()?);
                let deadline = Deadline::after(Duration::from_secs(5));
                let accepted = hosted
                    .store
                    .reserve(&receipt, &descriptor, &manifest, &deadline)?;
                let r = &accepted.receipt;
                let matches = r.commit == receipt.commit
                    && r.origin_lease_epoch == receipt.lease_epoch
                    && r.artifact_sha256 == receipt.artifact_sha256
                    && r.artifact_bytes == receipt.artifact_bytes
                    && r.manifest_sha256 == manifest
                    && r.binding.company == receipt.binding.company
                    && r.binding.instance == receipt.binding.instance
                    && r.binding.generation == receipt.binding.generation;
                if !matches || deadline.expired() {
                    return Err(Error::rejected("reservation mismatch or late response"));
                }
                Ok(accepted)
            })();
            match attempt {
                Ok(accepted) => {
                    #[cfg(test)]
                    observation::reserved(&accepted);
                    Some(accepted)
                }
                Err(e) => {
                    failure = Some(e.to_string());
                    None
                }
            }
        })
    } else {
        None
    };
    Captured {
        receipt,
        artifact,
        reservation,
        failure,
    }
}

/// AFTER release: all chunks and completion share ONE budget. Never reserve,
/// re-enter a tracker lock, retry a chunk, or recover a lost reservation.
pub fn persist(captured: &Captured, hosted: &Hosted) -> (Value, Durability) {
    let receipt = &captured.receipt;
    let Some(artifact) = &captured.artifact else {
        return unconfirmed(captured, None);
    };
    let deadline = Deadline::after(hosted.persist_budget.min(Duration::from_secs(20)));
    if hosted.store.ordered_protocol() {
        let Some(reservation) = &captured.reservation else {
            return unconfirmed(captured, None);
        };
        return match hosted
            .store
            .complete_reserved(receipt, artifact, reservation, &deadline)
        {
            Ok(completed) if completed == reservation.receipt && !deadline.expired() => (
                serde_json::to_value(completed).unwrap_or(Value::Null),
                Durability::Confirmed,
            ),
            Err(e) => unconfirmed(captured, Some(&e.to_string())),
            _ => unconfirmed(
                captured,
                Some("completion receipt mismatch or late response"),
            ),
        };
    }
    match hosted.store.persist(receipt, artifact, &deadline) {
        Ok(StoreOutcome::Confirmed { effect })
            if !deadline.expired()
                && effect.commit == receipt.commit
                && effect.binding == receipt.binding
                && effect.lease_epoch == receipt.lease_epoch
                && effect.artifact_sha256 == receipt.artifact_sha256
                && !effect.effect_id.is_empty() =>
        {
            let mut proof = receipt.json();
            proof["legacy_effect_id"] = json!(effect.effect_id);
            (proof, Durability::Confirmed)
        }
        Err(e) => unconfirmed(captured, Some(&e.to_string())),
        Ok(StoreOutcome::Unconfirmed { reason }) => unconfirmed(captured, Some(&reason)),
        _ => unconfirmed(
            captured,
            Some("legacy completion receipt mismatch or late response"),
        ),
    }
}
fn unconfirmed(captured: &Captured, reason: Option<&str>) -> (Value, Durability) {
    let reason = reason
        .or(captured.failure.as_deref())
        .unwrap_or("held reservation unavailable");
    let message: String = reason
        .chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect();
    let code = reason
        .strip_prefix("tracker-v1 refusal: ")
        .filter(|c| {
            matches!(
                *c,
                "invalid_request"
                    | "lease_lost"
                    | "stale_generation"
                    | "conflict"
                    | "superseded"
                    | "not_ready"
                    | "capability_unavailable"
                    | "expired"
                    | "capacity"
                    | "custody_unavailable"
                    | "not_found"
                    | "unknown"
            )
        })
        .unwrap_or("unconfirmed");
    let mut receipt = captured.receipt.json();
    receipt["durability_error"] = json!({"code":code, "message":message});
    if let Some(reservation) = &captured.reservation {
        receipt["reservation"] = serde_json::to_value(&reservation.receipt).unwrap_or(Value::Null);
    }
    (receipt, Durability::Unconfirmed)
}
