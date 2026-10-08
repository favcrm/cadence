//! CAD-534: `cadence daemon` serve RPC handlers — moved verbatim from src/daemon.rs.

pub(super) mod accept_wait;

use super::*;

use std::cell::Cell;
use std::fs::{File, OpenOptions};
use std::io::BufReader;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// CAD-538 heartbeat under CAD-702 renewal policy, started by CAD-947
/// immediately after the lease is acquired — before the store opens or
/// recovery runs — so startup work of any length rides a renewed lease.
///
/// Renews every `renew_every` until stopped. `serve` stops it only
/// after the shutdown flush completes, so the WAL checkpoint and
/// tracker flush (and a deliberately slow test flush) always run under
/// exactly one renewal poster; dropping it (an early startup failure)
/// stops and joins it too, so a failed start leaves no poster behind
/// and the lease simply expires. A permanent failure trips the fence
/// and ends the loop; a transient blip — marked by the HTTP provider
/// while the local deadline stays open — is logged and retried on the
/// next beat without fencing.
pub(super) struct LeaseHeartbeat {
    stop: Arc<AtomicBool>,
    /// Set once `Shared` exists, so a trip can also wait out the
    /// in-flight store writer; before that the shared fence alone
    /// refuses every write the store will make.
    shared: Arc<OnceLock<Arc<Shared>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl LeaseHeartbeat {
    pub(super) fn start(state_dir: &Path, lease: &Arc<crate::lease::LeaseCtl>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let shared: Arc<OnceLock<Arc<Shared>>> = Arc::new(OnceLock::new());
        let handle = {
            let (stop, shared, lease) = (Arc::clone(&stop), Arc::clone(&shared), Arc::clone(lease));
            let state_dir = state_dir.to_path_buf();
            // Named `lh-` + the state dir's last 12 name chars, so a test
            // can observe its own poster's lifetime against the lease
            // release (15 chars: the Linux `comm` limit).
            thread::Builder::new()
                .name(Self::thread_name(&state_dir))
                .spawn(move || Self::run(&state_dir, &lease, &stop, &shared))
                .expect("spawn lease heartbeat")
        };
        Self {
            stop,
            shared,
            handle: Some(handle),
        }
    }

    /// The poster's OS thread name for `state_dir`.
    pub(super) fn thread_name(state_dir: &Path) -> String {
        let leaf = state_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tail: String = leaf
            .chars()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("lh-{tail}")
    }

    /// Hand the heartbeat the daemon it keeps leased.
    pub(super) fn attach(&self, shared: &Arc<Shared>) {
        let _ = self.shared.set(Arc::clone(shared));
    }

    /// Stop renewing and wait for the poster to exit. Idempotent.
    pub(super) fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    fn run(
        state_dir: &Path,
        lease: &Arc<crate::lease::LeaseCtl>,
        stop: &AtomicBool,
        shared: &OnceLock<Arc<Shared>>,
    ) {
        while !stop.load(Ordering::SeqCst) {
            let deadline = Instant::now() + lease.renew_every;
            while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
            match lease.renew() {
                Ok(()) => {}
                Err(error) if crate::lease::is_transient_lease_error(&error) => {
                    eprintln!("cadence: hosted lease renewal blip — retrying: {error}");
                    tracing::warn!("hosted lease renewal blip — retrying: {error}");
                }
                Err(error) => {
                    let why = format!("lease renewal failed: {error}");
                    match shared.get() {
                        Some(shared) => shared.trip_lease(why),
                        None => {
                            lease.fence().trip(why.clone());
                            record_lease_loss(state_dir, lease, &why);
                        }
                    }
                    return;
                }
            }
        }
    }
}

impl Drop for LeaseHeartbeat {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The forensic record of a lost lease, in the state dir — the daemon's
/// own file, not the leased store: every store write including the
/// event stream is refused once the fence trips.
fn record_lease_loss(state_dir: &Path, lease: &crate::lease::LeaseCtl, why: &str) {
    let fact = json!({"reason": why, "epoch": lease.epoch(), "at": epoch_secs()});
    let _ = std::fs::write(
        state_dir.join("lease-fence.json"),
        serde_json::to_string_pretty(&fact).unwrap_or_default(),
    );
    eprintln!("cadence: hosted lease lost — daemon fenced: {why}");
    tracing::warn!("hosted lease lost — daemon fenced: {why}");
}

impl Shared {
    /// CAD-538: the moment the lease is known lost — trip the shared
    /// fence (`fence_writes` waits out the in-flight writer so no write
    /// ordered after this can still run ungated), then leave the
    /// forensic record in the state dir.
    fn trip_lease(&self, why: String) {
        self.store.fence_writes(why.clone());
        if let Some(lease) = &self.lease {
            record_lease_loss(&self.state_dir, lease, &why);
        }
    }
}

// This entire RPC dispatch tree is synchronous: one OS thread per
// accepted connection, no async task migration. The scope is set for
// each frame and restored on drop. In configured UID mode, a missing
// scope refuses; a future async refactor must explicitly carry this
// credential instead of falling back to ambient operator identity.
thread_local! {
    static FRAME_PEER_UID: Cell<Option<u32>> = const { Cell::new(None) };
}

pub(super) struct PeerUidScope(Option<u32>);

impl PeerUidScope {
    fn enter(uid: u32) -> Self {
        FRAME_PEER_UID.with(|slot| Self(slot.replace(Some(uid))))
    }
}

impl Drop for PeerUidScope {
    fn drop(&mut self) {
        FRAME_PEER_UID.with(|slot| slot.set(self.0));
    }
}

pub(super) fn frame_peer_uid(configured: Option<u32>) -> Result<u32> {
    if configured.is_none() {
        return Ok(unsafe { libc::geteuid() });
    }
    FRAME_PEER_UID.with(|slot| {
        slot.get()
            .ok_or_else(|| Error::rejected("socket peer UID context missing"))
    })
}

#[derive(Clone, Copy)]
struct PeerCred {
    pid: u32,
    uid: u32,
}

/// Reject peers outside the exact kernel-credential UID admit set;
/// the pid still derives slot and approval-answer caller identity.
#[cfg(any(target_os = "linux", test))]
fn admit_peer_uid(peer_uid: u32, daemon_uid: u32, agent_uid: Option<u32>) -> bool {
    if agent_uid.is_some_and(|uid| daemon_uid == 0 || uid == 0 || uid == daemon_uid) {
        return false;
    }
    peer_uid == daemon_uid || agent_uid == Some(peer_uid)
}

#[cfg(target_os = "linux")]
fn check_peer(stream: &UnixStream, agent_uid: Option<u32>) -> Result<PeerCred> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return Err(Error::internal("Cannot determine socket peer credentials"));
    }
    if len as usize != std::mem::size_of::<libc::ucred>()
        || cred.pid <= 0
        || !admit_peer_uid(cred.uid, unsafe { libc::geteuid() }, agent_uid)
    {
        return Err(Error::rejected("Socket peer UID or PID is not admitted"));
    }
    Ok(PeerCred {
        pid: cred.pid as u32,
        uid: cred.uid,
    })
}

/// Off Linux there is no `SO_PEERCRED`: refuse every peer (fail closed)
/// until the macOS port (CAD-315) brings a verified equivalent. The
/// daemon does not start there anyway — see `reaper::enable`.
#[cfg(not(target_os = "linux"))]
fn check_peer(_stream: &UnixStream, _agent_uid: Option<u32>) -> Result<PeerCred> {
    Err(Error::rejected(
        "socket peer credentials are only checked on Linux; the daemon is Linux-only \
         until the macOS port (CAD-315)",
    ))
}

/// Linux process-start identity for an endpoint pid. The daemon stores the
/// registration discriminator separately; this request-time provenance
/// catches a stale or reused pid before issuing a receipt.
pub(super) fn process_start_identity(pid: u32) -> Result<u64> {
    crate::peer::proc_starttime(pid).ok_or_else(|| {
        Error::rejected(format!(
            "Cannot read native endpoint process start for pid {pid} (/proc/{pid}/stat)"
        ))
    })
}

pub(super) fn handle_conn(shared: Arc<Shared>, stream: UnixStream) {
    let Ok(peer) = check_peer(&stream, shared.agent_uid) else {
        return;
    };
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let _peer_scope = PeerUidScope::enter(peer.uid);
        let response = serde_json::from_str::<Value>(&line)
            .map_err(|_| Error::rejected("Request must be one JSON object per line"))
            .and_then(|frame| {
                let method = frame
                    .get("method")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::rejected("Missing 'method'"))?;
                let params = frame.get("params").cloned().unwrap_or(json!({}));
                // CAD-482: an armed fixture reads `test_caller`; the
                // asserted identity — and nothing ambient — is the
                // caller for this one dispatch. The field is refused
                // everywhere else.
                let _scope = crate::test_seam::scope_frame(shared.seam.as_ref(), &frame)?;
                shared.dispatch(method, &params, peer.pid)
            });
        let frame = match response {
            Ok(result) => proto::ok(result),
            Err(error) => proto::err(&error),
        };
        if writeln!(writer, "{frame}").is_err() {
            break;
        }
    }
}

/// The second socket lives outside the private state directory, so
/// different state-dir daemons need one global lifetime lock. A stale
/// socket is removed only while this lock is held and only after its
/// owner/mode/type and refused connection prove it is inert.
pub(super) struct SharedSocket {
    pub listener: UnixListener,
    path: PathBuf,
    _lock: File,
}

impl Drop for SharedSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(super) fn bind_shared_socket(path: &Path, gid: u32, fixture: bool) -> Result<SharedSocket> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::rejected("shared socket path has no parent"))?;
    let parent_meta = std::fs::symlink_metadata(parent)?;
    let euid = unsafe { libc::geteuid() };
    if !parent_meta.is_dir()
        || parent_meta.uid() != euid
        || parent_meta.mode() & 0o022 != 0
        || (!fixture && (parent_meta.gid() != gid || parent_meta.mode() & 0o050 != 0o050))
    {
        return Err(Error::rejected(
            "shared socket parent owner, group or mode refused",
        ));
    }
    let lock_path = parent.join("cadence-socket.lock");
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(lock_path)?;
    let meta = lock.metadata()?;
    if !meta.is_file() || meta.uid() != euid || meta.mode() & 0o077 != 0 {
        return Err(Error::rejected("shared socket lock owner or mode refused"));
    }
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::rejected("another daemon owns the shared socket"));
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.file_type().is_socket()
                || meta.uid() != euid
                || meta.gid() != gid
                || meta.mode() & 0o777 != 0o660
                || UnixStream::connect(path)
                    .err()
                    .is_none_or(|error| error.kind() != std::io::ErrorKind::ConnectionRefused)
            {
                return Err(Error::rejected(
                    "shared socket is active or its provenance is refused",
                ));
            }
            std::fs::remove_file(path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(path)?;
    let socket = SharedSocket {
        listener,
        path: path.to_path_buf(),
        _lock: lock,
    };
    // The protected parent and held lock make this path stable until
    // the bind is fully prepared; the agent group cannot rename it.
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::rejected("shared socket path contains NUL"))?;
    if unsafe { libc::chown(cpath.as_ptr(), euid, gid) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    socket.listener.set_nonblocking(true)?;
    Ok(socket)
}

/// Exclusive lifetime ownership of the state directory. The lock file is
/// held for the whole `serve` call; a second daemon fails here before it
/// can touch the store, the socket, or any actor.
pub(super) fn acquire_singleton(state_dir: &Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(state_dir.join("cadence.lock"))?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err(Error::rejected(
            "Another cadence daemon already owns this state directory",
        ));
    }
    Ok(file)
}

/// What the CAD-484 checkup calls to dispatch a picked ticket to a
/// lane: `(shared, pm_dir, issue, alias) → dispatch::run`'s out map.
/// Production binds `issue::dispatch::run`; a test returns a recorded
/// stub. The seam injects behavior, not authority — `dispatch_one`
/// re-validates the ticket under `dispatch_lock` before calling it.
pub(super) type CheckupDispatch =
    dyn Fn(&Arc<Shared>, &Path, &str, &str) -> Result<Value> + Send + Sync;

/// CAD-538: the `hosted:` table — `ServeOptions.lease` verbatim when
/// set (a test pins `Some(Hosted::default())` for explicitly-off), else
/// the tracker's pm.yaml. Absent is off; present-but-unreadable refuses
/// — a daemon configured to lease must never start unleased on a typo.
pub(super) fn hosted_config(opts: &ServeOptions) -> Result<crate::lease::Hosted> {
    if let Some(hosted) = &opts.lease {
        return Ok(hosted.clone());
    }
    let pm_dir = match opts.provider_env.var("CADENCE_PM_DIR") {
        Some(dir) if !dir.is_empty() => crate::home::guard_tracker(PathBuf::from(dir))?,
        _ => crate::issue::default_dir()?,
    };
    crate::doctor::host::read_hosted_overrides(&pm_dir)
        .map(|o| o.unwrap_or_default())
        .map_err(Error::internal)
}

/// The SIGTERM flush bound — the held lease's `flush_timeout`, else
/// `[hosted] flush_timeout_secs`, else the default. (The flush runs on
/// every clean stop; the bound exists whether or not a lease did.)
pub(super) fn flush_budget(
    hosted: &crate::lease::Hosted,
    lease: Option<&crate::lease::LeaseCtl>,
) -> Duration {
    if let Some(lease) = lease {
        return lease.flush_timeout;
    }
    Duration::from_secs(
        hosted
            .flush_timeout_secs
            .unwrap_or(crate::lease::DEFAULT_FLUSH_SECS)
            .max(1),
    )
}

/// CAD-538: the stop-time flush — fold the store's WAL back into the db
/// and, on a leased daemon, commit whatever the tracker's index still
/// stages — on one thread bounded by `budget`, so a wedged filesystem
/// or hung git can never hold a signal hostage. The WAL fold runs on
/// every clean stop (the store is always the daemon's own); the tracker
/// half is leased-only — an unleased daemon's `pm_dir` may be the
/// operator's own `~/pm`, which a routine stop must never commit into.
/// A fenced daemon's tracker half refuses like every write; the WAL
/// fold is the flush of what it committed while it still held the lease.
/// `slow` is a test-only dwell (CAD-702) at the flush's start, proving
/// renewal spans a slow flush; production passes zero.
pub(super) fn lease_flush(
    shared: &Arc<Shared>,
    budget: Duration,
    slow: Duration,
    gate: Option<crate::daemon::FlushGate>,
    done: Option<crate::daemon::FlushGate>,
) -> bool {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let leased = shared.lease.is_some();
    let shared = Arc::clone(shared);
    let cancel = Arc::new(crate::issue::FlushCancel::default());
    let worker_cancel = Arc::clone(&cancel);
    let worker = thread::spawn(move || {
        let cancel = worker_cancel;
        // Test seam (CAD-694): a parked gate models a flush still in
        // flight when the budget lapses — the tail must then keep the
        // lease rather than release under a live writer.
        if let Some(gate) = gate {
            gate();
        }
        if !slow.is_zero() {
            thread::sleep(slow);
        }
        match shared.store.checkpoint() {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("cadence: shutdown flush — WAL checkpoint deferred (a reader held it)")
            }
            Err(e) => eprintln!("cadence: shutdown flush — WAL checkpoint failed: {e}"),
        }
        if leased {
            match shared
                .pm()
                .map(|pm| pm.flush_pending_cancellable(DAEMON_ALIAS, &cancel))
            {
                Err(e) => eprintln!("cadence: shutdown flush — tracker flush skipped: {e}"),
                Ok(Err(e)) => eprintln!("cadence: shutdown flush — tracker flush refused: {e}"),
                Ok(Ok(_)) => {}
            }
        }
        let _ = tx.send(());
        // Test seam (CAD-694): the worker's completion receipt, so a
        // test owns the late completion instead of racing it.
        if let Some(done) = done {
            done();
        }
    });
    // The send is the worker's completion proof — every write it owns
    // (checkpoint, the synchronous `git commit`) finished by then.
    // `false` means the worker may still be writing: callers must not
    // release the lease under it.
    if rx.recv_timeout(budget).is_err() {
        eprintln!("cadence: shutdown flush exceeded {budget:?} — completion unproven");
        // The budget lapsed: the worker may still own a `git commit`.
        // Cancel it and wait (bounded) for the worker to unwind, so no
        // commit of ours can land after the lease transfers by TTL.
        // Release stays withheld either way: completion is unproven.
        cancel.cancel();
        let unwind = Instant::now() + FLUSH_CANCEL_JOIN;
        while !worker.is_finished() && Instant::now() < unwind {
            thread::sleep(Duration::from_millis(10));
        }
        if worker.is_finished() {
            let _ = worker.join();
        } else {
            eprintln!(
                "cadence: shutdown flush worker still running after cancel — \
                 not inside the tracker commit (the WAL checkpoint, the read-only \
                 staged probe or a parked seam)"
            );
        }
        return false;
    }
    true
}

/// How long the tail waits for a cancelled flush worker to unwind: the
/// commit's SIGTERM grace plus its reap, with margin.
const FLUSH_CANCEL_JOIN: Duration = Duration::from_millis(2500);

/// Slot configuration precedence: explicit `ServeOptions.slots`, then
/// `[host]` in the repo's pm.yaml, then the built-in defaults.
pub(super) fn resolve_slot_config(opts: &ServeOptions) -> SlotConfig {
    if let Some(c) = &opts.slots {
        return c.clone();
    }
    let mut c = SlotConfig::default();
    let overrides = crate::issue::default_dir()
        .ok()
        .as_deref()
        .and_then(crate::doctor::host::host_overrides);
    if let Some(o) = overrides {
        if let Some(v) = o.build_slots {
            c.build_slots = v as usize;
        }
        if let Some(v) = o.suite_slots {
            c.suite_slots = v as usize;
        }
        if let Some(v) = o.jobs_per_lane {
            c.jobs_per_lane = v as usize;
        }
        if let Some(v) = o.starve_secs {
            c.starve_secs = v;
        }
        if let Some(v) = o.priority_lanes {
            c.priority_lanes = v;
        }
        if let Some(v) = o.max_hold_secs {
            c.max_hold_secs = v;
        }
        // CAD-1021: the check pool + resource floors.
        if let Some(v) = o.check_slots {
            c.check_slots = v as usize;
        }
        if let Some(v) = o.slot_mem_min_available_bytes {
            c.slot_mem_min_available_bytes = v;
        }
        if let Some(v) = o.slot_mem_min_available_check_bytes {
            c.slot_mem_min_available_check_bytes = v;
        }
        if let Some(v) = o.slot_disk_min_free_bytes {
            c.slot_disk_min_free_bytes = v;
        }
    }
    c
}

// ---- Hot restart (CAD-89): clean-stop marker + instance files ----
//
// The daemon's last act always writes `shutdown.json`: a provably
// clean shutdown records each pty turn still `running`; a FAILED drain
// records the error instead (CAD-694 — the restart verdict must read a
// lost-evidence stop, never mistake it for a clean one). Either marker
// names the daemon run that wrote it (`daemon-instance`, recorded at
// serve start). On the next start the marker is consumed exactly once:
// the recorded ENTRIES are adoption candidates, valid only against the
// immediately preceding recorded run and only within MARKER_TTL — stale
// entries take the historical fence path. A recorded drain FAILURE is
// different evidence — provenance, not freshness: it stays set whenever
// the marker names the immediately preceding run, even past the TTL.

/// The last recorded serve() run's instance id.
pub const INSTANCE_FILE: &str = "daemon-instance";

/// The clean-shutdown marker: running pty turns awaiting re-adoption.
pub(super) const SHUTDOWN_FILE: &str = "shutdown.json";

/// This run's recovery record (CAD-694): what `recover()` fenced and
/// the consumed marker's provenance. A `daemon restart` verdict reads
/// it instead of relying on per-alias event cursors a dead predecessor
/// may never have allowed — and it names the run that wrote it, so a
/// record from an earlier start cannot stand in for this one's.
pub const LAST_RECOVERY_FILE: &str = "last-recovery.json";

/// How long a shutdown marker stays adoptable — a bound on pane
/// longevity, not on restart speed. Past it the recorded checks would
/// read stale pane state as fresh; the turns fence instead.
const MARKER_TTL_SECS: f64 = 900.0;

/// What `serve()` carries into `Shared`: this run's instance id plus
/// the consumed marker (entries and a staleness reason when the marker
/// itself failed validation).
pub struct HotStart {
    pub instance: String,
    pub(super) marker: Option<store::ConsumedMarker>,
}

impl HotStart {
    /// No marker — tests and every non-serve `Shared::new`.
    pub fn fresh() -> Self {
        Self {
            instance: Uuid::new_v4().simple().to_string(),
            marker: None,
        }
    }
}

/// Small-string atomic write: tmp file in the same directory, then
/// rename — a reader never sees a torn marker.
fn write_file_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn read_instance(state_dir: &Path) -> Option<String> {
    std::fs::read_to_string(state_dir.join(INSTANCE_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Read, validate and DELETE the shutdown marker — consume-once: a
/// daemon that crashes after this point leaves nothing to adopt, which
/// is exactly the crash path. The new run's instance id is recorded
/// immediately after, so `marker.instance` must equal the PREVIOUS
/// recorded start to count as provably clean.
pub(super) fn hot_restart_begin(state_dir: &Path) -> HotStart {
    let previous = read_instance(state_dir);
    let path = state_dir.join(SHUTDOWN_FILE);
    let raw = std::fs::read_to_string(&path).ok();
    // Consume-once regardless of what the marker says — a stale or
    // unreadable marker must never be adopted twice.
    let _ = std::fs::remove_file(&path);
    let marker = raw.and_then(|raw| match serde_json::from_str::<Value>(&raw) {
        Ok(v) => {
            let instance = v["instance"].as_str().unwrap_or_default().to_string();
            let at = v["at"].as_f64().unwrap_or(0.0);
            let entries = v["entries"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|r| {
                            Some(store::AdoptEntry {
                                alias: r["alias"].as_str()?.to_string(),
                                message_id: r["message_id"].as_str()?.to_string(),
                                turn_id: r["turn_id"].as_str()?.to_string(),
                                generation: r["generation"].as_str()?.to_string(),
                                pane_pid: r["pane_pid"].as_u64()? as u32,
                                native_session: r["native_session"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            // Provenance (does this marker name the run just gone?)
            // is a separate axis from freshness (is it still inside
            // the pane-adoption TTL?): expiry must not erase a recorded
            // drain failure, and a wrong-instance marker must not lend
            // its failure to this shutdown (CAD-694).
            let proven = !instance.is_empty() && Some(instance.as_str()) == previous.as_deref();
            let stale = if !proven {
                Some("shutdown marker does not match the last recorded daemon run".to_string())
            } else if epoch_secs() - at > MARKER_TTL_SECS {
                Some(format!(
                    "shutdown marker expired ({:.0}s old, bound {:.0}s)",
                    epoch_secs() - at,
                    MARKER_TTL_SECS
                ))
            } else {
                None
            };
            // `failed` is evidence whenever the marker names the
            // immediately preceding run — TTL expiry bounds pane
            // adoption, never the provenance of a recorded failure.
            let failed = if proven {
                v["failed"].as_str().map(str::to_string)
            } else {
                None
            };
            Some(store::ConsumedMarker {
                instance,
                entries,
                stale,
                failed,
            })
        }
        Err(_) => {
            eprintln!("hot-restart: unreadable shutdown marker discarded");
            None
        }
    });
    let instance = Uuid::new_v4().simple().to_string();
    if let Err(e) = write_file_atomic(&state_dir.join(INSTANCE_FILE), &instance) {
        eprintln!("hot-restart: could not record daemon instance: {e}");
    }
    HotStart { instance, marker }
}

/// The last write of a clean shutdown — after this the daemon exits.
/// Never fails the stop itself: a marker that can't be written is a
/// crash-equivalent state dir, which the next start already handles.
pub(super) fn write_shutdown_marker(
    state_dir: &Path,
    instance: &str,
    entries: Vec<store::AdoptEntry>,
) {
    let marker = json!({
        "instance": instance,
        "at": epoch_secs(),
        "entries": entries.iter().map(|e| json!({
            "alias": e.alias,
            "message_id": e.message_id,
            "turn_id": e.turn_id,
            "generation": e.generation,
            "pane_pid": e.pane_pid,
            "native_session": e.native_session,
        })).collect::<Vec<_>>(),
    });
    if let Err(e) = write_file_atomic(&state_dir.join(SHUTDOWN_FILE), &marker.to_string()) {
        eprintln!("hot-restart: could not write shutdown marker: {e}");
    }
}

/// The last write of a FAILED shutdown drain (CAD-694): the store
/// refused the refusal events, so the file is the only channel left —
/// the next start's `recover()` fences every in-flight row it sweeps,
/// and the restart verdict cannot read the stop as clean. Like
/// `write_shutdown_marker` it never fails the stop itself: a marker
/// that cannot be written is a crash-equivalent state dir.
pub(super) fn write_failed_shutdown_marker(state_dir: &Path, instance: &str, error: &str) {
    let marker = json!({
        "instance": instance,
        "at": epoch_secs(),
        "failed": error,
    });
    if let Err(e) = write_file_atomic(&state_dir.join(SHUTDOWN_FILE), &marker.to_string()) {
        eprintln!("hot-restart: could not write failed-shutdown marker: {e}");
    }
}

/// Persist what this start's `recover()` fenced — the successor-bound
/// drain evidence a `daemon restart` verdict reads in place of the
/// per-alias event cursors a predecessor already dead cannot have
/// provided (CAD-694). Rewritten on every start, bound to `instance`,
/// so a later restart reads only this run's recovery. A failed write
/// can leave an OLDER record in place — the verdict therefore binds to
/// the instance its own start returned and rejects any record (and any
/// instance file) naming a different run.
pub(crate) fn write_recovery_record(
    state_dir: &Path,
    instance: &str,
    outcome: &store::RecoveryOutcome,
) {
    let consumed = outcome.marker_instance.as_ref().map(|id| {
        json!({
            "instance": id,
            "stale": outcome.stale,
            "failed": outcome.failed,
        })
    });
    let fence_rows = |rows: &[(String, String)]| -> Vec<Value> {
        rows.iter()
            .map(|(alias, message_id)| json!({"alias": alias, "message_id": message_id}))
            .collect()
    };
    let record = json!({
        "instance": instance,
        "consumed": consumed,
        "fenced": fence_rows(&outcome.fenced),
        "unevidenced": fence_rows(&outcome.unevidenced),
    });
    if let Err(e) = write_file_atomic(&state_dir.join(LAST_RECOVERY_FILE), &record.to_string()) {
        eprintln!("hot-restart: could not write recovery record: {e}");
    }
}

/// CAD-1247: one start-up line per provider command this daemon would
/// fail to exec — pi first (CAD-1247's incident: a minimal service
/// PATH left `pi` unresolvable and every pi agent fenced with only a
/// provider log saying why). Resolved against the daemon's own PATH
/// exactly as the adapter's `execvp` would; a warning only — a daemon
/// whose workload never touches the provider must still start.
fn warn_unresolved_provider_commands(env: &ProviderEnv) {
    let path = std::env::var("PATH").ok();
    if let Err(reason) = crate::adapter::pi::resolve_pi_launch_command(env, path.as_deref()) {
        eprintln!("provider pi: {reason}; set CADENCE_PI_COMMAND");
    }
}

/// Relaunch enabled actors at daemon start; fenced ones land in
/// `attention` instead.
pub(super) fn relaunch_agents(shared: &Arc<Shared>) -> Result<()> {
    warn_unresolved_provider_commands(&shared.provider_env);
    // Inbox rows are durable mailboxes — enabled or not, they own no
    // actor and keep their pseudo-endpoint across restarts.
    for agent in shared.store.agents()? {
        if !agent.enabled || !registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            // No actor can prove these recovered panes; release any
            // matching report waiter to fail closed against recovery state.
            shared.adoptions_settle(&agent.alias);
            continue;
        }
        // A fenced agent stays registered but must never churn on a
        // daemon restart — no actor, no provider process. `attention`
        // or an unreconciled `unknown` both mean the operator must
        // reconcile before it runs again. The fenced state and its
        // recovery hint are preserved/restored, then `relaunch_skipped`
        // is emitted instead of a launch.
        let unknown = shared.store.has_unknown(&agent.alias)?;
        if agent.state == "attention" || unknown {
            // A kept-for-adoption `running` message whose agent still
            // fences — a sibling in-flight message swept it into
            // `unknown` — has no actor left to prove the pane.
            // Unverified `running` is just `unknown`: fence it too.
            // Discard its adoption candidates as well — a resume after
            // unfence must be an ordinary open (fresh generation), not
            // an adopt of entries whose panes were never re-proven.
            let _ = shared
                .store
                .orphan_running(&agent.alias, "agent fenced at restart; turn never verified");
            if let Some(entries) = shared.store.take_adoption(&agent.alias) {
                for e in entries {
                    let _ = shared.store.event_public(
                        &agent.alias,
                        "turn_adopt_refused",
                        json!({"message": e.message_id,
                               "turn_id": e.turn_id,
                               "reason": "agent fenced at restart"}),
                    );
                }
            }
            let (reason, error) = if unknown {
                (
                    "unknown messages await reconcile",
                    shared.uncertain_fence_text(&agent.alias),
                )
            } else {
                // Other fences (session mismatch, failed open) keep
                // their recorded error verbatim.
                (
                    "agent is in attention",
                    agent.error.clone().unwrap_or_default(),
                )
            };
            shared
                .store
                .set_state_detached(&agent.alias, "attention", Some(&error))?;
            eprintln!("start: skipping fenced agent '{}' ({reason})", agent.alias);
            let _ = shared.store.event_public(
                &agent.alias,
                "relaunch_skipped",
                json!({"reason": reason}),
            );
            // The recovery candidate was discarded with the fence; no
            // actor will perform its adoption proof on this start.
            shared.adoptions_settle(&agent.alias);
            continue;
        }
        shared.launch_actor(&agent.alias)?;
    }
    Ok(())
}

#[cfg(test)]
mod agent_uid_tests {
    use super::{admit_peer_uid, bind_shared_socket, frame_peer_uid, PeerUidScope};
    use crate::daemon::{ServeOptions, Shared};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn configured_agent_uid_is_admitted_but_foreign_and_root_are_refused() {
        assert!(admit_peer_uid(1000, 1000, Some(2000)));
        assert!(admit_peer_uid(2000, 1000, Some(2000)));
        assert!(!admit_peer_uid(2000, 1000, None));
        assert!(!admit_peer_uid(0, 1000, Some(2000)));
        assert!(!admit_peer_uid(3000, 1000, Some(2000)));
        assert!(!admit_peer_uid(1000, 1000, Some(1000)));
        assert!(admit_peer_uid(0, 0, None));
        assert!(!admit_peer_uid(0, 0, Some(2000)));
    }

    #[test]
    fn held_agent_frame_cannot_bleed_into_concurrent_operator_frame() {
        assert!(frame_peer_uid(Some(2000)).is_err());
        let barrier = Arc::new(Barrier::new(2));
        let workers: Vec<_> = [1000u32, 2000u32]
            .into_iter()
            .map(|uid| {
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let _held = PeerUidScope::enter(uid);
                    barrier.wait();
                    assert_eq!(frame_peer_uid(Some(2000)).unwrap(), uid);
                    let _nested = PeerUidScope::enter(uid + 10);
                    assert_eq!(frame_peer_uid(Some(2000)).unwrap(), uid + 10);
                    drop(_nested);
                    assert_eq!(frame_peer_uid(Some(2000)).unwrap(), uid);
                    barrier.wait();
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(frame_peer_uid(Some(2000)).is_err());
    }

    #[test]
    fn forged_operator_assertion_cannot_upgrade_agent_uid_frame() {
        let root = tempfile::tempdir().unwrap();
        let opts = ServeOptions {
            agent_uid: Some(2000),
            ..ServeOptions::default()
        };
        let shared = Shared::new(root.path(), &opts).unwrap();
        crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
            assert!(shared.operator_evidence(std::process::id()).is_err());
            let _agent_frame = PeerUidScope::enter(2000);
            assert!(shared.operator_evidence(std::process::id()).is_err());
            assert!(!matches!(
                shared.connection_caller(std::process::id()).unwrap(),
                crate::daemon::caller_rule::Who::Operator
            ));
        });
    }

    #[test]
    fn shared_socket_is_group_only_and_stale_or_active_paths_are_fenced() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("cadence.sock");
        let gid = unsafe { libc::getegid() };
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = bind_shared_socket(&path, gid, true).unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o660);
        assert_eq!(meta.gid(), gid);
        assert!(UnixStream::connect(&path).is_ok());
        assert!(bind_shared_socket(&path, gid, true).is_err());
        drop(socket);
        assert!(!path.exists());

        let stale = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660)).unwrap();
        drop(stale);
        let replacement = bind_shared_socket(&path, gid, true).unwrap();
        drop(replacement);
        assert!(!path.exists());
    }
}

#[cfg(test)]
mod hot_restart_marker_tests {
    use super::{hot_restart_begin, INSTANCE_FILE, SHUTDOWN_FILE};
    use crate::daemon::watch::epoch_secs;

    /// A state dir holding `daemon-instance` + `shutdown.json` as
    /// written — the marker's `at` is constructed, never waited out.
    fn dir_with(previous: &str, marker: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(INSTANCE_FILE), previous).unwrap();
        std::fs::write(dir.path().join(SHUTDOWN_FILE), marker).unwrap();
        dir
    }

    /// CAD-694 (review B2): the pane-adoption TTL bounds freshness, not
    /// provenance — an expired marker that still names the run just
    /// gone keeps its recorded drain failure.
    #[test]
    fn an_expired_matching_failed_marker_keeps_its_failure() {
        let at = epoch_secs() - 3600.0;
        let marker = format!(r#"{{"instance":"old-run","at":{at},"failed":"drain exploded"}}"#);
        let dir = dir_with("old-run", &marker);
        let hot = hot_restart_begin(dir.path());
        let marker = hot.marker.expect("the marker still parses");
        assert!(
            marker.stale.is_some(),
            "expired entries fence, but the marker parsed"
        );
        assert_eq!(marker.failed.as_deref(), Some("drain exploded"));
        assert_eq!(marker.instance, "old-run");
    }

    /// The same expired marker under a different recorded run is
    /// another daemon's evidence — neither its panes nor its failure
    /// may bind to this shutdown.
    #[test]
    fn a_wrong_instance_failed_marker_suppresses_its_failure() {
        let at = epoch_secs() - 3600.0;
        let marker = format!(r#"{{"instance":"other-run","at":{at},"failed":"their crash"}}"#);
        let dir = dir_with("old-run", &marker);
        let hot = hot_restart_begin(dir.path());
        let marker = hot.marker.expect("the marker still parses");
        assert!(marker.stale.is_some());
        assert_eq!(
            marker.failed, None,
            "a foreign run's failure must never fence here"
        );
    }

    /// Control: a marker still inside the TTL keeps the failure too —
    /// the change is that expiry no longer drops it.
    #[test]
    fn a_fresh_matching_failed_marker_keeps_its_failure() {
        let marker = format!(
            r#"{{"instance":"old-run","at":{},"failed":"drain exploded"}}"#,
            epoch_secs()
        );
        let dir = dir_with("old-run", &marker);
        let hot = hot_restart_begin(dir.path());
        let marker = hot.marker.expect("the marker still parses");
        assert!(marker.stale.is_none(), "fresh marker adopts");
        assert_eq!(marker.failed.as_deref(), Some("drain exploded"));
    }
}
