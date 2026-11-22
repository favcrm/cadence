//! Lease fencing, controller lifecycle, and hosted configuration parsing for
//! `lease`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{Error, Result};

use super::{is_transient_lease_error, now_unix, Lease, Provider, HOST_RENEW_URL};

// ---------- the fence ----------

/// Default validity floor for a tracker commit (CAD-694): the SIGTERM
/// grace plus a margin. Short leases derive a smaller one.
pub const DEFAULT_COMMIT_FLOOR: Duration = Duration::from_millis(2000);

/// The trip-once latch the store's write path and every leased `Pm`
/// share. The first trip wins — later losses add nothing to the story.
/// Expiry is the second tripwire: a daemon whose heartbeat stopped but
/// whose shutdown tail is still running must not commit once the lease
/// it holds has lapsed — a successor may already own the store.
#[derive(Debug)]
pub struct Fence {
    reason: Mutex<Option<String>>,
    /// The held lease's expiry, `f64` bits — refreshed on every
    /// successful renew. `f64::INFINITY` until a lease sets it, so an
    /// unfenced or pre-lease store never refuses.
    expires_unix: AtomicU64,
    pub(super) expires_monotonic: Mutex<Option<Instant>>,
    /// Validity below which a tracker commit is refused or cancelled
    /// (CAD-694), in ms; derived from the lease's own margin.
    pub(super) commit_floor_ms: AtomicU64,
}

impl Default for Fence {
    fn default() -> Self {
        Self {
            reason: Mutex::new(None),
            expires_unix: AtomicU64::new(f64::INFINITY.to_bits()),
            expires_monotonic: Mutex::new(None),
            commit_floor_ms: AtomicU64::new(DEFAULT_COMMIT_FLOOR.as_millis() as u64),
        }
    }
}

impl Fence {
    pub fn trip(&self, reason: impl Into<String>) {
        let mut slot = self.reason.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(reason.into());
        }
    }

    /// Why the fence was explicitly tripped, when it was.
    pub fn reason(&self) -> Option<String> {
        self.reason
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// How long the held lease stays valid if it is never renewed
    /// again: `None` when no expiry applies (unleased), `Some(ZERO)`
    /// once tripped or lapsed.
    pub fn remaining(&self) -> Option<Duration> {
        if self.reason().is_some() {
            return Some(Duration::ZERO);
        }
        self.lapse().map(|(left, _)| left)
    }

    /// Validity below which a tracker commit must not run.
    pub fn commit_floor(&self) -> Duration {
        Duration::from_millis(self.commit_floor_ms.load(Ordering::SeqCst))
    }

    pub fn tripped(&self) -> bool {
        self.reason
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// The lease expiry the write checks consult — set on acquire and
    /// on every renewal, so it always means "the lease we hold lapses
    /// at this wall-clock".
    pub(crate) fn set_expiry(&self, expires_unix: f64) {
        self.expires_unix
            .store(expires_unix.to_bits(), Ordering::SeqCst);
    }

    pub(super) fn set_lease(&self, lease: &Lease) {
        self.set_expiry(lease.expires_unix);
        *self
            .expires_monotonic
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = lease.expires_monotonic;
    }

    /// The effective write refusal: an explicit trip, or the held
    /// lease's expiry having passed — renewal failure is the *detected*
    /// half of lease loss; this is the half that needs no detector, so
    /// a shutdown tail outliving the TTL cannot commit into a lease a
    /// successor already took.
    pub fn check(&self) -> Option<String> {
        if let Some(reason) = self.reason() {
            return Some(reason);
        }
        self.lapse()
            .and_then(|(left, why)| left.is_zero().then_some(why))
    }

    /// The held lease's expiry as `(time left, why it reads as expired)`
    /// — the single source `check` and `remaining` share. `None` when no
    /// expiry applies (unleased).
    fn lapse(&self) -> Option<(Duration, String)> {
        if let Some(deadline) = *self
            .expires_monotonic
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            return Some((
                deadline.saturating_duration_since(Instant::now()),
                "host lease renewal deadline expired".to_string(),
            ));
        }
        let expiry = f64::from_bits(self.expires_unix.load(Ordering::SeqCst));
        expiry.is_finite().then(|| {
            (
                Duration::from_secs_f64((expiry - now_unix()).max(0.0)),
                format!("lease expired at {expiry:.0} — a successor may hold it"),
            )
        })
    }
}

/// The lease handle a daemon attaches to each `Pm` it opens: the shared
/// fence plus the live epoch — read at commit time so a renewed or
/// re-acquired lease stamps the current epoch, not a stale snapshot.
#[derive(Clone)]
pub struct PmLease {
    fence: Arc<Fence>,
    epoch: Option<Arc<AtomicU64>>,
}

impl PmLease {
    /// Refuse once the daemon's lease is gone — tripped or expired.
    pub fn check(&self) -> Result<()> {
        match self.fence.check() {
            None => Ok(()),
            Some(reason) => Err(Error::rejected(format!(
                "tracker write refused — the daemon's hosted lease is lost: {reason}"
            ))),
        }
    }

    /// See [`Fence::commit_floor`].
    pub fn commit_floor(&self) -> Duration {
        self.fence.commit_floor()
    }

    /// See [`Fence::remaining`].
    pub fn remaining(&self) -> Option<Duration> {
        self.fence.remaining()
    }

    /// The lease epoch outbound writes record (`Lease-Epoch:`).
    pub fn epoch(&self) -> Option<u64> {
        self.epoch
            .as_ref()
            .map(|epoch| epoch.load(Ordering::SeqCst))
    }
}

// ---------- the held lease ----------

/// The daemon's lease and its knobs: what to renew, when the fence is
/// shared, and the epoch outbound writes carry.
pub struct LeaseCtl {
    pub(super) provider: Arc<dyn Provider>,
    pub(super) spec: String,
    pub(super) current: Mutex<Lease>,
    pub(super) fence: Arc<Fence>,
    pub(super) epoch: Option<Arc<AtomicU64>>,
    /// Heartbeat period — always under the TTL.
    pub renew_every: Duration,
    /// The SIGTERM flush bound from `hosted:` (or the default).
    pub flush_timeout: Duration,
}

impl LeaseCtl {
    /// Extend the hold one heartbeat. An expired, stolen or vanished
    /// lease surfaces as `Err` — the caller trips the fence — unless
    /// the failure [`is_transient_lease_error`] reports: a renewal blip
    /// inside the local deadline, which surfaces without fencing so a
    /// later `204` can still heal.
    pub fn renew(&self) -> Result<()> {
        if let Some(reason) = self.fence.check() {
            return Err(Error::rejected(format!("lease renewal refused: {reason}")));
        }
        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        let next = match self.provider.renew(&current) {
            Ok(next) => next,
            Err(error) if is_transient_lease_error(&error) => return Err(error),
            Err(error) => {
                self.fence.trip(format!("lease renewal failed: {error}"));
                return Err(error);
            }
        };
        match (&self.epoch, next.epoch) {
            (Some(epoch), Some(value)) => epoch.store(value, Ordering::SeqCst),
            (None, None) => {}
            _ => {
                self.fence
                    .trip("lease provider changed generation representation");
                return Err(Error::rejected(
                    "lease provider changed generation representation",
                ));
            }
        }
        self.fence.set_lease(&next);
        *current = next;
        Ok(())
    }

    /// Release the hold. Best-effort at shutdown — expiry covers the
    /// case where this fails or never runs.
    pub fn release(&self) -> Result<()> {
        let current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        self.provider.release(&current)
    }

    /// The latch the store and tracker fence checks read.
    pub fn fence(&self) -> Arc<Fence> {
        Arc::clone(&self.fence)
    }

    /// The handle a `Pm` carries.
    pub fn pm_lease(&self) -> PmLease {
        PmLease {
            fence: Arc::clone(&self.fence),
            epoch: self.epoch.clone(),
        }
    }

    /// The current epoch — stamped on the lease lifecycle events.
    pub fn epoch(&self) -> Option<u64> {
        self.epoch
            .as_ref()
            .map(|epoch| epoch.load(Ordering::SeqCst))
    }

    /// `health`/`daemon_info` surface: provider, epoch, expiry, fence.
    /// An HTTP provider in its retry window adds `retrying: true` with
    /// `last_renew_error`; once fenced, `retrying` reads false — the
    /// heartbeat has stopped, so nothing is still retrying.
    pub fn status_json(&self) -> Value {
        let current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        let fenced = self.fence.check();
        let is_fenced = fenced.is_some();
        let mut status = json!({
            "provider": self.spec,
            "holder": current.holder,
            "epoch": current.epoch,
            "epoch_available": current.epoch.is_some(),
            "expiry_authority": if current.expires_monotonic.is_some() { "local_renewal_deadline" } else { "provider" },
            "expires_unix": current.expires_unix,
            "renew_secs": self.renew_every.as_secs_f64(),
            "fenced": fenced,
        });
        if let Value::Object(extra) = self.provider.status_extra() {
            if let Some(map) = status.as_object_mut() {
                map.extend(extra);
            }
        }
        if is_fenced {
            if let Some(map) = status.as_object_mut() {
                if map.contains_key("retrying") {
                    map.insert("retrying".to_string(), Value::Bool(false));
                }
            }
        }
        status
    }
}

/// A unique-per-process holder — pid for ops readability, a random
/// suffix so a restarted daemon never collides with its own stale
/// record.
pub(super) fn holder_for(state_dir: &Path) -> String {
    let dir = state_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    format!("{}:{}:{}", std::process::id(), dir, &nonce[..8])
}

/// The `hosted:` table in `pm.yaml` — every field optional; no `lease`
/// key (the default) is hosted lifecycle off. Also the verbatim value
/// `ServeOptions.lease` carries: a test sets `Some(Hosted::default())`
/// to pin "off" regardless of what pm.yaml says.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hosted {
    /// `file:<path>` | `http://lease.internal` | `none`/`off` — unset is off.
    pub lease: Option<String>,
    /// Seconds a held lease survives the daemon that took it.
    pub lease_ttl_secs: Option<u64>,
    /// Seconds between renews — must stay under the TTL.
    pub lease_renew_secs: Option<u64>,
    /// SIGTERM bound on the WAL-checkpoint + tracker flush.
    pub flush_timeout_secs: Option<u64>,
}

/// Read the tracker's `hosted:` table. Parse only: no acquire, renew,
/// or authority check. `Ok(None)` is a missing `pm.yaml` or a missing
/// table (hosted lifecycle off). Any other I/O failure, invalid YAML,
/// or a present table that is not a mapping or cannot deserialize as [`Hosted`]
/// (`deny_unknown_fields`, wrong types) is an error naming the path
/// and, when one key is the culprit, that key. `none`/`off`/unset
/// `lease` stays a successful [`Hosted`] value; [`acquire`](super::acquire) enforces
/// the lease spec.
pub fn read_hosted(pm_dir: &Path) -> Result<Option<Hosted>> {
    let path = pm_dir.join("pm.yaml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Error::rejected(format!(
                "cannot read lifecycle config {} ({e}) — refusing rather than \
                 guessing whether hosted lifecycle is configured",
                path.display()
            )));
        }
    };
    let yaml: serde_yaml::Value = serde_yaml::from_str(&text)
        .map_err(|e| Error::rejected(format!("{} is not valid YAML: {e}", path.display())))?;
    let Some(hosted) = yaml.get("hosted") else {
        return Ok(None);
    };
    let mapping = hosted
        .as_mapping()
        .ok_or_else(|| Error::rejected(format!("{} [hosted] must be a mapping", path.display())))?;
    serde_yaml::from_value(hosted.clone())
        .map(Some)
        .map_err(|e| {
            let culprit = mapping.iter().find_map(|(k, v)| {
                let mut one = serde_yaml::Mapping::new();
                one.insert(k.clone(), v.clone());
                serde_yaml::from_value::<Hosted>(serde_yaml::Value::Mapping(one))
                    .is_err()
                    .then(|| k.as_str().unwrap_or("?").to_string())
            });
            match culprit {
                Some(key) => Error::rejected(format!("{} [hosted] {key}: {e}", path.display())),
                None => Error::rejected(format!("{} [hosted]: {e}", path.display())),
            }
        })
}

/// Which provider a `hosted.lease` spec names.
pub(super) enum Spec {
    Off,
    File(PathBuf),
    Http(String),
}

impl Spec {
    /// `file:<path>` is the local double; `http(s)://` names the company
    /// DO; `none`/`off`/empty disable. Anything else is a typo — the
    /// daemon must not guess at a scheme it does not know.
    pub(super) fn parse(raw: &str) -> Result<Spec> {
        if let Some(path) = raw.strip_prefix("file:") {
            let path = path.trim();
            if path.is_empty() {
                return Err(Error::rejected(
                    "hosted.lease 'file:' needs a path — e.g. file:/run/cadence/lease.json",
                ));
            }
            return Ok(Spec::File(PathBuf::from(path)));
        }
        if raw.starts_with("http://") || raw.starts_with("https://") {
            return match raw {
                "http://lease.internal" | "http://lease.internal/" | HOST_RENEW_URL => {
                    Ok(Spec::Http(HOST_RENEW_URL.to_string()))
                }
                _ => Err(Error::rejected(
                    "hosted.lease HTTP endpoint must be http://lease.internal/renew without credentials, port, query or fragment",
                )),
            };
        }
        match raw {
            "" | "none" | "off" => Ok(Spec::Off),
            _ => Err(Error::rejected(format!(
                "hosted.lease '{raw}' is not file:<path>, http://lease.internal or off"
            ))),
        }
    }
}

/// The last epoch this state dir's daemon committed under — an
/// acquire-time hint so a wiped lease record still cannot hand back a
/// colliding epoch.
pub(super) fn epoch_hint(state_dir: &Path) -> u64 {
    std::fs::read_to_string(state_dir.join("lease-epoch"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|last| last.saturating_add(1))
        .unwrap_or(0)
}

/// Record the granted epoch for the next start's hint.
pub(super) fn save_epoch(state_dir: &Path, epoch: u64) {
    let _ = std::fs::write(state_dir.join("lease-epoch"), format!("{epoch}\n"));
}
