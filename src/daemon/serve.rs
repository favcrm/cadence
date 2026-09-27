//! CAD-534: `cadence daemon` serve RPC handlers — moved verbatim from src/daemon.rs.

use super::*;

use std::io::BufReader;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;

impl Shared {
    /// CAD-538: the hosted-lease heartbeat — renew every `renew_every`
    /// until the daemon begins closing, in 100ms sub-steps so a stop
    /// lands at once. The first failed or expired renewal is lease
    /// loss: [`Self::trip_lease`] drops the write fence before the
    /// next store or tracker write can begin, and this daemon never
    /// writes again.
    pub(super) fn run_lease_heartbeat(self: &Arc<Self>, lease: &Arc<crate::lease::LeaseCtl>) {
        while !self.closing.load(Ordering::SeqCst) {
            let deadline = Instant::now() + lease.renew_every;
            while !self.closing.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            if self.closing.load(Ordering::SeqCst) {
                break;
            }
            if let Err(e) = lease.renew() {
                self.trip_lease(format!("lease renewal failed: {e}"));
                return;
            }
        }
    }

    /// CAD-538: the moment the lease is known lost — trip the shared
    /// fence (`fence_writes` waits out the in-flight writer so no write
    /// ordered after this can still run ungated), then leave the
    /// forensic record in the state dir — the daemon's own file, not
    /// the leased store: every store write including the event stream
    /// is refused from here on.
    fn trip_lease(&self, why: String) {
        self.store.fence_writes(why.clone());
        let epoch = self.lease.as_ref().map(|l| l.epoch()).unwrap_or(0);
        let fact = json!({"reason": why, "epoch": epoch, "at": epoch_secs()});
        let _ = std::fs::write(
            self.state_dir.join("lease-fence.json"),
            serde_json::to_string_pretty(&fact).unwrap_or_default(),
        );
        eprintln!("cadence: hosted lease lost — daemon fenced: {why}");
        tracing::warn!("hosted lease lost — daemon fenced: {why}");
    }
}

/// Reject peers that are not the same Unix user; return the peer PID
/// used to derive slot and approval-answer caller identity.
#[cfg(target_os = "linux")]
fn check_peer(stream: &UnixStream) -> Result<u32> {
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
    if cred.uid != unsafe { libc::geteuid() } {
        return Err(Error::rejected("Socket peer is not the same user"));
    }
    Ok(cred.pid as u32)
}

/// Off Linux there is no `SO_PEERCRED`: refuse every peer (fail closed)
/// until the macOS port (CAD-315) brings a verified equivalent. The
/// daemon does not start there anyway — see `reaper::enable`.
#[cfg(not(target_os = "linux"))]
fn check_peer(_stream: &UnixStream) -> Result<u32> {
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
    let Ok(peer_pid) = check_peer(&stream) else {
        return;
    };
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
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
                shared.dispatch(method, &params, peer_pid)
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
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
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
pub(super) fn lease_flush(shared: &Arc<Shared>, budget: Duration) {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let leased = shared.lease.is_some();
    let shared = Arc::clone(shared);
    thread::spawn(move || {
        match shared.store.checkpoint() {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("cadence: shutdown flush — WAL checkpoint deferred (a reader held it)")
            }
            Err(e) => eprintln!("cadence: shutdown flush — WAL checkpoint failed: {e}"),
        }
        if leased {
            match shared.pm().map(|pm| pm.flush_pending(DAEMON_ALIAS)) {
                Err(e) => eprintln!("cadence: shutdown flush — tracker flush skipped: {e}"),
                Ok(Err(e)) => eprintln!("cadence: shutdown flush — tracker flush refused: {e}"),
                Ok(Ok(_)) => {}
            }
        }
        let _ = tx.send(());
    });
    if rx.recv_timeout(budget).is_err() {
        eprintln!("cadence: shutdown flush exceeded {budget:?} — exiting anyway");
    }
}

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
    }
    c
}

// ---- Hot restart (CAD-89): clean-stop marker + instance files ----
//
// A provably clean shutdown is the ONLY path that writes
// `shutdown.json`: it is the daemon's last act, after every actor has
// detached. The marker names the daemon run that wrote it
// (`daemon-instance`, recorded at serve start) and each pty turn still
// `running`. On the next start the marker is consumed exactly once —
// it is valid only against the immediately preceding recorded run and
// only within MARKER_TTL; anything else takes the historical fence
// path for the recorded agents.

/// The last recorded serve() run's instance id.
const INSTANCE_FILE: &str = "daemon-instance";

/// The clean-shutdown marker: running pty turns awaiting re-adoption.
pub(super) const SHUTDOWN_FILE: &str = "shutdown.json";

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
            let stale = if instance.is_empty() || Some(instance.as_str()) != previous.as_deref() {
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
            Some(store::ConsumedMarker { entries, stale })
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

/// Relaunch enabled actors at daemon start; fenced ones land in
/// `attention` instead.
pub(super) fn relaunch_agents(shared: &Arc<Shared>) -> Result<()> {
    // Inbox rows are durable mailboxes — enabled or not, they own no
    // actor and keep their pseudo-endpoint across restarts.
    for agent in shared.store.agents()? {
        if !agent.enabled || !registry::has_actor(&agent.provider, &agent.endpoint_kind) {
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
            continue;
        }
        shared.launch_actor(&agent.alias)?;
    }
    Ok(())
}
