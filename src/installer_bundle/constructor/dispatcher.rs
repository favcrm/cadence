//! Serial ROOT dispatcher. Admission is against an actual own-created retained
//! daemon or Pi-owned actual helper, never a selector, copied PID or RuntimeProof.
use super::super::{files, refused, Deadline, Result};
use super::{
    context,
    layout::Layout,
    lifecycle,
    private_wire::{self, Domain, Packet},
    OwnedDaemon,
};
use crate::adapter::pi_guest::{
    owner::{LaunchPermit, OwnerProfile},
    service::{PendingLaunch, RunningLaunch},
};
use crate::protected_pi_profile::authority::{
    ProcessPhase, Request as PiRequest, Response as PiResponse,
};
use crate::store::{Binding, Purpose};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
const PI: &str = "/run/cadence/private/pi-launch.sock";
const STORE: &str = "/run/cadence/private/store-owner.sock";
const DB: &str = "/srv/cadence/protected/store/cadence.db";
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Facts {
    reference: String,
    lineage_reference: String,
    launch: Value,
    binding: Binding,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    facts: Facts,
    phase: String,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum StoreRequest {
    Startup {
        version: u8,
        sequence: u64,
    },
    Acquire {
        version: u8,
        sequence: u64,
        binding: Binding,
    },
    Consume {
        version: u8,
        sequence: u64,
        grant: String,
        binding: Binding,
    },
    Current {
        version: u8,
        sequence: u64,
        grant: String,
        binding: Binding,
    },
    InitFile {
        version: u8,
        sequence: u64,
        grant: String,
        binding: Binding,
    },
    DatabaseCurrent {
        version: u8,
        sequence: u64,
        grant: String,
        binding: Binding,
    },
}
struct StoreSession {
    stream: UnixStream,
    sequence: u64,
    facts: Option<Facts>,
    phase: String,
    burned: bool,
    file_delivered: bool,
}
struct PiSession {
    stream: UnixStream,
    permit: Option<LaunchPermit>,
    running: Option<RunningLaunch>,
    operation: String,
}
struct Socket {
    path: &'static str,
    listener: UnixListener,
    stamp: (u64, u64),
}
impl Socket {
    fn bind(path: &'static str) -> Result<Self> {
        // Never unlink an unelected/stale endpoint; construction is create-new.
        if std::fs::symlink_metadata(path).is_ok() {
            return Err(refused());
        }
        let listener = UnixListener::bind(path).map_err(|_| refused())?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| refused())?;
        let name = std::ffi::CString::new(path).map_err(|_| refused())?;
        if unsafe { libc::chown(name.as_ptr(), 21000, 21000) } != 0 {
            return Err(refused());
        }
        listener.set_nonblocking(true).map_err(|_| refused())?;
        let m = std::fs::symlink_metadata(path).map_err(|_| refused())?;
        Ok(Self {
            path,
            listener,
            stamp: (m.dev(), m.ino()),
        })
    }
    fn recheck(&self) -> Result<()> {
        let m = std::fs::symlink_metadata(self.path).map_err(|_| refused())?;
        use std::os::unix::fs::FileTypeExt;
        if !m.file_type().is_socket()
            || (m.dev(), m.ino()) != self.stamp
            || m.uid() != 21000
            || m.gid() != 21000
            || m.mode() & 0o7777 != 0o600
        {
            return Err(refused());
        }
        Ok(())
    }
}
struct DbState {
    startup: Option<Facts>,
    closed: bool,
    witness: Option<Value>,
    intent: Option<Binding>,
    file: Option<File>,
    opening_peer: Option<UnixStream>,
}
struct TaskState {
    id: String,
    alias: String,
    model: String,
    part: u64,
    launch_started: bool,
    cancelled: bool,
    retiring: bool,
    retired: bool,
}
struct Registry {
    task: Mutex<Option<TaskState>>,
    task_history: Mutex<std::collections::HashSet<String>>,
    serving: OnceLock<super::serving::OwnedServing>,
    serving_stopped: std::sync::atomic::AtomicBool,
    buffered: Mutex<std::collections::VecDeque<(Packet, Vec<std::os::fd::OwnedFd>)>>,
    buffered_bytes: std::sync::atomic::AtomicUsize,
    daemon: OwnedDaemon,
    layout: Layout,
    sockets: [Socket; 2],
    reference: String,
    db: Mutex<DbState>,
}
static REGISTRY: OnceLock<Registry> = OnceLock::new();
pub(super) fn launch_live(until: Instant) -> Result<()> {
    let r = registry()?;
    r.check(until)?;
    r.serving_current(until)?;
    let task = r.task.lock().map_err(|_| refused())?;
    let task = task.as_ref().ok_or_else(refused)?;
    if task.cancelled || task.retiring || task.retired {
        return Err(refused());
    }
    Ok(())
}
pub(super) fn note_commands(commands: &[lifecycle::Command]) -> Result<()> {
    let Some(r) = REGISTRY.get() else {
        return Ok(());
    };
    let mut t = r.task.lock().map_err(|_| refused())?;
    if let Some(t) = t.as_mut() {
        for command in commands {
            if matches!(command,lifecycle::Command::Cancel{task}|lifecycle::Command::Retire{task} if task==&t.id)
            {
                t.cancelled = true;
            }
        }
    }
    Ok(())
}
fn registry() -> Result<&'static Registry> {
    REGISTRY.get().ok_or_else(refused)
}
impl Registry {
    fn serving_current(&self, until: Instant) -> Result<Value> {
        use std::sync::atomic::Ordering;
        self.sample_control()?;
        if self.serving_stopped.load(Ordering::SeqCst) {
            return Err(refused());
        }
        let proof = self.serving.get().ok_or_else(refused)?;
        proof.recheck(&self.daemon, &self.layout, until)?;
        self.database()?;
        let facts = {
            let db = self.db.lock().map_err(|_| refused())?;
            if db.closed {
                return Err(refused());
            }
            db.startup.clone().ok_or_else(refused)?
        };
        // Independent committed WAL-aware READ_ONLY identity/schema32/open-latch
        // observation, never the same blocked Store mutex or a recovery writer.
        super::capture::database_current(&facts.binding, until)?;
        let (bootstrap, _, _, _) = context::runtime_parts()?;
        let a = &bootstrap.manifest.artifacts;
        proof.recheck(&self.daemon, &self.layout, until)?;
        self.database()?;
        self.sample_control()?;
        if self.serving_stopped.load(Ordering::SeqCst) {
            return Err(refused());
        }
        Ok(
            json!({"version":1,"reference":self.reference,"revision":1,"databaseReference":facts.reference,"helperSha256":a.helper,"nodeSha256":a.node,"profileSha256":a.pi_graph,"policySha256":a.policy}),
        )
    }
    // During a provider owner await, sample terminal LOCAL control NOW, not
    // only after its reply. Retain all other actual packets/FDs in bounded FIFO
    // for the outer serial dispatcher: never recursively exchange this channel.
    fn sample_control(&self) -> Result<()> {
        use std::sync::atomic::Ordering;
        let mut queue = self.buffered.lock().map_err(|_| refused())?;
        while let Some((packet, fds)) = private_wire::receive(&self.daemon.control)? {
            match packet {
                Packet::ServingStopped { version: 1 } => {
                    self.serving_stopped.store(true, Ordering::SeqCst);
                }
                Packet::Failed { .. } => {
                    self.serving_stopped.store(true, Ordering::SeqCst);
                    return Err(refused());
                }
                other @ (Packet::Accepted { version: 1, .. }
                | Packet::Serving { version: 1 }
                | Packet::TaskEvent { version: 1, .. }
                | Packet::Retired { version: 1, .. }
                | Packet::Closed { version: 1, .. }
                | Packet::Witnessed { version: 1, .. }) => {
                    let size = serde_json::to_vec(&other).map_err(|_| refused())?.len();
                    let total = self
                        .buffered_bytes
                        .load(Ordering::SeqCst)
                        .checked_add(size)
                        .ok_or_else(refused)?;
                    if queue.len() >= 4096 || total > 262144 {
                        return Err(refused());
                    }
                    queue.push_back((other, fds));
                    self.buffered_bytes.store(total, Ordering::SeqCst);
                }
                _ => return Err(refused()),
            }
        }
        Ok(())
    }
    fn take_packet(&self) -> Result<Option<(Packet, Vec<std::os::fd::OwnedFd>)>> {
        let mut queue = self.buffered.lock().map_err(|_| refused())?;
        if let Some((packet, fds)) = queue.pop_front() {
            self.buffered_bytes.fetch_sub(
                serde_json::to_vec(&packet).map_err(|_| refused())?.len(),
                std::sync::atomic::Ordering::SeqCst,
            );
            Ok(Some((packet, fds)))
        } else {
            private_wire::receive(&self.daemon.control)
        }
    }
    fn check(&self, until: Instant) -> Result<()> {
        self.daemon.recheck(until)?;
        self.layout.recheck(Deadline(until))?;
        for s in &self.sockets {
            s.recheck()?
        }
        Ok(())
    }
    fn admit(&self, stream: &UnixStream, until: Instant) -> Result<()> {
        self.check(until)?;
        self.daemon.require_peer(stream, until)?;
        self.check(until)
    }
    fn create_init(&self, facts: &Facts, stream: &UnixStream, until: Instant) -> Result<()> {
        // Only this original locally burned + externally consumed Init session
        // reaches here, after original activation-current. Root is the actual
        // creator; a path/FD/UID observation can never elect an existing file.
        self.admit(stream, until)?;
        facts.binding.validate()?;
        if facts.binding.purpose != Purpose::Init {
            return Err(refused());
        }
        fresh_target()?;
        let mut state = self.db.lock().map_err(|_| refused())?;
        if state.closed || state.startup.is_some() || state.file.is_some() {
            return Err(refused());
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(DB)
            .map_err(|_| refused())?;
        // A failure from here leaves a spent partial file. Never delete/retry.
        if unsafe { libc::fchown(file.as_raw_fd(), 21000, 21000) } != 0
            || unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0
        {
            return Err(refused());
        }
        self.layout.store_file(&file)?;
        let m = file.metadata().map_err(|_| refused())?;
        if !m.is_file()
            || m.len() != 0
            || m.nlink() != 1
            || m.uid() != 21000
            || m.gid() != 21000
            || m.mode() & 0o7777 != 0o600
        {
            return Err(refused());
        }
        state.file = Some(file);
        state.startup = Some(facts.clone());
        drop(state);
        self.database()?;
        absent_sidecars()?;
        facts.binding.validate()?;
        self.admit(stream, until)
    }
    fn construction_current(&self, until: Instant) -> Result<()> {
        let facts = {
            let state = self.db.lock().map_err(|_| refused())?;
            if state.closed {
                return Err(refused());
            }
            self.admit(state.opening_peer.as_ref().ok_or_else(refused)?, until)?;
            match (&state.startup, &state.file) {
                (None, None) => None,
                (Some(facts), Some(_)) if facts.binding.purpose == Purpose::Init => {
                    Some(facts.clone())
                }
                _ => return Err(refused()), // never reset a held creation to absent.
            }
        };
        if let Some(facts) = facts {
            facts.binding.validate()?; // ORIGINAL opening deadline, not a lease.
            self.database()?;
            for suffix in ["-wal", "-shm", "-journal"] {
                let file = match OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
                    .open(format!("{DB}{suffix}"))
                {
                    Ok(file) => file,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(_) => return Err(refused()),
                };
                let m = file.metadata().map_err(|_| refused())?;
                if !m.is_file()
                    || m.uid() != 21000
                    || m.gid() != 21000
                    || m.nlink() != 1
                    || m.mode() & 0o7777 != 0o600
                {
                    return Err(refused());
                }
                self.layout.store_file(&file)?;
            }
            self.database()?;
            facts.binding.validate()?;
        } else {
            fresh_target()?; // initial election only, never existing DB admission.
        }
        self.check(until)
    }
    fn database(&self) -> Result<()> {
        let f = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(DB)
            .map_err(|_| refused())?;
        let m = f.metadata().map_err(|_| refused())?;
        if !m.is_file()
            || m.uid() != 21000
            || m.gid() != 21000
            || m.nlink() != 1
            || m.mode() & 0o7777 != 0o600
        {
            return Err(refused());
        }
        let state = self.db.lock().map_err(|_| refused())?;
        let old = state.file.as_ref().ok_or_else(refused)?;
        let held = old.metadata().map_err(|_| refused())?;
        if (held.dev(), held.ino()) != (m.dev(), m.ino()) {
            return Err(refused());
        }
        self.layout.store_file(old)?;
        self.layout.store_file(&f)?;
        Ok(())
    }
}
fn budget(until: Instant) -> Result<Duration> {
    until
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(refused)
}
fn read<T: for<'a> Deserialize<'a>>(
    stream: &mut UnixStream,
    max: usize,
    until: Instant,
) -> Result<T> {
    stream.set_nonblocking(false).map_err(|_| refused())?;
    fn exact(s: &mut UnixStream, mut b: &mut [u8], until: Instant) -> Result<()> {
        while !b.is_empty() {
            s.set_read_timeout(Some(budget(until)?))
                .map_err(|_| refused())?;
            let n = super::init_file::read_plain(s, b)?;
            if n == 0 {
                return Err(refused());
            }
            b = &mut b[n..];
        }
        Ok(())
    }
    let mut len = [0; 4];
    exact(stream, &mut len, until)?;
    let n = u32::from_be_bytes(len) as usize;
    if n == 0 || n > max {
        return Err(refused());
    }
    let mut bytes = vec![0; n];
    exact(stream, &mut bytes, until)?;
    budget(until)?;
    serde_json::from_slice(&bytes).map_err(|_| refused())
}
fn write<T: Serialize>(stream: &mut UnixStream, value: &T, until: Instant) -> Result<()> {
    let body = serde_json::to_vec(value).map_err(|_| refused())?;
    if body.is_empty() || body.len() > 8 * 1024 * 1024 {
        return Err(refused());
    }
    let mut bytes = (body.len() as u32).to_be_bytes().to_vec();
    bytes.extend(body);
    let mut bytes = bytes.as_slice();
    while !bytes.is_empty() {
        stream
            .set_write_timeout(Some(budget(until)?))
            .map_err(|_| refused())?;
        let n = stream.write(bytes).map_err(|_| refused())?;
        if n == 0 {
            return Err(refused());
        }
        bytes = &bytes[n..];
    }
    budget(until)?;
    Ok(())
}
fn ready(stream: &UnixStream) -> Result<bool> {
    let mut p = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let n = unsafe { libc::poll(&mut p, 1, 0) };
    if n < 0 || p.revents & libc::POLLNVAL != 0 {
        return Err(refused());
    }
    Ok(n > 0)
}
fn validate(
    value: Value,
    phase: &str,
    expected: Option<&Facts>,
    activation: bool,
) -> Result<Facts> {
    let state: State = serde_json::from_value(value).map_err(|_| refused())?;
    let (b, _, _, _) = context::runtime_parts()?;
    let f = state.facts;
    let launch = crate::daemon::supervisor_grant::parse_launch(&f.launch)?;
    if state.phase != phase
        || launch != b.launch
        || f.lineage_reference != b.lineage.reference
        || f.binding.database_epoch != b.lineage.database_epoch
        || f.binding.operation != b.operation
        || f.binding.path != DB
        // External Store correlation is a canonical UUID, NOT Root's hex32
        // construction reference. Preserve its exact issued bytes throughout.
        || !store_reference(&f.reference)
        || expected.is_some_and(|e| serde_json::to_value(e).ok() != serde_json::to_value(&f).ok())
    {
        return Err(refused());
    }
    if activation {
        f.binding.validate()?
    } // database-current keeps exact previously validated tuple; never renews it.
    Ok(f)
}
fn store_reference(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| {
        id.get_variant() == uuid::Variant::RFC4122
            && id.get_version() == Some(uuid::Version::Random)
            && id.hyphenated().to_string() == value
    })
}
impl StoreSession {
    fn handle(&mut self, until: Instant) -> Result<()> {
        let r = registry()?;
        r.admit(&self.stream, until)?;
        let request: StoreRequest = read(&mut self.stream, 32768, until)?;
        let (version, seq) = match &request {
            StoreRequest::Startup { version, sequence }
            | StoreRequest::Acquire {
                version, sequence, ..
            }
            | StoreRequest::Consume {
                version, sequence, ..
            }
            | StoreRequest::Current {
                version, sequence, ..
            }
            | StoreRequest::InitFile {
                version, sequence, ..
            }
            | StoreRequest::DatabaseCurrent {
                version, sequence, ..
            } => (*version, *sequence),
        };
        if version != 1
            || seq != self.sequence.checked_add(1).ok_or_else(refused)?
            || seq > super::MAX_SAFE
        {
            return Err(refused());
        }
        self.sequence = seq;
        let (mut outcome, mut phase) = ("issued", "issued");
        let is_consume = matches!(&request, StoreRequest::Consume { .. });
        let is_database = matches!(&request, StoreRequest::DatabaseCurrent { .. });
        let mut delivery = None;
        match request {
            StoreRequest::Startup { .. } if self.facts.is_none() && seq == 1 => {
                // Pilot is fresh-only. Old DB/sidecars NEVER become fresh,
                // restore/open and cold recovery remain explicitly unavailable.
                fresh_target()?;
                {
                    let mut db = r.db.lock().map_err(|_| refused())?;
                    if db.opening_peer.is_some() {
                        return Err(refused());
                    }
                    db.opening_peer = Some(self.stream.try_clone().map_err(|_| refused())?);
                }
                let f = validate(lifecycle::store_startup(until)?, "issued", None, true)?;
                if f.binding.purpose != Purpose::Init {
                    return Err(refused());
                }
                fresh_target()?;
                if r.db.lock().map_err(|_| refused())?.startup.is_some() {
                    return Err(refused());
                }
                self.facts = Some(f);
            }
            StoreRequest::Acquire { binding, .. } if self.facts.is_none() && seq == 1 => {
                binding.validate()?;
                if !matches!(binding.purpose, Purpose::Close | Purpose::Witness) {
                    return Err(refused());
                }
                let intent =
                    r.db.lock()
                        .map_err(|_| refused())?
                        .intent
                        .clone()
                        .ok_or_else(refused)?;
                if binding != intent {
                    return Err(refused());
                }
                let p = if binding.purpose == Purpose::Close {
                    lifecycle::StorePurpose::Close
                } else {
                    lifecycle::StorePurpose::Witness
                };
                let f = validate(
                    lifecycle::store_acquire(p, &binding.attempt, until)?,
                    "issued",
                    None,
                    true,
                )?;
                if f.binding != binding {
                    return Err(refused());
                }
                self.facts = Some(f);
            }
            StoreRequest::InitFile { grant, binding, .. } => {
                let f = self.facts.as_ref().ok_or_else(refused)?;
                if self.phase != "consumed"
                    || !self.burned
                    || self.file_delivered
                    || binding.purpose != Purpose::Init
                    || f.reference != grant
                    || f.binding != binding
                {
                    return Err(refused());
                }
                self.file_delivered = true; // burn BEFORE current or any response/FD write.
                validate(
                    lifecycle::store_current(&grant, until)?,
                    "consumed",
                    Some(f),
                    true,
                )?;
                r.construction_current(until)?;
                absent_sidecars()?;
                let db = r.db.lock().map_err(|_| refused())?;
                if db.startup.as_ref().is_none_or(|original| {
                    serde_json::to_value(original).ok() != serde_json::to_value(f).ok()
                }) {
                    return Err(refused());
                }
                let file = db.file.as_ref().ok_or_else(refused)?;
                if file.metadata().map_err(|_| refused())?.len() != 0 {
                    return Err(refused());
                }
                delivery = Some(file.try_clone().map_err(|_| refused())?);
                outcome = "init_file";
                phase = "consumed";
            }
            StoreRequest::Consume { grant, binding, .. }
            | StoreRequest::Current { grant, binding, .. }
            | StoreRequest::DatabaseCurrent { grant, binding, .. } => {
                let f = self.facts.as_ref().ok_or_else(refused)?;
                if f.reference != grant || f.binding != binding {
                    return Err(refused());
                }
                if is_database {
                    if self.phase != "consumed"
                        || !matches!(
                            binding.purpose,
                            Purpose::Init | Purpose::Restore | Purpose::Open
                        )
                        || r.db.lock().map_err(|_| refused())?.closed
                    {
                        return Err(refused());
                    }
                    r.database()?;
                    validate(
                        lifecycle::store_database_current(&grant, until)?,
                        "consumed",
                        Some(f),
                        false,
                    )?;
                    outcome = "database_current";
                    phase = "consumed";
                } else if is_consume {
                    if self.burned || self.phase != "issued" {
                        return Err(refused());
                    }
                    self.burned = true;
                    validate(
                        lifecycle::store_consume(&grant, until)?,
                        "consumed",
                        Some(f),
                        true,
                    )?;
                    self.phase = "consumed".into();
                    outcome = "consumed";
                    phase = "consumed";
                    if binding.purpose == Purpose::Init {
                        validate(
                            lifecycle::store_current(&grant, until)?,
                            "consumed",
                            Some(f),
                            true,
                        )?;
                        r.create_init(f, &self.stream, until)?;
                    } else if matches!(binding.purpose, Purpose::Restore | Purpose::Open) {
                        return Err(refused()); // no fresh/restore fallback in this milestone.
                    }
                } else {
                    validate(
                        lifecycle::store_current(&grant, until)?,
                        &self.phase,
                        Some(f),
                        true,
                    )?;
                    outcome = "current";
                    phase = &self.phase;
                }
            }
            _ => return Err(refused()),
        }
        r.admit(&self.stream, until)?;
        let f = self.facts.as_ref().ok_or_else(refused)?;
        write(
            &mut self.stream,
            &json!({"version":1,"sequence":seq,"grant":f.reference,"binding":f.binding,"outcome":outcome,"phase":phase}),
            until,
        )?;
        if let Some(file) = delivery {
            f.binding.validate()?;
            r.admit(&self.stream, until)?;
            r.database()?;
            super::init_file::send(&self.stream, &file, until)?;
            f.binding.validate()?;
            r.admit(&self.stream, until)?;
        }
        Ok(())
    }
}
fn absent_sidecars() -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        if !std::fs::symlink_metadata(format!("{DB}{suffix}"))
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            return Err(refused());
        }
    }
    Ok(())
}
fn fresh_target() -> Result<()> {
    super::require_absent_startup_target(std::path::Path::new(DB))
}
/// The same read-only absence predicate used at every fixed production barrier.
/// A path is filesystem DATA, not a registry, Init grant or alternate DB route.
pub(crate) fn fresh_target_at(path: &std::path::Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut target = path.as_os_str().to_os_string();
        target.push(suffix);
        if !std::fs::symlink_metadata(std::path::Path::new(&target))
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            return Err(refused());
        }
    }
    Ok(())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn accepted() -> Result<Option<(Domain, UnixStream)>> {
    let r = registry()?;
    let Some((packet, mut fds)) = r.take_packet()? else {
        return Ok(None);
    };
    r.check(Instant::now() + Duration::from_secs(10))?;
    match packet {
        Packet::Serving { version: 1 } => {
            let until = Instant::now() + Duration::from_secs(10);
            if r.serving_stopped.load(std::sync::atomic::Ordering::SeqCst)
                || r.serving.get().is_some()
            {
                return Err(refused());
            }
            let proof = super::serving::OwnedServing::capture(
                &r.daemon,
                &r.layout,
                fds.into_iter().map(File::from).collect(),
                until,
            )?;
            r.serving.set(proof).map_err(|_| refused())?;
            r.serving_current(until)?;
            lifecycle::serving_ready(&r.reference, until)?;
            r.serving_current(until)?;
            Ok(None)
        }
        Packet::ServingStopped { version: 1 } => {
            r.serving_stopped
                .store(true, std::sync::atomic::Ordering::SeqCst);
            // Terminal invalidation ONLY. No rearm/physical/durable FINAL.
            Ok(None)
        }
        Packet::TaskEvent {
            version: 1,
            task,
            part,
            bytes,
        } => {
            {
                let mut t = r.task.lock().map_err(|_| refused())?;
                let t = t.as_mut().ok_or_else(refused)?;
                if t.id != task || t.part != part || t.retired {
                    return Err(refused());
                }
                t.part = t.part.checked_add(1).ok_or_else(refused)?;
            }
            lifecycle::task_event(
                &task,
                part,
                &bytes,
                Instant::now() + Duration::from_secs(10),
            )?;
            Ok(None)
        }
        Packet::Retired { version: 1, task } => {
            {
                let mut state = r.task.lock().map_err(|_| refused())?;
                let state = state.as_mut().ok_or_else(refused)?;
                // This ACK proves only the actual daemon adapter/worker/stream
                // quiesced. Root's prior retained namespace-init kernel retirement
                // is mandatory; an ACK alone cannot create family-exit authority.
                if state.id != task || !state.retiring || state.retired {
                    return Err(refused());
                }
                state.retired = true;
            }
            // Release the task mutex before the owner exchange: its same-pending
            // task readback must observe these fences without a recursive lock.
            let until = Instant::now() + Duration::from_secs(10);
            r.check(until)?;
            lifecycle::task_retired(&task, until)?;
            r.check(until)?;
            Ok(None)
        }
        Packet::Accepted { version: 1, domain } => Ok(Some((
            domain,
            UnixStream::from(fds.pop().ok_or_else(refused)?),
        ))),
        Packet::Closed {
            version: 1,
            attempt,
        } => {
            let mut db = r.db.lock().map_err(|_| refused())?;
            if db.closed
                || db
                    .intent
                    .as_ref()
                    .is_none_or(|i| i.purpose != Purpose::Close || i.attempt != attempt)
            {
                return Err(refused());
            }
            db.closed = true;
            Ok(None)
        }
        Packet::Witnessed {
            version: 1,
            witness,
        } => {
            let intent = {
                let db = r.db.lock().map_err(|_| refused())?;
                if !db.closed || db.witness.is_some() {
                    return Err(refused());
                }
                db.intent.clone().ok_or_else(refused)?
            };
            if intent.purpose != Purpose::Witness {
                return Err(refused());
            }
            super::capture::validate_witness(&intent, &witness)?;
            lifecycle::store_witness_readback(&witness, Instant::now() + Duration::from_secs(10))?;
            r.db.lock().map_err(|_| refused())?.witness = Some(witness);
            Ok(None)
        }
        _ => Err(refused()),
    }
}
fn pipes(stream: &UnixStream, stdio: super::HelperStdio, until: Instant) -> Result<()> {
    let files = [
        unsafe { File::from_raw_fd(stdio.stdin.into_raw_fd()) },
        unsafe { File::from_raw_fd(stdio.stdout.into_raw_fd()) },
        unsafe { File::from_raw_fd(stdio.stderr.into_raw_fd()) },
    ];
    let mut tag = [0x50u8];
    let mut iov = libc::iovec {
        iov_base: tag.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 16];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen =
        unsafe { libc::CMSG_SPACE((3 * std::mem::size_of::<RawFd>()) as u32) } as usize;
    unsafe {
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN((3 * std::mem::size_of::<RawFd>()) as u32) as usize;
        for (i, f) in files.iter().enumerate() {
            std::ptr::write_unaligned(libc::CMSG_DATA(c).cast::<RawFd>().add(i), f.as_raw_fd());
        }
    }
    loop {
        budget(until)?;
        let n = unsafe {
            libc::sendmsg(
                stream.as_raw_fd(),
                &msg,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if n == 1 {
            return Ok(());
        }
        if n >= 0 {
            return Err(refused());
        }
        if !matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EAGAIN | libc::EINTR)
        ) {
            return Err(refused());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
impl PiSession {
    fn handle(
        &mut self,
        profile: &OwnerProfile,
        stores: &mut Vec<StoreSession>,
        until: Instant,
    ) -> Result<()> {
        let r = registry()?;
        r.admit(&self.stream, until)?;
        let request: PiRequest = read(&mut self.stream, 4096, until)?;
        let interrupt = matches!(&request, PiRequest::Interrupt { .. });
        let retire = matches!(&request, PiRequest::Retire { .. });
        match request {
            PiRequest::Provision {
                version: 1,
                selection,
                alias,
            } if self.permit.is_none() && self.running.is_none() && self.operation.is_empty() => {
                {
                    let mut task = r.task.lock().map_err(|_| refused())?;
                    let task = task.as_mut().ok_or_else(refused)?;
                    if task.launch_started
                        || task.cancelled
                        || task.retired
                        || task.alias != alias
                        || task.model != selection.model
                    {
                        return Err(refused());
                    }
                    // Reserve BEFORE issuance/await. Ambiguity/failure cannot
                    // cause a second helper/Node effect in this ephemeral task.
                    task.launch_started = true;
                }
                let p = profile.issue(selection, &alias)?;
                r.admit(&self.stream, until)?;
                p.provision()?;
                self.operation = p.describe().operation.clone();
                write(
                    &mut self.stream,
                    &PiResponse::Authorized {
                        launch: p.describe().clone(),
                        signed: p.signed_operation()?,
                    },
                    until,
                )?;
                self.permit = Some(p);
            }
            PiRequest::Launch {
                version: 1,
                selection,
                operation,
            } if self.running.is_none() => {
                let permit = self.permit.take().ok_or_else(refused)?;
                crate::adapter::pi_guest::owner::require_binding(
                    permit.describe(),
                    &selection,
                    Some(&operation),
                )?;
                let (pending, stdio) = PendingLaunch::spawn(permit, until)?;
                let description = pending.describe().clone();
                let helper = loop {
                    budget(until)?;
                    r.check(until)?;
                    r.daemon.drain_logs()?;
                    if let Some((domain, stream)) = accepted()? {
                        match domain {
                            Domain::Pi => break stream,
                            Domain::Store => {
                                r.admit(&stream, until)?;
                                stores.push(StoreSession {
                                    stream,
                                    sequence: 0,
                                    facts: None,
                                    phase: "issued".into(),
                                    burned: false,
                                    file_delivered: false,
                                });
                            }
                        }
                    }
                    for s in stores.iter_mut() {
                        if ready(&s.stream)? {
                            s.handle(until)?
                        }
                    }
                    std::thread::sleep(Duration::from_millis(1));
                };
                let running = pending.serve(helper, until)?;
                r.admit(&self.stream, until)?;
                write(
                    &mut self.stream,
                    &PiResponse::Started {
                        version: 1,
                        selection: description.selection,
                        operation: description.operation,
                        pid: running.helper.pid(),
                    },
                    until,
                )?;
                pipes(&self.stream, stdio, until)?;
                self.running = Some(running);
            }
            PiRequest::Interrupt {
                version: 1,
                selection,
                operation,
            }
            | PiRequest::Retire {
                version: 1,
                selection,
                operation,
            }
            | PiRequest::Status {
                version: 1,
                selection,
                operation,
            } => {
                let running = self.running.as_ref().ok_or_else(refused)?;
                if operation != self.operation || selection != running.selection {
                    return Err(refused());
                }
                if interrupt {
                    running.helper.interrupt(until)?
                }
                if retire {
                    running.helper.retire(until)?
                }
                let phase = if running.helper.exited(until)? {
                    ProcessPhase::Exited
                } else {
                    ProcessPhase::Running
                };
                r.admit(&self.stream, until)?;
                write(
                    &mut self.stream,
                    &PiResponse::State {
                        version: 1,
                        selection,
                        operation,
                        pid: running.helper.pid(),
                        phase,
                    },
                    until,
                )?;
            }
            _ => return Err(refused()),
        }
        Ok(())
    }
}
pub(super) fn run(layout: Layout) -> Result<()> {
    let until = Instant::now() + Duration::from_secs(30);
    let sockets = [Socket::bind(PI)?, Socket::bind(STORE)?];
    let b = context::runtime_parts()?.0;
    let mut daemon = OwnedDaemon::spawn(
        files::HeldArtifact::open(
            files::Artifact::Client,
            super::pin(&b.manifest.artifacts.client)?,
            Deadline(until),
        )?,
        [
            sockets[0].listener.as_raw_fd(),
            sockets[1].listener.as_raw_fd(),
        ],
        Deadline(until),
    )?;
    private_wire::send(
        &daemon.control,
        &Packet::Boot {
            version: 1,
            image_attestation: b.image_attestation.clone(),
        },
        &[],
        Deadline(until),
    )?;
    daemon.release(until)?;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| refused())?;
    REGISTRY
        .set(Registry {
            task: Mutex::new(None),
            task_history: Mutex::new(std::collections::HashSet::new()),
            serving: OnceLock::new(),
            serving_stopped: std::sync::atomic::AtomicBool::new(false),
            buffered: Mutex::new(std::collections::VecDeque::new()),
            buffered_bytes: std::sync::atomic::AtomicUsize::new(0),
            daemon,
            layout,
            sockets,
            reference: hex(&nonce),
            db: Mutex::new(DbState {
                startup: None,
                closed: false,
                witness: None,
                intent: None,
                file: None,
                opening_peer: None,
            }),
        })
        .map_err(|_| refused())?;
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Ok(r) = registry() {
                r.daemon.cancel();
            }
        }
    }
    let _cleanup = Cleanup;
    lifecycle::daemon_ready(&registry()?.reference, until)?;
    let profile = OwnerProfile::from_constructor(until)?;
    let (mut pi, mut stores) = (Vec::<PiSession>::new(), Vec::<StoreSession>::new());
    let mut heartbeat = Instant::now();
    loop {
        let until = Instant::now() + Duration::from_secs(30);
        let r = registry()?;
        r.check(until)?;
        r.daemon.drain_logs()?;
        // Service real tracer signal stops even when no Status RPC arrives.
        // Exact retained Node/caller/seal/ns is rechecked before forwarding;
        // unexpected exec/event/trap aborts this runtime rather than releasing.
        for session in &pi {
            if let Some(run) = &session.running {
                run.helper.exited(until)?;
            }
        }
        if heartbeat.elapsed() >= Duration::from_secs(1) {
            lifecycle::runtime_current(until)?;
            heartbeat = Instant::now();
        }
        for command in lifecycle::take_commands()? {
            match command {
                lifecycle::Command::Task {
                    task,
                    alias,
                    model,
                    prompt,
                } => {
                    if !super::hex(&task, 32)
                        || alias.is_empty()
                        || alias.len() > 192
                        || model.len() > 192
                        || !model.contains('/')
                        || prompt.is_empty()
                        || prompt.len() > 32768
                    {
                        return Err(refused());
                    }
                    {
                        r.serving_current(until)?; // physical+consumed DB before task/Store/Pi effects
                        let mut state = r.task.lock().map_err(|_| refused())?;
                        if state.as_ref().is_some_and(|s| !s.retired) {
                            return Err(refused());
                        }
                        let mut history = r.task_history.lock().map_err(|_| refused())?;
                        if history.len() >= 4096 || !history.insert(task.clone()) {
                            return Err(refused());
                        }
                        *state = Some(TaskState {
                            id: task.clone(),
                            alias: alias.clone(),
                            model: model.clone(),
                            part: 0,
                            launch_started: false,
                            cancelled: false,
                            retiring: false,
                            retired: false,
                        });
                    }
                    private_wire::send(
                        &r.daemon.control,
                        &Packet::Task {
                            version: 1,
                            task,
                            alias,
                            model,
                            prompt,
                        },
                        &[],
                        Deadline(until),
                    )?;
                }
                lifecycle::Command::Cancel { task } => {
                    {
                        let mut t = r.task.lock().map_err(|_| refused())?;
                        let t = t.as_mut().ok_or_else(refused)?;
                        if t.id != task || t.retired {
                            return Err(refused());
                        }
                        t.cancelled = true;
                    }
                    private_wire::send(
                        &r.daemon.control,
                        &Packet::Cancel { version: 1, task },
                        &[],
                        Deadline(until),
                    )?;
                }
                lifecycle::Command::Retire { task } => {
                    {
                        let t = r.task.lock().map_err(|_| refused())?;
                        let t = t.as_ref().ok_or_else(refused)?;
                        if t.id != task || t.retiring || t.retired {
                            return Err(refused());
                        }
                    }
                    // Physical proof comes from retained kernel objects, NOT
                    // adapter EOF/result/close or a daemon-reported retired bit.
                    for session in &pi {
                        if let Some(run) = &session.running {
                            run.helper.retire(until)?
                        }
                    }
                    r.task
                        .lock()
                        .map_err(|_| refused())?
                        .as_mut()
                        .ok_or_else(refused)?
                        .retiring = true;
                    private_wire::send(
                        &r.daemon.control,
                        &Packet::Retire { version: 1, task },
                        &[],
                        Deadline(until),
                    )?;
                    // No new generation until actual daemon quiescence ACK too.
                }
                lifecycle::Command::Close { binding } | lifecycle::Command::Witness { binding } => {
                    binding.validate()?;
                    let purpose = binding.purpose;
                    if !matches!(purpose, Purpose::Close | Purpose::Witness) {
                        return Err(refused());
                    }
                    {
                        let mut db = r.db.lock().map_err(|_| refused())?;
                        let start = db.startup.as_ref().ok_or_else(refused)?;
                        if binding.database_id != start.binding.database_id
                            || binding.incarnation != start.binding.incarnation
                            || binding.database_epoch != start.binding.database_epoch
                            || binding.operation != start.binding.operation
                            || binding.path != DB
                            || (purpose == Purpose::Close && db.closed)
                            || (purpose == Purpose::Witness && !db.closed)
                        {
                            return Err(refused());
                        }
                        db.intent = Some(binding.clone());
                    }
                    let packet = if purpose == Purpose::Close {
                        Packet::Close {
                            version: 1,
                            binding,
                        }
                    } else {
                        Packet::Witness {
                            version: 1,
                            binding,
                        }
                    };
                    private_wire::send(&r.daemon.control, &packet, &[], Deadline(until))?;
                }
                lifecycle::Command::Capture { attempt } => {
                    let witness =
                        r.db.lock()
                            .map_err(|_| refused())?
                            .witness
                            .clone()
                            .ok_or_else(refused)?;
                    let deadline =
                        r.db.lock()
                            .map_err(|_| refused())?
                            .intent
                            .as_ref()
                            .ok_or_else(refused)?
                            .deadline_unix;
                    let now = context::runtime_ms()? / 1000;
                    if deadline <= now as i64 {
                        return Err(refused());
                    }
                    super::capture::capture(
                        &attempt,
                        &witness,
                        Instant::now() + Duration::from_secs((deadline as u64 - now).min(300)),
                    )?;
                }
            }
        }
        if let Some((domain, stream)) = accepted()? {
            r.admit(&stream, until)?;
            match domain {
                Domain::Pi => pi.push(PiSession {
                    stream,
                    permit: None,
                    running: None,
                    operation: String::new(),
                }),
                Domain::Store => stores.push(StoreSession {
                    stream,
                    sequence: 0,
                    facts: None,
                    phase: "issued".into(),
                    burned: false,
                    file_delivered: false,
                }),
            }
        }
        if pi.len() + stores.len() > 4096 {
            return Err(refused());
        }
        // Closed/malformed clients lose their retained custody; UNKNOWN is not
        // a retry permission. Other owned clients remain serviceable.
        let mut i = 0;
        while i < stores.len() {
            if ready(&stores[i].stream)? && stores[i].handle(until).is_err() {
                stores.remove(i);
            } else {
                i += 1
            }
        }
        let mut i = 0;
        while i < pi.len() {
            if ready(&pi[i].stream)? && pi[i].handle(&profile, &mut stores, until).is_err() {
                // Losing control BEFORE owned retirement is UNKNOWN. Expected
                // closure is admitted only by THIS session's private non-Clone
                // namespace-init retirement evidence + actual kernel pidfd exit,
                // not the CURRENT task's phase (which may already be a new one).
                if pi[i]
                    .running
                    .as_ref()
                    .is_none_or(|r| r.helper.retirement_verified(until).ok() != Some(true))
                {
                    return Err(refused());
                }
                pi.remove(i);
            } else {
                i += 1
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
/// Fresh physical readback serviced during the same bounded owner exchange.
/// Host must retain its actual ExecProcess/source capability, not mint a port
/// from this data/reference/PID. No effect or grant is inferred from JSON.
pub(super) fn readback(query: &Value, until: Instant) -> Result<Value> {
    let r = registry()?;
    r.check(until)?;
    r.sample_control()?;
    let obj = query.as_object().ok_or_else(refused)?;
    let result = match obj.get("type").and_then(Value::as_str) {
        Some("current") if obj.len() == 1 => json!({"reference":r.reference,"revision":1}),
        Some("serving-current") if obj.len() == 2 => {
            let data = r.serving_current(until)?;
            if obj.get("reference") != data.get("databaseReference") {
                return Err(refused());
            }
            data
        }
        Some("task") if obj.len() == 2 => {
            let state = r.task.lock().map_err(|_| refused())?;
            let state = state.as_ref().ok_or_else(refused)?;
            if obj.get("task").and_then(Value::as_str) != Some(state.id.as_str()) {
                return Err(refused());
            }
            let phase = if state.retired {
                "retired"
            } else if state.retiring {
                "retiring"
            } else if state.cancelled {
                "cancelled"
            } else if state.launch_started {
                "launch-reserved"
            } else {
                "selected"
            };
            json!({"version":1,"task":state.id,"alias":state.alias,"model":state.model,"phase":phase,"parts":state.part})
        }
        Some("task-retired") if obj.len() == 2 => {
            let task = obj
                .get("task")
                .and_then(Value::as_str)
                .ok_or_else(refused)?;
            if !super::hex(task, 32) {
                return Err(refused());
            }
            let before = r.serving_current(until)?;
            {
                let state = r.task.lock().map_err(|_| refused())?;
                let state = state.as_ref().ok_or_else(refused)?;
                // Only the original physical-family + worker-ACK boundary sets
                // this private state. Caller data/result/EOF cannot supply it.
                if state.id != task || !state.retiring || !state.retired {
                    return Err(refused());
                }
            }
            let after = r.serving_current(until)?;
            if before != after {
                return Err(refused());
            }
            json!({"reference":r.reference,"revision":1,"databaseReference":after.get("databaseReference").ok_or_else(refused)?,"task":task,"phase":"retired"})
        }
        Some("opening") if obj.len() == 1 => {
            r.construction_current(until)?;
            json!({"reference":r.reference,"revision":1,"purpose":"init","path":DB})
        }
        Some("database-current") if obj.len() == 2 => {
            let facts = {
                let db = r.db.lock().map_err(|_| refused())?;
                if db.closed {
                    return Err(refused());
                }
                let f = db.startup.as_ref().ok_or_else(refused)?;
                if obj.get("reference").and_then(Value::as_str) != Some(f.reference.as_str()) {
                    return Err(refused());
                }
                r.admit(db.opening_peer.as_ref().ok_or_else(refused)?, until)?;
                f.clone()
            };
            // Independent WAL-aware READ_ONLY connection, not the daemon's
            // currently held write mutex or immutable/recovery-writing reader.
            r.database()?;
            super::capture::database_current(&facts.binding, until)?;
            r.database()?;
            json!({"version":1,"reference":r.reference,"revision":1,"facts":facts,"phase":"consumed"})
        }
        Some("maintenance") if obj.len() == 3 => {
            let db = r.db.lock().map_err(|_| refused())?;
            let intent = db.intent.as_ref().ok_or_else(refused)?;
            if obj.get("attempt").and_then(Value::as_str) != Some(&intent.attempt)
                || obj.get("purpose") != serde_json::to_value(intent.purpose).ok().as_ref()
            {
                return Err(refused());
            }
            json!({"purpose":intent.purpose,"attempt":intent.attempt,"deadlineMs":intent.deadline_unix*1000})
        }
        Some("witness") if obj.len() == 2 => {
            let db = r.db.lock().map_err(|_| refused())?;
            let witness = db.witness.as_ref().ok_or_else(refused)?;
            if obj.get("attempt") != witness.get("attempt") {
                return Err(refused());
            }
            witness.clone()
        }
        _ => return Err(refused()),
    };
    r.check(until)?;
    Ok(result)
}
pub(super) fn physical_check(until: Instant) -> Result<()> {
    registry()?.check(until)?;
    registry()?.database()
}
