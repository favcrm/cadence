//! Host build slots (CAD-113) — bounded, fair, observable cargo
//! build/test scheduling, owned by the daemon.
//!
//! Two pools share one queue: `build`/`test` requests draw on
//! `build_slots` (default 3), `suite` requests on `suite_slots`
//! (default 1) — independent, so a queued full suite never starves
//! ordinary builds. Grant order is FIFO with two modifiers:
//! `test`/`suite` requests from a configured priority lane outrank
//! everything ordinary, and a `(lane, kind)` whose requests have
//! waited continuously longer than `starve_secs` (default 900) jumps
//! to the front so priority can never starve a lane out. Seniority
//! belongs to an *unserved* wait and is carried by exactly one
//! waiter — the lane's eldest for that kind: a caller re-queueing
//! under a new request id inherits the anchor through a brief polling
//! gap, but later arrivals of a burst stamp their own arrival, so a
//! lane cannot multiply one anchor into N front-runners. Every grant
//! for that `(lane, kind)` restarts the clock — a lane can never keep
//! an old anchor alive just by keeping one more request queued.
//!
//! A slot is held by a daemon-minted token bound to (lane, pid,
//! pid-starttime): `release` must name the holding caller, and a
//! holder whose process dies or is recycled is reaped on the next
//! operation so a killed agent frees its slot. Holds are also reaped
//! past `max_hold_secs` — a caller that never releases cannot wedge a
//! pool forever. Waiting is client-side — `acquire` answers instantly
//! with granted-or-queued, and the caller polls with a stable
//! `request_id` so its place in line is sticky. A second process
//! polling the same `request_id` is a DIFFERENT caller: it queues
//! like everyone else, it never adopts another caller's hold.
//!
//! Holds persist to `<state>/slots.json` (best-effort, atomic) so a
//! daemon restart revalidates them: a hold survives restart only
//! while its recorded process still lives (pid + starttime), a dead
//! holder's slot is reaped at boot rather than silently re-granted.
//!
//! Two bindings share the pools (CAD-230). A *legacy* hold is the
//! CAD-113 shape above — a pty pane's lane, its alive-only fallback and
//! its `max_hold_secs` reap, all unchanged. A *strict* hold belongs to
//! a daemon-minted [`Enrollment`] of a managed provider process
//! ([`strict`]): it is admitted only through verified process identity,
//! is written through a fail-closed writer (a failed write leaves disk
//! and memory unchanged and blocks strict admission), and is freed only
//! by an exact release or proven death — never by revocation, expiry
//! or `max_hold_secs`, which mark it `expired_pending_reconcile`.
//! Liveness is tri-state: `alive` retains, `dead` frees, `unknown`
//! stays accounted. The first strict record upgrades `slots.json` to
//! the v2 envelope (`enrollments`, strict `holds`, `legacy_holds`);
//! until then the file keeps its v1 shape.

use std::collections::HashMap;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{Error, Result};

mod heartbeat;
mod lease;
mod persistence;
mod queue;
mod strict;
#[cfg(test)]
mod strict_tests;

pub use strict::{AuthState, Enrollment, Liveness, ProcFs, ProcIdentity, StrictCaller};

/// The daemon's cap on one enrollment's lifetime. Expiry refuses new
/// work (holds stay accounted); the same endpoint identity renews at
/// its next open, a new process enrolls afresh.
pub const ENROLLMENT_TTL_SECS: f64 = 86_400.0;

/// `[host]` slot configuration from pm.yaml — every key optional,
/// unset keys keep the defaults.
#[derive(Clone, Debug)]
pub struct SlotConfig {
    /// Concurrent `build`+`test` grants (default 3, minimum 1).
    pub build_slots: usize,
    /// Concurrent `suite` grants (default 1, minimum 1).
    pub suite_slots: usize,
    /// `CARGO_BUILD_JOBS` value dispatch injects (default 2).
    pub jobs_per_lane: usize,
    /// CAD-1268: combined cap on grants across ALL pools (default 2,
    /// minimum 1). Per-pool limits still apply — a grant needs both —
    /// so `build`/`test`, `suite` and `check` can never cumulatively
    /// overcommit one host beyond this budget. This registry's
    /// aggregate only; coordination across separate daemon
    /// registries is a follow-up.
    pub total_slots: usize,
    /// A `(lane, kind)` waiting continuously longer than this outranks
    /// even priority lanes (default 900) — the never-starve bound.
    pub starve_secs: u64,
    /// Lanes whose `test`/`suite` requests outrank ordinary requests
    /// (`[host] priority_lanes` — the reviewer lane, e.g. `qa-1`).
    pub priority_lanes: Vec<String>,
    /// A hold older than this is reaped (default 7200) — a caller that
    /// dies without its pid dying, or simply forgets release, cannot
    /// wedge a pool forever.
    pub max_hold_secs: u64,
    /// CAD-1021: `check` requests draw on this pool (default 2) — a
    /// pre-push gate never queues behind a full build. A `check` grant
    /// is admitted only through the recipe allowlist (see
    /// [`Slots::check_admitted`]).
    pub check_slots: usize,
    /// Memory floor for a build/test grant: `MemAvailable` must be at
    /// least this (default 4 GiB). Below it the request WAITS with
    /// reason `memory` — never refused for being low.
    pub slot_mem_min_available_bytes: u64,
    /// The same floor for the lighter `check` pool (default 2 GiB).
    pub slot_mem_min_available_check_bytes: u64,
    /// Free disk on the worktree's filesystem for any grant (default
    /// 15 GiB). Below it the request waits with reason `disk`.
    pub slot_disk_min_free_bytes: u64,
}

/// A host-resource reading the daemon injects: `(mem_available_bytes,
/// disk_free_bytes)`. `None` fields read as "unmeasurable" — a request
/// waits rather than being refused on a probe failure.
pub type ResourceProbe = fn() -> (Option<u64>, Option<u64>);

impl Default for SlotConfig {
    fn default() -> Self {
        Self {
            build_slots: 3,
            suite_slots: 1,
            jobs_per_lane: 2,
            total_slots: 2,
            starve_secs: 900,
            priority_lanes: Vec::new(),
            max_hold_secs: 7200,
            check_slots: 2,
            slot_mem_min_available_bytes: 4 * GIB,
            slot_mem_min_available_check_bytes: 2 * GIB,
            slot_disk_min_free_bytes: 15 * GIB,
        }
    }
}

const GIB: u64 = 1 << 30;

/// CAD-1021: the lane label of a caller that derives no pane and no
/// managed endpoint. The daemon alone mints it (`unregistered:<uid>`,
/// from the socket peer's kernel credentials) — it is never read from a
/// request. A registered alias is `[A-Za-z0-9_-]` only (`check_alias`),
/// so no agent can own this prefix and no label can name an agent. It
/// buys a queue position and nothing else: such a lane ranks behind
/// every registered waiter (see `Slots::rank`).
pub const UNREGISTERED_LANE_PREFIX: &str = "unregistered:";

/// Whether `lane` is a daemon-minted unregistered label.
pub fn is_unregistered_lane(lane: &str) -> bool {
    lane.starts_with(UNREGISTERED_LANE_PREFIX)
}

/// Slot kinds map to pools: `build`/`test` share `build_slots`,
/// `suite` owns `suite_slots`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SlotKind {
    Build,
    Test,
    Suite,
    /// CAD-1021: the light pre-push class — fmt, split-map, clippy
    /// and `check` under `CARGO_BUILD_JOBS=2`. Its own pool so a full
    /// `build`/`test` wave never starves a lane's pre-push gate.
    Check,
}

impl SlotKind {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "build" => Ok(Self::Build),
            "test" => Ok(Self::Test),
            "suite" => Ok(Self::Suite),
            "check" => Ok(Self::Check),
            _ => Err(Error::rejected(format!(
                "Slot kind must be build, test, suite or check — got '{name}'"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Test => "test",
            Self::Suite => "suite",
            Self::Check => "check",
        }
    }

    /// The pool this kind draws on: suite is independent, check owns
    /// its pool, and build/test share — `kind` still rides the queue
    /// for reporting.
    fn pool(self) -> Pool {
        match self {
            Self::Suite => Pool::Suite,
            Self::Check => Pool::Check,
            _ => Pool::Build,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum Pool {
    Build = 0,
    Suite = 1,
    Check = 2,
}

impl Pool {
    fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Suite => "suite",
            Self::Check => "check",
        }
    }
}

/// CAD-1268: one reason string for a refused nested acquire — the
/// message names the held pool and the refused one; they may match.
fn hold_and_wait_refusal(held: Pool, waiting: Pool) -> Error {
    Error::rejected(format!(
        "A caller holding a {} slot cannot queue for {} — \
         release that hold first (hold-and-wait deadlock guard)",
        held.as_str(),
        waiting.as_str(),
    ))
}

/// The mirror refusal: the caller's queued waiter is the dependency
/// that must resolve before any new grant — or any new enqueue.
fn wait_and_hold_refusal(wanted: Pool, waiting: Pool) -> Error {
    Error::rejected(format!(
        "A caller with a queued {} slot request cannot ask for {} — \
         that queued request must resolve first (wait-then-hold deadlock guard)",
        waiting.as_str(),
        wanted.as_str(),
    ))
}

struct SlotWait {
    request_id: String,
    kind: SlotKind,
    lane: String,
    pid: u32,
    pid_start: Option<u64>,
    /// CAD-1021: why this waiter is held — `capacity` (no free slot),
    /// `memory` or `disk` (below the kind's floor). Surfaces in
    /// `build-slot status`.
    wait_reason: WaitReason,
    /// Seniority start — when this `(lane, kind)` began its current
    /// unserved wait, which may predate this request_id (a re-queued
    /// caller keeps the lane's place rather than restarting at the
    /// back). The anchor can never outlive a serve: `grant` clears
    /// the record, so waiters enqueued later stamp fresh.
    queued_at: f64,
    /// Last time this request polled — a caller that stops polling is
    /// abandoned and reaped after `WAITER_TTL_SECS`, so a fast-failed
    /// or timed-out CLI never jams the queue behind a dead lane's pid.
    last_poll: f64,
    /// A strict waiter's enrollment binding — revalidated on every
    /// reap pass, before any ranking. `None` for legacy waiters.
    strict: Option<StrictBind>,
}

/// What ties a strict waiter or hold to its enrollment: the enrollment
/// and owner generation it was admitted under, and the exact holder
/// process (the claimed pid, verified on the caller's chain).
#[derive(Clone, Debug)]
struct StrictBind {
    enrollment_id: String,
    owner_generation: String,
    holder: ProcIdentity,
    /// Last observed holder liveness — status only, never persisted.
    liveness: Liveness,
    /// CAD-230b: the holder is exactly the process that execs the
    /// command — `build-slot run` itself (the requesting peer, never an
    /// ancestor) or a daemon-launched runner. Exec keeps the pid and
    /// starttime, so the hold names the running build and ends with it.
    exec_bound: bool,
}

#[derive(Clone)]
struct SlotHold {
    /// Daemon-minted grant token — never the caller's request_id.
    token: String,
    /// The acquire request that minted this hold: a re-poll with the
    /// same full identity returns this token (idempotent grant).
    request_id: String,
    kind: SlotKind,
    lane: String,
    pid: u32,
    /// `/proc/<pid>/stat` starttime at grant — a recycled pid is a
    /// different process and does not keep the hold.
    pid_start: Option<u64>,
    /// Monotonic grant instant — ages, expiry and ordering.
    acquired_at: f64,
    /// Wall-clock grant instant — persisted so a restart can restore
    /// the hold's age against the new clock epoch.
    acquired_epoch: f64,
    /// `Some` for a strict hold (CAD-230); `None` is the legacy
    /// binding with its unchanged alive-only and max-hold rules.
    strict: Option<StrictBind>,
}

impl SlotHold {
    fn enrollment_id(&self) -> Option<&str> {
        self.strict.as_ref().map(|b| b.enrollment_id.as_str())
    }

    /// The legacy persisted row — the v1 shape, `legacy_holds` in v2.
    fn legacy_json(&self) -> Value {
        json!({
            "token": self.token, "request_id": self.request_id,
            "kind": self.kind.as_str(), "lane": self.lane,
            "pid": self.pid, "pid_start": self.pid_start,
            "acquired_epoch": self.acquired_epoch,
        })
    }
}

/// Why strict admission is unavailable. `preserve_file` means the
/// state file itself was rejected — it is retained as evidence and
/// nothing overwrites it; a failed strict write leaves the last good
/// file and lets legacy best-effort writes continue.
#[derive(Clone, Debug)]
struct Blocked {
    reason: String,
    preserve_file: bool,
}

/// One event the caller should emit — `(alias, kind, payload)`; the
/// daemon routes them through `store.event_public`.
pub type SlotEvent = (String, &'static str, Value);

/// The two clocks a mutating call runs against: `mono` drives ages,
/// expiry and ordering (NTP-proof); `wall` rides the persist file so
/// a restart can restore hold age.
#[derive(Clone, Copy)]
pub struct SlotClock {
    pub mono: f64,
    pub wall: f64,
}

impl SlotClock {
    pub fn at(mono: f64, wall: f64) -> Self {
        Self { mono, wall }
    }
}

/// Compact wait age for status lines: `45s`, `4m12s`, `1h03m`.
pub fn fmt_wait(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// `kill(pid, 0)`: ESRCH is dead, EPERM/alive both mean present.
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let rc = unsafe { libc::kill(pid as i32, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// `/proc/<pid>/stat` field 22 — process start time (jiffies since
/// boot). Two pids with the same number but different starttimes are
/// different processes: this is the recycling check.
fn pid_start(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm (field 2) may contain spaces and parens — split after the
    // last ')'. Field 3 (state) is then index 0, so field 22 is 19.
    let after = stat.rsplit(')').next()?;
    after.split_whitespace().nth(19)?.parse().ok()
}

/// Liveness + identity: alive, and — when a starttime was captured —
/// still the same boot-time process. An unreadable starttime on
/// either side weakens the check to alive-only rather than reaping a
/// live process on a /proc hiccup — but never silently: a live pid
/// whose starttime cannot be compared degrades the recycling check,
/// so it is surfaced once per process.
fn pid_matches(pid: u32, recorded: Option<u64>) -> bool {
    if !pid_alive(pid) {
        return false;
    }
    match (recorded, pid_start(pid)) {
        (Some(recorded), Some(current)) => recorded == current,
        _ => {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                eprintln!(
                    "slots: pid starttime unavailable — recycled-pid \
                     detection is degraded (alive-only) until /proc reads work"
                );
            });
            true
        }
    }
}

/// A waiter that has not re-polled within this window is abandoned —
/// ~120× the CLI's 250ms poll cadence, generous under load.
const WAITER_TTL_SECS: f64 = 30.0;

/// The queue is a list, not a stream — bound it so a flood of
/// request_ids cannot grow memory without limit. Reaped stale
/// entries keep real queues far below this.
const MAX_WAITERS: usize = 128;

/// One lane's share of the queue — without it a single lane could
/// fill `MAX_WAITERS` and deny everyone else a place. A lane running
/// more than this many distinct slot requests at once is already
/// abusive (each should be one cargo invocation).
const MAX_WAITERS_PER_LANE: usize = 32;

/// A hold `reap_dead` just dropped — `release` answers these softly
/// (`released:false` + the reap reason) instead of calling them
/// unknown tokens — to the hold's own lane only, so a foreign caller
/// can't probe whether a token ever existed.
#[derive(Clone)]
struct Reaped {
    token: String,
    lane: String,
    kind: SlotKind,
    reason: &'static str,
}

/// One `(lane, kind)` wait episode: `anchor` is the stamp the lane's
/// eldest unserved waiter carries; `seen` is the last time the lane's
/// queue for that kind held a waiter — once it has stayed empty for
/// `WAITER_TTL_SECS` the episode ended and the next request anchors
/// fresh.
#[derive(Clone, Copy)]
struct Seniority {
    anchor: f64,
    seen: f64,
}

/// The unrevoked agent-owned enrollments at one instant: `(id, owner)`.
/// See [`Slots::enrollment_snapshot`].
#[derive(Clone, Debug, Default)]
pub struct EnrollmentSnapshot {
    members: Vec<(String, String)>,
}

impl EnrollmentSnapshot {
    /// The owners whose rows must be read — sorted, once each.
    pub fn owners(&self) -> Vec<String> {
        let mut owners: Vec<String> = self.members.iter().map(|(_, o)| o.clone()).collect();
        owners.sort();
        owners.dedup();
        owners
    }
}

/// The slot registry. In-memory for queue state (waiters re-poll
/// after a restart), persisted for holds: `persist_path` is written
/// on every hold change so a daemon restart can revalidate them
/// instead of forgetting (fail-open) or duplicating grants.
#[derive(Default)]
pub struct Slots {
    pub config: SlotConfig,
    waiting: Vec<SlotWait>,
    held: Vec<SlotHold>,
    /// Unserved-wait episode per `(lane, kind)` — the starvation
    /// clock that survives a caller re-queuing under a new
    /// request_id but NEVER a grant: `grant` removes the record, so
    /// the anchor can only be as old as the lane's current
    /// post-serve wait. Only the lane's eldest waiter of a kind ever
    /// inherits it — later arrivals stamp their own arrival.
    seniority: HashMap<(String, SlotKind), Seniority>,
    persist_path: Option<PathBuf>,
    /// Strict enrollments (CAD-230) — persisted with the holds.
    enrollments: Vec<Enrollment>,
    /// Where strict identity is read — `/proc`, a fixture in tests.
    proc: ProcFs,
    /// The only uid a strict root, worker, holder or peer may run as.
    daemon_uid: u32,
    /// `Some` once strict admission is unavailable — a rejected state
    /// file or a failed strict write. Legacy callers are unaffected.
    blocked: Option<Blocked>,
    /// The persisted file is the v2 envelope: set by loading one or by
    /// the first strict write, never cleared — a legacy write never
    /// downgrades it or drops a strict record.
    v2: bool,
    /// Bumped on every v2 write — the envelope's `state_generation`.
    state_generation: u64,
    /// Strict holds the daemon's own watcher freed between slot calls
    /// (CAD-230b) — bounded, so a holder's later `release` still gets
    /// the soft `released:false` answer a same-call reap would give.
    recent_reaped: Vec<Reaped>,
    /// CAD-1021: how the daemon reads host memory/disk before a grant.
    /// `None` uses the live probe (`/proc/meminfo` + statvfs of the
    /// state dir); tests inject a fixture. A probe returning `None`
    /// for a resource means "unmeasurable" — a request waits, never
    /// refuses, on an unreadable probe.
    resources: Option<ResourceProbe>,
    /// The state dir the disk floor probes — set by `persist_to` (the
    /// daemon's slots.json lives under it, so it shares the daemon's
    /// filesystem).
    disk_probe_path: Option<PathBuf>,
}

/// Why a grant is held up — a `wait_reason` on the acquire answer and
/// the status queue. `capacity` is the ordinary "no free slot" case;
/// `memory`/`disk` are the resource floors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WaitReason {
    Capacity,
    Memory,
    Disk,
}

impl WaitReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Capacity => "capacity",
            Self::Memory => "memory",
            Self::Disk => "disk",
        }
    }
}

/// How many watcher-reaped holds `release` remembers for its soft answer.
const RECENT_REAPED: usize = 64;

impl Slots {
    pub fn new(config: SlotConfig) -> Self {
        Self {
            config,
            daemon_uid: unsafe { libc::geteuid() },
            ..Default::default()
        }
    }

    /// Inject a resource probe (tests); `None` restores the live probe.
    #[cfg(test)]
    pub(crate) fn use_resources(&mut self, probe: Option<ResourceProbe>) {
        self.resources = probe;
    }

    /// The resource floor that would hold a grant of `kind`: `Some`
    /// reason when below floor (or unmeasurable), `None` when a grant
    /// is resource-safe. Never refuses — the caller waits.
    fn resource_floor(&self, kind: SlotKind) -> Option<WaitReason> {
        let (mem_min, disk_min) = (
            match kind {
                SlotKind::Check => self.config.slot_mem_min_available_check_bytes,
                _ => self.config.slot_mem_min_available_bytes,
            },
            self.config.slot_disk_min_free_bytes,
        );
        let (mem, disk) = match self.resources {
            Some(p) => p(),
            None => crate::slots::probe::live(self.disk_probe_path.as_deref()),
        };
        // Unreadable does not gate: a probe we cannot read proves
        // nothing either way, and a floor is protective, not
        // authoritative — a host whose meminfo/statvfs fails must
        // still build (the wait would never clear).
        if let Some(m) = mem {
            if m < mem_min {
                return Some(WaitReason::Memory);
            }
        }
        if let Some(d) = disk {
            if d < disk_min {
                return Some(WaitReason::Disk);
            }
        }
        None
    }

    fn capacity(&self, pool: Pool) -> usize {
        match pool {
            Pool::Build => self.config.build_slots.max(1),
            Pool::Suite => self.config.suite_slots.max(1),
            Pool::Check => self.config.check_slots.max(1),
        }
    }

    fn held_in(&self, pool: Pool) -> usize {
        self.held.iter().filter(|h| h.kind.pool() == pool).count()
    }

    /// CAD-1268: the aggregate cap — combined grants across every
    /// pool. `0` clamps to 1, matching the per-pool capacities.
    fn total_capacity(&self) -> usize {
        self.config.total_slots.max(1)
    }

    /// Grants admitted need `held_total() < total_capacity()` AND the
    /// per-pool room — both are checked together at the probe and the
    /// real grant.
    fn held_total(&self) -> usize {
        self.held.len()
    }

    /// Whether a request for `kind` could be served right now: its
    /// pool has free capacity, the aggregate has room and its kind's
    /// resource floors hold. Shared by the probe, the real grant and
    /// cross-pool ranking so eligibility reads identically
    /// everywhere.
    fn grantable(&self, kind: SlotKind, resource: Option<WaitReason>) -> bool {
        resource.is_none()
            && self.held_in(kind.pool()) < self.capacity(kind.pool())
            && self.held_total() < self.total_capacity()
    }
}

/// Strict enrollments (CAD-230 phase a) — minted, renewed, revoked
/// and revalidated only by the daemon; a caller never names one.
impl Slots {
    /// Read strict identity from `root` instead of `/proc` — tests.
    #[cfg(test)]
    fn use_proc(&mut self, root: &Path, uid: u32) {
        self.proc = ProcFs::at(root);
        self.daemon_uid = uid;
    }

    /// Mint the enrollment for a managed endpoint the daemon just
    /// opened: owner `owner` at `owner_generation`, root = worker = the
    /// provider process `root_pid` as `/proc` reads it now. The same
    /// owner generation and root identity renew an existing active or
    /// expired enrollment (resume); anything else supersedes the
    /// owner's older enrollments. Refuses the daemon itself, init, any
    /// root not running as the daemon's uid, and a process already
    /// enrolled for another owner.
    pub fn enroll(
        &mut self,
        owner: &str,
        owner_generation: &str,
        root_pid: u32,
        clk: SlotClock,
    ) -> Result<(Value, Vec<SlotEvent>)> {
        self.heal_write_latch()?;
        if root_pid <= 1 || root_pid == std::process::id() {
            return Err(Error::rejected(format!(
                "pid {root_pid} cannot be an enrolled provider root — the daemon \
                 never enrolls itself or init"
            )));
        }
        let root = self
            .proc
            .identity(root_pid)
            .map_err(|why| Error::rejected(format!("cannot enroll pid {root_pid}: {why}")))?;
        if root.uid != self.daemon_uid {
            return Err(Error::rejected(format!(
                "cannot enroll pid {root_pid}: it runs as uid {} — not the daemon's uid {}",
                root.uid, self.daemon_uid
            )));
        }
        // One provider process is one endpoint: the exact identity
        // already enrolled for another owner is never enrolled again —
        // the pair would be the cross-owner same-root state `restore`
        // rejects (CAD-276).
        if let Some(other) = self
            .enrollments
            .iter()
            .find(|e| e.root == root && (e.owner_actor != owner || e.runner.is_some()))
        {
            return Err(Error::rejected(format!(
                "cannot enroll pid {root_pid} for '{owner}': that exact process is \
                 already enrolled for '{}' ({})",
                other.owner_actor, other.id
            )));
        }
        let expires_at = clk.mono + ENROLLMENT_TTL_SECS;
        let expires_epoch = clk.wall + ENROLLMENT_TTL_SECS;
        let mut next = self.enrollments.clone();
        let renew = next.iter().position(|e| {
            e.owner_actor == owner
                && e.owner_generation == owner_generation
                && e.root == root
                && matches!(e.auth, AuthState::Active | AuthState::Expired)
        });
        let (id, renewed) = match renew {
            Some(i) => {
                next[i].auth = AuthState::Active;
                next[i].expires_at = expires_at;
                next[i].expires_epoch = expires_epoch;
                (next[i].id.clone(), true)
            }
            None => {
                for e in next
                    .iter_mut()
                    .filter(|e| e.owner_actor == owner && e.runner.is_none())
                {
                    if !matches!(e.auth, AuthState::Revoked(_)) {
                        e.auth = AuthState::Revoked("superseded by a new endpoint".into());
                    }
                }
                // A tombstone for this very root is covered by the new
                // enrollment (which keeps blocking the pane fallback);
                // keep it only while a hold still names it.
                let held = &self.held;
                next.retain(|e| {
                    e.root != root
                        || held
                            .iter()
                            .any(|h| h.enrollment_id() == Some(e.id.as_str()))
                });
                let id = format!("enr-{}", Uuid::new_v4().simple());
                next.push(Enrollment {
                    id: id.clone(),
                    owner_actor: owner.to_string(),
                    owner_generation: owner_generation.to_string(),
                    runner: None,
                    root,
                    worker: root,
                    issued_epoch: clk.wall,
                    expires_epoch,
                    expires_at,
                    auth: AuthState::Active,
                });
                (id, false)
            }
        };
        self.commit(next, self.held.clone())?;
        let summary = json!({"enrollment_id": id, "root_pid": root_pid,
                             "expires_epoch": expires_epoch, "renewed": renewed});
        Ok((
            summary.clone(),
            vec![(owner.to_string(), "slot_enrolled", summary)],
        ))
    }

    /// Revoke every live enrollment of `owner` — its endpoint closed.
    /// New work is refused and its waiters drop; its holds stay
    /// accounted until released or proven dead.
    pub fn revoke_owner(&mut self, owner: &str, reason: &str) -> Vec<SlotEvent> {
        let reason = reason.to_string();
        self.revoke_where(
            |e| e.owner_actor == owner && e.runner.is_none(),
            |_| reason.clone(),
        )
    }

    /// Owners with a live (active or expired, never revoked) endpoint
    /// enrollment — whose rows the daemon must read for
    /// [`Self::revalidate_owners`]. An expired enrollment still vouches
    /// for identity (CAD-381), so it is revalidated too: omitting it
    /// would read as "owner gone" and revoke it whenever any other
    /// owner is active.
    pub fn enrolled_owners(&self) -> Vec<String> {
        self.enrollment_snapshot().owners()
    }

    /// The enrollments revalidation will judge: the unrevoked agent-owned
    /// ones as of now, by id. The daemon reads owner rows after releasing
    /// the slot lock, so what it reads says nothing about an enrollment
    /// minted since — only this snapshot may be revalidated against them.
    pub fn enrollment_snapshot(&self) -> EnrollmentSnapshot {
        EnrollmentSnapshot {
            members: self
                .enrollments
                .iter()
                .filter(|e| !matches!(e.auth, AuthState::Revoked(_)) && e.runner.is_none())
                .map(|e| (e.id.clone(), e.owner_actor.clone()))
                .collect(),
        }
    }

    /// Revalidate each unrevoked enrollment against its owner row as the
    /// daemon reads it now (`current`: owner → its generation, `None`
    /// when the row is gone or has no live endpoint). A missing or
    /// changed generation revokes — fail closed.
    pub fn revalidate_owners(
        &mut self,
        current: &HashMap<String, Option<String>>,
    ) -> Vec<SlotEvent> {
        self.revalidate(current, |_| true)
    }

    /// [`Self::revalidate_owners`], limited to the enrollments of
    /// `snapshot` (CAD-893): one minted after the snapshot — a master
    /// that just started — has no row in `current` to be judged by and is
    /// left for the next revalidation.
    pub fn revalidate_snapshot(
        &mut self,
        snapshot: &EnrollmentSnapshot,
        current: &HashMap<String, Option<String>>,
    ) -> Vec<SlotEvent> {
        self.revalidate(current, |e| {
            snapshot.members.iter().any(|(id, _)| *id == e.id)
        })
    }

    fn revalidate(
        &mut self,
        current: &HashMap<String, Option<String>>,
        judged: impl Fn(&Enrollment) -> bool,
    ) -> Vec<SlotEvent> {
        // A runner's owner is the daemon's runner record, not an agent
        // row — it is never revalidated against one (CAD-230b).
        let drifted = |e: &Enrollment| {
            e.runner.is_none()
                && judged(e)
                && current.get(&e.owner_actor).and_then(Option::as_deref)
                    != Some(e.owner_generation.as_str())
        };
        self.revoke_where(drifted, |e| {
            match current.get(&e.owner_actor).and_then(Option::as_deref) {
                None => "owner has no live endpoint".to_string(),
                Some(_) => "owner generation changed".to_string(),
            }
        })
    }

    fn revoke_where(
        &mut self,
        hit: impl Fn(&Enrollment) -> bool,
        reason: impl Fn(&Enrollment) -> String,
    ) -> Vec<SlotEvent> {
        let mut next = self.enrollments.clone();
        let mut events = Vec::new();
        for e in next.iter_mut() {
            if matches!(e.auth, AuthState::Revoked(_)) || !hit(e) {
                continue;
            }
            let why = reason(e);
            events.push((
                e.owner_actor.clone(),
                "slot_enrollment_revoked",
                json!({"enrollment_id": e.id, "reason": why}),
            ));
            e.auth = AuthState::Revoked(why);
        }
        if events.is_empty() || self.commit(next, self.held.clone()).is_err() {
            return Vec::new();
        }
        events
    }

    /// The chain index of the nearest enrolled root on a caller's
    /// ancestry — any authorization state, tombstones included (see
    /// [`Self::commit`]), so a revoked or expired enrollment's
    /// processes can still release but never fall through to an outer
    /// pane for as long as that exact root lives. A pid only matches a root that is still the same process
    /// (an unreadable one matches, so the strict verifier refuses it).
    pub fn nearest_enrolled_root(&self, chain: &[u32]) -> Option<usize> {
        chain.iter().position(|pid| self.is_enrolled_root(*pid))
    }

    /// Every chain index holding an enrolled root, nearest first — the
    /// same match as [`Self::nearest_enrolled_root`]. The caller
    /// identity verifier (CAD-381) counts them all: two agent
    /// endpoints on one ancestry is an ambiguous caller, refused.
    pub fn enrolled_roots_on(&self, chain: &[u32]) -> Vec<usize> {
        (0..chain.len())
            .filter(|&i| self.is_enrolled_root(chain[i]))
            .collect()
    }

    fn is_enrolled_root(&self, pid: u32) -> bool {
        self.enrollments
            .iter()
            .any(|e| e.root.pid == pid && self.proc.same_start(pid, e.root.starttime))
    }

    /// The enrollment `id` while it still vouches for a live agent
    /// endpoint: never revoked, and owned by an agent row — a build
    /// runner's enrollment (CAD-230b) is the daemon's own command,
    /// never an agent identity. `expired` still vouches: the TTL caps
    /// build-slot admission, not who the endpoint is, and a long-lived
    /// endpoint renews only at its next open. Identity rests on the
    /// per-call checks — owner-row revalidation (revocation), verified
    /// descent to the exact root (pid + starttime + uid).
    pub fn endpoint_enrollment(&self, id: &str) -> Option<&Enrollment> {
        self.enrollments
            .iter()
            .find(|e| e.id == id && !matches!(e.auth, AuthState::Revoked(_)) && e.runner.is_none())
    }

    /// Every enrollment rooted at `root_pid`, in the one order a caller
    /// is matched against them (CAD-276): live first (`active`, then
    /// `expired`, then revoked tombstones), then the most recently
    /// issued, then the enrollment id — never file or map order, so two
    /// records sharing a root always resolve the same way.
    fn enrollments_at(&self, root_pid: u32) -> Vec<&Enrollment> {
        let rank = |e: &Enrollment| match e.auth {
            AuthState::Active => 0,
            AuthState::Expired => 1,
            AuthState::Revoked(_) => 2,
        };
        let mut at: Vec<&Enrollment> = self
            .enrollments
            .iter()
            .filter(|e| e.root.pid == root_pid)
            .collect();
        at.sort_by(|a, b| {
            rank(a)
                .cmp(&rank(b))
                .then(b.issued_epoch.total_cmp(&a.issued_epoch))
                .then(a.id.cmp(&b.id))
        });
        at
    }

    /// Verify `peer_pid` against the enrollment rooted at `root_pid`:
    /// the peer must be the root or reach it through a complete,
    /// verified ancestry (see [`ProcFs::verified_descent`]). Several
    /// enrollments on one root are tried in [`Self::enrollments_at`]
    /// order and the first that verifies wins. There is no fallback —
    /// a failed verification refuses the call.
    pub fn strict_caller(&mut self, peer_pid: u32, root_pid: u32) -> Result<StrictCaller> {
        self.heal_write_latch()?;
        let mut why = Vec::new();
        for e in self.enrollments_at(root_pid) {
            match self
                .proc
                .verified_descent(peer_pid, &e.root, self.daemon_uid)
            {
                Ok(segment) => {
                    return Ok(StrictCaller {
                        enrollment_id: e.id.clone(),
                        lane: e.owner_actor.clone(),
                        segment,
                    })
                }
                Err(e) => why.push(e),
            }
        }
        Err(Error::rejected(format!(
            "Slot caller pid {peer_pid} is not verifiably its enrolled managed \
             endpoint's process — {}",
            if why.is_empty() {
                "no enrollment".to_string()
            } else {
                why.join("; ")
            }
        )))
    }

    /// Strict acquire: the caller's enrollment must be active and the
    /// claimed `pid` on its verified segment; the holder identity is
    /// read now and bound to the hold.
    pub fn acquire_strict(
        &mut self,
        kind: SlotKind,
        caller: &StrictCaller,
        pid: u32,
        request_id: &str,
        probe: bool,
        clk: SlotClock,
    ) -> Result<(Value, Vec<SlotEvent>)> {
        self.acquire_strict_bound(kind, caller, pid, request_id, probe, clk, false)
    }

    /// [`Self::acquire_strict`], optionally exec-bound (CAD-230b): with
    /// `exec` the claimed `pid` must be the connection peer ITSELF — the
    /// `build-slot run` process that execs into the command, so the
    /// hold's recorded `(pid, starttime, uid)` is exactly the running
    /// build and ends with its exit. An ancestor is refused. A runner's
    /// own process tree never acquires: its one hold is the daemon's.
    #[allow(clippy::too_many_arguments)]
    pub fn acquire_strict_bound(
        &mut self,
        kind: SlotKind,
        caller: &StrictCaller,
        pid: u32,
        request_id: &str,
        probe: bool,
        clk: SlotClock,
        exec: bool,
    ) -> Result<(Value, Vec<SlotEvent>)> {
        self.heal_write_latch()?;
        let e = self
            .enrollments
            .iter()
            .find(|e| e.id == caller.enrollment_id)
            .ok_or_else(|| Error::rejected("Slot caller's enrollment is gone"))?;
        if let Some(runner) = &e.runner {
            return Err(runner_tree_refused(runner));
        }
        if e.auth != AuthState::Active || clk.mono >= e.expires_at {
            let state = if e.auth == AuthState::Active {
                "expired"
            } else {
                e.auth.as_str()
            };
            return Err(Error::rejected(format!(
                "Enrollment {} is {state} — a revoked or expired enrollment admits \
                 no new slot work; its holds stay until released or dead",
                e.id
            )));
        }
        let (owner, owner_generation) = (e.owner_actor.clone(), e.owner_generation.clone());
        if exec && caller.segment.first() != Some(&pid) {
            return Err(exec_not_peer(pid));
        }
        let holder = self.strict_holder(caller, pid)?;
        self.acquire_as(
            kind,
            &owner,
            pid,
            request_id,
            probe,
            clk,
            Some(StrictBind {
                enrollment_id: caller.enrollment_id.clone(),
                owner_generation,
                holder,
                liveness: Liveness::Alive,
                exec_bound: exec,
            }),
        )
    }

    /// Strict release — see [`Self::release_as`]. The caller was
    /// matched to its root's preferred enrollment; when the hold names
    /// ANOTHER enrollment on that exact root (pid and starttime — a
    /// same-root supersession left the hold under the tombstone), the
    /// same verified segment proves descent from that one too, so the
    /// release runs as the enrollment whose id matches the hold
    /// (CAD-276). The exact-holder rule still applies unchanged.
    pub fn release_strict(
        &mut self,
        token: &str,
        caller: &StrictCaller,
        pid: u32,
        now: f64,
    ) -> Result<(Value, Vec<SlotEvent>)> {
        self.heal_write_latch()?;
        if let Some(runner) = self
            .enrollments
            .iter()
            .find(|e| e.id == caller.enrollment_id)
            .and_then(|e| e.runner.as_ref())
        {
            return Err(runner_tree_refused(runner));
        }
        let caller = self.hold_enrollment_caller(token, caller);
        let lane = caller.lane.clone();
        self.release_as(token, &lane, pid, now, Some(&caller))
    }

    /// `caller` rebound to the enrollment the hold `token` names, when
    /// that enrollment shares the exact root identity AND the owner of
    /// the one the caller verified against; otherwise `caller`
    /// unchanged. Never across owners (CAD-289): one owner's verified
    /// caller must not release another owner's hold.
    fn hold_enrollment_caller(&self, token: &str, caller: &StrictCaller) -> StrictCaller {
        let by_id = |id: &str| self.enrollments.iter().find(|e| e.id == id);
        let held = self
            .held
            .iter()
            .find(|h| h.token == token)
            .and_then(SlotHold::enrollment_id)
            .filter(|id| *id != caller.enrollment_id)
            .and_then(by_id);
        match (held, by_id(&caller.enrollment_id)) {
            (Some(held), Some(verified))
                if held.root == verified.root && held.owner_actor == verified.owner_actor =>
            {
                StrictCaller {
                    enrollment_id: held.id.clone(),
                    lane: held.owner_actor.clone(),
                    segment: caller.segment.clone(),
                }
            }
            _ => caller.clone(),
        }
    }

    /// The exact identity a strict hold binds: the connection peer or
    /// one of its verified NON-ROOT ancestors, running as the daemon's
    /// uid (CAD-276). The enrolled provider root lives as long as its
    /// endpoint, so a hold a descendant binds to it (`acquire --pid
    /// <root>`) would never end with the work; `build-slot run` binds
    /// its own pid and `acquire --pid $$` its shell. The root may hold
    /// only when it is itself the peer.
    fn strict_holder(&self, caller: &StrictCaller, pid: u32) -> Result<ProcIdentity> {
        let peer = caller.segment.first() == Some(&pid);
        let below_root = caller
            .segment
            .split_last()
            .is_some_and(|(_, below)| below.contains(&pid));
        if !peer && !below_root {
            let root = caller.segment.last().copied().unwrap_or_default();
            let why = if pid == root {
                "it is the enrolled provider root, which outlives the work — \
                 claim the connection peer or a non-root ancestor (`build-slot \
                 run` binds its own pid)"
            } else {
                "it is not the connection peer or its verified ancestor below \
                 the enrolled root"
            };
            return Err(Error::rejected(format!(
                "Slot caller cannot claim pid {pid} — {why}"
            )));
        }
        let holder = self
            .proc
            .identity(pid)
            .map_err(|why| Error::rejected(format!("Slot holder pid {pid}: {why}")))?;
        if holder.uid != self.daemon_uid {
            return Err(Error::rejected(format!(
                "Slot holder pid {pid} runs as uid {} — not the daemon's uid {}",
                holder.uid, self.daemon_uid
            )));
        }
        Ok(holder)
    }

    /// Whether strict admission is available now.
    pub fn strict_available(&self) -> bool {
        self.blocked.is_none()
    }

    /// The one mutating operator path over a strict hold: a named
    /// reconcile of `(enrollment_id, token)` carrying the operator's
    /// evidence. The evidence must name the hold's recorded identity
    /// exactly; the daemon then reads `/proc` itself and frees the
    /// hold only on proven death — a live or unknown holder is refused,
    /// whatever the evidence says. Deliberately no reap pass first:
    /// the decision is this call's own observation.
    pub fn reconcile(
        &mut self,
        enrollment_id: &str,
        token: &str,
        evidence: &Value,
        now: f64,
    ) -> Result<(Value, Vec<SlotEvent>)> {
        const REQUIRED: [&str; 8] = [
            "owner_generation",
            "pid",
            "starttime",
            "uid",
            "observed_at",
            "process_read",
            "command_outcome",
            "side_effect_review",
        ];
        let Some((idx, b)) = self.held.iter().enumerate().find_map(|(i, h)| {
            let b = h
                .strict
                .as_ref()
                .filter(|b| b.enrollment_id == enrollment_id)?;
            (h.token == token).then(|| (i, b.clone()))
        }) else {
            return Err(Error::rejected(format!(
                "No strict hold '{token}' under enrollment {enrollment_id}"
            )));
        };
        if let Some(missing) = REQUIRED
            .iter()
            .find(|k| evidence.get(**k).is_none_or(Value::is_null))
        {
            return Err(Error::rejected(format!(
                "Reconcile evidence must carry {} — missing '{missing}'",
                REQUIRED.join(", ")
            )));
        }
        let claimed = ProcIdentity::from_json(evidence);
        if evidence["owner_generation"].as_str() != Some(b.owner_generation.as_str())
            || claimed != Some(b.holder)
        {
            return Err(Error::rejected(
                "Reconcile evidence does not match the recorded hold — owner \
                 generation, pid, starttime and uid must be the recorded ones",
            ));
        }
        match self.proc.liveness(&b.holder) {
            (Liveness::Alive, _) => Err(Error::rejected(format!(
                "Holder pid {} is alive — reconcile never frees a live hold; \
                 release it from the holder or let it exit",
                b.holder.pid
            ))),
            (Liveness::Unknown, why) => Err(Error::rejected(format!(
                "Holder pid {} liveness is unknown ({why}) — reconcile never \
                 frees a hold it cannot prove dead",
                b.holder.pid
            ))),
            (Liveness::Dead, why) => {
                let mut next = self.held.clone();
                let h = next.remove(idx);
                self.commit(self.enrollments.clone(), next)?;
                let payload = json!({"kind": h.kind.as_str(), "pid": h.pid,
                                     "held_secs": (now - h.acquired_at).max(0.0),
                                     "reason": format!("reconciled: {why}"),
                                     "enrollment_id": enrollment_id,
                                     "evidence": evidence});
                Ok((
                    json!({"reconciled": true, "token": token, "kind": h.kind.as_str(),
                           "observed": why}),
                    vec![(h.lane, "slot_released", payload)],
                ))
            }
        }
    }
}

/// Runners (CAD-230b) — daemon-launched processes that hold one strict,
/// exec-bound slot for their whole life. Only the daemon enrolls,
/// queues, grants and ends them; nothing here takes a caller's word.
impl Slots {
    /// The lane a strict caller may launch a runner for: its
    /// enrollment must be an active managed endpoint's. A runner's own
    /// process tree never launches another runner.
    pub fn launch_lane(&self, caller: &StrictCaller, now: f64) -> Result<String> {
        if let Some(b) = &self.blocked {
            return Err(strict_unavailable(&b.reason));
        }
        let e = self
            .enrollments
            .iter()
            .find(|e| e.id == caller.enrollment_id)
            .ok_or_else(|| Error::rejected("Launch caller's enrollment is gone"))?;
        if let Some(runner) = &e.runner {
            return Err(runner_tree_refused(runner));
        }
        if e.auth != AuthState::Active || now >= e.expires_at {
            return Err(Error::rejected(format!(
                "Enrollment {} is not active — a revoked or expired managed \
                 endpoint launches no runner",
                e.id
            )));
        }
        Ok(e.owner_actor.clone())
    }

    /// Enroll the process the daemon just spawned for runner
    /// `runner_id`: root = worker = that exact process (pid, starttime,
    /// uid as `/proc` reads it now), accounted to `lane`, its owner
    /// generation binding the launch-intent `digest` to the runner id.
    /// Refuses what [`Self::enroll`] refuses, a process that is already
    /// enrolled, and a runner id already in use.
    pub fn enroll_runner(
        &mut self,
        lane: &str,
        runner_id: &str,
        digest: &str,
        pid: u32,
        clk: SlotClock,
    ) -> Result<(String, ProcIdentity, Vec<SlotEvent>)> {
        self.heal_write_latch()?;
        if pid <= 1 || pid == std::process::id() {
            return Err(Error::rejected(format!(
                "pid {pid} cannot be a runner root — the daemon never enrolls \
                 itself or init"
            )));
        }
        let root = self
            .proc
            .identity(pid)
            .map_err(|why| Error::rejected(format!("cannot enroll runner pid {pid}: {why}")))?;
        if root.uid != self.daemon_uid {
            return Err(Error::rejected(format!(
                "cannot enroll runner pid {pid}: it runs as uid {} — not the daemon's uid {}",
                root.uid, self.daemon_uid
            )));
        }
        if let Some(e) = self
            .enrollments
            .iter()
            .find(|e| e.root == root || e.runner.as_deref() == Some(runner_id))
        {
            return Err(Error::rejected(format!(
                "cannot enroll runner {runner_id} (pid {pid}): already enrolled as {}",
                e.id
            )));
        }
        let id = format!("enr-{}", Uuid::new_v4().simple());
        let mut next = self.enrollments.clone();
        next.push(Enrollment {
            id: id.clone(),
            owner_actor: lane.to_string(),
            owner_generation: format!("runner:{runner_id}:{digest}"),
            runner: Some(runner_id.to_string()),
            root,
            worker: root,
            issued_epoch: clk.wall,
            expires_epoch: clk.wall + ENROLLMENT_TTL_SECS,
            expires_at: clk.mono + ENROLLMENT_TTL_SECS,
            auth: AuthState::Active,
        });
        self.commit(next, self.held.clone())?;
        let event = (
            lane.to_string(),
            "slot_runner_enrolled",
            json!({"enrollment_id": id, "runner_id": runner_id, "root_pid": pid}),
        );
        Ok((id, root, vec![event]))
    }

    /// One queue poll for runner enrollment `enrollment_id`: a strict,
    /// exec-bound request for its exact root process under the
    /// runner's lane, `request_id` = the runner id. The root must still
    /// be the enrolled process (pid AND starttime) — a recycled pid is
    /// refused, never queued or granted.
    pub fn acquire_runner(
        &mut self,
        enrollment_id: &str,
        kind: SlotKind,
        clk: SlotClock,
    ) -> Result<(Value, Vec<SlotEvent>)> {
        self.heal_write_latch()?;
        let e = self
            .enrollments
            .iter()
            .find(|e| e.id == enrollment_id)
            .ok_or_else(|| Error::rejected("Runner enrollment is gone"))?;
        let Some(runner_id) = e.runner.clone() else {
            return Err(Error::rejected(format!(
                "Enrollment {enrollment_id} is not a runner's"
            )));
        };
        if e.auth != AuthState::Active || clk.mono >= e.expires_at {
            return Err(Error::rejected(format!(
                "Runner enrollment {enrollment_id} is no longer active"
            )));
        }
        let (lane, owner_generation, root) =
            (e.owner_actor.clone(), e.owner_generation.clone(), e.root);
        let holder = self
            .proc
            .identity(root.pid)
            .map_err(|why| Error::rejected(format!("runner pid {}: {why}", root.pid)))?;
        if holder != root {
            return Err(Error::rejected(format!(
                "runner pid {} is no longer the enrolled process",
                root.pid
            )));
        }
        self.acquire_as(
            kind,
            &lane,
            root.pid,
            &runner_id,
            false,
            clk,
            Some(StrictBind {
                enrollment_id: enrollment_id.to_string(),
                owner_generation,
                holder,
                liveness: Liveness::Alive,
                exec_bound: true,
            }),
        )
    }

    /// The runner ended — its process exited and was reaped by the
    /// daemon, or never started. Frees its hold only through the
    /// tri-state rule (the holder must read `dead`), drops its waiter,
    /// and revokes the enrollment, which [`Self::commit`] prunes once
    /// nothing holds under it.
    pub fn end_runner(&mut self, enrollment_id: &str, reason: &str, now: f64) -> Vec<SlotEvent> {
        let mut events = self.reap_strict_holds(now);
        self.waiting
            .retain(|w| w.strict.as_ref().map(|b| b.enrollment_id.as_str()) != Some(enrollment_id));
        let reason = reason.to_string();
        events.extend(self.revoke_where(|e| e.id == enrollment_id, |_| reason.clone()));
        events
    }
}

/// A slot call from inside a daemon-launched runner's ATTACHED process
/// tree (its verified ancestry reaches the runner's enrolled root). A
/// descendant that detaches and scrubs `CADENCE_RUNNER_ID` leaves that
/// tree and can pass operator proof instead — the known residual,
/// closed by the daemon child-subreaper follow-up (CAD-308).
fn runner_tree_refused(runner: &str) -> Error {
    Error::rejected(format!(
        "This process runs under daemon-launched runner {runner}, which already \
         holds its slot — a recipe must not nest build-slot acquire/run/release \
         or launch; its hold ends when the runner exits"
    ))
}

/// An exec-bound request claiming anything but the connection peer.
pub(crate) fn exec_not_peer(pid: u32) -> Error {
    Error::rejected(format!(
        "An exec-bound hold names the requesting process itself — pid {pid} is \
         not the connection peer (`build-slot run` binds its own pid, then execs)"
    ))
}

fn strict_unavailable(reason: &str) -> Error {
    Error::rejected(format!(
        "Strict build-slot admission is unavailable — {reason}"
    ))
}

/// Borrowed acquire inputs — `grant`/`find_hold` take them as one
/// argument.
struct SlotReq<'a> {
    kind: SlotKind,
    lane: &'a str,
    pid: u32,
    request_id: &'a str,
    strict: Option<StrictBind>,
}

impl SlotReq<'_> {
    fn enrollment_id(&self) -> Option<&str> {
        self.strict.as_ref().map(|b| b.enrollment_id.as_str())
    }

    /// The holder's starttime — read from `/proc` for a legacy caller,
    /// the verified identity for a strict one.
    fn pid_start(&self) -> Option<u64> {
        match &self.strict {
            Some(b) => Some(b.holder.starttime),
            None => pid_start(self.pid),
        }
    }
}

/// The caller identity a status read runs as — connection-derived by
/// the daemon, never asserted (CAD-113): `lane` is the registered
/// pane the socket peer descends from and `pids` is every pid the
/// caller may claim as a hold owner — the peer plus its /proc
/// ancestors. A hold's token is visible only to its owner: same lane
/// AND the hold's pid on the caller's own chain, so a lane-mate in a
/// different process still cannot see it.
#[derive(Clone, Copy)]
pub struct SlotCaller<'a> {
    pub lane: &'a str,
    pub pids: &'a [u32],
}

/// Resolve the caller's slot lane the same way everywhere:
/// `$CADENCE_ALIAS`, else `$USER`, else `unknown`.
pub fn default_lane() -> String {
    std::env::var("CADENCE_ALIAS")
        .ok()
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "unknown".to_string())
}

/// CAD-1021: the live host resource reading — `MemAvailable` from
/// `/proc/meminfo` and free bytes on `path`'s filesystem via statvfs.
/// Each field is `Option` so an unreadable probe is "unmeasurable"
/// (the floor treats it as not-gating), never a refused admission.
mod probe {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    fn meminfo_available() -> Option<u64> {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in text.lines() {
            let (key, rest) = line.split_once(':')?;
            if key.trim() == "MemAvailable" {
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb.saturating_mul(1024));
            }
        }
        None
    }

    fn fs_free(path: &Path) -> Option<u64> {
        let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut st = unsafe { std::mem::zeroed::<libc::statvfs>() };
        if unsafe { libc::statvfs(c_path.as_ptr(), &mut st) } != 0 {
            return None;
        }
        #[allow(clippy::useless_conversion)]
        Some(u64::from(st.f_bavail).saturating_mul(u64::from(st.f_frsize)))
    }

    /// `(mem_available, disk_free)` — the daemon probes the state dir's
    /// filesystem (where slots.json lives, so it shares its device).
    pub(super) fn live(disk_path: Option<&Path>) -> (Option<u64>, Option<u64>) {
        (meminfo_available(), disk_path.and_then(fs_free))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These tests exercise the per-pool machinery in isolation, so
    /// they opt out of CAD-1268's aggregate cap explicitly. Tests for
    /// the shared budget itself build their own `SlotConfig` below.
    fn slots(build: usize, suite: usize, starve: u64, priority: &[&str]) -> Slots {
        Slots::new(SlotConfig {
            build_slots: build,
            suite_slots: suite,
            starve_secs: starve,
            priority_lanes: priority.iter().map(|s| s.to_string()).collect(),
            total_slots: 64,
            ..Default::default()
        })
    }

    /// The test process's own pid — alive for the whole test.
    fn me() -> u32 {
        std::process::id()
    }

    /// A `status` caller owning exactly `pids` — the derived identity
    /// the daemon hands down.
    fn sc<'a>(lane: &'a str, pids: &'a [u32]) -> SlotCaller<'a> {
        SlotCaller { lane, pids }
    }

    fn acquire(s: &mut Slots, kind: SlotKind, lane: &str, req: &str, now: f64) -> Value {
        s.acquire(
            kind,
            lane,
            me(),
            req,
            false,
            SlotClock::at(now, 1_000_000.0),
        )
        .unwrap()
        .0
    }

    fn acquire_pid(
        s: &mut Slots,
        kind: SlotKind,
        lane: &str,
        pid: u32,
        req: &str,
        now: f64,
    ) -> Value {
        s.acquire(kind, lane, pid, req, false, SlotClock::at(now, 1_000_000.0))
            .unwrap()
            .0
    }

    fn probe(s: &mut Slots, kind: SlotKind, lane: &str, req: &str, now: f64) -> Value {
        s.acquire(kind, lane, me(), req, true, SlotClock::at(now, 1_000_000.0))
            .unwrap()
            .0
    }

    fn release(s: &mut Slots, token: &str, lane: &str, now: f64) -> Value {
        s.release(token, lane, me(), now).unwrap().0
    }

    /// Release the first hold — the token is copied out before the
    /// mutable borrow starts.
    fn release_first(s: &mut Slots, lane: &str, now: f64) -> Value {
        let token = s.held[0].token.clone();
        release(s, &token, lane, now)
    }

    /// Release the current hold whichever lane owns it — the
    /// contention tests hand the slot between lanes. The release
    /// names the hold's recorded lane AND pid: requester children
    /// own these holds, so `me()` would be a foreign caller.
    fn release_any(s: &mut Slots, now: f64) -> Value {
        let h = &s.held[0];
        let (token, lane, pid) = (h.token.clone(), h.lane.clone(), h.pid);
        s.release(&token, &lane, pid, now).unwrap().0
    }

    /// A real child pid distinct from the test process — killed on drop.
    struct Child(std::process::Child);
    impl Child {
        fn spawn() -> Self {
            Self(
                std::process::Command::new("sleep")
                    .arg("600")
                    .spawn()
                    .unwrap(),
            )
        }
        fn pid(&self) -> u32 {
            self.0.id()
        }
        fn reap(mut self) {
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn fifo_within_a_pool() {
        let mut s = slots(1, 1, 900, &[]);
        let a = acquire(&mut s, SlotKind::Build, "dev-1", "r1", 100.0);
        assert_eq!(a["granted"], true);
        let t1 = a["token"].as_str().unwrap().to_string();
        assert!(t1.starts_with("slot-"), "daemon mints the token: {t1}");
        let b = acquire(&mut s, SlotKind::Build, "dev-2", "r2", 100.0);
        assert_eq!(b["granted"], false);
        release(&mut s, &t1, "dev-1", 110.0);
        let b = acquire(&mut s, SlotKind::Build, "dev-2", "r2", 110.0);
        assert_eq!(b["granted"], true);
        // Re-poll is idempotent — the same minted token back.
        let again = acquire(&mut s, SlotKind::Build, "dev-2", "r2", 111.0);
        assert_eq!(again["token"], b["token"]);
    }

    /// BLOCKER: two callers sharing a request_id — the second queues,
    /// it never adopts the first's hold.
    #[test]
    fn duplicate_request_id_from_another_pid_queues() {
        let mut s = slots(1, 1, 900, &[]);
        let other = Child::spawn();
        let a = acquire_pid(&mut s, SlotKind::Build, "dev-1", other.pid(), "r1", 0.0);
        assert_eq!(a["granted"], true);
        // Same request_id, different pid — a different caller: queued.
        let b = acquire(&mut s, SlotKind::Build, "dev-1", "r1", 0.0);
        assert_eq!(b["granted"], false, "must not adopt another caller's hold");
        assert_eq!(b["position"], 1);
        // A second request identity while the first still queues is
        // refused outright — one caller, one queued request — even
        // under the SAME request_id with a different kind.
        let err = s
            .acquire(
                SlotKind::Test,
                "dev-1",
                me(),
                "r1",
                false,
                SlotClock::at(0.0, 0.0),
            )
            .unwrap_err();
        assert!(err.to_string().contains("deadlock guard"), "{err}");
        // A same-identity re-poll still adopts its own hold.
        let a2 = acquire_pid(&mut s, SlotKind::Build, "dev-1", other.pid(), "r1", 0.0);
        assert_eq!(a2["token"], a["token"]);
        other.reap();
    }

    /// BLOCKER: release must come from the holding (lane, pid) — a
    /// foreign token is a named refusal, not a free pass.
    #[test]
    fn release_foreign_token_is_rejected() {
        let mut s = slots(1, 1, 900, &[]);
        let other = Child::spawn();
        let a = acquire_pid(&mut s, SlotKind::Build, "dev-1", other.pid(), "r1", 0.0);
        let token = a["token"].as_str().unwrap().to_string();
        // Wrong pid — even the right lane cannot release.
        let err = s.release(&token, "dev-1", me(), 0.0).unwrap_err();
        assert!(err.to_string().contains("another caller"), "{err}");
        // Wrong lane AND wrong pid — still refused.
        let err = s.release(&token, "dev-2", me(), 0.0).unwrap_err();
        assert!(err.to_string().contains("another caller"), "{err}");
        // The hold survives the failed releases.
        assert_eq!(
            s.status(sc("dev-1", &[other.pid()]), 0.0).0["pools"]["build"]["held"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // The true caller releases normally.
        let ok = s.release(&token, "dev-1", other.pid(), 0.0).unwrap();
        assert_eq!(ok.0["released"], true);
        other.reap();
    }

    /// A dead holder is reaped by the next operation — and a release
    /// racing that reap answers softly: the daemon already took the
    /// slot back, so `released:false` + the reap reason instead of a
    /// hard error a `trap`-style cleanup would trip on.
    #[test]
    fn reap_racing_release_is_deterministic() {
        let mut s = slots(1, 1, 900, &[]);
        let mut child = std::process::Command::new("sleep")
            .arg("600")
            .spawn()
            .unwrap();
        let pid = child.id();
        let a = acquire_pid(&mut s, SlotKind::Build, "dev-1", pid, "r1", 0.0);
        let token = a["token"].as_str().unwrap().to_string();
        child.kill().unwrap();
        child.wait().unwrap();
        // The release's own reap pass wins: the token is already gone
        // by the time the lookup runs — and the reply says so.
        let (r, events) = s.release(&token, "dev-1", pid, 1.0).unwrap();
        assert_eq!(r["released"], false);
        assert_eq!(r["reason"], "holder died");
        assert!(events.iter().any(|e| e.1 == "slot_released"
            && e.2["reason"] == "holder died"
            && e.2.get("token").is_none()));
        // A token that was never granted is still a hard rejection.
        let err = s
            .release("slot-never-granted", "dev-1", pid, 1.0)
            .unwrap_err();
        assert!(err.to_string().contains("Unknown slot token"), "{err}");
        // The reap emitted slot_released with the cause.
        let (status, _) = s.status(sc("dev-1", &[pid]), 2.0);
        assert!(status["pools"]["build"]["held"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    /// pid starttime: a live pid whose recorded start differs is a
    /// recycled process — the hold does not survive it.
    #[test]
    fn recycled_pid_does_not_keep_the_hold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slots.json");
        // A persisted hold on THIS pid but a bogus starttime — as if
        // the pid were recycled after the daemon wrote the file.
        let bogus = pid_start(me()).unwrap_or(0).wrapping_add(1);
        std::fs::write(
            &path,
            json!({"version": 1, "holds": [
                {"token": "slot-old", "request_id": "r1", "kind": "build",
                 "lane": "dev-1", "pid": me(), "pid_start": bogus,
                 "acquired_epoch": 900.0},
                {"token": "slot-live", "request_id": "r2", "kind": "build",
                 "lane": "dev-2", "pid": me(), "pid_start": pid_start(me()),
                 "acquired_epoch": 900.0},
                {"token": "slot-dead", "request_id": "r3", "kind": "build",
                 "lane": "dev-3", "pid": 4_000_000, "pid_start": 1,
                 "acquired_epoch": 900.0},
            ], "seniority": []})
            .to_string(),
        )
        .unwrap();
        let mut s = slots(3, 1, 900, &[]);
        s.persist_to(path.clone());
        let events = s.restore(SlotClock::at(10.0, 1000.0));
        assert_eq!(s.held.len(), 1);
        assert_eq!(s.held[0].token, "slot-live");
        // Its age carries across the clock epoch change: acquired at
        // wall 900, now wall 1000 → 100s held.
        assert_eq!(s.held[0].acquired_at, 10.0 - 100.0);
        let reasons: Vec<String> = events
            .iter()
            .map(|(_, _, p)| p["reason"].as_str().unwrap().to_string())
            .collect();
        assert!(reasons.contains(&"pid recycled".to_string()), "{reasons:?}");
        assert!(reasons.contains(&"holder died".to_string()), "{reasons:?}");
        // The canonical rewrite dropped the dead entries from disk.
        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk["holds"].as_array().unwrap().len(), 1);
    }

    /// Mono vs wall: a wall-clock jump must not age or freeze waits —
    /// every age derives from the monotonic `now`.
    #[test]
    fn clock_jump_does_not_touch_wait_ages() {
        let mut s = slots(1, 1, 900, &[]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 10.0);
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 10.0);
        // Wall jumps backwards an hour between polls — wait_secs still
        // reports the 10s of monotonic time that actually passed.
        let q = s
            .acquire(
                SlotKind::Build,
                "dev-2",
                me(),
                "w1",
                false,
                SlotClock::at(20.0, -1_000_000.0),
            )
            .unwrap()
            .0;
        assert_eq!(q["wait_secs"], 10.0);
    }

    /// The episode anchor is carried by ONE waiter — the lane's
    /// eldest unserved request of that kind. Later arrivals of a
    /// burst stamp their own arrival, so a lane cannot multiply a
    /// stale anchor into a queue of front-runners; the anchor
    /// survives a brief polling gap (a requeue under a new
    /// request_id keeps the lane's place) but dies with the first
    /// serve for that `(lane, kind)`.
    #[test]
    fn only_the_eldest_waiter_inherits_the_anchor() {
        let mut s = slots(1, 1, 60, &[]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        // dev-2 waits on build from t=0 — its eldest waiter w1
        // carries the t=0 anchor.
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 0.0);
        // qa-1 queues honestly at t=5.
        acquire(&mut s, SlotKind::Build, "qa-1", "qb", 5.0);
        // dev-2 bursts three more requests at t=10 — on the pre-r4
        // head all three inherited t=0 and pinned qa-1 behind them;
        // now they stamp their own arrival. Each burst requester is a
        // DISTINCT live process: one caller holds at most one queued
        // request (the wait-then-hold guard), so a lane burst is
        // always a fan-in of separate callers.
        let burst: Vec<Child> = (0..3).map(|_| Child::spawn()).collect();
        for (i, req) in ["w2", "w3", "w4"].iter().enumerate() {
            acquire_pid(&mut s, SlotKind::Build, "dev-2", burst[i].pid(), req, 10.0);
        }
        release_first(&mut s, "dev-1", 20.0);
        let g = acquire(&mut s, SlotKind::Build, "dev-2", "w1", 20.0);
        assert_eq!(g["granted"], true, "the eldest keeps the anchor");
        release_first(&mut s, "dev-2", 21.0);
        let g = acquire(&mut s, SlotKind::Build, "qa-1", "qb", 21.0);
        assert_eq!(g["granted"], true, "qa-1's t=5 beats the t=10 burst");
        release_first(&mut s, "qa-1", 22.0);
        let g = acquire_pid(&mut s, SlotKind::Build, "dev-2", burst[0].pid(), "w2", 22.0);
        assert_eq!(g["granted"], true, "the burst follows in arrival order");
    }

    /// A requeue gap keeps the episode only while the lane's last
    /// poll is still inside the waiter TTL: a caller whose process
    /// dies mid-wait gets its anchor carried to the restarted
    /// request, but a lane that just went quiet spent the window
    /// already — the next request anchors fresh.
    #[test]
    fn anchor_survives_a_polling_gap_but_not_silence() {
        let mut s = slots(1, 1, 60, &[]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        // dev-2's caller is a real process that polls at t=20 and is
        // then killed — the waiter reaps on pid death with a fresh
        // last_poll, leaving one TTL of gap grace.
        let child = Child::spawn();
        acquire_pid(&mut s, SlotKind::Build, "dev-2", child.pid(), "w1", 0.0);
        acquire_pid(&mut s, SlotKind::Build, "dev-2", child.pid(), "w1", 20.0);
        child.reap();
        // The requeue under a new request_id lands inside the window
        // and inherits the t=0 anchor.
        let g = acquire(&mut s, SlotKind::Build, "dev-2", "w2", 25.0);
        assert_eq!(g["wait_secs"], 25.0, "the requeue keeps the wait");
        // But a lane that goes QUIET (alive pid, silent past the TTL)
        // ends its episode — a later request anchors at arrival.
        let mut s = slots(1, 1, 60, &[]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 0.0);
        let g = acquire(&mut s, SlotKind::Build, "dev-2", "w2", 45.0);
        assert_eq!(g["wait_secs"], 0.0, "silence spent the window");
    }

    /// Every serve ends the episode: after dev-2's eldest grants,
    /// the `(dev-2, build)` anchor is gone — a new request stamps
    /// its own arrival even inside the gap window.
    #[test]
    fn a_grant_still_ends_the_episode() {
        let mut s = slots(1, 1, 60, &[]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 0.0);
        acquire(&mut s, SlotKind::Build, "qa-1", "qb", 5.0);
        release_first(&mut s, "dev-1", 10.0);
        let g = acquire(&mut s, SlotKind::Build, "dev-2", "w1", 10.0);
        assert_eq!(g["granted"], true, "anchor served the eldest");
        release_first(&mut s, "dev-2", 12.0);
        // qa-1 (t=5) grants next — dev-2's new request at t=13 must
        // NOT resurrect the t=0 episode.
        acquire(&mut s, SlotKind::Build, "dev-2", "w2", 13.0);
        let g = acquire(&mut s, SlotKind::Build, "qa-1", "qb", 14.0);
        assert_eq!(g["granted"], true, "post-serve request ranks honestly");
    }

    /// A probe is read-only: it must never arm a seniority anchor
    /// for a lane that hasn't actually queued — otherwise a probe
    /// would mint an anchor older than the lane's real first
    /// request.
    #[test]
    fn a_probe_never_arms_an_anchor() {
        let mut s = slots(1, 1, 60, &[]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        // dev-2 probes at t=0 — this must NOT plant a t=0 anchor.
        let p = s
            .acquire(
                SlotKind::Build,
                "dev-2",
                me(),
                "p1",
                true,
                SlotClock::at(0.0, 0.0),
            )
            .unwrap()
            .0;
        assert_eq!(p["granted"], false);
        // qa-1 queues honestly at t=5; dev-2 really queues at t=10.
        acquire(&mut s, SlotKind::Build, "qa-1", "q1", 5.0);
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 10.0);
        release_first(&mut s, "dev-1", 20.0);
        let g = acquire(&mut s, SlotKind::Build, "qa-1", "q1", 20.0);
        assert_eq!(g["granted"], true, "qa-1's t=5 beats dev-2's real t=10");
    }

    /// Two waiters past `starve_secs` keep their FIFO order — the
    /// starvation bound guarantees a grant, not a tie: a 2h waiter
    /// still outranks a 901s one.
    #[test]
    fn starved_waiters_keep_their_order() {
        let mut s = slots(1, 1, 60, &[]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        // Two lanes waiting since t=0 and t=10 — at t=70 both are
        // starved, but the elder still grants first. (Both re-poll
        // inside the 30s waiter TTL so the original stamps hold.)
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 0.0);
        acquire(&mut s, SlotKind::Build, "dev-3", "x1", 10.0);
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 25.0);
        acquire(&mut s, SlotKind::Build, "dev-3", "x1", 35.0);
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 50.0);
        acquire(&mut s, SlotKind::Build, "dev-3", "x1", 60.0);
        release_first(&mut s, "dev-1", 70.0);
        let g = acquire(&mut s, SlotKind::Build, "dev-3", "x1", 70.0);
        assert_eq!(g["granted"], false, "the elder starved waiter is first");
        let g = acquire(&mut s, SlotKind::Build, "dev-2", "w1", 70.0);
        assert_eq!(g["granted"], true);
        release_first(&mut s, "dev-2", 71.0);
        let g = acquire(&mut s, SlotKind::Build, "dev-3", "x1", 71.0);
        assert_eq!(g["granted"], true, "then the younger");
    }

    /// max_hold_secs reaps a forgotten hold — one caller can never
    /// wedge a pool forever.
    #[test]
    fn hold_expires_at_max_hold() {
        let mut s = Slots::new(SlotConfig {
            build_slots: 1,
            max_hold_secs: 100,
            total_slots: 64,
            ..Default::default()
        });
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        let g = acquire(&mut s, SlotKind::Build, "dev-2", "w1", 50.0);
        assert_eq!(g["granted"], false, "within max_hold the hold stands");
        let (g, events) = s
            .acquire(
                SlotKind::Build,
                "dev-2",
                me(),
                "w1",
                false,
                SlotClock::at(101.0, 0.0),
            )
            .unwrap();
        assert_eq!(g["granted"], true, "expired hold frees the pool");
        let reasons: Vec<String> = events
            .iter()
            .map(|(_, _, p)| p["reason"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(reasons.contains(&"hold expired".to_string()), "{reasons:?}");
    }

    /// Hold-and-wait guard: a lane may never QUEUE for one pool while
    /// holding a slot in the other — but a grant that never waits is
    /// always fine.
    #[test]
    fn pool_order_guard() {
        let mut s = slots(1, 1, 900, &[]);
        // Fill the build pool, then qa-1 takes the suite slot.
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        acquire(&mut s, SlotKind::Suite, "qa-1", "s1", 0.0);
        // Holding suite and QUEUEING for build — refused.
        let err = s
            .acquire(
                SlotKind::Build,
                "qa-1",
                me(),
                "b1",
                false,
                SlotClock::at(0.0, 0.0),
            )
            .unwrap_err();
        assert!(err.to_string().contains("deadlock guard"), "{err}");
        // And the symmetric direction: holding build and queueing for
        // suite is refused too — waiters never hold the other pool.
        let mut s = slots(1, 1, 900, &[]);
        acquire(&mut s, SlotKind::Suite, "qa-2", "s0", 0.0);
        acquire(&mut s, SlotKind::Build, "qa-1", "b1", 0.0);
        let err = s
            .acquire(
                SlotKind::Suite,
                "qa-1",
                me(),
                "s1",
                false,
                SlotClock::at(0.0, 0.0),
            )
            .unwrap_err();
        assert!(err.to_string().contains("deadlock guard"), "{err}");
        // A grant that never waits is allowed in either direction:
        // hold suite, take a free build slot without queueing.
        let mut s = slots(2, 1, 900, &[]);
        acquire(&mut s, SlotKind::Suite, "qa-1", "s1", 0.0);
        let g = acquire(&mut s, SlotKind::Build, "qa-1", "b1", 0.0);
        assert_eq!(g["granted"], true, "free-pool grant never waits");
    }

    /// status shows a token only to the holding process's own chain —
    /// same lane AND the hold's pid among the caller's claimable pids.
    /// Other lanes, other processes — even a lane-mate in a different
    /// process — see holder identity only.
    #[test]
    fn status_hides_foreign_tokens() {
        let mut s = slots(2, 1, 900, &[]);
        let other = Child::spawn();
        let a = acquire_pid(&mut s, SlotKind::Build, "dev-1", other.pid(), "r1", 0.0);
        let b = acquire(&mut s, SlotKind::Build, "dev-2", "r2", 0.0);
        let t1 = a["token"].as_str().unwrap();
        let t2 = b["token"].as_str().unwrap();
        // The owner sees its own token, never a foreign one.
        let (s1, _) = s.status(sc("dev-1", &[me(), other.pid()]), 0.0);
        let held = s1["pools"]["build"]["held"].as_array().unwrap();
        let mine = held.iter().find(|h| h["lane"] == "dev-1").unwrap();
        let theirs = held.iter().find(|h| h["lane"] == "dev-2").unwrap();
        assert_eq!(mine["token"], t1);
        assert!(
            theirs.get("token").is_none(),
            "foreign token hidden: {theirs}"
        );
        // A lane-MATE in a different process owns nothing: same lane,
        // pids not on the hold — its token stays hidden too.
        let (s1b, _) = s.status(sc("dev-1", &[me()]), 0.0);
        let held = s1b["pools"]["build"]["held"].as_array().unwrap();
        assert!(held.iter().all(|h| h.get("token").is_none()));
        // An unaffiliated caller sees no tokens at all.
        let (s2, _) = s.status(sc("unknown", &[me()]), 0.0);
        let held = s2["pools"]["build"]["held"].as_array().unwrap();
        assert!(held.iter().all(|h| h.get("token").is_none()));
        let _ = (t1, t2);
        other.reap();
    }

    /// The queue is bounded — the global cap refuses new request ids
    /// (four lanes at the per-lane bound fill it to the brim).
    #[test]
    fn queue_cap_rejects_overflow() {
        let mut s = slots(1, 1, 900, &[]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        // Each queued request is a distinct live process — one caller
        // holds at most one queued request.
        let kids: Vec<Child> = (0..MAX_WAITERS).map(|_| Child::spawn()).collect();
        let mut next = kids.iter();
        for lane in 0..4 {
            for i in 0..MAX_WAITERS_PER_LANE {
                let q = acquire_pid(
                    &mut s,
                    SlotKind::Build,
                    &format!("dev-w{lane}"),
                    next.next().unwrap().pid(),
                    &format!("w{i}"),
                    0.0,
                );
                assert_eq!(q["granted"], false);
            }
        }
        assert_eq!(s.waiting.len(), MAX_WAITERS);
        let err = s
            .acquire(
                SlotKind::Build,
                "dev-9",
                me(),
                "one-more",
                false,
                SlotClock::at(0.0, 0.0),
            )
            .unwrap_err();
        assert!(err.to_string().contains("queue is full"), "{err}");
    }

    /// One lane's share of the queue is bounded too — a lane past its
    /// own cap is refused while other lanes still queue fine.
    #[test]
    fn per_lane_queue_cap_bounds_one_lanes_share() {
        let mut s = slots(1, 1, 900, &[]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        // One caller queues at most one request — the lane's share is
        // filled by distinct live processes.
        let kids: Vec<Child> = (0..MAX_WAITERS_PER_LANE).map(|_| Child::spawn()).collect();
        for (i, kid) in kids.iter().enumerate() {
            let q = acquire_pid(
                &mut s,
                SlotKind::Build,
                "dev-2",
                kid.pid(),
                &format!("w{i}"),
                0.0,
            );
            assert_eq!(q["granted"], false);
        }
        let err = s
            .acquire(
                SlotKind::Build,
                "dev-2",
                me(),
                "one-more",
                false,
                SlotClock::at(0.0, 0.0),
            )
            .unwrap_err();
        assert!(err.to_string().contains("32 slot requests"), "{err}");
        // The refusal is per-lane — the queue itself still has room.
        let q = acquire(&mut s, SlotKind::Build, "dev-3", "b1", 0.0);
        assert_eq!(q["granted"], false);
    }

    #[test]
    fn priority_lane_outranks_but_starve_wins() {
        let mut s = slots(1, 1, 60, &["qa-1"]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 1.0);
        // Reviewer test arrives later but outranks the build waiter.
        acquire(&mut s, SlotKind::Test, "qa-1", "w2", 2.0);
        release_first(&mut s, "dev-1", 3.0);
        let g = acquire(&mut s, SlotKind::Build, "dev-2", "w1", 3.0);
        assert_eq!(g["granted"], false, "ordinary build must yield");
        let g = acquire(&mut s, SlotKind::Test, "qa-1", "w2", 3.0);
        assert_eq!(g["granted"], true);
        // After starve_secs the ordinary waiter jumps priority —
        // re-polling along the way, as a real caller would (the
        // waiter TTL reaps only polls gone silent).
        let mut s = slots(1, 1, 60, &["qa-1"]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 1.0);
        for t in [20.0, 45.0] {
            acquire(&mut s, SlotKind::Build, "dev-2", "w1", t); // keep the poll fresh
        }
        acquire(&mut s, SlotKind::Test, "qa-1", "w2", 70.0); // w1 starved at 61
        release_first(&mut s, "dev-1", 71.0);
        let g = acquire(&mut s, SlotKind::Build, "dev-2", "w1", 71.0);
        assert_eq!(g["granted"], true, "starved waiter wins: {g}");
    }

    #[test]
    fn suite_pool_is_independent() {
        let mut s = slots(1, 1, 900, &[]);
        acquire(&mut s, SlotKind::Suite, "qa-1", "s1", 0.0);
        // The second suite request comes from another lane: qa-1's
        // own process already holds, and under the shared-budget
        // hold-and-wait guard it could never queue (refused, not
        // queued) — the queueing caller here is qa-2.
        let q = acquire(&mut s, SlotKind::Suite, "qa-2", "s2", 0.0);
        assert_eq!(q["granted"], false);
        // Build pool unaffected by a full suite pool.
        let b = acquire(&mut s, SlotKind::Build, "dev-1", "b1", 0.0);
        assert_eq!(b["granted"], true);
        // test shares the build pool — now full too. (qa-1 itself
        // holds suite, so it could never QUEUE for build — the
        // hold-and-wait guard; the queueing caller here is dev-2.)
        let t = acquire(&mut s, SlotKind::Test, "dev-2", "t1", 0.0);
        assert_eq!(t["granted"], false, "build pool now full");
    }

    #[test]
    fn probe_answers_without_queueing() {
        let mut s = slots(1, 1, 900, &[]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        // A probe on a full pool answers position but joins nothing.
        let p = probe(&mut s, SlotKind::Build, "dev-2", "q1", 0.0);
        assert_eq!(p["granted"], false);
        assert_eq!(p["position"], 1);
        assert!(s.status(sc("dev-2", &[me()]), 1.0).0["waiting"]
            .as_array()
            .unwrap()
            .is_empty());
        // A probe on a free pool grants like an acquire — with a
        // minted token.
        release_first(&mut s, "dev-1", 1.0);
        let p = probe(&mut s, SlotKind::Build, "dev-2", "q1", 1.0);
        assert_eq!(p["granted"], true);
        assert!(p["token"].as_str().unwrap().starts_with("slot-"));
    }

    #[test]
    fn abandoned_waiter_is_reaped() {
        let mut s = slots(1, 1, 900, &[]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        // w1 queues at t=0 and never polls again — live pid, stale.
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 0.0);
        // w2 queues at t=10 keeping its poll fresh.
        acquire(&mut s, SlotKind::Build, "dev-3", "w2", 10.0);
        release_first(&mut s, "dev-1", 20.0);
        // At t=31 w1's poll is 31s stale (> TTL) — reaped; w2 granted.
        let g = acquire(&mut s, SlotKind::Build, "dev-3", "w2", 31.0);
        assert_eq!(g["granted"], true, "stale waiter must not outrank");
        // Inside the TTL w1 still holds its place.
        let mut s = slots(1, 1, 900, &[]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 0.0);
        acquire(&mut s, SlotKind::Build, "dev-3", "w2", 1.0);
        release_first(&mut s, "dev-1", 2.0);
        let g = acquire(&mut s, SlotKind::Build, "dev-3", "w2", 2.0);
        assert_eq!(g["granted"], false, "fresh waiter keeps its place");
        let g = acquire(&mut s, SlotKind::Build, "dev-2", "w1", 2.0);
        assert_eq!(g["granted"], true);
    }

    /// REGRESSION (r3 blocker): a lane keeping requests continuously
    /// in flight must not own the pool. Seniority ends at each serve
    /// and ordering age is capped at `starve_secs`, so a rival lane
    /// grants within the bound no matter how often the first lane
    /// re-queues. On the pre-r3 head the `(lane, kind)` anchor
    /// survived every grant — every fresh A waiter stamped t=0 and B
    /// never reached the front (this test's loop runs to its end and
    /// `b_granted_at` stays None).
    #[test]
    fn continuous_requeue_cannot_monopolize_a_pool() {
        let mut s = slots(1, 1, 900, &[]);
        // Lane A holds the only build slot and already keeps two
        // waiters in flight riding the t=0 anchor. Every lane-A
        // requester is a DISTINCT live pid: under the hold-and-wait
        // guard one process may hold OR wait, never both — the holder
        // and the two waiters are separate children of this test.
        let holder = Child::spawn();
        let waiter_a = Child::spawn();
        let waiter_b = Child::spawn();
        acquire_pid(&mut s, SlotKind::Build, "A", holder.pid(), "h", 0.0);
        acquire_pid(&mut s, SlotKind::Build, "A", waiter_a.pid(), "w1", 0.0);
        acquire_pid(&mut s, SlotKind::Build, "A", waiter_b.pid(), "w2", 0.0);
        // Lane B requests once, a second later, as this test process.
        acquire(&mut s, SlotKind::Build, "B", "b1", 1.0);
        let mut a_waiters = vec![
            ("w1".to_string(), waiter_a.pid()),
            ("w2".to_string(), waiter_b.pid()),
        ];
        let mut next = 3u32;
        let mut b_granted_at = None;
        // Every 20s the holder releases; A's waiters poll first
        // (adversarial), B polls once, and A tops back up to two
        // in-flight requests — the monopolization pattern. A granted
        // waiter's request flips to the holder's pid (the seat is now
        // its own), so each request's pid stays stable: no pid ever
        // holds and waits at once, and the three children bound the
        // live processes the whole run.
        for tick in 1..=45 {
            let t = tick as f64 * 20.0;
            release_any(&mut s, t);
            let mut still = Vec::new();
            for (r, pid) in &a_waiters {
                let g = acquire_pid(&mut s, SlotKind::Build, "A", *pid, r, t);
                if !g["granted"].as_bool().unwrap_or(false) {
                    still.push((r.clone(), *pid));
                }
            }
            a_waiters = still;
            let g = acquire(&mut s, SlotKind::Build, "B", "b1", t);
            if g["granted"].as_bool().unwrap_or(false) {
                b_granted_at = Some(t);
                break;
            }
            while a_waiters.len() < 2 {
                let r = format!("w{next}");
                next += 1;
                // A topped-up request queues under a child that now
                // neither waits nor holds — a granted waiter's pid
                // took the hold this tick, so it is busy.
                let busy: Vec<u32> = a_waiters
                    .iter()
                    .map(|(_, p)| *p)
                    .chain(s.held.iter().map(|h| h.pid))
                    .collect();
                let pid = [holder.pid(), waiter_a.pid(), waiter_b.pid()]
                    .into_iter()
                    .find(|p| !busy.contains(p))
                    .unwrap();
                acquire_pid(&mut s, SlotKind::Build, "A", pid, &r, t);
                a_waiters.push((r, pid));
            }
        }
        let t = b_granted_at.expect("B never granted — A owned the pool");
        assert!(t <= 900.0, "B granted only at t={t} — past starve_secs");
    }

    /// REGRESSION (r3 blocker): `slot_acquired` rides the lane's event
    /// stream — readable by ANY local caller — so it must never carry
    /// the token. token+lane+pid are the whole release credential;
    /// publishing all three hands every peer the keys to a live hold.
    /// On the pre-r3 head the event payload contained `token` and the
    /// first assertion fails.
    #[test]
    fn slot_events_never_carry_tokens() {
        let mut s = slots(1, 1, 900, &[]);
        let (g, events) = s
            .acquire(
                SlotKind::Build,
                "victim",
                me(),
                "r1",
                false,
                SlotClock::at(0.0, 0.0),
            )
            .unwrap();
        // The RPC reply still returns the token to the acquirer.
        let token = g["token"].as_str().unwrap().to_string();
        assert!(token.starts_with("slot-"));
        let ev = events
            .iter()
            .find(|e| e.1 == "slot_acquired")
            .expect("acquired event");
        assert!(
            ev.2.get("token").is_none() && !ev.2.to_string().contains(&token),
            "slot_acquired leaks the token: {}",
            ev.2
        );
        // A peer who read the event knows lane+pid but not the token —
        // every guess is refused, and the hold survives.
        assert!(s.release("slot-guess", "victim", me(), 1.0).is_err());
        assert_eq!(s.held.len(), 1);
        // The owner releases normally; the released event doesn't
        // print the spent token either.
        let (r, events) = s.release(&token, "victim", me(), 1.0).unwrap();
        assert_eq!(r["released"], true);
        let ev = events
            .iter()
            .find(|e| e.1 == "slot_released")
            .expect("released event");
        assert!(
            ev.2.get("token").is_none(),
            "slot_released leaks the token: {}",
            ev.2
        );
    }

    /// The deadlock guard binds the holding PROCESS — a second shell
    /// in the same lane (`$USER` fallback makes lanes collide) is an
    /// independent actor and may queue across pools.
    #[test]
    fn same_lane_other_pid_may_queue_across_pools() {
        let mut s = slots(1, 1, 900, &[]);
        let child = Child::spawn();
        // The child holds the suite slot; another lane fills build.
        acquire_pid(&mut s, SlotKind::Suite, "shared", child.pid(), "s1", 0.0);
        acquire(&mut s, SlotKind::Build, "dev-9", "b9", 0.0);
        // Same lane, DIFFERENT process queueing for build — allowed.
        let q = acquire_pid(&mut s, SlotKind::Build, "shared", me(), "b1", 0.0);
        assert_eq!(q["granted"], false, "queued, not refused");
        // The holding process itself is still guarded.
        let err = s
            .acquire(
                SlotKind::Build,
                "shared",
                child.pid(),
                "b2",
                false,
                SlotClock::at(0.0, 0.0),
            )
            .unwrap_err();
        assert!(err.to_string().contains("deadlock guard"), "{err}");
        child.reap();
    }

    // ---------- CAD-1021: check pool + resource floors ----------

    fn generous() -> ResourceProbe {
        // A probe that always reports a comfortably-provisioned host.
        fn g() -> (Option<u64>, Option<u64>) {
            (Some(64 * GIB), Some(500 * GIB))
        }
        g
    }

    fn below_memory() -> ResourceProbe {
        fn low() -> (Option<u64>, Option<u64>) {
            (Some(512 * (1 << 20)), Some(500 * GIB))
        }
        low
    }

    fn below_disk() -> ResourceProbe {
        fn low() -> (Option<u64>, Option<u64>) {
            (Some(64 * GIB), Some(1 << 20))
        }
        low
    }

    #[test]
    fn check_pool_is_independent_of_build_and_suite() {
        // Three build holders + one suite fill both pools; a check
        // request still grants on its own pool.
        let mut s = slots(3, 1, 900, &[]);
        s.use_resources(Some(generous()));
        for i in 0..3 {
            let r = acquire(&mut s, SlotKind::Build, "b", &format!("b{i}"), 0.0);
            assert_eq!(r["granted"], true, "build {i}");
        }
        let q = acquire(&mut s, SlotKind::Build, "dev-x", "bx", 0.0);
        assert_eq!(q["granted"], false);
        // The 4th build waits on capacity; a different caller can
        // still take a free check slot without holding while queued.
        let c = acquire(&mut s, SlotKind::Check, "dev-y", "c1", 0.0);
        assert_eq!(c["granted"], true, "check granted on its own pool: {c}");
        assert_eq!(c["kind"], "check");
    }

    #[test]
    fn below_memory_floor_waits_with_reason_not_refused() {
        let mut s = slots(3, 1, 900, &[]);
        s.use_resources(Some(below_memory()));
        let r = acquire(&mut s, SlotKind::Build, "dev-1", "b1", 0.0);
        // Below floor: queued, never refused, reason=memory.
        assert_eq!(r["granted"], false);
        assert_eq!(r["wait_reason"], "memory", "{r}");
        // A different caller tests the check pool's own (lower)
        // floor without submitting a second queued request.
        let c = acquire(&mut s, SlotKind::Check, "dev-2", "c1", 0.0);
        assert_eq!(c["granted"], false);
        assert_eq!(c["wait_reason"], "memory", "{c}");
    }

    #[test]
    fn below_memory_floor_recovers_when_memory_frees() {
        let mut s = slots(3, 1, 900, &[]);
        // Start low, then a later probe reports recovered memory — the
        // same request id re-polls and is granted.
        s.use_resources(Some(below_memory()));
        assert_eq!(
            acquire(&mut s, SlotKind::Build, "dev-1", "b1", 0.0)["granted"],
            false
        );
        s.use_resources(Some(generous()));
        let r = acquire(&mut s, SlotKind::Build, "dev-1", "b1", 0.0);
        assert_eq!(r["granted"], true, "recovered memory grants: {r}");
    }

    #[test]
    fn below_disk_floor_waits_with_reason() {
        let mut s = slots(3, 1, 900, &[]);
        s.use_resources(Some(below_disk()));
        let r = acquire(&mut s, SlotKind::Build, "dev-1", "b1", 0.0);
        assert_eq!(r["granted"], false);
        assert_eq!(r["wait_reason"], "disk", "{r}");
        s.use_resources(Some(generous()));
        let r = acquire(&mut s, SlotKind::Build, "dev-1", "b1", 0.0);
        assert_eq!(r["granted"], true);
    }

    #[test]
    fn unreadable_probe_does_not_gate() {
        // A probe returning None for both resources means "cannot
        // measure" — the floor is protective, not authoritative, so a
        // host whose probe fails still builds.
        fn none() -> (Option<u64>, Option<u64>) {
            (None, None)
        }
        let mut s = slots(1, 1, 900, &[]);
        s.use_resources(Some(none));
        let r = acquire(&mut s, SlotKind::Build, "dev-1", "b1", 0.0);
        assert_eq!(r["granted"], true, "unmeasurable probe grants: {r}");
    }

    #[test]
    fn status_shows_check_pool_and_wait_reason() {
        let mut s = slots(1, 1, 900, &[]);
        s.use_resources(Some(below_memory()));
        let _ = acquire(&mut s, SlotKind::Build, "dev-1", "b1", 0.0);
        let status = s.status(sc("dev-1", &[me()]), 0.0).0;
        // The check pool is reported; the waiter names its reason.
        assert_eq!(status["pools"]["check"]["capacity"], 2);
        assert_eq!(status["waiting"][0]["wait_reason"], "memory");
        assert_eq!(status["config"]["check_slots"], 2);
        assert!(status["config"]["slot_mem_min_available_bytes"].is_u64());
    }

    /// CAD-1021 (D5): an unregistered lane never leaps a registered one,
    /// even when it has waited past `starve_secs` and the registered
    /// waiter arrived later.
    #[test]
    fn unregistered_waiter_ranks_behind_every_registered_waiter() {
        let mut s = slots(1, 1, 60, &["qa-1"]);
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        let u = format!("{UNREGISTERED_LANE_PREFIX}1000");
        // The unregistered caller queues FIRST and keeps polling until it
        // is long past the starve bound; registered waiters arrive later.
        acquire(&mut s, SlotKind::Build, &u, "u1", 1.0);
        for t in [20.0, 45.0, 70.0, 95.0] {
            acquire(&mut s, SlotKind::Build, &u, "u1", t);
        }
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 96.0);
        acquire(&mut s, SlotKind::Test, "qa-1", "w2", 97.0);
        release_first(&mut s, "dev-1", 98.0);
        let g = acquire(&mut s, SlotKind::Build, &u, "u1", 98.0);
        assert_eq!(g["granted"], false, "unregistered must yield: {g}");
        assert_eq!(g["position"], 3, "{g}");
        // The registered waiters are served first, in their own order.
        let g = acquire(&mut s, SlotKind::Test, "qa-1", "w2", 98.0);
        assert_eq!(g["granted"], true, "{g}");
        release_any(&mut s, 99.0);
        let g = acquire(&mut s, SlotKind::Build, &u, "u1", 99.0);
        assert_eq!(g["granted"], false, "still behind dev-2: {g}");
        let g = acquire(&mut s, SlotKind::Build, "dev-2", "w1", 99.0);
        assert_eq!(g["granted"], true, "{g}");
        release_any(&mut s, 100.0);
        // Only with no registered waiter left does it get the slot.
        let g = acquire(&mut s, SlotKind::Build, &u, "u1", 100.0);
        assert_eq!(g["granted"], true, "{g}");
    }

    /// CAD-1021 part 4: `status` names each waiter's own reason and marks
    /// an unregistered one — capacity first, then a disk floor.
    #[test]
    fn status_names_each_waiters_reason_and_flags_unregistered() {
        let mut s = slots(1, 1, 900, &[]);
        s.use_resources(Some(generous()));
        acquire(&mut s, SlotKind::Build, "dev-1", "h1", 0.0);
        let u = format!("{UNREGISTERED_LANE_PREFIX}1000");
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 1.0);
        acquire(&mut s, SlotKind::Build, &u, "u1", 2.0);
        let st = s.status(sc("dev-2", &[me()]), 3.0).0;
        assert_eq!(st["pools"]["build"]["capacity"], 1);
        assert_eq!(st["pools"]["build"]["held"][0]["lane"], "dev-1");
        let w = st["waiting"].as_array().unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(
            (w[0]["lane"].as_str(), w[0]["wait_reason"].as_str()),
            (Some("dev-2"), Some("capacity"))
        );
        assert_eq!(
            (w[1]["lane"].as_str(), w[1]["unregistered"].as_bool()),
            (Some(u.as_str()), Some(true))
        );
        assert_eq!(w[0]["unregistered"], false);
        // The disk floor drops; both waiters re-poll and report it.
        s.use_resources(Some(below_disk()));
        acquire(&mut s, SlotKind::Build, "dev-2", "w1", 4.0);
        acquire(&mut s, SlotKind::Build, &u, "u1", 4.0);
        let st = s.status(sc("dev-2", &[me()]), 5.0).0;
        for w in st["waiting"].as_array().unwrap() {
            assert_eq!(w["wait_reason"], "disk", "{w}");
        }
    }

    // ---------- CAD-1268: aggregate budget ----------

    /// One scheduler under the shared cap with roomy per-pool limits
    /// and no resource floors — the aggregate is the only binder.
    fn budgeted(total: usize) -> Slots {
        let mut s = Slots::new(SlotConfig {
            total_slots: total,
            build_slots: 8,
            suite_slots: 8,
            check_slots: 8,
            ..Default::default()
        });
        s.use_resources(Some(generous()));
        s
    }

    /// The combined cap binds every pool and the probe alike: with one
    /// grant held, no second grant lands — a full suite pool does not
    /// head-of-line block a build waiter either.
    #[test]
    fn aggregate_cap_binds_every_pool_and_probe() {
        let mut s = budgeted(2);
        let a = acquire(&mut s, SlotKind::Build, "dev-1", "b1", 0.0);
        let b = acquire(&mut s, SlotKind::Build, "dev-2", "b2", 0.0);
        assert_eq!(a["granted"], true);
        assert_eq!(b["granted"], true);
        // Third grant — any pool — exceeds the shared budget even
        // though suite and check have per-pool room.
        let p = probe(&mut s, SlotKind::Check, "dev-3", "c1", 1.0);
        assert_eq!(p["granted"], false, "probe obeys the combined cap: {p}");
        assert_eq!(p["total_held"], 2);
        assert_eq!(p["total_capacity"], 2);
        let q = acquire(&mut s, SlotKind::Suite, "dev-3", "s1", 1.0);
        assert_eq!(q["granted"], false, "acquire obeys the combined cap: {q}");
        // Saturated suite is irrelevant once a seat frees: release one
        // build hold and the suite waiter drains into it — a FULL
        // pool's waiters never block an eligible rival's claim.
        let mut s = budgeted(1);
        acquire(&mut s, SlotKind::Suite, "dev-1", "s0", 0.0);
        acquire(&mut s, SlotKind::Suite, "dev-2", "s1", 0.0); // own pool full
        let q = acquire(&mut s, SlotKind::Build, "dev-3", "b1", 0.0);
        assert_eq!(q["granted"], false, "aggregate full: {q}");
        release_any(&mut s, 1.0);
        let g = acquire(&mut s, SlotKind::Build, "dev-3", "b1", 2.0);
        assert_eq!(g["granted"], true, "saturated suite pool cannot block: {g}");
    }

    /// Zero configured clamps to one, matching the per-pool floors.
    #[test]
    fn aggregate_cap_zero_clamps_to_one() {
        let mut s = budgeted(0);
        let g = acquire(&mut s, SlotKind::Build, "dev-1", "b1", 0.0);
        assert_eq!(g["granted"], true, "clamped to one: {g}");
        let g = acquire(&mut s, SlotKind::Build, "dev-2", "b2", 0.0);
        assert_eq!(g["granted"], false);
    }

    /// The hold-and-wait guard reaches a SAME-POOL hold under the
    /// shared budget, and re-polling the identical request still
    /// returns the same minted token.
    #[test]
    fn holding_process_cannot_queue_anywhere_but_repolling_is_idempotent() {
        let mut s = budgeted(2);
        let g = acquire(&mut s, SlotKind::Build, "dev-1", "r1", 0.0);
        assert_eq!(g["granted"], true);
        // Same full identity — the original token back, never queued.
        let again = acquire(&mut s, SlotKind::Build, "dev-1", "r1", 1.0);
        assert_eq!(again["token"], g["token"]);
        // Same process, different request, same pool: queued would be
        // hold-and-wait once the aggregate binds — refused outright.
        acquire(&mut s, SlotKind::Build, "dev-2", "r2", 1.0); // cap full
        let err = s
            .acquire(
                SlotKind::Build,
                "dev-1",
                me(),
                "r3",
                false,
                SlotClock::at(2.0, 0.0),
            )
            .unwrap_err();
        assert!(err.to_string().contains("deadlock guard"), "{err}");
        assert!(s.waiting.is_empty(), "the refused request never queued");
    }

    /// Status exposes the aggregate next to the pools, and the
    /// resolved config carries `total_slots` — no token escapes into
    /// either block.
    #[test]
    fn status_reports_aggregate_capacity_without_tokens() {
        let mut s = budgeted(2);
        acquire(&mut s, SlotKind::Build, "dev-1", "b1", 0.0);
        let st = s.status(sc("stranger", &[]), 0.0).0;
        assert_eq!(st["total_capacity"], 2);
        assert_eq!(st["total_held"], 1);
        assert_eq!(st["config"]["total_slots"], 2);
        assert_eq!(st["config"]["jobs_per_lane"], 2);
        assert!(!st.to_string().contains("slot-"), "no token leaked: {st}");
    }

    /// Slots restored over a reduced cap keep their holds — they drain
    /// on release, never revoked — while new work waits.
    #[test]
    fn restored_overcap_holds_drain_without_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("slots.json");
        std::fs::write(
            &file,
            json!({"holds": [
                {"token": "slot-a", "request_id": "ra", "kind": "build",
                 "lane": "dev-1", "pid": me(), "pid_start": null,
                 "acquired_epoch": 0.0},
                {"token": "slot-b", "request_id": "rb", "kind": "suite",
                 "lane": "dev-2", "pid": me(), "pid_start": null,
                 "acquired_epoch": 0.0},
            ]})
            .to_string(),
        )
        .unwrap();
        let mut s = budgeted(1);
        s.persist_to(file);
        s.restore(SlotClock::at(10.0, 10.0));
        assert_eq!(s.held.len(), 2, "over-capacity holds are retained");
        let q = acquire(&mut s, SlotKind::Check, "dev-3", "c1", 11.0);
        assert_eq!(q["granted"], false, "new work waits while over cap");
        release(&mut s, "slot-a", "dev-1", 12.0);
        let q = acquire(&mut s, SlotKind::Check, "dev-3", "c1", 13.0);
        assert_eq!(q["granted"], false, "still over the reduced cap");
        release(&mut s, "slot-b", "dev-2", 14.0);
        let q = acquire(&mut s, SlotKind::Check, "dev-3", "c1", 15.0);
        assert_eq!(q["granted"], true, "drained to the cap: {q}");
    }

    /// The wait-then-hold mirror guard: one caller keeps at most one
    /// request in the queue — a NEW request identity (probe or
    /// acquire) from a process already waiting is refused before it
    /// joins or grants, because granting it would mint a holder that
    /// still waits, and queuing it could never be granted without
    /// creating the same state. The queued request is untouched and
    /// grants normally once it leads.
    #[test]
    fn queued_caller_cannot_take_a_new_grant_elsewhere() {
        let mut s = Slots::new(SlotConfig {
            total_slots: 2,
            build_slots: 1,
            ..Default::default()
        });
        s.use_resources(Some(generous()));
        // The only build seat is held, but aggregate capacity remains
        // available for suite: refusal must come from the caller guard.
        acquire(&mut s, SlotKind::Build, "dev-9", "b9", 0.0);
        let w = acquire(&mut s, SlotKind::Build, "dev-1", "b1", 0.0);
        assert_eq!(w["granted"], false);
        // A NEW suite request identity while the build request still
        // queues — refused BEFORE it joins, probe or acquire alike.
        let err = s
            .acquire(
                SlotKind::Suite,
                "dev-1",
                me(),
                "s1",
                false,
                SlotClock::at(1.0, 0.0),
            )
            .unwrap_err();
        assert!(err.to_string().contains("deadlock guard"), "{err}");
        // Same for the read-only probe: refused, never granted.
        let err = s
            .acquire(
                SlotKind::Suite,
                "dev-1",
                me(),
                "s2",
                true,
                SlotClock::at(1.0, 0.0),
            )
            .unwrap_err();
        assert!(err.to_string().contains("deadlock guard"), "{err}");
        // The refusal removed nothing: dev-1's build waiter is alone.
        assert_eq!(s.waiting.len(), 1);
        assert_eq!(s.waiting[0].request_id, "b1");
        assert_eq!(s.waiting[0].kind, SlotKind::Build);
        assert!(s.held.iter().all(|h| h.kind.pool() != Pool::Suite));
        // The queued request itself re-polls and grants normally —
        // only NEW request identities are refused.
        release_any(&mut s, 2.0);
        let g = acquire(&mut s, SlotKind::Build, "dev-1", "b1", 3.0);
        assert_eq!(g["granted"], true, "queued request grants normally: {g}");
        // A different process in the same lane is not the queued
        // caller — the guard binds (lane, pid), so it grants freely.
        let child = Child::spawn();
        let q = acquire_pid(&mut s, SlotKind::Suite, "dev-1", child.pid(), "s2", 4.0);
        assert_eq!(q["granted"], true, "other pid in lane is unguarded: {q}");
        child.reap();
    }
}
