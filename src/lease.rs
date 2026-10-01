//! CAD-538: hosted lifecycle — the lease that makes a hosted daemon's
//! writes safe.
//!
//! A hosted daemon is not alone with its state: the platform can kill
//! it (SIGKILL, an OOM, a failed health check) and start a replacement
//! while the old process still believes it owns the store. The lease
//! is the single-writer contract behind that:
//!
//! - `serve` must [`acquire`] the lease before the store opens or any
//!   state write lands; a daemon that cannot take it refuses to start.
//! - A heartbeat [`LeaseCtl::renew`]s it well inside its TTL.
//! - The first failed or expired renewal trips the shared [`Fence`] —
//!   `Store::write_conn` and every leased `Pm` then refuse every write,
//!   before a byte moves. Reads stay up so the fenced daemon can still
//!   be diagnosed.
//! - On a clean stop the daemon checkpoints the WAL and commits the
//!   tracker's staged index within [`Hosted::flush_timeout_secs`], then
//!   releases the lease last — only once it will write no more.
//!
//! `hosted:` in `pm.yaml` enables it — it is off by default:
//!
//! ```yaml
//! hosted:
//!   lease: file:/run/cadence/lease.json
//!   lease_ttl_secs: 30        # how long a hold survives a kill
//!   lease_renew_secs: 10      # heartbeat; must be under the TTL
//!   flush_timeout_secs: 10    # SIGTERM flush bound
//! ```
//!
//! `lease:` takes `file:<path>` (the local test double — real atomic
//! single-writer semantics over one filesystem), `http://lease.internal`
//! (AgenticOS host-bound renewal; generation is not exposed), or
//! `none`/`off` — the default, which changes nothing: there
//! is deliberately no `none` provider object because "no provider" is
//! the no-op.

use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};

/// `hosted.lease_ttl_secs` when unset — how long a SIGKILLed daemon's
/// lease keeps contenders out.
pub const DEFAULT_TTL_SECS: u64 = 30;
/// `hosted.flush_timeout_secs` when unset — the SIGTERM flush bound.
pub const DEFAULT_FLUSH_SECS: u64 = 10;
/// AgenticOS's local renewal policy, not a host-issued expiration. The
/// platform retains claim/revoke authority and the container fences itself.
const HOST_TTL: Duration = Duration::from_secs(6);
const HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const HOST_RENEW_URL: &str = "http://lease.internal/renew";

/// Unix epoch seconds, sub-second — lease expiry is wall-clock.
fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// A held lease: who holds it, which writer-generation it is, and when
/// it lapses. File epochs order writers across restarts. AgenticOS does
/// not expose its generation: `None` must never become a synthetic epoch.
#[derive(Clone, Debug)]
pub struct Lease {
    pub holder: String,
    pub epoch: Option<u64>,
    pub expires_unix: f64,
    /// Host renewal deadlines are monotonic; wall time is diagnostic only.
    pub expires_monotonic: Option<Instant>,
}

/// Where leases come from. The daemon knows only this seam — the file
/// provider is the local double; the company DO lands behind `http`.
pub trait Provider: Send + Sync {
    /// Take the lease for `holder` or refuse. `epoch_hint` is the epoch
    /// after the highest this daemon ever held (0 = none on record), so
    /// a provider whose state was wiped still hands out an epoch ahead
    /// of any commit this writer may have made.
    fn acquire(&self, holder: &str, epoch_hint: u64) -> Result<Lease>;
    /// Extend `lease`'s expiry. Must fail when the lease was lost or
    /// expired — the heartbeat turns that failure into the write fence.
    /// A failure [`is_transient_lease_error`] reports is a blip, not
    /// loss: the caller must surface it without fencing.
    fn renew(&self, lease: &Lease) -> Result<Lease>;
    /// Give the lease up early; a crash is covered by expiry anyway.
    fn release(&self, lease: &Lease) -> Result<()>;
    /// One line for logs and `health`.
    fn describe(&self) -> String;
    /// Extra `status_json` fields — `{}` unless the provider has
    /// retry state worth reporting (the HTTP provider's `retrying` /
    /// `last_renew_error`). Never overrides the base keys.
    fn status_extra(&self) -> Value {
        json!({})
    }
}

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
    expires_monotonic: Mutex<Option<Instant>>,
    /// Validity below which a tracker commit is refused or cancelled
    /// (CAD-694), in ms; derived from the lease's own margin.
    commit_floor_ms: AtomicU64,
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

    fn set_lease(&self, lease: &Lease) {
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
    provider: Arc<dyn Provider>,
    spec: String,
    current: Mutex<Lease>,
    fence: Arc<Fence>,
    epoch: Option<Arc<AtomicU64>>,
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
fn holder_for(state_dir: &Path) -> String {
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

/// Which provider a `hosted.lease` spec names.
enum Spec {
    Off,
    File(PathBuf),
    Http(String),
}

impl Spec {
    /// `file:<path>` is the local double; `http(s)://` names the company
    /// DO; `none`/`off`/empty disable. Anything else is a typo — the
    /// daemon must not guess at a scheme it does not know.
    fn parse(raw: &str) -> Result<Spec> {
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
                "http://lease.internal" | "http://lease.internal/" | HOST_RENEW_URL =>
                    Ok(Spec::Http(HOST_RENEW_URL.to_string())),
                _ => Err(Error::rejected("hosted.lease HTTP endpoint must be http://lease.internal/renew without credentials, port, query or fragment")),
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
fn epoch_hint(state_dir: &Path) -> u64 {
    std::fs::read_to_string(state_dir.join("lease-epoch"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|last| last.saturating_add(1))
        .unwrap_or(0)
}

/// Record the granted epoch for the next start's hint.
fn save_epoch(state_dir: &Path, epoch: u64) {
    let _ = std::fs::write(state_dir.join("lease-epoch"), format!("{epoch}\n"));
}

/// Resolve `hosted:` and take the lease. `Ok(None)` is hosted mode off
/// — nothing in the daemon changes. Any lease configured but not
/// granted is `Err`: a hosted daemon never starts unleased.
pub fn acquire(state_dir: &Path, hosted: &Hosted) -> Result<Option<Arc<LeaseCtl>>> {
    acquire_with_endpoint(state_dir, hosted, None)
}

/// [`acquire`] with a test-only HTTP endpoint override (CAD-702): when
/// the configured spec is HTTP and `endpoint_override` is `Some`, the
/// renewal transport dials it instead of `lease.internal` — a loopback
/// stub in tests. The configured spec is still parsed first, so the
/// endpoint restrictions hold on what operators configure; only
/// in-process `ServeOptions` ever sets the override, never pm.yaml,
/// the RPC surface, or the environment.
pub(crate) fn acquire_with_endpoint(
    state_dir: &Path,
    hosted: &Hosted,
    endpoint_override: Option<&str>,
) -> Result<Option<Arc<LeaseCtl>>> {
    let Some(raw) = hosted.lease.as_deref().map(str::trim) else {
        return Ok(None);
    };
    let spec = Spec::parse(raw)?;
    let host_bound = matches!(&spec, Spec::Http(_));
    let ttl = Duration::from_secs(
        hosted
            .lease_ttl_secs
            .unwrap_or(if host_bound {
                HOST_TTL.as_secs()
            } else {
                DEFAULT_TTL_SECS
            })
            .max(1),
    );
    if host_bound && ttl > HOST_TTL {
        return Err(Error::rejected(
            "host-bound lease TTL must not exceed the 6s local renewal deadline",
        ));
    }
    let renew_every = match hosted.lease_renew_secs {
        Some(secs) => {
            let every = Duration::from_secs(secs.max(1));
            if every >= ttl {
                return Err(Error::rejected(format!(
                    "hosted.lease_renew_secs ({secs}) must stay under the lease TTL \
                     ({}s) — a heartbeat that slow cannot hold the lease",
                    ttl.as_secs()
                )));
            }
            every
        }
        // A third of the TTL: one missed or late beat never lapses.
        None => Duration::from_secs_f64((ttl.as_secs_f64() / 3.0).max(0.2)),
    };
    // CAD-694: the flush must end while the lease is still ours even if
    // renewals stop — a commit admitted late may not outlive the TTL
    // (HOST_TTL 6s vs the 10s default would otherwise allow it). Clamp
    // to the room one missed beat leaves.
    let margin = ttl.saturating_sub(renew_every);
    let flush_timeout = Duration::from_secs(
        hosted
            .flush_timeout_secs
            .unwrap_or(DEFAULT_FLUSH_SECS)
            .max(1),
    )
    .min(margin)
    .max(Duration::from_secs(1).min(margin));
    let (provider, spec_name): (Arc<dyn Provider>, String) = match spec {
        Spec::Off => return Ok(None),
        Spec::File(path) => (Arc::new(FileProvider::new(path, ttl)), raw.to_string()),
        Spec::Http(url) => {
            let url = endpoint_override.unwrap_or(url.as_str()).to_string();
            (
                Arc::new(HttpProvider::new(url, ttl)),
                HOST_RENEW_URL.to_string(),
            )
        }
    };
    let ctl = start_lease(state_dir, provider, spec_name, renew_every, flush_timeout)?;
    // A commit needs the SIGTERM grace and its kill to fit inside the
    // validity one missed beat leaves: half the margin, at most the
    // default — so even ttl 2 / renew 1 still flushes.
    ctl.fence.commit_floor_ms.store(
        DEFAULT_COMMIT_FLOOR.min(margin / 2).as_millis() as u64,
        Ordering::SeqCst,
    );
    Ok(Some(ctl))
}

fn start_lease(
    state_dir: &Path,
    provider: Arc<dyn Provider>,
    spec_name: String,
    renew_every: Duration,
    flush_timeout: Duration,
) -> Result<Arc<LeaseCtl>> {
    let holder = holder_for(state_dir);
    let hint = epoch_hint(state_dir);
    let lease = provider.acquire(&holder, hint)?;
    if let Some(epoch) = lease.epoch {
        save_epoch(state_dir, epoch);
    }
    let ctl = Arc::new(LeaseCtl {
        provider,
        spec: spec_name,
        epoch: lease.epoch.map(|epoch| Arc::new(AtomicU64::new(epoch))),
        current: Mutex::new(lease.clone()),
        fence: Arc::new(Fence::default()),
        renew_every,
        flush_timeout,
    });
    ctl.fence.set_lease(&lease);
    Ok(ctl)
}

// ---------- file provider ----------

/// The durable record `file:<path>` carries — rewritten atomically
/// (tmp + fsync + rename) on every grant and renewal.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct FileBody {
    holder: String,
    epoch: u64,
    expires_unix: f64,
}

impl FileBody {
    fn expired(&self, now: f64) -> bool {
        self.expires_unix <= now
    }
}

/// How long a `flock` waits on a held lock before failing closed —
/// a wedged holder (a SIGSTOPed contender, a hung filesystem) must
/// not park a renewal or the shutdown join forever.
const LOCK_WAIT: Duration = Duration::from_secs(2);

/// The local test double: one file as the lease, one `<path>.lock`
/// `flock` as the compare-and-swap. Mutual exclusion and expiry
/// take-over are real — only the transport is fake.
pub struct FileProvider {
    path: PathBuf,
    ttl: Duration,
    /// Wall clock — real time in production; tests inject a clock they
    /// advance by hand so expiry cases never sleep on real seconds.
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
}

impl FileProvider {
    pub fn new(path: PathBuf, ttl: Duration) -> Self {
        Self {
            path,
            ttl,
            clock: Arc::new(now_unix),
        }
    }

    #[cfg(test)]
    fn with_clock(path: PathBuf, ttl: Duration, clock: Arc<Mutex<f64>>) -> Self {
        Self {
            path,
            ttl,
            clock: Arc::new(move || *clock.lock().unwrap()),
        }
    }

    fn lock_path(&self) -> PathBuf {
        self.path.with_extension(format!(
            "{}.lock",
            self.path
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_default()
        ))
    }

    /// Read the record; a corrupt file is an error, not a free lease —
    /// guessing wrong here is how two writers end up holding it.
    fn body(&self) -> Result<Option<FileBody>> {
        match std::fs::read_to_string(&self.path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
            Ok(text) => serde_json::from_str(&text).map(Some).map_err(|e| {
                Error::internal(format!("{} is not a lease file: {e}", self.path.display()))
            }),
        }
    }

    /// Write the record atomically — a reader never sees a partial
    /// body, and a crash cannot leave half a lease.
    fn write_body(&self, body: &FileBody) -> Result<()> {
        let tmp = self.path.with_file_name(format!(
            ".{}.tmp.{}",
            self.path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "lease".to_string()),
            std::process::id()
        ));
        let text = serde_json::to_string(body)
            .map_err(|e| Error::internal(format!("lease record: {e}")))?;
        std::fs::write(&tmp, text)?;
        File::open(&tmp)?.sync_all()?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// The critical section: `flock` on `<path>.lock` — alive across
    /// lease-file replace/remove since the lock file itself is stable.
    /// LOCK_EX|LOCK_NB retried for [`LOCK_WAIT`]: a live holder's
    /// critical section is microseconds, so transient contention rides
    /// through; a wedged holder fails closed instead of parking the
    /// heartbeat — and with it the SIGTERM join — forever.
    fn locked<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.lock_path())?;
        self.flock_ex(&file)?;
        let out = f();
        let _ = flock(&file, libc::LOCK_UN);
        out
    }

    /// Take the lock or fail closed inside [`LOCK_WAIT`].
    fn flock_ex(&self, file: &File) -> Result<()> {
        let deadline = std::time::Instant::now() + LOCK_WAIT;
        loop {
            match flock(file, libc::LOCK_EX | libc::LOCK_NB) {
                Ok(()) => return Ok(()),
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(Error::rejected(format!(
                        "lease lock {} is held past {LOCK_WAIT:?} — the holder is \
                         wedged or dying: {e}",
                        self.lock_path().display()
                    )))
                }
            }
        }
    }
}

/// `flock(2)` on `file` — the raw io error, so callers can tell
/// `EWOULDBLOCK` (lock held) from a real failure.
fn flock(file: &File, op: i32) -> std::io::Result<()> {
    let rc = unsafe { libc::flock(file.as_raw_fd(), op) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

impl Provider for FileProvider {
    fn acquire(&self, holder: &str, epoch_hint: u64) -> Result<Lease> {
        self.locked(|| {
            let now = (self.clock)();
            if let Some(body) = self.body()? {
                if !body.expired(now) && body.holder != holder {
                    return Err(Error::rejected(format!(
                        "lease {} is held by {} until {:.0}",
                        self.path.display(),
                        body.holder,
                        body.expires_unix
                    )));
                }
                let epoch = body.epoch.saturating_add(1).max(epoch_hint.max(1));
                let body = FileBody {
                    holder: holder.to_string(),
                    epoch,
                    expires_unix: now + self.ttl.as_secs_f64(),
                };
                self.write_body(&body)?;
                return Ok(Lease {
                    holder: body.holder,
                    epoch: Some(body.epoch),
                    expires_unix: body.expires_unix,
                    expires_monotonic: None,
                });
            }
            let epoch = epoch_hint.max(1);
            let body = FileBody {
                holder: holder.to_string(),
                epoch,
                expires_unix: now + self.ttl.as_secs_f64(),
            };
            self.write_body(&body)?;
            Ok(Lease {
                holder: body.holder,
                epoch: Some(epoch),
                expires_unix: body.expires_unix,
                expires_monotonic: None,
            })
        })
    }

    fn renew(&self, lease: &Lease) -> Result<Lease> {
        self.locked(|| {
            let now = (self.clock)();
            let body = self.body()?.ok_or_else(|| {
                Error::rejected(format!("lease {} vanished", self.path.display()))
            })?;
            if body.holder != lease.holder || Some(body.epoch) != lease.epoch {
                return Err(Error::rejected(format!(
                    "lease {} is held by {} at epoch {} — we hold {} at epoch {:?}",
                    self.path.display(),
                    body.holder,
                    body.epoch,
                    lease.holder,
                    lease.epoch
                )));
            }
            if body.expired(now) {
                return Err(Error::rejected(format!(
                    "lease {} expired at {:.0} — the take-over window is open",
                    self.path.display(),
                    body.expires_unix
                )));
            }
            let next = FileBody {
                expires_unix: now + self.ttl.as_secs_f64(),
                ..body
            };
            self.write_body(&next)?;
            Ok(Lease {
                holder: next.holder,
                epoch: Some(next.epoch),
                expires_unix: next.expires_unix,
                expires_monotonic: None,
            })
        })
    }

    fn release(&self, lease: &Lease) -> Result<()> {
        self.locked(|| {
            match self.body()? {
                // Only the holder may let it go — a take-over since our
                // last renew means the file is theirs now. The record
                // stays, expired: epochs are monotone per lease, so the
                // next holder must take epoch+1, never a fresh 1.
                Some(body) if body.holder == lease.holder && Some(body.epoch) == lease.epoch => {
                    self.write_body(&FileBody {
                        expires_unix: 0.0,
                        ..body
                    })?;
                }
                _ => {}
            }
            Ok(())
        })
    }

    fn describe(&self) -> String {
        format!("file:{}", self.path.display())
    }
}

// ---------- AgenticOS host-bound HTTP provider ----------

/// Admission/renewal only: the host has already claimed this container.
/// Host routing supplies company and instance; request fields cannot claim
/// another identity. This adapter never revokes that host claim on release.
///
/// Renewal policy (CAD-702): `409` is definite loss and latches at once.
/// Any other failure — a 5xx, a timeout, a transport blip — retries while
/// the six-second local deadline from the last `204` stays open: the
/// failure surfaces (marked [`TRANSIENT_PREFIX`]) without fencing, and a
/// later `204` heals. Only a deadline with no `204` fences. Each attempt
/// keeps a budget of at most two seconds.
struct HttpProvider {
    url: String,
    ttl: Duration,
    agent: ureq::Agent,
    state: Mutex<HttpState>,
}

#[derive(Default)]
struct HttpState {
    deadline: Option<Instant>,
    lost: Option<String>,
    /// The latest blip inside the open deadline — cleared by a `204`,
    /// so health can tell retrying apart from loss.
    last_error: Option<String>,
}

/// A renewal attempt's verdict: admitted, definitely lost (`409` or past
/// the local deadline — latch `lost`), or a blip to retry while the
/// deadline from the last `204` stays open.
enum RenewAttempt {
    Admitted(Lease),
    Lost(String),
    Blip(String),
}

/// Prefix marking a renewal failure as a blip, not loss. [`LeaseCtl`]
/// and the heartbeat consult [`is_transient_lease_error`] instead of
/// fencing on it; the file provider never emits it, so its
/// first-failure fence is unchanged.
pub(crate) const TRANSIENT_PREFIX: &str = "host lease transient: ";

fn transient_lease_error(message: impl Into<String>) -> Error {
    Error::rejected(format!("{TRANSIENT_PREFIX}{}", message.into()))
}

/// Whether a renewal failure is a retryable blip inside the local
/// deadline rather than lease loss.
pub(crate) fn is_transient_lease_error(error: &Error) -> bool {
    matches!(error, Error::Rejected(message) if message.starts_with(TRANSIENT_PREFIX))
}

impl HttpProvider {
    /// Only Spec::parse supplies production URLs. Tests construct an isolated
    /// loopback peer; no runtime flag or environment variable enables it.
    fn new(url: String, ttl: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            // Ambient proxy settings must not reroute this identity-bound door.
            .proxy(None)
            .timeout_global(Some(HTTP_TIMEOUT))
            .build();
        Self {
            url,
            ttl,
            agent: config.into(),
            state: Mutex::new(HttpState::default()),
        }
    }

    /// Admission fails closed on any non-204, as before: with no prior
    /// `204` there is no deadline to retry inside, so the first failure
    /// latches and the daemon refuses to start unleased.
    fn admission(&self, holder: &str, state: &mut HttpState) -> Result<Lease> {
        if let Some(reason) = &state.lost {
            return Err(Error::rejected(format!(
                "host lease permanently fenced: {reason}"
            )));
        }
        match self.post_renew(holder, state.deadline) {
            RenewAttempt::Admitted(lease) => {
                state.deadline = lease.expires_monotonic;
                state.last_error = None;
                Ok(lease)
            }
            RenewAttempt::Lost(message) | RenewAttempt::Blip(message) => {
                state.lost = Some(message.clone());
                Err(Error::rejected(message))
            }
        }
    }

    /// Renewal inside the local deadline: `409` latches at once; any
    /// other failure is a blip while the deadline stays open (surfaced
    /// without fencing, healable by a later `204`); a deadline with no
    /// `204` latches without spending another host attempt.
    fn readmission(&self, holder: &str, state: &mut HttpState) -> Result<Lease> {
        if let Some(reason) = &state.lost {
            return Err(Error::rejected(format!(
                "host lease permanently fenced: {reason}"
            )));
        }
        match self.post_renew(holder, state.deadline) {
            RenewAttempt::Admitted(lease) => {
                state.deadline = lease.expires_monotonic;
                state.last_error = None;
                Ok(lease)
            }
            RenewAttempt::Lost(message) => {
                state.lost = Some(message.clone());
                Err(Error::rejected(message))
            }
            RenewAttempt::Blip(message) => {
                if state
                    .deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    let message = "host lease renewal deadline expired".to_string();
                    state.lost = Some(message.clone());
                    return Err(Error::rejected(message));
                }
                state.last_error = Some(message.clone());
                Err(transient_lease_error(message))
            }
        }
    }

    fn post_renew(&self, holder: &str, previous: Option<Instant>) -> RenewAttempt {
        let started = Instant::now();
        let remaining = previous
            .map(|deadline| deadline.saturating_duration_since(started))
            .unwrap_or(self.ttl);
        if remaining.is_zero() {
            return RenewAttempt::Lost("host lease renewal deadline expired".into());
        }
        let budget = HTTP_TIMEOUT.min(remaining);
        let response = match self
            .agent
            .post(&self.url)
            .config()
            .timeout_global(Some(budget))
            .build()
            .send_empty()
        {
            Ok(response) => response,
            Err(_) => {
                return RenewAttempt::Blip(
                    "host lease renewal transport failed or timed out".into(),
                );
            }
        };
        let completed = Instant::now();
        if completed.duration_since(started) >= budget
            || previous.is_some_and(|deadline| completed >= deadline)
        {
            return RenewAttempt::Lost("host lease renewal arrived after its deadline".into());
        }
        match response.status().as_u16() {
            204 => {
                let deadline = started + self.ttl;
                RenewAttempt::Admitted(Lease {
                    holder: holder.to_string(),
                    epoch: None,
                    expires_unix: now_unix()
                        + deadline.saturating_duration_since(completed).as_secs_f64(),
                    expires_monotonic: Some(deadline),
                })
            }
            409 => RenewAttempt::Lost("host lease lost (HTTP 409)".into()),
            status => RenewAttempt::Blip(format!(
                "host lease renewal refused (HTTP {status}; only 204 admits)"
            )),
        }
    }
}

impl Provider for HttpProvider {
    fn acquire(&self, holder: &str, _epoch_hint: u64) -> Result<Lease> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.admission(holder, &mut state)
    }

    fn renew(&self, lease: &Lease) -> Result<Lease> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.deadline.is_none() {
            return Err(Error::rejected("host lease has not been admitted"));
        }
        // Supplied identity/generation/expiry fields never affect host routing
        // or the deadline. The provider's admitted deadline is authoritative.
        self.readmission(&lease.holder, &mut state)
    }

    fn release(&self, _lease: &Lease) -> Result<()> {
        Ok(())
    }

    fn describe(&self) -> String {
        self.url.clone()
    }

    fn status_extra(&self) -> Value {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        json!({
            "retrying": state.lost.is_none() && state.last_error.is_some(),
            "last_renew_error": state.last_error.clone(),
        })
    }
}

#[cfg(test)]
mod http_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn file_lease_is_single_writer() {
        let dir = dir();
        let path = dir.path().join("lease.json");
        let a = FileProvider::new(path.clone(), Duration::from_secs(30));
        let b = FileProvider::new(path, Duration::from_secs(30));
        let la = a.acquire("a", 0).unwrap();
        assert_eq!(la.epoch, Some(1));
        // A second live holder is refused.
        let e = b.acquire("b", 0).unwrap_err();
        assert!(e.to_string().contains("held by a"), "{e}");
        // Release frees it for the next writer — epoch moves on.
        a.release(&la).unwrap();
        let lb = b.acquire("b", 0).unwrap();
        assert_eq!(lb.epoch, Some(2));
    }

    /// A clock the test advances by hand — expiry cases never wait on
    /// real milliseconds, so they cannot flake under a loaded host.
    fn clock() -> Arc<Mutex<f64>> {
        Arc::new(Mutex::new(1_000.0f64))
    }

    #[test]
    fn file_lease_takeover_after_expiry() {
        let dir = dir();
        let path = dir.path().join("lease.json");
        let clock = clock();
        let a = FileProvider::with_clock(path.clone(), Duration::from_secs(30), clock.clone());
        let b = FileProvider::with_clock(path, Duration::from_secs(30), clock.clone());
        let la = a.acquire("a", 0).unwrap(); // expires at 1030
        *clock.lock().unwrap() = 1_040.0;
        // a never released but its TTL lapsed — b takes over at +1.
        let lb = b.acquire("b", 0).unwrap();
        assert_eq!(lb.epoch, la.epoch.map(|epoch| epoch + 1));
        // The dead holder cannot renew the lease it lost.
        assert!(a.renew(&la).is_err());
    }

    #[test]
    fn file_lease_renew_extends_and_validates() {
        let dir = dir();
        let path = dir.path().join("lease.json");
        let clock = clock();
        let a = FileProvider::with_clock(path.clone(), Duration::from_secs(30), clock.clone());
        let lease = a.acquire("a", 0).unwrap(); // expires at 1030
        *clock.lock().unwrap() = 1_010.0;
        let next = a.renew(&lease).unwrap(); // expires at 1040
        assert_eq!(next.epoch, lease.epoch);
        assert!(next.expires_unix > lease.expires_unix);
        // A forged renewal — right holder name, wrong epoch — fails.
        let mut forged = next.clone();
        forged.epoch = forged.epoch.map(|epoch| epoch + 1);
        assert!(a.renew(&forged).is_err());
        // A vanished file fails renewal, not takeover.
        std::fs::remove_file(&path).unwrap();
        assert!(a.renew(&next).is_err());
        // And a renewal that arrives after its own expiry is refused —
        // the take-over window is open, so claiming the lease now would
        // split the brain.
        let b = FileProvider::with_clock(path.clone(), Duration::from_secs(30), clock.clone());
        let lb = b.acquire("b", 0).unwrap();
        *clock.lock().unwrap() = 1_100.0;
        assert!(b.renew(&lb).is_err());
    }

    /// A wedged holder — a SIGSTOPed contender or a hung filesystem —
    /// parks a blocking `flock` forever. The provider retries LOCK_NB
    /// for `LOCK_WAIT`, then fails closed: acquire refuses, renewal
    /// surfaces the failure, and no caller (heartbeat join included)
    /// can hang on the lock.
    #[test]
    fn file_lease_held_lock_fails_closed() {
        let dir = dir();
        let path = dir.path().join("lease.json");
        let p = FileProvider::new(path, Duration::from_secs(30));
        let wedged = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(p.lock_path())
            .unwrap();
        flock(&wedged, libc::LOCK_EX).unwrap();
        let start = std::time::Instant::now();
        let e = p.acquire("a", 0).unwrap_err();
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "acquire parked on the wedged lock"
        );
        assert!(e.to_string().contains("lease lock"), "{e}");
    }

    #[test]
    fn file_lease_corrupt_body_fails_closed() {
        let dir = dir();
        let path = dir.path().join("lease.json");
        std::fs::write(&path, "not json").unwrap();
        let a = FileProvider::new(path, Duration::from_secs(30));
        assert!(a.acquire("a", 0).is_err());
    }

    #[test]
    fn file_lease_epoch_hint_survives_state_loss() {
        let dir = dir();
        let path = dir.path().join("lease.json");
        let a = FileProvider::new(path.clone(), Duration::from_secs(30));
        let la = a.acquire("a", 0).unwrap();
        a.release(&la).unwrap();
        // The lease record is gone but this daemon wrote epoch 41 last
        // time — the next grant cannot collide with it.
        let lb = a.acquire("a", 42).unwrap();
        assert_eq!(lb.epoch, Some(42));
    }

    #[test]
    fn spec_parse_rules() {
        assert!(matches!(Spec::parse("off").unwrap(), Spec::Off));
        assert!(matches!(Spec::parse("none").unwrap(), Spec::Off));
        assert!(matches!(
            Spec::parse("file:/tmp/l.json").unwrap(),
            Spec::File(p) if p == Path::new("/tmp/l.json")
        ));
        assert!(matches!(
            Spec::parse("http://lease.internal").unwrap(),
            Spec::Http(u) if u == HOST_RENEW_URL
        ));
        assert!(Spec::parse("file:").is_err());
        assert!(Spec::parse("socket:/x").is_err());
    }

    #[test]
    fn foreign_http_endpoint_refuses_before_state_writes() {
        let dir = dir();
        let hosted = Hosted {
            lease: Some("https://do.example/lease".into()),
            ..Hosted::default()
        };
        let e = acquire(dir.path(), &hosted).err().unwrap();
        assert!(e.to_string().contains("lease.internal"), "{e}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn fence_trips_once_and_reports_first_reason() {
        let fence = Fence::default();
        assert!(!fence.tripped());
        fence.trip("first");
        fence.trip("second");
        assert_eq!(fence.reason().as_deref(), Some("first"));
    }

    /// The expiry tripwire: no detector needed — a fence whose lease
    /// has lapsed refuses writes on its own, and a renewal that lands
    /// later moves the refusal with it.
    #[test]
    fn fence_refuses_writes_once_the_lease_expires() {
        let fence = Fence::default();
        assert!(fence.check().is_none(), "unarmed fence must not refuse");
        fence.set_expiry(now_unix() + 60.0);
        assert!(fence.check().is_none(), "live lease must not refuse");
        fence.set_expiry(now_unix() - 1.0);
        assert!(fence.check().unwrap().contains("expired"));
        // A successful renewal re-arms it — expiry follows the lease,
        // not the wall.
        fence.set_expiry(now_unix() + 60.0);
        assert!(fence.check().is_none());
        // An explicit trip still wins.
        fence.trip("renewal failed");
        assert_eq!(fence.reason().as_deref(), Some("renewal failed"));
        assert!(fence.check().is_some());
    }

    #[test]
    fn acquire_builds_ctl_and_records_epoch() {
        let dir = dir();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let hosted = Hosted {
            lease: Some(format!("file:{}", dir.path().join("l").display())),
            lease_ttl_secs: Some(5),
            lease_renew_secs: Some(1),
            flush_timeout_secs: Some(3),
        };
        let ctl = acquire(&state, &hosted).unwrap().unwrap();
        assert_eq!(ctl.epoch(), Some(1));
        assert!(state.join("lease-epoch").is_file());
        assert_eq!(ctl.renew_every, Duration::from_secs(1));
        // renew >= ttl is refused, not clamped into uselessness.
        let bad = Hosted {
            lease: Some(format!("file:{}", dir.path().join("m").display())),
            lease_ttl_secs: Some(5),
            lease_renew_secs: Some(5),
            flush_timeout_secs: None,
        };
        assert!(acquire(&state, &bad).is_err());
        // Off is off: no provider, no files.
        assert!(acquire(&state, &Hosted::default()).unwrap().is_none());
    }

    /// CAD-694: the flush bound never outlasts the room one missed
    /// renewal leaves — the production defaults (TTL 6s, flush 10s)
    /// would otherwise let a late commit outlive the lease.
    #[test]
    fn flush_timeout_is_clamped_under_the_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let hosted = Hosted {
            lease: Some(format!("file:{}", dir.path().join("l.json").display())),
            lease_ttl_secs: Some(6),
            lease_renew_secs: Some(2),
            flush_timeout_secs: Some(10),
        };
        let ctl = acquire(dir.path(), &hosted).unwrap().unwrap();
        assert_eq!(ctl.flush_timeout, Duration::from_secs(4));
        assert_eq!(ctl.fence().commit_floor(), DEFAULT_COMMIT_FLOOR);
        let relaxed = Hosted {
            flush_timeout_secs: Some(3),
            ..hosted
        };
        let dir2 = tempfile::tempdir().unwrap();
        let relaxed = Hosted {
            lease: Some(format!("file:{}", dir2.path().join("l.json").display())),
            ..relaxed
        };
        let ctl = acquire(dir2.path(), &relaxed).unwrap().unwrap();
        assert_eq!(ctl.flush_timeout, Duration::from_secs(3));
    }

    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::TempDir::new().unwrap()
    }

    /// CAD-694: a short but valid lease (TTL 2s, renew 1s) keeps a commit
    /// floor under the validity one missed beat leaves, so the stop
    /// flush can still commit — a fixed 2s floor would never be met.
    #[test]
    fn short_leases_derive_a_commit_floor_they_can_meet() {
        let dir = tempfile::tempdir().unwrap();
        let hosted = Hosted {
            lease: Some(format!("file:{}", dir.path().join("l.json").display())),
            lease_ttl_secs: Some(2),
            lease_renew_secs: Some(1),
            flush_timeout_secs: Some(10),
        };
        let ctl = acquire(dir.path(), &hosted).unwrap().unwrap();
        let floor = ctl.fence().commit_floor();
        assert_eq!(floor, Duration::from_millis(500));
        // Validity right before the next beat is still above the floor.
        assert!(floor < Duration::from_secs(2) - ctl.renew_every);
        assert_eq!(ctl.flush_timeout, Duration::from_secs(1));
    }
}
